use super::combat_input::combat_plan_input;
use super::production_entry::has_direct_capture_route;
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
        capture_replacement_pending: false,
        island_id: crate::ai::islands::IslandId(0),
        kind: OperationKind::Capture,
        anchor,
        objective_properties: vec![anchor],
        capture_eta: None,
        required_capture_survivors: 0,
        logistics_rank: Some(0),
        forced_target_enemies: HashSet::default(),
        protected_capture_entities: HashSet::default(),
        staging_anchor: anchor,
        execution_authorized: true,
    }]
}

#[test]
fn campaign_objective_is_the_only_capture_source_and_keeps_exact_anchor() {
    let mut scan = multi_factory_scan();
    let campaign_anchor = pos(8, 2);
    scan.campaign_objectives = vec![CampaignPlanningObjective {
        capture_replacement_pending: false,
        island_id: crate::ai::islands::IslandId(0),
        kind: OperationKind::Capture,
        anchor: campaign_anchor,
        objective_properties: vec![campaign_anchor],
        capture_eta: Some(3),
        required_capture_survivors: 1,
        logistics_rank: Some(0),
        forced_target_enemies: HashSet::default(),
        protected_capture_entities: HashSet::default(),
        staging_anchor: campaign_anchor,
        execution_authorized: true,
    }];

    let operations = build_operations(
        &scan,
        &mut ReachCtx::default(),
        &[],
        PlayerId(1),
        ProductionPolicy::Standard,
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
    let mut scan = multi_factory_scan();
    let local_anchor = pos(6, 2);
    let capital = pos(8, 2);
    let second_front = pos(7, 1);
    let island_id = crate::ai::islands::IslandId(0);
    scan.open_properties = vec![local_anchor, second_front, capital];
    scan.campaign_objectives = vec![
        CampaignPlanningObjective {
            capture_replacement_pending: false,
            island_id,
            kind: OperationKind::Capture,
            anchor: local_anchor,
            objective_properties: vec![local_anchor, second_front],
            capture_eta: Some(3),
            required_capture_survivors: 1,
            logistics_rank: Some(0),
            forced_target_enemies: HashSet::default(),
            protected_capture_entities: HashSet::default(),
            staging_anchor: local_anchor,
            execution_authorized: true,
        },
        CampaignPlanningObjective {
            capture_replacement_pending: false,
            island_id,
            kind: OperationKind::AssaultCapital,
            anchor: capital,
            objective_properties: vec![capital],
            capture_eta: None,
            required_capture_survivors: 0,
            logistics_rank: None,
            forced_target_enemies: HashSet::default(),
            protected_capture_entities: HashSet::default(),
            staging_anchor: local_anchor,
            execution_authorized: false,
        },
    ];

    let operations = build_operations(
        &scan,
        &mut ReachCtx::default(),
        &[],
        PlayerId(1),
        ProductionPolicy::Standard,
    );
    let same_island = operations
        .iter()
        .filter(|operation| operation.island_id == Some(island_id))
        .collect::<Vec<_>>();
    assert_eq!(same_island.len(), 2);
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
    assert_eq!(assault.anchor, capital);
    assert_eq!(assault.objective_properties, vec![capital]);
    assert_eq!(assault.slots.capture_units, 1);
    assert!(!assault.execution_authorized);
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
            &[true, true],
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
            &[true, true],
        ),
        Some(0)
    );
    // 未許可の首都編成は敵の局地帰属先にしない。敵本拠地側からでも期限内に
    // Capture前線へ来られる敵は、実行中の局地護衛計画が引き受ける。
    assert_eq!(
        nearest_relevant_anchor_index(
            &scan,
            &mut ctx,
            &pos(8, 2),
            MovementType::Infantry,
            3,
            &anchors,
            &[3, 12],
            &[true, false],
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
        current_turn: 0,
        opening_policy: None,
        capital_position: None,
        property_entities: HashMap::default(),
        neutral_properties: Vec::new(),
        enemy_production_forecast: Default::default(),
        map: flat_map(20, 3).into(),
        master_data: MasterDataRegistry::load().unwrap().into(),
        damage_chart: damage_chart.into(),
        funds: 20_000,
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
        &[true, true],
        &[anchors[0]],
        &HashSet::default(),
        &HashSet::default(),
        2,
        PlayerId(1),
        ProductionPolicy::Standard,
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
        ProductionPolicy::Standard,
    );

    assert!(near.facts.friendly_combat_units_committed > 0);
    assert_eq!(far.facts.friendly_combat_units_committed, 0);
}

#[test]
fn current_mission_combat_is_not_filtered_by_purchase_plan() {
    let own_entity = Entity::from_raw(900);
    let foreign_entity = Entity::from_raw(901);
    let legacy_entity = Entity::from_raw(902);
    let own_plan = plan_revision::PlanId(7);
    let assignments = HashMap::from_iter(
        [
            (own_entity, Some(own_plan)),
            (foreign_entity, Some(plan_revision::PlanId(8))),
            (legacy_entity, None),
        ]
        .map(|(entity, plan_id)| {
            (
                entity,
                deployment::ActiveTargetAssignment {
                    plan_id,
                    slot_kind: SlotKind::Combat,
                    objective: Some(pos(6, 2)),
                    targets: HashSet::default(),
                },
            )
        }),
    );

    assert_eq!(
        committed_entities_for_operation(Some(&assignments), None, ProductionPolicy::Standard),
        HashSet::from_iter([own_entity, foreign_entity, legacy_entity]),
        "現在任務への一意配分後は、新規購入Planでも既存戦力を数える"
    );
    assert_eq!(
        committed_entities_for_operation(
            Some(&assignments),
            Some(own_plan),
            ProductionPolicy::Standard
        ),
        HashSet::from_iter([own_entity, foreign_entity, legacy_entity])
    );
}

#[test]
fn combat_plan_projects_only_the_capturer_ordered_this_turn() {
    let mut scan = mixed_threat_multi_factory_scan();
    let anchor = pos(6, 2);
    let second_front = pos(7, 2);
    scan.open_properties = vec![anchor, second_front];
    scan.campaign_objectives = vec![CampaignPlanningObjective {
        capture_replacement_pending: false,
        island_id: crate::ai::islands::IslandId(0),
        kind: OperationKind::Capture,
        anchor,
        objective_properties: vec![anchor, second_front],
        capture_eta: None,
        required_capture_survivors: 2,
        logistics_rank: Some(0),
        forced_target_enemies: HashSet::default(),
        protected_capture_entities: HashSet::default(),
        staging_anchor: anchor,
        execution_authorized: true,
    }];
    let mut ctx = ReachCtx::default();
    let mut operations = build_operations(
        &scan,
        &mut ctx,
        &[],
        PlayerId(1),
        ProductionPolicy::Standard,
    );
    let operation = operations
        .iter_mut()
        .find(|operation| operation.kind == OperationKind::Capture)
        .expect("局地Capture作戦");
    // 同じ生産手番で、占領兵1体を先に発注した状態を再現する。
    operation.filled.capture_units = 1;

    let input = standard_combat_plan_input(
        &scan,
        &mut ctx,
        operation,
        &HashSet::default(),
        &HashSet::default(),
        scan.funds,
        false,
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
            capture_replacement_pending: false,
            island_id: crate::ai::islands::IslandId(1),
            kind: OperationKind::AssaultCapital,
            anchor: capital,
            objective_properties: vec![capital],
            capture_eta: None,
            required_capture_survivors: 0,
            logistics_rank: None,
            forced_target_enemies: HashSet::default(),
            protected_capture_entities: HashSet::default(),
            staging_anchor: pos(0, 2),
            execution_authorized: false,
        },
        CampaignPlanningObjective {
            capture_replacement_pending: false,
            island_id: crate::ai::islands::IslandId(2),
            kind: OperationKind::Capture,
            anchor: capture,
            objective_properties: vec![capture],
            capture_eta: Some(4),
            required_capture_survivors: 3,
            logistics_rank: Some(0),
            forced_target_enemies: HashSet::default(),
            protected_capture_entities: HashSet::default(),
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
        enemy_forecast::ReinforcementScenario::Stress,
        ProductionPolicy::Standard,
    );
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
    let master = MasterDataRegistry::load().unwrap();
    let weapon_stats = master
        .create_unit_stats(&crate::resources::master_data::UnitName(
            unit_type.as_str().to_owned(),
        ))
        .unwrap();
    UnitStats {
        unit_type,
        cost,
        max_ammo1: weapon_stats.max_ammo1,
        max_ammo2: weapon_stats.max_ammo2,
        min_range: weapon_stats.min_range,
        max_range: weapon_stats.max_range,
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
    let scan = BoardScan {
        current_turn: 0,
        opening_policy: None,
        capital_position: None,
        property_entities: HashMap::default(),
        neutral_properties: Vec::new(),
        enemy_production_forecast: Default::default(),
        map: flat_map(10, 3).into(),
        master_data: MasterDataRegistry::load().unwrap().into(),
        damage_chart: damage_chart.into(),
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

    let (commands, trace) = plan_production(&scan, PlayerId(1), false, &HashMap::default());

    // 占領役を1体確保してもCombat計画を消さず、65%攻撃を何回実行できるかを
    // シミュレーションした攻撃列を保持する。実弾薬では1機が6回攻撃して
    // 3体を排除できるため、価格を敵価値へ換算せず必要な1機だけを生産する。
    assert_eq!(commands.len(), 2, "commands={commands:?}, trace={trace:?}");
    assert_eq!(
        commands
            .iter()
            .filter(|command| command.unit_type == UnitType::Infantry)
            .count(),
        1
    );
    assert_eq!(
        commands
            .iter()
            .filter(|command| command.unit_type == UnitType::Bcopters)
            .count(),
        1
    );
    let combat_plan = &trace.rolling_combat_plans[0];
    assert_eq!(combat_plan.targets.len(), 3);
    assert!(
        combat_plan
            .targets
            .iter()
            .all(|target| target.remaining_hp == 0),
        "combat_plan={combat_plan:?}"
    );
    assert_eq!(combat_plan.purchases.len(), 1);
}

/// テスト用の作戦。枠の充足状況だけを見たいので敵情報は空にしておく。
fn operation(kind: OperationKind, slots: OperationSlots, filled: OperationSlots) -> Operation {
    Operation {
        operation_id: None,
        capture_replacement_pending: false,
        property_control: None,
        property_controls: Vec::new(),
        capture_lane_targets: Vec::new(),
        planned_capture_units: Vec::new(),
        expected_reinforcements: Vec::new(),
        stress_reinforcement_funds: 0,
        kind,
        island_id: None,
        anchor: pos(0, 0),
        staging_anchor: pos(0, 0),
        execution_authorized: true,
        protected_capture_entities: HashSet::default(),
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

/// 期限付き物件制御を持つCaptureの扱いは経路で異なる（F12・F13の経路差を固定する）。
/// 通常経路は普通のCaptureとして順位付けし、占領枠も単独で埋める。拡張経路は
/// 首都本隊より前の緊急段へ上げ、初回編成の占領役を戦闘との共同計画へ回す。
#[test]
fn property_control_capture_urgency_differs_only_on_extended_path() {
    let slots = OperationSlots {
        capture_units: 3,
        combat_plan_required: 1,
        ..OperationSlots::default()
    };
    let mut initial_formation = operation(OperationKind::Capture, slots, OperationSlots::default());
    initial_formation
        .property_controls
        .push(PropertyControlObjective::interdict_for_test(pos(4, 1)));
    for (path, expected_rank, expected_slot) in [
        (
            crate::ai::code_path::CodePath::MainCompatible,
            4,
            SlotKind::Capture,
        ),
        (
            crate::ai::code_path::CodePath::Extended,
            1,
            SlotKind::Combat,
        ),
    ] {
        let _guard = crate::ai::code_path::enter(path);
        assert_eq!(
            operation_priority_rank(&initial_formation, ProductionPolicy::for_current_path()),
            expected_rank,
            "{path:?}"
        );
        assert_eq!(
            most_starved_slot(
                std::slice::from_ref(&initial_formation),
                &SlotLedger::new(),
                ProductionPolicy::for_current_path()
            ),
            Some((0, expected_slot)),
            "{path:?}"
        );
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
        current_turn: 0,
        opening_policy: None,
        capital_position: None,
        property_entities: HashMap::default(),
        neutral_properties: Vec::new(),
        enemy_production_forecast: Default::default(),
        map: strait_map(Some(Terrain::Shoal)).into(),
        master_data: MasterDataRegistry::load().unwrap().into(),
        damage_chart: DamageChart::new().into(),
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
        &[true, true],
        &[anchors[1]],
        &HashSet::default(),
        &HashSet::default(),
        3,
        PlayerId(1),
        ProductionPolicy::Standard,
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
        &[true],
        &[anchor],
        &HashSet::default(),
        &HashSet::default(),
        3,
        PlayerId(1),
        ProductionPolicy::Standard,
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
        &[true],
        &[anchor],
        &HashSet::default(),
        &HashSet::default(),
        3,
        PlayerId(1),
        ProductionPolicy::Standard,
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
        current_turn: 0,
        opening_policy: None,
        capital_position: None,
        property_entities: HashMap::default(),
        neutral_properties: Vec::new(),
        enemy_production_forecast: Default::default(),
        map: flat_map(9, 5).into(),
        master_data: MasterDataRegistry::load().unwrap().into(),
        damage_chart: DamageChart::new().into(),
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

/// 候補選定の共有骨格：予算内候補が予算超過候補より優先され、同値なら
/// 先に走査した施設が残る。適合度は価格除外より前に全施設×兵種で評価する。
#[test]
fn select_candidate_by_keeps_budget_priority_and_scan_order() {
    let scan = multi_factory_scan();
    let mut ctx = ReachCtx::default();
    let mut evaluated = Vec::new();
    let constraints = CandidateConstraints {
        remaining_funds: 7000,
        per_slot_budget: 1000,
    };
    // 戦車の方が正規化後も高いが、1枠予算を超えるので歩兵が選ばれる
    let selected = select_candidate_by(
        &scan,
        &mut ctx,
        SlotKind::Capture,
        &HashSet::default(),
        constraints,
        |_, facility, stats| {
            evaluated.push((*facility, stats.unit_type));
            Some(if stats.unit_type == UnitType::Tank {
                10.0
            } else {
                1.0
            })
        },
    )
    .unwrap();
    assert_eq!(selected.unit_type, UnitType::Infantry);
    assert_eq!(selected.facility, pos(1, 1));
    // 施設が外側・兵種が内側の順で全組合せを評価する
    assert_eq!(
        evaluated,
        vec![
            (pos(1, 1), UnitType::Infantry),
            (pos(1, 1), UnitType::Tank),
            (pos(1, 2), UnitType::Infantry),
            (pos(1, 2), UnitType::Tank),
            (pos(1, 3), UnitType::Infantry),
            (pos(1, 3), UnitType::Tank),
        ]
    );

    // 買えない戦車も適合度評価は行い、その後に価格で除外する
    let mut calls = 0;
    let selected = select_candidate_by(
        &scan,
        &mut ctx,
        SlotKind::Capture,
        &HashSet::default(),
        CandidateConstraints {
            remaining_funds: 1000,
            per_slot_budget: 10000,
        },
        |_, _, _| {
            calls += 1;
            Some(1.0)
        },
    )
    .unwrap();
    assert_eq!(calls, 6);
    assert_eq!(selected.unit_type, UnitType::Infantry);

    // 使用済み施設は飛ばし、厳密に良い施設だけが先頭候補を置き換える
    let used = HashSet::from_iter([pos(1, 1)]);
    let selected = select_candidate_by(
        &scan,
        &mut ctx,
        SlotKind::Capture,
        &used,
        constraints,
        |_, facility, _| Some(if *facility == pos(1, 3) { 2.0 } else { 1.0 }),
    )
    .unwrap();
    assert_eq!(selected.facility, pos(1, 3));
    let selected = select_candidate_by(
        &scan,
        &mut ctx,
        SlotKind::Capture,
        &used,
        constraints,
        |_, _, _| Some(1.0),
    )
    .unwrap();
    assert_eq!(selected.facility, pos(1, 2));
}

/// 見送り購入の共有骨格：一括編成の作戦だけが対象で、将来候補が厳密に良く、
/// 不足額を`RESERVATION_PATIENCE_TURNS`以内の収入で賄えるときだけ見送る。
#[test]
fn should_defer_purchase_by_respects_mode_and_patience_boundary() {
    let mut scan = multi_factory_scan();
    let mut ctx = ReachCtx::default();
    let mut op = operation(
        OperationKind::Capture,
        OperationSlots::default(),
        OperationSlots::default(),
    );
    let tank_better = |_: &mut ReachCtx, _: &GridPosition, stats: &UnitStats| {
        Some(if stats.unit_type == UnitType::Tank {
            70.0
        } else {
            1.0
        })
    };

    // 補充型の作戦は適合度を評価せずに見送らない
    let mut calls = 0;
    assert!(!should_defer_purchase_by(
        &scan,
        &mut ctx,
        &op,
        1000,
        1000,
        |_, _, _| {
            calls += 1;
            Some(1.0)
        },
    ));
    assert_eq!(calls, 0);

    op.facts.requires_transport = true;
    // 不足6000を収入1200なら5ターンで賄えるので見送る
    scan.my_income = 1200;
    assert!(should_defer_purchase_by(
        &scan,
        &mut ctx,
        &op,
        1000,
        1000,
        tank_better
    ));
    // 収入1199では6ターンかかり待機期限を超える
    scan.my_income = 1199;
    assert!(!should_defer_purchase_by(
        &scan,
        &mut ctx,
        &op,
        1000,
        1000,
        tank_better
    ));
    // 正規化後の適合度が同値なら見送らない（厳密比較）
    scan.my_income = 1200;
    assert!(!should_defer_purchase_by(
        &scan,
        &mut ctx,
        &op,
        1000,
        1000,
        |_, _, stats| Some(if stats.unit_type == UnitType::Tank {
            7.0
        } else {
            1.0
        }),
    ));
    // 収入が無ければ見送らない
    scan.my_income = 0;
    assert!(!should_defer_purchase_by(
        &scan,
        &mut ctx,
        &op,
        1000,
        1000,
        tank_better
    ));
}

/// 撃破順：優先敵が無い（または候補外の）ときは従来順、候補内なら優先敵だけが先頭。
#[test]
fn planned_deployment_moves_only_eligible_preferred_enemy_first() {
    let mut scan = multi_factory_scan();
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Tank, UnitType::Infantry, 75);
    scan.damage_chart = chart.into();
    let infantry = scan.available_types[0].1.clone();
    let threat = |raw: u32, position: GridPosition| ThreatTarget {
        entity: Some(Entity::from_raw(raw)),
        stats: infantry.clone(),
        position,
        current_hp: 100,
        available_turn: 0,
    };
    let mut op = operation(
        OperationKind::Capture,
        OperationSlots::default(),
        OperationSlots::default(),
    );
    // 同分類・同攻撃回数なので、従来順は座標（x→y）で決まる
    op.unreachable_threats = vec![
        threat(30, pos(6, 3)),
        threat(10, pos(5, 1)),
        threat(20, pos(5, 3)),
    ];
    let candidate = SlotCandidate {
        unit_type: UnitType::Tank,
        cost: 7000,
        facility: pos(1, 1),
        fitness: 1.0,
    };
    let mut ctx = ReachCtx::default();
    let mut order = |preferred: Option<Entity>| {
        planned_deployment(
            &scan,
            &mut ctx,
            &op,
            SlotKind::Intercept,
            &candidate,
            preferred,
        )
        .unwrap()
        .priority_enemies
    };
    let legacy = vec![
        Entity::from_raw(10),
        Entity::from_raw(20),
        Entity::from_raw(30),
    ];
    assert_eq!(order(None), legacy);
    assert_eq!(order(Some(Entity::from_raw(99))), legacy);
    assert_eq!(
        order(Some(Entity::from_raw(30))),
        vec![
            Entity::from_raw(30),
            Entity::from_raw(10),
            Entity::from_raw(20),
        ]
    );
}

/// 呼び出しを記録する占領枠policy。
struct RecordingCapture {
    anchor: GridPosition,
    anchor_calls: std::cell::Cell<u32>,
    fitness_anchor: std::cell::Cell<Option<GridPosition>>,
}

impl CaptureSlotPolicy for RecordingCapture {
    fn anchor(&self, _op: &Operation) -> GridPosition {
        self.anchor_calls.set(self.anchor_calls.get() + 1);
        self.anchor
    }

    fn fitness(
        &self,
        _scan: &BoardScan,
        _ctx: &mut ReachCtx,
        anchor: GridPosition,
        _facility: &GridPosition,
        _stats: &UnitStats,
    ) -> Option<f32> {
        self.fitness_anchor.set(Some(anchor));
        Some(3.5)
    }
}

/// 枠の適合度：占領枠policyはCapture枠だけで使われ、能力・参加可否を通過した
/// 候補にだけ適合度を問い合わせる。輸送・迎撃枠では一切呼ばれない。
#[test]
fn slot_fitness_consults_capture_policy_only_for_capture_slots() {
    let scan = multi_factory_scan();
    let mut ctx = ReachCtx::default();
    let op = operation(
        OperationKind::Capture,
        OperationSlots::default(),
        OperationSlots::default(),
    );
    let infantry = scan.available_types[0].1.clone();
    let tank = scan.available_types[1].1.clone();
    let policy = RecordingCapture {
        anchor: pos(6, 2),
        anchor_calls: Default::default(),
        fitness_anchor: Default::default(),
    };
    let facility = pos(1, 1);

    for kind in [SlotKind::Transport, SlotKind::Intercept, SlotKind::Combat] {
        assert_eq!(
            slot_fitness(&scan, &mut ctx, &op, kind, &facility, &infantry, &policy),
            None
        );
    }
    assert_eq!(policy.anchor_calls.get(), 0);
    assert_eq!(policy.fitness_anchor.get(), None);

    // 占領能力が無ければ地点だけ決めて、適合度は問い合わせない
    assert_eq!(
        slot_fitness(
            &scan,
            &mut ctx,
            &op,
            SlotKind::Capture,
            &facility,
            &tank,
            &policy
        ),
        None
    );
    assert_eq!(policy.anchor_calls.get(), 1);
    assert_eq!(policy.fitness_anchor.get(), None);

    // 参加可能な占領兵には、policyが決めた地点で適合度を問い合わせる
    assert_eq!(
        slot_fitness(
            &scan,
            &mut ctx,
            &op,
            SlotKind::Capture,
            &facility,
            &infantry,
            &policy
        ),
        Some(3.5)
    );
    assert_eq!(policy.fitness_anchor.get(), Some(pos(6, 2)));
    // 通常経路は参加可能なら同じ適合度
    assert_eq!(
        slot_fitness(
            &scan,
            &mut ctx,
            &op,
            SlotKind::Capture,
            &facility,
            &infantry,
            &ProductionPolicy::Standard.capture_evaluation(PlayerId(1))
        ),
        Some(1.0)
    );
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
        current_turn: 0,
        opening_policy: None,
        capital_position: None,
        property_entities: HashMap::default(),
        neutral_properties: Vec::new(),
        enemy_production_forecast: Default::default(),
        map: flat_map(9, 5).into(),
        master_data: MasterDataRegistry::load().unwrap().into(),
        damage_chart: damage_chart.into(),
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
        current_turn: 0,
        opening_policy: None,
        capital_position: None,
        property_entities: HashMap::default(),
        neutral_properties: Vec::new(),
        enemy_production_forecast: Default::default(),
        map: map.into(),
        master_data: MasterDataRegistry::load().unwrap().into(),
        damage_chart: damage_chart.into(),
        funds: 20_000,
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

/// 同一手番の混成パッケージに、航空・地上の両脅威へ有効なunitを含める。
#[test]
fn multi_factory_plan_switches_after_air_threat_is_covered() {
    let scan = mixed_threat_multi_factory_scan();
    let (commands, trace) = plan_production(&scan, PlayerId(0), true, &HashMap::default());
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

    let (commands, trace) = plan_production(&scan, PlayerId(0), true, &HashMap::default());

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
    let (commands, trace) = plan_production(&scan, PlayerId(0), true, &HashMap::default());

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
        ProductionPolicy::Standard,
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

    assert_eq!(
        most_starved_slot(
            &ops,
            &SlotLedger::new(),
            ProductionPolicy::for_current_path()
        ),
        Some((1, SlotKind::Combat))
    );
}

/// 同一前線では占領兵を全数先買いせず、最初の占領兵に続いて掃討計画を起動する。
#[test]
fn combat_plan_follows_first_capture_before_remaining_fronts_expand() {
    let slots = OperationSlots {
        capture_units: 8,
        combat_plan_required: 1,
        ..OperationSlots::default()
    };
    let empty = operation(OperationKind::Capture, slots, OperationSlots::default());
    assert_eq!(
        most_starved_slot(
            &[empty],
            &SlotLedger::new(),
            ProductionPolicy::for_current_path()
        ),
        Some((0, SlotKind::Capture))
    );

    let first_capture_ready = operation(
        OperationKind::Capture,
        slots,
        OperationSlots {
            capture_units: 1,
            ..OperationSlots::default()
        },
    );
    assert_eq!(
        most_starved_slot(
            &[first_capture_ready],
            &SlotLedger::new(),
            ProductionPolicy::for_current_path()
        ),
        Some((0, SlotKind::Combat))
    );
}

/// 前線の分類はmap名ではなく、占領可能unitの実到達性だけで決める。
#[test]
fn direct_front_uses_operation_production_while_strait_keeps_transport_campaign() {
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

    assert_eq!(
        most_starved_slot(
            &ops,
            &SlotLedger::new(),
            ProductionPolicy::for_current_path()
        ),
        Some((0, SlotKind::Combat))
    );
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
    assert_eq!(
        most_starved_slot(
            &ops,
            &SlotLedger::new(),
            ProductionPolicy::for_current_path()
        ),
        Some((2, SlotKind::Combat))
    );
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

/// 島作戦の不足はV4汎用作戦より先に発注し、不完全なパッケージの途中で
/// 余った施設を汎用生産へ開放しない。
#[test]
fn v4_prioritizes_campaign_package_and_blocks_generic_when_incomplete() {
    use crate::ai::island_campaign::{
        IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignPortfolio,
        IslandCampaignRequirement,
    };
    use crate::ai::islands::IslandId;
    use crate::ai::planning::decision::AdoptedTurnPortfolio;
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
    let mut cache = AdoptedTurnPortfolio::default();
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
            .resource::<ProductionTurnState>()
            .campaign_production_blocks_generic(player_id)
    );
}

// 標準経路の既存fixtureはStage入力を持たない。共通APIへ空集合だけを補う。
fn standard_combat_plan_input(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    used: &HashSet<GridPosition>,
    committed: &HashSet<Entity>,
    funds: u32,
    self_deployment: bool,
) -> Option<RollingPlanInput> {
    combat_plan_input(
        scan,
        ctx,
        op,
        used,
        committed,
        &HashSet::default(),
        funds,
        self_deployment,
        PlayerId(1),
        ProductionPolicy::Standard,
    )
}
