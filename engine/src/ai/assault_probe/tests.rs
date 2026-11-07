use super::*;
use crate::resources::master_data::UnitName;

fn fixture() -> (World, Schedule, MasterDataRegistry) {
    let master = MasterDataRegistry::load().unwrap();
    let (mut world, schedule) = crate::setup::initialize_world_from_master_data_with_topology(
        &master,
        "map_20",
        GridTopology::Hex,
    )
    .unwrap();
    {
        let mut state = world.resource_mut::<MatchState>();
        state.current_phase = Phase::Main;
        state.current_turn_number = TurnNumber(26);
        state.active_player_index = PlayerIndex(0);
    }
    for player in &mut world.resource_mut::<Players>().0 {
        player.funds = 0;
    }
    (world, schedule, master)
}

fn unit(world: &mut World, player: PlayerId, kind: UnitType, x: usize, y: usize) -> Entity {
    let stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&UnitName(kind.as_str().to_owned()))
        .unwrap();
    let fuel = Fuel {
        current: stats.max_fuel,
        max: stats.max_fuel,
    };
    let ammo = Ammo {
        ammo1: stats.max_ammo1,
        max_ammo1: stats.max_ammo1,
        ammo2: stats.max_ammo2,
        max_ammo2: stats.max_ammo2,
    };
    let id = UnitId(uuid::Uuid::from_u128(world.entities().len() as u128 + 1000));
    world
        .spawn((
            id,
            Faction(player),
            GridPosition { x, y },
            stats,
            Health {
                current: 100,
                max: 100,
            },
            fuel,
            ammo,
            HasMoved(false),
            ActionCompleted(false),
        ))
        .id()
}

#[test]
fn moved_ranged_attack_is_not_a_candidate() {
    let (mut world, _, _) = fixture();
    let actor = unit(&mut world, PlayerId(1), UnitType::HeavySpGun, 12, 18);
    unit(&mut world, PlayerId(2), UnitType::Artillery, 16, 18);
    let objective = GridPosition { x: 21, y: 16 };
    // 同じ敵は静止射撃なら合法だが、一歩移動してからの遠距離射撃は不可。
    assert!(
        attack_choice(
            &mut world,
            actor,
            &[GridPosition { x: 12, y: 18 }],
            PlayerId(1),
            ProbePlan::CounterBattery,
            objective
        )
        .is_some()
    );
    assert!(
        attack_choice(
            &mut world,
            actor,
            &[GridPosition { x: 13, y: 18 }],
            PlayerId(1),
            ProbePlan::CounterBattery,
            objective
        )
        .is_none()
    );
}

#[test]
fn completed_units_and_cargo_are_not_overridden() {
    let (mut world, _, _) = fixture();
    let actor = unit(&mut world, PlayerId(1), UnitType::Tank, 15, 18);
    world.get_mut::<ActionCompleted>(actor).unwrap().0 = true;
    assert!(!available_units(&world, PlayerId(1), ProbePlan::Advance).contains(&actor));
    world.get_mut::<ActionCompleted>(actor).unwrap().0 = false;
    let carrier = unit(&mut world, PlayerId(1), UnitType::SupplyTruck, 14, 18);
    world.entity_mut(actor).insert(Transporting(carrier));
    assert!(!available_units(&world, PlayerId(1), ProbePlan::Advance).contains(&actor));
}

#[test]
fn enemy_artillery_spends_only_one_attack_in_its_turn() {
    let (mut world, mut schedule, _) = fixture();
    let actor = unit(&mut world, PlayerId(2), UnitType::Artillery, 21, 16);
    let target = unit(&mut world, PlayerId(1), UnitType::Tank, 16, 18);
    unit(&mut world, PlayerId(1), UnitType::Mech, 17, 17);
    world.resource_mut::<MatchState>().active_player_index = PlayerIndex(1);
    assert!(crate::systems::combat::can_attack(actor, target, &mut world).is_ok());
    let id = world.get::<UnitId>(actor).unwrap().0.to_string();
    let mut audit = Audit::default();
    finish_turn(&mut world, &mut schedule, &mut audit, PlayerId(1)).unwrap();
    assert_eq!(
        audit
            .attacks
            .iter()
            .filter(|event| event.attacker == id)
            .count(),
        1,
        "砲1基の射撃を複数の味方への被弾として二重計上しない"
    );
}

fn result(metrics: PositionMetrics, net: i64) -> PlanResult {
    PlanResult {
        plan: ProbePlan::Advance,
        initial: PositionMetrics::default(),
        final_position: metrics,
        battle: BattleTotals {
            net_exchange_value: net,
            ..Default::default()
        },
        commands: vec![],
        attacks: vec![],
        comparison: String::new(),
        completed_own_turns: 1,
    }
}

#[test]
fn comparison_does_not_reject_all_losses_or_hide_tradeoffs() {
    let base = result(
        PositionMetrics {
            enemy_value: 1000,
            enemy_indirect_value: 1000,
            ..Default::default()
        },
        0,
    );
    let better = result(
        PositionMetrics {
            enemy_value: 500,
            enemy_indirect_value: 500,
            ..Default::default()
        },
        100,
    );
    assert_eq!(
        compare_outcomes(&better, &base, PlayerId(1)),
        "dominates_baseline"
    );
    let tradeoff = result(
        PositionMetrics {
            enemy_value: 500,
            enemy_indirect_value: 500,
            ..Default::default()
        },
        -100,
    );
    assert_eq!(
        compare_outcomes(&tradeoff, &base, PlayerId(1)),
        "tradeoff_inspect_metrics"
    );
    let worse = result(
        PositionMetrics {
            enemy_value: 2000,
            enemy_indirect_value: 1000,
            ..Default::default()
        },
        0,
    );
    assert_eq!(
        compare_outcomes(&worse, &base, PlayerId(1)),
        "dominated_by_baseline"
    );
}

#[test]
fn winning_and_losing_are_not_classified_by_damage_only() {
    let base = result(PositionMetrics::default(), 0);
    let loss = result(
        PositionMetrics {
            winner: Some(2),
            ..Default::default()
        },
        100_000,
    );
    assert_eq!(
        compare_outcomes(&loss, &base, PlayerId(1)),
        "loss_in_horizon"
    );
    let win = result(
        PositionMetrics {
            winner: Some(1),
            ..Default::default()
        },
        -100_000,
    );
    assert_eq!(compare_outcomes(&win, &base, PlayerId(1)), "win_in_horizon");
}

#[test]
fn diagnostic_keeps_source_board_rng_and_action_flags_unchanged() {
    let (mut world, _, master) = fixture();
    unit(&mut world, PlayerId(1), UnitType::Tank, 15, 18);
    unit(&mut world, PlayerId(2), UnitType::Artillery, 21, 16);
    let before = crate::serialize::export_save_data(&mut world, "map_20").unwrap();
    let report = compare_assaults(
        &mut world,
        &master,
        "map_20",
        PlayerId(1),
        GridPosition { x: 21, y: 16 },
        1,
    )
    .unwrap();
    let after = crate::serialize::export_save_data(&mut world, "map_20").unwrap();
    assert_eq!(before, after, "診断が元Worldを進めてはいけない");
    assert!(
        report
            .results
            .iter()
            .all(|branch| branch.initial == report.results[0].initial)
    );
    assert!(
        report
            .results
            .iter()
            .all(|branch| branch.commands.iter().all(|command| command.accepted))
    );
}
