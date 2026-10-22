//! 生産判断の公開入口と手番の観測・確定。

use crate::ai::v4::*;

#[cfg(test)]
mod tests;

/// 発行待ちの列から、発行直前の実盤面検証を通る1命令だけを返してIssuedへ進める。
///
/// 計画後に資金・施設が変わって検証を通らない購入案は発行しない（未発行の取消）。
/// 取り消した案の配備・占領意図は同じ手番に解放し、永続stepはPlannedのまま残して
/// 次手番の継続判定が生産失敗として扱って再計画する。発行した案だけがIssuedになり、
/// 完了イベントとの照合を待つ。
pub(in crate::ai::v4) fn issue_next_planned_production(
    world: &mut World,
    turn: u32,
    queue: &mut VecDeque<PlannedProduction>,
) -> Vec<ProduceUnitCommand> {
    // 終局後は発行待ちの案を監査入口へ渡すだけで、Issuedへ進めない。
    if world
        .get_resource::<crate::resources::MatchState>()
        .is_some_and(|state| state.game_over.is_some())
    {
        return queue
            .pop_front()
            .map(|planned| vec![planned.command])
            .unwrap_or_default();
    }
    let (planned, cancelled) =
        commit::next_issuable(world, turn, queue, |planned| &planned.command);
    production_cancellation::release_cancelled_purchases(world, turn, &cancelled);
    let Some(planned) = planned else {
        return Vec::new();
    };
    if let PurchasePurpose::PlanStep(step_ref) = planned.purpose
        && let Some(mut registry) = world.get_resource_mut::<V4RollingPlanRegistry>()
    {
        registry.mark_issued(step_ref, turn);
    }
    vec![planned.command]
}

/// 発行待ちの列を手番資源から取り出し、次の1命令を発行してから戻す。
pub(in crate::ai::v4) fn issue_from_turn_plan(
    world: &mut World,
    turn: u32,
    mut turn_plan: V4ProductionTurnPlan,
) -> Vec<ProduceUnitCommand> {
    let issued = issue_next_planned_production(world, turn, &mut turn_plan.commands);
    world.insert_resource(turn_plan);
    issued
}

/// 当手番の最終購入案を確定する。
///
/// 同じ購入案の列から、配備意図（生産完了イベントとの照合用）・診断trace・
/// 発行待ちの列を生成する。永続Plan Registryはここで一度だけWorldへ戻す。
/// 通常経路と小規模拡張経路は、購入案の作り方だけが異なり、確定処理は共通。
#[allow(clippy::too_many_arguments)]
pub(in crate::ai::v4) fn commit_turn_production(
    world: &mut World,
    player_id: PlayerId,
    turn: u32,
    scan: &BoardScan,
    planned: Vec<PlannedProduction>,
    plan_trace: ProductionPlanTrace,
    rolling_registry: V4RollingPlanRegistry,
    mut turn_plan: V4ProductionTurnPlan,
) -> Vec<ProduceUnitCommand> {
    // 配分は開始時資金を超えて確保しない。超えていれば配分状態の更新漏れである。
    debug_assert!(
        planned
            .iter()
            .map(|planned| planned.cost)
            .fold(0_u32, u32::saturating_add)
            <= scan.funds,
        "購入案の合計費用が配分開始時の資金を超えている"
    );
    // 陸続き前線のCapture購入は、生産完了後にRoadmap NodeのCapture Squadへ
    // 受け入れるため、発注意図をCampaign実行registryへ追記する。
    let capture_intents = planned
        .iter()
        .filter_map(|planned| planned.capture_intent.clone())
        .collect::<Vec<_>>();
    if !capture_intents.is_empty() {
        world.init_resource::<campaign_execution::V4CampaignExecutionRegistry>();
        world
            .resource_mut::<campaign_execution::V4CampaignExecutionRegistry>()
            .append_turn_intents(player_id, turn, &capture_intents);
    }

    // 診断traceとは別に、生産完了イベントと照合する作戦意図を永続化する。
    let pending = planned
        .iter()
        .enumerate()
        .filter_map(|(order, planned)| {
            let deployment = planned.deployment.as_ref()?;
            debug_assert_eq!(deployment.slot_kind, planned.role);
            Some(deployment::PendingDeployment {
                forming_slot: None,
                operation_binding: deployment.operation_binding,
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
                slot_kind: planned.role,
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
    ProductionPolicy::for_current_path().register_deployment_orders(
        &mut deployment_registry,
        &rolling_registry,
        scan,
        player_id,
        turn,
        pending,
    );
    world.insert_resource(deployment_registry);
    world.insert_resource(rolling_registry);
    // 生産判断の内訳を診断リソースへ残す（判定は行わず記録のみ）。
    world
        .get_resource_or_insert_with(ProductionTraceDiagnostics::default)
        .record(turn, plan_trace);

    turn_plan.player_id = Some(player_id);
    turn_plan.turn = turn;
    turn_plan.commands = VecDeque::from(planned);
    issue_from_turn_plan(world, turn, turn_plan)
}

/// V4 の生産意思決定エントリポイント。
///
/// `decide_production` から `AiVersion::uses_operation_driven_production()` が
/// true のときだけ委譲される。V1/V2/V3 の経路には一切影響しない。
pub fn decide_production_v4(world: &mut World, player_id: PlayerId) -> Vec<ProduceUnitCommand> {
    let policy = ProductionPolicy::for_current_path();
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    // 島嶼キャンペーン生産だけでreturnする手番も含め、作戦の実績は毎ターン観測する。
    // 生産APIは施設ごとに呼ばれるため、同じ手番の2体目以降では再走査しない。
    let already_observed = world
        .get_resource::<V4ProductionObservationCache>()
        .is_some_and(|cache| cache.player_id == Some(player_id) && cache.turn == turn);
    if !already_observed {
        observe_plan_execution(world, player_id, turn);
        world.insert_resource(V4ProductionObservationCache {
            player_id: Some(player_id),
            turn,
        });
    }

    let campaign_surplus = match decide_campaign_production_v4(world, player_id, policy) {
        CampaignProductionControl::Command(command) => return vec![command],
        CampaignProductionControl::BlockGeneric => return Vec::new(),
        CampaignProductionControl::ContinueWithSurplus(budget) => Some(budget),
        CampaignProductionControl::Continue => None,
    };

    let turn_plan = world
        .remove_resource::<V4ProductionTurnPlan>()
        .unwrap_or_default();
    if turn_plan.player_id == Some(player_id) && turn_plan.turn == turn {
        return issue_from_turn_plan(world, turn, turn_plan);
    }

    let prepared = strategy_pipeline::prepare_production_scan(world, player_id);
    let queries = board_scan::BoardScanQueries::prepare(world);
    let Some(mut scan) = BoardScan::collect(world, player_id, prepared.as_ref(), &queries, policy)
    else {
        world.insert_resource(turn_plan);
        return Vec::new();
    };
    // 敵の初期配置は学習せず、前回走査後に現れた実Entityだけで次手番を予測する。
    let enemy_production_forecast = if policy.reports_property_control() {
        enemy_forecast::observe_enemy_production(world, player_id, turn, &scan)
    } else {
        EnemyProductionForecastTrace::default()
    };
    scan.enemy_production_forecast = enemy_production_forecast;
    if let Some(budget) = campaign_surplus {
        scan.funds = scan.funds.min(budget);
    }
    // 既存戦力は現在任務から読む。購入履歴のPlanIdを現在所属と混同しない。
    let committed_combat_assignments = policy.current_combat_assignments(world, player_id);
    // Squad計画でDAGへ束縛した前進部隊を、首都作戦の既存戦力として同じ手番の
    // 生産plannerへ渡す。PlanIdの有無ではなく、親戦役の未完了区間へ実際に進む
    // という作戦状態を正本にする。
    let advancing_route_entities = world
        .get_resource::<capital_route::model::CapitalRoutePathRegistry>()
        .zip(world.get_resource::<crate::ai::squad::SquadManager>())
        .map(|(registry, manager)| registry.advancing_combat_entities(player_id, manager))
        .unwrap_or_default();
    let produced_plan_steps = world
        .get_resource::<deployment::V4DeploymentRegistry>()
        .map(|registry| registry.produced_plan_steps(player_id))
        .unwrap_or_default();
    let mut rolling_registry = world
        .remove_resource::<V4RollingPlanRegistry>()
        .unwrap_or_default();
    let mut contract_registry = if policy.uses_joint_capture() {
        world
            .remove_resource::<plan_contract::PlanContractRegistry>()
            .unwrap_or_default()
    } else {
        plan_contract::PlanContractRegistry::default()
    };
    rolling_registry.reconcile_produced_steps(player_id, &produced_plan_steps);
    let input = production_pipeline::ProductionInputs {
        policy,
        scan: &scan,
        player_id,
        allow_structural_slots: campaign_surplus.is_none(),
        committed_combat_assignments: &committed_combat_assignments,
        advancing_route_entities: &advancing_route_entities,
        turn,
    };
    let (planned, mut plan_trace) = production_pipeline::plan_production_with_registry(
        &input,
        &mut rolling_registry,
        &mut contract_registry,
    );
    plan_trace.enemy_production_forecast = enemy_production_forecast;
    if policy.uses_joint_capture() {
        world.insert_resource(contract_registry);
    }
    commit_turn_production(
        world,
        player_id,
        turn,
        &scan,
        planned,
        plan_trace,
        rolling_registry,
        turn_plan,
    )
}

/// 生産計画を実Entityの戦闘・損耗・目標HP・拠点占領へ接続する予実集計。
///
/// 診断用の金額はUnitTypeの生産費と現在HPから導出する。計画変更の判定自体は
/// 金額価値ではなく、初攻撃・敵排除・拠点占領の予定手番との差を使用する。
pub(in crate::ai::v4) fn observe_plan_execution(world: &mut World, player_id: PlayerId, turn: u32) {
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

    let mut owned_properties = HashSet::default();
    let mut property_query = world.query::<(&GridPosition, &Property)>();
    for (position, property) in property_query.iter(world) {
        if property.owner_id == Some(player_id) {
            owned_properties.insert(*position);
        }
    }

    // 搭載中の敵も増援・残存目標として追うため、盤面走査の除外条件を使わない。
    let mut enemy_health = HashMap::default();
    let mut enemy_query = world.query::<(Entity, &Faction, &Health)>();
    for (entity, faction, health) in enemy_query.iter(world) {
        if faction.0 != player_id && health.current > 0 {
            enemy_health.insert(entity, health.current);
        }
    }

    let current_missions = match ProductionPolicy::for_current_path() {
        ProductionPolicy::Standard => mission_input::active_entities(world, player_id),
        ProductionPolicy::SmallMap => HashSet::default(),
    };
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
                // 購入予実の帰属は不変。現在任務中かどうかだけをSquad正本から観測する。
                mission_active: alive
                    && (current_missions.contains(&record.entity)
                        || (record.active && record.squad_id.is_some())),
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
/// `AdoptedTurnPortfolio` は行動計画時に同じプレイヤーのportfolioを保持している。
/// 生産APIは1命令ごとに呼ばれるため、V3と同じcache queueへ完全パッケージを保存し、
/// shortfallの再計算による二重発注を防ぐ。高優先行を完成できないときはgenericを
/// blockし、輸送・積荷・戦闘戦力の一部だけを逐次投入しない。
pub(in crate::ai::v4) fn decide_campaign_production_v4(
    world: &mut World,
    player_id: PlayerId,
    policy: ProductionPolicy,
) -> CampaignProductionControl {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let plan_exists = world
        .get_resource::<ProductionTurnState>()
        .is_some_and(|cache| cache.campaign_production_planned(player_id));
    if plan_exists {
        let mut cache = world
            .remove_resource::<ProductionTurnState>()
            .unwrap_or_default();
        let next = take_campaign_command(world, &mut cache, player_id);
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
        .get_resource::<AdoptedTurnPortfolio>()
        .and_then(|cache| cache.campaign_portfolio(player_id))
        .map(|portfolio| portfolio.aggregate_missing_requirements())
        .unwrap_or_default();
    if shortfalls.is_empty() {
        return CampaignProductionControl::Continue;
    }

    let prepared = strategy_pipeline::prepare_production_scan(world, player_id);
    let queries = board_scan::BoardScanQueries::prepare(world);
    let Some(scan) = BoardScan::collect(world, player_id, prepared.as_ref(), &queries, policy)
    else {
        return CampaignProductionControl::BlockGeneric;
    };

    // 陸続きで自力到達できる前線まで島campaign専用生産へ通すと、占領枠を
    // 全て買い終えるまで時系列Combat Planが起動せず、裸の占領兵を逐次損耗させる。
    // 専用経路が担当するのは輸送工程を必要とする前線だけとし、陸続きの前線は
    // 同一Operation内でCaptureとCombatを混成して計画する。目標一覧そのものは
    // campaign/roadmapに残るため、単一目標へ縮退することはない。
    let mut reach = ReachCtx::default();
    shortfalls.retain(|shortfall| !has_direct_capture_route(&scan, &mut reach, shortfall));
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
    let reservation = if policy.uses_route_commitments() {
        campaign_execution::CampaignFormingReservation::Immediate
    } else {
        campaign_execution::CampaignFormingReservation::Deferred
    };
    world.init_resource::<campaign_execution::V4CampaignExecutionRegistry>();
    world
        .resource_mut::<campaign_execution::V4CampaignExecutionRegistry>()
        .replace_turn_intents(player_id, turn, &outcome.intents, reservation);
    let generic_budget = outcome.generic_funds;
    let mut cache = world
        .remove_resource::<ProductionTurnState>()
        .unwrap_or_default();
    cache.set_campaign_production_plan_with_generic_budget(
        player_id,
        turn,
        outcome.commands,
        generic_budget,
    );
    let next = take_campaign_command(world, &mut cache, player_id);
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

/// campaign専用列から発行する命令を取り出し、取り消した命令の発注記録を同じ手番に解放する。
fn take_campaign_command(
    world: &mut World,
    cache: &mut ProductionTurnState,
    player_id: PlayerId,
) -> Option<ProduceUnitCommand> {
    let issue = cache.take_issuable_campaign_command(world, player_id);
    production_cancellation::release_cancelled_campaign_commands(
        world,
        player_id,
        issue.planned_turn,
        &issue.cancelled,
    );
    issue.command
}

/// 生産施設から占領可能unitが対象へ自力到達できるかを判定する。
///
/// 「島か大陸か」ではなく実際の移動可能地形で判定するため、島map内の陸続き前線も
/// 単一大陸mapも同じOperation生産へ入り、海峡を越える前線だけ輸送工程へ残る。
pub(in crate::ai::v4) fn has_direct_capture_route(
    scan: &BoardScan,
    reach: &mut ReachCtx,
    shortfall: &crate::ai::island_campaign::IslandCampaignShortfall,
) -> bool {
    scan.production_facilities
        .iter()
        .any(|(facility, terrain)| {
            scan.available_types.iter().any(|(unit_type, stats)| {
                stats.can_capture
                    && scan.can_produce(*terrain, *unit_type)
                    && reach.is_reachable(
                        &scan.map,
                        &scan.master_data,
                        (facility.x, facility.y),
                        (shortfall.target_position.x, shortfall.target_position.y),
                        stats.movement_type,
                    )
            })
        })
}

pub(in crate::ai::v4) fn mark_campaign_production_issued(
    world: &mut World,
    player_id: PlayerId,
    turn: u32,
    command: &ProduceUnitCommand,
) {
    // 終局後の監査用出力は実際の発行として台帳へ記録しない。
    if world
        .get_resource::<crate::resources::MatchState>()
        .is_some_and(|state| state.game_over.is_some())
    {
        return;
    }

    if let Some(mut registry) =
        world.get_resource_mut::<campaign_execution::V4CampaignExecutionRegistry>()
    {
        registry.mark_issued(player_id, turn, command);
    }
}
