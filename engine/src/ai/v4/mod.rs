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

pub mod campaign_execution;
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
    ProductionStepTrace, ProductionTraceDiagnostics, ReinforcementContingencyTrace,
    RollingCombatPlanTrace, RollingPurchaseTrace, RollingTargetTrace,
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
type EngagementKey = (MovementType, (usize, usize), (usize, usize), u32);

/// 到達性まわりの計算結果を、1 回の生産判断のあいだだけ再利用するためのコンテキスト。
///
/// 揚陸可否の判定はマップ全域の走査を伴うため、施設と目標の組み合わせごとに
/// 結果を憶えておかないと候補評価のたびに同じ探索を繰り返すことになる。
#[derive(Default)]
struct ReachCtx {
    terrain: TerrainConnectivity,
    delivery: HashMap<DeliveryKey, bool>,
    engagement: HashMap<EngagementKey, bool>,
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

    /// 標的位置そのものではなく、射程内の合法地形へ到達できるかを判定する。
    /// 艦船の遠距離射撃を「陸上座標へ入れない」という理由で候補外にしない。
    fn can_reach_engagement_envelope(
        &mut self,
        map: &Map,
        registry: &MasterDataRegistry,
        start: (usize, usize),
        target: (usize, usize),
        movement_type: MovementType,
        max_range: u32,
    ) -> bool {
        let key = (movement_type, start, target, max_range.max(1));
        if let Some(cached) = self.engagement.get(&key) {
            return *cached;
        }
        let range = max_range.max(1);
        let reachable = (0..map.height).any(|y| {
            (0..map.width).any(|x| {
                map.distance(x, y, target.0, target.1) <= range
                    && map.get_terrain(x, y).is_some_and(|terrain| {
                        crate::systems::movement::get_valid_movement_cost(
                            registry,
                            movement_type,
                            terrain,
                        )
                        .is_some()
                    })
                    && self.is_reachable(map, registry, start, (x, y), movement_type)
            })
        });
        self.engagement.insert(key, reachable);
        reachable
    }
}

/// 同時に抱える作戦の最大数。多すぎると戦力が分散するため制限する。
const MAX_OPERATIONS: usize = 4;

/// 敵がこのターン数以内に到達できる自軍拠点は防衛作戦の対象とする。
const DEFENSE_THREAT_ETA: u32 = 2;

/// 占領開始後に拠点を確保し切るまでに必要な最小手番数。
const CAPTURE_COMPLETION_TURNS: u32 = 2;

type ScoredOperation = (bool, u32, OperationKind, Vec<GridPosition>);

/// 必須作戦を差し込む際、もう一方の勝利ロードマップ必須作戦を追い出さない。
fn required_operation_eviction_index(scored: &[ScoredOperation], required: OperationKind) -> usize {
    scored
        .iter()
        .rposition(|(_, _, kind, _)| {
            *kind != required
                && !matches!(
                    (required, *kind),
                    (OperationKind::Capture, OperationKind::AssaultCapital)
                        | (OperationKind::AssaultCapital, OperationKind::Capture)
                )
        })
        .unwrap_or(scored.len() - 1)
}

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

/// 1体の敵に対する実盤面情報。
///
/// 戦力を購入価格へ換算しない。必要戦力はRollingPlanがHP・与ダメージ・移動・
/// 攻撃可能回数を手番ごとにシミュレーションして決める。
#[derive(Debug, Clone)]
struct ThreatTarget {
    entity: Option<Entity>,
    stats: UnitStats,
    position: GridPosition,
    current_hp: u32,
    /// 0は現在の局地敵。1以上はanchorへ到着してから交戦可能になる観測済み増援。
    available_turn: u32,
}

impl ThreatTarget {
    fn from_snapshot(unit: &UnitSnapshot) -> Self {
        Self {
            entity: unit.entity,
            stats: unit.stats.clone(),
            position: unit.pos,
            current_hp: unit.hp,
            available_turn: 0,
        }
    }
}

/// 1 つの作戦。対象拠点のまとまりと、そこから導出された枠を保持する。
#[derive(Debug)]
struct Operation {
    kind: OperationKind,
    /// 島campaignの永続identity。anchorや未所有施設集合が変わっても維持する。
    island_id: Option<crate::ai::islands::IslandId>,
    /// 作戦の代表地点（距離計算の基準）
    anchor: GridPosition,
    /// 編成中の戦力を逐次投入せず集結させる、自軍側の安全な地点。
    staging_anchor: GridPosition,
    /// falseの首都作戦は生産だけ進め、攻撃任務へ切り替えない。
    execution_authorized: bool,
    /// 占領完了まで敵の攻撃から生存させる必要があるcampaign占領Entity。
    protected_capture_entities: HashSet<Entity>,
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
    /// 観測後のcounterでは接触前に止められない将来増援だけを現在編成へ含める。
    unavoidable_reinforcements: Vec<EnemyPlanUnit>,
    /// 観測後に間に合うため、具体的counterと生産slotだけを予約する条件付き計画。
    reinforcement_contingencies: Vec<ReinforcementContingency>,
    contingency_reserve_funds: u32,
}

#[derive(Debug, Clone, Copy)]
struct ReinforcementContingency {
    enemy_type: UnitType,
    enemy_contact_turn: u32,
    counter_type: UnitType,
    counter_facility: GridPosition,
    counter_build_turn: u32,
    counter_contact_turn: u32,
    attacks_required: u32,
    reserve_cost: u32,
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
            let combat_actuals = record.plan_combat_actuals();
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
                // PlanIdを持つ生産Entityの全戦闘を、そのPlanの実績へ一度だけ戻す。
                // 「現在標的だけ」に絞ると、同一作戦圏内の好機標的へ切り替えた攻撃や、
                // revisionで標的が変わる前の戦闘が予実から欠落する。
                first_attack_turn: combat_actuals.first_attack_turn,
                attack_count: combat_actuals.attack_count,
                priority_attack_count: combat_actuals.priority_attack_count,
                kill_count: combat_actuals.kill_count,
                damage_value_dealt: combat_actuals.damage_value_dealt,
                counter_value_received: combat_actuals.counter_value_received,
                destroyed_value: combat_actuals.destroyed_value,
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
    let plan_exists = world
        .get_resource::<crate::ai::engine::AiTurnStrategyCache>()
        .is_some_and(|cache| cache.campaign_production_planned(player_id));
    if plan_exists {
        let mut cache = world
            .remove_resource::<crate::ai::engine::AiTurnStrategyCache>()
            .unwrap_or_default();
        let next = cache.take_campaign_production_command(player_id);
        let blocks_generic = cache.campaign_production_blocks_generic(player_id);
        let generic_budget = cache.campaign_production_generic_budget(player_id);
        world.insert_resource(cache);
        return match next {
            Some(command) => {
                mark_campaign_production_issued(world, player_id, turn, &command);
                CampaignProductionControl::Command(command)
            }
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
        return CampaignProductionControl::Continue;
    }

    // 航空掃討は後段のrolling planへ委譲する。一方、敵領Assaultで輸送・配置枠まで
    // 予約済みの地上波はcampaign Squadへ渡さないと港で遊兵になるため、必要実体数と
    // その購入上限だけを残す。
    for shortfall in &mut shortfalls {
        if shortfall.decision != crate::ai::island_campaign::IslandCampaignDecision::Assault
            || shortfall.ground_combat_units == 0
        {
            shortfall.combat_units = 0;
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
    let outcome = plan_campaign_with_expansion_denial_reserve(
        player_id,
        &shortfalls,
        &scan.free_facilities,
        scan.owned_airport_count,
        &scan.available_types,
        &enemy_stats,
        &scan.damage_chart,
        &scan.map,
        &scan.master_data,
        scan.funds,
    );
    world.init_resource::<campaign_execution::V4CampaignExecutionRegistry>();
    world
        .resource_mut::<campaign_execution::V4CampaignExecutionRegistry>()
        .replace_turn_intents(player_id, turn, &outcome.intents);
    let generic_budget = outcome.generic_funds;
    let mut cache = world
        .remove_resource::<crate::ai::engine::AiTurnStrategyCache>()
        .unwrap_or_default();
    cache.set_campaign_production_plan_with_generic_budget(
        player_id,
        outcome.commands,
        generic_budget,
    );
    let next = cache.take_campaign_production_command(player_id);
    let blocks_generic = cache.campaign_production_blocks_generic(player_id);
    let generic_budget = cache.campaign_production_generic_budget(player_id);
    world.insert_resource(cache);

    match next {
        Some(command) => {
            mark_campaign_production_issued(world, player_id, turn, &command);
            CampaignProductionControl::Command(command)
        }
        None if blocks_generic => CampaignProductionControl::BlockGeneric,
        None if generic_budget.is_some_and(|budget| budget > 0) => {
            CampaignProductionControl::ContinueWithSurplus(generic_budget.unwrap_or(0))
        }
        None => CampaignProductionControl::Continue,
    }
}

fn mark_campaign_production_issued(
    world: &mut World,
    player_id: PlayerId,
    turn: u32,
    command: &ProduceUnitCommand,
) {
    if let Some(mut registry) =
        world.get_resource_mut::<campaign_execution::V4CampaignExecutionRegistry>()
    {
        registry.mark_issued(player_id, turn, command);
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
    /// anchorの局所clusterではなく、島作戦本体が要求する占領完了時の生存兵数。
    required_capture_survivors: usize,
    /// 固定兵站経路内の工程順。経路外campaignはNone。
    logistics_rank: Option<u32>,
    /// 同じ島の敵を別前線へ分配せず、この戦略作戦へ所属させる。
    forced_target_enemies: HashSet<Entity>,
    /// 施設数だけでなく、実際の占領兵が完了時点まで生存する案を評価する。
    protected_capture_entities: HashSet<Entity>,
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
                            required_capture_survivors: usize::try_from(
                                assignment.requirement.capture_units,
                            )
                            .unwrap_or(usize::MAX),
                            logistics_rank: logistics_ranks.get(&assignment.island_id).copied(),
                            forced_target_enemies: HashSet::new(),
                            protected_capture_entities: assignment
                                .capture_entities
                                .iter()
                                .copied()
                                .collect(),
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
                required_capture_survivors: 0,
                logistics_rank: None,
                forced_target_enemies,
                protected_capture_entities: HashSet::new(),
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
    let mut campaign_clusters = scan
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
    // 1島に通常campaignと首都攻略を同時生成しない。首都島では勝利条件を表す
    // AssaultCapitalを正本とし、同じ島の局地Captureを別Planとして並走させない。
    campaign_clusters.sort_by_key(|(objective, _)| {
        (
            objective.island_id.0,
            u8::from(objective.kind != OperationKind::AssaultCapital),
            objective.kind.priority_rank(),
        )
    });
    campaign_clusters.dedup_by_key(|(objective, _)| objective.island_id);
    raw.retain(|(_, cluster)| {
        !campaign_clusters.iter().any(|(objective, _)| {
            cluster.iter().any(|property| {
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
            .or_else(|| {
                // Capture→Defense/Contestの遷移でも、島そのものが同じなら同一campaign。
                campaign_clusters.iter().find_map(|(objective, _)| {
                    cluster
                        .iter()
                        .any(|property| {
                            island_map
                                .get_island_at(property)
                                .is_some_and(|island| island.id == objective.island_id)
                        })
                        .then_some(*objective)
                })
            })
    };

    // 生産施設から近い作戦を優先して MAX_OPERATIONS 件に絞る
    let mut scored: Vec<ScoredOperation> = raw
        .into_iter()
        .filter(|(_, cluster)| !cluster.is_empty())
        .map(|(kind, cluster)| {
            let anchor = campaign_for_cluster(kind, &cluster)
                .map_or_else(|| anchor_of(&cluster, scan), |objective| objective.anchor);
            let lead = facility_lead_time(scan, &anchor, reference.max_movement);
            let continuing = active_objectives.iter().any(|objective| {
                campaign_for_cluster(kind, &cluster)
                    .is_some_and(|campaign| objective.island_id == Some(campaign.island_id))
                    || (objective.island_id.is_none()
                        && objective.kind == kind
                        && cluster
                            .iter()
                            .any(|property| objective.properties.contains(property)))
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
            let removable = required_operation_eviction_index(&scored, OperationKind::Capture);
            scored.remove(removable);
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
            // 末尾には救済した進行中Captureが入ることがある。これを再び落とさず、
            // Capture/首都以外で最も低位の作戦を1件だけ外す。
            let removable =
                required_operation_eviction_index(&scored, OperationKind::AssaultCapital);
            scored.remove(removable);
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
                        .find(|objective| {
                            planning_objective.is_some_and(|campaign| {
                                objective.island_id == Some(campaign.island_id)
                            }) || (objective.island_id.is_none()
                                && objective.kind == kind
                                && objective.properties == cluster)
                        })
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
                operation.island_id = Some(objective.island_id);
                operation.staging_anchor = objective.staging_anchor;
                operation.execution_authorized = objective.execution_authorized;
                operation.protected_capture_entities = objective.protected_capture_entities.clone();
            } else if let Some(active) = active_objectives
                .iter()
                .find(|objective| objective.kind == kind && objective.properties == cluster)
            {
                // portfolioがObserveへ一時遷移しても、active Plan由来のOperationは
                // 島identityを失わない。敵やanchorの変化で新Planへ分裂させない。
                operation.island_id = active.island_id;
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
fn projected_enemy_reinforcement_funds(
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
                // 編成中でまだ前進を許可していない首都攻略は、敵増援の配分先ではない。
                // ここへ距離0で全枠を吸わせると、実際に占領中の中央島を「増援なし」と
                // 誤認して占領兵だけを送り続けるため、実行中の局地作戦を先に評価する。
                let is_unauthorized_capital_formation = scan
                    .campaign_objectives
                    .iter()
                    .find(|objective| objective.anchor == *anchor)
                    .is_some_and(|objective| {
                        objective.kind == OperationKind::AssaultCapital
                            && !objective.execution_authorized
                    });
                if is_unauthorized_capital_formation {
                    return None;
                }
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

    let mut reachable_threats = Vec::new();
    let mut unreachable_threats = Vec::new();
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
            reachable_threats.push(ThreatTarget::from_snapshot(enemy));
        } else if !i_can_reach && local_contact {
            unreachable_threats.push(ThreatTarget::from_snapshot(enemy));
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
            let mut incoming = ThreatTarget::from_snapshot(enemy);
            incoming.entity = None;
            incoming.position = anchor;
            incoming.available_turn = arrival_eta.max(1);
            reachable_threats.push(incoming);
        }
    }
    let enemy_combat_units = u32::try_from(reachable_threats.len()).unwrap_or(u32::MAX);
    let unreachable_threat_units = u32::try_from(unreachable_threats.len()).unwrap_or(u32::MAX);
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
    let mut friendly_combat_units_committed = 0u32;
    let mut friendly_intercept_units_committed = 0u32;
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
                friendly_intercept_units_committed =
                    friendly_intercept_units_committed.saturating_add(1);
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
                friendly_combat_units_committed = friendly_combat_units_committed.saturating_add(1);
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

    let enemy_reinforcement_funds = anchor_index.map_or(0, |index| {
        projected_enemy_reinforcement_funds(scan, ctx, anchors, horizons, index)
    });

    // 輸送 1 往復にかかるターン数（片道リードタイムの 2 倍）
    let transport_round_trip_turns = deploy_lead_time.saturating_mul(2).max(1);

    let facts = OperationFacts {
        target_property_count: cluster.len() as u32,
        friendly_capture_units_committed,
        enemy_combat_units,
        friendly_combat_units_committed,
        enemy_reinforcement_funds,
        friendly_intercept_units_committed,
        deploy_lead_time,
        enemy_contact_eta: if enemy_contact_eta == u32::MAX {
            u32::MAX
        } else {
            enemy_contact_eta
        },
        requires_transport,
        transport_round_trip_turns,
        available_free_cargo_slots,
        unreachable_threat_units,
    };

    let mut slots = derive_slots(&facts);
    // Combat枠は金額の差分ではなく、観測敵が残っていることだけで計画器を起動する。
    // 必要数と完了判定はrolling plannerのHPシミュレーションが決める。
    slots.combat_plan_required = u32::from(
        !reachable_threats.is_empty()
            || (kind == OperationKind::AssaultCapital && enemy_reinforcement_funds > 0),
    );
    if kind == OperationKind::AssaultCapital {
        // 首都準備段階では戦闘編成だけを形成する。占領兵と輸送は兵站路確保後に
        // IslandCampaignが実行可能な波として組み、ここで汎用任務へ流さない。
        slots.capture_units = 0;
        slots.transport_slots = 0;
        slots.intercept_units = 0;
    }

    let mut operation = Operation {
        kind,
        island_id: None,
        anchor,
        staging_anchor: anchor,
        execution_authorized: true,
        protected_capture_entities: HashSet::new(),
        objective_properties: cluster.to_vec(),
        threat_horizon: anchor_index
            .and_then(|index| horizons.get(index).copied())
            .unwrap_or(0),
        slots,
        facts,
        filled: OperationSlots::default(),
        unreachable_threats,
        reachable_threats,
        unavoidable_reinforcements: Vec::new(),
        reinforcement_contingencies: Vec::new(),
        contingency_reserve_funds: 0,
    };
    let assessment =
        enemy_reinforcement_assessment(scan, ctx, &operation, operation.threat_horizon.max(1));
    operation.unavoidable_reinforcements = assessment.unavoidable;
    operation.reinforcement_contingencies = assessment.contingencies;
    operation.contingency_reserve_funds =
        contingency_reserve_now(&operation.reinforcement_contingencies, scan.my_income);
    if !operation.unavoidable_reinforcements.is_empty() {
        operation.slots.combat_plan_required = 1;
    }
    operation
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
        // 作戦に接続されない汎用戦闘生産は行わない。戦力が必要なら、兵站確保・
        // 防衛・首都攻略のいずれかを先にOperationとして成立させる。
        return (Vec::new(), plan_trace);
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
            operation_priority_rank(op),
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
            enemy_combat_units: op.facts.enemy_combat_units,
            enemy_reinforcement_funds: op.facts.enemy_reinforcement_funds,
            contingency_reserve_funds: op.contingency_reserve_funds,
            reinforcement_contingencies: op
                .reinforcement_contingencies
                .iter()
                .map(|contingency| ReinforcementContingencyTrace {
                    enemy_type: contingency.enemy_type,
                    enemy_contact_turn: contingency.enemy_contact_turn,
                    counter_type: contingency.counter_type,
                    counter_facility: contingency.counter_facility,
                    counter_build_turn: contingency.counter_build_turn,
                    counter_contact_turn: contingency.counter_contact_turn,
                    attacks_required: contingency.attacks_required,
                    reserve_cost: contingency.reserve_cost,
                })
                .collect(),
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
    let mut facility_owners: HashMap<GridPosition, u32> = HashMap::new();
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
        // 最も不足している枠を持つ作戦から順に見ていく
        let Some((op_index, slot_kind)) = most_starved_slot(&operations) else {
            break;
        };

        // 先に並ぶ作戦の観測後counterを発動できる現金は、下位作戦へ流さない。
        // 自作戦の予約は現在の確定脅威を処理するために使ってよい。
        let higher_priority_contingency_reserve = operations[..op_index]
            .iter()
            .map(|operation| operation.contingency_reserve_funds)
            .fold(0_u32, u32::saturating_add);
        let spendable_funds = remaining_funds.saturating_sub(higher_priority_contingency_reserve);
        // 1 枠あたり予算。高価なユニットで枠を食い潰さないためのソフト上限。
        let per_slot_budget = spendable_funds / free_slots as u32;

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
            let continuation = plan_registry.continuation_for_operation(
                player_id,
                turn,
                operation_kind,
                operations[op_index].island_id,
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
                spendable_funds,
                !allow_structural_slots,
            );
            if let Some(input) = rolling_input
                && let Some(candidate_plan) = plan_force_package(&input)
            {
                let evaluated_continuation = continuation.map(|previous| {
                    let evaluated = evaluate_fixed_package(&input, &previous.purchases);
                    (previous, evaluated)
                });
                let conflicted_facilities = evaluated_continuation
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
                        let mut conflicts = HashSet::new();
                        for purchase in due {
                            let claimed_by_higher_priority = facility_owners
                                .get(&purchase.facility)
                                .is_some_and(|owner| {
                                    *owner < operation_priority_rank(&operations[op_index])
                                });
                            let claimed_by_campaign = scan
                                .production_facilities
                                .iter()
                                .any(|(facility, _)| *facility == purchase.facility)
                                && !scan
                                    .free_facilities
                                    .iter()
                                    .any(|(facility, _)| *facility == purchase.facility);
                            if claimed_by_higher_priority || claimed_by_campaign {
                                conflicts.insert(purchase.facility);
                            } else if operation_kind == OperationKind::AssaultCapital {
                                if purchase.cost <= affordable_funds {
                                    affordable_funds =
                                        affordable_funds.saturating_sub(purchase.cost);
                                } else {
                                    // 上位作戦が当手番の現金を使った場合も、失敗ではなく
                                    // 当該施設の首都編成列を次手番以降へ繰り下げる。
                                    conflicts.insert(purchase.facility);
                                }
                            }
                        }
                        conflicts
                    })
                    .unwrap_or_default();
                let selected = plan_registry.select_for_operation(
                    player_id,
                    turn,
                    operation_kind,
                    operations[op_index].island_id,
                    operation_anchor,
                    operations[op_index].objective_properties.clone(),
                    target_enemies,
                    operations[op_index].execution_authorized,
                    evaluated_continuation,
                    candidate_plan,
                    input.hard_deadline,
                    conflicted_facilities,
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
                        protected_unit_count: plan.protected_unit_count,
                        protected_survivor_count: plan.protected_survivor_count,
                        required_capture_survivor_count: plan.required_capture_survivor_count,
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
                            && purchase.cost <= spendable_funds
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
                    remaining_funds: spendable_funds,
                    per_slot_budget,
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
                spendable_funds,
                candidate.cost,
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
        facility_owners.insert(
            candidate.facility,
            operation_priority_rank(&operations[op_index]),
        );
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
        // Combatは同じパッケージの未使用current purchaseを次の反復で選ぶ。
        // 全て消費した後は候補なしとなり、この手番のCombat枠を完了する。
        if slot_kind != SlotKind::Combat {
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
    let contingency_reserve = operations
        .iter()
        .map(|operation| operation.contingency_reserve_funds)
        .fold(0_u32, u32::saturating_add);
    plan_trace.reserved_funds = remaining_funds.min(
        plan_registry
            .reserved_purchase_cost(player_id)
            .saturating_add(contingency_reserve),
    );
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
#[derive(Debug, Default)]
struct ReinforcementAssessment {
    unavoidable: Vec<EnemyPlanUnit>,
    contingencies: Vec<ReinforcementContingency>,
}

/// 敵増援ごとに接触turnと、観測後に生産する最速counterの接触turnを比較する。
/// counterが間に合う仮説は現在の撃破対象へ混ぜず、条件付き予約として保持する。
fn enemy_reinforcement_assessment(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    horizon: u32,
) -> ReinforcementAssessment {
    let scenario_budget = op.facts.enemy_reinforcement_funds;
    if scenario_budget == 0 {
        return ReinforcementAssessment::default();
    }
    let mut budget = 0_u32;
    let mut funded = 0_u32;
    let mut assessment = ReinforcementAssessment::default();
    let mut reserved_counter_slots = HashSet::new();
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
            let reinforcement = EnemyPlanUnit {
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
            };
            if let Some(contingency) = fastest_observed_counter(
                scan,
                ctx,
                op.anchor,
                stats,
                build_turn,
                available_turn,
                &reserved_counter_slots,
            ) {
                reserved_counter_slots
                    .insert((contingency.counter_build_turn, contingency.counter_facility));
                assessment.contingencies.push(contingency);
            } else {
                assessment.unavoidable.push(reinforcement);
            }
        }
    }
    assessment
}

/// 観測した敵1体へ、生産slot・移動・与ダメージを満たす最速counterを返す。
fn fastest_observed_counter(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    anchor: GridPosition,
    enemy: &UnitStats,
    counter_build_turn: u32,
    enemy_contact_turn: u32,
    reserved_slots: &HashSet<(u32, GridPosition)>,
) -> Option<ReinforcementContingency> {
    scan.production_facilities
        .iter()
        .flat_map(|(facility, terrain)| {
            scan.available_types
                .iter()
                .filter(move |(unit_type, stats)| {
                    !stats.can_capture
                        && stats.max_cargo == 0
                        && scan.can_produce(*terrain, *unit_type)
                })
                .map(move |(_, stats)| (*facility, stats))
        })
        .filter(|(facility, stats)| {
            !reserved_slots.contains(&(counter_build_turn, *facility))
                && best_damage(&scan.damage_chart, stats.unit_type, enemy.unit_type) > 0
                && ctx.is_reachable(
                    &scan.map,
                    &scan.master_data,
                    (facility.x, facility.y),
                    (anchor.x, anchor.y),
                    stats.movement_type,
                )
        })
        .filter_map(|(facility, stats)| {
            let damage = best_damage(&scan.damage_chart, stats.unit_type, enemy.unit_type);
            let attacks_required = 100_u32.div_ceil(damage.max(1));
            let counter_contact_turn = counter_build_turn
                .saturating_add(1)
                .saturating_add(eta_turns(&scan.map, &facility, &anchor, stats.max_movement));
            (counter_contact_turn <= enemy_contact_turn).then_some((
                counter_contact_turn,
                attacks_required,
                stats.cost,
                facility,
                stats,
            ))
        })
        .min_by_key(|(contact, attacks, cost, facility, _)| {
            (*contact, *attacks, *cost, facility.y, facility.x)
        })
        .map(
            |(counter_contact_turn, attacks_required, reserve_cost, facility, stats)| {
                ReinforcementContingency {
                    enemy_type: enemy.unit_type,
                    enemy_contact_turn,
                    counter_type: stats.unit_type,
                    counter_facility: facility,
                    counter_build_turn,
                    counter_contact_turn,
                    attacks_required,
                    reserve_cost,
                }
            },
        )
}

/// 将来収入で賄えない累積counter費用だけを、現在残高から予約する。
fn contingency_reserve_now(
    contingencies: &[ReinforcementContingency],
    income_per_turn: u32,
) -> u32 {
    let mut due_by_turn = HashMap::<u32, u32>::new();
    for contingency in contingencies {
        due_by_turn
            .entry(contingency.counter_build_turn)
            .and_modify(|cost| *cost = cost.saturating_add(contingency.reserve_cost))
            .or_insert(contingency.reserve_cost);
    }
    let mut turns = due_by_turn.keys().copied().collect::<Vec<_>>();
    turns.sort_unstable();
    let mut cumulative_cost = 0_u32;
    turns.into_iter().fold(0_u32, |required_now, turn| {
        cumulative_cost = cumulative_cost.saturating_add(due_by_turn[&turn]);
        required_now.max(cumulative_cost.saturating_sub(income_per_turn.saturating_mul(turn)))
    })
}

/// 観測敵と到着しうる増援を排除できる混成生産列を、探索期間の全生産slotから計画する。
///
/// `combat_plan_required`は呼び出し条件にだけ残し、候補数・生産停止・完了判定には使わない。
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
    let campaign_objective = scan.campaign_objectives.iter().find(|objective| {
        objective.anchor == op.anchor || op.objective_properties.contains(&objective.anchor)
    });
    let capture_completion_turn = campaign_objective.and_then(|objective| objective.capture_eta);
    let required_capture_survivors = campaign_objective.map_or_else(
        || {
            op.objective_properties
                .iter()
                .filter(|property| scan.open_properties.contains(property))
                .count()
        },
        |objective| objective.required_capture_survivors,
    );
    let planning_horizon = hard_deadline.unwrap_or(DEFAULT_SEARCH_TURNS).max(1);
    let mut enemies = enemies;
    enemies.extend(op.unavoidable_reinforcements.iter().cloned());
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
                    && ctx.can_reach_engagement_envelope(
                        &scan.map,
                        &scan.master_data,
                        (unit.pos.x, unit.pos.y),
                        (enemy.position.x, enemy.position.y),
                        unit.stats.movement_type,
                        unit.stats.max_range,
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
    let protected_units = scan
        .my_units
        .iter()
        .filter(|unit| {
            unit.entity
                .is_some_and(|entity| op.protected_capture_entities.contains(&entity))
        })
        .map(|unit| FriendlyPlanUnit {
            stats: unit.stats.clone(),
            position: unit.pos,
            hp: unit.hp,
            available_turn: 0,
            // 保護対象は攻撃要員として二重計上しないため空にする。
            engageable_enemy_indices: Vec::new(),
        })
        .collect();

    let mut options = production_options(
        &scan.free_facilities,
        &scan.production_facilities,
        &scan.available_types,
        &scan.master_data,
        planning_horizon,
        |facility, stats| {
            let can_engage = enemies.iter().any(|enemy| {
                best_damage(&scan.damage_chart, stats.unit_type, enemy.stats.unit_type) > 0
                    && ctx.can_reach_engagement_envelope(
                        &scan.map,
                        &scan.master_data,
                        (facility.x, facility.y),
                        (enemy.position.x, enemy.position.y),
                        stats.movement_type,
                        stats.max_range,
                    )
            });
            if require_self_deployment {
                return can_engage;
            }
            can_engage
                || can_join_operation(
                    scan,
                    ctx,
                    &op.anchor,
                    op.facts.requires_transport,
                    &facility,
                    stats,
                )
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
                    && ctx.can_reach_engagement_envelope(
                        &scan.map,
                        &scan.master_data,
                        (option.purchase.facility.x, option.purchase.facility.y),
                        (enemy.position.x, enemy.position.y),
                        option.stats.movement_type,
                        option.stats.max_range,
                    ))
                .then_some(index)
            })
            .collect();
    }

    Some(RollingPlanInput {
        map: scan.map.clone(),
        damage_chart: scan.damage_chart.clone(),
        existing_units,
        protected_units,
        enemies,
        production_options: options,
        current_funds: remaining_funds,
        income_per_turn: scan.my_income,
        hard_deadline,
        capture_completion_turn,
        required_capture_survivors,
        delay_cost_per_turn: op.facts.target_property_count.max(1).saturating_mul(1_000),
    })
}

/// 生産された戦闘Entityへ、実HPと与ダメージから撃破順を与える。
///
/// 価格は敵の硬さでも攻撃能力でもないため使わない。占領・輸送能力を持つ敵を先にし、
/// 同分類では必要攻撃回数が少ない敵から集中撃破する。
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
            // Combatは標的位置ではなく射撃圏へ自力展開する。艦船を陸上anchorへ
            // 仮置きすると到達判定が再び失敗するため、生産施設を常に起点とする。
            let origin = candidate.facility;
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
            let damage = best_damage(&scan.damage_chart, stats.unit_type, threat.stats.unit_type);
            let attacks_to_destroy = threat.current_hp.div_ceil(damage.max(1));
            let strategic_class = if threat.stats.can_capture {
                0
            } else if threat.stats.max_cargo > 0 {
                1
            } else {
                2
            };
            (damage > 0 && threat.current_hp > 0).then_some((
                strategic_class,
                attacks_to_destroy,
                threat.position.x,
                threat.position.y,
                entity.to_bits(),
                entity,
            ))
        })
        .collect::<Vec<_>>();
    targets.sort_unstable_by_key(|target| (target.0, target.1, target.2, target.3, target.4));
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

/// 生産枠の実効優先度。すべて同じ作戦集合で比較し、別枠の緊急作戦は作らない。
/// Defenseは敵接触ETAと展開リードタイムから優先度を上げるが、必要枠だけを取得する。
fn operation_priority_rank(operation: &Operation) -> u32 {
    let defense_cannot_wait = operation.kind == OperationKind::Defense
        && operation.facts.enemy_combat_units > 0
        && operation.facts.enemy_contact_eta
            <= operation.facts.deploy_lead_time.max(1).saturating_add(1);
    match (
        operation.kind,
        operation.execution_authorized,
        defense_cannot_wait,
    ) {
        (OperationKind::Defense, _, true) => 0,
        (OperationKind::AssaultCapital, true, _) => 1,
        (OperationKind::Defense, _, false) => 2,
        (OperationKind::Capture, _, _) => 3,
        (OperationKind::AssaultCapital, false, _) => 4,
    }
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
                SlotTier::Prerequisite => (operation_priority_rank(op), priority, -deficit),
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
        SlotKind::Intercept => op.slots.intercept_units = 0,
        SlotKind::Transport => op.slots.transport_slots = 0,
        SlotKind::Capture => op.slots.capture_units = 0,
        SlotKind::Combat => {
            op.slots.combat_plan_required = 0;
        }
    }
}

/// 購入した 1 体分を充足量へ反映する。
fn record_fill(
    scan: &BoardScan,
    _ctx: &mut ReachCtx,
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
            op.filled.intercept_units = op.filled.intercept_units.saturating_add(1);
        }
        SlotKind::Transport => {
            op.filled.transport_slots = op.filled.transport_slots.saturating_add(cargo.max(1))
        }
        SlotKind::Capture => op.filled.capture_units += 1,
        SlotKind::Combat => unreachable!("Combat購入はRollingPlan経路だけで処理する"),
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
            ctx.can_reach_engagement_envelope(
                &scan.map,
                &scan.master_data,
                (origin.x, origin.y),
                (threat.position.x, threat.position.y),
                stats.movement_type,
                stats.max_range,
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
        .any(|index| best_damage(chart, unit.unit_type, threats[*index].stats.unit_type) > 0)
}

/// 構造枠・迎撃枠の候補を、実能力と購入費のROIで比較する。
/// Combatはこの関数を通らず、RollingPlanが具体的な戦闘scheduleを比較する。
fn normalized_candidate_fitness(
    kind: SlotKind,
    raw_fitness: f32,
    cost: u32,
    per_slot_budget: u32,
) -> f32 {
    let _ = (kind, per_slot_budget);
    let opportunity_cost = cost.max(1);
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
            let Some(fitness) = slot_fitness(scan, ctx, op, kind, facility, stats) else {
                continue;
            };
            if stats.cost == 0 || stats.cost > constraints.remaining_funds {
                continue;
            }
            // CombatはRollingPlan専用で、この候補選定へ入らない。残る枠はいずれも
            // 体数・輸送容量なので、同じ実能力なら安い方を選ぶ。
            let count_denominated = true;
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
            // 価格ではなく、次の一撃で実際に削れるHPを適合度にする。
            let value = op
                .unreachable_threats
                .iter()
                .map(|threat| {
                    best_damage(&scan.damage_chart, stats.unit_type, threat.stats.unit_type)
                        .min(threat.current_hp) as f32
                })
                .sum::<f32>();
            if value <= 0.0 { None } else { Some(value) }
        }
        SlotKind::Combat => None,
    }
}

/// 主武器・副武器のうち有効な方のダメージ。
fn best_damage(chart: &DamageChart, attacker: UnitType, defender: UnitType) -> u32 {
    chart.get_base_damage(attacker, defender).unwrap_or(0).max(
        chart
            .get_base_damage_secondary(attacker, defender)
            .unwrap_or(0),
    )
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
            let Some(fitness) = slot_fitness(scan, ctx, op, kind, facility, stats) else {
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
                slot_fitness(scan, ctx, op, kind, facility, stats)
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
            required_capture_survivors: 0,
            logistics_rank: Some(0),
            forced_target_enemies: HashSet::new(),
            protected_capture_entities: HashSet::new(),
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
            required_capture_survivors: 1,
            logistics_rank: Some(0),
            forced_target_enemies: HashSet::new(),
            protected_capture_entities: HashSet::new(),
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

    #[test]
    fn capital_objective_replaces_local_campaign_on_the_same_island() {
        let mut scan = multi_factory_scan();
        let local_anchor = pos(6, 2);
        let capital = pos(8, 2);
        let island_id = crate::ai::islands::IslandId(0);
        scan.campaign_objectives = vec![
            CampaignPlanningObjective {
                island_id,
                kind: OperationKind::Capture,
                anchor: local_anchor,
                capture_eta: Some(3),
                required_capture_survivors: 1,
                logistics_rank: Some(0),
                forced_target_enemies: HashSet::new(),
                protected_capture_entities: HashSet::new(),
                staging_anchor: local_anchor,
                execution_authorized: true,
            },
            CampaignPlanningObjective {
                island_id,
                kind: OperationKind::AssaultCapital,
                anchor: capital,
                capture_eta: None,
                required_capture_survivors: 0,
                logistics_rank: None,
                forced_target_enemies: HashSet::new(),
                protected_capture_entities: HashSet::new(),
                staging_anchor: local_anchor,
                execution_authorized: false,
            },
        ];

        let operations = build_operations(&scan, &mut ReachCtx::default(), &[]);
        let same_island = operations
            .iter()
            .filter(|operation| operation.island_id == Some(island_id))
            .collect::<Vec<_>>();
        assert_eq!(same_island.len(), 1);
        assert_eq!(same_island[0].kind, OperationKind::AssaultCapital);
        assert_eq!(same_island[0].anchor, capital);
    }

    #[test]
    fn capital_rescue_evicts_defense_instead_of_the_required_capture() {
        let scored = vec![
            (true, 1, OperationKind::Defense, vec![pos(0, 0)]),
            (true, 2, OperationKind::Defense, vec![pos(1, 0)]),
            (true, 3, OperationKind::Defense, vec![pos(2, 0)]),
            (true, 4, OperationKind::Capture, vec![pos(3, 0)]),
        ];

        let removed = required_operation_eviction_index(&scored, OperationKind::AssaultCapital);
        assert_eq!(scored[removed].2, OperationKind::Defense);
        assert_ne!(scored[removed].2, OperationKind::Capture);
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

        assert!(near.facts.friendly_combat_units_committed > 0);
        assert_eq!(far.facts.friendly_combat_units_committed, 0);
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
    fn enemy_reinforcement_funds_is_local_unique_and_deadline_bounded() {
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
            projected_enemy_reinforcement_funds(&scan, &mut ctx, &anchors, &horizons, 0),
            3000
        );
        assert_eq!(
            projected_enemy_reinforcement_funds(&scan, &mut ctx, &anchors, &horizons, 1),
            0
        );

        // 到着期限が0なら、この施設はどの作戦の脅威にもならない。
        assert_eq!(
            projected_enemy_reinforcement_funds(&scan, &mut ctx, &anchors, &[0, 0], 0),
            0
        );
    }

    #[test]
    fn unauthorized_capital_formation_does_not_hide_reinforcements_from_capture() {
        let mut scan = multi_factory_scan();
        scan.enemy_income = 6_000;
        scan.enemy_production_slots = 1;
        scan.enemy_facilities = vec![EnemyFacilitySnapshot {
            pos: pos(8, 2),
            terrain: Terrain::Factory,
        }];
        let capital = pos(8, 2);
        let capture = pos(6, 2);
        scan.campaign_objectives = vec![
            CampaignPlanningObjective {
                island_id: crate::ai::islands::IslandId(1),
                kind: OperationKind::AssaultCapital,
                anchor: capital,
                capture_eta: None,
                required_capture_survivors: 0,
                logistics_rank: None,
                forced_target_enemies: HashSet::new(),
                protected_capture_entities: HashSet::new(),
                staging_anchor: pos(0, 2),
                execution_authorized: false,
            },
            CampaignPlanningObjective {
                island_id: crate::ai::islands::IslandId(2),
                kind: OperationKind::Capture,
                anchor: capture,
                capture_eta: Some(4),
                required_capture_survivors: 3,
                logistics_rank: Some(0),
                forced_target_enemies: HashSet::new(),
                protected_capture_entities: HashSet::new(),
                staging_anchor: capture,
                execution_authorized: true,
            },
        ];
        let anchors = vec![capital, capture];
        let horizons = vec![12, 4];
        let mut ctx = ReachCtx::default();

        assert_eq!(
            projected_enemy_reinforcement_funds(&scan, &mut ctx, &anchors, &horizons, 0),
            0
        );
        assert!(projected_enemy_reinforcement_funds(&scan, &mut ctx, &anchors, &horizons, 1) > 0);
    }

    #[test]
    fn enemy_reinforcement_assessment_places_each_purchase_on_the_time_axis() {
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
        op.facts.enemy_reinforcement_funds = 3_000;

        let assessment = enemy_reinforcement_assessment(&scan, &mut ReachCtx::default(), &op, 5);
        let mut arrivals = assessment
            .unavoidable
            .iter()
            .map(|enemy| (enemy.available_turn, enemy.stats.unit_type))
            .chain(
                assessment
                    .contingencies
                    .iter()
                    .map(|plan| (plan.enemy_contact_turn, plan.enemy_type)),
            )
            .collect::<Vec<_>>();
        arrivals.sort_unstable_by_key(|(turn, _)| *turn);

        assert_eq!(arrivals.len(), 3);
        assert_eq!(
            arrivals.iter().map(|(turn, _)| *turn).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        assert!(
            arrivals
                .iter()
                .all(|(_, unit_type)| *unit_type == UnitType::Infantry)
        );
    }

    #[test]
    fn contingency_reserves_only_the_funding_gap_before_future_income() {
        let plans = vec![
            ReinforcementContingency {
                enemy_type: UnitType::Infantry,
                enemy_contact_turn: 3,
                counter_type: UnitType::Bcopters,
                counter_facility: pos(1, 1),
                counter_build_turn: 1,
                counter_contact_turn: 3,
                attacks_required: 2,
                reserve_cost: 7_500,
            },
            ReinforcementContingency {
                enemy_type: UnitType::Infantry,
                enemy_contact_turn: 4,
                counter_type: UnitType::Bcopters,
                counter_facility: pos(2, 1),
                counter_build_turn: 2,
                counter_contact_turn: 4,
                attacks_required: 2,
                reserve_cost: 7_500,
            },
        ];

        assert_eq!(contingency_reserve_now(&plans, 5_000), 5_000);
        assert_eq!(contingency_reserve_now(&plans, 8_000), 0);
    }

    /// テスト用のユニット諸元。
    fn stats(unit_type: UnitType, cost: u32) -> UnitStats {
        UnitStats {
            unit_type,
            cost,
            ..UnitStats::mock()
        }
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

    #[test]
    fn ranged_ship_can_join_land_assault_from_reachable_firing_envelope() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(None);
        let mut ctx = ReachCtx::default();

        assert!(!ctx.is_reachable(&map, &registry, (1, 1), (8, 1), MovementType::Ship,));
        assert!(ctx.can_reach_engagement_envelope(
            &map,
            &registry,
            (1, 1),
            (8, 1),
            MovementType::Ship,
            3,
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

    /// Combat計画は購入価格ではなく、期限内の具体的な攻撃列で充足する。
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

        // 65%攻撃を何回実行できるかをシミュレーションするため、価格に関係なく
        // 3目標を期限内に処理できる3機が編成される。
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
            island_id: None,
            anchor: pos(0, 0),
            staging_anchor: pos(0, 0),
            execution_authorized: true,
            protected_capture_entities: HashSet::new(),
            objective_properties: vec![pos(0, 0)],
            threat_horizon: 0,
            facts: OperationFacts::default(),
            slots,
            filled,
            unreachable_threats: Vec::new(),
            reachable_threats: Vec::new(),
            unavoidable_reinforcements: Vec::new(),
            reinforcement_contingencies: Vec::new(),
            contingency_reserve_funds: 0,
        }
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
            // 防衛作戦の戦闘計画（枠としては最後だが、作戦が最優先）。
            operation(
                OperationKind::Defense,
                OperationSlots {
                    combat_plan_required: 1,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
        ];

        assert_eq!(most_starved_slot(&ops), Some((1, SlotKind::Combat)));
    }

    /// 具体的な敵を持つ最優先防衛作戦は、後順位の輸送より先に計画する。
    #[test]
    fn a_top_priority_combat_plan_precedes_a_lower_priority_transport() {
        let ops = vec![
            operation(
                OperationKind::Defense,
                OperationSlots {
                    combat_plan_required: 1,
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

        assert_eq!(most_starved_slot(&ops), Some((0, SlotKind::Combat)));
    }

    #[test]
    fn authorized_capital_outranks_non_imminent_local_operations_only() {
        let mut capture = operation(
            OperationKind::Capture,
            OperationSlots {
                transport_slots: 1,
                ..OperationSlots::default()
            },
            OperationSlots::default(),
        );
        capture.execution_authorized = true;
        let mut capital = operation(
            OperationKind::AssaultCapital,
            OperationSlots {
                combat_plan_required: 1,
                ..OperationSlots::default()
            },
            OperationSlots::default(),
        );
        capital.execution_authorized = true;
        let mut distant_defense = operation(
            OperationKind::Defense,
            OperationSlots {
                combat_plan_required: 1,
                ..OperationSlots::default()
            },
            OperationSlots::default(),
        );
        distant_defense.facts.enemy_combat_units = 1;
        distant_defense.facts.enemy_contact_eta = 6;
        distant_defense.facts.deploy_lead_time = 1;

        let ops = vec![capture, distant_defense, capital];
        assert_eq!(most_starved_slot(&ops), Some((2, SlotKind::Combat)));
    }

    #[test]
    fn defense_urgency_raises_normal_operation_priority_without_deleting_capital() {
        let mut capital = operation(
            OperationKind::AssaultCapital,
            OperationSlots {
                combat_plan_required: 1,
                ..OperationSlots::default()
            },
            OperationSlots::default(),
        );
        capital.execution_authorized = true;
        let mut defense = operation(
            OperationKind::Defense,
            OperationSlots {
                combat_plan_required: 1,
                ..OperationSlots::default()
            },
            OperationSlots::default(),
        );
        defense.facts.enemy_combat_units = 1;
        defense.facts.enemy_contact_eta = 1;
        defense.facts.deploy_lead_time = 2;

        let ops = vec![capital, defense];
        assert_eq!(operation_priority_rank(&ops[0]), 1);
        assert_eq!(operation_priority_rank(&ops[1]), 0);
        assert_eq!(most_starved_slot(&ops), Some((1, SlotKind::Combat)));
        assert_eq!(ops[0].slots.combat_plan_required, 1);
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
            combat_units: 0,
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
