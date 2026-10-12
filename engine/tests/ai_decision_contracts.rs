//! V4生産判断の状態契約。
//!
//! 固定seedのAI対戦を数ターン進め、V4の各手番について「計画した購入案」と
//! 「実際に適用された生産」と「未充足として残した要求」の整合を検証する。
//! 盤面や行動列の一致（golden）ではなく、どの実装でも守るべき不変条件だけを見るため、
//! 戦略変更（S）を入れた後も同じ契約で検証できる。
//!
//! - 1手番の購入案は同じ施設を二度使わず、合計費用が配分開始時の資金を超えない。
//! - 計画した購入案は、発行直前の検証で取り消されない限り、同じ兵種で生産として適用される。
//! - 候補なし・計画なしで見送った枠は、実在した要求（要求量>0）に対する判断として記録される。
//! - 発行直前の検証で取り消した購入案は、通常の対局では発生しない。

use bevy_ecs::event::EventCursor;
use bevy_ecs::prelude::*;
use engine::ai::ai_version::{AiVersion, PlayerAiSettings};
use engine::ai::engine::execute_ai_turn;
use engine::ai::production::commit::IssueAudit;
use engine::ai::production::demand::UnfilledReason;
use engine::ai::v4::trace::{ProductionDecision, ProductionTraceDiagnostics};
use engine::collections::{HashMap, HashSet};
use engine::components::*;
use engine::events::*;
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::*;
/// 1ターン内で許容するAIステップ数の上限（無限ループ検出用）
const MAX_STEPS_PER_TURN: usize = 400;

/// 固定seed・固定AI版でマップを初期化する。
fn new_game(map_name: &str, seed: u64, p1: AiVersion, p2: AiVersion) -> (World, Schedule) {
    let master_data = MasterDataRegistry::load().expect("マスターデータの読み込みに失敗");
    let (mut world, schedule) = engine::setup::initialize_world_from_master_data_with_topology(
        &master_data,
        map_name,
        GridTopology::Hex,
    )
    .expect("ワールド初期化に失敗");
    world.insert_resource(GameRng::new(seed));
    let mut ai_settings = PlayerAiSettings::default();
    ai_settings.set_version(PlayerId(1), p1);
    ai_settings.set_version(PlayerId(2), p2);
    world.insert_resource(ai_settings);
    (world, schedule)
}

/// V4の1手番で観測した計画と実適用。
struct TurnObservation {
    turn: u32,
    player: PlayerId,
    funds_at_start: u32,
    produced: Vec<UnitProducedEvent>,
}

/// 1手番をAIに最後まで実行させ、その手番に適用された生産を返す。
fn play_one_turn(
    world: &mut World,
    schedule: &mut Schedule,
    cursor: &mut EventCursor<UnitProducedEvent>,
    active: PlayerId,
) -> Vec<UnitProducedEvent> {
    let mut produced = Vec::new();
    for _ in 0..MAX_STEPS_PER_TURN {
        let action = execute_ai_turn(world, active);
        schedule.run(world);
        produced.extend(
            cursor
                .read(world.resource::<Events<UnitProducedEvent>>())
                .filter(|event| event.player_id == active)
                .cloned(),
        );
        if action.is_none() {
            return produced;
        }
    }
    panic!("P{} がステップ上限を超過", active.0);
}

/// 計画traceと実適用を照合し、契約違反を文字列で返す。
fn check_turn_contracts(world: &World, observation: &TurnObservation) -> Vec<String> {
    let mut violations = Vec::new();
    let context = format!("T{} P{}", observation.turn, observation.player.0);

    // 発行直前の検証による取消は、計画と実盤面の食い違いを意味する。
    if let Some(audit) = world.get_resource::<IssueAudit>()
        && !audit.rejected().is_empty()
    {
        violations.push(format!(
            "{context}: 発行直前に取り消した購入案がある: {:?}",
            audit.rejected()
        ));
    }

    let Some(trace) = world
        .get_resource::<ProductionTraceDiagnostics>()
        .and_then(|diagnostics| diagnostics.by_player.get(&observation.player))
    else {
        // 生産計画を立てなかった手番（島嶼campaign専用生産のみ等）は照合対象がない。
        return violations;
    };

    if trace.funds > observation.funds_at_start {
        violations.push(format!(
            "{context}: 配分開始資金 {} が手番開始時の資金 {} を超える",
            trace.funds, observation.funds_at_start
        ));
    }

    let mut facilities = HashSet::default();
    let mut planned_cost = 0_u32;
    let mut planned_by_type: HashMap<UnitType, u32> = HashMap::default();
    for step in &trace.steps {
        let (unit_type, cost, facility) = match step.decision {
            ProductionDecision::Produced {
                unit_type,
                cost,
                facility,
            }
            | ProductionDecision::ProducedImmediateReinforcement {
                unit_type,
                cost,
                facility,
            } => (unit_type, cost, facility),
            ProductionDecision::Unfilled { .. }
            | ProductionDecision::Deferred { .. }
            | ProductionDecision::Reserved { .. } => continue,
        };
        if !facilities.insert(facility) {
            violations.push(format!(
                "{context}: 施設 ({},{}) を同じ手番で二度使う購入案",
                facility.x, facility.y
            ));
        }
        planned_cost = planned_cost.saturating_add(cost);
        *planned_by_type.entry(unit_type).or_default() += 1;
    }
    if planned_cost > trace.funds {
        violations.push(format!(
            "{context}: 購入案の合計 {planned_cost} が配分開始資金 {} を超える",
            trace.funds
        ));
    }

    // 計画した購入案は、同じ兵種の生産として適用されている（施設は確定前の最適化で
    // 入れ替わり得るため兵種単位で照合する。campaign専用生産の分だけ実適用が多くてよい）。
    let mut produced_by_type: HashMap<UnitType, u32> = HashMap::default();
    for event in &observation.produced {
        *produced_by_type.entry(event.unit_type).or_default() += 1;
    }
    for (unit_type, planned) in &planned_by_type {
        let produced = produced_by_type.get(unit_type).copied().unwrap_or(0);
        if produced < *planned {
            violations.push(format!(
                "{context}: {unit_type:?} を {planned} 体計画したが適用は {produced} 体"
            ));
        }
    }

    // 候補・計画の不在で見送った枠は、実在した要求に対する判断でなければならない。
    for unfilled in &trace.unfilled_slots {
        let searched = matches!(
            unfilled.reason,
            UnfilledReason::NoCandidate
                | UnfilledReason::NoForcePackage
                | UnfilledReason::NoExecutablePlan
        );
        if searched && unfilled.requirement == 0 {
            violations.push(format!(
                "{context}: 要求0の枠を {:?} で見送った記録: {unfilled:?}",
                unfilled.reason
            ));
        }
    }
    violations
}

/// 指定ターンまでAI同士を対戦させ、V4手番ごとに契約を検証する。
fn run_contract_scenario(map: &str, seed: u64, p1: AiVersion, p2: AiVersion, turns: u32) {
    let (mut world, mut schedule) = new_game(map, seed, p1, p2);
    let mut cursor = EventCursor::<UnitProducedEvent>::default();
    let mut violations = Vec::new();
    let mut checked_turns = 0;
    loop {
        let (turn, active, game_over) = {
            let ms = world.resource::<MatchState>();
            let players = world.resource::<Players>();
            (
                ms.current_turn_number.0,
                players.0[ms.active_player_index.0].id,
                ms.game_over.is_some(),
            )
        };
        if game_over || turn > turns {
            break;
        }
        let is_v4 = world.resource::<PlayerAiSettings>().get_version(active) == AiVersion::V4;
        let funds_at_start = world
            .resource::<Players>()
            .0
            .iter()
            .find(|player| player.id == active)
            .map_or(0, |player| player.funds);
        if is_v4 {
            // 診断リソースは判断に使われない。手番ごとに空にして当手番分だけを照合する。
            world.remove_resource::<ProductionTraceDiagnostics>();
            world.remove_resource::<IssueAudit>();
        }
        let produced = play_one_turn(&mut world, &mut schedule, &mut cursor, active);
        if is_v4 {
            checked_turns += 1;
            violations.extend(check_turn_contracts(
                &world,
                &TurnObservation {
                    turn,
                    player: active,
                    funds_at_start,
                    produced,
                },
            ));
        }
    }
    assert!(checked_turns > 0, "{map}: V4の手番を一度も検証していない");
    assert!(
        violations.is_empty(),
        "{map} seed={seed}: 生産判断の契約違反\n{}",
        violations.join("\n")
    );
}

#[test]
fn v4_small_map_production_contracts() {
    // 小規模拡張経路（196セル以下）。
    run_contract_scenario("map_1", 1, AiVersion::V4, AiVersion::V200, 6);
}

#[test]
fn v4_small_map_second_player_production_contracts() {
    run_contract_scenario("map_2", 2, AiVersion::V200, AiVersion::V4, 6);
}

#[test]
fn v4_standard_map_production_contracts() {
    // 通常経路。
    run_contract_scenario("map_6", 1, AiVersion::V4, AiVersion::V200, 6);
}

#[test]
fn v4_standard_map_second_player_production_contracts() {
    run_contract_scenario("map_25", 2, AiVersion::V200, AiVersion::V4, 5);
}
