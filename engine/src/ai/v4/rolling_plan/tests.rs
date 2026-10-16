use super::force_package::{plan_force_package, update_best_feasible};
use super::frontier::{
    property_control_frontier_key, property_control_state_limit, retain_property_control_frontier,
};
use super::model::{
    MANY_FACTORY_PROPERTY_STATE_LIMIT, OpeningProductionPolicy, PlannedCapturePurchase,
    ProductionAttackProjection,
};
use super::plan_ranking::{
    exact_property_plan_better, opening_package_spends_all_usable_funds,
    update_best_effort_interdiction, update_best_sufficient_interdiction,
    uses_small_map_opening_rules,
};
use super::{
    Arc, DEFAULT_SEARCH_TURNS, DamageChart, EnemyPlanUnit, Entity, FixedPackageError,
    FriendlyPlanUnit, GridPosition, HashSet, Map, MasterDataRegistry, MovementType,
    PlannedPurchase, ProductionPlanOption, ProductionSlot, RollingPlanInput, SEARCH_BEAM_WIDTH,
    SearchState, SimulationCatalog, TargetForecast, Terrain, UnitStats, UnitType,
    evaluate_fixed_package, fill_best_effort_current_screen, funding_suffices,
    left_shift_purchases, simulate_state,
};

use crate::resources::GridTopology;
use crate::resources::master_data::UnitName;

pub(super) fn stats(unit_type: UnitType, cost: u32, movement: u32) -> UnitStats {
    let master = MasterDataRegistry::load().unwrap();
    let weapon_stats = master
        .create_unit_stats(&UnitName(unit_type.as_str().to_owned()))
        .unwrap();
    UnitStats {
        unit_type,
        cost,
        max_movement: movement,
        movement_type: MovementType::Air,
        max_fuel: 99,
        max_ammo1: weapon_stats.max_ammo1,
        max_ammo2: weapon_stats.max_ammo2,
        min_range: weapon_stats.min_range,
        max_range: weapon_stats.max_range,
        ..UnitStats::mock()
    }
}

pub(super) fn input() -> RollingPlanInput {
    let map = Map::new(12, 1, Terrain::Plains, GridTopology::Square);
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 40);
    chart.insert_damage(UnitType::Bomber, UnitType::Infantry, 100);
    chart.insert_damage(UnitType::Infantry, UnitType::Bcopters, 10);
    RollingPlanInput {
        map: Arc::new(map),
        master_data: Arc::new(MasterDataRegistry::load().unwrap()),
        damage_chart: Arc::new(chart),
        current_turn: 1,
        opening_policy: OpeningProductionPolicy::Standard,
        existing_units: Vec::new(),
        protected_units: Vec::new(),
        enemies: vec![EnemyPlanUnit {
            entity: Some(Entity::from_raw(7)),
            stats: UnitStats {
                can_capture: true,
                ..stats(UnitType::Infantry, 1_000, 3)
            },
            position: GridPosition { x: 8, y: 0 },
            hp: 100,
            defense_bonus: 0,
            available_turn: 0,
        }],
        production_options: vec![
            ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility: GridPosition { x: 0, y: 0 },
                    unit_type: UnitType::Bcopters,
                    build_turn: 0,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
                capture_durability: None,
                capture_blocking_enemy_index: None,
            },
            ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility: GridPosition { x: 1, y: 0 },
                    unit_type: UnitType::Bomber,
                    build_turn: 0,
                    cost: 20_000,
                },
                stats: stats(UnitType::Bomber, 20_000, 6),
                engageable_enemy_indices: vec![0],
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
                capture_durability: None,
                capture_blocking_enemy_index: None,
            },
        ],
        production_attack_projections: Vec::new(),
        deadline_target_index: None,
        interdiction_deadlines: Vec::new(),
        current_funds: 30_000,
        income_per_turn: 0,
        hard_deadline: None,
        capture_completion_turn: None,
        required_capture_survivors: 0,
        delay_cost_per_turn: 5_000,
        search_beam_width: SEARCH_BEAM_WIDTH,
        prioritize_deadline_interdiction: false,
        exact_property_control: false,
    }
}

#[test]
fn indirect_fire_requires_an_off_factory_shot_after_direct_fire_saturates_the_target() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    // 中央の同じ敵へ直接部隊が二体集まり、一体分が余っているとする。ここでは、
    // 工場外から撃てる間接部隊が直接部隊の担当を置き換えず火力を増やせる。
    input.existing_units.push(FriendlyPlanUnit {
        stats: stats(UnitType::Bcopters, 7_500, 6),
        position: GridPosition { x: 7, y: 0 },
        hp: 100,
        available_turn: 0,
        engageable_enemy_indices: vec![0],
        protection_completion_turn: None,
    });
    input.existing_units.push(FriendlyPlanUnit {
        stats: stats(UnitType::Bcopters, 7_500, 6),
        position: GridPosition { x: 9, y: 0 },
        hp: 100,
        available_turn: 0,
        engageable_enemy_indices: vec![0],
        protection_completion_turn: None,
    });
    let indirect = ProductionPlanOption {
        purchase: PlannedPurchase {
            facility: GridPosition { x: 1, y: 0 },
            unit_type: UnitType::Rockets,
            build_turn: 0,
            cost: 6_000,
        },
        stats: stats(UnitType::Rockets, 6_000, 5),
        engageable_enemy_indices: vec![0],
        capture_target: None,
        capture_arrival_turn: None,
        capture_completion_turn: None,
        capture_durability: None,
        capture_blocking_enemy_index: None,
    };
    input.production_options.push(indirect.clone());
    input.production_attack_projections = vec![
        vec![None],
        vec![None],
        vec![Some(ProductionAttackProjection {
            ready_turn: 2,
            firing_position: GridPosition { x: 6, y: 0 },
            requires_movement: true,
        })],
    ];
    let mut plan = plan_force_package(&input).expect("a plan");
    plan.purchases = vec![indirect.purchase];

    assert!(plan.indirect_fire_has_local_support(&input));

    input.production_attack_projections[2][0]
        .as_mut()
        .expect("projection")
        .firing_position = indirect.purchase.facility;
    assert!(!plan.indirect_fire_has_local_support(&input));
}

#[test]
fn infeasible_plan_spends_an_idle_current_slot_before_late_purchases() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    input.current_funds = 8_500;
    Arc::make_mut(&mut input.damage_chart).insert_damage(
        UnitType::Infantry,
        UnitType::Infantry,
        55,
    );
    Arc::make_mut(&mut input.damage_chart).insert_damage(
        UnitType::Infantry,
        UnitType::Bcopters,
        10,
    );
    input.production_options.push(ProductionPlanOption {
        purchase: PlannedPurchase {
            facility: GridPosition { x: 1, y: 0 },
            unit_type: UnitType::Infantry,
            build_turn: 0,
            cost: 1_000,
        },
        stats: UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1_000,
            can_capture: true,
            max_movement: 3,
            movement_type: MovementType::Infantry,
            max_fuel: 99,
            max_ammo1: 9,
            min_range: 1,
            max_range: 1,
            ..UnitStats::mock()
        },
        engageable_enemy_indices: vec![0],
        capture_target: None,
        capture_arrival_turn: None,
        capture_completion_turn: None,
        capture_durability: None,
        capture_blocking_enemy_index: None,
    });
    let state = SearchState {
        option_indices: vec![0],
        used_slots: HashSet::from_iter([ProductionSlot {
            facility: GridPosition { x: 0, y: 0 },
            build_turn: 0,
        }]),
        used_capture_targets: HashSet::default(),
        cost: 7_500,
    };
    let mut selected = simulate_state(&input, &state, DEFAULT_SEARCH_TURNS);
    selected.feasible = false;

    let catalog = SimulationCatalog::new(&input);
    let augmented =
        fill_best_effort_current_screen(&input, &catalog, selected, DEFAULT_SEARCH_TURNS);

    assert!(
        augmented.purchases.iter().any(|purchase| {
            purchase.build_turn == 0 && purchase.unit_type == UnitType::Infantry
        })
    );
    assert_eq!(
        augmented
            .purchases
            .iter()
            .filter(|purchase| purchase.build_turn == 0)
            .count(),
        2
    );
}

/// 占領期限がある場合、兵種の移動力ではなくシミュレーション上の初撃時刻を優先する。
#[test]
fn deadline_fallback_prefers_earlier_simulated_interdiction() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut early = plan_force_package(&input()).unwrap();
    early.feasible = false;
    early.first_attack_turn = Some(1);
    early.target_forecasts[0].remaining_hp = 60;
    early.expected_loss = 4_000;
    early.production_cost = 8_000;

    let mut late = early.clone();
    late.first_attack_turn = Some(2);
    late.target_forecasts[0].remaining_hp = 0;
    late.expected_loss = 0;
    late.production_cost = 1_000;

    let mut selected = None;
    update_best_effort_interdiction(&mut selected, &late, None);
    update_best_effort_interdiction(&mut selected, &early, None);

    assert_eq!(selected.unwrap().first_attack_turn, Some(1));
}

/// 同じ初撃時刻なら、敵残HPをより削れる案（高い軍事成果）を優先する。
#[test]
fn deadline_fallback_prefers_lower_remaining_hp_after_equal_attack_turn() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut cheap = plan_force_package(&input()).unwrap();
    cheap.feasible = false;
    cheap.first_attack_turn = Some(1);
    cheap.target_forecasts[0].remaining_hp = 60;
    cheap.production_cost = 1_000;

    let mut expensive = cheap.clone();
    expensive.target_forecasts[0].remaining_hp = 20;
    expensive.production_cost = 20_000;

    let mut selected = None;
    update_best_effort_interdiction(&mut selected, &expensive, None);
    update_best_effort_interdiction(&mut selected, &cheap, None);

    assert_eq!(selected.unwrap().production_cost, 20_000);
}

/// 期限前の初撃が成立した案同士でも、敵残HPをより削れる案を優先する。
#[test]
fn sufficient_interdiction_prefers_higher_enemy_hp_reduction() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut early_expensive = plan_force_package(&input()).unwrap();
    early_expensive.feasible = false;
    early_expensive.first_attack_turn = Some(1);
    early_expensive.target_forecasts[0].remaining_hp = 20;
    early_expensive.production_cost = 20_000;

    let mut timely_cheap = early_expensive.clone();
    timely_cheap.first_attack_turn = Some(2);
    timely_cheap.target_forecasts[0].remaining_hp = 60;
    timely_cheap.production_cost = 4_000;

    let mut selected = None;
    update_best_sufficient_interdiction(&mut selected, &early_expensive, None);
    update_best_sufficient_interdiction(&mut selected, &timely_cheap, None);

    assert_eq!(selected.unwrap().production_cost, 20_000);
}

/// 同じ敵への一撃を複数物件の妨害として二重計上しない。
#[test]
fn interdiction_requires_a_legal_attack_for_every_distinct_contract() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut plan = plan_force_package(&input()).unwrap();
    plan.target_first_attack_turns = vec![Some(1), None];
    let deadlines = vec![(0, 2), (1, 3)];

    assert_eq!(plan.missed_interdiction_contracts(&deadlines), 1);
    assert!(!plan.interdiction_contracts_satisfied(&deadlines));

    plan.target_first_attack_turns[1] = Some(3);
    assert_eq!(plan.missed_interdiction_contracts(&deadlines), 0);
    assert!(plan.interdiction_contracts_satisfied(&deadlines));
}

#[test]
fn feasible_selection_prefers_a_plan_that_keeps_the_expected_overmatch_margin() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut without_margin = plan_force_package(&input()).unwrap();
    without_margin.production_cost = 7_500;
    without_margin.expected_loss = 0;
    without_margin.overmatch_ready = false;
    let mut with_margin = without_margin.clone();
    with_margin.production_cost = 12_000;
    with_margin.surviving_combat_value = 6_000;
    with_margin.required_overmatch_value = 2_000;
    with_margin.overmatch_ready = true;
    let mut selected = None;

    let input = input();
    update_best_feasible(&input, &mut selected, &without_margin, 0);
    update_best_feasible(&input, &mut selected, &with_margin, 0);

    assert!(selected.expect("a feasible candidate").overmatch_ready);
}

#[test]
fn dominated_candidate_in_the_same_production_slot_is_pruned() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    input.production_options.truncate(1);
    let dominated = ProductionPlanOption {
        purchase: PlannedPurchase {
            facility: GridPosition { x: 0, y: 0 },
            unit_type: UnitType::Bcopters,
            build_turn: 0,
            cost: 8_000,
        },
        stats: input.production_options[0].stats.clone(),
        engageable_enemy_indices: vec![0],
        capture_target: None,
        capture_arrival_turn: None,
        capture_completion_turn: None,
        capture_durability: None,
        capture_blocking_enemy_index: None,
    };
    input.production_options.push(dominated);

    let plan = plan_force_package(&input).expect("a plan");

    assert_eq!(plan.candidates_pruned, 1);
    assert!(plan.purchases.iter().all(|purchase| purchase.cost != 8_000));
}

#[test]
fn capture_unit_attacks_blocker_then_completes_with_one_action_per_turn() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    input.production_options = vec![ProductionPlanOption {
        purchase: PlannedPurchase {
            facility: GridPosition { x: 0, y: 0 },
            unit_type: UnitType::Infantry,
            build_turn: 0,
            cost: 1_000,
        },
        stats: UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1_000,
            can_capture: true,
            max_movement: 3,
            movement_type: MovementType::Infantry,
            max_fuel: 99,
            max_ammo1: 9,
            min_range: 1,
            max_range: 1,
            ..UnitStats::mock()
        },
        engageable_enemy_indices: vec![0],
        capture_target: Some(GridPosition { x: 1, y: 0 }),
        capture_arrival_turn: Some(1),
        capture_completion_turn: Some(2),
        capture_durability: Some(200),
        capture_blocking_enemy_index: Some(0),
    }];
    input.production_attack_projections = vec![vec![Some(ProductionAttackProjection {
        ready_turn: 1,
        firing_position: GridPosition { x: 0, y: 0 },
        requires_movement: false,
    })]];
    input.current_funds = 1_000;
    input.income_per_turn = 0;
    input.hard_deadline = Some(3);
    input.required_capture_survivors = 1;
    input.exact_property_control = true;
    Arc::make_mut(&mut input.damage_chart).insert_damage(
        UnitType::Infantry,
        UnitType::Infantry,
        100,
    );

    let plan = plan_force_package(&input).expect("同じ歩兵が排除後に占領する合同案");

    assert_eq!(plan.purchases.len(), 1, "購入と費用を二重計上しない");
    assert_eq!(plan.production_cost, 1_000);
    assert_eq!(plan.first_attack_turn, Some(1), "生産当日には攻撃しない");
    assert_eq!(plan.target_forecasts[0].destroyed_turn, Some(1));
    assert_eq!(plan.capture_purchases[0].completion_turn, Some(3));
    assert_eq!(plan.deadline_capture_survivor_count, 1);
    assert_eq!(plan.turn_forecasts[0].attack_count, 1);
    assert_eq!(
        plan.turn_forecasts[1].attack_count, 0,
        "占領手番には攻撃しない"
    );
    assert_eq!(
        plan.turn_forecasts[2].attack_count, 0,
        "占領手番には攻撃しない"
    );
}

#[test]
fn occupation_uses_live_campaign_eta_instead_of_a_fixed_delay() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    input.capture_completion_turn = Some(7);
    input.required_capture_survivors = 1;
    input.protected_units.push(FriendlyPlanUnit {
        stats: UnitStats {
            unit_type: UnitType::Infantry,
            can_capture: true,
            ..UnitStats::mock()
        },
        position: GridPosition { x: 8, y: 0 },
        hp: 100,
        available_turn: 0,
        engageable_enemy_indices: Vec::new(),
        protection_completion_turn: None,
    });

    let plan = plan_force_package(&input).unwrap();

    assert!(plan.elimination_turn.is_some_and(|turn| turn <= 7));
    assert_eq!(plan.occupation_turn, Some(7));
}

#[test]
fn combat_capturer_counts_once_as_a_surviving_occupation_unit() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    input.production_options.clear();
    input.existing_units = vec![FriendlyPlanUnit {
        stats: UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1_000,
            can_capture: true,
            max_ammo1: 9,
            max_fuel: 99,
            max_movement: 3,
            movement_type: MovementType::Infantry,
            min_range: 1,
            max_range: 1,
            ..UnitStats::mock()
        },
        position: GridPosition { x: 8, y: 0 },
        hp: 100,
        available_turn: 0,
        engageable_enemy_indices: vec![0],
        protection_completion_turn: None,
    }];
    Arc::make_mut(&mut input.damage_chart).insert_damage(
        UnitType::Infantry,
        UnitType::Infantry,
        100,
    );
    input.capture_completion_turn = Some(2);
    input.required_capture_survivors = 1;

    let plan = plan_force_package(&input).expect("歩兵だけで撃破と占領を継続できる");

    assert!(plan.feasible);
    assert_eq!(plan.protected_unit_count, 0);
    assert_eq!(plan.protected_survivor_count, 1);
    assert_eq!(plan.occupation_turn, Some(2));
}

#[test]
fn losing_one_of_two_required_capturers_makes_the_plan_infeasible() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    input.production_options.clear();
    Arc::make_mut(&mut input.damage_chart).insert_damage(
        UnitType::Infantry,
        UnitType::Infantry,
        100,
    );
    input.existing_units.push(FriendlyPlanUnit {
        stats: stats(UnitType::Bomber, 20_000, 6),
        position: GridPosition { x: 6, y: 0 },
        hp: 100,
        // 敵を最終的には倒せるが、占領兵が先に損耗する状況を作る。
        available_turn: 2,
        engageable_enemy_indices: vec![0],
        protection_completion_turn: None,
    });
    let capturer = FriendlyPlanUnit {
        stats: UnitStats {
            unit_type: UnitType::Infantry,
            can_capture: true,
            ..UnitStats::mock()
        },
        position: GridPosition { x: 7, y: 0 },
        hp: 100,
        available_turn: 0,
        engageable_enemy_indices: Vec::new(),
        protection_completion_turn: None,
    };
    input.protected_units = vec![capturer.clone(), capturer];
    input.capture_completion_turn = Some(3);
    input.required_capture_survivors = 2;

    let plan = plan_force_package(&input).unwrap();

    assert_eq!(plan.elimination_turn, Some(3));
    assert_eq!(plan.protected_unit_count, 2);
    assert_eq!(plan.protected_survivor_count, 0);
    assert_eq!(plan.required_capture_survivor_count, 2);
    assert!(!plan.feasible);
    assert_eq!(plan.occupation_turn, None);
}

#[test]
fn fixed_formation_uses_a_new_free_facility_without_changing_composition() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    let old_facility = GridPosition { x: 0, y: 0 };
    let new_facility = GridPosition { x: 2, y: 0 };
    input.production_options = vec![
        ProductionPlanOption {
            purchase: PlannedPurchase {
                facility: new_facility,
                unit_type: UnitType::Bcopters,
                build_turn: 0,
                cost: 7_500,
            },
            stats: stats(UnitType::Bcopters, 7_500, 6),
            engageable_enemy_indices: vec![0],
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
            capture_durability: None,
            capture_blocking_enemy_index: None,
        },
        ProductionPlanOption {
            purchase: PlannedPurchase {
                facility: old_facility,
                unit_type: UnitType::Bcopters,
                build_turn: 1,
                cost: 7_500,
            },
            stats: stats(UnitType::Bcopters, 7_500, 6),
            engageable_enemy_indices: vec![0],
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
            capture_durability: None,
            capture_blocking_enemy_index: None,
        },
    ];

    let plan = evaluate_fixed_package(
        &input,
        &[PlannedPurchase {
            facility: old_facility,
            unit_type: UnitType::Bcopters,
            build_turn: 1,
            cost: 7_500,
        }],
    )
    .unwrap();

    assert_eq!(plan.purchases.len(), 1);
    assert_eq!(plan.purchases[0].facility, new_facility);
    assert_eq!(plan.purchases[0].build_turn, 0);
    assert_eq!(plan.purchases[0].unit_type, UnitType::Bcopters);
}

#[test]
fn schedule_compaction_does_not_steal_an_unprocessed_original_slot() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    let facility = GridPosition { x: 0, y: 0 };
    input.production_options = vec![
        ProductionPlanOption {
            purchase: PlannedPurchase {
                facility,
                unit_type: UnitType::Bcopters,
                build_turn: 1,
                cost: 7_500,
            },
            stats: stats(UnitType::Bcopters, 7_500, 6),
            engageable_enemy_indices: vec![0],
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
            capture_durability: None,
            capture_blocking_enemy_index: None,
        },
        ProductionPlanOption {
            purchase: PlannedPurchase {
                facility,
                unit_type: UnitType::Bomber,
                build_turn: 1,
                cost: 20_000,
            },
            stats: stats(UnitType::Bomber, 20_000, 6),
            engageable_enemy_indices: vec![0],
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
            capture_durability: None,
            capture_blocking_enemy_index: None,
        },
    ];
    let original = vec![
        PlannedPurchase {
            facility,
            unit_type: UnitType::Bcopters,
            build_turn: 0,
            cost: 7_500,
        },
        PlannedPurchase {
            facility,
            unit_type: UnitType::Bomber,
            build_turn: 1,
            cost: 20_000,
        },
    ];

    let shifted = left_shift_purchases(&input, &original);
    let slots = shifted
        .iter()
        .map(|purchase| (purchase.facility, purchase.build_turn))
        .collect::<HashSet<_>>();

    assert_eq!(slots.len(), shifted.len());
    assert!(matches!(
        evaluate_fixed_package(&input, &original),
        Err(FixedPackageError::ProductionSlotUnavailable)
    ));
}

#[test]
fn schedule_compaction_preserves_funding_for_later_original_purchases() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    input.current_funds = 7_500;
    input.income_per_turn = 10_000;
    let facility = GridPosition { x: 0, y: 0 };
    input.production_options = vec![
        ProductionPlanOption {
            purchase: PlannedPurchase {
                facility,
                unit_type: UnitType::Bcopters,
                build_turn: 0,
                cost: 7_500,
            },
            stats: stats(UnitType::Bcopters, 7_500, 6),
            engageable_enemy_indices: vec![0],
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
            capture_durability: None,
            capture_blocking_enemy_index: None,
        },
        ProductionPlanOption {
            purchase: PlannedPurchase {
                facility,
                unit_type: UnitType::Bcopters,
                build_turn: 1,
                cost: 7_500,
            },
            stats: stats(UnitType::Bcopters, 7_500, 6),
            engageable_enemy_indices: vec![0],
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
            capture_durability: None,
            capture_blocking_enemy_index: None,
        },
        ProductionPlanOption {
            purchase: PlannedPurchase {
                facility,
                unit_type: UnitType::Bomber,
                build_turn: 2,
                cost: 20_000,
            },
            stats: stats(UnitType::Bomber, 20_000, 6),
            engageable_enemy_indices: vec![0],
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
            capture_durability: None,
            capture_blocking_enemy_index: None,
        },
    ];
    let original = vec![
        PlannedPurchase {
            facility,
            unit_type: UnitType::Bcopters,
            build_turn: 1,
            cost: 7_500,
        },
        PlannedPurchase {
            facility,
            unit_type: UnitType::Bomber,
            build_turn: 2,
            cost: 20_000,
        },
    ];

    let shifted = left_shift_purchases(&input, &original);

    assert_eq!(shifted[0].build_turn, 0);
    assert_eq!(shifted[1].build_turn, 2);
    assert!(funding_suffices(&input, &shifted));
    assert!(!matches!(
        evaluate_fixed_package(&input, &original),
        Err(FixedPackageError::FundingUnavailable)
    ));
}

#[test]
fn property_plan_prefers_two_cleared_lanes_over_one_clear_and_extra_capturer() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let input = input();
    let mut one_clear = plan_force_package(&input).expect("比較元の作戦案");
    one_clear.feasible = false;
    one_clear.target_forecasts = vec![
        TargetForecast {
            destroyed_turn: Some(1),
            ..one_clear.target_forecasts[0].clone()
        },
        TargetForecast {
            destroyed_turn: None,
            ..one_clear.target_forecasts[0].clone()
        },
        TargetForecast {
            destroyed_turn: None,
            ..one_clear.target_forecasts[0].clone()
        },
    ];
    let capture_purchase = |x, completion_turn| PlannedCapturePurchase {
        purchase: PlannedPurchase {
            facility: GridPosition { x, y: 0 },
            unit_type: UnitType::Infantry,
            build_turn: 0,
            cost: 1_000,
        },
        target: GridPosition { x, y: 1 },
        completion_turn: Some(completion_turn),
        blocking_enemy_index: Some(x),
        survived_at_completion: true,
    };
    one_clear.capture_purchases = vec![
        capture_purchase(0, 3),
        capture_purchase(1, 4),
        capture_purchase(2, 5),
    ];
    one_clear.required_capture_survivor_count = 3;
    one_clear.deadline_capture_survivor_count = 3;

    let mut two_clears = one_clear.clone();
    two_clears.target_forecasts[1].destroyed_turn = Some(2);
    two_clears.capture_purchases.pop();
    two_clears.deadline_capture_survivor_count = 2;

    assert_eq!(one_clear.cleared_capture_lane_count(), 1);
    assert_eq!(two_clears.cleared_capture_lane_count(), 2);
    assert!(exact_property_plan_better(&input, &two_clears, &one_clear));
}

#[test]
fn property_plan_prefers_earlier_joint_arrival_over_earlier_damage_only() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let input = input();
    let mut fast_arrival = plan_force_package(&input).expect("快速案");
    fast_arrival.feasible = false;
    fast_arrival.target_forecasts[0].destroyed_turn = Some(2);
    fast_arrival.capture_purchases = vec![PlannedCapturePurchase {
        purchase: fast_arrival.purchases[0],
        target: GridPosition { x: 5, y: 0 },
        completion_turn: Some(3),
        blocking_enemy_index: Some(0),
        survived_at_completion: true,
    }];
    fast_arrival.required_capture_survivor_count = 1;
    fast_arrival.deadline_capture_survivor_count = 1;

    let mut slow_stronger = fast_arrival.clone();
    slow_stronger.target_forecasts[0].destroyed_turn = Some(1);
    slow_stronger.capture_purchases[0].completion_turn = Some(4);
    slow_stronger.expected_loss = 0;
    slow_stronger.surviving_combat_value = u32::MAX;

    assert_eq!(fast_arrival.cleared_capture_lane_count(), 1);
    assert_eq!(slow_stronger.cleared_capture_lane_count(), 1);
    assert!(exact_property_plan_better(
        &input,
        &fast_arrival,
        &slow_stronger
    ));
}

#[test]
fn opening_package_rejects_an_affordable_idle_factory() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let input = input();
    let first_facility = GridPosition { x: 0, y: 0 };
    let second_facility = GridPosition { x: 1, y: 0 };
    let facilities = vec![(first_facility, vec![0]), (second_facility, vec![1])];
    let first_only = SearchState {
        option_indices: vec![0],
        used_slots: HashSet::from_iter([ProductionSlot {
            facility: first_facility,
            build_turn: 0,
        }]),
        used_capture_targets: HashSet::default(),
        cost: 7_500,
    };
    let both_facilities = SearchState {
        option_indices: vec![0, 1],
        used_slots: HashSet::from_iter([
            ProductionSlot {
                facility: first_facility,
                build_turn: 0,
            },
            ProductionSlot {
                facility: second_facility,
                build_turn: 0,
            },
        ]),
        used_capture_targets: HashSet::default(),
        cost: 27_500,
    };

    assert!(!opening_package_spends_all_usable_funds(
        &input,
        &first_only,
        &facilities
    ));
    assert!(opening_package_spends_all_usable_funds(
        &input,
        &both_facilities,
        &facilities
    ));
}

#[test]
fn opening_constraints_are_owned_by_the_small_map_pipeline() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    assert!(!uses_small_map_opening_rules(&input));

    input.opening_policy = OpeningProductionPolicy::SmallMapExpansion;
    assert!(uses_small_map_opening_rules(&input));

    input.current_turn = 2;
    assert!(!uses_small_map_opening_rules(&input));
}

#[test]
fn property_front_selects_two_unit_breakthrough_and_three_capture_lanes() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    input.production_options.clear();
    input.production_attack_projections.clear();
    input.current_funds = 11_400;
    input.income_per_turn = 0;
    input.hard_deadline = Some(6);
    input.required_capture_survivors = 3;
    input.exact_property_control = true;
    input.prioritize_deadline_interdiction = false;
    Arc::make_mut(&mut input.damage_chart).insert_damage(UnitType::Recon, UnitType::Infantry, 60);

    for x in 0..5 {
        let facility = GridPosition { x, y: 0 };
        input.production_options.push(ProductionPlanOption {
            purchase: PlannedPurchase {
                facility,
                unit_type: UnitType::Recon,
                build_turn: 0,
                cost: 4_200,
            },
            stats: stats(UnitType::Recon, 4_200, 6),
            engageable_enemy_indices: vec![0],
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
            capture_durability: None,
            capture_blocking_enemy_index: None,
        });
        input
            .production_attack_projections
            .push(vec![Some(ProductionAttackProjection {
                ready_turn: 1,
                firing_position: facility,
                requires_movement: false,
            })]);

        input.production_options.push(ProductionPlanOption {
            purchase: PlannedPurchase {
                facility,
                unit_type: UnitType::Infantry,
                build_turn: 0,
                cost: 1_000,
            },
            stats: UnitStats {
                can_capture: true,
                movement_type: MovementType::Infantry,
                ..stats(UnitType::Infantry, 1_000, 3)
            },
            engageable_enemy_indices: Vec::new(),
            capture_target: Some(GridPosition { x: x + 5, y: 0 }),
            capture_arrival_turn: Some(2),
            capture_completion_turn: Some(4),
            capture_durability: Some(300),
            capture_blocking_enemy_index: Some(0),
        });
        input.production_attack_projections.push(vec![None]);
    }

    let plan = plan_force_package(&input).expect("突破と占領を合同で満たす生産案");
    let recon_count = plan
        .purchases
        .iter()
        .filter(|purchase| purchase.unit_type == UnitType::Recon)
        .count();

    assert_eq!(plan.front_breakthrough_turn, Some(1), "{plan:#?}");
    assert_eq!(recon_count, 2, "{plan:#?}");
    assert_eq!(plan.capture_purchases.len(), 3, "{plan:#?}");
    assert_eq!(plan.deadline_capture_survivor_count, 3, "{plan:#?}");
    assert!(
        plan.combat_purchases
            .iter()
            .all(|assignment| assignment.target == Entity::from_raw(7))
    );
}

#[test]
fn property_control_bounds_many_factory_combinations_before_simulation() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    assert_eq!(
        property_control_state_limit(5),
        MANY_FACTORY_PROPERTY_STATE_LIMIT
    );
    assert_eq!(
        property_control_state_limit(6),
        MANY_FACTORY_PROPERTY_STATE_LIMIT
    );

    let mut input = input();
    input.map = Arc::new(Map::new(20, 1, Terrain::Plains, GridTopology::Square));
    input.production_options.clear();
    input.production_attack_projections.clear();
    input.current_funds = 100_000;
    input.required_capture_survivors = 2;
    input.exact_property_control = true;
    Arc::make_mut(&mut input.damage_chart).insert_damage(UnitType::Recon, UnitType::Infantry, 60);

    // 7工場 x (不生産・直接・占領) で2,187通りになる入力を作る。
    // 上限を超えた場合だけ候補前線へ縮約し、最終候補は実戦闘simで選ぶ。
    for x in 0..7 {
        let facility = GridPosition { x, y: 0 };
        input.production_options.push(ProductionPlanOption {
            purchase: PlannedPurchase {
                facility,
                unit_type: UnitType::Recon,
                build_turn: 0,
                cost: 4_200,
            },
            stats: stats(UnitType::Recon, 4_200, 6),
            engageable_enemy_indices: vec![0],
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
            capture_durability: None,
            capture_blocking_enemy_index: None,
        });
        input
            .production_attack_projections
            .push(vec![Some(ProductionAttackProjection {
                ready_turn: 1,
                firing_position: facility,
                requires_movement: false,
            })]);
        input.production_options.push(ProductionPlanOption {
            purchase: PlannedPurchase {
                facility,
                unit_type: UnitType::Infantry,
                build_turn: 0,
                cost: 1_000,
            },
            stats: UnitStats {
                can_capture: true,
                movement_type: MovementType::Infantry,
                ..stats(UnitType::Infantry, 1_000, 3)
            },
            engageable_enemy_indices: Vec::new(),
            capture_target: Some(GridPosition { x: x + 10, y: 0 }),
            capture_arrival_turn: Some(2),
            capture_completion_turn: Some(4),
            capture_durability: Some(200),
            capture_blocking_enemy_index: Some(0),
        });
        input.production_attack_projections.push(vec![None]);
    }

    let plan = plan_force_package(&input).expect("bounded property plan");

    assert!(plan.search_truncated);
    assert!(
        plan.candidates_considered <= property_control_state_limit(7),
        "{plan:#?}"
    );
}

#[test]
fn property_frontier_preserves_fast_screen_profile_before_damage_rank() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    input.production_options.clear();
    input.production_attack_projections.clear();
    input.required_capture_survivors = 2;
    Arc::make_mut(&mut input.damage_chart).insert_damage(UnitType::Mech, UnitType::Infantry, 100);
    Arc::make_mut(&mut input.damage_chart).insert_damage(UnitType::Recon, UnitType::Infantry, 1);

    let slow_option = |unit_type| ProductionPlanOption {
        purchase: PlannedPurchase {
            facility: GridPosition { x: 0, y: 0 },
            unit_type,
            build_turn: 0,
            cost: 1_000,
        },
        stats: stats(unit_type, 1_000, 3),
        engageable_enemy_indices: vec![0],
        capture_target: None,
        capture_arrival_turn: None,
        capture_completion_turn: None,
        capture_durability: None,
        capture_blocking_enemy_index: None,
    };
    for _ in 0..MANY_FACTORY_PROPERTY_STATE_LIMIT {
        input.production_options.push(slow_option(UnitType::Mech));
        input
            .production_attack_projections
            .push(vec![Some(ProductionAttackProjection {
                ready_turn: 2,
                firing_position: GridPosition { x: 0, y: 0 },
                requires_movement: true,
            })]);
    }
    let fast_index = input.production_options.len();
    input.production_options.push(slow_option(UnitType::Recon));
    input
        .production_attack_projections
        .push(vec![Some(ProductionAttackProjection {
            ready_turn: 1,
            firing_position: GridPosition { x: 1, y: 0 },
            requires_movement: true,
        })]);

    let two_capture_lanes =
        HashSet::from_iter([GridPosition { x: 2, y: 0 }, GridPosition { x: 3, y: 0 }]);
    let mut states = (0..MANY_FACTORY_PROPERTY_STATE_LIMIT)
        .map(|index| SearchState {
            option_indices: vec![index],
            used_slots: HashSet::default(),
            used_capture_targets: two_capture_lanes.clone(),
            cost: 1_000,
        })
        .collect::<Vec<_>>();
    states.push(SearchState {
        option_indices: vec![fast_index],
        used_slots: HashSet::default(),
        used_capture_targets: HashSet::from_iter([
            GridPosition { x: 2, y: 0 },
            GridPosition { x: 3, y: 0 },
            GridPosition { x: 4, y: 0 },
        ]),
        cost: 1_000,
    });

    retain_property_control_frontier(&input, &mut states, MANY_FACTORY_PROPERTY_STATE_LIMIT);

    assert_eq!(states.len(), MANY_FACTORY_PROPERTY_STATE_LIMIT);
    assert!(
        states
            .iter()
            .any(|state| state.option_indices == vec![fast_index])
    );
}

#[test]
fn small_map_frontier_keeps_earlier_capture_over_stronger_late_capture() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = input();
    input.opening_policy = OpeningProductionPolicy::SmallMapExpansion;
    input.required_capture_survivors = 1;
    input.production_options.clear();
    input.production_attack_projections.clear();
    Arc::make_mut(&mut input.damage_chart).insert_damage(UnitType::Mech, UnitType::Infantry, 100);
    Arc::make_mut(&mut input.damage_chart).insert_damage(
        UnitType::Infantry,
        UnitType::Infantry,
        10,
    );

    let target = GridPosition { x: 5, y: 0 };
    let slow = ProductionPlanOption {
        purchase: PlannedPurchase {
            facility: GridPosition { x: 0, y: 0 },
            unit_type: UnitType::Mech,
            build_turn: 0,
            cost: 2_000,
        },
        stats: UnitStats {
            can_capture: true,
            ..stats(UnitType::Mech, 2_000, 2)
        },
        engageable_enemy_indices: vec![0],
        capture_target: Some(target),
        capture_arrival_turn: Some(3),
        capture_completion_turn: Some(5),
        capture_durability: Some(100),
        capture_blocking_enemy_index: None,
    };
    let fast = ProductionPlanOption {
        purchase: PlannedPurchase {
            facility: GridPosition { x: 0, y: 0 },
            unit_type: UnitType::Infantry,
            build_turn: 0,
            cost: 1_000,
        },
        stats: UnitStats {
            can_capture: true,
            ..stats(UnitType::Infantry, 1_000, 3)
        },
        engageable_enemy_indices: vec![0],
        capture_target: Some(target),
        capture_arrival_turn: Some(2),
        capture_completion_turn: Some(4),
        capture_durability: Some(100),
        capture_blocking_enemy_index: None,
    };
    input.production_options = vec![slow, fast];
    input.production_attack_projections = vec![vec![None], vec![None]];

    let slow_state = SearchState {
        option_indices: vec![0],
        used_slots: HashSet::default(),
        used_capture_targets: HashSet::from_iter([target]),
        cost: 2_000,
    };
    let fast_state = SearchState {
        option_indices: vec![1],
        used_slots: HashSet::default(),
        used_capture_targets: HashSet::from_iter([target]),
        cost: 1_000,
    };

    assert!(
        property_control_frontier_key(&input, &fast_state)
            < property_control_frontier_key(&input, &slow_state)
    );
}
