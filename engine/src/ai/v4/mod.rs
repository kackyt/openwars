//! V4: 作戦駆動生産 AI。
//!
//! V1〜V3 の生産ロジック（`GamePhase` ごとの理想構成をハードコードし、
//! 構成比の差分で買うものを決める方式）とは完全に分離した独立モジュール。
//!
//! 基本方針:
//! 1. 盤面から「作戦（Operation）」＝獲る／守るべき拠点のまとまりを切り出す
//! 2. 各作戦について、観測量だけから 5 つの枠を逆算する
//!    （占領枠・撃破枠・護衛枠・輸送枠・迎撃枠）
//! 3. 空いている生産枠を、最も不足している枠から順に埋める
//!
//! 「敵を減らす」と「占領する」は別フェーズではなく同一作戦の別枠として
//! 同時に立つため、倒してから占領するのではなく並行して進む。

pub mod deployment;
pub mod logistics_plan;
pub mod operation;
pub mod plan_revision;
pub mod rolling_plan;
pub mod trace;
pub mod victory_roadmap;

use crate::ai::production::plan_campaign_with_expansion_denial_reserve;
use operation::{
    AcquisitionMode, OperationFacts, OperationKind, OperationSlots, RESERVATION_PATIENCE_TURNS,
    SLOT_PRIORITY, SlotKind, SlotTier, acquisition_mode, derive_slots,
};
use plan_revision::{
    ActivePlanObjective, DeploymentExecutionObservation, PlanDisposition, PlanStepRef,
    SelectedPlan, V4RollingPlanRegistry,
};
use rolling_plan::{
    DEFAULT_SEARCH_TURNS, EnemyPlanUnit, FriendlyPlanUnit, RollingPlanInput,
    evaluate_fixed_package, plan_force_package, production_options,
};
use trace::{
    CampaignTurnForecastTrace, ProductionDecision, ProductionOperationTrace, ProductionPlanTrace,
    ProductionStepTrace, ProductionTraceDiagnostics, RollingCombatPlanTrace, RollingPurchaseTrace,
    RollingTargetTrace,
};

use crate::ai::turn_distance::TerrainConnectivity;
use crate::components::{
    CargoCapacity, Faction, GridPosition, Health, PlayerId, Property, Transporting, UnitStats,
};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Map, MovementType, Players, Terrain, UnitRegistry, UnitType};
use crate::systems::transport::can_unload_from_terrain;
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};

/// 揚陸可否キャッシュのキー：(輸送の移動タイプ, 積荷の移動タイプ, 出発地, 目標)
type DeliveryKey = (MovementType, MovementType, (usize, usize), (usize, usize));

/// 到達性まわりの計算結果を、1 回の生産判断のあいだだけ再利用するためのコンテキスト。
///
/// 揚陸可否の判定はマップ全域の走査を伴うため、施設と目標の組み合わせごとに
/// 結果を憶えておかないと候補評価のたびに同じ探索を繰り返すことになる。
#[derive(Default)]
struct ReachCtx {
    terrain: TerrainConnectivity,
    delivery: HashMap<DeliveryKey, bool>,
}

impl ReachCtx {
    /// 地形連結による到達判定（`TerrainConnectivity` への委譲）。
    fn is_reachable(
        &mut self,
        map: &Map,
        registry: &MasterDataRegistry,
        start: (usize, usize),
        target: (usize, usize),
        movement_type: MovementType,
    ) -> bool {
        self.terrain
            .is_reachable(map, registry, start, target, movement_type)
    }
}

/// 同時に抱える作戦の最大数。多すぎると戦力が分散するため制限する。
const MAX_OPERATIONS: usize = 4;

/// 敵がこのターン数以内に到達できる自軍拠点は防衛作戦の対象とする。
const DEFENSE_THREAT_ETA: u32 = 2;

/// 占領開始後に拠点を確保し切るまでに必要な最小手番数。
const CAPTURE_COMPLETION_TURNS: u32 = 2;

/// 敵の「拡張装置」（占領可能ユニットと、それを運ぶ輸送）に掛ける脅威の倍率。
/// これらはコスト以上に盤面の収入を動かすため、素のコスト価値で数えると
/// 撃破枠が立たず、局地戦で勝ちながら territory を明け渡すことになる。
const EXPANSION_THREAT_WEIGHT: u32 = 2;

/// 盤面から取り出したユニット 1 体分の情報。
#[derive(Debug, Clone)]
struct UnitSnapshot {
    /// 盤面上の実Entity。純粋関数テストの合成snapshotではNoneを許容する。
    entity: Option<Entity>,
    pos: GridPosition,
    stats: UnitStats,
    hp: u32,
    free_cargo: u32,
}

/// 敵が実際に生産へ使える施設。所有者の首都から生産範囲内にある施設だけを保持する。
#[derive(Debug, Clone, Copy)]
struct EnemyFacilitySnapshot {
    pos: GridPosition,
    terrain: Terrain,
}

impl UnitSnapshot {
    /// HP を加味した戦力価値。
    fn value(&self) -> u32 {
        self.stats.cost.saturating_mul(self.hp) / 100
    }
}

/// 1体の敵に対する未対処戦力。
///
/// `remaining_value` はHP補正済みの実戦力、`priority_weight` は倒す順序にだけ使う。
/// 占領・輸送能力の戦略的重要度を実戦力へ掛けると「重要だから2体必要」という
/// 誤った需要になるため、両者を別の次元として保持する。
#[derive(Debug, Clone)]
struct ThreatTarget {
    entity: Option<Entity>,
    stats: UnitStats,
    position: GridPosition,
    /// 金額被覆で変形させない、実盤面の残HP。
    current_hp: u32,
    remaining_value: f32,
    priority_weight: f32,
    /// 0は現在の局地敵。1以上はanchorへ到着してから交戦可能になる観測済み増援。
    available_turn: u32,
}

impl ThreatTarget {
    fn from_snapshot(unit: &UnitSnapshot, expansion_race_live: bool) -> Self {
        let priority_weight =
            if expansion_race_live && (unit.stats.can_capture || unit.stats.max_cargo > 0) {
                EXPANSION_THREAT_WEIGHT as f32
            } else {
                1.0
            };
        Self {
            entity: unit.entity,
            stats: unit.stats.clone(),
            position: unit.pos,
            current_hp: unit.hp,
            remaining_value: unit.value() as f32,
            priority_weight,
            available_turn: 0,
        }
    }
}

/// 中立・敵拠点の確保を直接妨げる、地上戦力または輸送戦力であるか。
fn is_territory_control_threat(stats: &UnitStats) -> bool {
    !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
        || stats.can_capture
        || stats.max_cargo > 0
}

/// 1 つの作戦。対象拠点のまとまりと、そこから導出された枠を保持する。
#[derive(Debug)]
struct Operation {
    kind: OperationKind,
    /// 作戦の代表地点（距離計算の基準）
    anchor: GridPosition,
    /// 編成中の戦力を逐次投入せず集結させる、自軍側の安全な地点。
    staging_anchor: GridPosition,
    /// falseの首都作戦は生産だけ進め、攻撃任務へ切り替えない。
    execution_authorized: bool,
    /// anchorが所有権変化で動いても同じ目的を照合するための拠点集合。
    objective_properties: Vec<GridPosition>,
    /// 防衛の硬い期限と、旧枠の診断にだけ使う作戦時間幅。
    /// 敵を作戦へ帰属させる条件には使わない。
    threat_horizon: u32,
    facts: OperationFacts,
    slots: OperationSlots,
    /// この生産計画の中で既に購入した分
    filled: OperationSlots,
    /// 自軍が生産しうるどの移動タイプでも到達できない位置にいる敵（＝迎え撃つしかない敵）
    unreachable_threats: Vec<ThreatTarget>,
    /// 自軍が生産しうるいずれかの移動タイプで到達できる位置にいる敵（＝殴りに行ける敵）
    reachable_threats: Vec<ThreatTarget>,
}

/// 1手番の全施設について一度だけ作ったV4生産計画。
/// 1命令ごとの再計画で残存脅威台帳を初期化しないため、同じ計画を順に消費する。
#[derive(Resource, Debug, Default)]
struct V4ProductionTurnPlan {
    player_id: Option<PlayerId>,
    turn: u32,
    commands: VecDeque<ProduceUnitCommand>,
}

/// 島嶼キャンペーンの完全パッケージをV4の汎用作戦より先に処理した結果。
enum CampaignProductionControl {
    /// 今回の呼び出しで発行するキャンペーン生産命令。
    Command(ProduceUnitCommand),
    /// 高優先作戦を完成できないため、汎用生産へ予算を流さず終了する。
    BlockGeneric,
    /// 島嶼作戦の予約額を除いた余剰だけで、迎撃・戦闘枠を生産する。
    ContinueWithSurplus(u32),
    /// キャンペーン要求が無いか全行を完成済みなので、V4汎用生産へ進める。
    Continue,
}

/// 生産候補 1 件。
#[derive(Debug, Clone, Copy)]
struct SlotCandidate {
    unit_type: UnitType,
    cost: u32,
    facility: GridPosition,
    /// 枠への適合度。大きいほど良い。
    fitness: f32,
}

#[derive(Debug, Clone, Copy)]
struct CandidateConstraints {
    remaining_funds: u32,
    per_slot_budget: u32,
    require_self_deployment: bool,
}

/// 生産命令と、その命令だけが持つV4作戦意図。
#[derive(Debug)]
struct PlannedProduction {
    command: ProduceUnitCommand,
    deployment: Option<PlannedDeployment>,
}

/// Combat / Intercept枠からpending deploymentへ渡す情報。
#[derive(Debug)]
struct PlannedDeployment {
    anchor: GridPosition,
    staging_anchor: GridPosition,
    posture: deployment::DeploymentPosture,
    slot_kind: SlotKind,
    priority_enemies: Vec<Entity>,
    threat_horizon: u32,
    forecast: deployment::DeploymentForecast,
    /// 永続Combat計画の生産step。旧Intercept等ではNone。
    plan_step: Option<PlanStepRef>,
}

impl std::ops::Deref for PlannedProduction {
    type Target = ProduceUnitCommand;

    fn deref(&self) -> &Self::Target {
        &self.command
    }
}

/// V4 の生産意思決定エントリポイント。
///
/// `decide_production` から `AiVersion::uses_operation_driven_production()` が
/// true のときだけ委譲される。V1/V2/V3 の経路には一切影響しない。
pub fn decide_production_v4(world: &mut World, player_id: PlayerId) -> Vec<ProduceUnitCommand> {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    // 島嶼キャンペーン生産だけでreturnする手番も含め、作戦の実績は毎ターン観測する。
    // beam searchの内側ではなくPlan/Entityを一度ずつ走査するため、探索量を増やさない。
    observe_plan_execution(world, player_id, turn);

    let campaign_surplus = match decide_campaign_production_v4(world, player_id) {
        CampaignProductionControl::Command(command) => return vec![command],
        CampaignProductionControl::BlockGeneric => return Vec::new(),
        CampaignProductionControl::ContinueWithSurplus(budget) => Some(budget),
        CampaignProductionControl::Continue => None,
    };

    let mut turn_plan = world
        .remove_resource::<V4ProductionTurnPlan>()
        .unwrap_or_default();
    if turn_plan.player_id == Some(player_id) && turn_plan.turn == turn {
        let next = turn_plan.commands.pop_front();
        world.insert_resource(turn_plan);
        return next.into_iter().collect();
    }

    let Some(mut scan) = BoardScan::collect(world, player_id) else {
        world.insert_resource(turn_plan);
        return Vec::new();
    };
    if let Some(budget) = campaign_surplus {
        scan.funds = scan.funds.min(budget);
    }
    // 既存戦力として見積へ入れるのは、実際にV4 Combat任務へ接続済みのEntityだけ。
    // 占領・輸送など別任務中のunitを「倒せるはず」と二重計上しない。
    let committed_combat_assignments = world
        .get_resource::<deployment::V4DeploymentRegistry>()
        .map(|registry| registry.active_target_assignments(player_id))
        .unwrap_or_default();
    let produced_plan_steps = world
        .get_resource::<deployment::V4DeploymentRegistry>()
        .map(|registry| registry.produced_plan_steps(player_id))
        .unwrap_or_default();
    let mut rolling_registry = world
        .remove_resource::<V4RollingPlanRegistry>()
        .unwrap_or_default();
    rolling_registry.reconcile_produced_steps(player_id, &produced_plan_steps);
    let (planned, plan_trace) = plan_production_with_registry(
        &scan,
        player_id,
        campaign_surplus.is_none(),
        &committed_combat_assignments,
        turn,
        &mut rolling_registry,
    );
    let closed_plan_ids = rolling_registry
        .audit_records(player_id)
        .into_iter()
        .filter(|audit| audit.turn == turn)
        .filter(|audit| {
            matches!(
                audit.disposition,
                PlanDisposition::Completed | PlanDisposition::Withdrawn
            )
        })
        .map(|audit| audit.plan_id)
        .collect::<HashSet<_>>();
    let active_plan_intents = rolling_registry.active_deployment_intents(
        player_id,
        scan.capital_assault_authorized,
        scan.capital_staging_anchor,
    );
    world.insert_resource(rolling_registry);

    // 診断traceとは別に、生産完了イベントと照合する作戦意図を永続化する。
    let pending = planned
        .iter()
        .enumerate()
        .filter_map(|(order, planned)| {
            let deployment = planned.deployment.as_ref()?;
            Some(deployment::PendingDeployment {
                player_id,
                turn,
                order: u32::try_from(order).unwrap_or(u32::MAX),
                facility: GridPosition {
                    x: planned.command.target_x,
                    y: planned.command.target_y,
                },
                unit_type: planned.command.unit_type,
                anchor: deployment.anchor,
                staging_anchor: deployment.staging_anchor,
                posture: deployment.posture,
                slot_kind: deployment.slot_kind,
                priority_enemies: deployment.priority_enemies.clone(),
                threat_horizon: deployment.threat_horizon,
                forecast: deployment.forecast,
                plan_step: deployment.plan_step,
            })
        })
        .collect::<Vec<_>>();
    let mut deployment_registry = world
        .remove_resource::<deployment::V4DeploymentRegistry>()
        .unwrap_or_default();
    deployment_registry.release_closed_plans(&closed_plan_ids);
    deployment_registry.replace_turn_orders(player_id, turn, pending);
    deployment_registry.refresh_plan_intents(&active_plan_intents);
    world.insert_resource(deployment_registry);
    let commands = planned
        .into_iter()
        .map(|planned| planned.command)
        .collect::<Vec<_>>();

    // 生産判断の内訳を診断リソースへ残す（判定は行わず記録のみ）。
    if let Some(mut diagnostics) = world.get_resource_mut::<ProductionTraceDiagnostics>() {
        diagnostics.record(turn, plan_trace);
    } else {
        let mut diagnostics = ProductionTraceDiagnostics::default();
        diagnostics.record(turn, plan_trace);
        world.insert_resource(diagnostics);
    }

    turn_plan.player_id = Some(player_id);
    turn_plan.turn = turn;
    turn_plan.commands = VecDeque::from(commands);
    let next = turn_plan.commands.pop_front();
    world.insert_resource(turn_plan);
    next.into_iter().collect()
}

/// 生産計画を実Entityの戦闘・損耗・目標HP・拠点占領へ接続する予実集計。
///
/// 診断用の金額はUnitTypeの生産費と現在HPから導出する。計画変更の判定自体は
/// 金額価値ではなく、初攻撃・敵排除・拠点占領の予定手番との差を使用する。
fn observe_plan_execution(world: &mut World, player_id: PlayerId, turn: u32) {
    let (audit_records, produced_steps) = world
        .get_resource::<deployment::V4DeploymentRegistry>()
        .map(|registry| {
            (
                registry.audit_records(player_id),
                registry.produced_plan_steps(player_id),
            )
        })
        .unwrap_or_default();

    let unit_costs = world
        .get_resource::<UnitRegistry>()
        .map(|registry| {
            audit_records
                .iter()
                .filter_map(|record| {
                    registry
                        .get_stats(record.unit_type)
                        .map(|stats| (record.unit_type, stats.cost))
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    let mut owned_properties = HashSet::new();
    let mut property_query = world.query::<(&GridPosition, &Property)>();
    for (position, property) in property_query.iter(world) {
        if property.owner_id == Some(player_id) {
            owned_properties.insert(*position);
        }
    }

    // 搭載中の敵も増援・残存目標として追うため、盤面走査の除外条件を使わない。
    let mut enemy_health = HashMap::new();
    let mut enemy_query = world.query::<(Entity, &Faction, &Health)>();
    for (entity, faction, health) in enemy_query.iter(world) {
        if faction.0 != player_id && health.current > 0 {
            enemy_health.insert(entity, health.current);
        }
    }

    let deployments = audit_records
        .into_iter()
        .filter_map(|record| {
            let plan_step = record.plan_step?;
            let unit_cost = unit_costs.get(&record.unit_type).copied().unwrap_or(0);
            let health = world.get::<Health>(record.entity).copied();
            let alive =
                world.get_entity(record.entity).is_ok() && health.is_some_and(|hp| hp.current > 0);
            let current_loss_value = health.map_or(unit_cost, |hp| {
                unit_cost.saturating_mul(hp.max.saturating_sub(hp.current)) / hp.max.max(1)
            });
            Some(DeploymentExecutionObservation {
                entity: record.entity,
                plan_id: plan_step.plan_id,
                plan_step: Some(plan_step),
                unit_cost,
                alive,
                // higher-priority任務へ一時preemptされSquadを失ったEntityを、Planが
                // 実行中の戦力として数えない。Defense待機はSquadを持つため残る。
                mission_active: alive && record.active && record.squad_id.is_some(),
                current_loss_value,
                first_attack_turn: record.first_attack_turn,
                attack_count: record.attack_count,
                priority_attack_count: record.priority_attack_count,
                kill_count: record.kill_count,
                damage_value_dealt: record.damage_value_dealt,
                counter_value_received: record.counter_value_received,
                destroyed_value: record.destroyed_value,
            })
        })
        .collect::<Vec<_>>();

    let mut registry = world
        .remove_resource::<V4RollingPlanRegistry>()
        .unwrap_or_default();
    registry.reconcile_produced_steps(player_id, &produced_steps);
    registry.observe_execution(
        player_id,
        turn,
        &owned_properties,
        &enemy_health,
        &deployments,
    );
    world.insert_resource(registry);
}

/// 戦術層が組んだ島嶼キャンペーンの不足を、V4固有の汎用作戦より先に生産する。
///
/// `AiTurnStrategyCache` は行動計画時に同じプレイヤーのportfolioを保持している。
/// 生産APIは1命令ごとに呼ばれるため、V3と同じcache queueへ完全パッケージを保存し、
/// shortfallの再計算による二重発注を防ぐ。高優先行を完成できないときはgenericを
/// blockし、輸送・積荷・戦闘戦力の一部だけを逐次投入しない。
fn decide_campaign_production_v4(
    world: &mut World,
    player_id: PlayerId,
) -> CampaignProductionControl {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let due_capital_purchases = world
        .get_resource::<V4RollingPlanRegistry>()
        .map(|registry| registry.capital_due_purchases(player_id, turn))
        .unwrap_or_default();
    let reserved_capital_facilities = due_capital_purchases
        .iter()
        .map(|(facility, _)| *facility)
        .collect::<HashSet<_>>();
    let reserved_capital_budget = due_capital_purchases
        .iter()
        .map(|(_, cost)| *cost)
        .fold(0_u32, u32::saturating_add);
    let plan_exists = world
        .get_resource::<crate::ai::engine::AiTurnStrategyCache>()
        .is_some_and(|cache| cache.campaign_production_planned(player_id));
    if plan_exists {
        let mut cache = world
            .remove_resource::<crate::ai::engine::AiTurnStrategyCache>()
            .unwrap_or_default();
        let next = loop {
            let next = cache.take_campaign_production_command(player_id);
            match next {
                Some(command)
                    if reserved_capital_facilities.contains(&GridPosition {
                        x: command.target_x,
                        y: command.target_y,
                    }) =>
                {
                    // 首都編成が今手番に使う施設は、局地購入から外す。
                    continue;
                }
                other => break other,
            }
        };
        let blocks_generic = cache.campaign_production_blocks_generic(player_id);
        let generic_budget = cache.campaign_production_generic_budget(player_id);
        world.insert_resource(cache);
        return match next {
            Some(command) => CampaignProductionControl::Command(command),
            None if reserved_capital_budget > 0 => CampaignProductionControl::ContinueWithSurplus(
                generic_budget
                    .unwrap_or(0)
                    .saturating_add(reserved_capital_budget),
            ),
            None if blocks_generic => CampaignProductionControl::BlockGeneric,
            None if generic_budget.is_some_and(|budget| budget > 0) => {
                CampaignProductionControl::ContinueWithSurplus(generic_budget.unwrap_or(0))
            }
            None => CampaignProductionControl::Continue,
        };
    }

    let mut shortfalls = world
        .get_resource::<crate::ai::engine::AiTurnStrategyCache>()
        .and_then(|cache| cache.campaign_portfolio(player_id))
        .map(|portfolio| portfolio.aggregate_missing_requirements())
        .unwrap_or_default();
    if shortfalls.is_empty() {
        return if reserved_capital_budget > 0 {
            CampaignProductionControl::ContinueWithSurplus(reserved_capital_budget)
        } else {
            CampaignProductionControl::Continue
        };
    }

    // 航空掃討は後段のrolling planへ委譲する。一方、敵領Assaultで輸送・配置枠まで
    // 予約済みの地上波はcampaign Squadへ渡さないと港で遊兵になるため、必要実体数と
    // その購入上限だけを残す。
    for shortfall in &mut shortfalls {
        if shortfall.decision != crate::ai::island_campaign::IslandCampaignDecision::Assault
            || shortfall.ground_combat_units == 0
        {
            shortfall.reserved_budget = shortfall
                .reserved_budget
                .saturating_sub(shortfall.combat_budget);
            shortfall.combat_budget = 0;
        }
    }

    let Some(scan) = BoardScan::collect(world, player_id) else {
        return CampaignProductionControl::BlockGeneric;
    };
    let enemy_stats: Vec<_> = scan
        .enemy_units
        .iter()
        .map(|unit| unit.stats.clone())
        .collect();
    let campaign_facilities = scan
        .free_facilities
        .iter()
        .filter(|(facility, _)| !reserved_capital_facilities.contains(facility))
        .copied()
        .collect::<Vec<_>>();
    let outcome = plan_campaign_with_expansion_denial_reserve(
        player_id,
        &shortfalls,
        &campaign_facilities,
        scan.owned_airport_count,
        &scan.available_types,
        &enemy_stats,
        &scan.damage_chart,
        &scan.map,
        &scan.master_data,
        scan.funds.saturating_sub(reserved_capital_budget),
    );
    let generic_budget = outcome.generic_funds;
    let mut cache = world
        .remove_resource::<crate::ai::engine::AiTurnStrategyCache>()
        .unwrap_or_default();
    cache.set_campaign_production_plan_with_generic_budget(
        player_id,
        outcome.commands,
        generic_budget,
    );
    let next = loop {
        let next = cache.take_campaign_production_command(player_id);
        match next {
            Some(command)
                if reserved_capital_facilities.contains(&GridPosition {
                    x: command.target_x,
                    y: command.target_y,
                }) =>
            {
                continue;
            }
            other => break other,
        }
    };
    let blocks_generic = cache.campaign_production_blocks_generic(player_id);
    let generic_budget = cache.campaign_production_generic_budget(player_id);
    world.insert_resource(cache);

    match next {
        Some(command) => CampaignProductionControl::Command(command),
        None if reserved_capital_budget > 0 => CampaignProductionControl::ContinueWithSurplus(
            generic_budget
                .unwrap_or(0)
                .saturating_add(reserved_capital_budget),
        ),
        None if blocks_generic => CampaignProductionControl::BlockGeneric,
        None if generic_budget.is_some_and(|budget| budget > 0) => {
            CampaignProductionControl::ContinueWithSurplus(generic_budget.unwrap_or(0))
        }
        None => CampaignProductionControl::Continue,
    }
}

/// 盤面から生産判断に必要な観測量をすべて取り出したもの。
struct BoardScan {
    map: Map,
    master_data: MasterDataRegistry,
    damage_chart: DamageChart,
    funds: u32,
    /// 生産可能な施設（未占有・生産範囲内・クールダウン対象外）
    free_facilities: Vec<(GridPosition, Terrain)>,
    /// 次ターン以降に空くことを見込める、生産範囲内の全所有施設。
    production_facilities: Vec<(GridPosition, Terrain)>,
    available_types: Vec<(UnitType, UnitStats)>,
    my_units: Vec<UnitSnapshot>,
    enemy_units: Vec<UnitSnapshot>,
    /// 首都の生産範囲内にある所有空港総数（占有中を含む）
    owned_airport_count: u32,
    /// 自軍が保有していない拠点（中立・敵）
    open_properties: Vec<GridPosition>,
    enemy_income: u32,
    enemy_production_slots: u32,
    enemy_facilities: Vec<EnemyFacilitySnapshot>,
    my_income: u32,
    /// 島campaignが実行を決めた作戦。汎用clusterへ事後照合せず、このanchorと
    /// IslandIdをCombat生産計画の入力としてそのまま使う。
    campaign_objectives: Vec<CampaignPlanningObjective>,
    /// 首都攻略部隊を前進させてよいのは、固定兵站経路の確保後だけである。
    capital_assault_authorized: bool,
    /// 編成中の首都攻略部隊を、生産施設から退避させて集結する自軍拠点。
    capital_staging_anchor: Option<GridPosition>,
}

#[derive(Debug, Clone)]
struct CampaignPlanningObjective {
    island_id: crate::ai::islands::IslandId,
    kind: OperationKind,
    anchor: GridPosition,
    capture_eta: Option<u32>,
    /// 固定兵站経路内の工程順。経路外campaignはNone。
    logistics_rank: Option<u32>,
    /// 同じ島の敵を別前線へ分配せず、この戦略作戦へ所属させる。
    forced_target_enemies: HashSet<Entity>,
    /// 編成中は目的地へ逐次投入せず、ここへ集結させる。
    staging_anchor: GridPosition,
    /// falseなら生産は行うが、作戦地点への攻撃任務はまだ発行しない。
    execution_authorized: bool,
}

impl BoardScan {
    fn collect(world: &mut World, player_id: PlayerId) -> Option<Self> {
        let map = world.get_resource::<Map>()?.clone();
        let unit_registry = world.get_resource::<UnitRegistry>()?.clone();
        let damage_chart = world.get_resource::<DamageChart>()?.clone();
        let master_data = world.get_resource::<MasterDataRegistry>()?.clone();
        let funds = world
            .get_resource::<Players>()?
            .0
            .iter()
            .find(|p| p.id == player_id)
            .map(|p| p.funds)?;
        let logistics_plan = world
            .get_resource::<logistics_plan::V4LogisticsPlanRegistry>()
            .and_then(|registry| registry.plan(player_id))
            .cloned();
        let logistics_ranks = logistics_plan
            .as_ref()
            .map(|plan| {
                plan.route_islands
                    .iter()
                    .enumerate()
                    .map(|(rank, island)| (*island, u32::try_from(rank).unwrap_or(u32::MAX)))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let mut campaign_objectives: Vec<CampaignPlanningObjective> = world
            .get_resource::<crate::ai::engine::AiTurnStrategyCache>()
            .and_then(|cache| cache.campaign_portfolio(player_id))
            .map(|portfolio| {
                portfolio
                    .defenses
                    .iter()
                    .chain(portfolio.active_offensives.iter())
                    .map(|assignment| {
                        let eta = portfolio
                            .islands
                            .iter()
                            .find(|assessment| assessment.island_id == assignment.island_id)
                            .and_then(|assessment| assessment.friendly_capture_eta);
                        CampaignPlanningObjective {
                            island_id: assignment.island_id,
                            kind: if assignment.decision
                                == crate::ai::island_campaign::IslandCampaignDecision::Defend
                            {
                                OperationKind::Defense
                            } else {
                                OperationKind::Capture
                            },
                            anchor: assignment.target_position,
                            capture_eta: eta,
                            logistics_rank: logistics_ranks.get(&assignment.island_id).copied(),
                            forced_target_enemies: HashSet::new(),
                            staging_anchor: assignment.target_position,
                            execution_authorized: true,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // 同一ターン内で生産に失敗した施設を除外するためのクールダウン
        let cooldown: HashSet<(usize, usize)> = world
            .get_resource::<crate::ai::engine::AiProductionCooldown>()
            .map(|c| c.0.clone())
            .unwrap_or_default();

        // --- ユニットの走査 ---
        let mut occupied = HashSet::new();
        let mut my_units = Vec::new();
        let mut enemy_units = Vec::new();
        {
            let mut q = world.query::<(
                Entity,
                &GridPosition,
                &Faction,
                &UnitStats,
                Option<&Health>,
                Option<&CargoCapacity>,
                Option<&Transporting>,
            )>();
            for (entity, pos, faction, stats, health, cargo, transporting) in q.iter(world) {
                // 輸送中のユニットは盤面を占有しない
                if transporting.is_some() {
                    continue;
                }
                occupied.insert(*pos);
                let snapshot = UnitSnapshot {
                    entity: Some(entity),
                    pos: *pos,
                    hp: health.map_or(100, |h| h.current),
                    free_cargo: cargo.map_or(stats.max_cargo, |c| {
                        stats.max_cargo.saturating_sub(c.loaded.len() as u32)
                    }),
                    stats: stats.clone(),
                };
                if faction.0 == player_id {
                    my_units.push(snapshot);
                } else {
                    enemy_units.push(snapshot);
                }
            }
        }

        // --- 拠点の走査 ---
        let mut capital_pos = None;
        let mut open_properties = Vec::new();
        let mut facilities = Vec::new();
        let mut production_facilities = Vec::new();
        let mut enemy_income = 0u32;
        let mut enemy_production_slots = 0u32;
        let mut enemy_facilities = Vec::new();
        let mut my_income = 0u32;
        let mut owned_airport_count = 0u32;
        let mut owned_properties = Vec::new();
        let mut enemy_capital = world
            .get_resource::<victory_roadmap::VictoryRoadmapRegistry>()
            .and_then(|registry| registry.roadmap(player_id))
            .and_then(|roadmap| roadmap.enemy_capital);
        {
            let mut q = world.query::<(&GridPosition, &Property)>();
            let mut enemy_capitals = HashMap::new();
            for (pos, prop) in q.iter(world) {
                if prop.owner_id == Some(player_id) && prop.terrain == Terrain::Capital {
                    capital_pos = Some(*pos);
                } else if let Some(owner) = prop.owner_id
                    && prop.terrain == Terrain::Capital
                {
                    enemy_capitals.insert(owner, *pos);
                    enemy_capital.get_or_insert(*pos);
                }
            }
            for (pos, prop) in q.iter(world) {
                let income = master_data.landscape_income(prop.terrain.as_str());
                let is_facility = master_data.is_production_facility(prop.terrain.as_str());
                match prop.owner_id {
                    Some(owner) if owner == player_id => {
                        my_income = my_income.saturating_add(income);
                        owned_properties.push((*pos, prop.terrain));
                        if prop.terrain == Terrain::Airport
                            && crate::systems::production::is_within_production_range(
                                capital_pos.as_slice(),
                                pos.x,
                                pos.y,
                                map.topology,
                            )
                        {
                            owned_airport_count = owned_airport_count.saturating_add(1);
                        }
                        let is_in_production_range = is_facility
                            && crate::systems::production::is_within_production_range(
                                capital_pos.as_slice(),
                                pos.x,
                                pos.y,
                                map.topology,
                            );
                        if is_in_production_range {
                            production_facilities.push((*pos, prop.terrain));
                        }
                        // 現在手番に命令できるのは、全生産施設のうち空いているものだけ。
                        if is_in_production_range
                            && !occupied.contains(pos)
                            && !cooldown.contains(&(pos.x, pos.y))
                        {
                            facilities.push((*pos, prop.terrain));
                        }
                    }
                    Some(owner) => {
                        enemy_income = enemy_income.saturating_add(income);
                        let is_usable_enemy_facility = is_facility
                            && enemy_capitals.get(&owner).is_some_and(|capital| {
                                crate::systems::production::is_within_production_range(
                                    std::slice::from_ref(capital),
                                    pos.x,
                                    pos.y,
                                    map.topology,
                                )
                            });
                        if is_usable_enemy_facility {
                            enemy_production_slots += 1;
                            enemy_facilities.push(EnemyFacilitySnapshot {
                                pos: *pos,
                                terrain: prop.terrain,
                            });
                        }
                        open_properties.push(*pos);
                    }
                    None => open_properties.push(*pos),
                }
            }
        }

        if facilities.is_empty() {
            return None;
        }

        let available_types: Vec<(UnitType, UnitStats)> = unit_registry
            .0
            .iter()
            .map(|(unit_type, stats)| (*unit_type, stats.clone()))
            .collect();

        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let enemy_capital_island = enemy_capital
            .and_then(|position| island_map.get_island_at(&position))
            .map(|island| island.id);
        let home_island = capital_pos
            .and_then(|position| island_map.get_island_at(&position))
            .map(|island| island.id);
        let completed_route_island = logistics_plan.as_ref().and_then(|plan| {
            plan.route_islands
                .iter()
                .rev()
                .copied()
                .find(|island| !plan.selected_islands.contains(island))
        });
        let staging_anchor = enemy_capital.and_then(|capital| {
            owned_properties
                .iter()
                .filter(|(position, terrain)| {
                    !master_data.is_production_facility(terrain.as_str())
                        && completed_route_island.is_none_or(|expected| {
                            island_map
                                .get_island_at(position)
                                .is_some_and(|island| island.id == expected)
                        })
                })
                .min_by_key(|(position, _)| {
                    map.distance(position.x, position.y, capital.x, capital.y)
                })
                .or_else(|| {
                    owned_properties
                        .iter()
                        .filter(|(_, terrain)| {
                            !master_data.is_production_facility(terrain.as_str())
                        })
                        .min_by_key(|(position, _)| {
                            map.distance(position.x, position.y, capital.x, capital.y)
                        })
                })
                .or_else(|| owned_properties.first())
                .map(|(position, _)| *position)
        });
        let capital_assault_authorized = enemy_capital_island.is_some_and(|capital_island| {
            home_island == Some(capital_island)
                || logistics_plan
                    .as_ref()
                    .is_some_and(|plan| plan.selected_islands.is_empty())
        });
        if let (Some(capital), Some(capital_island), Some(staging_anchor)) =
            (enemy_capital, enemy_capital_island, staging_anchor)
        {
            let forced_target_enemies = enemy_units
                .iter()
                .filter_map(|enemy| {
                    island_map
                        .get_island_at(&enemy.pos)
                        .is_some_and(|island| island.id == capital_island)
                        .then_some(enemy.entity)
                        .flatten()
                })
                .collect();
            campaign_objectives.push(CampaignPlanningObjective {
                island_id: capital_island,
                kind: OperationKind::AssaultCapital,
                anchor: capital,
                capture_eta: None,
                logistics_rank: None,
                forced_target_enemies,
                staging_anchor,
                execution_authorized: capital_assault_authorized,
            });
        }

        Some(BoardScan {
            map,
            master_data,
            damage_chart,
            funds,
            free_facilities: facilities,
            production_facilities,
            available_types,
            my_units,
            enemy_units,
            owned_airport_count,
            open_properties,
            enemy_income,
            enemy_production_slots,
            enemy_facilities,
            my_income,
            campaign_objectives,
            capital_assault_authorized,
            capital_staging_anchor: staging_anchor,
        })
    }

    /// 指定した拠点で `unit_type` を生産できるか。
    fn can_produce(&self, terrain: Terrain, unit_type: UnitType) -> bool {
        self.master_data
            .can_produce_unit(terrain.as_str(), unit_type)
    }

    /// 生産可能なユニットのうち、占領可能で最も安いものを「基準占領ユニット」とする。
    /// 展開リードタイムや到達可能性はこのユニットの足で測る。
    fn reference_capture_unit(&self) -> Option<&UnitStats> {
        self.available_types
            .iter()
            .filter(|(unit_type, stats)| {
                stats.can_capture
                    && self
                        .production_facilities
                        .iter()
                        .any(|(_, terrain)| self.can_produce(*terrain, *unit_type))
            })
            .min_by_key(|(_, stats)| stats.cost)
            .map(|(_, stats)| stats)
    }
}

/// 距離と移動力から到達ターン数を見積もる。
fn eta_turns(map: &Map, from: &GridPosition, to: &GridPosition, movement: u32) -> u32 {
    let distance = map.distance(from.x, from.y, to.x, to.y);
    distance.div_ceil(movement.max(1))
}

/// 盤面から作戦の一覧を組み立てる。
fn build_operations(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    active_objectives: &[ActivePlanObjective],
) -> Vec<Operation> {
    let Some(reference) = scan.reference_capture_unit().cloned() else {
        return Vec::new();
    };

    // V4の作戦源は、島campaignが選んだCapture/Defenseと、既に実行を始めた
    // Combat planだけに限定する。全未所有拠点を距離で束ねる第二の目標選定器を
    // 併用すると、勝利ロードマップ外の前線へ生産予算が流れるためである。
    let mut raw = active_objectives
        .iter()
        .filter(|objective| !objective.properties.is_empty())
        .map(|objective| (objective.kind, objective.properties.clone()))
        .collect::<Vec<_>>();

    let island_map = crate::ai::islands::IslandMap::analyze(&scan.map);
    let campaign_clusters = scan
        .campaign_objectives
        .iter()
        .map(|objective| {
            let mut properties = if objective.kind == OperationKind::Defense {
                vec![objective.anchor]
            } else {
                scan.open_properties
                    .iter()
                    .filter(|position| {
                        island_map
                            .get_island_at(position)
                            .is_some_and(|island| island.id == objective.island_id)
                    })
                    .copied()
                    .collect::<Vec<_>>()
            };
            if properties.is_empty() {
                // 全施設取得後のContest/Reinforceも、残敵への保持・掃討作戦として残す。
                properties.push(objective.anchor);
            }
            properties.sort_unstable_by_key(|position| (position.y, position.x));
            (objective, properties)
        })
        .collect::<Vec<_>>();
    raw.retain(|(kind, cluster)| {
        !campaign_clusters.iter().any(|(objective, _)| {
            objective.kind == *kind
                && cluster.iter().any(|property| {
                    island_map
                        .get_island_at(property)
                        .is_some_and(|island| island.id == objective.island_id)
                })
        })
    });
    raw.extend(
        campaign_clusters
            .iter()
            .map(|(objective, cluster)| (objective.kind, cluster.clone())),
    );

    let campaign_for_cluster = |kind: OperationKind, cluster: &[GridPosition]| {
        campaign_clusters
            .iter()
            .find_map(|(objective, properties)| {
                (objective.kind == kind
                    && cluster.iter().any(|property| properties.contains(property)))
                .then_some(*objective)
            })
    };

    // 生産施設から近い作戦を優先して MAX_OPERATIONS 件に絞る
    let mut scored: Vec<(bool, u32, OperationKind, Vec<GridPosition>)> = raw
        .into_iter()
        .filter(|(_, cluster)| !cluster.is_empty())
        .map(|(kind, cluster)| {
            let anchor = campaign_for_cluster(kind, &cluster)
                .map_or_else(|| anchor_of(&cluster, scan), |objective| objective.anchor);
            let lead = facility_lead_time(scan, &anchor, reference.max_movement);
            let continuing = active_objectives.iter().any(|objective| {
                objective.kind == kind
                    && cluster
                        .iter()
                        .any(|property| objective.properties.contains(property))
            });
            (continuing, lead, kind, cluster)
        })
        .collect();
    scored.sort_by_key(|(continuing, lead, kind, cluster)| {
        let logistics_rank = campaign_for_cluster(*kind, cluster)
            .and_then(|objective| objective.logistics_rank)
            .unwrap_or(u32::MAX);
        (
            kind.priority_rank(),
            logistics_rank,
            !*continuing,
            *lead,
            // 同条件なら拠点数の多い（面が広い）作戦を優先
            usize::MAX - cluster.len(),
        )
    });
    // 防衛作戦は priority_rank が最上位なので、素直に truncate すると防衛だけで枠が埋まり、
    // 占領作戦が 1 つも残らずに拡張が完全停止する（＝ジリ貧）ことがある。
    // 占領目標が残っている限り、最良の占領作戦を 1 枠だけ確保する。
    let rescued_capture = if scored[..scored.len().min(MAX_OPERATIONS)]
        .iter()
        .any(|(_, _, kind, _)| *kind == OperationKind::Capture)
    {
        None
    } else {
        scored
            .iter()
            .position(|(_, _, kind, _)| *kind == OperationKind::Capture)
            .map(|index| scored.remove(index))
    };
    scored.truncate(MAX_OPERATIONS);
    if let Some(capture) = rescued_capture {
        // 最も優先度の低い枠を明け渡して占領作戦を差し込む
        if scored.len() >= MAX_OPERATIONS {
            scored.pop();
        }
        scored.push(capture);
    }
    // 首都作戦は局地作戦より優先度が低いが、候補集合から消してはならない。
    // 消えると兵站作戦後の資金が勝利条件へ一切予約されなくなる。
    let rescued_assault = if scored
        .iter()
        .any(|(_, _, kind, _)| *kind == OperationKind::AssaultCapital)
    {
        None
    } else {
        scan.campaign_objectives
            .iter()
            .find(|objective| objective.kind == OperationKind::AssaultCapital)
            .and_then(|objective| {
                let cluster = campaign_clusters
                    .iter()
                    .find(|(candidate, _)| candidate.kind == OperationKind::AssaultCapital)
                    .map(|(_, properties)| properties.clone())?;
                let lead = facility_lead_time(scan, &objective.anchor, reference.max_movement);
                Some((false, lead, OperationKind::AssaultCapital, cluster))
            })
    };
    if let Some(assault) = rescued_assault {
        if scored.len() >= MAX_OPERATIONS {
            scored.pop();
        }
        scored.push(assault);
    }

    let anchors: Vec<GridPosition> = scored
        .iter()
        .map(|(_, _, kind, cluster)| {
            campaign_for_cluster(*kind, cluster)
                .map_or_else(|| anchor_of(cluster, scan), |objective| objective.anchor)
        })
        .collect();
    let horizons: Vec<u32> = scored
        .iter()
        .map(|(_, lead, kind, _)| operation_threat_horizon(*kind, *lead))
        .collect();
    let empty_entity_set = HashSet::new();

    scored
        .into_iter()
        .enumerate()
        .map(|(index, (_, lead, kind, cluster))| {
            let anchor = anchors[index];
            let planning_objective = campaign_for_cluster(kind, &cluster);
            let forced_target_enemies = planning_objective
                .map(|objective| &objective.forced_target_enemies)
                .or_else(|| {
                    active_objectives
                        .iter()
                        .find(|objective| objective.kind == kind && objective.properties == cluster)
                        .map(|objective| &objective.target_enemies)
                })
                .unwrap_or(&empty_entity_set);
            let mut operation = build_operation(
                scan,
                ctx,
                &reference,
                kind,
                anchor,
                &anchors,
                &horizons,
                &cluster,
                forced_target_enemies,
                lead,
            );
            if let Some(objective) = planning_objective {
                operation.staging_anchor = objective.staging_anchor;
                operation.execution_authorized = objective.execution_authorized;
            }
            operation
        })
        .collect()
}

/// クラスタの代表地点。自軍生産施設に最も近い拠点を選ぶ。
fn anchor_of(cluster: &[GridPosition], scan: &BoardScan) -> GridPosition {
    cluster
        .iter()
        .copied()
        .min_by_key(|pos| {
            scan.free_facilities
                .iter()
                .map(|(f, _)| scan.map.distance(f.x, f.y, pos.x, pos.y))
                .min()
                .unwrap_or(u32::MAX)
        })
        .unwrap_or(cluster[0])
}

/// 生産施設から代表地点までの展開リードタイム（最短）。
fn facility_lead_time(scan: &BoardScan, anchor: &GridPosition, movement: u32) -> u32 {
    scan.free_facilities
        .iter()
        .map(|(f, _)| eta_turns(&scan.map, f, anchor, movement))
        .min()
        .unwrap_or(u32::MAX)
}

fn operation_threat_horizon(kind: OperationKind, deploy_lead_time: u32) -> u32 {
    match kind {
        OperationKind::Defense => DEFENSE_THREAT_ETA,
        OperationKind::Capture => deploy_lead_time.saturating_add(CAPTURE_COMPLETION_TURNS),
        OperationKind::AssaultCapital => {
            deploy_lead_time.saturating_add(rolling_plan::DEFAULT_SEARCH_TURNS)
        }
    }
}

/// 敵が期限内に自力到着できる作戦だけを比較し、その中で最短の1件へ帰属させる。
fn nearest_relevant_anchor_index(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    pos: &GridPosition,
    movement: MovementType,
    max_movement: u32,
    anchors: &[GridPosition],
    horizons: &[u32],
) -> Option<usize> {
    anchors
        .iter()
        .enumerate()
        .filter_map(|(index, anchor)| {
            if !ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (pos.x, pos.y),
                (anchor.x, anchor.y),
                movement,
            ) {
                return None;
            }
            let eta = eta_turns(&scan.map, pos, anchor, max_movement);
            (eta <= horizons.get(index).copied().unwrap_or(0)).then_some((eta, index))
        })
        .min()
        .map(|(_, index)| index)
}

/// 敵を、地形的に到達できる最寄りの作戦へ一意に帰属させる。
///
/// 作戦の期限は毎ターン変化する予測値であり、敵が局地目標から遠いという理由で
/// Combat計画の入力から消してはならない。期限は防衛案の比較にだけ用いる。
fn nearest_reachable_anchor_index(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    pos: &GridPosition,
    movement: MovementType,
    max_movement: u32,
    anchors: &[GridPosition],
) -> Option<usize> {
    anchors
        .iter()
        .enumerate()
        .filter_map(|(index, anchor)| {
            ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (pos.x, pos.y),
                (anchor.x, anchor.y),
                movement,
            )
            .then_some((eta_turns(&scan.map, pos, anchor, max_movement), index))
        })
        .min()
        .map(|(_, index)| index)
}

fn enemy_facility_arrival(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    facility: EnemyFacilitySnapshot,
    anchor: GridPosition,
) -> Option<(u32, u32)> {
    scan.available_types
        .iter()
        .filter(|(unit_type, stats)| {
            stats.max_cargo == 0 && scan.can_produce(facility.terrain, *unit_type)
        })
        .filter_map(|(_, stats)| {
            ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (facility.pos.x, facility.pos.y),
                (anchor.x, anchor.y),
                stats.movement_type,
            )
            .then_some((
                eta_turns(&scan.map, &facility.pos, &anchor, stats.max_movement),
                stats.cost,
            ))
        })
        .min_by_key(|(eta, cost)| (*eta, *cost))
}

/// 敵施設も期限内に到着できる最寄り作戦へ一意に割り当て、全収入の重複計上を防ぐ。
fn projected_enemy_reinforcement_budget(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    anchors: &[GridPosition],
    horizons: &[u32],
    anchor_index: usize,
) -> u32 {
    if scan.enemy_production_slots == 0 {
        return 0;
    }
    let mut local_slots = 0_u32;
    let mut production_capacity = 0_u32;
    for facility in &scan.enemy_facilities {
        let assignment = anchors
            .iter()
            .enumerate()
            .filter_map(|(index, anchor)| {
                let (eta, cost) = enemy_facility_arrival(scan, ctx, *facility, *anchor)?;
                (eta <= horizons.get(index).copied().unwrap_or(0)).then_some((eta, index, cost))
            })
            .min_by_key(|(eta, index, _)| (*eta, *index));
        let Some((eta, assigned_index, unit_cost)) = assignment else {
            continue;
        };
        if assigned_index != anchor_index {
            continue;
        }
        let production_turns = horizons[anchor_index].saturating_sub(eta);
        if production_turns == 0 {
            continue;
        }
        local_slots = local_slots.saturating_add(1);
        production_capacity =
            production_capacity.saturating_add(unit_cost.saturating_mul(production_turns));
    }
    if local_slots == 0 {
        return 0;
    }
    let allocated_income_per_turn = u64::from(scan.enemy_income)
        .saturating_mul(u64::from(local_slots))
        / u64::from(scan.enemy_production_slots.max(1));
    let income_capacity =
        allocated_income_per_turn.saturating_mul(u64::from(horizons[anchor_index]));
    production_capacity.min(u32::try_from(income_capacity).unwrap_or(u32::MAX))
}

/// 1 つの作戦について観測量を集め、枠を導出する。
#[allow(clippy::too_many_arguments)]
fn build_operation(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    reference: &UnitStats,
    kind: OperationKind,
    anchor: GridPosition,
    anchors: &[GridPosition],
    horizons: &[u32],
    cluster: &[GridPosition],
    forced_target_enemies: &HashSet<Entity>,
    deploy_lead_time: u32,
) -> Operation {
    // この作戦を「最寄りの作戦」とするユニットだけを、この作戦の担当として数える。
    // これにより 1 体のユニットが複数作戦に二重計上されない。
    let anchor_index = anchors.iter().position(|candidate| *candidate == anchor);

    // 基準占領ユニットが自力で到達できるかどうかで輸送要否が決まる。
    // 「島だから輸送が要る」ではなく「地形的に繋がっていないから要る」と判定する。
    let requires_transport = !scan.free_facilities.iter().any(|(f, _)| {
        ctx.is_reachable(
            &scan.map,
            &scan.master_data,
            (f.x, f.y),
            (anchor.x, anchor.y),
            reference.movement_type,
        )
    });

    // 敵戦力は、現在の局地敵と将来到着する増援に分離する。
    // 別島の敵Entityを具体的な撃破目標へ入れると、生産Entityがanchorを離れて
    // 敵初期空港まで追跡し、輸送・占領工程を前進させない。別島から期限内に
    // 到着できる敵はanchor到着時点の仮想増援として計画へ入れ、現在の任務対象にしない。
    //
    // (1) を基準占領ユニット（歩兵）の足で判定してはならない。
    // 海の向こうで拠点を取り続ける敵の占領部隊は、歩兵の足では届かないというだけで
    // 脅威の集計から丸ごと消え、撃破枠が立たず、対抗候補の採点対象にもならなくなる。
    // 実際にはヘリ・艦船・航空機で届くのだから、生産しうる移動タイプ全体で問う。
    // 「制空で応じるか対空で応じるか」が思想ではなく到達可能性の問題であるのと同じで、
    // 「敵の占領部隊を潰しに行けるか」もまた到達可能性の問題でしかない。
    let producible_movement_types: Vec<MovementType> = {
        let mut seen = HashSet::new();
        scan.available_types
            .iter()
            .map(|(_, stats)| stats.movement_type)
            .filter(|movement_type| seen.insert(*movement_type))
            .collect()
    };

    // 未取得の拠点が残っている限り、占領レースは進行中である。
    // その間、敵の拡張装置は素のコスト以上の脅威として数える。
    let expansion_race_live = !scan.open_properties.is_empty();

    let mut reachable_threats = Vec::new();
    let mut unreachable_threats = Vec::new();
    let mut enemy_combat_value = 0u32;
    let mut territory_control_threat_units = 0u32;
    let mut unreachable_threat_value = 0u32;
    let mut enemy_contact_eta = u32::MAX;
    let island_map = crate::ai::islands::IslandMap::analyze(&scan.map);
    let anchor_island = island_map.get_island_at(&anchor).map(|island| island.id);
    for enemy in &scan.enemy_units {
        let forced_target = enemy
            .entity
            .is_some_and(|entity| forced_target_enemies.contains(&entity));
        if !forced_target
            && nearest_reachable_anchor_index(
                scan,
                ctx,
                &enemy.pos,
                enemy.stats.movement_type,
                enemy.stats.max_movement,
                anchors,
            ) != anchor_index
        {
            continue;
        }
        let i_can_reach = producible_movement_types.iter().any(|movement_type| {
            ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (anchor.x, anchor.y),
                (enemy.pos.x, enemy.pos.y),
                *movement_type,
            )
        });
        let it_can_reach_me = ctx.is_reachable(
            &scan.map,
            &scan.master_data,
            (enemy.pos.x, enemy.pos.y),
            (anchor.x, anchor.y),
            enemy.stats.movement_type,
        );
        debug_assert!(
            forced_target || it_can_reach_me,
            "到達可能な最寄り作戦へ敵を帰属済み"
        );
        let arrival_eta = eta_turns(&scan.map, &enemy.pos, &anchor, enemy.stats.max_movement);
        if it_can_reach_me {
            enemy_contact_eta = enemy_contact_eta.min(arrival_eta);
        }
        let enemy_island = island_map.get_island_at(&enemy.pos).map(|island| island.id);
        let local_contact = enemy_island == anchor_island || arrival_eta == 0;
        if i_can_reach && local_contact {
            let threat = threat_value(enemy, expansion_race_live);
            enemy_combat_value = enemy_combat_value.saturating_add(threat);
            if expansion_race_live && is_territory_control_threat(&enemy.stats) {
                territory_control_threat_units = territory_control_threat_units.saturating_add(1);
            }
            reachable_threats.push(ThreatTarget::from_snapshot(enemy, expansion_race_live));
        } else if !i_can_reach && local_contact {
            let threat = threat_value(enemy, expansion_race_live);
            unreachable_threat_value = unreachable_threat_value.saturating_add(threat);
            unreachable_threats.push(ThreatTarget::from_snapshot(enemy, expansion_race_live));
        } else if it_can_reach_me
            // 空の輸送unitは単独では占領も攻撃もできない。全ての中立前線で
            // 「将来来る輸送ヘリ」として対空購入を発生させず、実cargoを搭載して
            // 作戦を開始した時点から到着scenarioへ含める。
            && (enemy.stats.max_cargo == 0 || enemy.free_cargo < enemy.stats.max_cargo)
            && arrival_eta
                <= horizons
                    .get(anchor_index.unwrap_or(usize::MAX))
                    .copied()
                    .unwrap_or(0)
        {
            // 具体Entityを追わせず、到着後にだけ攻撃可能な増援として保持する。
            let mut incoming = ThreatTarget::from_snapshot(enemy, expansion_race_live);
            incoming.entity = None;
            incoming.position = anchor;
            incoming.available_turn = arrival_eta.max(1);
            enemy_combat_value =
                enemy_combat_value.saturating_add(threat_value(enemy, expansion_race_live));
            reachable_threats.push(incoming);
        }
    }
    // --- 自軍戦力の仕分け ---
    // 敵の仕分けが済んでから数える。
    //
    // 台帳（既存戦力の計上）は、必ず `slot_fitness` の採用条件と同じ判定を使う。
    // 両者がずれると「その枠を埋められるのに、その枠の充足としては数えられない」
    // ユニットが生まれ、枠の要求が永久に減らずに同じユニットを買い続けるラチェットになる。
    // そのため排他に振り分けず、埋められる枠すべてに計上する。
    // （1 体の対空ユニットが航空脅威の抑止と地上の頭数を兼ねるのは実態としても正しい）
    //
    // ここで「最寄りの作戦か」で仕分けてはならない。自軍ユニットは自陣の生産施設で
    // 生まれるので、位置で排他に割り振ると母港に近い作戦が全部を吸い、渡洋作戦側の
    // 台帳は永久に 0 のままになる。要求が一切減らないので同じユニットを毎ターン
    // 買い続けるラチェットになる（実測で歩兵 56 体、揚陸艇 7 隻、対空 20 両）。
    // 上限（`MAX_CAPTURE_SLOTS` 等）はあくまで 1 波の規模であって、
    // 「既に持っている分」を差し引く役割は担っていない。差し引きはこの台帳の仕事。
    let mut friendly_capture_units_committed = 0u32;
    let mut friendly_combat_value_committed = 0u32;
    let mut friendly_territory_control_units = 0u32;
    let mut friendly_intercept_value_committed = 0u32;
    let mut available_free_cargo_slots = 0u32;
    for unit in &scan.my_units {
        if unit.stats.can_capture {
            // 占領枠の採用条件と同じ関数で数える（自力到達 or 輸送の当てがある）
            if can_join_operation(
                scan,
                ctx,
                &anchor,
                requires_transport,
                &unit.pos,
                &unit.stats,
            ) {
                friendly_capture_units_committed += 1;
            }
        } else if unit.stats.max_cargo == 0 {
            // 迎撃枠の条件：到達できない脅威へ有効打を持ち、かつ自力で現地へ行ける
            let self_deployable = ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (unit.pos.x, unit.pos.y),
                (anchor.x, anchor.y),
                unit.stats.movement_type,
            );
            if self_deployable
                && threats_have_counter(
                    &unit.stats,
                    &unreachable_threats,
                    &(0..unreachable_threats.len()).collect::<Vec<_>>(),
                    &scan.damage_chart,
                )
            {
                friendly_intercept_value_committed =
                    friendly_intercept_value_committed.saturating_add(unit.value());
                let indices: Vec<usize> = (0..unreachable_threats.len()).collect();
                apply_coverage(
                    &unit.stats,
                    unit.value() as f32,
                    &mut unreachable_threats,
                    &indices,
                    &scan.damage_chart,
                );
            }
            // 撃破枠の条件：現地へ行けて、自分が実際に届く敵に対して有効打を持つ。
            // 敵が観測できない段階では誰でも採用されうるので、台帳側も同様に全員を数える。
            if !can_join_operation(
                scan,
                ctx,
                &anchor,
                requires_transport,
                &unit.pos,
                &unit.stats,
            ) {
                continue;
            }
            let origin = if self_deployable { unit.pos } else { anchor };
            let engageable =
                reachable_threat_indices(scan, ctx, &reachable_threats, origin, &unit.stats);
            let combat_eligible = reachable_threats.is_empty()
                || threats_have_counter(
                    &unit.stats,
                    &reachable_threats,
                    &engageable,
                    &scan.damage_chart,
                );
            if combat_eligible {
                // 1体が同じ手番に攻撃できる作戦は1つだけである。unit価格による
                // 戦力価値もsortie体数も、期限内に到着できる最寄り作戦へ排他的に
                // 帰属させ、全前線で同じ1体を重複控除しない。
                let belongs_to_control_operation = nearest_relevant_anchor_index(
                    scan,
                    ctx,
                    &unit.pos,
                    unit.stats.movement_type,
                    unit.stats.max_movement,
                    anchors,
                    horizons,
                ) == anchor_index;
                if !belongs_to_control_operation {
                    continue;
                }
                if engageable.iter().any(|index| {
                    let threat = &reachable_threats[*index];
                    is_territory_control_threat(&threat.stats)
                        && coverage_efficiency(&unit.stats, &threat.stats, &scan.damage_chart) > 0.0
                }) {
                    friendly_territory_control_units =
                        friendly_territory_control_units.saturating_add(1);
                }
                // 購入価格は撃破済み価値ではない。作戦期限までに残存敵へ実際に
                // 与えられる期待交換価値だけを、撃破要求の充足として控除する。
                let expected_return = apply_sortie_return(
                    &unit.stats,
                    CAPTURE_COMPLETION_TURNS,
                    &mut reachable_threats,
                    &engageable,
                    &scan.damage_chart,
                );
                friendly_combat_value_committed =
                    friendly_combat_value_committed.saturating_add(return_budget(expected_return));
            }
        }
    }

    // 輸送台帳も同じ原則で数える。輸送枠の採用条件は「その積荷をこの作戦地点へ
    // 届けられるか」なので、台帳も同じく `can_deliver_cargo` で数える。
    let cargo_movements: Vec<MovementType> = {
        let mut seen = HashSet::new();
        scan.available_types
            .iter()
            .filter(|(_, stats)| stats.can_capture)
            .map(|(_, stats)| stats.movement_type)
            .filter(|movement_type| seen.insert(*movement_type))
            .collect()
    };
    for unit in &scan.my_units {
        if unit.free_cargo == 0 {
            continue;
        }
        let deliverable = cargo_movements.iter().any(|cargo_movement| {
            can_deliver_cargo(
                &scan.map,
                &scan.master_data,
                ctx,
                &unit.pos,
                &anchor,
                unit.stats.movement_type,
                *cargo_movement,
            )
        });
        if deliverable {
            available_free_cargo_slots = available_free_cargo_slots.saturating_add(unit.free_cargo);
        }
    }

    // 必要戦力を丸い比率で水増しせず、実際にこの作戦へ参加できる最安の
    // 対抗ユニット単位へ切り上げる。候補採用と同じ条件を通すことで、
    // 要求額だけ存在して埋められない枠を作らない。
    let mut minimum_combat_unit_cost = u32::MAX;
    let mut minimum_intercept_unit_cost = u32::MAX;
    let unreachable_indices: Vec<usize> = (0..unreachable_threats.len()).collect();
    for (facility, terrain) in &scan.free_facilities {
        for (unit_type, stats) in &scan.available_types {
            if stats.can_capture
                || stats.max_cargo > 0
                || stats.cost == 0
                || !scan.can_produce(*terrain, *unit_type)
            {
                continue;
            }

            let self_deployable = ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (facility.x, facility.y),
                (anchor.x, anchor.y),
                stats.movement_type,
            );
            if self_deployable
                && threats_have_counter(
                    stats,
                    &unreachable_threats,
                    &unreachable_indices,
                    &scan.damage_chart,
                )
            {
                minimum_intercept_unit_cost = minimum_intercept_unit_cost.min(stats.cost);
            }

            if !can_join_operation(scan, ctx, &anchor, requires_transport, facility, stats) {
                continue;
            }
            let origin = if self_deployable { *facility } else { anchor };
            let indices = reachable_threat_indices(scan, ctx, &reachable_threats, origin, stats);
            if reachable_threats.is_empty()
                || threats_have_counter(stats, &reachable_threats, &indices, &scan.damage_chart)
            {
                minimum_combat_unit_cost = minimum_combat_unit_cost.min(stats.cost);
            }
        }
    }
    let minimum_combat_unit_cost = if minimum_combat_unit_cost != u32::MAX {
        minimum_combat_unit_cost
    } else {
        0
    };
    let minimum_intercept_unit_cost = if minimum_intercept_unit_cost != u32::MAX {
        minimum_intercept_unit_cost
    } else {
        0
    };
    let enemy_reinforcement_budget = anchor_index.map_or(0, |index| {
        projected_enemy_reinforcement_budget(scan, ctx, anchors, horizons, index)
    });

    // 輸送 1 往復にかかるターン数（片道リードタイムの 2 倍）
    let transport_round_trip_turns = deploy_lead_time.saturating_mul(2).max(1);

    let facts = OperationFacts {
        target_property_count: cluster.len() as u32,
        friendly_capture_units_committed,
        friendly_combat_value_committed,
        friendly_intercept_value_committed,
        enemy_combat_value,
        enemy_reinforcement_budget,
        minimum_combat_unit_cost,
        territory_control_threat_units,
        friendly_territory_control_units,
        territory_control_window_turns: CAPTURE_COMPLETION_TURNS,
        minimum_intercept_unit_cost,
        deploy_lead_time,
        enemy_contact_eta: if enemy_contact_eta == u32::MAX {
            u32::MAX
        } else {
            enemy_contact_eta
        },
        requires_transport,
        transport_round_trip_turns,
        available_free_cargo_slots,
        unreachable_threat_value,
    };

    let mut slots = derive_slots(&facts);
    // Combat枠は金額の差分ではなく、観測敵が残っていることだけで計画器を起動する。
    // 必要数と完了判定はrolling plannerのHPシミュレーションが決める。
    slots.destroy_budget = u32::from(
        !reachable_threats.is_empty()
            || (kind == OperationKind::AssaultCapital && enemy_reinforcement_budget > 0),
    );
    if kind == OperationKind::AssaultCapital {
        // 首都準備段階では戦闘編成だけを形成する。占領兵と輸送は兵站路確保後に
        // IslandCampaignが実行可能な波として組み、ここで汎用任務へ流さない。
        slots.capture_units = 0;
        slots.transport_slots = 0;
        slots.intercept_budget = 0;
        slots.escort_units = 0;
    }

    Operation {
        kind,
        anchor,
        staging_anchor: anchor,
        execution_authorized: true,
        objective_properties: cluster.to_vec(),
        threat_horizon: anchor_index
            .and_then(|index| horizons.get(index).copied())
            .unwrap_or(0),
        slots,
        facts,
        filled: OperationSlots::default(),
        unreachable_threats,
        reachable_threats,
    }
}

/// 作戦一覧と資金から、この生産フェーズで発行する生産命令を組み立てる。
#[cfg(test)]
fn plan_production(
    scan: &BoardScan,
    player_id: PlayerId,
    allow_structural_slots: bool,
    committed_combat_assignments: &HashMap<Entity, deployment::ActiveTargetAssignment>,
) -> (Vec<PlannedProduction>, ProductionPlanTrace) {
    let mut registry = V4RollingPlanRegistry::default();
    plan_production_with_registry(
        scan,
        player_id,
        allow_structural_slots,
        committed_combat_assignments,
        0,
        &mut registry,
    )
}

fn plan_production_with_registry(
    scan: &BoardScan,
    player_id: PlayerId,
    allow_structural_slots: bool,
    committed_combat_assignments: &HashMap<Entity, deployment::ActiveTargetAssignment>,
    turn: u32,
    plan_registry: &mut V4RollingPlanRegistry,
) -> (Vec<PlannedProduction>, ProductionPlanTrace) {
    let mut ctx = ReachCtx::default();
    let active_objectives = plan_registry.active_objectives(player_id);
    let mut operations = build_operations(scan, &mut ctx, &active_objectives);
    let mut plan_trace =
        ProductionPlanTrace::new(player_id, scan.funds, scan.free_facilities.len());

    if operations.is_empty() {
        plan_trace.fallback = true;
        if !allow_structural_slots {
            return (Vec::new(), plan_trace);
        }
        return (
            fallback_production(scan, player_id)
                .into_iter()
                .map(|command| PlannedProduction {
                    command,
                    deployment: None,
                })
                .collect(),
            plan_trace,
        );
    }

    // campaignと切り離した汎用clusterが、固定兵站工程のCombat予算を先取りしない。
    // 防衛を最上位に保ちつつ、Capture同士では兵站工程順、その後に敵接触ETAで並べる。
    operations.sort_by_key(|op| {
        let logistics_rank = scan
            .campaign_objectives
            .iter()
            .find(|objective| objective.anchor == op.anchor)
            .and_then(|objective| objective.logistics_rank)
            .unwrap_or(u32::MAX);
        (
            op.kind.priority_rank(),
            logistics_rank,
            op.facts.enemy_contact_eta,
        )
    });
    if !allow_structural_slots {
        // 占領要員と輸送役は島嶼キャンペーン側で予約済み。余剰予算を同じ役割へ
        // 二重投入せず、観測済みの敵に対する迎撃・護衛・撃破だけへ使う。
        for operation in &mut operations {
            operation.slots.capture_units = 0;
            operation.slots.transport_slots = 0;
        }
    }

    plan_trace.operations = operations
        .iter()
        .map(|op| ProductionOperationTrace {
            kind: op.kind,
            anchor: op.anchor,
            slots: op.slots,
            requires_transport: op.facts.requires_transport,
            enemy_combat_value: op.facts.enemy_combat_value,
            enemy_reinforcement_budget: op.facts.enemy_reinforcement_budget,
            minimum_combat_unit_cost: op.facts.minimum_combat_unit_cost,
            friendly_combat_value_committed: op.facts.friendly_combat_value_committed,
            deploy_lead_time: op.facts.deploy_lead_time,
        })
        .collect();

    // 1体は1手番に1作戦しか遂行できない。過去のpriority enemyが移動して複数作戦へ
    // 分散しても、現在位置から最も近い1作戦だけへ既存Combat Entityを帰属させる。
    let mut committed_by_operation: HashMap<usize, HashMap<Entity, Option<plan_revision::PlanId>>> =
        HashMap::new();
    for (&entity, assignment) in committed_combat_assignments {
        let Some(unit) = scan
            .my_units
            .iter()
            .find(|unit| unit.entity == Some(entity))
        else {
            continue;
        };
        let assigned_operation = operations
            .iter()
            .enumerate()
            .filter(|(_, operation)| {
                operation.reachable_threats.iter().any(|threat| {
                    threat
                        .entity
                        .is_some_and(|enemy| assignment.targets.contains(&enemy))
                })
            })
            .min_by_key(|(_, operation)| {
                scan.map.distance(
                    unit.pos.x,
                    unit.pos.y,
                    operation.anchor.x,
                    operation.anchor.y,
                )
            })
            .map(|(index, _)| index);
        if let Some(index) = assigned_operation {
            committed_by_operation
                .entry(index)
                .or_default()
                .insert(entity, assignment.plan_id);
        }
    }

    let mut used_facilities: HashSet<GridPosition> = HashSet::new();
    // 占有済みという事実だけでなく、どの作戦が枠を使ったかも保持する。
    // これにより占領作戦同士の競合を「上位防衛による中断」と誤認しない。
    let mut facility_owners: HashMap<GridPosition, OperationKind> = HashMap::new();
    // rolling plannerは1作戦につき1回だけ実行し、その混成パッケージの当手番分を
    // 施設ごとに順次消費する。施設を1つ埋めるたびに同じbeam searchをやり直さない。
    let mut rolling_plans: HashMap<usize, SelectedPlan> = HashMap::new();
    let mut seen_plan_ids = HashSet::new();
    let empty_committed_entities = HashSet::new();
    let mut remaining_funds = scan.funds;
    let mut commands = Vec::new();

    loop {
        let free_slots = scan
            .free_facilities
            .iter()
            .filter(|(pos, _)| !used_facilities.contains(pos))
            .count();
        if free_slots == 0 {
            break;
        }
        // 1 枠あたり予算。高価なユニットで枠を食い潰さないためのソフト上限。
        let per_slot_budget = remaining_funds / free_slots as u32;

        // 最も不足している枠を持つ作戦から順に見ていく
        let Some((op_index, slot_kind)) = most_starved_slot(&operations) else {
            break;
        };

        // トレース用に、選定前の未充足率と作戦の識別情報を控えておく。
        let operation_kind = operations[op_index].kind;
        let operation_anchor = operations[op_index].anchor;
        let deficit_before = operations[op_index]
            .slots
            .deficit_ratio(slot_kind, &operations[op_index].filled);
        let remaining_funds_before = remaining_funds;

        if slot_kind == SlotKind::Combat && !rolling_plans.contains_key(&op_index) {
            let target_enemies = operations[op_index]
                .reachable_threats
                .iter()
                .filter_map(|threat| threat.entity)
                .collect::<HashSet<_>>();
            let continuation = plan_registry.continuation(
                player_id,
                turn,
                operation_kind,
                &operations[op_index].objective_properties,
                &target_enemies,
            );
            // 別PlanのEntityを「既存戦力」として見積もっても任務は移管されない。
            // 継続する同一Planの配属Entityだけを固定パッケージへ入力する。
            let continuation_plan_id = continuation.as_ref().map(|plan| plan.plan_id);
            let committed_for_plan = committed_entities_for_plan(
                committed_by_operation.get(&op_index),
                continuation_plan_id,
            );
            let rolling_input = combat_plan_input(
                scan,
                &mut ctx,
                &operations[op_index],
                &used_facilities,
                if committed_for_plan.is_empty() {
                    &empty_committed_entities
                } else {
                    &committed_for_plan
                },
                remaining_funds,
                !allow_structural_slots,
            );
            if let Some(input) = rolling_input
                && let Some(candidate_plan) = plan_force_package(&input)
            {
                let evaluated_continuation = continuation.map(|previous| {
                    let evaluated = evaluate_fixed_package(&input, &previous.purchases);
                    (previous, evaluated)
                });
                let preempted_facilities = evaluated_continuation
                    .as_ref()
                    .map(|(previous, evaluated)| {
                        if evaluated.is_ok() {
                            return HashSet::new();
                        }
                        let mut due = previous
                            .purchases
                            .iter()
                            .filter(|purchase| purchase.build_turn == 0)
                            .copied()
                            .collect::<Vec<_>>();
                        due.sort_unstable_by_key(|purchase| {
                            (purchase.facility.y, purchase.facility.x, purchase.cost)
                        });
                        let mut affordable_funds = input.current_funds;
                        let mut preempted = HashSet::new();
                        for purchase in due {
                            let locally_preempted = facility_owners
                                .get(&purchase.facility)
                                .is_some_and(|owner| {
                                    owner.priority_rank() < operation_kind.priority_rank()
                                });
                            let campaign_preempted = scan
                                .production_facilities
                                .iter()
                                .any(|(facility, _)| *facility == purchase.facility)
                                && !scan
                                    .free_facilities
                                    .iter()
                                    .any(|(facility, _)| *facility == purchase.facility);
                            if locally_preempted || campaign_preempted {
                                preempted.insert(purchase.facility);
                            } else if operation_kind == OperationKind::AssaultCapital {
                                if purchase.cost <= affordable_funds {
                                    affordable_funds =
                                        affordable_funds.saturating_sub(purchase.cost);
                                } else {
                                    // 上位作戦が当手番の現金を使った場合も、失敗ではなく
                                    // 当該施設の首都編成列を次手番以降へ繰り下げる。
                                    preempted.insert(purchase.facility);
                                }
                            }
                        }
                        preempted
                    })
                    .unwrap_or_default();
                let selected = plan_registry.select(
                    player_id,
                    turn,
                    operation_kind,
                    operation_anchor,
                    operations[op_index].objective_properties.clone(),
                    target_enemies,
                    evaluated_continuation,
                    candidate_plan,
                    input.hard_deadline,
                    preempted_facilities,
                );
                if let Some(plan_id) = selected.plan_id {
                    seen_plan_ids.insert(plan_id);
                }
                let plan = &selected.plan;
                plan_trace.rolling_combat_plans.retain(|current| {
                    current.operation_kind != operation_kind || current.anchor != operation_anchor
                });
                plan_trace
                    .rolling_combat_plans
                    .push(RollingCombatPlanTrace {
                        plan_id: selected.plan_id,
                        revision: selected.revision,
                        disposition: selected.disposition,
                        replan_reason: selected.reason,
                        operation_kind,
                        anchor: operation_anchor,
                        feasible: plan.feasible,
                        purchases: plan
                            .purchases
                            .iter()
                            .copied()
                            .map(|purchase| RollingPurchaseTrace {
                                unit_type: purchase.unit_type,
                                facility: purchase.facility,
                                build_turn: purchase.build_turn,
                                cost: purchase.cost,
                            })
                            .collect(),
                        targets: plan
                            .target_forecasts
                            .iter()
                            .map(|target| RollingTargetTrace {
                                entity: target.entity,
                                unit_type: target.unit_type,
                                available_turn: target.available_turn,
                                initial_hp: target.initial_hp,
                                remaining_hp: target.remaining_hp,
                                destroyed_turn: target.destroyed_turn,
                            })
                            .collect(),
                        turn_forecasts: plan
                            .turn_forecasts
                            .iter()
                            .map(|forecast| CampaignTurnForecastTrace {
                                turn: forecast.turn,
                                enemy_arrival_hp: forecast.enemy_arrival_hp,
                                enemy_hp_removed: forecast.enemy_hp_removed,
                                friendly_hp_lost: forecast.friendly_hp_lost,
                                attack_count: forecast.attack_count,
                            })
                            .collect(),
                        first_attack_turn: plan.first_attack_turn,
                        elimination_turn: plan.elimination_turn,
                        occupation_turn: plan.occupation_turn,
                        production_cost: plan.production_cost,
                        expected_loss: plan.expected_loss,
                        candidates_considered: plan.candidates_considered,
                        search_truncated: plan.search_truncated,
                    });
                if selected.plan_id.is_none() {
                    // `NoFeasibleReplacement`は診断候補であって実行計画ではない。
                    // 先頭の購入だけをPlanIdなしで発注すると、次手番に継続も撤回も
                    // できず汎用任務へ流れるため、この作戦のCombat枠は実行しない。
                    clear_slot(&mut operations[op_index], SlotKind::Combat);
                    continue;
                }
                rolling_plans.insert(op_index, selected);
            }
        }
        let rolling_plan = rolling_plans.get(&op_index);
        let mut planned_purchase = None;
        let candidate = if slot_kind == SlotKind::Combat {
            rolling_plan.and_then(|plan| {
                plan.plan
                    .current_purchases()
                    .find(|purchase| {
                        !used_facilities.contains(&purchase.facility)
                            && purchase.cost <= remaining_funds
                    })
                    .map(|purchase| {
                        planned_purchase = Some(purchase);
                        SlotCandidate {
                            unit_type: purchase.unit_type,
                            cost: purchase.cost,
                            facility: purchase.facility,
                            fitness: 1.0,
                        }
                    })
            })
        } else {
            select_candidate(
                scan,
                &mut ctx,
                &operations[op_index],
                slot_kind,
                &used_facilities,
                CandidateConstraints {
                    remaining_funds,
                    per_slot_budget,
                    require_self_deployment: !allow_structural_slots,
                },
            )
        };

        let Some(candidate) = candidate else {
            // この枠を満たせる候補が無い場合は、枠の要求を落として次を探す
            clear_slot(&mut operations[op_index], slot_kind);
            let reserved = (slot_kind == SlotKind::Combat)
                .then_some(rolling_plan)
                .flatten()
                .filter(|plan| plan.plan_id.is_some())
                .and_then(|plan| {
                    plan.plan
                        .purchases
                        .iter()
                        .filter(|purchase| purchase.build_turn > 0)
                        .min_by_key(|purchase| purchase.build_turn)
                });
            plan_trace.steps.push(ProductionStepTrace {
                operation_kind,
                operation_anchor,
                slot_kind,
                deficit_before,
                deficit_after: deficit_before,
                remaining_funds_before,
                decision: reserved.map_or(ProductionDecision::SlotCleared, |purchase| {
                    ProductionDecision::Reserved {
                        unit_type: purchase.unit_type,
                        cost: purchase.cost,
                        build_turn: purchase.build_turn,
                    }
                }),
            });
            continue;
        };

        // 見送り購入: 一括編成が必要な作戦で、今買える範囲に適合候補が無く、
        // 数ターン待てばより適合する候補が買えるなら、資金を貯める。
        if slot_kind != SlotKind::Combat
            && should_defer_purchase(
                scan,
                &mut ctx,
                &operations[op_index],
                slot_kind,
                remaining_funds,
                candidate.cost,
                !allow_structural_slots,
            )
        {
            plan_trace.steps.push(ProductionStepTrace {
                operation_kind,
                operation_anchor,
                slot_kind,
                deficit_before,
                deficit_after: deficit_before,
                remaining_funds_before,
                decision: ProductionDecision::Deferred {
                    unit_type: candidate.unit_type,
                    cost: candidate.cost,
                },
            });
            break;
        }

        remaining_funds = remaining_funds.saturating_sub(candidate.cost);
        used_facilities.insert(candidate.facility);
        facility_owners.insert(candidate.facility, operation_kind);
        let mut deployment =
            planned_deployment(scan, &mut ctx, &operations[op_index], slot_kind, &candidate);
        if slot_kind == SlotKind::Combat
            && let (Some(deployment), Some(plan)) = (deployment.as_mut(), rolling_plan)
        {
            deployment.forecast = deployment::DeploymentForecast {
                first_attack_turn: plan.plan.first_attack_turn,
                elimination_turn: plan.plan.elimination_turn,
                occupation_turn: plan.plan.occupation_turn,
                package_cost: plan.plan.production_cost,
                package_size: u32::try_from(plan.plan.purchases.len()).unwrap_or(u32::MAX),
            };
            deployment.plan_step = plan
                .plan_id
                .zip(plan.revision)
                .zip(planned_purchase)
                .and_then(|((plan_id, revision), purchase)| {
                    plan_registry.current_step_ref(plan_id, revision, turn, purchase)
                });
        }
        if slot_kind == SlotKind::Combat {
            // 同じパッケージの未使用current purchaseは次の反復で選ばれる。
            // 全て消費した後は候補なしとなり、この手番のCombat枠を完了する。
            operations[op_index].filled.escort_units =
                operations[op_index].filled.escort_units.saturating_add(1);
        } else {
            record_fill(
                scan,
                &mut ctx,
                &mut operations[op_index],
                slot_kind,
                &candidate,
            );
        }
        plan_trace.steps.push(ProductionStepTrace {
            operation_kind,
            operation_anchor,
            slot_kind,
            deficit_before,
            // 購入を反映した後の未充足率。ここが下がらない枠が同一ユニットを買い続ける。
            deficit_after: operations[op_index]
                .slots
                .deficit_ratio(slot_kind, &operations[op_index].filled),
            remaining_funds_before,
            decision: ProductionDecision::Produced {
                unit_type: candidate.unit_type,
                cost: candidate.cost,
                facility: candidate.facility,
            },
        });
        commands.push(PlannedProduction {
            command: ProduceUnitCommand {
                player_id,
                target_x: candidate.facility.x,
                target_y: candidate.facility.y,
                unit_type: candidate.unit_type,
            },
            deployment,
        });
        if let Some(step_ref) = commands
            .last()
            .and_then(|planned| planned.deployment.as_ref())
            .and_then(|deployment| deployment.plan_step)
        {
            plan_registry.mark_issued(step_ref, turn);
        }
    }

    plan_registry.reconcile_unseen_plans(player_id, turn, &seen_plan_ids);
    plan_trace.leftover_funds = remaining_funds;
    plan_trace.reserved_funds =
        remaining_funds.min(plan_registry.reserved_purchase_cost(player_id));
    plan_trace.uncommitted_funds = remaining_funds.saturating_sub(plan_trace.reserved_funds);
    (commands, plan_trace)
}

/// 見積に含めてよいのは、同じ永続Planへ実際に配属済みのEntityだけ。
fn committed_entities_for_plan(
    assignments: Option<&HashMap<Entity, Option<plan_revision::PlanId>>>,
    continuation_plan_id: Option<plan_revision::PlanId>,
) -> HashSet<Entity> {
    assignments
        .into_iter()
        .flat_map(|assignments| assignments.iter())
        .filter_map(|(&entity, &plan_id)| {
            (plan_id == continuation_plan_id && plan_id.is_some()).then_some(entity)
        })
        .collect()
}

/// 敵が保持する生産施設と収入から、作戦地点へ期限内に到着できるcounter増援列を作る。
///
/// 現在数へ固定値を足すのではなく、各手番の資金、facility slot、生産可能兵種、移動ETAを
/// 同じ時間軸へ置く。敵の現在資金は非公開なので0から始め、将来収入だけを使う悲観scenario
/// とする。これは実際の敵命令の予言ではなく、成立しない甘い計画を弾くstress testである。
fn enemy_reinforcement_scenario(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    horizon: u32,
) -> Vec<EnemyPlanUnit> {
    let scenario_budget = op.facts.enemy_reinforcement_budget;
    if scenario_budget == 0 {
        return Vec::new();
    }
    let mut budget = 0_u32;
    let mut funded = 0_u32;
    let mut reinforcements = Vec::new();
    let friendly_combat_types = scan
        .available_types
        .iter()
        .filter(|(_, stats)| !stats.can_capture && stats.max_cargo == 0)
        .map(|(_, stats)| stats)
        .collect::<Vec<_>>();

    for build_turn in 1..horizon {
        let income = scan
            .enemy_income
            .min(scenario_budget.saturating_sub(funded));
        budget = budget.saturating_add(income);
        funded = funded.saturating_add(income);
        for facility in &scan.enemy_facilities {
            let mut candidates = scan
                .available_types
                .iter()
                .filter(|(unit_type, stats)| {
                    stats.max_cargo == 0
                        && stats.cost > 0
                        && stats.cost <= budget
                        && scan.can_produce(facility.terrain, *unit_type)
                        && ctx.is_reachable(
                            &scan.map,
                            &scan.master_data,
                            (facility.pos.x, facility.pos.y),
                            (op.anchor.x, op.anchor.y),
                            stats.movement_type,
                        )
                })
                .filter_map(|(_, stats)| {
                    let eta = eta_turns(&scan.map, &facility.pos, &op.anchor, stats.max_movement);
                    let available_turn = build_turn.saturating_add(1).saturating_add(eta);
                    if available_turn > horizon {
                        return None;
                    }
                    let counter_damage = friendly_combat_types
                        .iter()
                        .map(|friendly| {
                            best_damage(&scan.damage_chart, stats.unit_type, friendly.unit_type)
                        })
                        .max()
                        .unwrap_or_default();
                    let can_be_engaged = friendly_combat_types.iter().any(|friendly| {
                        best_damage(&scan.damage_chart, friendly.unit_type, stats.unit_type) > 0
                    });
                    can_be_engaged.then_some((
                        std::cmp::Reverse(counter_damage),
                        std::cmp::Reverse(u32::from(stats.can_capture)),
                        std::cmp::Reverse(stats.cost),
                        available_turn,
                        stats,
                    ))
                })
                .collect::<Vec<_>>();
            candidates.sort_by_key(|candidate| (candidate.0, candidate.1, candidate.2));
            let Some((_, _, _, available_turn, stats)) = candidates.into_iter().next() else {
                continue;
            };
            budget = budget.saturating_sub(stats.cost);
            reinforcements.push(EnemyPlanUnit {
                entity: None,
                stats: stats.clone(),
                // ETA到達後は作戦anchorに接触するものとして交戦時間を見積もる。
                position: op.anchor,
                hp: 100,
                defense_bonus: scan
                    .map
                    .get_terrain(op.anchor.x, op.anchor.y)
                    .map_or(0, |terrain| {
                        scan.master_data.get_terrain_defense_bonus(terrain)
                    }),
                available_turn,
            });
        }
    }
    reinforcements
}

/// 観測敵と到着しうる増援を排除できる混成生産列を、探索期間の全生産slotから計画する。
///
/// `destroy_budget`は呼び出し条件にだけ残し、候補数・生産停止・完了判定には使わない。
/// 既存unitと同じ手番に発注済みのunitを初期編成へ入れ、敵EntityのHPが0になるまで
/// ターン単位で攻撃を進めた結果から必要な購入だけを返す。
#[allow(clippy::too_many_arguments)]
fn combat_plan_input(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    used_facilities: &HashSet<GridPosition>,
    committed_combat_entities: &HashSet<Entity>,
    remaining_funds: u32,
    require_self_deployment: bool,
) -> Option<RollingPlanInput> {
    let enemies: Vec<_> = op
        .reachable_threats
        .iter()
        .filter(|threat| threat.current_hp > 0)
        .map(|threat| {
            let terrain = scan
                .map
                .get_terrain(threat.position.x, threat.position.y)
                .unwrap_or(Terrain::Plains);
            EnemyPlanUnit {
                entity: threat.entity,
                stats: threat.stats.clone(),
                position: threat.position,
                hp: threat.current_hp,
                defense_bonus: scan.master_data.get_terrain_defense_bonus(terrain),
                available_turn: threat.available_turn,
            }
        })
        .collect();
    let hard_deadline =
        if op.kind == OperationKind::Defense && op.facts.enemy_contact_eta != u32::MAX {
            Some(op.facts.enemy_contact_eta.max(1))
        } else {
            None
        };
    let capture_completion_turn = scan
        .campaign_objectives
        .iter()
        .filter(|objective| {
            objective.anchor == op.anchor || op.objective_properties.contains(&objective.anchor)
        })
        .filter_map(|objective| objective.capture_eta)
        .min();
    let planning_horizon = hard_deadline.unwrap_or(DEFAULT_SEARCH_TURNS).max(1);
    let mut enemies = enemies;
    enemies.extend(enemy_reinforcement_scenario(
        scan,
        ctx,
        op,
        planning_horizon,
    ));
    if enemies.is_empty() {
        return None;
    }

    let mut existing_units = Vec::new();
    for unit in &scan.my_units {
        if unit.stats.can_capture || unit.stats.max_cargo > 0 {
            continue;
        }
        if !unit
            .entity
            .is_some_and(|entity| committed_combat_entities.contains(&entity))
        {
            continue;
        }
        let engageable_enemy_indices = enemies
            .iter()
            .enumerate()
            .filter_map(|(index, enemy)| {
                (best_damage(
                    &scan.damage_chart,
                    unit.stats.unit_type,
                    enemy.stats.unit_type,
                ) > 0
                    && ctx.is_reachable(
                        &scan.map,
                        &scan.master_data,
                        (unit.pos.x, unit.pos.y),
                        (enemy.position.x, enemy.position.y),
                        unit.stats.movement_type,
                    ))
                .then_some(index)
            })
            .collect::<Vec<_>>();
        if engageable_enemy_indices.is_empty() {
            continue;
        }
        existing_units.push(FriendlyPlanUnit {
            stats: unit.stats.clone(),
            position: unit.pos,
            hp: unit.hp,
            available_turn: 0,
            engageable_enemy_indices,
        });
    }

    let mut options = production_options(
        &scan.free_facilities,
        &scan.production_facilities,
        &scan.available_types,
        &scan.master_data,
        planning_horizon,
        |facility, stats| {
            let self_deployable = ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (facility.x, facility.y),
                (op.anchor.x, op.anchor.y),
                stats.movement_type,
            );
            if require_self_deployment && !self_deployable {
                return false;
            }
            can_join_operation(
                scan,
                ctx,
                &op.anchor,
                op.facts.requires_transport,
                &facility,
                stats,
            ) && enemies.iter().any(|enemy| {
                best_damage(&scan.damage_chart, stats.unit_type, enemy.stats.unit_type) > 0
                    && ctx.is_reachable(
                        &scan.map,
                        &scan.master_data,
                        (facility.x, facility.y),
                        (enemy.position.x, enemy.position.y),
                        stats.movement_type,
                    )
            })
        },
    );
    // この手番に既に使った施設は将来手番には再利用できるが、build_turn=0では使えない。
    options.retain(|option| {
        option.purchase.build_turn > 0 || !used_facilities.contains(&option.purchase.facility)
    });
    for option in &mut options {
        option.engageable_enemy_indices = enemies
            .iter()
            .enumerate()
            .filter_map(|(index, enemy)| {
                (best_damage(
                    &scan.damage_chart,
                    option.stats.unit_type,
                    enemy.stats.unit_type,
                ) > 0
                    && ctx.is_reachable(
                        &scan.map,
                        &scan.master_data,
                        (option.purchase.facility.x, option.purchase.facility.y),
                        (enemy.position.x, enemy.position.y),
                        option.stats.movement_type,
                    ))
                .then_some(index)
            })
            .collect();
    }

    Some(RollingPlanInput {
        map: scan.map.clone(),
        damage_chart: scan.damage_chart.clone(),
        existing_units,
        enemies,
        production_options: options,
        current_funds: remaining_funds,
        income_per_turn: scan.my_income,
        hard_deadline,
        capture_completion_turn,
        delay_cost_per_turn: op.facts.target_property_count.max(1).saturating_mul(1_000),
    })
}

/// 候補が限界被覆を与える局地敵を、生産採点と同じ重要度×相性の順に保持する。
fn planned_deployment(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    slot_kind: SlotKind,
    candidate: &SlotCandidate,
) -> Option<PlannedDeployment> {
    let stats = candidate_stats(scan, candidate);
    let (threats, eligible): (&[ThreatTarget], Vec<usize>) = match slot_kind {
        SlotKind::Intercept => (
            &op.unreachable_threats,
            (0..op.unreachable_threats.len()).collect(),
        ),
        SlotKind::Combat => {
            let self_deployable = ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (candidate.facility.x, candidate.facility.y),
                (op.anchor.x, op.anchor.y),
                stats.movement_type,
            );
            let origin = if self_deployable {
                candidate.facility
            } else {
                op.anchor
            };
            (
                &op.reachable_threats,
                reachable_threat_indices(scan, ctx, &op.reachable_threats, origin, stats),
            )
        }
        SlotKind::Capture | SlotKind::Transport => return None,
    };
    let mut targets = eligible
        .into_iter()
        .filter_map(|index| {
            let threat = &threats[index];
            let entity = threat.entity?;
            let efficiency = coverage_efficiency(stats, &threat.stats, &scan.damage_chart);
            (threat.remaining_value > 0.0 && efficiency > 0.0).then_some((
                efficiency * threat.priority_weight,
                threat.priority_weight,
                threat.position.x,
                threat.position.y,
                entity.to_bits(),
                entity,
            ))
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| right.1.total_cmp(&left.1))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.4.cmp(&right.4))
    });
    let priority_enemies = targets.into_iter().map(|target| target.5).collect();
    Some(PlannedDeployment {
        anchor: op.anchor,
        staging_anchor: op.staging_anchor,
        posture: if op.kind == OperationKind::AssaultCapital {
            deployment::DeploymentPosture::Forming
        } else {
            deployment::DeploymentPosture::Execute
        },
        slot_kind,
        priority_enemies,
        threat_horizon: op.threat_horizon,
        forecast: deployment::DeploymentForecast::default(),
        plan_step: None,
    })
}

/// 次に埋めるべき枠を返す。
///
/// 2 段階で選ぶ。要求が有限の枠（前提条件）を全作戦ぶん先に満たし、
/// そのうえで残額を要求が青天井の撃破枠へ注ぎ込む。
/// 有限要求と青天井要求を同じ土俵で比べてはならない（`SlotTier` 参照）。
fn most_starved_slot(operations: &[Operation]) -> Option<(usize, SlotKind)> {
    most_starved_in_tier(operations, SlotTier::Prerequisite)
        .or_else(|| most_starved_in_tier(operations, SlotTier::Residual))
}

/// 指定した段階の中で最も飢えた枠を返す。
fn most_starved_in_tier(operations: &[Operation], tier: SlotTier) -> Option<(usize, SlotKind)> {
    let mut best: Option<(usize, SlotKind, (u32, usize, f32))> = None;
    for (index, op) in operations.iter().enumerate() {
        for (priority, kind) in SLOT_PRIORITY.iter().enumerate() {
            let deficit = op.slots.tier_deficit(*kind, &op.filled, tier);
            if deficit <= 0.0 {
                continue;
            }
            let key = match tier {
                // 前提条件は「どの作戦を先に成立させるか」で並べる。
                // 作戦の優先度 → 枠の固定優先順位（SLOT_PRIORITY は作戦遂行の
                // 前提から順に並んでいる）→ 未充足率。
                SlotTier::Prerequisite => (op.kind.priority_rank(), priority, -deficit),
                // 余剰は作戦の別なく、未充足率だけで配る。
                // ここで作戦優先度を先に見てはならない。撃破枠の要求は青天井なので、
                // 最優先の作戦（＝自陣の防衛）が全額を吸い、渡洋作戦には 1 円も
                // 回らなくなる＝自陣に引きこもる。撃破要求は既に前線ごとの分担比
                // (`frontline_share`) で割ってあるので、未充足率で選べば
                // 資金は自然と各前線の分担比どおりに配分される。
                SlotTier::Residual => (0, 0, -deficit),
            };
            if best.is_none_or(|(_, _, best_key)| key < best_key) {
                best = Some((index, *kind, key));
            }
        }
    }
    best.map(|(index, kind, _)| (index, kind))
}

/// 満たせないと判明した枠の要求を消す（無限ループ防止）。
fn clear_slot(op: &mut Operation, kind: SlotKind) {
    match kind {
        SlotKind::Intercept => op.slots.intercept_budget = 0,
        SlotKind::Transport => op.slots.transport_slots = 0,
        SlotKind::Capture => op.slots.capture_units = 0,
        SlotKind::Combat => {
            op.slots.escort_units = 0;
            op.slots.destroy_budget = 0;
        }
    }
}

/// 購入した 1 体分を充足量へ反映する。
fn record_fill(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &mut Operation,
    kind: SlotKind,
    candidate: &SlotCandidate,
) {
    let cargo = scan
        .available_types
        .iter()
        .find(|(unit_type, _)| *unit_type == candidate.unit_type)
        .map(|(_, stats)| stats.max_cargo)
        .unwrap_or(0);
    match kind {
        SlotKind::Intercept => {
            op.filled.intercept_budget = op.filled.intercept_budget.saturating_add(candidate.cost);
            let indices: Vec<usize> = (0..op.unreachable_threats.len()).collect();
            apply_coverage(
                candidate_stats(scan, candidate),
                candidate.cost as f32,
                &mut op.unreachable_threats,
                &indices,
                &scan.damage_chart,
            );
        }
        SlotKind::Transport => {
            op.filled.transport_slots = op.filled.transport_slots.saturating_add(cargo.max(1))
        }
        SlotKind::Capture => op.filled.capture_units += 1,
        SlotKind::Combat => {
            // 戦闘ユニットの購入は護衛枠（体数）を1つ満たす。一方、撃破枠を
            // 満たす量は価格ではなく、作戦期限までの期待交換価値である。
            op.filled.escort_units += 1;
            let stats = candidate_stats(scan, candidate);
            let self_deployable = ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (candidate.facility.x, candidate.facility.y),
                (op.anchor.x, op.anchor.y),
                stats.movement_type,
            );
            let origin = if self_deployable {
                candidate.facility
            } else {
                op.anchor
            };
            let indices = reachable_threat_indices(scan, ctx, &op.reachable_threats, origin, stats);
            let expected_return = apply_sortie_return(
                stats,
                op.facts.territory_control_window_turns,
                &mut op.reachable_threats,
                &indices,
                &scan.damage_chart,
            );
            op.filled.destroy_budget = op
                .filled
                .destroy_budget
                .saturating_add(return_budget(expected_return));
        }
    }
}

/// 候補の能力値を生産可能一覧から復元する。候補は同じ一覧から生成されるため必ず存在する。
fn candidate_stats<'a>(scan: &'a BoardScan, candidate: &SlotCandidate) -> &'a UnitStats {
    scan.available_types
        .iter()
        .find(|(unit_type, _)| *unit_type == candidate.unit_type)
        .map(|(_, stats)| stats)
        .expect("生産候補は生産可能ユニット一覧に存在する")
}

/// 指定地点から実際に交戦できる脅威の添字を返す。
fn reachable_threat_indices(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    threats: &[ThreatTarget],
    origin: GridPosition,
    stats: &UnitStats,
) -> Vec<usize> {
    threats
        .iter()
        .enumerate()
        .filter(|(_, threat)| {
            ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (origin.x, origin.y),
                (threat.position.x, threat.position.y),
                stats.movement_type,
            )
        })
        .map(|(index, _)| index)
        .collect()
}

fn threats_have_counter(
    unit: &UnitStats,
    threats: &[ThreatTarget],
    eligible_indices: &[usize],
    chart: &DamageChart,
) -> bool {
    eligible_indices
        .iter()
        .any(|index| coverage_efficiency(unit, &threats[*index].stats, chart) > 0.0)
}

/// 価値交換効率を、脅威被覆に使う 0.0..=1.0 の効率へ変換する。
fn coverage_efficiency(unit: &UnitStats, enemy: &UnitStats, chart: &DamageChart) -> f32 {
    if enemy.cost == 0 {
        return 0.0;
    }
    (pair_value(unit, enemy, chart).max(0.0) / enemy.cost as f32).clamp(0.0, 1.0)
}

/// 1体の対抗ユニットを、重要度×相性が高い未対処脅威から順に割り当てる。
/// 戻り値は戦略的重要度を掛けた被覆増分で、候補採点と実台帳更新が同じ関数を通る。
fn apply_coverage(
    unit: &UnitStats,
    capacity: f32,
    threats: &mut [ThreatTarget],
    eligible_indices: &[usize],
    chart: &DamageChart,
) -> f32 {
    let mut remaining_capacity = capacity.max(0.0);
    let mut weighted_coverage = 0.0;
    while remaining_capacity > 0.0 {
        let Some((index, efficiency)) = eligible_indices
            .iter()
            .map(|index| {
                let threat = &threats[*index];
                (*index, coverage_efficiency(unit, &threat.stats, chart))
            })
            .filter(|(index, efficiency)| {
                threats[*index].remaining_value > 0.0 && *efficiency > 0.0
            })
            .max_by(
                |(left_index, left_efficiency), (right_index, right_efficiency)| {
                    let left = *left_efficiency * threats[*left_index].priority_weight;
                    let right = *right_efficiency * threats[*right_index].priority_weight;
                    left.total_cmp(&right)
                        .then_with(|| right_index.cmp(left_index))
                },
            )
        else {
            break;
        };
        let effective_capacity = remaining_capacity * efficiency;
        let covered = threats[index].remaining_value.min(effective_capacity);
        if covered <= 0.0 {
            break;
        }
        threats[index].remaining_value -= covered;
        remaining_capacity -= covered / efficiency;
        weighted_coverage += covered * threats[index].priority_weight;
    }
    weighted_coverage
}

/// 残存脅威へ新たに与えられる被覆量。候補の比較は平均相性ではなくこの増分で行う。
fn marginal_coverage(
    unit: &UnitStats,
    capacity: f32,
    threats: &[ThreatTarget],
    eligible_indices: &[usize],
    chart: &DamageChart,
) -> f32 {
    let mut projected = threats.to_vec();
    apply_coverage(unit, capacity, &mut projected, eligible_indices, chart)
}

/// 作戦期限までに1体が実行できる攻撃回数を上限として、期待交換価値を脅威へ割り当てる。
///
/// unit価格を処理能力として使うと、高価な戦闘機1機が同じ手番に複数目標を撃破できる
/// 扱いになる。実際の制約である「1体1手番1攻撃」に合わせ、各sortieで期待交換価値が
/// 最大の未処理目標を1件だけ減らす。
fn apply_sortie_return(
    unit: &UnitStats,
    max_sorties: u32,
    threats: &mut [ThreatTarget],
    eligible_indices: &[usize],
    chart: &DamageChart,
) -> f32 {
    let mut weighted_return = 0.0;
    for _ in 0..max_sorties.max(1) {
        let Some((index, expected_return)) = eligible_indices
            .iter()
            .filter_map(|index| {
                let threat = &threats[*index];
                if threat.remaining_value <= 0.0 {
                    return None;
                }
                let expected_return = pair_value(unit, &threat.stats, chart)
                    .max(0.0)
                    .min(threat.remaining_value);
                (expected_return > 0.0).then_some((*index, expected_return))
            })
            .max_by(|(left_index, left_return), (right_index, right_return)| {
                let left = *left_return * threats[*left_index].priority_weight;
                let right = *right_return * threats[*right_index].priority_weight;
                left.total_cmp(&right)
                    .then_with(|| right_index.cmp(left_index))
            })
        else {
            break;
        };
        threats[index].remaining_value -= expected_return;
        weighted_return += expected_return * threats[index].priority_weight;
    }
    weighted_return
}

fn marginal_sortie_return(
    unit: &UnitStats,
    max_sorties: u32,
    threats: &[ThreatTarget],
    eligible_indices: &[usize],
    chart: &DamageChart,
) -> f32 {
    let mut projected = threats.to_vec();
    apply_sortie_return(unit, max_sorties, &mut projected, eligible_indices, chart)
}

/// 浮動小数の期待交換価値を、撃破要求と同じ資金価値単位へ切り上げる。
fn return_budget(expected_return: f32) -> u32 {
    expected_return.max(0.0).ceil().min(u32::MAX as f32) as u32
}

/// 敵の地上・占領・輸送unitへ攻撃機会を作るためのsortie適合度。
///
/// `remaining_value`は高価な既存unitの価値被覆で0になりうるが、1体が同じ手番に
/// 複数目標を攻撃できるわけではない。護衛体数枠を埋める間は元の敵costと相性から、
/// 追加の攻撃bodyが作戦期限までに生む価値を評価する。
fn territory_control_sortie_value(
    unit: &UnitStats,
    max_sorties: u32,
    threats: &[ThreatTarget],
    eligible_indices: &[usize],
    chart: &DamageChart,
) -> f32 {
    let mut returns: Vec<_> = eligible_indices
        .iter()
        .map(|index| &threats[*index])
        .filter(|threat| is_territory_control_threat(&threat.stats))
        .map(|threat| pair_value(unit, &threat.stats, chart).max(0.0) * threat.priority_weight)
        .filter(|value| *value > 0.0)
        .collect();
    returns.sort_by(|left, right| right.total_cmp(left));
    returns.into_iter().take(max_sorties.max(1) as usize).sum()
}

/// 枠への生の期待収益を、現在の資金と生産枠の制約に合わせて比較可能にする。
///
/// Combatは単純な最安優先ではない。1枠あたり予算を下回る候補は同じ機会費用で比較し、
/// 資金潤沢時は高価でも絶対戦果が大きい候補を選べる。予算が厳しい場合だけ実価格が
/// 分母になり、戦果/費用の良い候補が優位になる。
fn normalized_candidate_fitness(
    kind: SlotKind,
    raw_fitness: f32,
    cost: u32,
    per_slot_budget: u32,
) -> f32 {
    let count_denominated = matches!(kind, SlotKind::Capture | SlotKind::Transport);
    let opportunity_cost = if count_denominated {
        cost
    } else {
        cost.max(per_slot_budget)
    }
    .max(1);
    raw_fitness * 1000.0 / opportunity_cost as f32
}

/// 指定の枠を満たす最良の候補を選ぶ。
fn select_candidate(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    kind: SlotKind,
    used_facilities: &HashSet<GridPosition>,
    constraints: CandidateConstraints,
) -> Option<SlotCandidate> {
    let mut best: Option<SlotCandidate> = None;
    let mut best_over_budget: Option<SlotCandidate> = None;

    for (facility, terrain) in &scan.free_facilities {
        if used_facilities.contains(facility) {
            continue;
        }
        for (unit_type, stats) in &scan.available_types {
            if !scan.can_produce(*terrain, *unit_type) {
                continue;
            }
            let Some(fitness) = slot_fitness(
                scan,
                ctx,
                op,
                kind,
                facility,
                stats,
                constraints.require_self_deployment,
            ) else {
                continue;
            };
            if stats.cost == 0 || stats.cost > constraints.remaining_funds {
                continue;
            }
            // 枠の要求単位は種別ごとに違う（`OperationSlots::requirement` 参照）ので、
            // 1 購入あたりの機会費用も種別で変える。
            // - 占領枠／輸送枠は要求が「体数」「スロット数」。1 購入で満たせる要求は
            //   価格に関わらず 1 でしかないため、高い候補を買うほど同じ要求を満たす
            //   総額が膨らむ。ここは支払額そのものが機会費用になる。
            // - 撃破枠／迎撃枠は要求が「資金」。1 ターンに使える生産枠数は固定なので、
            //   投入戦力を増やす唯一の手段は 1 枠あたりの戦力を上げることであり、
            //   安く済ませても余剰はその枠では使えず割引にならない。よって分母は
            //   cost と per_slot_budget の大きい方を取り、枠あたり戦力で比較する。
            let count_denominated = matches!(kind, SlotKind::Capture | SlotKind::Transport);
            let candidate = SlotCandidate {
                unit_type: *unit_type,
                cost: stats.cost,
                facility: *facility,
                fitness: normalized_candidate_fitness(
                    kind,
                    fitness,
                    stats.cost,
                    constraints.per_slot_budget,
                ),
            };
            // 予算内／予算超過の階層分けも体数系の枠にだけ残す。資金系の枠でこれを
            // やると、どれほど弱くても予算内の候補が常に強い候補に勝ってしまい、
            // 資金が潤沢でも安いユニットしか買わなくなる（＝戦力の逐次投入）。
            let slot = if count_denominated && stats.cost > constraints.per_slot_budget.max(1) {
                &mut best_over_budget
            } else {
                &mut best
            };
            let better = slot.is_none_or(|current| {
                if count_denominated {
                    // 同性能なら安い方が多く揃う
                    (candidate.fitness, current.cost) > (current.fitness, candidate.cost)
                } else {
                    // 枠が制約なので、同性能なら大きい方を投入する
                    (candidate.fitness, candidate.cost) > (current.fitness, current.cost)
                }
            });
            if better {
                *slot = Some(candidate);
            }
        }
    }

    // 1 枠あたり予算に収まる候補を優先し、無ければ予算超過でも買える候補を使う
    best.or(best_over_budget)
}

/// 積荷を目標へ届けられるか。
///
/// 降車の可否はゲームのルール（`can_unload_from_terrain`）が決めており、
/// 艦船は港か浅瀬に接岸しないと積荷を降ろせない。海上に浮いたまま
/// 隣のマスへ降ろすことはできないので、「目標の隣まで行けるか」で
/// 判定しても渡洋作戦の成否とは対応しない。そこで
///   (1) 輸送自身が到達でき、かつ降車が許される揚陸地点があり、
///   (2) その隣接マスから積荷が自力で目標まで行ける
/// マスが存在するかどうかを見る。
///
/// 地形ルールは `can_unload_from_terrain` に、隣接の定義は
/// `map.get_adjacent` に委ねるため、特定のマップやトポロジーに依存しない。
fn can_deliver_cargo(
    map: &Map,
    registry: &MasterDataRegistry,
    ctx: &mut ReachCtx,
    from: &GridPosition,
    anchor: &GridPosition,
    transport_movement: MovementType,
    cargo_movement: MovementType,
) -> bool {
    let key = (
        transport_movement,
        cargo_movement,
        (from.x, from.y),
        (anchor.x, anchor.y),
    );
    if let Some(cached) = ctx.delivery.get(&key) {
        return *cached;
    }

    let mut result = false;
    'outer: for y in 0..map.height {
        for x in 0..map.width {
            let Some(terrain) = map.get_terrain(x, y) else {
                continue;
            };
            // 揚陸が許される地形か（艦船なら港・浅瀬のみ）
            if !can_unload_from_terrain(Some(transport_movement), Some(terrain)) {
                continue;
            }
            // 輸送自身がその揚陸地点まで行けるか
            if !ctx.is_reachable(map, registry, (from.x, from.y), (x, y), transport_movement) {
                continue;
            }
            // 降ろした先から積荷が目標へ行けるか。
            // `is_reachable` は積荷が進入できない地形を非連結として弾くので、
            // 降車先そのものの通行可否もここで同時に判定される。
            for (ax, ay) in map.get_adjacent(x, y) {
                if ctx.is_reachable(
                    map,
                    registry,
                    (ax, ay),
                    (anchor.x, anchor.y),
                    cargo_movement,
                ) {
                    result = true;
                    break 'outer;
                }
            }
        }
    }

    ctx.delivery.insert(key, result);
    result
}

/// `from` にいる（あるいはそこで生産される）ユニットが、この作戦へ投入できるか。
///
/// 成立するのは次のいずれか。
/// (1) 自力で作戦地点まで到達できる
/// (2) それを積める輸送ユニットを空き施設で生産でき、その輸送が積荷を目標へ揚陸できる
///
/// この関数は **購入候補の採用判定（`slot_fitness`）と既存戦力の計上（台帳）の
/// 両方から呼ばれなければならない**。片側だけ条件を変えると「その枠を埋められるのに
/// 充足としては数えられない」ユニットが生まれ、要求が永久に減らずに同じユニットを
/// 買い続けるラチェットになる。
/// 判定はユニット名ではなく能力（`max_cargo` / `loadable_unit_types`）で行う。
fn can_join_operation(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    anchor: &GridPosition,
    requires_transport: bool,
    from: &GridPosition,
    stats: &UnitStats,
) -> bool {
    // 自力で作戦地点まで到達できるか
    if ctx.is_reachable(
        &scan.map,
        &scan.master_data,
        (from.x, from.y),
        (anchor.x, anchor.y),
        stats.movement_type,
    ) {
        return true;
    }
    if !requires_transport {
        return false;
    }
    // 空き施設で生産できる輸送ユニットの候補を先に洗い出す（借用を分離するため）
    let carriers: Vec<(GridPosition, MovementType)> = scan
        .free_facilities
        .iter()
        .flat_map(|(facility, terrain)| {
            scan.available_types
                .iter()
                .filter(|(unit_type, carrier)| {
                    carrier.max_cargo > 0
                        && carrier.loadable_unit_types.contains(&stats.unit_type)
                        && scan.can_produce(*terrain, *unit_type)
                })
                .map(move |(_, carrier)| (*facility, carrier.movement_type))
        })
        .collect();

    carriers.into_iter().any(|(facility, movement_type)| {
        can_deliver_cargo(
            &scan.map,
            &scan.master_data,
            ctx,
            &facility,
            anchor,
            movement_type,
            stats.movement_type,
        )
    })
}

/// ユニットが指定枠にどれだけ適合するかを返す。適合しない場合は `None`。
fn slot_fitness(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    kind: SlotKind,
    facility: &GridPosition,
    stats: &UnitStats,
    require_self_deployment: bool,
) -> Option<f32> {
    // 施設から作戦地点まで自力で到達できるか
    let self_deployable = ctx.is_reachable(
        &scan.map,
        &scan.master_data,
        (facility.x, facility.y),
        (op.anchor.x, op.anchor.y),
        stats.movement_type,
    );

    match kind {
        SlotKind::Capture => {
            if !stats.can_capture {
                return None;
            }
            // 自力で行けないなら、実際に運べる輸送手段が存在することが前提。
            // 「輸送枠が立っている」だけでは運搬が成立する保証にならない。
            // 台帳（`build_operation` の自軍仕分け）と同じ関数を通すこと。
            if !can_join_operation(
                scan,
                ctx,
                &op.anchor,
                op.facts.requires_transport,
                facility,
                stats,
            ) {
                return None;
            }
            Some(1.0)
        }
        SlotKind::Transport => {
            if stats.max_cargo == 0 {
                return None;
            }
            // 「占領ユニットを積める」だけでは足りない。
            // その積荷を実際に目標へ揚陸できるところまで確かめる。
            // ここを緩めると運用の当てがない輸送を買い続けることになり、
            // 逆に目標マス自体への到達を求めると艦船が永久に候補から外れる。
            let deliverable = scan
                .available_types
                .iter()
                .filter(|(unit_type, cargo)| {
                    cargo.can_capture && stats.loadable_unit_types.contains(unit_type)
                })
                .map(|(_, cargo)| cargo.movement_type)
                .collect::<Vec<_>>()
                .into_iter()
                .any(|cargo_movement| {
                    can_deliver_cargo(
                        &scan.map,
                        &scan.master_data,
                        ctx,
                        facility,
                        &op.anchor,
                        stats.movement_type,
                        cargo_movement,
                    )
                });
            if !deliverable {
                return None;
            }
            Some(stats.max_cargo as f32)
        }
        SlotKind::Intercept => {
            if op.unreachable_threats.is_empty() {
                return None;
            }
            // 迎撃には「その脅威に届く」ことと「自力で現地へ行ける」ことの両方が要る。
            // 対空戦車が海を渡れないために選ばれないのは、この 2 条件の帰結。
            if !self_deployable {
                return None;
            }
            let indices: Vec<usize> = (0..op.unreachable_threats.len()).collect();
            let value = marginal_coverage(
                stats,
                stats.cost as f32,
                &op.unreachable_threats,
                &indices,
                &scan.damage_chart,
            );
            if value <= 0.0 { None } else { Some(value) }
        }
        SlotKind::Combat => {
            if stats.can_capture || stats.max_cargo > 0 {
                return None;
            }
            // キャンペーン予約を超えた余剰購入では、別便の輸送を暗黙に期待しない。
            // これにより海外前線へ渡れない戦車を「いつか運べる」として買わない。
            if require_self_deployment && !self_deployable {
                return None;
            }
            // 自力で行けないなら、実際に運べる輸送手段が存在することが前提。
            // 台帳（`build_operation` の自軍仕分け）と同じ関数を通すこと。
            if !can_join_operation(
                scan,
                ctx,
                &op.anchor,
                op.facts.requires_transport,
                facility,
                stats,
            ) {
                return None;
            }
            // 増援予測は撃破予算の上限には使えるが、未観測の敵兵種までは決められない。
            // ここで機動力などの汎用点へ退避すると、敵が0体の作戦が同じ兵種を全施設へ
            // 発注し続ける。具体的な残存脅威が無ければ限界価値も0として購入を止める。
            if op.reachable_threats.is_empty() {
                return None;
            }
            // 採点対象は「このユニットが実際に殴りに行ける敵」だけに限る。
            // 届かない敵まで含めた平均で採点すると、相性表の上でだけ強い
            // ユニット（海を渡れない対空戦車など）が延々と選ばれ、
            // 生産拠点に張り付いたまま敵の占領部隊を素通しにしてしまう。
            //
            // ただし揚陸される部隊は「上陸地点から殴りに行けるか」で採点する。
            // 施設からの到達性で採点すると、船で運べば戦える陸戦部隊が
            // すべて不適合となり、渡洋作戦の戦力が航空ユニットだけになる。
            let origin = if self_deployable {
                *facility
            } else {
                op.anchor
            };
            let engageable =
                reachable_threat_indices(scan, ctx, &op.reachable_threats, origin, stats);
            if engageable.is_empty() {
                return None;
            }
            let value = marginal_sortie_return(
                stats,
                op.facts.territory_control_window_turns,
                &op.reachable_threats,
                &engageable,
                &scan.damage_chart,
            );
            if value > 0.0 {
                Some(value)
            } else if op.filled.escort_units < op.slots.escort_units {
                let sortie_value = territory_control_sortie_value(
                    stats,
                    op.facts.territory_control_window_turns,
                    &op.reachable_threats,
                    &engageable,
                    &scan.damage_chart,
                );
                (sortie_value > 0.0).then_some(sortie_value)
            } else {
                None
            }
        }
    }
}

/// 敵 1 体を撃破枠の見積もりへ算入するときの重み付き価値。
///
/// 占領レースが進行中（未取得の拠点が残っている）の間は、敵の「拡張装置」＝
/// 自力で拠点を取れるユニットと、それを運べる輸送ユニットを素のコスト価値より重く数える。
/// これらは撃破しなければ盤面の収入を動かし続けるため、コストどおりに数えると
/// 撃破枠が立たず、局地戦の交換比で勝ちながら領地を明け渡すことになる。
/// 判定はユニット名ではなく能力（`can_capture` / `max_cargo`）で行うため、
/// マップやユニット構成に依存しない。
fn threat_value(unit: &UnitSnapshot, expansion_race_live: bool) -> u32 {
    if expansion_race_live && (unit.stats.can_capture || unit.stats.max_cargo > 0) {
        unit.value().saturating_mul(EXPANSION_THREAT_WEIGHT)
    } else {
        unit.value()
    }
}

/// 敵 1 体に対する対抗効率（与える価値 − 受ける価値）。
fn pair_value(unit: &UnitStats, enemy: &UnitStats, chart: &DamageChart) -> f32 {
    let dmg_out = best_damage(chart, unit.unit_type, enemy.unit_type);
    let dmg_in = best_damage(chart, enemy.unit_type, unit.unit_type);
    let out = dmg_out as f32 * enemy.cost as f32 / 100.0 * engagement_factor(unit, enemy);
    let inc = dmg_in as f32 * unit.cost as f32 / 100.0 * engagement_factor(enemy, unit);
    out - inc
}

/// 敵編成に対する対抗効率の平均。
fn counter_value(unit: &UnitStats, enemies: &[UnitStats], chart: &DamageChart) -> f32 {
    if enemies.is_empty() {
        return 0.0;
    }
    let total: f32 = enemies
        .iter()
        .map(|enemy| pair_value(unit, enemy, chart))
        .sum();
    total / enemies.len() as f32
}

/// `counter_value` の参照スライス版。到達可能な敵だけを抜き出して採点する用途で使う。
#[cfg(test)]
fn counter_value_refs(unit: &UnitStats, enemies: &[&UnitStats], chart: &DamageChart) -> f32 {
    if enemies.is_empty() {
        return 0.0;
    }
    let total: f32 = enemies
        .iter()
        .map(|enemy| pair_value(unit, enemy, chart))
        .sum();
    total / enemies.len() as f32
}

/// 主武器・副武器のうち有効な方のダメージ。
fn best_damage(chart: &DamageChart, attacker: UnitType, defender: UnitType) -> u32 {
    chart.get_base_damage(attacker, defender).unwrap_or(0).max(
        chart
            .get_base_damage_secondary(attacker, defender)
            .unwrap_or(0),
    )
}

/// 射程と機動力の関係から、実際に交戦できる度合いを補正する係数。
fn engagement_factor(attacker: &UnitStats, defender: &UnitStats) -> f32 {
    let att_reach = attacker.max_movement + attacker.max_range;
    let def_reach = defender.max_movement + defender.max_range;
    if attacker.max_range > defender.max_range {
        if att_reach >= def_reach { 1.0 } else { 0.8 }
    } else if attacker.max_range < defender.max_range {
        0.5
    } else {
        1.0
    }
}

/// 見送り購入（資金を貯めて上位の候補を買う）を行うべきか。
///
/// 一括編成が必要な作戦に限り、いま買える候補より明確に適合度の高い候補が
/// `RESERVATION_PATIENCE_TURNS` 以内の収入で買えるなら、今ターンは生産しない。
fn should_defer_purchase(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    kind: SlotKind,
    remaining_funds: u32,
    affordable_cost: u32,
    require_self_deployment: bool,
) -> bool {
    if acquisition_mode(&op.facts) != AcquisitionMode::SquadPackage {
        return false;
    }
    if scan.my_income == 0 {
        return false;
    }
    // 輸送が要る作戦で輸送枠がまだ空いているのに他の枠を先に埋めるのは避ける
    let mut best_future: Option<(f32, u32)> = None;
    for (facility, terrain) in &scan.free_facilities {
        for (unit_type, stats) in &scan.available_types {
            if !scan.can_produce(*terrain, *unit_type) || stats.cost <= remaining_funds {
                continue;
            }
            let Some(fitness) = slot_fitness(
                scan,
                ctx,
                op,
                kind,
                facility,
                stats,
                require_self_deployment,
            ) else {
                continue;
            };
            let scaled = fitness * 1000.0 / stats.cost as f32;
            if best_future.is_none_or(|(current, _)| scaled > current) {
                best_future = Some((scaled, stats.cost));
            }
        }
    }
    let Some((future_fitness, future_cost)) = best_future else {
        return false;
    };
    // 現在買える候補の適合度
    let current_fitness = scan
        .available_types
        .iter()
        .filter(|(_, stats)| stats.cost <= remaining_funds && stats.cost == affordable_cost)
        .filter_map(|(_, stats)| {
            scan.free_facilities.iter().find_map(|(facility, terrain)| {
                if !scan.can_produce(*terrain, stats.unit_type) {
                    return None;
                }
                slot_fitness(
                    scan,
                    ctx,
                    op,
                    kind,
                    facility,
                    stats,
                    require_self_deployment,
                )
                .map(|f| f * 1000.0 / stats.cost as f32)
            })
        })
        .fold(0.0f32, f32::max);

    if future_fitness <= current_fitness {
        return false;
    }
    let shortfall = future_cost.saturating_sub(remaining_funds);
    let turns_to_afford = shortfall.div_ceil(scan.my_income.max(1));
    turns_to_afford <= RESERVATION_PATIENCE_TURNS
}

/// 作戦が 1 つも立たない平時のフォールバック。
///
/// `GamePhase` ごとの理想構成は使わず、敵編成に対する対抗効率のみで選ぶ。
fn fallback_production(scan: &BoardScan, player_id: PlayerId) -> Vec<ProduceUnitCommand> {
    let cheapest = scan
        .available_types
        .iter()
        .map(|(_, stats)| stats.cost)
        .filter(|cost| *cost > 0)
        .min()
        .unwrap_or(u32::MAX);
    // 資金に余裕がないうちは温存する
    if scan.funds < cheapest.saturating_mul(2) {
        return Vec::new();
    }

    let enemies: Vec<UnitStats> = scan
        .enemy_units
        .iter()
        .map(|unit| unit.stats.clone())
        .collect();

    let mut best: Option<(f32, GridPosition, UnitType)> = None;
    for (facility, terrain) in &scan.free_facilities {
        for (unit_type, stats) in &scan.available_types {
            if !scan.can_produce(*terrain, *unit_type) || stats.cost > scan.funds || stats.cost == 0
            {
                continue;
            }
            let base = if enemies.is_empty() {
                1.0
            } else {
                counter_value(stats, &enemies, &scan.damage_chart)
            };
            if base <= 0.0 {
                continue;
            }
            let score = base * 1000.0 / stats.cost as f32;
            if best.is_none_or(|(current, _, _)| score > current) {
                best = Some((score, *facility, *unit_type));
            }
        }
    }

    best.map(|(_, facility, unit_type)| ProduceUnitCommand {
        player_id,
        target_x: facility.x,
        target_y: facility.y,
        unit_type,
    })
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::GridTopology;

    fn flat_map(width: usize, height: usize) -> Map {
        Map {
            width,
            height,
            tiles: vec![Terrain::Plains; width * height],
            topology: GridTopology::Square,
        }
    }

    fn pos(x: usize, y: usize) -> GridPosition {
        GridPosition { x, y }
    }

    fn capture_objective(anchor: GridPosition) -> Vec<CampaignPlanningObjective> {
        vec![CampaignPlanningObjective {
            island_id: crate::ai::islands::IslandId(0),
            kind: OperationKind::Capture,
            anchor,
            capture_eta: None,
            logistics_rank: Some(0),
            forced_target_enemies: HashSet::new(),
            staging_anchor: anchor,
            execution_authorized: true,
        }]
    }

    #[test]
    fn campaign_objective_is_the_only_capture_source_and_keeps_exact_anchor() {
        let mut scan = multi_factory_scan();
        let campaign_anchor = pos(8, 2);
        scan.campaign_objectives = vec![CampaignPlanningObjective {
            island_id: crate::ai::islands::IslandId(0),
            kind: OperationKind::Capture,
            anchor: campaign_anchor,
            capture_eta: Some(3),
            logistics_rank: Some(0),
            forced_target_enemies: HashSet::new(),
            staging_anchor: campaign_anchor,
            execution_authorized: true,
        }];

        let operations = build_operations(&scan, &mut ReachCtx::default(), &[]);
        let captures = operations
            .iter()
            .filter(|operation| operation.kind == OperationKind::Capture)
            .collect::<Vec<_>>();

        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].anchor, campaign_anchor);
    }

    /// 到達ターン数は距離を移動力で割り上げた値になる
    #[test]
    fn eta_is_distance_divided_by_movement() {
        let map = flat_map(20, 20);
        assert_eq!(eta_turns(&map, &pos(0, 0), &pos(6, 0), 3), 2);
        assert_eq!(eta_turns(&map, &pos(0, 0), &pos(7, 0), 3), 3);
        // 移動力 0 でも 0 除算しない
        assert_eq!(eta_turns(&map, &pos(0, 0), &pos(2, 0), 0), 2);
    }

    /// 期限までに来られない敵は、単なる最寄り作戦へ押し込まない。
    #[test]
    fn enemy_is_assigned_only_when_it_can_arrive_before_an_objective_deadline() {
        let scan = multi_factory_scan();
        let mut ctx = ReachCtx::default();
        let anchors = vec![pos(1, 2), pos(8, 2)];
        let enemy = pos(4, 2);

        // 近い側でも1ターンかかるため、今ターン完了の作戦には無関係。
        assert_eq!(
            nearest_relevant_anchor_index(
                &scan,
                &mut ctx,
                &enemy,
                MovementType::Infantry,
                3,
                &anchors,
                &[0, 1],
            ),
            None
        );
        // 期限内なら最短の1作戦だけへ決定的に帰属する。
        assert_eq!(
            nearest_relevant_anchor_index(
                &scan,
                &mut ctx,
                &enemy,
                MovementType::Infantry,
                3,
                &anchors,
                &[1, 1],
            ),
            Some(0)
        );
    }

    /// 既存Combat 1体を複数前線の撃破要求から同時に控除してはならない。
    #[test]
    fn existing_combat_sorties_are_committed_to_only_one_operation() {
        let infantry = UnitStats {
            can_capture: true,
            movement_type: MovementType::Infantry,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1_000)
        };
        let helicopter = UnitStats {
            movement_type: MovementType::Air,
            max_movement: 8,
            ..stats(UnitType::Bcopters, 7_500)
        };
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 65);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Bcopters, 0);
        let anchors = vec![pos(5, 1), pos(15, 1)];
        let horizons = vec![5, 5];
        let scan = BoardScan {
            map: flat_map(20, 3),
            master_data: MasterDataRegistry::load().unwrap(),
            damage_chart,
            funds: 20_000,
            free_facilities: vec![(pos(0, 1), Terrain::Factory), (pos(1, 1), Terrain::Airport)],
            production_facilities: vec![
                (pos(0, 1), Terrain::Factory),
                (pos(1, 1), Terrain::Airport),
            ],
            available_types: vec![
                (UnitType::Infantry, infantry.clone()),
                (UnitType::Bcopters, helicopter.clone()),
            ],
            my_units: vec![UnitSnapshot {
                entity: Some(Entity::from_raw(900)),
                pos: pos(3, 1),
                stats: helicopter,
                hp: 100,
                free_cargo: 0,
            }],
            enemy_units: anchors
                .iter()
                .enumerate()
                .map(|(index, position)| UnitSnapshot {
                    entity: Some(Entity::from_raw(910 + index as u32)),
                    pos: *position,
                    stats: infantry.clone(),
                    hp: 100,
                    free_cargo: 0,
                })
                .collect(),
            owned_airport_count: 1,
            open_properties: anchors.clone(),
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 5_000,
            campaign_objectives: Vec::new(),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
        };
        let mut ctx = ReachCtx::default();
        let near = build_operation(
            &scan,
            &mut ctx,
            &infantry,
            OperationKind::Capture,
            anchors[0],
            &anchors,
            &horizons,
            &[anchors[0]],
            &HashSet::new(),
            2,
        );
        let far = build_operation(
            &scan,
            &mut ctx,
            &infantry,
            OperationKind::Capture,
            anchors[1],
            &anchors,
            &horizons,
            &[anchors[1]],
            &HashSet::new(),
            5,
        );

        assert!(near.facts.friendly_combat_value_committed > 0);
        assert_eq!(far.facts.friendly_combat_value_committed, 0);
    }

    #[test]
    fn existing_combat_entity_is_credited_only_to_its_own_persistent_plan() {
        let own_entity = Entity::from_raw(900);
        let foreign_entity = Entity::from_raw(901);
        let legacy_entity = Entity::from_raw(902);
        let own_plan = plan_revision::PlanId(7);
        let assignments = HashMap::from([
            (own_entity, Some(own_plan)),
            (foreign_entity, Some(plan_revision::PlanId(8))),
            (legacy_entity, None),
        ]);

        assert!(
            committed_entities_for_plan(Some(&assignments), None).is_empty(),
            "新規Planは別任務中の既存戦力を見積へ借りない"
        );
        assert_eq!(
            committed_entities_for_plan(Some(&assignments), Some(own_plan)),
            HashSet::from([own_entity])
        );
    }

    /// 敵施設の生産余力も、期限内に到着できる最寄りの1作戦だけへ計上する。
    #[test]
    fn enemy_reinforcement_budget_is_local_unique_and_deadline_bounded() {
        let mut scan = multi_factory_scan();
        scan.enemy_income = 6000;
        scan.enemy_production_slots = 1;
        scan.enemy_facilities = vec![EnemyFacilitySnapshot {
            pos: pos(8, 2),
            terrain: Terrain::Factory,
        }];
        let anchors = vec![pos(6, 2), pos(0, 2)];
        let horizons = vec![4, 4];
        let mut ctx = ReachCtx::default();

        // 最安の歩兵は1ターンで到着し、残る3生産ターン分だけを局地予算にする。
        assert_eq!(
            projected_enemy_reinforcement_budget(&scan, &mut ctx, &anchors, &horizons, 0),
            3000
        );
        assert_eq!(
            projected_enemy_reinforcement_budget(&scan, &mut ctx, &anchors, &horizons, 1),
            0
        );

        // 到着期限が0なら、この施設はどの作戦の脅威にもならない。
        assert_eq!(
            projected_enemy_reinforcement_budget(&scan, &mut ctx, &anchors, &[0, 0], 0),
            0
        );
    }

    #[test]
    fn enemy_reinforcement_scenario_places_each_purchase_on_the_time_axis() {
        let mut scan = multi_factory_scan();
        scan.enemy_income = 1_000;
        scan.enemy_production_slots = 1;
        scan.enemy_facilities = vec![EnemyFacilitySnapshot {
            pos: pos(8, 2),
            terrain: Terrain::Factory,
        }];
        scan.damage_chart
            .insert_damage(UnitType::Infantry, UnitType::Tank, 20);
        scan.damage_chart
            .insert_damage(UnitType::Tank, UnitType::Infantry, 80);
        let mut op = operation(
            OperationKind::Capture,
            OperationSlots::default(),
            OperationSlots::default(),
        );
        op.anchor = pos(6, 2);
        op.facts.enemy_reinforcement_budget = 3_000;

        let reinforcements = enemy_reinforcement_scenario(&scan, &mut ReachCtx::default(), &op, 5);

        assert_eq!(reinforcements.len(), 3);
        assert_eq!(
            reinforcements
                .iter()
                .map(|enemy| enemy.available_turn)
                .collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        assert!(
            reinforcements
                .iter()
                .all(|enemy| enemy.stats.unit_type == UnitType::Infantry)
        );
    }

    /// テスト用のユニット諸元。射程はすべて 0 なので `engagement_factor` は 1.0 になる
    fn stats(unit_type: UnitType, cost: u32) -> UnitStats {
        UnitStats {
            unit_type,
            cost,
            ..UnitStats::mock()
        }
    }

    fn snapshot(stats: UnitStats, hp: u32) -> UnitSnapshot {
        UnitSnapshot {
            entity: None,
            pos: pos(0, 0),
            stats,
            hp,
            free_cargo: 0,
        }
    }

    /// 占領レース中は、敵の占領ユニットと輸送ユニットを素のコスト価値より重く数える
    #[test]
    fn expansion_units_are_weighted_while_the_capture_race_is_live() {
        let capturer = snapshot(
            UnitStats {
                can_capture: true,
                ..stats(UnitType::Infantry, 1000)
            },
            100,
        );
        let transport = snapshot(
            UnitStats {
                max_cargo: 1,
                ..stats(UnitType::TransportHelicopter, 5000)
            },
            100,
        );

        assert_eq!(
            threat_value(&capturer, true),
            1000 * EXPANSION_THREAT_WEIGHT
        );
        assert_eq!(
            threat_value(&transport, true),
            5000 * EXPANSION_THREAT_WEIGHT
        );

        // 取れる拠点が尽きて占領レースが終われば、重み付けもなくなる
        assert_eq!(threat_value(&capturer, false), 1000);
        assert_eq!(threat_value(&transport, false), 5000);
    }

    /// 揚陸判定用のマップを組み立てる。
    ///
    /// 横一列のレーンを、左の陸地／中央の海／右の陸地に区切る。
    /// `landing` に地形を与えると右岸の入口 (x=6, y=1) をその地形に差し替えられる。
    /// 例: `Terrain::Shoal` を与えれば艦船が接岸できる揚陸地点になり、
    /// `None` のままなら海から陸へ乗り上げる手段が無いマップになる。
    ///
    /// レイアウト（各行 x=0..8 共通）:
    ///   Plains Port | Sea Sea Sea Sea | (landing) Plains Plains
    fn strait_map(landing: Option<Terrain>) -> Map {
        let width = 9;
        let height = 3;
        let mut tiles = vec![Terrain::Sea; width * height];
        for y in 0..height {
            for x in 0..width {
                let terrain = match x {
                    0 => Terrain::Plains,
                    1 => Terrain::Port,
                    6 => landing.unwrap_or(Terrain::Sea),
                    7..=8 => Terrain::Plains,
                    _ => Terrain::Sea,
                };
                tiles[y * width + x] = terrain;
            }
        }
        Map {
            width,
            height,
            tiles,
            topology: GridTopology::Square,
        }
    }

    /// 揚陸地点（港・浅瀬）が対岸にあれば、艦船は陸上ユニットを目標へ届けられる
    #[test]
    fn ship_can_deliver_land_cargo_through_a_beachhead() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(Some(Terrain::Shoal));
        let mut ctx = ReachCtx::default();

        assert!(can_deliver_cargo(
            &map,
            &registry,
            &mut ctx,
            &pos(1, 1),
            &pos(8, 1),
            MovementType::Ship,
            MovementType::Infantry,
        ));
    }

    /// 対岸に接岸できる地形が無ければ、隣接マスが陸地でも積荷は降ろせない
    ///
    /// 「目標の隣まで行けるか」で判定すると、海に浮いたままの艦船が
    /// 陸へ積荷を降ろせることになってしまうため、この区別が必要。
    #[test]
    fn ship_cannot_deliver_land_cargo_without_a_beachhead() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(None);
        let mut ctx = ReachCtx::default();

        // 対岸の陸地 (7,1) は海 (6,1) と隣接しているが、
        // 艦船は海の上では降車できないので不成立。
        assert!(!can_deliver_cargo(
            &map,
            &registry,
            &mut ctx,
            &pos(1, 1),
            &pos(8, 1),
            MovementType::Ship,
            MovementType::Infantry,
        ));
    }

    /// 自陣側の港からでも、自陣の陸地が目標なら当然届けられる（退行検出用）
    #[test]
    fn ship_can_deliver_cargo_back_to_its_own_shore() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(None);
        let mut ctx = ReachCtx::default();

        assert!(can_deliver_cargo(
            &map,
            &registry,
            &mut ctx,
            &pos(1, 1),
            &pos(0, 1),
            MovementType::Ship,
            MovementType::Infantry,
        ));
    }

    /// 艦船以外の輸送（空輸など）は地形に縛られず、どこへでも降ろせる
    #[test]
    fn air_transport_is_not_restricted_by_landing_terrain() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(None);
        let mut ctx = ReachCtx::default();

        assert!(can_deliver_cargo(
            &map,
            &registry,
            &mut ctx,
            &pos(1, 1),
            &pos(8, 1),
            MovementType::Air,
            MovementType::Infantry,
        ));
    }

    /// 占領も輸送もできない戦闘ユニットは、レース中でも重み付けされない
    #[test]
    fn plain_combat_units_are_never_weighted() {
        let tank = snapshot(stats(UnitType::Tank, 7000), 100);
        assert_eq!(threat_value(&tank, true), 7000);
        assert_eq!(threat_value(&tank, false), 7000);
    }

    /// 脅威価値は HP を加味した残存戦力で数える
    #[test]
    fn threat_value_scales_with_remaining_hp() {
        let half_dead = snapshot(
            UnitStats {
                can_capture: true,
                ..stats(UnitType::Infantry, 1000)
            },
            50,
        );
        assert_eq!(half_dead.value(), 500);
        assert_eq!(
            threat_value(&half_dead, true),
            500 * EXPANSION_THREAT_WEIGHT
        );
    }

    /// 撃破枠の採点は「そのユニットが実際に殴りに行ける敵」だけで行わないと、
    /// 届かない敵まで含めた平均のせいで、盤面に触れられないユニットが候補として残る。
    #[test]
    fn counter_value_ignores_enemies_the_unit_cannot_reach() {
        let mut chart = DamageChart::new();
        // 対空ユニットは航空ユニットに滅法強く、歩兵にはほとんど通らない
        chart.insert_damage(UnitType::AntiAir, UnitType::Bcopters, 120);
        chart.insert_damage(UnitType::Bcopters, UnitType::AntiAir, 10);
        chart.insert_damage(UnitType::AntiAir, UnitType::Infantry, 0);
        chart.insert_damage(UnitType::Infantry, UnitType::AntiAir, 5);

        let anti_air = stats(UnitType::AntiAir, 8000);
        let bcopter = stats(UnitType::Bcopters, 9000);
        let infantry = stats(UnitType::Infantry, 1000);

        // 海の向こうのヘリまで平均に混ぜると正の値になり、候補として生き残ってしまう
        let with_unreachable = counter_value(&anti_air, &[bcopter, infantry.clone()], &chart);
        assert!(with_unreachable > 0.0);

        // 実際に届く相手（上陸してくる歩兵）だけで採点すれば有効打がなく脱落する
        let engageable_only = counter_value_refs(&anti_air, &[&infantry], &chart);
        assert!(engageable_only <= 0.0);
    }

    /// 到達できる敵が 1 体もいなければ採点対象がなく、枠を埋める資格もない
    #[test]
    fn counter_value_of_an_empty_engageable_set_is_zero() {
        let chart = DamageChart::new();
        let anti_air = stats(UnitType::AntiAir, 8000);
        assert_eq!(counter_value_refs(&anti_air, &[], &chart), 0.0);
    }

    /// 同じ航空脅威を覆い切った後は、次の対空ユニットより未対処の地上脅威への
    /// 対抗ユニットが優先される。平均相性のままではこの切替が起きない。
    #[test]
    fn marginal_coverage_moves_from_covered_air_threat_to_ground_threat() {
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bcopters, 120);
        chart.insert_damage(UnitType::Bcopters, UnitType::AntiAir, 10);
        chart.insert_damage(UnitType::AntiAir, UnitType::Infantry, 0);
        chart.insert_damage(UnitType::Infantry, UnitType::AntiAir, 20);
        chart.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        chart.insert_damage(UnitType::Infantry, UnitType::Tank, 0);

        let anti_air = stats(UnitType::AntiAir, 8000);
        let tank = stats(UnitType::Tank, 7000);
        let mut threats = vec![
            ThreatTarget {
                entity: None,
                stats: stats(UnitType::Bcopters, 8000),
                position: pos(1, 0),
                current_hp: 100,
                remaining_value: 8000.0,
                priority_weight: 1.0,
                available_turn: 0,
            },
            ThreatTarget {
                entity: None,
                stats: stats(UnitType::Infantry, 7000),
                position: pos(2, 0),
                current_hp: 100,
                remaining_value: 7000.0,
                priority_weight: 1.0,
                available_turn: 0,
            },
        ];
        let indices = vec![0, 1];

        assert!(
            marginal_coverage(&anti_air, 8000.0, &threats, &indices, &chart)
                > marginal_coverage(&tank, 7000.0, &threats, &indices, &chart)
        );
        apply_coverage(&anti_air, 8000.0, &mut threats, &indices, &chart);

        assert_eq!(threats[0].remaining_value, 0.0);
        assert_eq!(
            marginal_coverage(&anti_air, 8000.0, &threats, &indices, &chart),
            0.0
        );
        assert!(marginal_coverage(&tank, 7000.0, &threats, &indices, &chart) > 0.0);
    }

    /// 戦略的重要度は候補の優先順位だけを上げ、必要な対抗戦力を水増ししない。
    #[test]
    fn strategic_weight_does_not_multiply_remaining_combat_value() {
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::TransportHelicopter, 120);
        chart.insert_damage(UnitType::TransportHelicopter, UnitType::AntiAir, 0);
        let anti_air = stats(UnitType::AntiAir, 8000);
        let mut threats = vec![ThreatTarget {
            entity: None,
            stats: stats(UnitType::TransportHelicopter, 8000),
            position: pos(1, 0),
            current_hp: 100,
            remaining_value: 8000.0,
            priority_weight: 2.0,
            available_turn: 0,
        }];

        let covered = apply_coverage(&anti_air, 8000.0, &mut threats, &[0], &chart);

        assert_eq!(covered, 16000.0);
        assert_eq!(threats[0].remaining_value, 0.0);
        assert_eq!(
            marginal_coverage(&anti_air, 8000.0, &threats, &[0], &chart),
            0.0
        );
    }

    /// 価格ベースの被覆を使い切っても、敵拡張unitを処理する攻撃回数は残る。
    #[test]
    fn territory_control_sortie_keeps_a_counter_candidate_after_value_is_covered() {
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 65);
        let helicopter = stats(UnitType::Bcopters, 7500);
        let threats = vec![ThreatTarget {
            entity: None,
            stats: stats(UnitType::Infantry, 1000),
            position: pos(1, 0),
            current_hp: 100,
            remaining_value: 0.0,
            priority_weight: 2.0,
            available_turn: 0,
        }];

        assert_eq!(
            marginal_coverage(&helicopter, 7500.0, &threats, &[0], &chart),
            0.0
        );
        assert!(territory_control_sortie_value(&helicopter, 2, &threats, &[0], &chart) > 0.0);
    }

    /// Combatは資金難なら費用効率、資金潤沢なら生産枠あたり戦果を優先する。
    #[test]
    fn combat_roi_can_choose_expensive_ground_attack_when_budget_is_abundant() {
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bcopters, UnitType::Rockets, 45);
        chart.insert_damage(UnitType::Bomber, UnitType::Rockets, 95);
        let helicopter = stats(UnitType::Bcopters, 7_500);
        let bomber = stats(UnitType::Bomber, 22_000);
        let threats = vec![
            ThreatTarget {
                entity: None,
                stats: stats(UnitType::Rockets, 6_000),
                position: pos(1, 0),
                current_hp: 100,
                remaining_value: 6_000.0,
                priority_weight: 1.0,
                available_turn: 0,
            },
            ThreatTarget {
                entity: None,
                stats: stats(UnitType::Rockets, 6_000),
                position: pos(2, 0),
                current_hp: 100,
                remaining_value: 6_000.0,
                priority_weight: 1.0,
                available_turn: 0,
            },
        ];
        let helicopter_return = marginal_sortie_return(&helicopter, 2, &threats, &[0, 1], &chart);
        let bomber_return = marginal_sortie_return(&bomber, 2, &threats, &[0, 1], &chart);

        assert!(bomber_return > helicopter_return);
        assert!(
            normalized_candidate_fitness(
                SlotKind::Combat,
                helicopter_return,
                helicopter.cost,
                30_000,
            ) < normalized_candidate_fitness(SlotKind::Combat, bomber_return, bomber.cost, 30_000,)
        );
        assert!(
            normalized_candidate_fitness(
                SlotKind::Combat,
                helicopter_return,
                helicopter.cost,
                8_000,
            ) > normalized_candidate_fitness(SlotKind::Combat, bomber_return, bomber.cost, 8_000,)
        );
    }

    /// Combat枠は購入価格ではなく、期限内に敵へ与えられる期待交換価値で充足する。
    #[test]
    fn combat_plan_does_not_treat_purchase_price_as_destroyed_enemy_value() {
        let infantry = UnitStats {
            can_capture: true,
            movement_type: MovementType::Infantry,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1_000)
        };
        let helicopter = UnitStats {
            movement_type: MovementType::Air,
            max_movement: 8,
            ..stats(UnitType::Bcopters, 7_500)
        };
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 65);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Bcopters, 0);
        let enemy_positions = [pos(8, 0), pos(8, 1), pos(8, 2)];
        let mut scan = BoardScan {
            map: flat_map(10, 3),
            master_data: MasterDataRegistry::load().unwrap(),
            damage_chart,
            funds: 22_500,
            free_facilities: vec![
                (pos(0, 1), Terrain::Factory),
                (pos(1, 0), Terrain::Airport),
                (pos(1, 1), Terrain::Airport),
                (pos(1, 2), Terrain::Airport),
            ],
            production_facilities: vec![
                (pos(0, 1), Terrain::Factory),
                (pos(1, 0), Terrain::Airport),
                (pos(1, 1), Terrain::Airport),
                (pos(1, 2), Terrain::Airport),
            ],
            available_types: vec![
                (UnitType::Infantry, infantry.clone()),
                (UnitType::Bcopters, helicopter),
            ],
            my_units: Vec::new(),
            enemy_units: enemy_positions
                .iter()
                .enumerate()
                .map(|(index, position)| UnitSnapshot {
                    entity: Some(Entity::from_raw(1_000 + index as u32)),
                    pos: *position,
                    stats: infantry.clone(),
                    hp: 100,
                    free_cargo: 0,
                })
                .collect(),
            owned_airport_count: 3,
            open_properties: enemy_positions.to_vec(),
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 5_000,
            campaign_objectives: capture_objective(enemy_positions[0]),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
        };

        let (commands, trace) = plan_production(&scan, PlayerId(1), false, &HashMap::new());

        // 旧実装は7,500の購入価格をそのまま撃破済み価値へ足し、2機で要求を
        // 消していた。65%攻撃を2回ずつ行う期待値では3機とも必要になる。
        assert_eq!(commands.len(), 3, "commands={commands:?}, trace={trace:?}");
        assert!(
            commands
                .iter()
                .all(|command| command.unit_type == UnitType::Bcopters)
        );

        // 当手番だけ空港が塞がっていても、将来購入を捨てず次手番に実行する。
        let helicopter = &mut scan
            .available_types
            .iter_mut()
            .find(|(unit_type, _)| *unit_type == UnitType::Bcopters)
            .expect("戦闘ヘリfixture")
            .1;
        helicopter.max_ammo1 = 9;
        helicopter.max_fuel = 100;
        helicopter.daily_fuel_consumption = 1;
        scan.free_facilities = vec![(pos(0, 1), Terrain::Factory)];
        let mut registry = V4RollingPlanRegistry::default();
        let (blocked_commands, blocked_trace) = plan_production_with_registry(
            &scan,
            PlayerId(1),
            false,
            &HashMap::new(),
            3,
            &mut registry,
        );
        assert!(blocked_commands.is_empty());
        let initial_plan_id = blocked_trace.rolling_combat_plans[0]
            .plan_id
            .unwrap_or_else(|| panic!("実行可能な将来計画は永続化される: {blocked_trace:?}"));
        assert!(
            blocked_trace
                .steps
                .iter()
                .any(|step| matches!(step.decision, ProductionDecision::Reserved { .. }))
        );

        scan.free_facilities = scan.production_facilities.clone();
        let (next_commands, next_trace) = plan_production_with_registry(
            &scan,
            PlayerId(1),
            false,
            &HashMap::new(),
            4,
            &mut registry,
        );
        assert!(!next_commands.is_empty(), "trace={next_trace:?}");
        assert!(
            next_trace
                .rolling_combat_plans
                .iter()
                .any(|plan| plan.plan_id == Some(initial_plan_id)),
            "同じplan_idの残りstepを実行する: {next_trace:?}"
        );
    }

    /// テスト用の作戦。枠の充足状況だけを見たいので敵情報は空にしておく。
    fn operation(kind: OperationKind, slots: OperationSlots, filled: OperationSlots) -> Operation {
        Operation {
            kind,
            anchor: pos(0, 0),
            staging_anchor: pos(0, 0),
            execution_authorized: true,
            objective_properties: vec![pos(0, 0)],
            threat_horizon: 0,
            facts: OperationFacts::default(),
            slots,
            filled,
            unreachable_threats: Vec::new(),
            reachable_threats: Vec::new(),
        }
    }

    /// 要求が青天井の撃破枠は、上限を持つ前提条件の枠を飢えさせてはならない
    ///
    /// 撃破枠の要求は「自軍が投入できる資金」そのものなので、資金の何倍にもなる。
    /// 未充足率は要求量で正規化されるため、何体買っても 1.0 からほとんど下がらない。
    /// 未充足率を枠の優先順位より先に見ると、撃破枠が恒久的に「最も飢えた枠」となり、
    /// 半分埋まった輸送枠（＝揚陸の足回り）へは 2 度と資金が回らなくなる。
    #[test]
    fn an_unbounded_slot_does_not_starve_bounded_prerequisite_slots() {
        // 輸送枠は半分充足（未充足率 0.5）、撃破枠は資金規模の要求でほぼ未充足（≒1.0）
        let ops = vec![operation(
            OperationKind::Capture,
            OperationSlots {
                transport_slots: 4,
                destroy_budget: 150_000,
                ..OperationSlots::default()
            },
            OperationSlots {
                transport_slots: 2,
                destroy_budget: 8_000,
                ..OperationSlots::default()
            },
        )];

        // 未充足率だけで選ぶと撃破枠が勝ってしまうことを、前提として確かめておく
        assert!(
            ops[0].slots.deficit_ratio(SlotKind::Combat, &ops[0].filled)
                > ops[0]
                    .slots
                    .deficit_ratio(SlotKind::Transport, &ops[0].filled)
        );

        assert_eq!(most_starved_slot(&ops), Some((0, SlotKind::Transport)));
    }

    /// 海峡マップ上に、母港の輸送艦 1 隻だけを置いた盤面を作る。
    ///
    /// 対岸 (8,1) を獲りにいく作戦から見て、この輸送艦は
    /// 「距離では母港側が最寄り」だが「積荷を対岸へ届けられる」という位置関係になる。
    fn strait_scan() -> BoardScan {
        let infantry = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1000)
        };
        let lander = UnitStats {
            movement_type: MovementType::Ship,
            max_movement: 6,
            max_cargo: 2,
            ..stats(UnitType::Lander, 12000)
        };

        BoardScan {
            map: strait_map(Some(Terrain::Shoal)),
            master_data: MasterDataRegistry::load().unwrap(),
            damage_chart: DamageChart::new(),
            funds: 20000,
            free_facilities: vec![(pos(1, 1), Terrain::Port)],
            production_facilities: vec![(pos(1, 1), Terrain::Port)],
            available_types: vec![
                (UnitType::Infantry, infantry),
                (UnitType::Lander, lander.clone()),
            ],
            // 母港 (1,1) に停泊したままの輸送艦。空き搭載スロット 2。
            my_units: vec![UnitSnapshot {
                entity: None,
                pos: pos(1, 1),
                stats: lander,
                hp: 100,
                free_cargo: 2,
            }],
            enemy_units: Vec::new(),
            owned_airport_count: 0,
            open_properties: vec![pos(8, 1)],
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 1000,
            campaign_objectives: Vec::new(),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
        }
    }

    /// 輸送台帳は「最寄り作戦」ではなく「その作戦へ届けられるか」で数える
    ///
    /// 輸送ユニットは自軍港湾に生まれて港に留まるため、位置で仕分けると常に
    /// 母港に近い作戦の台帳に載る。渡洋する作戦側の空き搭載スロットは永久に 0 となり、
    /// 輸送枠の要求が減らないまま揚陸艇を延々と買い増すラチェットになる。
    #[test]
    fn transports_are_ledgered_by_delivery_ability_not_proximity() {
        let scan = strait_scan();
        let mut ctx = ReachCtx::default();
        let reference = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1000)
        };
        // 母港側と対岸側、2 つの作戦地点がある盤面
        let anchors = vec![pos(0, 1), pos(8, 1)];
        let horizons = vec![5; anchors.len()];

        // 前提: 母港の輸送艦は距離では母港側の作戦が最寄りである
        assert!(
            eta_turns(&scan.map, &pos(1, 1), &anchors[0], 6)
                < eta_turns(&scan.map, &pos(1, 1), &anchors[1], 6)
        );

        let overseas = build_operation(
            &scan,
            &mut ctx,
            &reference,
            OperationKind::Capture,
            anchors[1],
            &anchors,
            &horizons,
            &[anchors[1]],
            &HashSet::new(),
            3,
        );

        // それでも「対岸へ積荷を届けられる」以上、渡洋作戦の台帳に載らねばならない
        assert_eq!(overseas.facts.available_free_cargo_slots, 2);
    }

    #[test]
    fn loaded_enemy_transport_on_another_island_is_a_future_arrival() {
        let mut scan = strait_scan();
        scan.enemy_units.push(UnitSnapshot {
            entity: Some(Entity::from_raw(700)),
            pos: pos(1, 1),
            stats: UnitStats {
                movement_type: MovementType::Air,
                max_movement: 7,
                max_cargo: 2,
                loadable_unit_types: vec![UnitType::Infantry],
                ..stats(UnitType::TransportHelicopter, 4_000)
            },
            hp: 100,
            free_cargo: 1,
        });
        let reference = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1_000)
        };
        let anchor = pos(8, 1);
        let mut ctx = ReachCtx::default();
        let operation = build_operation(
            &scan,
            &mut ctx,
            &reference,
            OperationKind::Capture,
            anchor,
            &[anchor],
            &[5],
            &[anchor],
            &HashSet::new(),
            3,
        );

        assert_eq!(operation.reachable_threats.len(), 1);
        let threat = &operation.reachable_threats[0];
        assert_eq!(threat.entity, None, "別島の敵Entityを追跡対象にしない");
        assert!(threat.available_turn > 0, "anchor到着後の増援として扱う");
        assert_eq!(threat.position, anchor);
    }

    #[test]
    fn empty_enemy_transport_does_not_create_anti_air_demand_on_every_island() {
        let mut scan = strait_scan();
        scan.enemy_units.push(UnitSnapshot {
            entity: Some(Entity::from_raw(700)),
            pos: pos(1, 1),
            stats: UnitStats {
                movement_type: MovementType::Air,
                max_movement: 7,
                max_cargo: 2,
                loadable_unit_types: vec![UnitType::Infantry],
                ..stats(UnitType::TransportHelicopter, 4_000)
            },
            hp: 100,
            free_cargo: 2,
        });
        let reference = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1_000)
        };
        let anchor = pos(8, 1);
        let operation = build_operation(
            &scan,
            &mut ReachCtx::default(),
            &reference,
            OperationKind::Capture,
            anchor,
            &[anchor],
            &[5],
            &[anchor],
            &HashSet::new(),
            3,
        );

        assert!(operation.reachable_threats.is_empty());
    }

    /// 平地マップに工場 3 基と未取得拠点を置いた、生産ループ検証用の盤面。
    ///
    /// 空き施設を複数持たせることで「同一ターン内に複数施設へ発注が飛ぶ」状況を作り、
    /// その内訳がトレースに残るかを確かめられるようにする。
    fn multi_factory_scan() -> BoardScan {
        let infantry = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1000)
        };
        let tank = UnitStats {
            max_movement: 6,
            ..stats(UnitType::Tank, 7000)
        };

        BoardScan {
            map: flat_map(9, 5),
            master_data: MasterDataRegistry::load().unwrap(),
            damage_chart: DamageChart::new(),
            funds: 20000,
            free_facilities: vec![
                (pos(1, 1), Terrain::Factory),
                (pos(1, 2), Terrain::Factory),
                (pos(1, 3), Terrain::Factory),
            ],
            production_facilities: vec![
                (pos(1, 1), Terrain::Factory),
                (pos(1, 2), Terrain::Factory),
                (pos(1, 3), Terrain::Factory),
            ],
            available_types: vec![(UnitType::Infantry, infantry), (UnitType::Tank, tank)],
            my_units: Vec::new(),
            enemy_units: Vec::new(),
            owned_airport_count: 0,
            open_properties: vec![pos(6, 2)],
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 1000,
            campaign_objectives: capture_objective(pos(6, 2)),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
        }
    }

    /// 航空・地上の未対処脅威と工場3基を持つ、限界価値の統合テスト盤面。
    fn mixed_threat_multi_factory_scan() -> BoardScan {
        let infantry = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1000)
        };
        let anti_air = UnitStats {
            max_movement: 6,
            ..stats(UnitType::AntiAir, 8000)
        };
        let tank = UnitStats {
            max_movement: 6,
            ..stats(UnitType::Tank, 7000)
        };
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::AntiAir, UnitType::Bcopters, 120);
        damage_chart.insert_damage(UnitType::Bcopters, UnitType::AntiAir, 10);
        damage_chart.insert_damage(UnitType::AntiAir, UnitType::Infantry, 0);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::AntiAir, 20);
        damage_chart.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Tank, 0);

        BoardScan {
            map: flat_map(9, 5),
            master_data: MasterDataRegistry::load().unwrap(),
            damage_chart,
            funds: 17000,
            free_facilities: vec![
                (pos(1, 0), Terrain::Factory),
                (pos(1, 1), Terrain::Factory),
                (pos(1, 2), Terrain::Factory),
                (pos(1, 3), Terrain::Factory),
            ],
            production_facilities: vec![
                (pos(1, 0), Terrain::Factory),
                (pos(1, 1), Terrain::Factory),
                (pos(1, 2), Terrain::Factory),
                (pos(1, 3), Terrain::Factory),
            ],
            available_types: vec![
                (UnitType::Infantry, infantry.clone()),
                (UnitType::AntiAir, anti_air),
                (UnitType::Tank, tank),
            ],
            my_units: Vec::new(),
            enemy_units: vec![
                UnitSnapshot {
                    entity: Some(Entity::from_raw(101)),
                    pos: pos(6, 1),
                    stats: stats(UnitType::Bcopters, 8000),
                    hp: 100,
                    free_cargo: 0,
                },
                UnitSnapshot {
                    entity: Some(Entity::from_raw(102)),
                    pos: pos(6, 3),
                    stats: infantry,
                    hp: 100,
                    free_cargo: 0,
                },
            ],
            owned_airport_count: 0,
            open_properties: vec![pos(6, 2)],
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 1000,
            campaign_objectives: capture_objective(pos(6, 2)),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
        }
    }

    /// キャンペーンの予約超過分は、構造枠を二重購入せず、自力展開可能な対敵戦力へ使う。
    #[test]
    fn campaign_surplus_targets_enemy_infantry_without_buying_stranded_tank() {
        let mut map = flat_map(9, 3);
        for y in 0..map.height {
            map.set_terrain(4, y, Terrain::Sea).unwrap();
        }
        let infantry = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1_000)
        };
        let tank = UnitStats {
            movement_type: MovementType::Tank,
            max_movement: 6,
            ..stats(UnitType::Tank, 6_000)
        };
        let fighter = UnitStats {
            movement_type: MovementType::Air,
            max_movement: 8,
            max_fuel: 70,
            daily_fuel_consumption: 5,
            ..stats(UnitType::Fighter, 16_000)
        };
        let transport = UnitStats {
            movement_type: MovementType::Air,
            max_movement: 7,
            max_cargo: 2,
            loadable_unit_types: vec![UnitType::Infantry],
            ..stats(UnitType::TransportHelicopter, 4_000)
        };
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Fighter, UnitType::Infantry, 80);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Fighter, 0);
        damage_chart.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Tank, 0);
        let scan = BoardScan {
            map,
            master_data: MasterDataRegistry::load().unwrap(),
            damage_chart,
            funds: 20_000,
            free_facilities: vec![(pos(1, 1), Terrain::Factory), (pos(2, 1), Terrain::Airport)],
            production_facilities: vec![
                (pos(1, 1), Terrain::Factory),
                (pos(2, 1), Terrain::Airport),
            ],
            available_types: vec![
                (UnitType::Infantry, infantry.clone()),
                (UnitType::Tank, tank),
                (UnitType::Fighter, fighter),
                (UnitType::TransportHelicopter, transport),
            ],
            my_units: Vec::new(),
            enemy_units: vec![UnitSnapshot {
                entity: Some(Entity::from_raw(901)),
                pos: pos(7, 1),
                stats: infantry,
                hp: 100,
                free_cargo: 0,
            }],
            owned_airport_count: 1,
            open_properties: vec![pos(7, 1)],
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 5_000,
            campaign_objectives: capture_objective(pos(7, 1)),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
        };

        let (commands, trace) = plan_production(&scan, PlayerId(1), false, &HashMap::new());

        assert_eq!(commands.len(), 1, "commands={commands:?}, trace={trace:?}");
        assert_eq!(commands[0].unit_type, UnitType::Fighter);
        assert!(commands.iter().all(|command| {
            !matches!(
                command.unit_type,
                UnitType::Infantry | UnitType::TransportHelicopter | UnitType::Tank
            )
        }));
    }

    /// 同一手番の混成パッケージに、航空・地上の両脅威へ有効なunitを含める。
    #[test]
    fn multi_factory_plan_switches_after_air_threat_is_covered() {
        let scan = mixed_threat_multi_factory_scan();
        let (commands, trace) = plan_production(&scan, PlayerId(0), true, &HashMap::new());
        let combat_types: Vec<UnitType> = commands
            .iter()
            .map(|command| command.unit_type)
            .filter(|unit_type| !matches!(unit_type, UnitType::Infantry))
            .collect();

        assert_eq!(
            combat_types.len(),
            2,
            "commands={commands:?}, trace={trace:?}"
        );
        assert!(combat_types.contains(&UnitType::AntiAir));
        assert!(combat_types.contains(&UnitType::Tank));
    }

    /// 増援予算だけでは敵兵種を特定できないため、敵未観測の撃破枠から汎用兵を作らない。
    #[test]
    fn combat_slot_without_observed_threat_does_not_produce() {
        let mut scan = multi_factory_scan();
        scan.enemy_income = 10_000;
        scan.enemy_production_slots = 1;

        let (commands, trace) = plan_production(&scan, PlayerId(0), true, &HashMap::new());

        assert!(
            commands
                .iter()
                .all(|command| command.unit_type != UnitType::Tank)
        );
        assert!(trace.steps.iter().all(|step| {
            step.slot_kind != SlotKind::Combat
                || !matches!(step.decision, ProductionDecision::Produced { .. })
        }));
    }

    /// 生産トレースは、発行した命令 1 件ごとに「どの作戦のどの枠から出たか」を残す
    ///
    /// 「同一ターン内に同じユニットが全施設へ発注される」現象を切り分けるには、
    /// 発注とトレースが 1 対 1 で対応していなければならない。ズレた時点で
    /// 診断そのものが無意味になるため、記録専用であるという不変条件をここで固定する。
    #[test]
    fn production_trace_attributes_every_command_to_a_slot() {
        let scan = multi_factory_scan();
        let (commands, trace) = plan_production(&scan, PlayerId(0), true, &HashMap::new());

        // 作戦が立つ盤面なので fallback には落ちない
        assert!(!trace.fallback);
        assert!(!trace.operations.is_empty());
        assert!(!commands.is_empty());
        assert_eq!(trace.funds, scan.funds);
        assert_eq!(trace.free_facility_count, scan.free_facilities.len());

        // 発注は 1 件残らず Produced ステップとして記録される
        let produced: Vec<_> = trace
            .steps
            .iter()
            .filter_map(|step| match &step.decision {
                ProductionDecision::Produced {
                    unit_type,
                    cost,
                    facility,
                } => Some((*unit_type, *cost, *facility)),
                _ => None,
            })
            .collect();
        assert_eq!(produced.len(), commands.len());
        for (command, (unit_type, _, facility)) in commands.iter().zip(produced.iter()) {
            assert_eq!(command.unit_type, *unit_type);
            assert_eq!(command.target_x, facility.x);
            assert_eq!(command.target_y, facility.y);
            // 発注先は必ず空き施設のいずれか
            assert!(scan.free_facilities.iter().any(|(f, _)| f == facility));
        }

        // 資金の収支が合うこと（余剰資金の積み上がりを測る土台になる）
        let spent: u32 = produced.iter().map(|(_, cost, _)| *cost).sum();
        assert_eq!(trace.leftover_funds, scan.funds - spent);

        // 種別ごとの体数集計も命令と一致する（工場数への張り付きを数える入口）
        assert_eq!(
            trace.produced_counts().values().sum::<usize>(),
            commands.len()
        );
    }

    /// 届けられない作戦地点の台帳には載せない（二重計上の歯止め）
    #[test]
    fn transports_are_not_ledgered_for_unreachable_anchors() {
        let mut scan = strait_scan();
        // 接岸できる地形を消すと、艦船は対岸へ陸上ユニットを降ろせなくなる
        scan.map = strait_map(None);
        let mut ctx = ReachCtx::default();
        let reference = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1000)
        };
        let anchors = vec![pos(0, 1), pos(8, 1)];
        let horizons = vec![5; anchors.len()];

        let overseas = build_operation(
            &scan,
            &mut ctx,
            &reference,
            OperationKind::Capture,
            anchors[1],
            &anchors,
            &horizons,
            &[anchors[1]],
            &HashSet::new(),
            3,
        );

        assert_eq!(overseas.facts.available_free_cargo_slots, 0);
    }

    /// 前提条件どうしの競合では、作戦の優先度が枠の優先順位より上位に効く
    #[test]
    fn operation_priority_outranks_slot_priority_among_prerequisites() {
        let ops = vec![
            // 占領作戦の輸送枠（枠としては先だが、作戦としては後回し）
            operation(
                OperationKind::Capture,
                OperationSlots {
                    transport_slots: 4,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
            // 防衛作戦の護衛枠（枠としては最後だが、作戦が最優先）。
            // 護衛は「敵の接触までに要る体数」で有限なので前提条件側に属する。
            operation(
                OperationKind::Defense,
                OperationSlots {
                    escort_units: 2,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
        ];

        assert_eq!(most_starved_slot(&ops), Some((1, SlotKind::Combat)));
    }

    /// 最優先作戦の撃破枠が、後回し作戦の前提条件を飢えさせてはならない
    ///
    /// 撃破枠の要求は青天井なので、作戦優先度で先に見てしまうと自陣の防衛作戦が
    /// 全額を吸い、渡洋作戦には輸送も占領要員も 1 体も回らない（＝引きこもる）。
    /// 前提条件は作戦をまたいで先に満たす。
    #[test]
    fn a_top_priority_destroy_budget_does_not_starve_a_lower_priority_prerequisite() {
        let ops = vec![
            // 自陣の防衛作戦。撃破枠は資金規模の青天井要求。
            operation(
                OperationKind::Defense,
                OperationSlots {
                    destroy_budget: 150_000,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
            // 渡洋する占領作戦。輸送が無ければ 1 歩も進めない。
            operation(
                OperationKind::Capture,
                OperationSlots {
                    transport_slots: 2,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
        ];

        assert_eq!(most_starved_slot(&ops), Some((1, SlotKind::Transport)));
    }

    /// 余剰の配分は作戦の優先度ではなく未充足率で決める
    ///
    /// 撃破要求は既に前線ごとの分担比で割ってあるので、未充足率で選び続ければ
    /// 資金は各前線の分担比どおりに配分される。
    #[test]
    fn residual_funds_follow_the_deficit_not_the_operation_rank() {
        let ops = vec![
            // 最優先の防衛作戦。撃破枠はほぼ充足済み。
            operation(
                OperationKind::Defense,
                OperationSlots {
                    destroy_budget: 10_000,
                    ..OperationSlots::default()
                },
                OperationSlots {
                    destroy_budget: 9_000,
                    ..OperationSlots::default()
                },
            ),
            // 後回しの占領作戦。撃破枠は手つかず。
            operation(
                OperationKind::Capture,
                OperationSlots {
                    destroy_budget: 10_000,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
        ];

        assert_eq!(most_starved_slot(&ops), Some((1, SlotKind::Combat)));
    }

    /// 島作戦の不足はV4汎用作戦より先に発注し、不完全なパッケージの途中で
    /// 余った施設を汎用生産へ開放しない。
    #[test]
    fn v4_prioritizes_campaign_package_and_blocks_generic_when_incomplete() {
        use crate::ai::engine::AiTurnStrategyCache;
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignPortfolio,
            IslandCampaignRequirement,
        };
        use crate::ai::islands::IslandId;
        use crate::resources::Players;
        use crate::resources::master_data::MasterDataRegistry;

        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_3",
            GridTopology::Hex,
        )
        .expect("map_3 should initialize");
        let player_id = PlayerId(1);

        // 輸送船だけは買えるが、同時必須の占領要員までは買えない資金に固定する。
        world
            .resource_mut::<Players>()
            .0
            .iter_mut()
            .find(|player| player.id == player_id)
            .expect("player 1 should exist")
            .funds = 16_500;

        let requirement = IslandCampaignRequirement {
            preferred_transport: Some(UnitType::Lander),
            transport_slots: 2,
            capture_units: 1,
            ground_combat_units: 0,
            combat_budget: 0,
            total_budget: 17_500,
        };
        let assignment = IslandCampaignAssignment {
            island_id: IslandId(1),
            decision: IslandCampaignDecision::Reinforce,
            target_position: pos(23, 24),
            capture_target_positions: vec![pos(23, 24)],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: requirement,
            allocated_budget: 0,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        let mut cache = AiTurnStrategyCache::default();
        cache.set_campaign_portfolio(
            player_id,
            IslandCampaignPortfolio {
                active_offensives: vec![assignment],
                ..IslandCampaignPortfolio::default()
            },
        );
        world.insert_resource(cache);

        let first = decide_production_v4(&mut world, player_id);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].unit_type, UnitType::Lander);

        // 同一手番の次呼び出しでは、不足した占領要員を無視してgenericを出さない。
        assert!(decide_production_v4(&mut world, player_id).is_empty());
        assert!(
            world
                .resource::<AiTurnStrategyCache>()
                .campaign_production_blocks_generic(player_id)
        );
    }
}
