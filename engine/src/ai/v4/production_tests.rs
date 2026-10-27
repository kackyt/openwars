use super::production_entry::has_direct_capture_route;
use crate::ai::v4::capital_route::commitment::{
    capital_route_frontline_sort_key, inherited_capital_route_commitment,
    refresh_capital_route_path_commitments,
};
use crate::ai::v4::capital_route::model::{
    CapitalRouteCommitment, CapitalRouteExecutionPhase, CapitalRouteId, CapitalRouteNodeObjective,
    CapitalRouteNodeScope, CapitalRoutePathRegistry,
};
use crate::ai::v4::capital_route::prepare_capital_route_topologies;
use crate::ai::v4::enemy_forecast::{
    EnemyReinforcementEnvelope, ReinforcementScenario, projected_enemy_reinforcement_envelope,
};
use crate::ai::v4::production_pipeline::test_exports::{
    ImmediateCombatEngagement, ImmediateCombatOption, build_operation, build_operations,
    combat_plan_input, committed_entities_for_operation,
    defer_unapproved_capital_structure_for_active_segment, enemy_reinforcement_assessment,
    immediate_combat_options, mobilization_role, select_immediate_combat_reinforcement,
};
use crate::ai::v4::property_control::PlannedCaptureUnit;
use crate::ai::v4::property_control::{
    PropertyControlApproach, PropertyControlObjective, enemy_is_capturing,
    select_property_capture_candidate,
};
use crate::ai::v4::test_support::*;
use crate::ai::v4::*;
use crate::resources::GridTopology;

#[test]
fn forward_stage_squad_outranks_rear_advance_squad_for_frontline_rotation() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let forward_stage =
        capital_route_frontline_sort_key(12, &MissionType::Attack, 0, 1_000, SquadId(200));
    let rear_advance =
        capital_route_frontline_sort_key(3, &MissionType::Attack, 0, 20_000, SquadId(1));

    assert!(
        forward_stage < rear_advance,
        "前線到達済みの予備を古い後方Squadより先にAdvanceへ昇格させる"
    );
}

#[test]
fn stable_operation_reference_preserves_order_without_member_inheritance() {
    use crate::ai::squad::SquadOperationId;
    let player = PlayerId(1);
    let island = crate::ai::islands::IslandId(0);
    let target = pos(6, 1);
    let commitment = CapitalRouteCommitment {
        player_id: player,
        island_id: island,
        route: CapitalRouteId(0),
        target,
        target_anchor: target,
        objective: CapitalRouteNodeObjective::Control,
        scope: CapitalRouteNodeScope::Point,
        control_area: vec![target],
        capture_targets: Vec::new(),
        crossing_exit: None,
        crossed: false,
        execution_target: target,
        frontline_capacity: 1,
        path: vec![pos(1, 1), target],
        phase: CapitalRouteExecutionPhase::Advance,
    };
    let logical = SquadOperationId(4);
    let mut registry = CapitalRoutePathRegistry::default();
    registry.commitments.insert(SquadId(4), commitment);
    registry.squads_by_operation.insert(logical, SquadId(4));
    let mut manager = crate::ai::squad::SquadManager::new();
    let mut old = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
    old.operation_id = logical;
    old.members.insert(Entity::from_raw(100));
    let mut relocated = old.clone();
    relocated.id = SquadId(9);
    relocated.members.insert(Entity::from_raw(101));
    assert_eq!(
        inherited_capital_route_commitment(&registry, relocated.operation_id)
            .map(|order| order.target_anchor),
        Some(target)
    );
    let mut unrelated = relocated.clone();
    unrelated.operation_id = SquadOperationId(10);
    assert!(
        inherited_capital_route_commitment(&registry, unrelated.operation_id).is_none(),
        "memberが完全一致しても別作戦へ古いDAG指令を復活させない"
    );
}

#[test]
fn explicit_combat_deployment_is_not_reassigned_to_a_capture_milestone() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let data = MasterDataRegistry::load().unwrap();
    let player = PlayerId(1);
    let mut map = flat_map(12, 1);
    map.set_terrain(0, 0, Terrain::Capital).unwrap();
    map.set_terrain(11, 0, Terrain::Capital).unwrap();
    map.set_terrain(3, 0, Terrain::City).unwrap();
    let islands = crate::ai::islands::IslandMap::analyze(&map);
    let island_id = islands.get_island_at(&pos(0, 0)).unwrap().id;
    let mut world = World::new();
    world.insert_resource(map);
    world.insert_resource(islands);
    world.insert_resource(data);
    world.spawn((
        pos(0, 0),
        Property::new(Terrain::Capital, Some(player), 100),
    ));
    world.spawn((
        pos(11, 0),
        Property::new(Terrain::Capital, Some(PlayerId(2)), 100),
    ));
    world.spawn((pos(3, 0), Property::new(Terrain::City, None, 100)));
    let attacker = world
        .spawn((
            pos(0, 0),
            Faction(player),
            Health {
                current: 100,
                max: 100,
            },
            UnitStats {
                movement_type: MovementType::Tank,
                max_movement: 6,
                cost: 5000,
                ..UnitStats::mock()
            },
        ))
        .id();
    let enemy = world
        .spawn((
            pos(9, 0),
            Faction(PlayerId(2)),
            Health {
                current: 100,
                max: 100,
            },
            UnitStats {
                movement_type: MovementType::Infantry,
                can_capture: true,
                ..UnitStats::mock()
            },
        ))
        .id();
    let mut deployment = deployment::V4DeploymentRegistry::default();
    deployment.assign_target_for_test(player, attacker, enemy);
    world.insert_resource(deployment);
    let mut manager = crate::ai::squad::SquadManager::new();
    let mut squad = manager.create_owned_squad(MissionType::Attack, player);
    squad.members.insert(attacker);
    squad.plan.target = Some(pos(9, 0));
    squad.plan.target_island = Some(island_id);
    let squad_id = squad.id;
    prepare_capital_route_topologies(&mut world, player);
    refresh_capital_route_path_commitments(&mut world, player, &mut manager);
    assert!(
        !world
            .resource::<CapitalRoutePathRegistry>()
            .commitments
            .contains_key(&squad_id),
        "実在敵を指定した購入任務を、手前の未占領都市への別命令で上書きしない"
    );
    world.remove_resource::<deployment::V4DeploymentRegistry>();
    refresh_capital_route_path_commitments(&mut world, player, &mut manager);
    assert!(
        world
            .resource::<CapitalRoutePathRegistry>()
            .commitments
            .contains_key(&squad_id),
        "専用任務がなくなった汎用戦闘部隊はDAGへ参加できる"
    );
}

#[test]
fn campaign_objective_is_the_only_capture_source_and_keeps_exact_anchor() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut scan = multi_factory_scan();
    let campaign_anchor = pos(8, 2);
    scan.campaign_objectives = vec![CampaignPlanningObjective {
        island_id: crate::ai::islands::IslandId(0),
        kind: OperationKind::Capture,
        anchor: campaign_anchor,
        objective_properties: vec![campaign_anchor],
        capture_eta: Some(3),
        required_capture_survivors: 1,
        logistics_rank: Some(0),
        forced_target_enemies: HashSet::default(),
        protected_capture_entities: HashSet::default(),
        capture_replacement_pending: false,
        staging_anchor: campaign_anchor,
        execution_authorized: true,
    }];

    let operations = build_operations(
        &scan,
        &mut ReachCtx::default(),
        &[],
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );
    let captures = operations
        .iter()
        .filter(|operation| operation.kind == OperationKind::Capture)
        .collect::<Vec<_>>();

    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].anchor, campaign_anchor);
}

#[test]
fn capital_objective_keeps_local_front_as_an_independent_operation() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut scan = multi_factory_scan();
    let local_anchor = pos(6, 2);
    let capital = pos(8, 2);
    let second_front = pos(7, 1);
    let island_id = crate::ai::islands::IslandId(0);
    scan.open_properties = vec![local_anchor, second_front, capital];
    scan.campaign_objectives = vec![
        CampaignPlanningObjective {
            island_id,
            kind: OperationKind::Capture,
            anchor: local_anchor,
            objective_properties: vec![local_anchor, second_front],
            capture_eta: Some(3),
            required_capture_survivors: 1,
            logistics_rank: Some(0),
            forced_target_enemies: HashSet::default(),
            protected_capture_entities: HashSet::default(),
            capture_replacement_pending: false,
            staging_anchor: local_anchor,
            execution_authorized: true,
        },
        CampaignPlanningObjective {
            island_id,
            kind: OperationKind::AssaultCapital,
            anchor: capital,
            objective_properties: vec![capital],
            capture_eta: None,
            required_capture_survivors: 0,
            logistics_rank: None,
            forced_target_enemies: HashSet::default(),
            protected_capture_entities: HashSet::default(),
            capture_replacement_pending: false,
            staging_anchor: local_anchor,
            execution_authorized: false,
        },
    ];

    let active = ActivePlanObjective {
        kind: OperationKind::AssaultCapital,
        island_id: Some(island_id),
        properties: vec![capital],
        target_enemies: HashSet::default(),
    };
    let operations = build_operations(
        &scan,
        &mut ReachCtx::default(),
        &[active],
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );
    let same_island = operations
        .iter()
        .filter(|operation| operation.island_id == Some(island_id))
        .collect::<Vec<_>>();
    assert_eq!(
        same_island.len(),
        2,
        "継続PlanとRoadmapが同じ首都作戦を提示しても二重化しない"
    );
    let local = same_island
        .iter()
        .find(|operation| operation.kind == OperationKind::Capture)
        .expect("局地Capture作戦");
    assert_eq!(local.anchor, local_anchor);
    assert_eq!(local.objective_properties, vec![second_front, local_anchor]);
    assert_eq!(local.slots.capture_units, 2);
    assert!(!local.facts.requires_transport);

    let assault = same_island
        .iter()
        .find(|operation| operation.kind == OperationKind::AssaultCapital)
        .expect("勝利ロードマップの首都作戦");
    // 首都は論理目標として保持し、見積りと接敵の基準だけを未完了DAG区間へ置く。
    assert_eq!(assault.anchor, local_anchor);
    assert_eq!(assault.objective_properties, vec![capital]);
    assert_eq!(assault.slots.capture_units, 1);
    assert!(!assault.execution_authorized);
}

/// 既存Combat 1体を複数前線の撃破要求から同時に控除してはならない。
#[test]
fn existing_combat_sorties_are_committed_to_only_one_operation() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let infantry = UnitStats {
        can_capture: true,
        movement_type: MovementType::Infantry,
        max_movement: 3,
        ..stats(UnitType::Infantry, 1_000)
    };
    let helicopter = UnitStats {
        movement_type: MovementType::Air,
        max_movement: 8,
        max_fuel: 99,
        min_range: 1,
        max_range: 1,
        ..stats(UnitType::Bcopters, 7_500)
    };
    let mut damage_chart = DamageChart::new();
    damage_chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 65);
    damage_chart.insert_damage(UnitType::Infantry, UnitType::Bcopters, 0);
    let anchors = vec![pos(5, 1), pos(15, 1)];
    let horizons = vec![5, 5];
    let scan = BoardScan {
        map: flat_map(20, 3).into(),
        master_data: MasterDataRegistry::load().unwrap().into(),
        damage_chart: damage_chart.into(),
        current_turn: 1,
        game_turn: 1,
        opening_policy: None,
        funds: 20_000,
        capital_position: None,
        free_facilities: vec![(pos(0, 1), Terrain::Factory), (pos(1, 1), Terrain::Airport)],
        production_facilities: vec![(pos(0, 1), Terrain::Factory), (pos(1, 1), Terrain::Airport)],
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
        property_entities: HashMap::default(),
        owned_airport_count: 1,
        open_properties: anchors.clone(),
        neutral_properties: anchors.clone(),
        enemy_income: 0,
        enemy_production_slots: 0,
        enemy_facilities: Vec::new(),
        my_income: 5_000,
        campaign_objectives: Vec::new(),
        capital_assault_authorized: false,
        capital_staging_anchor: None,
        enemy_production_forecast: EnemyProductionForecastTrace::default(),
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
        &[true, true],
        &[anchors[0]],
        &HashSet::default(),
        &HashSet::default(),
        2,
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );
    let far = build_operation(
        &scan,
        &mut ctx,
        &infantry,
        OperationKind::Capture,
        anchors[1],
        &anchors,
        &horizons,
        &[true, true],
        &[anchors[1]],
        &HashSet::default(),
        &HashSet::default(),
        5,
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );

    assert!(near.facts.friendly_combat_units_committed > 0);
    assert_eq!(far.facts.friendly_combat_units_committed, 0);
}

#[test]
fn committed_combat_keeps_plan_ownership_and_accepts_operation_bound_reinforcements() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let own_entity = Entity::from_raw(900);
    let foreign_entity = Entity::from_raw(901);
    let immediate_entity = Entity::from_raw(902);
    let intercept_entity = Entity::from_raw(903);
    let own_plan = plan_revision::PlanId(7);
    let assignments = HashMap::from_iter([
        (
            own_entity,
            deployment::ActiveTargetAssignment {
                plan_id: Some(own_plan),
                slot_kind: SlotKind::Combat,
                objective: None,
                targets: HashSet::default(),
            },
        ),
        (
            foreign_entity,
            deployment::ActiveTargetAssignment {
                plan_id: Some(plan_revision::PlanId(8)),
                slot_kind: SlotKind::Combat,
                objective: None,
                targets: HashSet::default(),
            },
        ),
        (
            immediate_entity,
            deployment::ActiveTargetAssignment {
                plan_id: None,
                slot_kind: SlotKind::Combat,
                objective: None,
                targets: HashSet::default(),
            },
        ),
        (
            intercept_entity,
            deployment::ActiveTargetAssignment {
                plan_id: None,
                slot_kind: SlotKind::Intercept,
                objective: None,
                targets: HashSet::default(),
            },
        ),
    ]);

    assert_eq!(
        committed_entities_for_operation(Some(&assignments), None, ProductionPolicy::SmallMap),
        HashSet::from_iter([immediate_entity]),
        "実在敵へ割り当てた即時Combat増援は、次の作戦見積でも継続して数える"
    );
    assert_eq!(
        committed_entities_for_operation(
            Some(&assignments),
            Some(own_plan),
            ProductionPolicy::SmallMap
        ),
        HashSet::from_iter([own_entity, immediate_entity]),
        "別PlanとInterceptは混ぜず、同じPlanと即時Combatだけを数える"
    );
}

#[test]
fn combat_plan_projects_only_the_capturer_ordered_this_turn() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut scan = mixed_threat_multi_factory_scan();
    let anchor = pos(6, 2);
    let second_front = pos(7, 2);
    scan.open_properties = vec![anchor, second_front];
    scan.campaign_objectives = vec![CampaignPlanningObjective {
        island_id: crate::ai::islands::IslandId(0),
        kind: OperationKind::Capture,
        anchor,
        objective_properties: vec![anchor, second_front],
        capture_eta: None,
        required_capture_survivors: 2,
        logistics_rank: Some(0),
        forced_target_enemies: HashSet::default(),
        protected_capture_entities: HashSet::default(),
        capture_replacement_pending: false,
        staging_anchor: anchor,
        execution_authorized: true,
    }];
    let mut ctx = ReachCtx::default();
    let mut operations = build_operations(
        &scan,
        &mut ctx,
        &[],
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );
    let planned_stats = scan
        .reference_capture_unit()
        .expect("fixtureには占領可能兵がある")
        .clone();
    let planned_facility = scan.free_facilities[0].0;
    let operation = operations
        .iter_mut()
        .find(|operation| operation.kind == OperationKind::Capture)
        .expect("局地Capture作戦");
    // 同じ生産手番で、占領兵1体を先に発注した状態を再現する。
    operation.filled.capture_units = 1;
    operation.planned_capture_units.push(PlannedCaptureUnit {
        stats: planned_stats,
        facility: planned_facility,
        target: anchor,
    });

    let input = combat_plan_input(
        &scan,
        &mut ctx,
        operation,
        &HashSet::default(),
        &HashSet::default(),
        &HashSet::default(),
        scan.funds,
        false,
        PlayerId(1),
        ProductionPolicy::SmallMap,
    )
    .expect("観測敵がいるためCombat計画入力が作られる");

    assert_eq!(input.protected_units.len(), 1);
    assert_eq!(input.required_capture_survivors, 2);
    assert!(
        input
            .protected_units
            .iter()
            .all(|unit| unit.available_turn > 0)
    );
    assert!(input.capture_completion_turn.is_none());
    assert_eq!(input.current_funds, scan.funds);
}

/// 実生産ペースを、期限内に到着できる最寄りの1作戦だけへ見積もる。
#[test]
fn enemy_reinforcement_funds_is_local_unique_and_deadline_bounded() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut scan = multi_factory_scan();
    scan.enemy_income = 6000;
    scan.enemy_production_slots = 1;
    scan.enemy_production_forecast = EnemyProductionForecastTrace {
        expected_cost_next_turn: 1_000,
        ..EnemyProductionForecastTrace::default()
    };
    scan.enemy_facilities = vec![EnemyFacilitySnapshot {
        pos: pos(8, 2),
        terrain: Terrain::Factory,
    }];
    let anchors = vec![pos(6, 2), pos(0, 2)];
    let horizons = vec![4, 4];
    let mut ctx = ReachCtx::default();

    // 最安の歩兵は1ターンで到着する。Expectedは次の一波だけ、Stressは
    // 作戦期限までの3生産ターンを局地予算にする。
    let local = projected_enemy_reinforcement_envelope(&scan, &mut ctx, &anchors, &horizons, 0);
    assert_eq!(local.expected_funds, 1000);
    assert_eq!(local.stress_funds, 3000);
    assert_eq!(
        projected_enemy_reinforcement_envelope(&scan, &mut ctx, &anchors, &horizons, 1),
        EnemyReinforcementEnvelope::default()
    );

    // 到着期限が0なら、この施設はどの作戦の脅威にもならない。
    assert_eq!(
        projected_enemy_reinforcement_envelope(&scan, &mut ctx, &anchors, &[0, 0], 0),
        EnemyReinforcementEnvelope::default()
    );
}

#[test]
fn expected_reinforcement_starts_production_without_changing_the_launch_gate() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut scan = multi_factory_scan();
    scan.enemy_income = 6_000;
    scan.enemy_production_slots = 1;
    scan.enemy_facilities = vec![EnemyFacilitySnapshot {
        pos: pos(8, 2),
        terrain: Terrain::Factory,
    }];
    // 初回観測は開幕配置と区別できないためforecastが空でも、敵施設と収入が
    // ある限りExpectedを0にしない。
    scan.enemy_production_forecast = EnemyProductionForecastTrace::default();
    let mut ctx = ReachCtx::default();
    let operations = build_operations(
        &scan,
        &mut ctx,
        &[],
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );
    let capture = operations
        .iter()
        .find(|operation| operation.kind == OperationKind::Capture)
        .expect("capture objective");

    assert!(!capture.expected_reinforcements.is_empty());
    assert!(capture.facts.enemy_reinforcement_funds > 0);
    assert_eq!(
        capture.slots.combat_plan_required, 1,
        "Expectedは継続生産を起動する"
    );
    assert!(
        !scan.capital_assault_authorized,
        "Expectedを増やしてもDAG・兵站の進撃Goを変更しない"
    );
}

#[test]
fn unauthorized_capital_formation_does_not_hide_reinforcements_from_capture() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut scan = multi_factory_scan();
    scan.enemy_income = 6_000;
    scan.enemy_production_slots = 1;
    scan.enemy_production_forecast = EnemyProductionForecastTrace {
        expected_cost_next_turn: 1_000,
        ..EnemyProductionForecastTrace::default()
    };
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
            objective_properties: vec![capital],
            capture_eta: None,
            required_capture_survivors: 0,
            logistics_rank: None,
            forced_target_enemies: HashSet::default(),
            protected_capture_entities: HashSet::default(),
            capture_replacement_pending: false,
            staging_anchor: pos(0, 2),
            execution_authorized: false,
        },
        CampaignPlanningObjective {
            island_id: crate::ai::islands::IslandId(2),
            kind: OperationKind::Capture,
            anchor: capture,
            objective_properties: vec![capture],
            capture_eta: Some(4),
            required_capture_survivors: 3,
            logistics_rank: Some(0),
            forced_target_enemies: HashSet::default(),
            protected_capture_entities: HashSet::default(),
            capture_replacement_pending: false,
            staging_anchor: capture,
            execution_authorized: true,
        },
    ];
    let anchors = vec![capital, capture];
    let horizons = vec![12, 4];
    let mut ctx = ReachCtx::default();

    assert_eq!(
        projected_enemy_reinforcement_envelope(&scan, &mut ctx, &anchors, &horizons, 0),
        EnemyReinforcementEnvelope::default()
    );
    assert!(
        projected_enemy_reinforcement_envelope(&scan, &mut ctx, &anchors, &horizons, 1)
            .expected_funds
            > 0
    );
}

#[test]
fn enemy_reinforcement_assessment_places_each_purchase_on_the_time_axis() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut scan = multi_factory_scan();
    scan.enemy_income = 1_000;
    scan.enemy_production_slots = 1;
    scan.enemy_facilities = vec![EnemyFacilitySnapshot {
        pos: pos(8, 2),
        terrain: Terrain::Factory,
    }];
    Arc::make_mut(&mut scan.damage_chart).insert_damage(UnitType::Infantry, UnitType::Tank, 20);
    Arc::make_mut(&mut scan.damage_chart).insert_damage(UnitType::Tank, UnitType::Infantry, 80);
    let mut op = operation(
        OperationKind::Capture,
        OperationSlots::default(),
        OperationSlots::default(),
    );
    op.anchor = pos(6, 2);
    op.facts.enemy_reinforcement_funds = 3_000;

    let assessment = enemy_reinforcement_assessment(
        &scan,
        &mut ReachCtx::default(),
        &op,
        5,
        op.facts.enemy_reinforcement_funds,
        ReinforcementScenario::Stress,
        ProductionPolicy::SmallMap,
    );
    let mut arrivals = assessment
        .reinforcements
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
        vec![2, 3, 4]
    );
    assert!(
        arrivals
            .iter()
            .all(|(_, unit_type)| *unit_type == UnitType::Infantry)
    );
}

#[test]
fn rejected_joint_package_keeps_requested_demand_and_records_effective_demand() {
    let requested = OperationSlots {
        capture_units: 4,
        ..OperationSlots::default()
    };
    let operations = vec![operation(
        OperationKind::Capture,
        requested,
        OperationSlots::default(),
    )];
    let mut ledger = SlotLedger::new();
    ledger.close(0, SlotKind::Capture, UnfilledReason::NoCandidate);
    // 実行対象の占領購入が無い合同案でも、旧実装と同じ位置でCaptureを再開する。
    ledger.set_effective(
        0,
        SlotKind::Capture,
        0,
        EffectiveDemandReason::JointPackageSelected,
    );
    ledger.reopen(0, SlotKind::Capture);
    assert_eq!(operations[0].slots, requested);
    assert!(ledger.is_open(0, SlotKind::Capture));
    assert_eq!(
        most_starved_slot(&operations, &ledger, ProductionPolicy::for_current_path()),
        None
    );
    let traces = production_demand_traces(&operations, &ledger);
    assert_eq!(traces[0].requested, requested);
    assert_eq!(traces[0].effective.capture_units, 0);
    assert_eq!(traces[0].filled.capture_units, 0);
    assert_eq!(
        traces[0].adjustments[0].reason,
        EffectiveDemandReason::JointPackageSelected
    );
    ledger.close(0, SlotKind::Capture, UnfilledReason::NoExecutablePlan);
    assert_eq!(unfilled_slot_traces(&operations, &ledger)[0].requirement, 4);
    assert_eq!(
        ledger.reason(0, SlotKind::Capture),
        Some(UnfilledReason::NoExecutablePlan)
    );
}

#[test]
fn effective_joint_demand_preserves_legacy_slot_selection_and_deficit() {
    for amount in [0, 1, 2, 6] {
        for filled in [0, 1, 3] {
            let requested = OperationSlots {
                capture_units: 4,
                ..OperationSlots::default()
            };
            let filled_slots = OperationSlots {
                capture_units: filled,
                ..OperationSlots::default()
            };
            let mut current = operation(OperationKind::Capture, requested, filled_slots);
            let competing_slots = OperationSlots {
                capture_units: 2,
                ..OperationSlots::default()
            };
            let mut ledger = SlotLedger::new();
            ledger.set_effective(
                0,
                SlotKind::Capture,
                amount,
                EffectiveDemandReason::JointPackageSelected,
            );
            let effective = allocation_slots(&current, 0, &ledger);
            let competing = operation(
                OperationKind::Capture,
                competing_slots,
                OperationSlots::default(),
            );
            let new_choice = most_starved_slot(
                &[current, competing],
                &ledger,
                ProductionPolicy::for_current_path(),
            );
            // 旧実装の直接上書きと、実効要求を読む実装の配分先・未充足率を比較する。
            current = operation(OperationKind::Capture, requested, filled_slots);
            current.slots.capture_units = amount;
            assert_eq!(effective, current.slots);
            assert_eq!(
                effective.deficit_ratio(SlotKind::Capture, &current.filled),
                current
                    .slots
                    .deficit_ratio(SlotKind::Capture, &current.filled)
            );
            let competing = operation(
                OperationKind::Capture,
                competing_slots,
                OperationSlots::default(),
            );
            assert_eq!(
                new_choice,
                most_starved_slot(
                    &[current, competing],
                    &SlotLedger::new(),
                    ProductionPolicy::for_current_path()
                )
            );
        }
    }
}

/// 輸送台帳は「最寄り作戦」ではなく「その作戦へ届けられるか」で数える
///
/// 輸送ユニットは自軍港湾に生まれて港に留まるため、位置で仕分けると常に
/// 母港に近い作戦の台帳に載る。渡洋する作戦側の空き搭載スロットは永久に 0 となり、
/// 輸送枠の要求が減らないまま揚陸艇を延々と買い増すラチェットになる。
#[test]
fn transports_are_ledgered_by_delivery_ability_not_proximity() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
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
        &[true, true],
        &[anchors[1]],
        &HashSet::default(),
        &HashSet::default(),
        3,
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );

    // それでも「対岸へ積荷を届けられる」以上、渡洋作戦の台帳に載らねばならない
    assert_eq!(overseas.facts.available_free_cargo_slots, 2);
}

#[test]
fn loaded_enemy_transport_on_another_island_is_a_future_arrival() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
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
        &[true],
        &[anchor],
        &HashSet::default(),
        &HashSet::default(),
        3,
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );

    assert_eq!(operation.reachable_threats.len(), 1);
    let threat = &operation.reachable_threats[0];
    assert_eq!(threat.entity, None, "別島の敵Entityを追跡対象にしない");
    assert!(threat.available_turn > 0, "anchor到着後の増援として扱う");
    assert_eq!(threat.position, anchor);
}

#[test]
fn empty_enemy_transport_does_not_create_anti_air_demand_on_every_island() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
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
        &[true],
        &[anchor],
        &HashSet::default(),
        &HashSet::default(),
        3,
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );

    assert!(operation.reachable_threats.is_empty());
}

/// キャンペーンの予約超過分は、構造枠を二重購入せず、自力展開可能な対敵戦力へ使う。
#[test]
fn campaign_surplus_targets_enemy_infantry_without_buying_stranded_tank() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut map = flat_map(9, 3);
    for y in 0..map.height {
        map.set_terrain(4, y, Terrain::Sea).unwrap();
    }
    let infantry = UnitStats {
        can_capture: true,
        max_movement: 3,
        max_fuel: 99,
        ..stats(UnitType::Infantry, 1_000)
    };
    let tank = UnitStats {
        movement_type: MovementType::Tank,
        max_movement: 6,
        max_fuel: 70,
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
        map: map.into(),
        master_data: MasterDataRegistry::load().unwrap().into(),
        damage_chart: damage_chart.into(),
        current_turn: 1,
        game_turn: 1,
        opening_policy: None,
        funds: 20_000,
        capital_position: None,
        free_facilities: vec![(pos(1, 1), Terrain::Factory), (pos(2, 1), Terrain::Airport)],
        production_facilities: vec![(pos(1, 1), Terrain::Factory), (pos(2, 1), Terrain::Airport)],
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
        property_entities: HashMap::default(),
        owned_airport_count: 1,
        open_properties: vec![pos(7, 1)],
        neutral_properties: vec![pos(7, 1)],
        enemy_income: 0,
        enemy_production_slots: 0,
        enemy_facilities: Vec::new(),
        my_income: 5_000,
        campaign_objectives: capture_objective(pos(7, 1)),
        capital_assault_authorized: false,
        capital_staging_anchor: None,
        enemy_production_forecast: EnemyProductionForecastTrace::default(),
    };

    let (commands, trace) = plan_production(&scan, PlayerId(1), false, &HashMap::default());

    assert_eq!(commands.len(), 1, "commands={commands:?}, trace={trace:?}");
    assert_eq!(commands[0].unit_type, UnitType::Fighter);
    assert!(commands.iter().all(|command| {
        !matches!(
            command.unit_type,
            UnitType::Infantry | UnitType::TransportHelicopter | UnitType::Tank
        )
    }));
}

/// Mustを満たした後の増援は、到達不能な工場や仮想敵へ流さず、実在する前線Entityを
/// 撃てる種類だけを選び、そのEntityをpriorityとして任務へ接続する。
#[test]
fn immediate_reinforcement_targets_a_live_reachable_frontline_enemy() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
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
    let mut damage_chart = DamageChart::new();
    damage_chart.insert_damage(UnitType::Fighter, UnitType::Infantry, 80);
    damage_chart.insert_damage(UnitType::Infantry, UnitType::Fighter, 0);
    damage_chart.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
    damage_chart.insert_damage(UnitType::Infantry, UnitType::Tank, 0);
    let enemy = Entity::from_raw(902);
    let scan = BoardScan {
        map: map.into(),
        master_data: MasterDataRegistry::load().unwrap().into(),
        damage_chart: damage_chart.into(),
        current_turn: 1,
        game_turn: 1,
        opening_policy: None,
        funds: 20_000,
        capital_position: None,
        free_facilities: vec![(pos(1, 1), Terrain::Factory), (pos(2, 1), Terrain::Airport)],
        production_facilities: vec![(pos(1, 1), Terrain::Factory), (pos(2, 1), Terrain::Airport)],
        available_types: vec![
            (UnitType::Infantry, infantry.clone()),
            (UnitType::Tank, tank),
            (UnitType::Fighter, fighter),
        ],
        my_units: Vec::new(),
        enemy_units: vec![UnitSnapshot {
            entity: Some(enemy),
            pos: pos(7, 1),
            stats: infantry,
            hp: 100,
            free_cargo: 0,
        }],
        property_entities: HashMap::default(),
        owned_airport_count: 1,
        open_properties: vec![pos(7, 1)],
        neutral_properties: vec![pos(7, 1)],
        enemy_income: 0,
        enemy_production_slots: 0,
        enemy_facilities: Vec::new(),
        my_income: 5_000,
        campaign_objectives: capture_objective(pos(7, 1)),
        capital_assault_authorized: false,
        capital_staging_anchor: None,
        enemy_production_forecast: EnemyProductionForecastTrace::default(),
    };
    let mut ctx = ReachCtx::default();
    let operations = build_operations(
        &scan,
        &mut ctx,
        &[],
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );

    let options = immediate_combat_options(
        &scan,
        &mut ctx,
        &operations,
        PlayerId(1),
        &HashSet::default(),
    );
    let (operation_index, candidate, engagement) = select_immediate_combat_reinforcement(
        &operations,
        &options,
        &HashSet::default(),
        &HashMap::default(),
        CandidateConstraints {
            remaining_funds: scan.funds,
            per_slot_budget: 10_000,
        },
        ProductionPolicy::SmallMap,
    )
    .expect("実在前線へ届く増援を選ぶ");

    assert_eq!(candidate.unit_type, UnitType::Fighter);
    let deployment = planned_deployment(
        &scan,
        &mut ctx,
        &operations[operation_index],
        SlotKind::Combat,
        &candidate,
        Some(engagement.entity),
    )
    .expect("Combat増援に任務を付ける");
    assert_eq!(deployment.priority_enemies, vec![enemy]);
    assert!(deployment.plan_step.is_none());
}

#[test]
fn standalone_reinforcement_does_not_replace_missing_front_with_indirect_fire() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut scan = mixed_threat_multi_factory_scan();
    // 汎用fixtureの燃料0は経路判定用ではない。実マスターの燃料を与える。
    for (kind, stats) in &mut scan.available_types {
        stats.max_fuel = scan
            .master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                kind.as_str().to_owned(),
            ))
            .unwrap()
            .max_fuel;
    }
    let gun = scan
        .master_data
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::HeavySpGun.as_str().to_owned(),
        ))
        .unwrap();
    scan.available_types.push((UnitType::HeavySpGun, gun));
    Arc::make_mut(&mut scan.damage_chart).insert_damage(
        UnitType::HeavySpGun,
        UnitType::Infantry,
        100,
    );
    let mut ctx = ReachCtx::default();
    let operations = build_operations(
        &scan,
        &mut ctx,
        &[],
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );
    let options = immediate_combat_options(
        &scan,
        &mut ctx,
        &operations,
        PlayerId(1),
        &HashSet::default(),
    );
    assert!(!options.is_empty(), "到達可能な前衛補充は残す");
    assert!(
        options
            .iter()
            .all(|option| option.candidate.unit_type != UnitType::HeavySpGun),
        "前衛との成立を検証するRollingPlanを単体の砲生産で迂回しない"
    );
}

/// 必要な占領枠を確保した後の前衛補充は、工場ごとの均等配分で戦力を落とさない。
#[test]
fn combat_reinforcement_uses_affordable_frontline_strength_not_equal_factory_budget() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let enemy = Entity::from_raw(904);
    let options = [
        (UnitType::Infantry, 1000, 20, 0.2),
        (UnitType::Tank, 6000, 80, 0.8),
    ]
    .into_iter()
    .map(|(unit_type, cost, damage, fitness)| ImmediateCombatOption {
        operation_index: 0,
        candidate: SlotCandidate {
            unit_type,
            cost,
            facility: pos(1, 1),
            fitness: 0.0,
        },
        engagements: vec![ImmediateCombatEngagement {
            entity: enemy,
            current_hp: 100,
            damage,
            fitness,
        }],
    })
    .collect::<Vec<_>>();
    let operations = vec![operation(
        OperationKind::Capture,
        OperationSlots::default(),
        OperationSlots::default(),
    )];
    let (_, candidate, _) = select_immediate_combat_reinforcement(
        &operations,
        &options,
        &HashSet::default(),
        &HashMap::default(),
        CandidateConstraints {
            remaining_funds: 6000,
            per_slot_budget: 1000,
        },
        ProductionPolicy::SmallMap,
    )
    .unwrap();
    assert_eq!(
        candidate.unit_type,
        UnitType::Tank,
        "全工場で一体ずつ買う必要はなく、到達/相性で優れる前衛を買う"
    );
    let (_, candidate, _) = select_immediate_combat_reinforcement(
        &operations,
        &options,
        &HashSet::default(),
        &HashMap::default(),
        CandidateConstraints {
            remaining_funds: 1000,
            per_slot_budget: 1000,
        },
        ProductionPolicy::SmallMap,
    )
    .unwrap();
    assert_eq!(
        candidate.unit_type,
        UnitType::Infantry,
        "実資金の制約は維持する"
    );
}

/// 余剰Combatは候補表を一度だけ作るが、1体が同じ敵全員を同時に攻撃できるとは
/// 見なさない。工場枠をまとめて割り当てるときは、先に必要HPを満たした敵ではなく
/// 次の実在敵を担当させる。
#[test]
fn immediate_reinforcement_package_spreads_damage_across_live_enemies() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let first_enemy = Entity::from_raw(904);
    let second_enemy = Entity::from_raw(905);
    let candidate = |facility| SlotCandidate {
        unit_type: UnitType::Tank,
        cost: 7_000,
        facility,
        fitness: 0.0,
    };
    let engagements = vec![
        ImmediateCombatEngagement {
            entity: first_enemy,
            current_hp: 100,
            damage: 100,
            fitness: 1.0,
        },
        ImmediateCombatEngagement {
            entity: second_enemy,
            current_hp: 100,
            damage: 100,
            fitness: 0.9,
        },
    ];
    let options = vec![
        ImmediateCombatOption {
            operation_index: 0,
            candidate: candidate(pos(1, 1)),
            engagements: engagements.clone(),
        },
        ImmediateCombatOption {
            operation_index: 0,
            candidate: candidate(pos(2, 1)),
            engagements,
        },
    ];
    let operations = vec![operation(
        OperationKind::Capture,
        OperationSlots::default(),
        OperationSlots::default(),
    )];
    let (operation_index, _, first) = select_immediate_combat_reinforcement(
        &operations,
        &options,
        &HashSet::default(),
        &HashMap::default(),
        CandidateConstraints {
            remaining_funds: 14_000,
            per_slot_budget: 7_000,
        },
        ProductionPolicy::SmallMap,
    )
    .expect("最初の前線敵へ増援を割り当てる");
    assert_eq!(first.entity, first_enemy);

    let mut used_facilities = HashSet::default();
    used_facilities.insert(pos(1, 1));
    let mut committed_damage = HashMap::default();
    committed_damage.insert((operation_index, first.entity), first.damage);
    let (_, _, second) = select_immediate_combat_reinforcement(
        &operations,
        &options,
        &used_facilities,
        &committed_damage,
        CandidateConstraints {
            remaining_funds: 7_000,
            per_slot_budget: 7_000,
        },
        ProductionPolicy::SmallMap,
    )
    .expect("別の未充足前線敵へ増援を割り当てる");
    assert_eq!(second.entity, second_enemy);
}

/// 工場が余っているのではなく「同じ工場枠で何を出すか」が制約の局面では、価格あたり
/// の効率で安い歩兵へ寄せず、前線全体をより大きく削れる戦闘unitを選ぶ。
#[test]
fn immediate_reinforcement_values_factory_slot_over_unit_price() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let infantry = UnitStats {
        can_capture: true,
        max_movement: 3,
        max_fuel: 99,
        ..stats(UnitType::Infantry, 1_000)
    };
    let tank = UnitStats {
        max_movement: 6,
        max_fuel: 70,
        ..stats(UnitType::Tank, 7_000)
    };
    let mut damage_chart = DamageChart::new();
    damage_chart.insert_damage(UnitType::Infantry, UnitType::Infantry, 70);
    damage_chart.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
    let enemy = Entity::from_raw(903);
    let scan = BoardScan {
        map: flat_map(9, 3).into(),
        master_data: MasterDataRegistry::load().unwrap().into(),
        damage_chart: damage_chart.into(),
        current_turn: 1,
        game_turn: 1,
        opening_policy: None,
        funds: 10_000,
        capital_position: None,
        free_facilities: vec![(pos(1, 1), Terrain::Factory)],
        production_facilities: vec![(pos(1, 1), Terrain::Factory)],
        available_types: vec![
            (UnitType::Infantry, infantry.clone()),
            (UnitType::Tank, tank),
        ],
        my_units: Vec::new(),
        enemy_units: vec![UnitSnapshot {
            entity: Some(enemy),
            pos: pos(6, 1),
            stats: infantry,
            hp: 100,
            free_cargo: 0,
        }],
        property_entities: HashMap::default(),
        owned_airport_count: 0,
        open_properties: vec![pos(6, 1)],
        neutral_properties: vec![pos(6, 1)],
        enemy_income: 0,
        enemy_production_slots: 0,
        enemy_facilities: Vec::new(),
        my_income: 1_000,
        campaign_objectives: capture_objective(pos(6, 1)),
        capital_assault_authorized: false,
        capital_staging_anchor: None,
        enemy_production_forecast: EnemyProductionForecastTrace::default(),
    };
    let mut ctx = ReachCtx::default();
    let operations = build_operations(
        &scan,
        &mut ctx,
        &[],
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );
    let options = immediate_combat_options(
        &scan,
        &mut ctx,
        &operations,
        PlayerId(1),
        &HashSet::default(),
    );

    let (_, candidate, _) = select_immediate_combat_reinforcement(
        &operations,
        &options,
        &HashSet::default(),
        &HashMap::default(),
        CandidateConstraints {
            remaining_funds: scan.funds,
            per_slot_budget: scan.funds,
        },
        ProductionPolicy::SmallMap,
    )
    .expect("両候補とも実在前線へ届く");

    assert_eq!(candidate.unit_type, UnitType::Tank);
}

/// 動員の役割は価格ではなく選定理由で決まる。脅威へ有効打を持つ占領兵は交戦役、
/// 脅威が無いときの占領兵は（価格に関係なく）占領役になる。
#[test]
fn mobilization_role_follows_selection_reason_not_price() {
    let mut damage_chart = DamageChart::new();
    damage_chart.insert_damage(UnitType::Infantry, UnitType::Infantry, 55);
    damage_chart.insert_damage(UnitType::Mech, UnitType::Tank, 55);
    let mut op = operation(
        OperationKind::Capture,
        OperationSlots::default(),
        OperationSlots::default(),
    );
    op.reachable_threats.push(ThreatTarget {
        entity: Some(Entity::from_raw(900)),
        stats: stats(UnitType::Tank, 6_000),
        position: pos(3, 3),
        current_hp: 100,
        available_turn: 0,
    });
    let infantry = UnitStats {
        can_capture: true,
        ..stats(UnitType::Infantry, 1_000)
    };
    let mech = UnitStats {
        can_capture: true,
        ..stats(UnitType::Mech, 2_000)
    };
    let tank = stats(UnitType::Tank, 6_000);

    // 脅威が無ければ占領能力兵は価格に関係なく占領役。
    assert_eq!(
        mobilization_role(&mech, false, &op, &damage_chart),
        SlotKind::Capture
    );
    assert_eq!(
        mobilization_role(&infantry, false, &op, &damage_chart),
        SlotKind::Capture
    );
    assert_eq!(
        mobilization_role(&tank, false, &op, &damage_chart),
        SlotKind::Combat
    );
    // 脅威へ有効打を持つ兵は交戦役、有効打の無い占領兵は占領役のまま。
    assert_eq!(
        mobilization_role(&mech, true, &op, &damage_chart),
        SlotKind::Combat
    );
    assert_eq!(
        mobilization_role(&infantry, true, &op, &damage_chart),
        SlotKind::Capture
    );
}

/// 今まさに未所有拠点の上で占領を進めている敵は、到達途中の占領可能敵より
/// 優先して攻撃対象（`Interdict`）になる。
#[test]
fn capturing_enemy_is_prioritized_for_interdict() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let scan = BoardScan {
        open_properties: vec![pos(5, 1)],
        ..mixed_threat_multi_factory_scan()
    };
    // 未所有拠点上の占領可能ユニットは「占領中」と判定される。
    let capturing = ThreatTarget {
        entity: Some(Entity::from_raw(700)),
        stats: UnitStats {
            can_capture: true,
            ..stats(UnitType::Infantry, 1_000)
        },
        position: pos(5, 1),
        current_hp: 100,
        available_turn: 0,
    };
    // 同じ占領可能ユニットでも、拠点上にいなければ「到達途中」で優先度は低い。
    let incoming = ThreatTarget {
        entity: Some(Entity::from_raw(701)),
        stats: UnitStats {
            can_capture: true,
            ..stats(UnitType::Infantry, 1_000)
        },
        position: pos(4, 1),
        current_hp: 100,
        available_turn: 0,
    };

    assert!(enemy_is_capturing(&scan, &capturing));
    assert!(!enemy_is_capturing(&scan, &incoming));
}

/// 同型購入の施設交換でも、命令・占領意図・購入診断を購入順で確定する。
#[test]
fn facility_finalization_synchronizes_same_type_capture_purchases() {
    let mut scan = multi_factory_scan();
    let facilities = [pos(1, 1), pos(1, 3)];
    // 最適化は施設一覧でなく実地形で生産可否を確認する。fixtureの地形も工場へ揃える。
    for facility in facilities {
        Arc::make_mut(&mut scan.map)
            .set_terrain(facility.x, facility.y, Terrain::Factory)
            .unwrap();
    }
    for optimize in [false, true] {
        let mut commands = Vec::new();
        let mut trace = ProductionPlanTrace::new(PlayerId(0), 2000, 2);
        for (facility, target) in facilities.into_iter().zip(facilities.into_iter().rev()) {
            let command = ProduceUnitCommand {
                player_id: PlayerId(0),
                target_x: facility.x,
                target_y: facility.y,
                unit_type: UnitType::Infantry,
            };
            commands.push(PlannedProduction {
                capture_intent: Some(campaign_execution::CampaignProductionIntent {
                    command: command.clone(),
                    island_id: crate::ai::islands::IslandId(0),
                    role: campaign_execution::CampaignProductionRole::Capture,
                    mission_target: Some(target),
                }),
                command,
                role: SlotKind::Capture,
                cost: 1000,
                purpose: PurchasePurpose::OperationSlot,
                deployment: None,
            });
            trace.steps.push(ProductionStepTrace {
                operation_kind: OperationKind::Capture,
                operation_anchor: pos(6, 2),
                slot_kind: SlotKind::Capture,
                deficit_before: 1.0,
                deficit_after: 0.0,
                remaining_funds_before: 2000,
                decision: ProductionDecision::Produced {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    facility,
                },
            });
        }
        trace.rolling_combat_plans.push(RollingCombatPlanTrace {
            plan_id: None,
            revision: None,
            disposition: PlanDisposition::Rejected,
            replan_reason: None,
            operation_kind: OperationKind::Capture,
            anchor: pos(6, 2),
            feasible: false,
            // 同型2件と同じ施設の将来購入を併記し、今手番だけを対応付ける。
            purchases: facilities
                .into_iter()
                .map(|facility| RollingPurchaseTrace {
                    unit_type: UnitType::Infantry,
                    facility,
                    build_turn: 0,
                    cost: 1000,
                })
                .chain(std::iter::once(RollingPurchaseTrace {
                    unit_type: UnitType::Infantry,
                    facility: facilities[0],
                    build_turn: 1,
                    cost: 1000,
                }))
                .collect(),
            targets: Vec::new(),
            turn_forecasts: Vec::new(),
            first_attack_turn: None,
            elimination_turn: None,
            occupation_turn: None,
            production_cost: 3000,
            expected_loss: 0,
            surviving_combat_value: 0,
            required_overmatch_value: 0,
            overmatch_ready: false,
            protected_unit_count: 0,
            protected_survivor_count: 0,
            required_capture_survivor_count: 0,
            candidates_considered: 0,
            candidates_pruned: 0,
            search_truncated: false,
        });
        production_finalize::finalize(
            &scan,
            &mut commands,
            &mut trace,
            &mut V4RollingPlanRegistry::default(),
            optimize,
        );
        let expected = if optimize {
            [facilities[1], facilities[0]]
        } else {
            facilities
        };
        for ((planned, step), expected) in commands.iter().zip(&trace.steps).zip(expected) {
            assert_eq!(pos(planned.target_x, planned.target_y), expected);
            let intent = planned.capture_intent.as_ref().unwrap();
            assert_eq!(
                pos(intent.command.target_x, intent.command.target_y),
                expected
            );
            let ProductionDecision::Produced { facility, .. } = step.decision else {
                panic!("購入traceが失われた");
            };
            assert_eq!(facility, expected);
        }
        let purchases = &trace.rolling_combat_plans[0].purchases;
        assert_eq!([purchases[0].facility, purchases[1].facility], expected);
        assert_eq!(purchases[2].facility, facilities[0]);
        assert_eq!(purchases[2].build_turn, 1);
    }
}

/// 生産トレースは、発行した命令 1 件ごとに「どの作戦のどの枠から出たか」を残す
///
/// 「同一ターン内に同じユニットが全施設へ発注される」現象を切り分けるには、
/// 発注とトレースが 1 対 1 で対応していなければならない。ズレた時点で
/// 診断そのものが無意味になるため、記録専用であるという不変条件をここで固定する。
#[test]
fn production_trace_attributes_every_command_to_a_slot() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let scan = multi_factory_scan();
    let (commands, trace) = plan_production(&scan, PlayerId(0), true, &HashMap::default());

    // 作戦が立つ盤面なので fallback には落ちない
    assert!(!trace.fallback);
    assert!(!trace.operations.is_empty());
    assert!(!commands.is_empty());
    assert_eq!(trace.funds, scan.funds);
    assert_eq!(trace.free_facility_count, scan.free_facilities.len());

    // 発注は 1 件残らず、通常購入または即時前線増援として記録される。
    let produced: Vec<_> = trace
        .steps
        .iter()
        .filter_map(|step| match &step.decision {
            ProductionDecision::Produced {
                unit_type,
                cost,
                facility,
            } => Some((*unit_type, *cost, *facility)),
            ProductionDecision::ProducedImmediateReinforcement {
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
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut scan = strait_scan();
    // 接岸できる地形を消すと、艦船は対岸へ陸上ユニットを降ろせなくなる
    scan.map = strait_map(None).into();
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
        &[true, true],
        &[anchors[1]],
        &HashSet::default(),
        &HashSet::default(),
        3,
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );

    assert_eq!(overseas.facts.available_free_cargo_slots, 0);
}

/// 期限付き物件制御でも担当が全滅したら、戦闘計画の完了待ちにせず1体だけ補充する。
#[test]
fn property_control_replaces_one_destroyed_capturer_before_combat() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let slots = OperationSlots {
        capture_units: 3,
        combat_plan_required: 1,
        ..OperationSlots::default()
    };
    let control = PropertyControlObjective {
        property: pos(4, 1),
        enemy_entity: Entity::from_raw(42),
        enemy_arrival_turn: 1,
        enemy_completion_turn: 3,
        attack_ready_turn: Some(2),
        approach: PropertyControlApproach::Interdict,
        disaggregated: false,
    };
    let mut initial_formation = operation(OperationKind::Capture, slots, OperationSlots::default());
    initial_formation.property_controls.push(control);
    assert_eq!(
        most_starved_slot(
            &[initial_formation],
            &SlotLedger::new(),
            ProductionPolicy::for_current_path()
        ),
        Some((0, SlotKind::Combat)),
        "担当履歴のない初回編成はCapture単独発注へ崩さず共同計画を使う"
    );

    let mut missing_capturer = operation(OperationKind::Capture, slots, OperationSlots::default());
    missing_capturer.property_controls.push(control);
    missing_capturer.capture_replacement_pending = true;
    missing_capturer.facts.friendly_capture_units_committed = 1;

    assert_eq!(
        most_starved_slot(
            &[missing_capturer],
            &SlotLedger::new(),
            ProductionPolicy::for_current_path()
        ),
        Some((0, SlotKind::Capture)),
        "一部の占領担当が生存していても、未解決の撃破1件はCombatより先に補う"
    );

    let mut replacement_ready = operation(
        OperationKind::Capture,
        slots,
        OperationSlots {
            capture_units: 1,
            ..OperationSlots::default()
        },
    );
    replacement_ready.property_controls.push(control);
    replacement_ready.capture_replacement_pending = true;

    assert_eq!(
        most_starved_slot(
            &[replacement_ready],
            &SlotLedger::new(),
            ProductionPolicy::for_current_path()
        ),
        Some((0, SlotKind::Combat)),
        "1体を再契約した後は残りのCaptureを連買いせず共同Combat計画へ戻す"
    );
}

/// 別レーンに近い既存歩兵がいても、未充足レーン用の有限な1枠は消さない。
#[test]
fn property_capture_candidate_is_not_blocked_by_another_lanes_capturer() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut scan = multi_factory_scan();
    let target = pos(6, 2);
    let infantry = scan
        .reference_capture_unit()
        .expect("fixtureには占領可能兵がある")
        .clone();
    scan.my_units.push(UnitSnapshot {
        entity: Some(Entity::from_raw(51)),
        pos: pos(5, 2),
        stats: infantry,
        hp: 100,
        free_cargo: 0,
    });
    let mut operation = operation(
        OperationKind::Capture,
        OperationSlots {
            capture_units: 1,
            ..OperationSlots::default()
        },
        OperationSlots::default(),
    );
    operation.anchor = target;
    operation.capture_lane_targets = vec![target];

    let candidate = select_property_capture_candidate(
        &scan,
        &mut ReachCtx::default(),
        &operation,
        &HashSet::default(),
        scan.funds,
        PlayerId(1),
    );

    assert!(
        candidate.is_some(),
        "別レーンの既存担当は同時にこの未充足レーンを占領できない"
    );
}

#[test]
fn active_dag_segment_defers_unapproved_capital_capture_slots() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut segment = operation(
        OperationKind::Capture,
        OperationSlots {
            combat_plan_required: 1,
            ..OperationSlots::default()
        },
        OperationSlots::default(),
    );
    segment.facts.enemy_combat_units = 3;
    let capital_slots = OperationSlots {
        capture_units: 1,
        transport_slots: 2,
        ..OperationSlots::default()
    };
    let mut capital = operation(
        OperationKind::AssaultCapital,
        capital_slots,
        OperationSlots::default(),
    );
    capital.execution_authorized = false;
    let operations = vec![segment, capital];
    let mut ledger = SlotLedger::new();

    defer_unapproved_capital_structure_for_active_segment(&operations, &mut ledger);

    // 要求は残し、構造枠だけを理由付きで当手番の配分対象から外す。
    assert_eq!(operations[1].slots.capture_units, 1);
    assert_eq!(operations[1].slots.transport_slots, 2);
    assert!(ledger.is_open(0, SlotKind::Combat));
    assert_eq!(
        ledger.reason(1, SlotKind::Capture),
        Some(UnfilledReason::DeferredBehindActiveSegment)
    );
    assert_eq!(
        ledger.reason(1, SlotKind::Transport),
        Some(UnfilledReason::DeferredBehindActiveSegment)
    );
    assert_eq!(effective_slots(&operations[1], 1, &ledger).capture_units, 0);
}

/// 前線の分類はmap名ではなく、占領可能unitの実到達性だけで決める。
#[test]
fn direct_front_uses_operation_production_while_strait_keeps_transport_campaign() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    use crate::ai::island_campaign::{IslandCampaignDecision, IslandCampaignShortfall};
    use crate::ai::islands::IslandId;

    let shortfall = |target_position| IslandCampaignShortfall {
        island_id: IslandId(0),
        decision: IslandCampaignDecision::Assault,
        target_position,
        light_transport_slots: 0,
        heavy_transport_slots: 0,
        capture_units: 4,
        ground_combat_units: 0,
        combat_units: 0,
        priority_enemy_types: Vec::new(),
        reserved_budget: 4_000,
        priority_rank: 0,
    };

    let mut mainland = strait_scan();
    mainland.map = flat_map(10, 3).into();
    mainland.production_facilities = vec![(pos(1, 1), Terrain::Factory)];
    mainland.free_facilities = mainland.production_facilities.clone();
    let mut reach = ReachCtx::default();
    assert!(has_direct_capture_route(
        &mainland,
        &mut reach,
        &shortfall(pos(8, 1))
    ));

    let overseas = strait_scan();
    let mut reach = ReachCtx::default();
    assert!(!has_direct_capture_route(
        &overseas,
        &mut reach,
        &shortfall(pos(8, 1))
    ));
}

#[test]
fn defense_urgency_raises_normal_operation_priority_without_deleting_capital() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
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
    // 期限付き物件制御の順位を1段差し込んでも、緊急Defenseが最優先で
    // 首都本隊そのものは削除されない。
    assert_eq!(
        operation_priority_rank(&ops[0], ProductionPolicy::for_current_path()),
        PRIORITY_RANK_AUTHORIZED_CAPITAL
    );
    assert_eq!(
        operation_priority_rank(&ops[1], ProductionPolicy::for_current_path()),
        PRIORITY_RANK_URGENT_DEFENSE
    );
    assert_eq!(
        most_starved_slot(
            &ops,
            &SlotLedger::new(),
            ProductionPolicy::for_current_path()
        ),
        Some((1, SlotKind::Combat))
    );
    assert_eq!(ops[0].slots.combat_plan_required, 1);
}

#[test]
fn board_scan_is_independent_of_ecs_storage_order() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let registry = MasterDataRegistry::load().unwrap();
    let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
        &registry,
        "map_32",
        GridTopology::Hex,
    )
    .unwrap();
    let player = PlayerId(2);
    let mut entities = Vec::new();
    for (x, owner) in [
        (3, player),
        (4, player),
        (5, player.opposite()),
        (6, player.opposite()),
    ] {
        entities.push(
            world
                .spawn((
                    pos(x, 7),
                    Faction(owner),
                    UnitStats::mock(),
                    Health {
                        current: 100,
                        max: 100,
                    },
                ))
                .id(),
        );
    }
    let prepared = strategy_pipeline::prepare_production_scan(&mut world, player);
    let queries = board_scan::BoardScanQueries::prepare(&mut world);
    let before = BoardScan::collect(
        &world,
        player,
        prepared.as_ref(),
        &queries,
        ProductionPolicy::for_current_path(),
    )
    .unwrap();
    // ゲーム上の状態を変えずにECS内の格納先だけを変える。
    // 走査順が計画入力へ漏れると、同じseedでも攻撃順・購入編成が変わる。
    world.entity_mut(entities[0]).insert(CargoCapacity {
        max: 0,
        loaded: Vec::new(),
    });
    world.entity_mut(entities[2]).insert(CargoCapacity {
        max: 0,
        loaded: Vec::new(),
    });
    let prepared = strategy_pipeline::prepare_production_scan(&mut world, player);
    let queries = board_scan::BoardScanQueries::prepare(&mut world);
    let after = BoardScan::collect(
        &world,
        player,
        prepared.as_ref(),
        &queries,
        ProductionPolicy::for_current_path(),
    )
    .unwrap();
    let ids = |units: &[UnitSnapshot]| units.iter().map(|unit| unit.entity).collect::<Vec<_>>();
    assert_eq!(before.my_units.len(), 2);
    assert_eq!(before.enemy_units.len(), 2);
    assert_eq!(ids(&before.my_units), ids(&after.my_units));
    assert_eq!(ids(&before.enemy_units), ids(&after.enemy_units));
}

#[test]
fn map32_p2_small_map_expansion_opening_and_capture() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let registry = MasterDataRegistry::load().unwrap();
    let (mut world, mut schedule) = crate::setup::initialize_world_from_master_data_with_topology(
        &registry,
        "map_32",
        GridTopology::Hex,
    )
    .unwrap();

    // 戦闘乱数を固定し、実行時刻で中盤の防衛結果が変わらないようにする。
    world.insert_resource(crate::resources::GameRng::new(42));

    let p1 = PlayerId(1);
    let p2 = PlayerId(2);
    let mut settings = crate::ai::PlayerAiSettings::new();
    settings.set_version(p1, crate::ai::AiVersion::V200);
    settings.set_version(p2, crate::ai::AiVersion::V4);
    world.insert_resource(settings);

    // Turn 1 P1 full turn
    while let Some(_cmd) = crate::ai::execute_ai_turn(&mut world, p1) {
        schedule.run(&mut world);
    }
    schedule.run(&mut world);

    // Turn 1 P2 full turn: 自陣5都市に対する歩兵5体と、前線迎撃用のRecon1体が生産される
    let mut t1_p2_commands = Vec::new();
    while let Some(cmd) = crate::ai::execute_ai_turn(&mut world, p2) {
        t1_p2_commands.push(cmd);
        schedule.run(&mut world);
    }
    schedule.run(&mut world);

    let infantry_count = t1_p2_commands
        .iter()
        .filter(|c| c.contains("Infantry"))
        .count();
    let recon_count = t1_p2_commands
        .iter()
        .filter(|c| c.contains("Recon"))
        .count();
    assert!(
        infantry_count >= 5,
        "自陣5都市に対応する歩兵5体以上: {t1_p2_commands:?}"
    );
    assert!(
        recon_count >= 1,
        "初動で前線迎撃用のReconを生産すること: {t1_p2_commands:?}"
    );

    // Turn 2 P1 full turn
    while let Some(_cmd) = crate::ai::execute_ai_turn(&mut world, p1) {
        schedule.run(&mut world);
    }
    schedule.run(&mut world);

    // Turn 2 P2: 自陣都市の占領が即座に開始され、過剰な歩兵乱造が抑止される
    let mut t2_p2_commands = Vec::new();
    while let Some(cmd) = crate::ai::execute_ai_turn(&mut world, p2) {
        t2_p2_commands.push(cmd);
        schedule.run(&mut world);
    }

    let capture_count = t2_p2_commands
        .iter()
        .filter(|c| c.contains("Capture"))
        .count();
    assert!(
        capture_count >= 2,
        "Turn 2で自陣都市の占領を開始すること: {t2_p2_commands:?}"
    );

    // Turn 3 P1 full turn
    while let Some(_cmd) = crate::ai::execute_ai_turn(&mut world, p1) {
        schedule.run(&mut world);
    }
    schedule.run(&mut world);

    // Turn 3 P2
    let mut t3_p2_commands = Vec::new();
    while let Some(cmd) = crate::ai::execute_ai_turn(&mut world, p2) {
        t3_p2_commands.push(cmd);
        schedule.run(&mut world);
    }
    schedule.run(&mut world);

    // Turn 4 P1 full turn
    while let Some(_cmd) = crate::ai::execute_ai_turn(&mut world, p1) {
        schedule.run(&mut world);
    }
    schedule.run(&mut world);

    // Turn 4 P2
    let mut t4_p2_commands = Vec::new();
    while let Some(cmd) = crate::ai::execute_ai_turn(&mut world, p2) {
        t4_p2_commands.push(cmd);
        schedule.run(&mut world);
    }
    schedule.run(&mut world);

    // 5都市の所有者を検証
    let target_cities = [
        GridPosition { x: 4, y: 9 },
        GridPosition { x: 4, y: 8 },
        GridPosition { x: 3, y: 9 },
        GridPosition { x: 3, y: 8 },
        GridPosition { x: 2, y: 9 },
    ];
    for city in &target_cities {
        let owner = world
            .iter_entities()
            .find_map(|e| {
                let pos = e.get::<GridPosition>()?;
                let prop = e.get::<Property>()?;
                if pos == city {
                    Some(prop.owner_id)
                } else {
                    None
                }
            })
            .flatten();
        assert_eq!(
            owner,
            Some(p2),
            "City {:?} should be captured by P2 by T4",
            city
        );
    }

    println!("=== Starting T5-T15 trace ===");
    for turn in 5..=15 {
        // P1 turn
        while let Some(cmd) = crate::ai::execute_ai_turn(&mut world, p1) {
            if cmd.contains("Produce") || cmd.contains("Capture") {
                println!("  T{turn} P1: {cmd}");
            }
            schedule.run(&mut world);
        }
        schedule.run(&mut world);

        // P2 funds before turn
        let p2_funds = world
            .get_resource::<crate::resources::Players>()
            .and_then(|players| players.0.iter().find(|p| p.id == p2).map(|p| p.funds))
            .unwrap_or(0);
        println!("--- T{turn} P2 Start (Funds: {p2_funds}) ---");
        if let Some(manager) = world.get_resource::<crate::ai::squad::SquadManager>() {
            for s in &manager.squads {
                if s.plan.owner_id == Some(p2) {
                    println!(
                        "  Squad {:?}: mission={:?}, target={:?}, members={:?}",
                        s.id, s.plan.mission_type, s.plan.target, s.members
                    );
                }
            }
        }
        for (e, stats, pos, hp) in world
            .query::<(Entity, &UnitStats, &GridPosition, &Health)>()
            .iter(&world)
        {
            let is_p2 = world.get::<Faction>(e).map(|f| f.0 == p2).unwrap_or(false);
            if is_p2 {
                let squad_id = world
                    .get_resource::<crate::ai::squad::SquadManager>()
                    .and_then(|m| {
                        m.squads
                            .iter()
                            .find(|s| s.members.contains(&e))
                            .map(|s| s.id)
                    });
                println!(
                    "  P2 Unit {:?}: {:?} at {:?}, hp={}, squad={:?}",
                    e, stats.unit_type, pos, hp.current, squad_id
                );
            }
        }

        // P2 turn
        let mut p2_produced = Vec::new();
        while let Some(cmd) = crate::ai::execute_ai_turn(&mut world, p2) {
            if cmd.contains("Produce") {
                p2_produced.push(cmd.clone());
            }
            println!("  T{turn} P2: {cmd}");
            schedule.run(&mut world);
        }
        schedule.run(&mut world);

        // 5都市の状態と滞在ユニット
        println!("  T{turn} Cities Status:");
        for city in &target_cities {
            let owner = world
                .iter_entities()
                .find_map(|e| {
                    let pos = e.get::<GridPosition>()?;
                    let prop = e.get::<Property>()?;
                    if pos == city {
                        Some(prop.owner_id)
                    } else {
                        None
                    }
                })
                .flatten();
            let occupying_unit = world.iter_entities().find_map(|e| {
                let pos = e.get::<GridPosition>()?;
                let stats = e.get::<UnitStats>()?;
                let faction = e.get::<Faction>()?;
                let hp = e.get::<Health>()?;
                if pos == city {
                    Some(format!(
                        "P{}:{:?}(hp={})",
                        faction.0.0, stats.unit_type, hp.current
                    ))
                } else {
                    None
                }
            });
            println!(
                "    City ({},{}): owner={:?}, occupying={:?}",
                city.x, city.y, owner, occupying_unit
            );
        }

        // 西側防衛線 (x <= 5, y >= 7) のユニット
        let mut west_p2 = Vec::new();
        let mut west_p1 = Vec::new();
        for (_e, f, stats, pos, hp) in world
            .query::<(Entity, &Faction, &UnitStats, &GridPosition, &Health)>()
            .iter(&world)
        {
            if pos.x <= 5 && pos.y >= 7 {
                let desc = format!(
                    "{:?}@({},{})(hp={})",
                    stats.unit_type, pos.x, pos.y, hp.current
                );
                if f.0 == p1 {
                    west_p1.push(desc);
                } else if f.0 == p2 {
                    west_p2.push(desc);
                }
            }
        }
        println!(
            "  West Sector (x<=5, y>=7) P2 units ({}): {}",
            west_p2.len(),
            west_p2.join(", ")
        );
        println!(
            "  West Sector (x<=5, y>=7) P1 units ({}): {}",
            west_p1.len(),
            west_p1.join(", ")
        );
    }

    let p2_owned_target_cities = target_cities
        .iter()
        .filter(|city| {
            world.iter_entities().any(|e| {
                e.get::<GridPosition>().is_some_and(|p| p == *city)
                    && e.get::<Property>()
                        .is_some_and(|prop| prop.owner_id == Some(p2))
            })
        })
        .count();
    assert!(
        p2_owned_target_cities >= 3,
        "P2 は T15 時点でも西側重要都市を少なくとも3箇所以上維持防衛できているべき (実際: {p2_owned_target_cities})"
    );
}

/// 呼出し元のスレッド経路に引かれず、入口から渡した方策だけで枠を選ぶ。
#[test]
fn explicit_production_policy_controls_joint_capture_without_thread_path() {
    let mut op = operation(
        OperationKind::Capture,
        OperationSlots {
            capture_units: 3,
            combat_plan_required: 1,
            ..OperationSlots::default()
        },
        OperationSlots::default(),
    );
    op.property_controls.push(PropertyControlObjective {
        property: pos(4, 1),
        enemy_entity: Entity::from_raw(42),
        enemy_arrival_turn: 1,
        enemy_completion_turn: 3,
        attack_ready_turn: Some(2),
        approach: PropertyControlApproach::Interdict,
        disaggregated: false,
    });
    let operations = [op];
    let ledger = SlotLedger::new();
    for path in [
        crate::ai::code_path::CodePath::MainCompatible,
        crate::ai::code_path::CodePath::Extended,
    ] {
        let _guard = crate::ai::code_path::enter(path);
        assert_eq!(
            most_starved_slot(&operations, &ledger, ProductionPolicy::Standard),
            Some((0, SlotKind::Capture))
        );
        assert_eq!(
            most_starved_slot(&operations, &ledger, ProductionPolicy::SmallMap),
            Some((0, SlotKind::Combat))
        );
    }
}
