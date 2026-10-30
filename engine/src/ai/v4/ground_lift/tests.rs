use super::*;
use crate::ai::v4::maritime::test_support::{load, naval_world, property, spawn};
use crate::components::ActionCompleted;

/// 両端に首都を置いた1列の陸地。重歩兵の重自走砲への損害だけを入れる。
fn land_world(row: &str) -> World {
    let mut world = naval_world(&[row]);
    let width = row.len();
    property(&mut world, 1, 0, 0);
    property(&mut world, 2, width - 1, 0);
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Mech, UnitType::HeavySpGun, 25);
    world.insert_resource(chart);
    world
}

fn decide(world: &mut World) -> Option<(Entity, AiCommand)> {
    decide_ground_lift(world, PlayerId(1), &HashSet::default())
}

#[test]
fn loaded_recon_drops_its_mech_within_striking_range_of_a_heavy_gun() {
    let mut world = land_world("H..................H");
    let recon = spawn(&mut world, 1, UnitType::Recon, 3, 0);
    let mech = load(&mut world, recon, UnitType::Mech, 1);
    spawn(&mut world, 2, UnitType::HeavySpGun, 12, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, recon);
    match command {
        AiCommand::Drop {
            transport_target_pos,
            cargo_drop_pos,
            cargo_entity,
        } => {
            assert_eq!(cargo_entity, mech);
            // 装甲車は移動力6で x=9 まで進み、重自走砲に最も近い隣へ降ろす。
            assert_eq!(transport_target_pos, GridPosition { x: 9, y: 0 });
            assert_eq!(cargo_drop_pos, GridPosition { x: 10, y: 0 });
        }
        other => panic!("重自走砲の近くへ降ろすはず: {other:?}"),
    }
}

#[test]
fn empty_recon_meets_a_rear_mech_on_the_front_side_of_its_reach() {
    let mut world = land_world("H..................H");
    let recon = spawn(&mut world, 1, UnitType::Recon, 8, 0);
    spawn(&mut world, 1, UnitType::Mech, 1, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 15, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, recon);
    let AiCommand::Wait { target_pos } = command else {
        panic!("重歩兵の届くセルへ迎えに行くはず: {command:?}");
    };
    // 重歩兵（移動力2）が届くセルのうち、最も前線寄り。
    assert_eq!(target_pos, GridPosition { x: 3, y: 0 });
}

#[test]
fn rear_mech_boards_the_recon_that_came_to_meet_it() {
    let mut world = land_world("H..................H");
    let recon = spawn(&mut world, 1, UnitType::Recon, 3, 0);
    world.get_mut::<ActionCompleted>(recon).unwrap().0 = true;
    let mech = spawn(&mut world, 1, UnitType::Mech, 1, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 15, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, mech);
    let AiCommand::Load {
        target_pos,
        transport_entity,
    } = command
    else {
        panic!("迎えの装甲車へ乗るはず: {command:?}");
    };
    assert_eq!(transport_entity, recon);
    assert_eq!(target_pos, GridPosition { x: 3, y: 0 });
}

#[test]
fn mech_close_to_the_front_walks_instead_of_waiting_for_a_ride() {
    let mut world = land_world("H..................H");
    let recon = spawn(&mut world, 1, UnitType::Recon, 3, 0);
    world.get_mut::<ActionCompleted>(recon).unwrap().0 = true;
    // 重自走砲まで4マス。歩いて2手番以内に届く。
    spawn(&mut world, 1, UnitType::Mech, 4, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 8, 0);

    assert!(decide(&mut world).is_none());
}

#[test]
fn capitals_on_different_islands_leave_recons_to_the_sealift() {
    let mut world = naval_world(&["H....~~~~....H"]);
    property(&mut world, 1, 0, 0);
    property(&mut world, 2, 13, 0);
    let recon = spawn(&mut world, 1, UnitType::Recon, 2, 0);
    load(&mut world, recon, UnitType::Mech, 1);
    spawn(&mut world, 2, UnitType::HeavySpGun, 11, 0);

    assert!(decide(&mut world).is_none());
}

#[test]
fn recon_does_not_drop_its_mech_behind_an_infantry_screen() {
    // 重自走砲(x=12)の前後を歩兵が塞ぐ2列の盤面。x=10へ降ろしても、重歩兵は
    // 歩兵のZOCで止まり、重自走砲の隣の空きセルへ入れない。
    let mut world = naval_world(&["H..................H", "H~~~~~~~~~~~~~~~~~~~"]);
    property(&mut world, 1, 0, 0);
    property(&mut world, 2, 19, 0);
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Mech, UnitType::HeavySpGun, 25);
    world.insert_resource(chart);
    let recon = spawn(&mut world, 1, UnitType::Recon, 3, 0);
    load(&mut world, recon, UnitType::Mech, 1);
    spawn(&mut world, 2, UnitType::Infantry, 11, 0);
    spawn(&mut world, 2, UnitType::Infantry, 13, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 12, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, recon);
    assert!(
        !matches!(command, AiCommand::Drop { .. }),
        "突けない重自走砲の手前では降ろさないはず: {command:?}"
    );
}
