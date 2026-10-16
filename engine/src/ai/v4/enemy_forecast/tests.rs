use crate::ai::v4::enemy_forecast::observe_enemy_production;
use crate::ai::v4::test_support::*;
use crate::ai::v4::*;

#[test]
fn enemy_production_forecast_learns_new_entities_and_records_next_turn_error() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let mut world = World::new();
    let mut scan = multi_factory_scan();
    scan.enemy_production_slots = 3;

    // 初回の盤面は開幕配置を含むため、実生産として学習しない。
    assert_eq!(
        observe_enemy_production(&mut world, player, 1, &scan).expected_units_next_turn,
        0
    );

    let tank = scan
        .available_types
        .iter()
        .find(|(unit_type, _)| *unit_type == UnitType::Tank)
        .map(|(_, stats)| stats.clone())
        .expect("戦車のマスターデータ");
    let produced = world.spawn_empty().id();
    scan.enemy_units = vec![UnitSnapshot {
        entity: Some(produced),
        pos: pos(7, 2),
        stats: tank.clone(),
        hp: 100,
        free_cargo: 0,
    }];
    let predicted = observe_enemy_production(&mut world, player, 2, &scan);
    assert_eq!(predicted.expected_units_next_turn, 1);
    assert_eq!(predicted.expected_cost_next_turn, tank.cost);
    assert_eq!(predicted.dominant_unit_type, Some(UnitType::Tank));
    assert_eq!(predicted.evaluated_samples, 0);

    // 次手番に実生産が無ければ、直前の「1体」予測を誤差として採点する。
    scan.enemy_units.clear();
    let audited = observe_enemy_production(&mut world, player, 3, &scan);
    assert_eq!(audited.evaluated_samples, 1);
    assert_eq!(audited.mean_absolute_unit_error, 1);
    assert_eq!(audited.mean_absolute_cost_error, tank.cost);
}
