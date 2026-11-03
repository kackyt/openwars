//! 診断OFFでも排除後の命令が実行されることと、通路・予約・再占有の反例。
use super::*;
use bevy_ecs::prelude::*;

fn fixture() -> (World, Entity, Entity, Entity) {
    let mut world = world_with_route(CapitalRouteExecutionPhase::Advance);
    world.insert_resource(crate::resources::Players(vec![
        crate::resources::Player::new(1, "P1".to_owned()),
        crate::resources::Player::new(2, "P2".to_owned()),
    ]));
    world.insert_resource(crate::resources::GameRng::default());
    world.insert_resource(crate::resources::MatchState {
        current_phase: crate::resources::Phase::Main,
        ..Default::default()
    });
    let [inf, mech, gun] = spawn_three_shooters(&mut world);
    world.despawn(inf);
    world.despawn(mech);
    let entry = spawn(&mut world, 1, UnitType::Tank, pos(6, 6), 100);
    join_route_squad(&mut world, entry);
    let enemy = spawn(&mut world, 2, UnitType::Infantry, CELL, 1);
    (world, gun, entry, enemy)
}

#[test]
fn fire_then_entry_runs_through_game_systems_with_and_without_trace() {
    for enabled in [false, true] {
        let (mut world, gun, entry, enemy) = fixture();
        world.insert_resource(FireFollowOrderRegistry {
            trace_enabled: Some(enabled),
            ..Default::default()
        });
        let mut actions = Schedule::default();
        actions.add_systems(
            (
                crate::systems::movement::move_unit_system,
                crate::systems::combat::attack_unit_system,
                crate::systems::combat::remove_destroyed_units_system,
                crate::systems::turn_management::wait_unit_system,
            )
                .chain(),
        );
        let (actor, command) =
            decide_fire_follow_action(&mut world, PlayerId(1), &Default::default()).unwrap();
        assert_eq!(actor, gun);
        crate::ai::runtime::issue::execute_ai_command(&mut world, actor, command);
        actions.run(&mut world);
        assert!(world.get::<Health>(enemy).is_none(), "実砲撃で敵を排除する");
        let skip = HashSet::from_iter([gun]);
        let (actor, command) = decide_fire_follow_action(&mut world, PlayerId(1), &skip).unwrap();
        assert_eq!(actor, entry, "予約した進入役自身へ命令が渡る");
        assert!(matches!(command, AiCommand::Wait { target_pos } if target_pos == CELL));
        crate::ai::runtime::issue::execute_ai_command(&mut world, actor, command);
        actions.run(&mut world);
        assert_eq!(
            world.get::<GridPosition>(entry),
            Some(&CELL),
            "実移動で空いたセルへ入る"
        );
    }
}

#[test]
fn a_blocker_on_the_forward_path_is_targeted_before_the_endpoint() {
    let (mut world, _, _, enemy) = fixture();
    let mut paths = world.resource_mut::<CapitalRoutePathRegistry>();
    let order = paths.commitments.values_mut().next().unwrap();
    order.path = vec![pos(6, 6), pos(6, 5), CELL, pos(6, 3)];
    order.execution_target = pos(6, 3);
    let orders = observe(&mut world, PlayerId(1), &Default::default());
    assert!(
        orders
            .iter()
            .any(|order| order.target == enemy && order.cell == CELL)
    );
}

#[test]
fn unreachable_entry_does_not_reserve_a_shooting_order() {
    let (mut world, _, _, _) = fixture();
    let mut map = world.resource_mut::<Map>();
    for x in 0..map.width {
        map.set_terrain(x, 5, crate::resources::Terrain::Sea)
            .unwrap();
    }
    assert!(observe(&mut world, PlayerId(1), &Default::default()).is_empty());
}

#[test]
fn inadequate_fire_does_not_reserve_an_entry() {
    let (mut world, _, _, enemy) = fixture();
    world.get_mut::<Health>(enemy).unwrap().current = 100;
    assert!(observe(&mut world, PlayerId(1), &Default::default()).is_empty());
}

#[test]
fn occupied_released_cell_does_not_receive_an_entry_command() {
    let (mut world, gun, _, enemy) = fixture();
    assert!(decide_fire_follow_action(&mut world, PlayerId(1), &Default::default()).is_some());
    let mut skip = HashSet::default();
    apply_fire(&mut world, gun, enemy, &mut skip);
    spawn(&mut world, 2, UnitType::Infantry, CELL, 100);
    assert!(released_entry_action(&mut world, PlayerId(1), &skip).is_none());
}

#[test]
fn capture_reservations_are_not_stolen_by_a_combat_entry() {
    let (mut world, _, _, _) = fixture();
    world.spawn((
        CELL,
        crate::components::Property::new(crate::resources::Terrain::City, Some(PlayerId(2)), 200),
    ));
    let claimant = spawn(&mut world, 1, UnitType::Infantry, pos(9, 6), 100);
    let capture = assign_route_squad(&mut world, PlayerId(1), MissionType::Capture, &[claimant]);
    world
        .resource_mut::<crate::ai::squad::SquadManager>()
        .squads
        .iter_mut()
        .find(|squad| squad.id == capture)
        .unwrap()
        .plan
        .target = Some(CELL);
    assert!(observe(&mut world, PlayerId(1), &Default::default()).is_empty());
}

/// 複数の排除済み注文を一件ずつ消費し、先頭が到達不能でも後続を試す。
#[test]
fn released_orders_are_consumed_individually_and_invalid_entries_do_not_block_others() {
    for blocked_first in [false, true] {
        let (mut world, gun, first, enemy) = fixture();
        let second_cell = pos(9, 4);
        let second = spawn(&mut world, 1, UnitType::Tank, pos(9, 6), 100);
        let second_enemy = spawn(&mut world, 2, UnitType::Infantry, second_cell, 1);
        let second_gun = spawn(&mut world, 1, UnitType::HeavySpGun, pos(9, 1), 100);
        let turn = world
            .resource::<crate::resources::MatchState>()
            .current_turn_number
            .0;
        world.insert_resource(FireFollowOrderRegistry {
            player: Some(PlayerId(1)),
            turn,
            orders: vec![
                FireFollowOrder {
                    target: enemy,
                    cell: CELL,
                    fire_tasks: vec![(gun, 100)],
                    entry: first,
                },
                FireFollowOrder {
                    target: second_enemy,
                    cell: second_cell,
                    fire_tasks: vec![(second_gun, 100)],
                    entry: second,
                },
            ],
            ..Default::default()
        });
        world.despawn(enemy);
        world.despawn(second_enemy);
        if blocked_first {
            world.get_mut::<Fuel>(first).unwrap().current = 0;
        }
        let mut skip = HashSet::from_iter([gun, second_gun]);
        let (actor, _) = decide_fire_follow_action(&mut world, PlayerId(1), &skip).unwrap();
        if blocked_first {
            assert_eq!(actor, second, "先頭の進入不能で後続の命令を失わない");
        } else {
            assert_eq!(actor, first);
            skip.insert(first);
            let (actor, command) =
                decide_fire_follow_action(&mut world, PlayerId(1), &skip).unwrap();
            assert_eq!(actor, second, "一件目の進入で残りの注文を消さない");
            assert!(matches!(command, AiCommand::Wait { target_pos } if target_pos == second_cell));
        }
    }
}
