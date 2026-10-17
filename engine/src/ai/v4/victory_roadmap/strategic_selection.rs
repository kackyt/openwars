//! 勝利Roadmapのstrategic_selection責務。

use super::execution_observation::{observe_campaign_execution, publish_roadmap_observation};
use super::{
    GridPosition, HashMap, IslandCampaignAssignment, IslandCampaignDecision,
    IslandCampaignPortfolio, IslandId, PlayerId, Resource, SquadManager, StrategicPurpose, World,
};

pub(super) fn purpose_for(
    assignment: &IslandCampaignAssignment,
    enemy_capital: Option<GridPosition>,
) -> StrategicPurpose {
    if assignment.decision == IslandCampaignDecision::Defend {
        StrategicPurpose::DefendIsland
    } else if enemy_capital.is_some_and(|capital| {
        assignment.target_position == capital
            || assignment.capture_target_positions.contains(&capital)
    }) {
        StrategicPurpose::AssaultCapital
    } else {
        StrategicPurpose::CaptureIsland
    }
}

/// 毎ターンの島portfolioを、不変な勝利ロードマップと子作戦へ照合する。
pub(crate) fn reconcile_campaign_roadmap(
    world: &mut World,
    player_id: PlayerId,
    portfolio: &IslandCampaignPortfolio,
    manager: &SquadManager,
) {
    let (selected_portfolio, candidate_evaluations) = select_strategic_portfolio(portfolio);
    // analyzerの単一proposalをそのまま受理せず、Roadmapが比較した候補のうち一つだけを
    // この手番の正本にする。全島assessmentは選択と無関係に保持する。
    let portfolio = &selected_portfolio;
    // `selected`が一意で、候補種別・効用が監査可能な値であることをTurnPlan化の境界で
    // 検証する。候補の詳細は下記Registryに残し、SquadReconcilerは選択済みPortfolioだけを読む。
    let selected_candidate = candidate_evaluations
        .iter()
        .filter(|evaluation| evaluation.selected)
        .map(|evaluation| (evaluation.kind, evaluation.score))
        .collect::<Vec<_>>();
    debug_assert_eq!(selected_candidate.len(), 1);
    let mut candidate_registry = world
        .remove_resource::<RoadmapStrategicCandidateRegistry>()
        .unwrap_or_default();
    candidate_registry
        .by_player
        .insert(player_id, candidate_evaluations);
    world.insert_resource(candidate_registry);
    let Some(observation) = observe_campaign_execution(world, player_id, portfolio, manager) else {
        return;
    };
    let approved_portfolio =
        observation
            .registry
            .approved_portfolio(player_id, observation.enemy_capital, portfolio);
    publish_roadmap_observation(
        world,
        player_id,
        approved_portfolio,
        manager,
        observation.registry,
    );
}

/// 編成後は採用済みの案を変えず、実績と最新Squadへの指令参照だけを更新する。
/// 候補比較と兵站gateの再承認は、編成前の採用にだけ属する。
pub(crate) fn observe_adopted_campaign_roadmap(
    world: &mut World,
    player_id: PlayerId,
    portfolio: &IslandCampaignPortfolio,
    manager: &SquadManager,
) {
    let Some(observation) = observe_campaign_execution(world, player_id, portfolio, manager) else {
        return;
    };
    publish_roadmap_observation(
        world,
        player_id,
        portfolio.clone(),
        manager,
        observation.registry,
    );
}
/// Roadmapが比較した、攻勢Nodeの資源配分候補の分類。
///
/// Defenseは候補間で固定しない。現に脅威がある島を「常に最低限守る」と決め打ちせず、
/// analyzerが提案したDefenseを全候補で保持したうえで、限られた攻勢資源の配分だけを
/// 比較する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrategicCandidateKind {
    Hold,
    Focus,
    Split,
}

/// 1手番に生成・枝刈りした戦略候補の監査情報。
#[derive(Debug, Clone)]
pub(crate) struct StrategicCandidateEvaluation {
    pub kind: StrategicCandidateKind,
    pub offensive_islands: Vec<IslandId>,
    pub score: i64,
    pub selected: bool,
}

/// Roadmapの候補生成が、後段のSquad再編へ渡した唯一のPortfolioを検証できるようにする。
#[derive(Resource, Debug, Default, Clone)]
pub(crate) struct RoadmapStrategicCandidateRegistry {
    pub(super) by_player: HashMap<PlayerId, Vec<StrategicCandidateEvaluation>>,
}

/// 盤面から得た局地assignmentの価値を、全体候補の比較に使う粗い効用へ正規化する。
///
/// RollingPlanが算出した不足費用・既存戦力・実行可否を読み、Roadmapはどの島Nodeを
/// 同時に進めるかだけを決める。兵種の組合せをここで再探索しない。
pub(super) fn local_assignment_merit(assignment: &IslandCampaignAssignment) -> i64 {
    let decision_value = match assignment.decision {
        IslandCampaignDecision::Assault => 640,
        IslandCampaignDecision::Expand => 520,
        IslandCampaignDecision::Contest => 470,
        IslandCampaignDecision::Reinforce => 390,
        IslandCampaignDecision::Secure => 330,
        IslandCampaignDecision::Defend => 300,
        IslandCampaignDecision::Observe | IslandCampaignDecision::Withdraw => 0,
    };
    let existing_force = assignment
        .transport_entities
        .len()
        .saturating_add(assignment.capture_entities.len())
        .saturating_add(assignment.combat_entities.len());
    let continuity = if assignment.continued_from_existing_squad {
        140
    } else {
        0
    };
    let ready = if assignment.operation_ready {
        100
    } else {
        -180
    };
    // shortfallはRollingPlanの成立性に必要な資金であり、Node価値そのものではない。
    // これを過大に引くと「歩兵が1体足りない進軍」をHoldが常に棄却してしまうため、
    // 現有戦力・継続作戦を残したまま比較できる重みに正規化する。
    let shortfall = i64::from(assignment.purchase_shortfall.total_budget) / 100;
    decision_value + i64::try_from(existing_force).unwrap_or(i64::MAX) * 35 + continuity + ready
        - shortfall
}

/// 最大3攻勢Nodeだけを残して全部分集合を比較する、境界付きの戦略候補探索。
///
/// Island allocator自体も同時攻勢を制限しているが、ここで改めて上位3件へ枝刈りし、
/// 将来allocatorの上限が変わってもRoadmapの組合せ爆発を防ぐ。2^3=8候補であり、
/// `Focus A`、`Focus B`、`A+B`、全力分散、攻勢保留を同じ評価式で比較できる。
pub(super) fn select_strategic_portfolio(
    portfolio: &IslandCampaignPortfolio,
) -> (IslandCampaignPortfolio, Vec<StrategicCandidateEvaluation>) {
    const MAX_BRANCHING_OFFENSIVES: usize = 3;
    let mut candidates = portfolio.active_offensives.clone();
    candidates.sort_unstable_by_key(|assignment| {
        (
            std::cmp::Reverse(local_assignment_merit(assignment)),
            assignment.island_id.0,
        )
    });
    candidates.truncate(MAX_BRANCHING_OFFENSIVES);
    let has_ready_offensive = candidates
        .iter()
        .any(|assignment| assignment.operation_ready || assignment.continued_from_existing_squad);

    let mut evaluations = Vec::new();
    let mut selected_assignments = Vec::new();
    let mut best_key = None;
    for mask in 0..(1_usize << candidates.len()) {
        // 実行可能または継続中の攻勢まで全て停止するのは、資金を貯める明確な撤収判断では
        // なく、短期shortfallを過大評価した副作用である。最低1Nodeは残し、Roadmapが
        // 「どこへ集中するか」を決める。
        if mask == 0 && has_ready_offensive {
            continue;
        }
        let assignments = candidates
            .iter()
            .enumerate()
            .filter_map(|(index, assignment)| ((mask & (1 << index)) != 0).then_some(assignment))
            .collect::<Vec<_>>();
        let offensive_islands = assignments
            .iter()
            .map(|assignment| assignment.island_id)
            .collect::<Vec<_>>();
        let base = assignments
            .iter()
            .map(|assignment| local_assignment_merit(assignment))
            .sum::<i64>();
        // 複数Nodeを同時に進めると輸送・capturer・生産施設を取り合う。実行中の
        // assignmentが大きく劣る場合だけ分散を選ぶよう二次ペナルティを置く。
        let count = i64::try_from(assignments.len()).unwrap_or(i64::MAX);
        let spread_penalty = count.saturating_sub(1).saturating_pow(2) * 110;
        let score = base - spread_penalty;
        let kind = match assignments.len() {
            0 => StrategicCandidateKind::Hold,
            1 => StrategicCandidateKind::Focus,
            _ => StrategicCandidateKind::Split,
        };
        // 同点はより少ない同時攻勢、次に島ID順で決める。優劣のない候補の揺れを
        // Battle評価のノイズにしないための決定規則である。
        let mut island_order = offensive_islands
            .iter()
            .map(|island_id| island_id.0)
            .collect::<Vec<_>>();
        island_order.sort_unstable();
        let key = (
            score,
            std::cmp::Reverse(assignments.len()),
            std::cmp::Reverse(island_order),
        );
        if best_key.as_ref().is_none_or(|current| key > *current) {
            best_key = Some(key);
            selected_assignments = assignments.into_iter().cloned().collect();
        }
        evaluations.push(StrategicCandidateEvaluation {
            kind,
            offensive_islands,
            score,
            selected: false,
        });
    }
    let selected_islands = selected_assignments
        .iter()
        .map(|assignment: &IslandCampaignAssignment| assignment.island_id)
        .collect::<Vec<_>>();
    for evaluation in &mut evaluations {
        evaluation.selected = evaluation.offensive_islands == selected_islands;
    }
    let mut selected = portfolio.clone();
    selected.active_offensives = selected_assignments;
    (selected, evaluations)
}
