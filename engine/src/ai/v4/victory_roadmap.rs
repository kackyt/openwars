use crate::ai::engine::AiCommand;
use crate::ai::island_campaign::{
    IslandCampaignAssessment, IslandCampaignAssignment, IslandCampaignDecision,
    IslandCampaignPortfolio,
};
use crate::ai::islands::{IslandId, IslandMap};
use crate::ai::squad::{MissionPhase, MissionType, SquadManager, TransportPhase};
use crate::ai::turn_distance::{TurnDistanceCache, calculate_turn_distance};
use crate::ai::v4::deployment::V4DeploymentRegistry;
use crate::ai::v4::operation::OperationKind;
use crate::ai::v4::plan_revision::{ActiveCombatPlanSummary, PlanId, V4RollingPlanRegistry};
use crate::components::{
    CargoCapacity, Faction, GridPosition, Health, PlayerId, Property, Transporting, UnitStats,
};
use crate::events::{
    PropertyCaptureProgressedEvent, PropertyCapturedEvent, UnitAttackedEvent, UnitDestroyedEvent,
    UnitLoadedEvent, UnitMovedEvent, UnitSuppliedEvent, UnitUnloadedEvent, UnitWaitedEvent,
};
use crate::resources::{Map, MatchState, Terrain, master_data::MasterDataRegistry};
use crate::systems::movement::OccupantInfo;
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};

/// 勝利条件までの親計画ID。局地PlanIdと分離し、勝利経路が変わるまで維持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VictoryRoadmapId(pub u64);

/// 島単位の不変な作戦ID。敵Entityや毎ターン変わるanchorをidentityに含めない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrategicOperationId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VictoryRoute {
    CapitalCapture,
    EnemyAnnihilation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StrategicPurpose {
    CaptureIsland,
    DefendIsland,
    AssaultCapital,
}

/// 同じ陸塊でも、局地作戦と勝利条件の首都作戦は別のライフサイクルを持つ。
/// Capture/Defense間の変更は同じ局地作戦のrevisionとして履歴を引き継ぐ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum StrategicOperationScope {
    Regional,
    Capital,
}

fn operation_scope(purpose: StrategicPurpose) -> StrategicOperationScope {
    match purpose {
        StrategicPurpose::CaptureIsland | StrategicPurpose::DefendIsland => {
            StrategicOperationScope::Regional
        }
        StrategicPurpose::AssaultCapital => StrategicOperationScope::Capital,
    }
}

/// Roadmap上の子作戦とRolling Combat Planの種別を一意に対応付ける。
///
/// 同じ首都島でも、DAGの未確保区間を取るCaptureと終端のAssaultCapitalは別の
/// 予実台帳を持つ。島IDだけで結び付けると、前段の計画を首都作戦が横取りする。
fn rolling_operation_kind(purpose: StrategicPurpose) -> OperationKind {
    match purpose {
        StrategicPurpose::CaptureIsland => OperationKind::Capture,
        StrategicPurpose::DefendIsland => OperationKind::Defense,
        StrategicPurpose::AssaultCapital => OperationKind::AssaultCapital,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CampaignStepKind {
    Produce,
    Move,
    Load,
    Transit,
    Drop,
    Attack,
    Capture,
    Hold,
    Supply,
    Wait,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationPhase {
    Forming,
    Pickup,
    Transit,
    Drop,
    Suppress,
    Capture,
    Hold,
    Completed,
    Blocked,
}

/// Roadmap Nodeを盤面へ投影した現在の進行状態。
///
/// `OperationPhase` はSquad・輸送の工程、こちらは前提Nodeを解放できるかという
/// 戦略上の状態を表す。両者を一つのenumへ混ぜると、例えば輸送中であることと
/// 局地戦で優勢であることを同じ軸で比較してしまう。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoadmapNodeState {
    Locked,
    Ready,
    Contested,
    Dominant,
    Capturing,
    Secured,
    Blocked,
}

/// Roadmap上のOperation間にある、実行順序を持つ依存辺。
///
/// `Logistics` は島間の兵站経路、`CapitalRoute` は敵首都島で前段Milestoneを
/// 確保してから首都強襲へ移る関係を表す。いずれも前提が `Dominant` または
/// `Secured` になった時だけ後続Nodeを解放する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoadmapDependencyKind {
    Logistics,
    CapitalRoute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoadmapDependency {
    pub predecessor: StrategicOperationId,
    pub successor: StrategicOperationId,
    pub kind: RoadmapDependencyKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationEntityRole {
    Transport,
    Capture,
    Combat,
}

/// 未完工程の直接原因。単なる「未完」から、再計画で取るべき行動を分離する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationIssueKind {
    AwaitingProduction,
    ProductionDelayed,
    ProducedEntityUnassigned,
    AssignmentLost,
    TransportUnassigned,
    TransportDestroyed,
    CapturerDestroyed,
    CombatUnitDestroyed,
    CargoLostWithTransport,
    /// 命令として発行した作戦stepに対応する完了Eventが手番内に届かなかった。
    StepBlocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationIssue {
    pub kind: OperationIssueKind,
    pub detected_turn: u32,
    pub entity: Option<Entity>,
    pub related_entity: Option<Entity>,
    pub detail: String,
}

/// 検知した原因に対して実際に確認できた復旧行動。原因検知だけを再計画実績に数えない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationRecoveryKind {
    RetryProduction,
    RestoreAssignment,
    AssignTransport,
    RequestReplacement,
    ReplanStep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecoveryAction {
    pub kind: OperationRecoveryKind,
    pub cause: OperationIssueKind,
    pub completed_turn: u32,
    pub entity: Option<Entity>,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepExecutionTotals {
    pub planned: u32,
    pub completed: u32,
    pub blocked: u32,
    pub moves: u32,
    pub loads: u32,
    pub drops: u32,
    pub attacks: u32,
    pub captures: u32,
    pub completed_captures: u32,
    pub supplies: u32,
    pub waits: u32,
    pub deviations: u32,
}

/// 命令発行から結果Eventまでを結ぶ、Entity単位の未完作戦step。
///
/// 1 Entity 1作戦の正本と同じキーを使うため、照合は平均O(1)であり、
/// 作戦や行動種別ごとに全Entityを走査しない。
#[derive(Debug, Clone)]
struct PendingOperationStep {
    operation_id: StrategicOperationId,
    planned_turn: u32,
    step: CampaignStepKind,
    terminal_event: CampaignStepKind,
    target_position: Option<GridPosition>,
    target_entity: Option<Entity>,
    movement_observed: bool,
}

#[derive(Debug, Clone)]
pub struct OperationStepRecord {
    pub operation_id: StrategicOperationId,
    pub entity: Entity,
    pub planned_turn: u32,
    pub resolved_turn: Option<u32>,
    pub step: CampaignStepKind,
    pub target_position: Option<GridPosition>,
    pub target_entity: Option<Entity>,
    pub completed: bool,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StrategicOperation {
    pub id: StrategicOperationId,
    pub roadmap_id: VictoryRoadmapId,
    pub player_id: PlayerId,
    pub island_id: IslandId,
    pub purpose: StrategicPurpose,
    pub created_turn: u32,
    pub last_observed_turn: u32,
    pub tactical_anchor: GridPosition,
    /// 現在の局地Milestoneが担当する目的拠点。完了済みの旧目標は残さない。
    pub objective_properties: Vec<GridPosition>,
    pub owned_objective_count: usize,
    /// `VictoryRoadmap` が公開する、Nodeの戦略上の進行状態。
    pub node_state: RoadmapNodeState,
    pub phase: OperationPhase,
    pub planned_completion_turn: Option<u32>,
    pub actual_completion_turn: Option<u32>,
    pub assigned_transports: HashSet<Entity>,
    pub assigned_capturers: HashSet<Entity>,
    pub assigned_combat: HashSet<Entity>,
    pub combat_plan_ids: HashSet<PlanId>,
    pub planned_suppression_turn: Option<u32>,
    pub execution: StepExecutionTotals,
    pub last_step: Option<CampaignStepKind>,
    pub last_progress_turn: Option<u32>,
    pub blocked_reason: Option<String>,
    pub current_issues: Vec<OperationIssue>,
    pub issue_history: Vec<OperationIssue>,
    pub recovery_history: Vec<OperationRecoveryAction>,
    pub replan_count: u32,
    pub last_replan_turn: Option<u32>,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct VictoryRoadmap {
    pub id: VictoryRoadmapId,
    pub player_id: PlayerId,
    pub route: VictoryRoute,
    pub created_turn: u32,
    pub last_observed_turn: u32,
    pub enemy_capital: Option<GridPosition>,
    pub enemy_capital_island: Option<IslandId>,
    pub planned_victory_turn: Option<u32>,
    pub actual_victory_turn: Option<u32>,
    pub initial_enemy_unit_count: usize,
    pub current_enemy_unit_count: usize,
    pub operation_ids: Vec<StrategicOperationId>,
    /// IslandCampaignと首都攻略を接続する、当該Roadmapの有向非巡回な依存辺。
    pub dependencies: Vec<RoadmapDependency>,
}

/// Roadmapがこの手番に実行すると確定したOperationの、Squad投影用要約。
///
/// 生産slotの予約と複数戦略案の探索は未決定であるため、ここでは既存の
/// IslandCampaign analyzerが提案した割当を一手番だけ凍結する。これにより
/// 後段のSquad再編器が別の島作戦を発明せず、同じ入力を投影できる。
#[derive(Debug, Clone)]
pub(crate) struct RoadmapOperationDirective {
    pub operation_id: StrategicOperationId,
    pub island_id: IslandId,
    pub purpose: StrategicPurpose,
    pub node_state: RoadmapNodeState,
    pub target: GridPosition,
    pub squad_ids: Vec<crate::ai::squad::SquadId>,
}

/// 1 player・1手番のRoadmap決定を保持する不変スナップショット。
#[derive(Debug, Clone)]
pub(crate) struct RoadmapTurnPlan {
    pub player_id: PlayerId,
    pub turn: u32,
    pub portfolio: IslandCampaignPortfolio,
    pub directives: Vec<RoadmapOperationDirective>,
}

impl RoadmapTurnPlan {
    /// Roadmapが同じSquadを複数Operationへ同時に投影していないことを検証する。
    ///
    /// これは候補選択ではなく、TurnPlanをSquadReconcilerへ渡す直前の整合性検査である。
    /// 空のSquad集合はForming Operationを表すため許可する。
    fn is_consistent(&self) -> bool {
        let mut operation_ids = HashSet::new();
        let mut objectives = HashSet::new();
        let mut squad_owners = HashMap::new();
        self.directives.iter().all(|directive| {
            let unique_operation = operation_ids.insert(directive.operation_id);
            let unique_objective =
                objectives.insert((directive.island_id, directive.purpose, directive.target));
            let active_node = directive.node_state != RoadmapNodeState::Locked;
            let squads_are_exclusive = directive.squad_ids.iter().all(|squad_id| {
                squad_owners
                    .insert(*squad_id, directive.operation_id)
                    .is_none_or(|owner| owner == directive.operation_id)
            });
            unique_operation && unique_objective && active_node && squads_are_exclusive
        })
    }
}

/// playerごとの当ターンRoadmap指示。Squad編成・行動器はこれを読み、
/// `IslandCampaignDiagnostics` のような観測専用Resourceを意思決定に使わない。
#[derive(Resource, Debug, Default, Clone)]
pub(crate) struct RoadmapTurnPlanRegistry {
    plans: HashMap<PlayerId, RoadmapTurnPlan>,
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
    by_player: HashMap<PlayerId, Vec<StrategicCandidateEvaluation>>,
}

/// 盤面から得た局地assignmentの価値を、全体候補の比較に使う粗い効用へ正規化する。
///
/// RollingPlanが算出した不足費用・既存戦力・実行可否を読み、Roadmapはどの島Nodeを
/// 同時に進めるかだけを決める。兵種の組合せをここで再探索しない。
fn local_assignment_merit(assignment: &IslandCampaignAssignment) -> i64 {
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
fn select_strategic_portfolio(
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

impl RoadmapTurnPlanRegistry {
    fn replace(&mut self, plan: RoadmapTurnPlan) {
        self.plans.insert(plan.player_id, plan);
    }

    fn portfolio_for_turn(
        &self,
        player_id: PlayerId,
        turn: u32,
    ) -> Option<&IslandCampaignPortfolio> {
        self.plans
            .get(&player_id)
            .filter(|plan| plan.turn == turn)
            .filter(|plan| plan.is_consistent())
            .map(|plan| &plan.portfolio)
    }
}

/// 当ターンにRoadmapが採用した島作戦を返す。
///
/// 戦略案探索は保留中なので、これは既存analyzerの出力をRoadmapが受理した結果である。
/// 受理後のSquad再編ではこのsnapshotだけを参照し、再度portfolioを分析しない。
pub(crate) fn current_turn_portfolio(
    world: &World,
    player_id: PlayerId,
    turn: u32,
) -> Option<IslandCampaignPortfolio> {
    world
        .get_resource::<RoadmapTurnPlanRegistry>()
        .and_then(|plans| plans.portfolio_for_turn(player_id, turn))
        .cloned()
}

#[derive(Debug, Clone, Copy)]
struct EntityOperationBinding {
    operation_id: StrategicOperationId,
    role: OperationEntityRole,
}

/// 勝利条件、島作戦、実Entity、実行Eventを同じidentityで監査する永続Resource。
#[derive(Resource, Debug, Default)]
pub struct VictoryRoadmapRegistry {
    next_roadmap_id: u64,
    next_operation_id: u64,
    roadmaps: HashMap<PlayerId, VictoryRoadmap>,
    operations: HashMap<StrategicOperationId, StrategicOperation>,
    // 同一島のCapture/Defenseは局地作戦として連続させる一方、首都作戦は別identityにする。
    // これにより局地目標のanchor更新が勝利条件の終端目標を上書きしない。
    operation_keys: HashMap<(PlayerId, IslandId, StrategicOperationScope), StrategicOperationId>,
    entity_bindings: HashMap<Entity, EntityOperationBinding>,
    transport_manifests: HashMap<Entity, HashSet<Entity>>,
    pending_steps: HashMap<Entity, PendingOperationStep>,
    step_history: Vec<OperationStepRecord>,
}

impl VictoryRoadmapRegistry {
    /// 局地portfolioが兵站gate待ちで首都作戦をまだ公開していない期間も、
    /// 勝利条件から消えない常在AssaultCapitalの島とanchorを返す。
    pub(crate) fn active_capital_objective(
        &self,
        player_id: PlayerId,
    ) -> Option<(IslandId, GridPosition)> {
        self.operations
            .values()
            .filter(|operation| {
                operation.player_id == player_id
                    && operation.active
                    && operation.purpose == StrategicPurpose::AssaultCapital
                    && operation.node_state != RoadmapNodeState::Locked
            })
            .min_by_key(|operation| operation.id.0)
            .map(|operation| (operation.island_id, operation.tactical_anchor))
    }

    /// 局地portfolioのgate外でも形成を続けている首都強襲Entityを返す。
    /// 次ターンのportfolio更新が「未claim」と誤認して作戦所有権を外さないための集合で、
    /// inactive化した作戦は対象に含めない。
    pub(crate) fn active_capital_entities(&self, player_id: PlayerId) -> HashSet<Entity> {
        self.operations
            .values()
            .filter(|operation| {
                operation.player_id == player_id
                    && operation.active
                    && operation.purpose == StrategicPurpose::AssaultCapital
            })
            .flat_map(|operation| {
                operation
                    .assigned_transports
                    .iter()
                    .chain(operation.assigned_capturers.iter())
                    .chain(operation.assigned_combat.iter())
                    .copied()
            })
            .collect()
    }

    pub fn roadmap(&self, player_id: PlayerId) -> Option<&VictoryRoadmap> {
        self.roadmaps.get(&player_id)
    }

    pub fn operations_for(&self, player_id: PlayerId) -> Vec<&StrategicOperation> {
        let mut operations = self
            .operations
            .values()
            .filter(|operation| operation.player_id == player_id)
            .collect::<Vec<_>>();
        operations.sort_unstable_by_key(|operation| operation.id.0);
        operations
    }

    pub fn step_history_for(&self, player_id: PlayerId) -> Vec<&OperationStepRecord> {
        self.step_history
            .iter()
            .filter(|record| {
                self.operations
                    .get(&record.operation_id)
                    .is_some_and(|operation| operation.player_id == player_id)
            })
            .collect()
    }

    fn plan_entity_step(
        &mut self,
        entity: Entity,
        turn: u32,
        step: CampaignStepKind,
        terminal_event: CampaignStepKind,
        target_position: Option<GridPosition>,
        target_entity: Option<Entity>,
    ) {
        let Some(binding) = self.entity_bindings.get(&entity).copied() else {
            return;
        };
        if let Some(previous) = self.pending_steps.remove(&entity) {
            self.block_pending_step(entity, previous, turn, "superseded before completion");
        }
        if let Some(operation) = self.operations.get_mut(&binding.operation_id) {
            operation.execution.planned = operation.execution.planned.saturating_add(1);
            if operation.current_issues.iter().any(|issue| {
                issue.kind == OperationIssueKind::StepBlocked && issue.entity == Some(entity)
            }) {
                let recovery = OperationRecoveryAction {
                    kind: OperationRecoveryKind::ReplanStep,
                    cause: OperationIssueKind::StepBlocked,
                    completed_turn: turn,
                    entity: Some(entity),
                    detail: format!(
                        "replanned Entity {} from blocked step to {:?}",
                        entity.to_bits(),
                        step
                    ),
                };
                operation.replan_count = operation.replan_count.saturating_add(1);
                operation.last_replan_turn = Some(turn);
                operation.recovery_history.push(recovery);
            }
        }
        self.pending_steps.insert(
            entity,
            PendingOperationStep {
                operation_id: binding.operation_id,
                planned_turn: turn,
                step,
                terminal_event,
                target_position,
                target_entity,
                movement_observed: false,
            },
        );
        self.step_history.push(OperationStepRecord {
            operation_id: binding.operation_id,
            entity,
            planned_turn: turn,
            resolved_turn: None,
            step,
            target_position,
            target_entity,
            completed: false,
            blocked_reason: None,
        });
    }

    fn observe_planned_move(&mut self, entity: Entity, to: GridPosition, turn: u32) -> bool {
        let Some(pending) = self.pending_steps.get_mut(&entity) else {
            return false;
        };
        if pending.planned_turn != turn || pending.target_position != Some(to) {
            return false;
        }
        pending.movement_observed = true;
        if let Some(operation) = self.operations.get_mut(&pending.operation_id) {
            operation.execution.moves = operation.execution.moves.saturating_add(1);
        }
        true
    }

    fn complete_planned_step(
        &mut self,
        entity: Entity,
        turn: u32,
        event_step: CampaignStepKind,
        position: Option<GridPosition>,
        related_entity: Option<Entity>,
    ) -> bool {
        let Some(pending) = self.pending_steps.get(&entity) else {
            return false;
        };
        let position_matches = pending.target_position.is_none()
            || position.is_none()
            || pending.target_position == position;
        let entity_matches = pending.target_entity.is_none()
            || related_entity.is_none()
            || pending.target_entity == related_entity;
        if pending.planned_turn != turn
            || pending.terminal_event != event_step
            || !position_matches
            || !entity_matches
        {
            return false;
        }

        let pending = self.pending_steps.remove(&entity).unwrap();
        if let Some(operation) = self.operations.get_mut(&pending.operation_id) {
            operation.execution.completed = operation.execution.completed.saturating_add(1);
            operation.last_step = Some(pending.step);
            operation.last_progress_turn = Some(turn);
            match pending.step {
                CampaignStepKind::Move | CampaignStepKind::Transit => {
                    // 実移動が無い同位置Waitは進捗に数えず、Holdとしてのみ完了させる。
                    if !pending.movement_observed {
                        operation.execution.waits = operation.execution.waits.saturating_add(1);
                    }
                }
                CampaignStepKind::Load => {
                    operation.execution.loads = operation.execution.loads.saturating_add(1);
                }
                CampaignStepKind::Drop => {
                    operation.execution.drops = operation.execution.drops.saturating_add(1);
                }
                CampaignStepKind::Attack => {
                    operation.execution.attacks = operation.execution.attacks.saturating_add(1);
                }
                CampaignStepKind::Capture => {
                    operation.execution.captures = operation.execution.captures.saturating_add(1);
                }
                CampaignStepKind::Supply => {
                    operation.execution.supplies = operation.execution.supplies.saturating_add(1);
                }
                CampaignStepKind::Wait | CampaignStepKind::Hold => {
                    operation.execution.waits = operation.execution.waits.saturating_add(1);
                }
                CampaignStepKind::Produce => {}
            }
        }
        if let Some(record) = self.step_history.iter_mut().rev().find(|record| {
            record.entity == entity
                && record.operation_id == pending.operation_id
                && record.planned_turn == pending.planned_turn
                && record.resolved_turn.is_none()
        }) {
            record.resolved_turn = Some(turn);
            record.completed = true;
        }
        true
    }

    fn block_pending_step(
        &mut self,
        entity: Entity,
        pending: PendingOperationStep,
        turn: u32,
        reason: &str,
    ) {
        let detail = format!(
            "planned {:?} for Entity {} at turn {} was not completed: {}",
            pending.step,
            entity.to_bits(),
            pending.planned_turn,
            reason
        );
        if let Some(operation) = self.operations.get_mut(&pending.operation_id) {
            operation.execution.blocked = operation.execution.blocked.saturating_add(1);
            operation.execution.deviations = operation.execution.deviations.saturating_add(1);
            operation.phase = OperationPhase::Blocked;
            operation.node_state = RoadmapNodeState::Blocked;
            operation.blocked_reason = Some(detail.clone());
            let issue = OperationIssue {
                kind: OperationIssueKind::StepBlocked,
                detected_turn: turn,
                entity: Some(entity),
                related_entity: pending.target_entity,
                detail: detail.clone(),
            };
            operation.current_issues.push(issue.clone());
            operation.issue_history.push(issue);
        }
        if let Some(record) = self.step_history.iter_mut().rev().find(|record| {
            record.entity == entity
                && record.operation_id == pending.operation_id
                && record.planned_turn == pending.planned_turn
                && record.resolved_turn.is_none()
        }) {
            record.resolved_turn = Some(turn);
            record.blocked_reason = Some(detail);
        }
    }

    fn expire_pending_steps(&mut self, player_id: PlayerId, turn: u32) {
        let expired = self
            .pending_steps
            .iter()
            .filter(|(_, pending)| {
                pending.planned_turn < turn
                    && self
                        .operations
                        .get(&pending.operation_id)
                        .is_some_and(|operation| operation.player_id == player_id)
            })
            .map(|(entity, pending)| (*entity, pending.clone()))
            .collect::<Vec<_>>();
        for (entity, pending) in expired {
            self.pending_steps.remove(&entity);
            self.block_pending_step(entity, pending, turn, "no matching result Event");
        }
    }

    fn ensure_roadmap(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        enemy_capital: Option<GridPosition>,
        enemy_capital_island: Option<IslandId>,
        enemy_unit_count: usize,
    ) -> VictoryRoadmapId {
        if let Some(roadmap) = self.roadmaps.get_mut(&player_id) {
            roadmap.last_observed_turn = turn;
            roadmap.enemy_capital = enemy_capital;
            roadmap.enemy_capital_island = enemy_capital_island;
            roadmap.current_enemy_unit_count = enemy_unit_count;
            return roadmap.id;
        }
        self.next_roadmap_id = self.next_roadmap_id.saturating_add(1);
        let id = VictoryRoadmapId(self.next_roadmap_id);
        self.roadmaps.insert(
            player_id,
            VictoryRoadmap {
                id,
                player_id,
                route: if enemy_capital.is_some() {
                    VictoryRoute::CapitalCapture
                } else {
                    VictoryRoute::EnemyAnnihilation
                },
                created_turn: turn,
                last_observed_turn: turn,
                enemy_capital,
                enemy_capital_island,
                planned_victory_turn: None,
                actual_victory_turn: None,
                initial_enemy_unit_count: enemy_unit_count,
                current_enemy_unit_count: enemy_unit_count,
                operation_ids: Vec::new(),
                dependencies: Vec::new(),
            },
        );
        id
    }

    fn ensure_capital_objective(
        &mut self,
        roadmap_id: VictoryRoadmapId,
        player_id: PlayerId,
        turn: u32,
        island_id: IslandId,
        capital: GridPosition,
        owned: bool,
    ) -> StrategicOperationId {
        let key = (player_id, island_id, StrategicOperationScope::Capital);
        let operation_id = if let Some(id) = self.operation_keys.get(&key).copied() {
            id
        } else {
            self.next_operation_id = self.next_operation_id.saturating_add(1);
            let id = StrategicOperationId(self.next_operation_id);
            self.operation_keys.insert(key, id);
            self.operations.insert(
                id,
                StrategicOperation {
                    id,
                    roadmap_id,
                    player_id,
                    island_id,
                    purpose: StrategicPurpose::AssaultCapital,
                    created_turn: turn,
                    last_observed_turn: turn,
                    tactical_anchor: capital,
                    objective_properties: vec![capital],
                    owned_objective_count: usize::from(owned),
                    node_state: if owned {
                        RoadmapNodeState::Secured
                    } else {
                        RoadmapNodeState::Blocked
                    },
                    phase: OperationPhase::Forming,
                    planned_completion_turn: None,
                    actual_completion_turn: None,
                    assigned_transports: HashSet::new(),
                    assigned_capturers: HashSet::new(),
                    assigned_combat: HashSet::new(),
                    combat_plan_ids: HashSet::new(),
                    planned_suppression_turn: None,
                    execution: StepExecutionTotals::default(),
                    last_step: None,
                    last_progress_turn: None,
                    blocked_reason: Some(
                        "no executable capital assault schedule has been selected".to_owned(),
                    ),
                    current_issues: Vec::new(),
                    issue_history: Vec::new(),
                    recovery_history: Vec::new(),
                    replan_count: 0,
                    last_replan_turn: None,
                    active: true,
                },
            );
            if let Some(roadmap) = self.roadmaps.get_mut(&player_id) {
                roadmap.operation_ids.push(id);
            }
            id
        };
        let operation = self
            .operations
            .get_mut(&operation_id)
            .expect("作成済み首都作戦");
        operation.purpose = StrategicPurpose::AssaultCapital;
        operation.last_observed_turn = turn;
        operation.tactical_anchor = capital;
        operation.owned_objective_count = usize::from(owned);
        operation.active = true;
        operation.assigned_transports.clear();
        operation.assigned_capturers.clear();
        operation.assigned_combat.clear();
        operation.combat_plan_ids.clear();
        operation.planned_suppression_turn = None;
        if owned {
            operation.phase = OperationPhase::Completed;
            operation.node_state = RoadmapNodeState::Secured;
            operation.actual_completion_turn.get_or_insert(turn);
            operation.blocked_reason = None;
        } else {
            operation.phase = OperationPhase::Forming;
            operation.node_state = RoadmapNodeState::Blocked;
            operation.planned_completion_turn = None;
            operation.blocked_reason =
                Some("no executable capital assault schedule has been selected".to_owned());
        }
        operation_id
    }

    /// Regional/CapitalのOperationを同じ島・scopeごとに一度だけ作成する。
    ///
    /// 最初に全島評価から観測Nodeを作り、その後に実行assignmentが同じIDを更新する。
    /// これにより「行動候補に選ばれなかったため、島自体のNodeが消える」ことを防ぐ。
    #[allow(clippy::too_many_arguments)]
    fn ensure_operation(
        &mut self,
        roadmap_id: VictoryRoadmapId,
        player_id: PlayerId,
        turn: u32,
        island_id: IslandId,
        purpose: StrategicPurpose,
        tactical_anchor: GridPosition,
        objective_properties: Vec<GridPosition>,
    ) -> StrategicOperationId {
        let key = (player_id, island_id, operation_scope(purpose));
        if let Some(operation_id) = self.operation_keys.get(&key).copied() {
            return operation_id;
        }
        self.next_operation_id = self.next_operation_id.saturating_add(1);
        let operation_id = StrategicOperationId(self.next_operation_id);
        self.operation_keys.insert(key, operation_id);
        self.operations.insert(
            operation_id,
            StrategicOperation {
                id: operation_id,
                roadmap_id,
                player_id,
                island_id,
                purpose,
                created_turn: turn,
                last_observed_turn: turn,
                tactical_anchor,
                objective_properties,
                owned_objective_count: 0,
                node_state: RoadmapNodeState::Locked,
                phase: OperationPhase::Forming,
                planned_completion_turn: None,
                actual_completion_turn: None,
                assigned_transports: HashSet::new(),
                assigned_capturers: HashSet::new(),
                assigned_combat: HashSet::new(),
                combat_plan_ids: HashSet::new(),
                planned_suppression_turn: None,
                execution: StepExecutionTotals::default(),
                last_step: None,
                last_progress_turn: None,
                blocked_reason: None,
                current_issues: Vec::new(),
                issue_history: Vec::new(),
                recovery_history: Vec::new(),
                replan_count: 0,
                last_replan_turn: None,
                active: false,
            },
        );
        if let Some(roadmap) = self.roadmaps.get_mut(&player_id) {
            roadmap.operation_ids.push(operation_id);
        }
        operation_id
    }

    /// allocatorの実行候補にならない島も、Roadmap上では観測Nodeとして残す。
    ///
    /// 観測NodeはEntityを所有せず、未確保ならLocked、全施設を所有していれば
    /// Securedとなる。実行候補が同じ島に現れた場合は後続の
    /// `reconcile_assignment` が同じOperation IDをactiveな作戦へ更新する。
    #[allow(clippy::too_many_arguments)]
    fn reconcile_observed_island(
        &mut self,
        roadmap_id: VictoryRoadmapId,
        player_id: PlayerId,
        turn: u32,
        assessment: &IslandCampaignAssessment,
        tactical_anchor: GridPosition,
        objective_properties: Vec<GridPosition>,
        owned_properties: &HashSet<GridPosition>,
    ) {
        let purpose = if assessment.decision == IslandCampaignDecision::Defend {
            StrategicPurpose::DefendIsland
        } else {
            StrategicPurpose::CaptureIsland
        };
        let operation_id = self.ensure_operation(
            roadmap_id,
            player_id,
            turn,
            assessment.island_id,
            purpose,
            tactical_anchor,
            objective_properties.clone(),
        );
        let operation = self
            .operations
            .get_mut(&operation_id)
            .expect("作成済み観測Node");
        operation.purpose = purpose;
        operation.last_observed_turn = turn;
        operation.tactical_anchor = tactical_anchor;
        operation.objective_properties = objective_properties;
        operation.owned_objective_count = operation
            .objective_properties
            .iter()
            .filter(|position| owned_properties.contains(position))
            .count();
        operation.active = false;
        operation.planned_completion_turn = None;
        operation.combat_plan_ids.clear();
        operation.planned_suppression_turn = None;
        operation.blocked_reason = None;
        if !operation.objective_properties.is_empty()
            && operation.owned_objective_count == operation.objective_properties.len()
        {
            operation.phase = OperationPhase::Completed;
            operation.actual_completion_turn.get_or_insert(turn);
        } else {
            operation.phase = OperationPhase::Forming;
            operation.actual_completion_turn = None;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_assignment(
        &mut self,
        roadmap_id: VictoryRoadmapId,
        player_id: PlayerId,
        turn: u32,
        assignment: &IslandCampaignAssignment,
        purpose: StrategicPurpose,
        objectives: Vec<GridPosition>,
        owned_properties: &HashSet<GridPosition>,
        phase: OperationPhase,
    ) -> StrategicOperationId {
        let operation_id = self.ensure_operation(
            roadmap_id,
            player_id,
            turn,
            assignment.island_id,
            purpose,
            assignment.target_position,
            objectives.clone(),
        );

        let operation = self
            .operations
            .get_mut(&operation_id)
            .expect("作成済みStrategicOperation");
        // purposeは作戦identityではなく、同じ作戦内の可変な戦術状態として更新する。
        operation.purpose = purpose;
        operation.last_observed_turn = turn;
        if operation.tactical_anchor != assignment.target_position {
            // 所有権変化・損耗・敵の再占領で最短前線が変わった事実を、同じ作戦IDの
            // 再計画として記録する。作戦を捨てず、次の局地目標と護衛計画へ差し替える。
            operation.replan_count = operation.replan_count.saturating_add(1);
            operation.last_replan_turn = Some(turn);
        }
        operation.tactical_anchor = assignment.target_position;
        // 作戦identityと実績は維持するが、Milestoneの目標集合は現在の波へ差し替える。
        // 成功済み施設を永続追加すると局地戦が全島制圧まで閉じないゾンビ作戦になる。
        operation.objective_properties = objectives;
        operation.owned_objective_count = operation
            .objective_properties
            .iter()
            .filter(|position| owned_properties.contains(position))
            .count();
        operation.assigned_transports = assignment.transport_entities.iter().copied().collect();
        operation.assigned_capturers = assignment.capture_entities.iter().copied().collect();
        operation.assigned_combat = assignment.combat_entities.iter().copied().collect();
        operation.active = true;
        operation.blocked_reason = None;
        operation.phase = if !operation.objective_properties.is_empty()
            && operation.owned_objective_count == operation.objective_properties.len()
        {
            operation.actual_completion_turn.get_or_insert(turn);
            OperationPhase::Completed
        } else {
            operation.actual_completion_turn = None;
            phase
        };
        operation_id
    }

    /// 施設所有の変化を、現在portfolioに現れない完了済みNodeにも反映する。
    ///
    /// 兵站島を確保した後にportfolioから外れても、そのNodeは後続の前提として
    /// `Secured` のまま残る。一方で敵に取り返された場合は完了実績を取り消し、
    /// 当ターンに再提案されていれば `Ready`、されていなければ `Locked` へ戻す。
    fn refresh_objective_ownership(
        &mut self,
        player_id: PlayerId,
        owned_properties: &HashSet<GridPosition>,
    ) {
        for operation in self
            .operations
            .values_mut()
            .filter(|operation| operation.player_id == player_id)
        {
            operation.owned_objective_count = operation
                .objective_properties
                .iter()
                .filter(|position| owned_properties.contains(position))
                .count();
            let objectives_secured = !operation.objective_properties.is_empty()
                && operation.owned_objective_count == operation.objective_properties.len();
            if !objectives_secured && operation.phase == OperationPhase::Completed {
                operation.phase = OperationPhase::Forming;
                operation.actual_completion_turn = None;
            }
        }
    }

    /// IslandCampaignと敵首都島の地上DAGを、実際の兵站経路順で接続する。
    ///
    /// `V4LogisticsPlan::selected_islands` は自軍側から敵首都側へ並んでいる。各島の
    /// Regional Operationをその順に結び、最後の兵站島と敵首都島の前段Regional
    /// Operationを `AssaultCapital` の前提にする。未割当の島はOperationを捏造せず、
    /// 次回analyzerが公開するまで辺から外す。
    fn rebuild_dependencies(
        &mut self,
        player_id: PlayerId,
        capital_island: Option<IslandId>,
        logistics_plan: Option<&crate::ai::v4::logistics_plan::V4LogisticsPlan>,
    ) {
        let regional_for = |island_id| {
            self.operation_keys
                .get(&(player_id, island_id, StrategicOperationScope::Regional))
                .copied()
        };
        let capital_operation = capital_island.and_then(|island_id| {
            self.operation_keys
                .get(&(player_id, island_id, StrategicOperationScope::Capital))
                .copied()
        });
        let mut dependencies = HashSet::new();
        let mut previous = None;
        if let Some(plan) = logistics_plan {
            for island_id in &plan.selected_islands {
                let Some(current) = regional_for(*island_id) else {
                    continue;
                };
                if let Some(predecessor) = previous
                    && predecessor != current
                {
                    dependencies.insert(RoadmapDependency {
                        predecessor,
                        successor: current,
                        kind: RoadmapDependencyKind::Logistics,
                    });
                }
                previous = Some(current);
            }
        }
        if let (Some(capital_island), Some(capital_operation)) = (capital_island, capital_operation)
        {
            if let Some(predecessor) = previous
                && predecessor != capital_operation
            {
                dependencies.insert(RoadmapDependency {
                    predecessor,
                    successor: capital_operation,
                    kind: RoadmapDependencyKind::Logistics,
                });
            }
            if let Some(predecessor) = regional_for(capital_island)
                && predecessor != capital_operation
            {
                dependencies.insert(RoadmapDependency {
                    predecessor,
                    successor: capital_operation,
                    kind: RoadmapDependencyKind::CapitalRoute,
                });
            }
        }
        let mut dependencies = dependencies.into_iter().collect::<Vec<_>>();
        dependencies.sort_unstable_by_key(|edge| {
            (
                edge.predecessor.0,
                edge.successor.0,
                match edge.kind {
                    RoadmapDependencyKind::Logistics => 0_u8,
                    RoadmapDependencyKind::CapitalRoute => 1_u8,
                },
            )
        });
        if let Some(roadmap) = self.roadmaps.get_mut(&player_id) {
            roadmap.dependencies = dependencies;
        }
    }

    /// Operation自身の進捗だけから得られる状態を返す。
    ///
    /// 首都作戦の「まだscheduleがない」は、前提Nodeの未解放を意味し得る待機理由で
    /// あって失敗ではない。実行stepの失敗や到達不能だけを `Blocked` として扱う。
    fn local_node_state(operation: &StrategicOperation) -> RoadmapNodeState {
        let objectives_secured = !operation.objective_properties.is_empty()
            && operation.owned_objective_count == operation.objective_properties.len();
        if operation.phase == OperationPhase::Completed || objectives_secured {
            return RoadmapNodeState::Secured;
        }
        if !operation.active {
            return RoadmapNodeState::Locked;
        }
        let waiting_for_capital_schedule = operation.purpose == StrategicPurpose::AssaultCapital
            && operation.phase == OperationPhase::Forming
            && operation.blocked_reason.as_deref()
                == Some("no executable capital assault schedule has been selected");
        if operation.phase == OperationPhase::Blocked
            || (operation.blocked_reason.is_some() && !waiting_for_capital_schedule)
        {
            return RoadmapNodeState::Blocked;
        }
        match operation.phase {
            OperationPhase::Capture => RoadmapNodeState::Capturing,
            OperationPhase::Suppress => {
                if !operation.assigned_combat.is_empty()
                    && operation.planned_suppression_turn.is_some()
                {
                    RoadmapNodeState::Dominant
                } else {
                    RoadmapNodeState::Contested
                }
            }
            OperationPhase::Forming
            | OperationPhase::Pickup
            | OperationPhase::Transit
            | OperationPhase::Drop
            | OperationPhase::Hold => RoadmapNodeState::Ready,
            OperationPhase::Completed | OperationPhase::Blocked => {
                unreachable!("完了・阻害状態は先に処理済み")
            }
        }
    }

    /// 前提Nodeの状態を適用して、実際に公開する Roadmap Node 状態を更新する。
    ///
    /// すべての前提が `Dominant` または `Secured` であるときだけ後続は局地状態を
    /// 公開できる。分岐の合流は安全側に全前提必須とし、複数前提の意味づけを
    /// 暗黙に「どれか一つでよい」としない。
    fn refresh_node_states(&mut self, player_id: PlayerId) {
        let mut states = self
            .operations
            .iter()
            .filter(|(_, operation)| operation.player_id == player_id)
            .map(|(id, operation)| (*id, Self::local_node_state(operation)))
            .collect::<HashMap<_, _>>();
        let dependencies = self
            .roadmaps
            .get(&player_id)
            .map(|roadmap| roadmap.dependencies.clone())
            .unwrap_or_default();
        let mut predecessors = HashMap::<StrategicOperationId, Vec<StrategicOperationId>>::new();
        for edge in dependencies {
            predecessors
                .entry(edge.successor)
                .or_default()
                .push(edge.predecessor);
        }
        for (successor, requirements) in predecessors {
            let Some(current) = states.get(&successor).copied() else {
                continue;
            };
            if matches!(
                current,
                RoadmapNodeState::Secured | RoadmapNodeState::Blocked
            ) {
                continue;
            }
            let predecessors_ready = requirements.iter().all(|predecessor| {
                states.get(predecessor).is_some_and(|state| {
                    matches!(
                        state,
                        RoadmapNodeState::Dominant | RoadmapNodeState::Secured
                    )
                })
            });
            if !predecessors_ready {
                states.insert(successor, RoadmapNodeState::Locked);
            }
        }
        for (operation_id, state) in states {
            if let Some(operation) = self.operations.get_mut(&operation_id) {
                operation.node_state = state;
            }
        }
    }

    /// 前提未達のNodeを除いた、SquadReconcilerへ渡す当ターンのPortfolioを作る。
    ///
    /// analyzerは将来の島作戦を同時に提案できるが、ここでLocked assignmentを落とす
    /// ことで、前提Nodeが優勢になる前に後続Squadや輸送便が組まれるのを防ぐ。
    fn approved_portfolio(
        &self,
        player_id: PlayerId,
        enemy_capital: Option<GridPosition>,
        portfolio: &IslandCampaignPortfolio,
    ) -> IslandCampaignPortfolio {
        let assignment_is_unlocked = |assignment: &IslandCampaignAssignment| {
            let purpose = purpose_for(assignment, enemy_capital);
            self.operation_keys
                .get(&(player_id, assignment.island_id, operation_scope(purpose)))
                .and_then(|operation_id| self.operations.get(operation_id))
                .is_none_or(|operation| operation.node_state != RoadmapNodeState::Locked)
        };
        let mut approved = portfolio.clone();
        approved
            .defenses
            .retain(|assignment| assignment_is_unlocked(assignment));
        approved
            .active_offensives
            .retain(|assignment| assignment_is_unlocked(assignment));
        approved
    }

    fn bind_assignment_entities(
        &mut self,
        operation_id: StrategicOperationId,
        assignment: &IslandCampaignAssignment,
    ) {
        for entity in &assignment.transport_entities {
            self.bind_entity_exclusive(operation_id, *entity, OperationEntityRole::Transport);
        }
        for entity in &assignment.capture_entities {
            self.bind_entity_exclusive(operation_id, *entity, OperationEntityRole::Capture);
        }
        for entity in &assignment.combat_entities {
            self.bind_entity_exclusive(operation_id, *entity, OperationEntityRole::Combat);
        }
    }

    /// Roadmap監査側もEntityを複数作戦へ残さない。旧作戦の逆集合から同時に除去する。
    fn bind_entity_exclusive(
        &mut self,
        operation_id: StrategicOperationId,
        entity: Entity,
        role: OperationEntityRole,
    ) {
        if let Some(previous) = self.entity_bindings.get(&entity).copied()
            && previous.operation_id != operation_id
            && let Some(operation) = self.operations.get_mut(&previous.operation_id)
        {
            operation.assigned_transports.remove(&entity);
            operation.assigned_capturers.remove(&entity);
            operation.assigned_combat.remove(&entity);
        }
        if let Some(operation) = self.operations.get_mut(&operation_id) {
            operation.assigned_transports.remove(&entity);
            operation.assigned_capturers.remove(&entity);
            operation.assigned_combat.remove(&entity);
            match role {
                OperationEntityRole::Transport => {
                    operation.assigned_transports.insert(entity);
                }
                OperationEntityRole::Capture => {
                    operation.assigned_capturers.insert(entity);
                }
                OperationEntityRole::Combat => {
                    operation.assigned_combat.insert(entity);
                }
            }
        }
        self.entity_bindings
            .insert(entity, EntityOperationBinding { operation_id, role });
    }

    /// inactive作戦は実績履歴だけを残し、現在のEntity割当を所有し続けない。
    fn release_inactive_assignments(&mut self, player_id: PlayerId) {
        for operation in self
            .operations
            .values_mut()
            .filter(|operation| operation.player_id == player_id && !operation.active)
        {
            operation.assigned_transports.clear();
            operation.assigned_capturers.clear();
            operation.assigned_combat.clear();
        }
        self.entity_bindings.retain(|_, binding| {
            self.operations
                .get(&binding.operation_id)
                .is_some_and(|operation| operation.active || operation.player_id != player_id)
        });
    }

    fn replace_operation_issues(
        &mut self,
        operation_id: StrategicOperationId,
        issues: Vec<OperationIssue>,
        recoveries: Vec<OperationRecoveryAction>,
    ) {
        let Some(operation) = self.operations.get_mut(&operation_id) else {
            return;
        };
        for issue in &issues {
            let already_recorded = operation.issue_history.iter().any(|recorded| {
                recorded.kind == issue.kind
                    && recorded.detected_turn == issue.detected_turn
                    && recorded.entity == issue.entity
                    && recorded.related_entity == issue.related_entity
            });
            if !already_recorded {
                operation.issue_history.push(issue.clone());
            }
        }
        for recovery in recoveries {
            let already_recorded = operation.recovery_history.iter().any(|recorded| {
                recorded.kind == recovery.kind
                    && recorded.cause == recovery.cause
                    && recorded.completed_turn == recovery.completed_turn
                    && recorded.entity == recovery.entity
            });
            if !already_recorded {
                operation.replan_count = operation.replan_count.saturating_add(1);
                operation.last_replan_turn = Some(recovery.completed_turn);
                operation.recovery_history.push(recovery);
            }
        }
        operation.current_issues = issues;
    }

    fn record_destroyed_entity(&mut self, entity: Entity, turn: u32) {
        let Some(binding) = self.entity_bindings.get(&entity).copied() else {
            return;
        };
        let kind = match binding.role {
            OperationEntityRole::Transport => OperationIssueKind::TransportDestroyed,
            OperationEntityRole::Capture => OperationIssueKind::CapturerDestroyed,
            OperationEntityRole::Combat => OperationIssueKind::CombatUnitDestroyed,
        };
        let issue = OperationIssue {
            kind,
            detected_turn: turn,
            entity: Some(entity),
            related_entity: None,
            detail: format!(
                "assigned {:?} Entity {} was destroyed",
                binding.role,
                entity.to_bits()
            ),
        };
        if let Some(operation) = self.operations.get_mut(&binding.operation_id) {
            operation.current_issues.push(issue.clone());
            operation.issue_history.push(issue);
            operation.phase = OperationPhase::Blocked;
        }

        if binding.role == OperationEntityRole::Transport {
            let cargo = self.transport_manifests.remove(&entity).unwrap_or_default();
            for cargo_entity in cargo {
                let cargo_issue = OperationIssue {
                    kind: OperationIssueKind::CargoLostWithTransport,
                    detected_turn: turn,
                    entity: Some(cargo_entity),
                    related_entity: Some(entity),
                    detail: format!(
                        "cargo Entity {} was lost with transport Entity {}",
                        cargo_entity.to_bits(),
                        entity.to_bits()
                    ),
                };
                if let Some(operation) = self.operations.get_mut(&binding.operation_id) {
                    operation.current_issues.push(cargo_issue.clone());
                    operation.issue_history.push(cargo_issue);
                }
            }
        }
    }

    fn record_move(
        &mut self,
        entity: Entity,
        from: GridPosition,
        to: GridPosition,
        turn: u32,
        island_map: &IslandMap,
    ) {
        let Some(binding) = self.entity_bindings.get(&entity).copied() else {
            return;
        };
        let had_pending_step = self.pending_steps.contains_key(&entity);
        let planned_move = self.observe_planned_move(entity, to, turn);
        let source_island = island_map.get_island_at(&from).map(|island| island.id);
        let destination_island = island_map.get_island_at(&to).map(|island| island.id);
        if let Some(operation) = self.operations.get_mut(&binding.operation_id)
            && binding.role != OperationEntityRole::Transport
            // 出発島で搭載地点へ寄る移動は逸脱ではない。実際に目的島上にいたEntityが
            // 目的島外へ出た場合だけを逸脱とし、作戦全体のphaseで推測しない。
            && source_island == Some(operation.island_id)
            && destination_island.is_some_and(|island| island != operation.island_id)
        {
            operation.execution.deviations = operation.execution.deviations.saturating_add(1);
            operation.phase = OperationPhase::Blocked;
            operation.blocked_reason = Some(format!(
                "Entity {} moved to island {} outside operation island {}",
                entity.to_bits(),
                destination_island.map_or(usize::MAX, |island| island.0),
                operation.island_id.0
            ));
        }
        if had_pending_step
            && !planned_move
            && let Some(operation) = self.operations.get_mut(&binding.operation_id)
        {
            operation.execution.deviations = operation.execution.deviations.saturating_add(1);
        }
    }
}

/// 実際に発行するAI命令を、担当StrategicOperationの予定stepとして先に登録する。
/// 結果Eventは同じEntityキーで照合し、命令を出しただけでは作戦進捗にしない。
pub(crate) fn record_operation_command(world: &mut World, entity: Entity, command: &AiCommand) {
    let turn = world
        .get_resource::<MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let origin = world.get::<GridPosition>(entity).copied();
    let transport_phase = world
        .get_resource::<SquadManager>()
        .and_then(|manager| {
            manager.squads.iter().find(|squad| {
                squad.members.contains(&entity)
                    || squad.transport_entity == Some(entity)
                    || squad.cargo_entities.contains(&entity)
            })
        })
        .and_then(|squad| match squad.phase {
            MissionPhase::Transport(phase) => Some(phase),
            _ => None,
        });
    let specification = match command {
        AiCommand::Attack {
            target_pos,
            target_entity,
        } => Some((
            CampaignStepKind::Attack,
            CampaignStepKind::Attack,
            Some(*target_pos),
            Some(*target_entity),
        )),
        AiCommand::Capture { target_pos } => Some((
            CampaignStepKind::Capture,
            CampaignStepKind::Capture,
            Some(*target_pos),
            None,
        )),
        AiCommand::Wait { target_pos } => {
            let moved = origin.is_some_and(|position| position != *target_pos);
            let step = if moved
                && matches!(
                    transport_phase,
                    Some(TransportPhase::Transit | TransportPhase::Drop)
                ) {
                CampaignStepKind::Transit
            } else if moved {
                CampaignStepKind::Move
            } else {
                CampaignStepKind::Hold
            };
            Some((step, CampaignStepKind::Wait, Some(*target_pos), None))
        }
        AiCommand::Load {
            target_pos,
            transport_entity,
        } => Some((
            CampaignStepKind::Load,
            CampaignStepKind::Load,
            Some(*target_pos),
            Some(*transport_entity),
        )),
        AiCommand::Drop {
            transport_target_pos,
            cargo_entity,
            ..
        } => Some((
            CampaignStepKind::Drop,
            CampaignStepKind::Drop,
            Some(*transport_target_pos),
            Some(*cargo_entity),
        )),
        AiCommand::Supply {
            target_pos,
            target_entity,
        } => Some((
            CampaignStepKind::Supply,
            CampaignStepKind::Supply,
            Some(*target_pos),
            Some(*target_entity),
        )),
        // Mergeは作戦工程ではなく損耗unitの統合であり、予定進捗に数えない。
        AiCommand::Merge { .. } => None,
    };
    let Some((step, terminal_event, target_position, target_entity)) = specification else {
        return;
    };
    if let Some(mut registry) = world.get_resource_mut::<VictoryRoadmapRegistry>() {
        registry.plan_entity_step(
            entity,
            turn,
            step,
            terminal_event,
            target_position,
            target_entity,
        );
    }
}

fn purpose_for(
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

fn operation_entity_is_alive(world: &World, entity: Entity) -> bool {
    world.get_entity(entity).is_ok()
}

fn entity_is_owned_by_island_squad(
    manager: &SquadManager,
    player_id: PlayerId,
    island_id: IslandId,
    entity: Entity,
) -> bool {
    manager.squads.iter().any(|squad| {
        squad.owner_id == Some(player_id)
            && squad.target_island == Some(island_id)
            && (squad.members.contains(&entity)
                || squad.cargo_entities.contains(&entity)
                || squad.delivered_cargo.contains(&entity))
    })
}

#[allow(clippy::too_many_arguments)]
fn diagnose_operation_execution(
    world: &World,
    manager: &SquadManager,
    player_id: PlayerId,
    turn: u32,
    assignment: &IslandCampaignAssignment,
    previous_entities: &[(Entity, OperationEntityRole)],
    persistent_combat_entities: &HashSet<Entity>,
    production_records: &[crate::ai::v4::campaign_execution::CampaignProductionRecord],
) -> Vec<OperationIssue> {
    use crate::ai::v4::campaign_execution::{CampaignProductionRole, CampaignProductionStatus};

    let current_entities = assignment
        .transport_entities
        .iter()
        .chain(assignment.capture_entities.iter())
        .chain(assignment.combat_entities.iter())
        .chain(persistent_combat_entities.iter())
        .copied()
        .collect::<HashSet<_>>();
    let mut issues = Vec::new();

    for (entity, role) in previous_entities {
        if !operation_entity_is_alive(world, *entity) {
            let kind = match role {
                OperationEntityRole::Transport => OperationIssueKind::TransportDestroyed,
                OperationEntityRole::Capture => OperationIssueKind::CapturerDestroyed,
                OperationEntityRole::Combat => OperationIssueKind::CombatUnitDestroyed,
            };
            issues.push(OperationIssue {
                kind,
                detected_turn: turn,
                entity: Some(*entity),
                related_entity: None,
                detail: format!(
                    "previously assigned {:?} Entity {} no longer exists",
                    role,
                    entity.to_bits()
                ),
            });
        } else if !current_entities.contains(entity)
            && !entity_is_owned_by_island_squad(manager, player_id, assignment.island_id, *entity)
        {
            issues.push(OperationIssue {
                kind: OperationIssueKind::AssignmentLost,
                detected_turn: turn,
                entity: Some(*entity),
                related_entity: None,
                detail: format!(
                    "live {:?} Entity {} disappeared from the operation assignment",
                    role,
                    entity.to_bits()
                ),
            });
        }
    }

    let island_map = world.get_resource::<IslandMap>();
    for entity in &assignment.capture_entities {
        if !operation_entity_is_alive(world, *entity) {
            continue;
        }
        let landed = world.get::<GridPosition>(*entity).is_some_and(|position| {
            island_map
                .and_then(|map| map.get_island_at(position))
                .is_some_and(|island| island.id == assignment.island_id)
        });
        if landed || world.get::<Transporting>(*entity).is_some() {
            continue;
        }
        let assigned_transport = manager.squads.iter().any(|squad| {
            squad.owner_id == Some(player_id)
                && squad.target_island == Some(assignment.island_id)
                && squad.cargo_entities.contains(entity)
                && squad.transport_entity.is_some()
        });
        if !assigned_transport {
            issues.push(OperationIssue {
                kind: OperationIssueKind::TransportUnassigned,
                detected_turn: turn,
                entity: Some(*entity),
                related_entity: None,
                detail: format!(
                    "capture Entity {} is alive but has no transport for island {}",
                    entity.to_bits(),
                    assignment.island_id.0
                ),
            });
        }
    }

    // 同じ役割の再発注がある場合、過去の遅延ではなく最新の発注状態を現在原因にする。
    for role in [
        CampaignProductionRole::Transport,
        CampaignProductionRole::Capture,
        CampaignProductionRole::Combat,
    ] {
        let Some(record) = production_records
            .iter()
            .filter(|record| record.role == role)
            .max_by_key(|record| record.planned_turn)
        else {
            continue;
        };
        let issue = match record.status {
            CampaignProductionStatus::Planned | CampaignProductionStatus::Issued => {
                Some((OperationIssueKind::AwaitingProduction, None))
            }
            CampaignProductionStatus::Delayed => {
                Some((OperationIssueKind::ProductionDelayed, None))
            }
            CampaignProductionStatus::Produced => {
                let entity = record.entity;
                entity
                    .filter(|entity| {
                        !current_entities.contains(entity)
                            && !entity_is_owned_by_island_squad(
                                manager,
                                player_id,
                                assignment.island_id,
                                *entity,
                            )
                    })
                    .map(|entity| (OperationIssueKind::ProducedEntityUnassigned, Some(entity)))
            }
            CampaignProductionStatus::Lost | CampaignProductionStatus::Assigned => None,
        };
        if let Some((kind, entity)) = issue {
            issues.push(OperationIssue {
                kind,
                detected_turn: turn,
                entity,
                related_entity: None,
                detail: format!(
                    "campaign {:?} production at ({},{}) is {:?}",
                    role, record.facility_x, record.facility_y, record.status
                ),
            });
        }
    }

    issues.sort_by_key(|issue| {
        (
            format!("{:?}", issue.kind),
            issue.entity.map_or(u64::MAX, Entity::to_bits),
        )
    });
    issues.dedup_by(|left, right| {
        left.kind == right.kind
            && left.entity == right.entity
            && left.related_entity == right.related_entity
    });
    issues
}

#[allow(clippy::too_many_arguments)]
fn diagnose_operation_recoveries(
    world: &World,
    manager: &SquadManager,
    player_id: PlayerId,
    turn: u32,
    assignment: &IslandCampaignAssignment,
    previous_issues: &[OperationIssue],
    persistent_combat_entities: &HashSet<Entity>,
    production_records: &[crate::ai::v4::campaign_execution::CampaignProductionRecord],
) -> Vec<OperationRecoveryAction> {
    use crate::ai::v4::campaign_execution::{CampaignProductionRole, CampaignProductionStatus};

    let assigned_entities = assignment
        .transport_entities
        .iter()
        .chain(assignment.capture_entities.iter())
        .chain(assignment.combat_entities.iter())
        .chain(persistent_combat_entities.iter())
        .copied()
        .collect::<HashSet<_>>();
    let mut recoveries = Vec::new();
    for issue in previous_issues {
        let recovery = match issue.kind {
            OperationIssueKind::AwaitingProduction => None,
            // 新しい命令を実際に発行した時点でplan_entity_stepがReplanStepを記録する。
            OperationIssueKind::StepBlocked => None,
            OperationIssueKind::ProductionDelayed => production_records
                .iter()
                .filter(|record| record.planned_turn >= issue.detected_turn)
                .filter(|record| {
                    matches!(
                        record.status,
                        CampaignProductionStatus::Planned
                            | CampaignProductionStatus::Issued
                            | CampaignProductionStatus::Produced
                            | CampaignProductionStatus::Assigned
                    )
                })
                .max_by_key(|record| record.planned_turn)
                .map(|record| OperationRecoveryAction {
                    kind: OperationRecoveryKind::RetryProduction,
                    cause: issue.kind,
                    completed_turn: turn,
                    entity: record.entity,
                    detail: format!(
                        "retried {:?} production for island {} on turn {}",
                        record.role, assignment.island_id.0, record.planned_turn
                    ),
                }),
            OperationIssueKind::ProducedEntityUnassigned | OperationIssueKind::AssignmentLost => {
                issue.entity.and_then(|entity| {
                    (assigned_entities.contains(&entity)
                        || entity_is_owned_by_island_squad(
                            manager,
                            player_id,
                            assignment.island_id,
                            entity,
                        ))
                    .then(|| OperationRecoveryAction {
                        kind: OperationRecoveryKind::RestoreAssignment,
                        cause: issue.kind,
                        completed_turn: turn,
                        entity: Some(entity),
                        detail: format!(
                            "restored Entity {} to island {} operation",
                            entity.to_bits(),
                            assignment.island_id.0
                        ),
                    })
                })
            }
            OperationIssueKind::TransportUnassigned => issue.entity.and_then(|entity| {
                let loaded = world.get::<Transporting>(entity).is_some();
                let assigned_transport = manager.squads.iter().any(|squad| {
                    squad.owner_id == Some(player_id)
                        && squad.target_island == Some(assignment.island_id)
                        && squad.cargo_entities.contains(&entity)
                        && squad.transport_entity.is_some()
                });
                (loaded || assigned_transport).then(|| OperationRecoveryAction {
                    kind: OperationRecoveryKind::AssignTransport,
                    cause: issue.kind,
                    completed_turn: turn,
                    entity: Some(entity),
                    detail: format!(
                        "assigned transport to capture Entity {} for island {}",
                        entity.to_bits(),
                        assignment.island_id.0
                    ),
                })
            }),
            OperationIssueKind::TransportDestroyed
            | OperationIssueKind::CapturerDestroyed
            | OperationIssueKind::CombatUnitDestroyed
            | OperationIssueKind::CargoLostWithTransport => {
                let role = match issue.kind {
                    OperationIssueKind::TransportDestroyed => CampaignProductionRole::Transport,
                    OperationIssueKind::CombatUnitDestroyed => CampaignProductionRole::Combat,
                    OperationIssueKind::CapturerDestroyed
                    | OperationIssueKind::CargoLostWithTransport => CampaignProductionRole::Capture,
                    _ => unreachable!(),
                };
                production_records
                    .iter()
                    .filter(|record| record.role == role)
                    .filter(|record| record.planned_turn >= issue.detected_turn)
                    .filter(|record| {
                        !matches!(
                            record.status,
                            CampaignProductionStatus::Delayed | CampaignProductionStatus::Lost
                        )
                    })
                    .max_by_key(|record| record.planned_turn)
                    .map(|record| OperationRecoveryAction {
                        kind: OperationRecoveryKind::RequestReplacement,
                        cause: issue.kind,
                        completed_turn: turn,
                        entity: record.entity,
                        detail: format!(
                            "requested replacement {:?} for island {} on turn {}",
                            role, assignment.island_id.0, record.planned_turn
                        ),
                    })
            }
        };
        if let Some(recovery) = recovery {
            recoveries.push(recovery);
        }
    }
    recoveries.sort_by_key(|recovery| {
        (
            format!("{:?}", recovery.kind),
            format!("{:?}", recovery.cause),
            recovery.entity.map_or(u64::MAX, Entity::to_bits),
        )
    });
    recoveries.dedup_by(|left, right| {
        left.kind == right.kind && left.cause == right.cause && left.entity == right.entity
    });
    recoveries
}

fn operation_phase(
    world: &World,
    island_map: &IslandMap,
    assignment: &IslandCampaignAssignment,
    manager: &SquadManager,
    player_id: PlayerId,
) -> OperationPhase {
    let transport_phase = manager
        .squads
        .iter()
        .filter(|squad| {
            squad.owner_id == Some(player_id)
                && squad.mission_type == MissionType::Transport
                && squad.target_island == Some(assignment.island_id)
        })
        .filter_map(|squad| match squad.phase {
            MissionPhase::Transport(phase) => Some(phase),
            _ => None,
        })
        .next();
    let entity_is_deployed = |entity: &Entity| {
        world.get::<Transporting>(*entity).is_none()
            && world.get::<GridPosition>(*entity).is_some_and(|position| {
                island_map
                    .get_island_at(position)
                    .is_some_and(|island| island.id == assignment.island_id)
            })
    };
    let combat_is_deployed = assignment.combat_entities.iter().any(&entity_is_deployed);
    let capturer_is_deployed = assignment.capture_entities.iter().any(entity_is_deployed);
    match transport_phase {
        Some(TransportPhase::Pickup) => OperationPhase::Pickup,
        Some(TransportPhase::Transit) | Some(TransportPhase::Return) => OperationPhase::Transit,
        Some(TransportPhase::Drop) => OperationPhase::Drop,
        None if combat_is_deployed => OperationPhase::Suppress,
        None if capturer_is_deployed => OperationPhase::Capture,
        None => OperationPhase::Forming,
    }
}

#[derive(Debug, Clone)]
struct CapturerForecast {
    position: GridPosition,
    stats: UnitStats,
    health: Health,
    /// 現在手番から数え、島内で行動可能になるまでの手番数。
    available_turn: u32,
}

struct CaptureRouteContext<'a> {
    map: &'a Map,
    master_data: &'a MasterDataRegistry,
    occupied: &'a HashMap<(usize, usize), OccupantInfo>,
    player_id: PlayerId,
    cache: TurnDistanceCache,
}

impl CaptureRouteContext<'_> {
    fn distance(
        &mut self,
        start: GridPosition,
        target: GridPosition,
        stats: &UnitStats,
    ) -> Option<u32> {
        let distance = calculate_turn_distance(
            self.map,
            self.master_data,
            self.occupied,
            (start.x, start.y),
            (target.x, target.y),
            stats.movement_type,
            stats.max_movement.max(1),
            0,
            self.player_id,
            &mut self.cache,
        );
        (distance.turns != u32::MAX).then_some(distance.turns)
    }
}

fn capture_power(health: Health) -> u32 {
    health
        .current
        .saturating_add(9)
        .div_ceil(10)
        .saturating_mul(10)
}

/// 担当占領兵を全未所有拠点へ分担し、最後の拠点を取り終える絶対手番を返す。
/// 移動・搭載・降車のどれかを実Entityから説明できない場合は楽観値を返さない。
#[allow(clippy::too_many_arguments)]
fn estimate_full_objective_completion(
    world: &mut World,
    player_id: PlayerId,
    turn: u32,
    island_id: IslandId,
    assignment: &IslandCampaignAssignment,
    objectives: &[GridPosition],
    property_snapshots: &[(GridPosition, Property)],
    island_map: &IslandMap,
    manager: &SquadManager,
    suppression_turn: Option<u32>,
) -> Result<u32, String> {
    let mut remaining = property_snapshots
        .iter()
        .filter(|(position, property)| {
            objectives.contains(position) && property.owner_id != Some(player_id)
        })
        .copied()
        .collect::<Vec<_>>();
    if remaining.is_empty() {
        return Ok(turn);
    }

    let mut occupied = HashMap::new();
    let mut unit_query = world.query::<(
        Entity,
        &GridPosition,
        &Faction,
        &UnitStats,
        Option<&CargoCapacity>,
        Option<&Transporting>,
    )>();
    for (_, position, faction, stats, capacity, transporting) in unit_query.iter(world) {
        if transporting.is_some() {
            continue;
        }
        occupied.insert(
            (position.x, position.y),
            OccupantInfo {
                player_id: faction.0,
                is_transport: stats.max_cargo > 0,
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots: capacity
                    .map(|capacity| capacity.max.saturating_sub(capacity.loaded.len() as u32))
                    .unwrap_or(0),
            },
        );
    }

    let map = world
        .get_resource::<Map>()
        .ok_or_else(|| "map resource missing".to_owned())?;
    let master_data = world
        .get_resource::<MasterDataRegistry>()
        .ok_or_else(|| "master data missing".to_owned())?;
    let mut route = CaptureRouteContext {
        map,
        master_data,
        occupied: &occupied,
        player_id,
        cache: TurnDistanceCache::default(),
    };
    let suppression_delay = suppression_turn.unwrap_or(turn).saturating_sub(turn);

    let mut capturer_entities = assignment
        .capture_entities
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    for squad in manager
        .squads
        .iter()
        .filter(|squad| squad.owner_id == Some(player_id) && squad.target_island == Some(island_id))
    {
        for entity in squad
            .cargo_entities
            .iter()
            .chain(squad.delivered_cargo.iter())
        {
            if world
                .get::<UnitStats>(*entity)
                .is_some_and(|stats| stats.can_capture)
            {
                capturer_entities.insert(*entity);
            }
        }
    }

    let mut capturers = Vec::new();
    for entity in capturer_entities {
        let Some(stats) = world.get::<UnitStats>(entity).cloned() else {
            continue;
        };
        let Some(health) = world.get::<Health>(entity).copied() else {
            continue;
        };
        if !stats.can_capture || capture_power(health) == 0 {
            continue;
        }
        let transporting = world.get::<Transporting>(entity).copied();
        let current_position = world.get::<GridPosition>(entity).copied();
        if transporting.is_none()
            && current_position.is_some_and(|position| {
                island_map
                    .get_island_at(&position)
                    .is_some_and(|island| island.id == island_id)
            })
        {
            capturers.push(CapturerForecast {
                position: current_position.expect("is_someで確認済み"),
                stats,
                health,
                available_turn: suppression_delay,
            });
            continue;
        }

        let squad = manager.squads.iter().find(|squad| {
            squad.owner_id == Some(player_id)
                && squad.target_island == Some(island_id)
                && (squad.cargo_entities.contains(&entity)
                    || squad.delivered_cargo.contains(&entity))
        });
        let transport = transporting
            .map(|transporting| transporting.0)
            .or_else(|| squad.and_then(|squad| squad.transport_entity))
            .ok_or_else(|| format!("capture Entity {} has no transport", entity.to_bits()))?;
        let transport_position = world
            .get::<GridPosition>(transport)
            .copied()
            .ok_or_else(|| format!("transport Entity {} has no position", transport.to_bits()))?;
        let transport_stats = world
            .get::<UnitStats>(transport)
            .ok_or_else(|| format!("transport Entity {} has no stats", transport.to_bits()))?;
        let pickup = squad
            .and_then(|squad| squad.pickup_position)
            .unwrap_or(transport_position);
        let mut ready = 0_u32;
        let departure = if transporting.is_some() {
            transport_position
        } else {
            let cargo_position = current_position.ok_or_else(|| {
                format!("capture Entity {} has no pickup position", entity.to_bits())
            })?;
            let cargo_eta = route
                .distance(cargo_position, pickup, &stats)
                .ok_or_else(|| {
                    format!("capture Entity {} cannot reach pickup", entity.to_bits())
                })?;
            let transport_eta = route
                .distance(transport_position, pickup, transport_stats)
                .ok_or_else(|| {
                    format!(
                        "transport Entity {} cannot reach pickup",
                        transport.to_bits()
                    )
                })?;
            ready = cargo_eta.max(transport_eta).saturating_add(1);
            pickup
        };
        let transit = route
            .distance(departure, assignment.target_position, transport_stats)
            .ok_or_else(|| format!("transport Entity {} cannot reach drop", transport.to_bits()))?;
        ready = ready.saturating_add(transit).saturating_add(1);
        capturers.push(CapturerForecast {
            position: assignment.target_position,
            stats,
            health,
            available_turn: ready.max(suppression_delay),
        });
    }
    if capturers.is_empty() {
        return Err("no assigned capture Entity can execute the operation".to_owned());
    }

    remaining.sort_unstable_by_key(|(position, _)| (position.y, position.x));
    while !remaining.is_empty() {
        let mut best: Option<(u32, usize, usize, u32)> = None;
        for (worker_index, worker) in capturers.iter().enumerate() {
            for (property_index, (position, property)) in remaining.iter().enumerate() {
                let Some(move_turns) = route.distance(worker.position, *position, &worker.stats)
                else {
                    continue;
                };
                let turns_to_capture = property
                    .capture_points
                    .div_ceil(capture_power(worker.health));
                let completion = worker
                    .available_turn
                    .saturating_add(move_turns)
                    .saturating_add(turns_to_capture);
                let candidate = (completion, worker_index, property_index, move_turns);
                if best.is_none_or(|current| candidate < current) {
                    best = Some(candidate);
                }
            }
        }
        let Some((completion, worker_index, property_index, _)) = best else {
            return Err(
                "an objective property is unreachable by every assigned capturer".to_owned(),
            );
        };
        let (position, _) = remaining.remove(property_index);
        capturers[worker_index].position = position;
        capturers[worker_index].available_turn = completion;
    }
    Ok(turn.saturating_add(
        capturers
            .iter()
            .map(|worker| worker.available_turn)
            .max()
            .unwrap_or(0),
    ))
}

/// 毎ターンの島portfolioを、不変な勝利ロードマップと子作戦へ照合する。
pub(crate) fn reconcile_campaign_roadmap(
    world: &mut World,
    player_id: PlayerId,
    portfolio: &IslandCampaignPortfolio,
    manager: &SquadManager,
) {
    let turn = world
        .get_resource::<MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
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
    let Some(island_map) = world.get_resource::<IslandMap>().cloned() else {
        return;
    };
    let mut property_snapshots = Vec::new();
    let mut property_query = world.query::<(&GridPosition, &Property)>();
    for (position, property) in property_query.iter(world) {
        property_snapshots.push((*position, *property));
    }
    let enemy_capital = property_snapshots
        .iter()
        .find(|(_, property)| {
            property.terrain == Terrain::Capital && property.owner_id != Some(player_id)
        })
        .map(|(position, _)| *position);
    let enemy_capital_island = enemy_capital
        .and_then(|position| island_map.get_island_at(&position))
        .map(|island| island.id);
    let enemy_unit_count = {
        let mut units = world.query::<&Faction>();
        units
            .iter(world)
            .filter(|faction| faction.0 != player_id)
            .count()
    };
    let enemy_positions = {
        let mut units = world.query::<(&Faction, &GridPosition, Option<&Transporting>)>();
        units
            .iter(world)
            .filter_map(|(faction, position, transporting)| {
                (faction.0 != player_id && transporting.is_none()).then_some(*position)
            })
            .collect::<Vec<_>>()
    };
    let combat_plan_summaries = world
        .get_resource::<V4RollingPlanRegistry>()
        .map(|registry| registry.active_combat_plan_summaries(player_id))
        .unwrap_or_default();
    let logistics_plan = world
        .get_resource::<crate::ai::v4::logistics_plan::V4LogisticsPlanRegistry>()
        .and_then(|registry| registry.plan(player_id))
        .cloned();
    let deployment_records = world
        .get_resource::<V4DeploymentRegistry>()
        .map(|registry| registry.audit_records(player_id))
        .unwrap_or_default();
    let owned_properties = property_snapshots
        .iter()
        .filter_map(|(position, property)| {
            (property.owner_id == Some(player_id)).then_some(*position)
        })
        .collect::<HashSet<_>>();

    let mut registry = world
        .remove_resource::<VictoryRoadmapRegistry>()
        .unwrap_or_default();
    // 前手番に発行した命令の結果Eventが無ければ、再構築で黙って消さず
    // StepBlockedとして同じRoadmapの原因別再計画へ返す。
    registry.expire_pending_steps(player_id, turn);
    let roadmap_id = registry.ensure_roadmap(
        player_id,
        turn,
        enemy_capital,
        enemy_capital_island,
        enemy_unit_count,
    );
    for operation in registry
        .operations
        .values_mut()
        .filter(|operation| operation.player_id == player_id)
    {
        operation.active = false;
    }
    registry.entity_bindings.retain(|_, binding| {
        registry
            .operations
            .get(&binding.operation_id)
            .is_some_and(|op| op.player_id != player_id)
    });

    // 局地portfolioにまだ現れなくても、勝利条件そのものを親計画から消さない。
    // 実行schedule未選択は失敗ではなく、dependencyが満たされるまでLocked/Readyで待つ。
    if let (Some(capital), Some(capital_island)) = (enemy_capital, enemy_capital_island) {
        registry.ensure_capital_objective(
            roadmap_id,
            player_id,
            turn,
            capital_island,
            capital,
            owned_properties.contains(&capital),
        );
    }
    let assigned_regional_islands = portfolio
        .defenses
        .iter()
        .chain(portfolio.active_offensives.iter())
        .filter(|assignment| {
            purpose_for(assignment, enemy_capital) != StrategicPurpose::AssaultCapital
        })
        .map(|assignment| assignment.island_id)
        .collect::<HashSet<_>>();
    // `portfolio.islands` は作戦候補ではなく全島評価である。ここで全島を観測Nodeへ
    // 写し、allocatorが今手番にassignmentを作らなかった島もRoadmapから消さない。
    // 敵首都島は常在AssaultCapital Nodeが担当し、Regional Nodeは実際に前段の
    // Milestoneが提案されたときだけ同じscopeで追加される。
    for assessment in &portfolio.islands {
        if Some(assessment.island_id) == enemy_capital_island
            || assigned_regional_islands.contains(&assessment.island_id)
        {
            continue;
        }
        let mut objectives = property_snapshots
            .iter()
            .filter(|(position, property)| {
                property.max_capture_points > 0
                    && island_map
                        .get_island_at(position)
                        .is_some_and(|island| island.id == assessment.island_id)
            })
            .map(|(position, _)| *position)
            .collect::<Vec<_>>();
        objectives.sort_unstable_by_key(|position| (position.y, position.x));
        objectives.dedup();
        let tactical_anchor = objectives.first().copied().or_else(|| {
            island_map
                .islands
                .iter()
                .find(|island| island.id == assessment.island_id)
                .and_then(|island| {
                    let mut tiles = island.tiles.iter().copied().collect::<Vec<_>>();
                    tiles.sort_unstable_by_key(|position| (position.y, position.x));
                    tiles.first().copied()
                })
        });
        if let Some(tactical_anchor) = tactical_anchor {
            registry.reconcile_observed_island(
                roadmap_id,
                player_id,
                turn,
                assessment,
                tactical_anchor,
                objectives,
                &owned_properties,
            );
        }
    }
    // 首都親作戦は勝利条件として常在させるが、DAGの先頭未確保区間を取る間は
    // Regional子作戦が実Entityを所有する。親が島内の全Squadを再束縛すると、
    // 中間拠点のCombat枠がゼロになり、占領兵だけが敵前で停止する。
    let capital_has_active_regional_segment = enemy_capital_island.is_some_and(|capital_island| {
        portfolio
            .defenses
            .iter()
            .chain(portfolio.active_offensives.iter())
            .any(|assignment| {
                assignment.island_id == capital_island
                    && purpose_for(assignment, enemy_capital) != StrategicPurpose::AssaultCapital
            })
    });

    for assignment in portfolio
        .defenses
        .iter()
        .chain(portfolio.active_offensives.iter())
    {
        let purpose = purpose_for(assignment, enemy_capital);
        let operation_key = (player_id, assignment.island_id, operation_scope(purpose));
        let previous_entities = registry
            .operation_keys
            .get(&operation_key)
            .and_then(|operation_id| registry.operations.get(operation_id))
            .map(|operation| {
                operation
                    .assigned_transports
                    .iter()
                    .map(|entity| (*entity, OperationEntityRole::Transport))
                    .chain(
                        operation
                            .assigned_capturers
                            .iter()
                            .map(|entity| (*entity, OperationEntityRole::Capture)),
                    )
                    .chain(
                        operation
                            .assigned_combat
                            .iter()
                            .map(|entity| (*entity, OperationEntityRole::Combat)),
                    )
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let previous_issues = registry
            .operation_keys
            .get(&operation_key)
            .and_then(|operation_id| registry.operations.get(operation_id))
            .map(|operation| operation.current_issues.clone())
            .unwrap_or_default();
        let production_records = world
            .get_resource::<crate::ai::v4::campaign_execution::V4CampaignExecutionRegistry>()
            .map(|execution| {
                execution
                    .records_for(player_id, assignment.island_id)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let objectives = if purpose == StrategicPurpose::AssaultCapital {
            // 首都島でも首都1点だけへ縮約しない。IslandCampaignが観測した全未所有
            // 施設を同じ勝利作戦の前線目標として保持し、首都は終端目標として補う。
            let mut positions = assignment.capture_target_positions.clone();
            if let Some(capital) = enemy_capital
                && !positions.contains(&capital)
            {
                positions.push(capital);
            }
            positions
        } else {
            // 局地作戦は全島の施設一覧ではなく、portfolioが今選んだMilestoneだけを持つ。
            // 空集合でも防衛・集結地点を明示し、作戦所属Entityの行き先を失わせない。
            let mut positions = assignment.capture_target_positions.clone();
            if positions.is_empty() {
                positions.push(assignment.target_position);
            }
            positions
        };
        let phase = operation_phase(world, &island_map, assignment, manager, player_id);
        let expected_plan_kind = rolling_operation_kind(purpose);
        let matching_combat_plans = combat_plan_summaries
            .iter()
            .filter(|plan| {
                plan.kind == expected_plan_kind
                    && (island_map
                        .get_island_at(&plan.anchor)
                        .is_some_and(|island| island.id == assignment.island_id)
                        || plan.objective_properties.iter().any(|position| {
                            island_map
                                .get_island_at(position)
                                .is_some_and(|island| island.id == assignment.island_id)
                        }))
            })
            .collect::<Vec<&ActiveCombatPlanSummary>>();
        let local_enemy_count = enemy_positions
            .iter()
            .filter(|position| {
                island_map
                    .get_island_at(position)
                    .is_some_and(|island| island.id == assignment.island_id)
            })
            .count();
        let suppression_turn = if local_enemy_count == 0 {
            Some(turn)
        } else if !matching_combat_plans.is_empty()
            && matching_combat_plans.iter().all(|plan| {
                plan.remaining_target_count == 0 || plan.planned_elimination_turn.is_some()
            })
        {
            matching_combat_plans
                .iter()
                .filter(|plan| plan.remaining_target_count > 0)
                .filter_map(|plan| plan.planned_elimination_turn)
                .max()
                .or(Some(turn))
        } else {
            None
        };
        let completion_forecast = if local_enemy_count > 0 && suppression_turn.is_none() {
            Err(format!(
                "{} local enemies remain but no executable suppression forecast exists",
                local_enemy_count
            ))
        } else {
            estimate_full_objective_completion(
                world,
                player_id,
                turn,
                assignment.island_id,
                assignment,
                &objectives,
                &property_snapshots,
                &island_map,
                manager,
                suppression_turn,
            )
        };
        let operation_id = registry.reconcile_assignment(
            roadmap_id,
            player_id,
            turn,
            assignment,
            purpose,
            objectives,
            &owned_properties,
            phase,
        );
        let matching_plan_ids = matching_combat_plans
            .iter()
            .map(|plan| plan.plan_id)
            .collect::<HashSet<_>>();
        let persistent_combat_entities = deployment_records
            .iter()
            .filter(|record| record.active)
            .filter(|record| {
                record
                    .plan_step
                    .is_some_and(|step| matching_plan_ids.contains(&step.plan_id))
            })
            .map(|record| record.entity)
            .collect::<HashSet<_>>();
        if let Some(operation) = registry.operations.get_mut(&operation_id) {
            operation.combat_plan_ids = matching_plan_ids;
            operation.planned_suppression_turn = suppression_turn;
            match completion_forecast {
                Ok(completion_turn) => {
                    operation.planned_completion_turn = Some(completion_turn);
                    operation.blocked_reason = None;
                }
                Err(reason) => {
                    operation.planned_completion_turn = None;
                    operation.blocked_reason = Some(reason);
                }
            }
        }
        let issues = diagnose_operation_execution(
            world,
            manager,
            player_id,
            turn,
            assignment,
            &previous_entities,
            &persistent_combat_entities,
            &production_records,
        );
        let recoveries = diagnose_operation_recoveries(
            world,
            manager,
            player_id,
            turn,
            assignment,
            &previous_issues,
            &persistent_combat_entities,
            &production_records,
        );
        registry.replace_operation_issues(operation_id, issues, recoveries);
        registry.bind_assignment_entities(operation_id, assignment);
        for squad in manager.squads.iter().filter(|squad| {
            squad.owner_id == Some(player_id) && squad.target_island == Some(assignment.island_id)
        }) {
            if let Some(transport) = squad.transport_entity {
                registry.bind_entity_exclusive(
                    operation_id,
                    transport,
                    OperationEntityRole::Transport,
                );
            }
            for cargo in squad
                .cargo_entities
                .iter()
                .chain(squad.delivered_cargo.iter())
            {
                let role = if world
                    .get::<crate::components::UnitStats>(*cargo)
                    .is_some_and(|stats| stats.can_capture)
                {
                    OperationEntityRole::Capture
                } else {
                    OperationEntityRole::Combat
                };
                registry.bind_entity_exclusive(operation_id, *cargo, role);
            }
        }
    }

    // 首都強襲は兵站gate未達の間、局地portfolioへ現れなくても形成を続ける。
    // その期間もRolling Planと生産済みEntityを常在AssaultCapitalへ結び、
    // 予実履歴を「作ったが所属作戦なし」にしない。
    if let Some(capital_island) = enemy_capital_island
        && let Some(operation_id) = registry
            .operation_keys
            .get(&(player_id, capital_island, StrategicOperationScope::Capital))
            .copied()
    {
        let capital_plans = combat_plan_summaries
            .iter()
            .filter(|plan| {
                plan.kind == OperationKind::AssaultCapital
                    && (island_map
                        .get_island_at(&plan.anchor)
                        .is_some_and(|island| island.id == capital_island)
                        || plan.objective_properties.iter().any(|position| {
                            island_map
                                .get_island_at(position)
                                .is_some_and(|island| island.id == capital_island)
                        }))
            })
            .collect::<Vec<_>>();
        if let Some(operation) = registry.operations.get_mut(&operation_id) {
            operation.combat_plan_ids = capital_plans.iter().map(|plan| plan.plan_id).collect();
            operation.planned_suppression_turn = if capital_plans.is_empty() {
                None
            } else if capital_plans
                .iter()
                .all(|plan| plan.remaining_target_count == 0)
            {
                Some(turn)
            } else {
                capital_plans
                    .iter()
                    .filter(|plan| plan.remaining_target_count > 0)
                    .filter_map(|plan| plan.planned_elimination_turn)
                    .max()
            };
            if !capital_plans.is_empty() {
                operation.blocked_reason =
                    operation.planned_suppression_turn.is_none().then(|| {
                        "capital combat plan is forming but has no executable suppression turn"
                            .to_owned()
                    });
            }
        }
        if !capital_has_active_regional_segment {
            // 中間区間が閉じた後は、親作戦がSquadから実Entityを引き継ぐ。これにより
            // Capitalへ進んだ時点でCampaign→Reserve→Campaignへ分断しない。
            for squad in manager.squads.iter().filter(|squad| {
                squad.owner_id == Some(player_id) && squad.target_island == Some(capital_island)
            }) {
                if let Some(transport) = squad.transport_entity {
                    registry.bind_entity_exclusive(
                        operation_id,
                        transport,
                        OperationEntityRole::Transport,
                    );
                }
                for cargo in squad
                    .cargo_entities
                    .iter()
                    .chain(squad.delivered_cargo.iter())
                {
                    let role = if world
                        .get::<crate::components::UnitStats>(*cargo)
                        .is_some_and(|stats| stats.can_capture)
                    {
                        OperationEntityRole::Capture
                    } else {
                        OperationEntityRole::Combat
                    };
                    registry.bind_entity_exclusive(operation_id, *cargo, role);
                }
            }
        }
    }

    // Combat枠で生産されたEntityはIslandCampaignAssignmentとは別台帳にいる。
    // PlanIdを介して同じStrategicOperationへ接続し、攻撃Eventを作戦実績へ集約する。
    for record in deployment_records
        .into_iter()
        .filter(|record| record.active)
    {
        let Some(plan_id) = record.plan_step.map(|step| step.plan_id) else {
            continue;
        };
        let Some(operation_id) = registry.operations.values().find_map(|operation| {
            (operation.player_id == player_id
                && operation.active
                && operation.combat_plan_ids.contains(&plan_id))
            .then_some(operation.id)
        }) else {
            continue;
        };
        registry.bind_entity_exclusive(operation_id, record.entity, OperationEntityRole::Combat);
    }

    // Portfolioから外れた確保済み兵站島も、後続Nodeの前提としては残す。所有権が
    // 失われた時だけ完了を取り消してLockedへ戻す。
    registry.refresh_objective_ownership(player_id, &owned_properties);
    registry.rebuild_dependencies(player_id, enemy_capital_island, logistics_plan.as_ref());
    registry.refresh_node_states(player_id);

    // 完了・撤回済み作戦は履歴として残すが、Entity集合を第二の割当正本にしない。
    registry.release_inactive_assignments(player_id);

    if let Some(roadmap) = registry.roadmaps.get_mut(&player_id) {
        roadmap.planned_victory_turn = enemy_capital_island.and_then(|capital_island| {
            registry
                .operations
                .values()
                .find(|operation| {
                    operation.player_id == player_id
                        && operation.island_id == capital_island
                        && operation.purpose == StrategicPurpose::AssaultCapital
                })
                .and_then(|operation| operation.planned_completion_turn)
        });
        if enemy_capital.is_none() || enemy_unit_count == 0 {
            roadmap.actual_victory_turn.get_or_insert(turn);
        }
    }
    let approved_portfolio = registry.approved_portfolio(player_id, enemy_capital, portfolio);
    let assigned_campaign_entities = registry
        .operations
        .values()
        .filter(|operation| operation.player_id == player_id && operation.active)
        .flat_map(|operation| {
            operation
                .assigned_transports
                .iter()
                .chain(operation.assigned_capturers.iter())
                .chain(operation.assigned_combat.iter())
                .copied()
        })
        .collect::<Vec<_>>();
    registry.refresh_node_states(player_id);
    let mut directives = registry
        .operations
        .values()
        .filter(|operation| {
            operation.player_id == player_id
                && operation.active
                && operation.node_state != RoadmapNodeState::Locked
        })
        .map(|operation| {
            let operation_entities = operation
                .assigned_transports
                .iter()
                .chain(operation.assigned_capturers.iter())
                .chain(operation.assigned_combat.iter())
                .copied()
                .collect::<HashSet<_>>();
            let mut squad_ids = manager
                .squads
                .iter()
                .filter(|squad| squad.owner_id == Some(player_id))
                .filter(|squad| {
                    squad
                        .members
                        .iter()
                        .any(|entity| operation_entities.contains(entity))
                })
                .map(|squad| squad.id)
                .collect::<Vec<_>>();
            squad_ids.sort_unstable_by_key(|squad_id| squad_id.0);
            squad_ids.dedup();
            RoadmapOperationDirective {
                operation_id: operation.id,
                island_id: operation.island_id,
                purpose: operation.purpose,
                node_state: operation.node_state,
                target: operation.tactical_anchor,
                squad_ids,
            }
        })
        .collect::<Vec<_>>();
    directives.sort_unstable_by_key(|directive| directive.operation_id.0);
    let mut turn_plans = world
        .remove_resource::<RoadmapTurnPlanRegistry>()
        .unwrap_or_default();
    turn_plans.replace(RoadmapTurnPlan {
        player_id,
        turn,
        portfolio: approved_portfolio,
        directives,
    });
    world.insert_resource(turn_plans);
    world.insert_resource(registry);
    if let Some(mut execution) =
        world.get_resource_mut::<crate::ai::v4::campaign_execution::V4CampaignExecutionRegistry>()
    {
        for entity in assigned_campaign_entities {
            execution.mark_assigned(entity, turn);
        }
    }
}

/// action result Eventを、担当EntityのStrategicOperationへ照合する。
#[allow(clippy::too_many_arguments)]
pub fn audit_victory_roadmap_system(
    match_state: Res<MatchState>,
    island_map: Option<Res<IslandMap>>,
    mut moved: EventReader<UnitMovedEvent>,
    mut attacked: EventReader<UnitAttackedEvent>,
    mut loaded: EventReader<UnitLoadedEvent>,
    mut unloaded: EventReader<UnitUnloadedEvent>,
    mut capture_progressed: EventReader<PropertyCaptureProgressedEvent>,
    mut captured: EventReader<PropertyCapturedEvent>,
    mut supplied: EventReader<UnitSuppliedEvent>,
    mut waited: EventReader<UnitWaitedEvent>,
    mut destroyed: EventReader<UnitDestroyedEvent>,
    mut registry: ResMut<VictoryRoadmapRegistry>,
    campaign_execution: Option<
        ResMut<crate::ai::v4::campaign_execution::V4CampaignExecutionRegistry>,
    >,
    operation_assignments: Option<ResMut<crate::ai::operation_assignment::UnitOperationRegistry>>,
) {
    let turn = match_state.current_turn_number.0;
    for event in moved.read() {
        if let Some(island_map) = island_map.as_deref() {
            registry.record_move(event.entity, event.from, event.to, turn, island_map);
        } else {
            registry.observe_planned_move(event.entity, event.to, turn);
        }
    }
    for event in attacked.read() {
        registry.complete_planned_step(
            event.attacker,
            turn,
            CampaignStepKind::Attack,
            None,
            Some(event.defender),
        );
    }
    for event in loaded.read() {
        registry
            .transport_manifests
            .entry(event.transport)
            .or_default()
            .insert(event.cargo);
        if !registry.complete_planned_step(
            event.cargo,
            turn,
            CampaignStepKind::Load,
            None,
            Some(event.transport),
        ) {
            registry.complete_planned_step(
                event.transport,
                turn,
                CampaignStepKind::Load,
                None,
                Some(event.cargo),
            );
        }
    }
    for event in unloaded.read() {
        if let Some(manifest) = registry.transport_manifests.get_mut(&event.transport) {
            manifest.remove(&event.cargo);
        }
        if !registry.complete_planned_step(
            event.transport,
            turn,
            CampaignStepKind::Drop,
            None,
            Some(event.cargo),
        ) {
            registry.complete_planned_step(
                event.cargo,
                turn,
                CampaignStepKind::Drop,
                None,
                Some(event.transport),
            );
        }
    }
    for event in capture_progressed.read() {
        let matched = registry.complete_planned_step(
            event.unit,
            turn,
            CampaignStepKind::Capture,
            Some(GridPosition {
                x: event.x,
                y: event.y,
            }),
            None,
        );
        if matched
            && event.completed
            && let Some(binding) = registry.entity_bindings.get(&event.unit).copied()
            && let Some(operation) = registry.operations.get_mut(&binding.operation_id)
        {
            operation.execution.completed_captures =
                operation.execution.completed_captures.saturating_add(1);
        }
    }
    for event in captured.read() {
        let position = GridPosition {
            x: event.x,
            y: event.y,
        };
        // PropertyCapturedEventには実行Entityが無いため、これ単独では作戦進捗にしない。
        // 対応するPropertyCaptureProgressedEventを照合済みの場合だけ所有数の再計測で反映する。
        let _belongs_to_active_operation = registry.operations.values().any(|operation| {
            operation.active && operation.objective_properties.contains(&position)
        });
    }
    for event in supplied.read() {
        registry.complete_planned_step(
            event.supplier,
            turn,
            CampaignStepKind::Supply,
            None,
            Some(event.target),
        );
    }
    for event in waited.read() {
        registry.complete_planned_step(event.entity, turn, CampaignStepKind::Wait, None, None);
    }
    let mut campaign_execution = campaign_execution;
    let mut operation_assignments = operation_assignments;
    for event in destroyed.read() {
        if let Some(execution) = campaign_execution.as_deref_mut() {
            execution.mark_destroyed(event.entity, turn);
        }
        if let Some(assignments) = operation_assignments.as_deref_mut() {
            assignments.release_entity(event.entity);
        }
        registry.record_destroyed_entity(event.entity, turn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{GridTopology, MovementType};

    fn operation_with_entity(
        registry: &mut VictoryRoadmapRegistry,
        player: PlayerId,
        island: IslandId,
        entity: Entity,
        role: OperationEntityRole,
    ) -> StrategicOperationId {
        let roadmap = registry.ensure_roadmap(player, 1, None, None, 1);
        let operation = StrategicOperationId(999);
        registry.operations.insert(
            operation,
            StrategicOperation {
                id: operation,
                roadmap_id: roadmap,
                player_id: player,
                island_id: island,
                purpose: StrategicPurpose::CaptureIsland,
                created_turn: 1,
                last_observed_turn: 1,
                tactical_anchor: GridPosition { x: 3, y: 3 },
                objective_properties: vec![GridPosition { x: 3, y: 3 }],
                owned_objective_count: 0,
                node_state: RoadmapNodeState::Ready,
                phase: OperationPhase::Forming,
                planned_completion_turn: None,
                actual_completion_turn: None,
                assigned_transports: HashSet::new(),
                assigned_capturers: HashSet::new(),
                assigned_combat: HashSet::new(),
                combat_plan_ids: HashSet::new(),
                planned_suppression_turn: None,
                execution: StepExecutionTotals::default(),
                last_step: None,
                last_progress_turn: None,
                blocked_reason: None,
                current_issues: Vec::new(),
                issue_history: Vec::new(),
                recovery_history: Vec::new(),
                replan_count: 0,
                last_replan_turn: None,
                active: true,
            },
        );
        registry.bind_entity_exclusive(operation, entity, role);
        operation
    }

    fn strategic_assignment(
        island_id: IslandId,
        decision: IslandCampaignDecision,
        shortfall: u32,
        continued: bool,
    ) -> IslandCampaignAssignment {
        IslandCampaignAssignment {
            island_id,
            decision,
            target_position: GridPosition {
                x: island_id.0,
                y: 0,
            },
            capture_target_positions: Vec::new(),
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: shortfall,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: shortfall,
            },
            allocated_budget: 0,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: continued,
        }
    }

    #[test]
    fn strategy_candidate_search_prunes_to_a_single_best_focus_without_dropping_defense() {
        let defense = strategic_assignment(IslandId(9), IslandCampaignDecision::Defend, 0, true);
        let portfolio = IslandCampaignPortfolio {
            islands: Vec::new(),
            active_offensives: vec![
                strategic_assignment(IslandId(1), IslandCampaignDecision::Expand, 100_000, false),
                strategic_assignment(IslandId(2), IslandCampaignDecision::Assault, 0, true),
                strategic_assignment(IslandId(3), IslandCampaignDecision::Contest, 100_000, false),
                // 上位3件だけを分岐対象にする。第4候補は候補爆発を防ぐため除外する。
                strategic_assignment(IslandId(4), IslandCampaignDecision::Secure, 100_000, false),
            ],
            defenses: vec![defense.clone()],
        };

        let (selected, evaluations) = select_strategic_portfolio(&portfolio);

        assert_eq!(selected.defenses, vec![defense]);
        assert_eq!(selected.active_offensives.len(), 1);
        assert_eq!(selected.active_offensives[0].island_id, IslandId(2));
        assert_eq!(
            evaluations.len(),
            7,
            "実行可能な攻勢があるためHoldを除いた上位3Nodeの候補だけを比較する"
        );
        assert_eq!(
            evaluations
                .iter()
                .filter(|evaluation| evaluation.selected)
                .count(),
            1
        );
    }

    #[test]
    fn node_state_distinguishes_dominance_capture_and_blocked() {
        let player = PlayerId(1);
        let combat = Entity::from_raw(42);
        let mut registry = VictoryRoadmapRegistry::default();
        let operation = operation_with_entity(
            &mut registry,
            player,
            IslandId(2),
            combat,
            OperationEntityRole::Combat,
        );
        let current = registry.operations.get_mut(&operation).unwrap();
        current.phase = OperationPhase::Suppress;
        current.assigned_combat.insert(combat);
        current.planned_suppression_turn = Some(3);
        registry.refresh_node_states(player);
        assert_eq!(
            registry.operations[&operation].node_state,
            RoadmapNodeState::Dominant
        );

        registry.operations.get_mut(&operation).unwrap().phase = OperationPhase::Capture;
        registry.refresh_node_states(player);
        assert_eq!(
            registry.operations[&operation].node_state,
            RoadmapNodeState::Capturing
        );

        let current = registry.operations.get_mut(&operation).unwrap();
        current.phase = OperationPhase::Blocked;
        current.blocked_reason = Some("transport destroyed".to_owned());
        registry.refresh_node_states(player);
        assert_eq!(
            registry.operations[&operation].node_state,
            RoadmapNodeState::Blocked
        );
    }

    #[test]
    fn observed_island_nodes_remain_in_the_roadmap_without_assignments() {
        let player = PlayerId(1);
        let mut registry = VictoryRoadmapRegistry::default();
        let roadmap = registry.ensure_roadmap(player, 1, None, None, 0);
        let assessment = |island_id, state, decision| IslandCampaignAssessment {
            island_id,
            state,
            decision,
            state_reason: String::new(),
            decision_reason: String::new(),
            pause_cause: None,
            neutral_properties: 0,
            friendly_properties: 0,
            enemy_properties: 0,
            friendly_combat_units: 0,
            enemy_combat_units: 0,
            friendly_arrival_eta: None,
            enemy_arrival_eta: None,
            friendly_capture_eta: None,
            enemy_capture_eta: None,
            roi_production_sites: 0,
            transport_eta: None,
            expansion_payback_turns: None,
            required_budget: 0,
            allocated_budget: 0,
        };
        let secured = assessment(
            IslandId(1),
            crate::ai::island_campaign::IslandCampaignState::Secured,
            IslandCampaignDecision::Secure,
        );
        let pending = assessment(
            IslandId(2),
            crate::ai::island_campaign::IslandCampaignState::OpenNeutral,
            IslandCampaignDecision::Expand,
        );
        let secured_anchor = GridPosition { x: 1, y: 1 };
        let pending_anchor = GridPosition { x: 2, y: 2 };
        registry.reconcile_observed_island(
            roadmap,
            player,
            1,
            &secured,
            secured_anchor,
            vec![secured_anchor],
            &HashSet::from([secured_anchor]),
        );
        registry.reconcile_observed_island(
            roadmap,
            player,
            1,
            &pending,
            pending_anchor,
            vec![pending_anchor],
            &HashSet::new(),
        );
        registry.refresh_node_states(player);

        let secured_operation = registry
            .operation_keys
            .get(&(player, IslandId(1), StrategicOperationScope::Regional))
            .copied()
            .expect("確保済み島の観測Node");
        let pending_operation = registry
            .operation_keys
            .get(&(player, IslandId(2), StrategicOperationScope::Regional))
            .copied()
            .expect("未着手島の観測Node");
        assert!(!registry.operations[&secured_operation].active);
        assert_eq!(
            registry.operations[&secured_operation].node_state,
            RoadmapNodeState::Secured
        );
        assert!(!registry.operations[&pending_operation].active);
        assert_eq!(
            registry.operations[&pending_operation].node_state,
            RoadmapNodeState::Locked
        );
    }

    #[test]
    fn successor_stays_locked_until_every_predecessor_is_dominant_or_secured() {
        let player = PlayerId(1);
        let combat = Entity::from_raw(42);
        let mut registry = VictoryRoadmapRegistry::default();
        let predecessor = operation_with_entity(
            &mut registry,
            player,
            IslandId(1),
            combat,
            OperationEntityRole::Combat,
        );
        let mut second_predecessor = registry.operations[&predecessor].clone();
        second_predecessor.id = StrategicOperationId(1_000);
        second_predecessor.island_id = IslandId(2);
        second_predecessor.assigned_combat.clear();
        second_predecessor.phase = OperationPhase::Forming;
        let second_predecessor_id = second_predecessor.id;
        registry
            .operations
            .insert(second_predecessor_id, second_predecessor);
        let mut successor = registry.operations[&predecessor].clone();
        successor.id = StrategicOperationId(1_001);
        successor.island_id = IslandId(3);
        successor.assigned_combat.clear();
        successor.phase = OperationPhase::Forming;
        successor.blocked_reason = None;
        let successor_id = successor.id;
        registry.operations.insert(successor_id, successor);
        registry.roadmaps.get_mut(&player).unwrap().dependencies = vec![
            RoadmapDependency {
                predecessor,
                successor: successor_id,
                kind: RoadmapDependencyKind::Logistics,
            },
            RoadmapDependency {
                predecessor: second_predecessor_id,
                successor: successor_id,
                kind: RoadmapDependencyKind::Logistics,
            },
        ];

        registry.refresh_node_states(player);
        assert_eq!(
            registry.operations[&successor_id].node_state,
            RoadmapNodeState::Locked
        );

        let first = registry.operations.get_mut(&predecessor).unwrap();
        first.phase = OperationPhase::Suppress;
        first.assigned_combat.insert(combat);
        first.planned_suppression_turn = Some(2);
        registry.refresh_node_states(player);
        assert_eq!(
            registry.operations[&successor_id].node_state,
            RoadmapNodeState::Locked,
            "合流Nodeは片方の前提だけで解放しない"
        );

        registry
            .operations
            .get_mut(&second_predecessor_id)
            .unwrap()
            .phase = OperationPhase::Completed;
        registry.refresh_node_states(player);
        assert_eq!(
            registry.operations[&successor_id].node_state,
            RoadmapNodeState::Ready
        );
    }

    #[test]
    fn locked_operation_is_not_projected_to_the_squad_turn_plan() {
        let player = PlayerId(1);
        let island = IslandId(2);
        let mut registry = VictoryRoadmapRegistry::default();
        let operation = operation_with_entity(
            &mut registry,
            player,
            island,
            Entity::from_raw(42),
            OperationEntityRole::Capture,
        );
        registry.operation_keys.insert(
            (player, island, StrategicOperationScope::Regional),
            operation,
        );
        let assignment = IslandCampaignAssignment {
            island_id: island,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 3, y: 3 },
            capture_target_positions: vec![GridPosition { x: 3, y: 3 }],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 1_000,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 1_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let portfolio = IslandCampaignPortfolio {
            islands: Vec::new(),
            active_offensives: vec![assignment],
            defenses: Vec::new(),
        };

        registry.operations.get_mut(&operation).unwrap().node_state = RoadmapNodeState::Locked;
        assert!(
            registry
                .approved_portfolio(player, None, &portfolio)
                .active_offensives
                .is_empty()
        );

        registry.operations.get_mut(&operation).unwrap().node_state = RoadmapNodeState::Ready;
        assert_eq!(
            registry
                .approved_portfolio(player, None, &portfolio)
                .active_offensives
                .len(),
            1
        );
    }

    #[test]
    fn logistics_route_and_capital_segment_create_deterministic_dependencies() {
        let player = PlayerId(1);
        let mut registry = VictoryRoadmapRegistry::default();
        let first = operation_with_entity(
            &mut registry,
            player,
            IslandId(1),
            Entity::from_raw(41),
            OperationEntityRole::Combat,
        );
        let mut second_operation = registry.operations[&first].clone();
        second_operation.id = StrategicOperationId(1_000);
        second_operation.island_id = IslandId(2);
        let second = second_operation.id;
        registry.operations.insert(second, second_operation);
        let mut capital_regional_operation = registry.operations[&first].clone();
        capital_regional_operation.id = StrategicOperationId(1_001);
        capital_regional_operation.island_id = IslandId(3);
        let capital_regional = capital_regional_operation.id;
        registry
            .operations
            .insert(capital_regional, capital_regional_operation);
        let mut capital_operation = registry.operations[&first].clone();
        capital_operation.id = StrategicOperationId(1_002);
        capital_operation.island_id = IslandId(3);
        capital_operation.purpose = StrategicPurpose::AssaultCapital;
        let capital = capital_operation.id;
        registry.operations.insert(capital, capital_operation);
        registry.operation_keys.extend([
            (
                (player, IslandId(1), StrategicOperationScope::Regional),
                first,
            ),
            (
                (player, IslandId(2), StrategicOperationScope::Regional),
                second,
            ),
            (
                (player, IslandId(3), StrategicOperationScope::Regional),
                capital_regional,
            ),
            (
                (player, IslandId(3), StrategicOperationScope::Capital),
                capital,
            ),
        ]);
        let logistics_plan = crate::ai::v4::logistics_plan::V4LogisticsPlan {
            plan_id: 1,
            player_id: player,
            created_turn: 1,
            revised_turn: 1,
            last_observed_turn: 1,
            revision: 0,
            route_islands: vec![IslandId(1), IslandId(2)],
            selected_islands: vec![IslandId(1), IslandId(2)],
            stages: Vec::new(),
            direct_metrics: Default::default(),
            selected_metrics: Default::default(),
            current_forecast_metrics: Default::default(),
            replan_reason: crate::ai::v4::logistics_plan::LogisticsReplanReason::InitialSelection,
        };

        registry.rebuild_dependencies(player, Some(IslandId(3)), Some(&logistics_plan));

        assert_eq!(
            registry.roadmap(player).unwrap().dependencies,
            vec![
                RoadmapDependency {
                    predecessor: first,
                    successor: second,
                    kind: RoadmapDependencyKind::Logistics,
                },
                RoadmapDependency {
                    predecessor: second,
                    successor: capital,
                    kind: RoadmapDependencyKind::Logistics,
                },
                RoadmapDependency {
                    predecessor: capital_regional,
                    successor: capital,
                    kind: RoadmapDependencyKind::CapitalRoute,
                },
            ]
        );
    }

    #[test]
    fn turn_plan_rejects_squad_shared_by_multiple_operations() {
        let player = PlayerId(1);
        let squad = crate::ai::squad::SquadId(7);
        let plan = RoadmapTurnPlan {
            player_id: player,
            turn: 1,
            portfolio: IslandCampaignPortfolio::default(),
            directives: vec![
                RoadmapOperationDirective {
                    operation_id: StrategicOperationId(1),
                    island_id: IslandId(1),
                    purpose: StrategicPurpose::CaptureIsland,
                    node_state: RoadmapNodeState::Ready,
                    target: GridPosition { x: 1, y: 1 },
                    squad_ids: vec![squad],
                },
                RoadmapOperationDirective {
                    operation_id: StrategicOperationId(2),
                    island_id: IslandId(2),
                    purpose: StrategicPurpose::DefendIsland,
                    node_state: RoadmapNodeState::Ready,
                    target: GridPosition { x: 2, y: 2 },
                    squad_ids: vec![squad],
                },
            ],
        };

        assert!(!plan.is_consistent());
    }

    #[test]
    fn planned_step_counts_only_a_matching_result_event() {
        let player = PlayerId(1);
        let entity = Entity::from_raw(42);
        let target = Entity::from_raw(84);
        let mut registry = VictoryRoadmapRegistry::default();
        let operation = operation_with_entity(
            &mut registry,
            player,
            IslandId(2),
            entity,
            OperationEntityRole::Combat,
        );
        registry.plan_entity_step(
            entity,
            3,
            CampaignStepKind::Attack,
            CampaignStepKind::Attack,
            Some(GridPosition { x: 3, y: 3 }),
            Some(target),
        );

        assert!(!registry.complete_planned_step(
            entity,
            3,
            CampaignStepKind::Attack,
            None,
            Some(Entity::from_raw(85)),
        ));
        assert_eq!(registry.operations[&operation].execution.attacks, 0);
        assert!(registry.complete_planned_step(
            entity,
            3,
            CampaignStepKind::Attack,
            None,
            Some(target),
        ));
        assert_eq!(registry.operations[&operation].execution.planned, 1);
        assert_eq!(registry.operations[&operation].execution.completed, 1);
        assert_eq!(registry.operations[&operation].execution.attacks, 1);
    }

    #[test]
    fn missing_result_event_becomes_step_blocked_and_replan_is_actual_command() {
        let player = PlayerId(1);
        let entity = Entity::from_raw(42);
        let mut registry = VictoryRoadmapRegistry::default();
        let operation = operation_with_entity(
            &mut registry,
            player,
            IslandId(2),
            entity,
            OperationEntityRole::Capture,
        );
        registry.plan_entity_step(
            entity,
            3,
            CampaignStepKind::Capture,
            CampaignStepKind::Capture,
            Some(GridPosition { x: 3, y: 3 }),
            None,
        );
        registry.expire_pending_steps(player, 4);

        let blocked = &registry.operations[&operation];
        assert_eq!(blocked.execution.blocked, 1);
        assert_eq!(blocked.replan_count, 0, "検知だけを再計画実績にしない");
        assert!(blocked.current_issues.iter().any(|issue| {
            issue.kind == OperationIssueKind::StepBlocked && issue.entity == Some(entity)
        }));

        registry.plan_entity_step(
            entity,
            4,
            CampaignStepKind::Capture,
            CampaignStepKind::Capture,
            Some(GridPosition { x: 3, y: 3 }),
            None,
        );
        let replanned = &registry.operations[&operation];
        assert_eq!(replanned.replan_count, 1);
        assert!(
            replanned
                .recovery_history
                .iter()
                .any(|recovery| recovery.kind == OperationRecoveryKind::ReplanStep)
        );
    }

    #[test]
    fn operation_identity_does_not_depend_on_enemy_entity_or_anchor() {
        let player = PlayerId(1);
        let island = IslandId(2);
        let assignment = IslandCampaignAssignment {
            island_id: island,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 10, y: 10 },
            capture_target_positions: vec![GridPosition { x: 10, y: 10 }],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 1_000,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 1_000,
            },
            allocated_budget: 1_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        let mut registry = VictoryRoadmapRegistry::default();
        let roadmap = registry.ensure_roadmap(player, 1, None, None, 3);
        let first = registry.reconcile_assignment(
            roadmap,
            player,
            1,
            &assignment,
            StrategicPurpose::CaptureIsland,
            vec![GridPosition { x: 10, y: 10 }],
            &HashSet::new(),
            OperationPhase::Forming,
        );
        let mut moved_anchor = assignment.clone();
        moved_anchor.target_position = GridPosition { x: 12, y: 11 };
        let second = registry.reconcile_assignment(
            roadmap,
            player,
            2,
            &moved_anchor,
            StrategicPurpose::CaptureIsland,
            vec![GridPosition { x: 12, y: 11 }],
            &HashSet::new(),
            OperationPhase::Transit,
        );

        assert_eq!(first, second);
        let operation = registry.operations.get(&first).expect("作戦");
        assert_eq!(
            operation.objective_properties,
            vec![GridPosition { x: 12, y: 11 }],
            "作戦実績は維持しつつ、成功済みMilestoneを現在目標から除く"
        );
        assert_eq!(operation.tactical_anchor, GridPosition { x: 12, y: 11 });
    }

    #[test]
    fn operation_identity_and_actuals_survive_capture_to_defense_revision() {
        let player = PlayerId(1);
        let island = IslandId(2);
        let assignment = IslandCampaignAssignment {
            island_id: island,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 10, y: 10 },
            capture_target_positions: vec![GridPosition { x: 10, y: 10 }],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 0,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        let mut registry = VictoryRoadmapRegistry::default();
        let roadmap = registry.ensure_roadmap(player, 1, None, None, 3);
        let operation_id = registry.reconcile_assignment(
            roadmap,
            player,
            1,
            &assignment,
            StrategicPurpose::CaptureIsland,
            assignment.capture_target_positions.clone(),
            &HashSet::new(),
            OperationPhase::Capture,
        );
        let operation = registry.operations.get_mut(&operation_id).unwrap();
        operation.execution.planned = 4;
        operation.execution.completed = 3;
        operation.issue_history.push(OperationIssue {
            kind: OperationIssueKind::CombatUnitDestroyed,
            detected_turn: 1,
            entity: None,
            related_entity: None,
            detail: "capturer lost".to_owned(),
        });

        let revised_id = registry.reconcile_assignment(
            roadmap,
            player,
            2,
            &assignment,
            StrategicPurpose::DefendIsland,
            assignment.capture_target_positions.clone(),
            &HashSet::new(),
            OperationPhase::Blocked,
        );

        assert_eq!(revised_id, operation_id);
        let operation = &registry.operations[&operation_id];
        assert_eq!(operation.created_turn, 1);
        assert_eq!(operation.purpose, StrategicPurpose::DefendIsland);
        assert_eq!(operation.execution.planned, 4);
        assert_eq!(operation.execution.completed, 3);
        assert_eq!(operation.issue_history.len(), 1);
        assert_eq!(registry.roadmaps[&player].operation_ids, vec![operation_id]);
    }

    #[test]
    fn roadmap_binding_moves_entity_out_of_previous_operation_atomically() {
        let player = PlayerId(1);
        let entity = Entity::from_raw(42);
        let assignment = |island_id: IslandId| IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition {
                x: island_id.0,
                y: 0,
            },
            capture_target_positions: vec![GridPosition {
                x: island_id.0,
                y: 0,
            }],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 1_000,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 1_000,
            transport_entities: Vec::new(),
            capture_entities: vec![entity],
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let first_assignment = assignment(IslandId(2));
        let second_assignment = assignment(IslandId(3));
        let mut registry = VictoryRoadmapRegistry::default();
        let roadmap = registry.ensure_roadmap(player, 1, None, None, 1);
        let first = registry.reconcile_assignment(
            roadmap,
            player,
            1,
            &first_assignment,
            StrategicPurpose::CaptureIsland,
            first_assignment.capture_target_positions.clone(),
            &HashSet::new(),
            OperationPhase::Forming,
        );
        registry.bind_assignment_entities(first, &first_assignment);
        let second = registry.reconcile_assignment(
            roadmap,
            player,
            1,
            &second_assignment,
            StrategicPurpose::CaptureIsland,
            second_assignment.capture_target_positions.clone(),
            &HashSet::new(),
            OperationPhase::Forming,
        );
        registry.bind_assignment_entities(second, &second_assignment);

        assert!(
            !registry.operations[&first]
                .assigned_capturers
                .contains(&entity)
        );
        assert!(
            registry.operations[&second]
                .assigned_capturers
                .contains(&entity)
        );
        assert_eq!(registry.entity_bindings[&entity].operation_id, second);

        registry.operations.get_mut(&second).unwrap().active = false;
        registry.release_inactive_assignments(player);
        assert!(registry.operations[&second].assigned_capturers.is_empty());
        assert!(!registry.entity_bindings.contains_key(&entity));
    }

    #[test]
    fn persistent_combat_deployment_is_not_reported_as_assignment_lost() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let player = PlayerId(1);
        let assignment = IslandCampaignAssignment {
            island_id: IslandId(2),
            decision: IslandCampaignDecision::Contest,
            target_position: GridPosition { x: 3, y: 4 },
            capture_target_positions: Vec::new(),
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 1,
                total_budget: 0,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 0,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: true,
        };
        let manager = SquadManager::new();

        let issues = diagnose_operation_execution(
            &world,
            &manager,
            player,
            3,
            &assignment,
            &[(entity, OperationEntityRole::Combat)],
            &HashSet::from([entity]),
            &[],
        );

        assert!(
            issues
                .iter()
                .all(|issue| issue.kind != OperationIssueKind::AssignmentLost),
            "rolling planの永続Combat deploymentは島assignment外でも作戦所属である"
        );
    }

    #[test]
    fn issue_detection_is_not_counted_as_replanning_until_recovery_occurs() {
        let player = PlayerId(1);
        let island = IslandId(2);
        let entity = Entity::from_raw(42);
        let assignment = IslandCampaignAssignment {
            island_id: island,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 3, y: 4 },
            capture_target_positions: vec![GridPosition { x: 3, y: 4 }],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 1_000,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 1_000,
            transport_entities: Vec::new(),
            capture_entities: vec![entity],
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let mut registry = VictoryRoadmapRegistry::default();
        let roadmap = registry.ensure_roadmap(player, 1, None, None, 1);
        let operation_id = registry.reconcile_assignment(
            roadmap,
            player,
            1,
            &assignment,
            StrategicPurpose::CaptureIsland,
            assignment.capture_target_positions.clone(),
            &HashSet::new(),
            OperationPhase::Forming,
        );
        registry.bind_assignment_entities(operation_id, &assignment);
        let issue = OperationIssue {
            kind: OperationIssueKind::AssignmentLost,
            detected_turn: 2,
            entity: Some(entity),
            related_entity: None,
            detail: "assignment disappeared".to_owned(),
        };

        registry.replace_operation_issues(operation_id, vec![issue.clone()], Vec::new());
        assert_eq!(registry.operations[&operation_id].replan_count, 0);
        assert_eq!(
            registry
                .entity_bindings
                .get(&entity)
                .and_then(|binding| registry.operations.get(&binding.operation_id))
                .map(|operation| operation.island_id),
            Some(island),
            "Roadmap監査でもEntityの作戦bindingを保持する"
        );
        let island_map = IslandMap {
            islands: vec![
                crate::ai::islands::Island {
                    id: IslandId(1),
                    tiles: HashSet::from([
                        GridPosition { x: 0, y: 0 },
                        GridPosition { x: 1, y: 0 },
                    ]),
                },
                crate::ai::islands::Island {
                    id: island,
                    tiles: HashSet::from([GridPosition { x: 3, y: 4 }]),
                },
            ],
        };
        registry.record_move(
            entity,
            GridPosition { x: 0, y: 0 },
            GridPosition { x: 1, y: 0 },
            2,
            &island_map,
        );
        assert_eq!(
            registry.operations[&operation_id].execution.deviations, 0,
            "搭載前の出発島内移動は逸脱ではない"
        );

        registry.replace_operation_issues(
            operation_id,
            Vec::new(),
            vec![OperationRecoveryAction {
                kind: OperationRecoveryKind::RestoreAssignment,
                cause: issue.kind,
                completed_turn: 3,
                entity: Some(entity),
                detail: "restored assignment".to_owned(),
            }],
        );
        let operation = &registry.operations[&operation_id];
        assert_eq!(operation.replan_count, 1);
        assert_eq!(operation.last_replan_turn, Some(3));
        assert_eq!(operation.recovery_history.len(), 1);
    }

    #[test]
    fn capturer_waiting_on_source_island_keeps_operation_forming() {
        let player = PlayerId(1);
        let target_island = IslandId(2);
        let mut world = World::new();
        let capturer = world.spawn(GridPosition { x: 0, y: 0 }).id();
        let island_map = IslandMap {
            islands: vec![
                crate::ai::islands::Island {
                    id: IslandId(1),
                    tiles: HashSet::from([GridPosition { x: 0, y: 0 }]),
                },
                crate::ai::islands::Island {
                    id: target_island,
                    tiles: HashSet::from([GridPosition { x: 3, y: 4 }]),
                },
            ],
        };
        let assignment = IslandCampaignAssignment {
            island_id: target_island,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 3, y: 4 },
            capture_target_positions: vec![GridPosition { x: 3, y: 4 }],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: Some(crate::resources::UnitType::TransportHelicopter),
                transport_slots: 1,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 6_000,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: Some(crate::resources::UnitType::TransportHelicopter),
                transport_slots: 1,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 5_000,
            },
            allocated_budget: 1_000,
            transport_entities: Vec::new(),
            capture_entities: vec![capturer],
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: true,
        };

        assert_eq!(
            operation_phase(
                &world,
                &island_map,
                &assignment,
                &SquadManager::default(),
                player,
            ),
            OperationPhase::Forming
        );
    }

    #[test]
    fn completion_forecast_covers_every_property_not_only_the_anchor() {
        let player = PlayerId(1);
        let mut world = World::new();
        let map = Map::new(5, 1, Terrain::Plains, GridTopology::Square);
        let island_map = IslandMap::analyze(&map);
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let capturer = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    can_capture: true,
                    max_movement: 3,
                    movement_type: MovementType::Infantry,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let first = GridPosition { x: 2, y: 0 };
        let second = GridPosition { x: 4, y: 0 };
        let properties = vec![
            (first, Property::new(Terrain::City, None, 100)),
            (second, Property::new(Terrain::City, None, 100)),
        ];
        let assignment = IslandCampaignAssignment {
            island_id: island_map.get_island_at(&first).unwrap().id,
            decision: IslandCampaignDecision::Expand,
            target_position: first,
            capture_target_positions: vec![first, second],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 1_000,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 1_000,
            transport_entities: Vec::new(),
            capture_entities: vec![capturer],
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: false,
        };

        let completion = estimate_full_objective_completion(
            &mut world,
            player,
            1,
            assignment.island_id,
            &assignment,
            &[first, second],
            &properties,
            &island_map,
            &SquadManager::default(),
            Some(1),
        )
        .unwrap();

        assert_eq!(completion, 5, "2拠点目の移動・占領まで期限へ含める");
    }

    #[test]
    fn capital_objective_exists_before_an_assault_package_is_executable() {
        let player = PlayerId(1);
        let capital = GridPosition { x: 20, y: 4 };
        let island = IslandId(7);
        let mut registry = VictoryRoadmapRegistry::default();
        let roadmap = registry.ensure_roadmap(player, 1, Some(capital), Some(island), 6);

        let first = registry.ensure_capital_objective(roadmap, player, 1, island, capital, false);
        let second = registry.ensure_capital_objective(roadmap, player, 2, island, capital, false);

        assert_eq!(first, second);
        let operation = registry.operations.get(&first).unwrap();
        assert!(operation.active);
        assert_eq!(operation.purpose, StrategicPurpose::AssaultCapital);
        assert_eq!(operation.objective_properties, vec![capital]);
        assert_eq!(operation.planned_completion_turn, None);
        assert!(operation.blocked_reason.is_some());
    }

    #[test]
    fn regional_milestone_does_not_overwrite_capital_operation_on_the_same_island() {
        let player = PlayerId(1);
        let island = IslandId(7);
        let capital = GridPosition { x: 20, y: 4 };
        let local_target = GridPosition { x: 10, y: 4 };
        let assignment = IslandCampaignAssignment {
            island_id: island,
            decision: IslandCampaignDecision::Contest,
            target_position: local_target,
            capture_target_positions: vec![local_target],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 0,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        let mut registry = VictoryRoadmapRegistry::default();
        let roadmap = registry.ensure_roadmap(player, 1, Some(capital), Some(island), 1);
        let capital_operation =
            registry.ensure_capital_objective(roadmap, player, 1, island, capital, false);

        assert_eq!(
            purpose_for(&assignment, Some(capital)),
            StrategicPurpose::CaptureIsland
        );
        let regional_operation = registry.reconcile_assignment(
            roadmap,
            player,
            1,
            &assignment,
            purpose_for(&assignment, Some(capital)),
            assignment.capture_target_positions.clone(),
            &HashSet::new(),
            OperationPhase::Forming,
        );

        assert_ne!(capital_operation, regional_operation);
        assert_eq!(registry.operations.len(), 2);
        assert_eq!(
            registry.operations[&capital_operation].tactical_anchor,
            capital
        );
        assert_eq!(
            registry.operations[&regional_operation].tactical_anchor,
            local_target
        );
    }

    #[test]
    fn destroyed_transport_classifies_carrier_and_loaded_cargo_separately() {
        let player = PlayerId(2);
        let island = IslandId(3);
        let transport = Entity::from_raw(40);
        let cargo = Entity::from_raw(41);
        let assignment = IslandCampaignAssignment {
            island_id: island,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 10, y: 10 },
            capture_target_positions: vec![GridPosition { x: 10, y: 10 }],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: Some(crate::resources::UnitType::TransportHelicopter),
                transport_slots: 1,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 5_000,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 5_000,
            transport_entities: vec![transport],
            capture_entities: vec![cargo],
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let mut registry = VictoryRoadmapRegistry::default();
        let roadmap = registry.ensure_roadmap(player, 1, None, None, 3);
        let operation_id = registry.reconcile_assignment(
            roadmap,
            player,
            1,
            &assignment,
            StrategicPurpose::CaptureIsland,
            assignment.capture_target_positions.clone(),
            &HashSet::new(),
            OperationPhase::Transit,
        );
        registry.bind_assignment_entities(operation_id, &assignment);
        registry
            .transport_manifests
            .entry(transport)
            .or_default()
            .insert(cargo);

        registry.record_destroyed_entity(transport, 2);

        let operation = registry.operations.get(&operation_id).unwrap();
        assert!(operation.current_issues.iter().any(|issue| {
            issue.kind == OperationIssueKind::TransportDestroyed && issue.entity == Some(transport)
        }));
        assert!(operation.current_issues.iter().any(|issue| {
            issue.kind == OperationIssueKind::CargoLostWithTransport
                && issue.entity == Some(cargo)
                && issue.related_entity == Some(transport)
        }));
        assert_eq!(operation.last_replan_turn, None);
        assert_eq!(operation.replan_count, 0, "損耗検知だけを再計画扱いしない");
        assert!(operation.recovery_history.is_empty());
    }
}
