//! GO中の損耗部隊の継戦と支援射撃を検証する。
use super::probe_tests::{pos, spawn, world};
use super::*;
use crate::components::Health;
use crate::resources::UnitType;

fn board(world: &mut World) -> TacticalBoard {
    let (_, occupancy) = movable_units_and_occupancy(world, PlayerId(1), &HashSet::default(), None);
    TacticalBoard::observe(
        world,
        PlayerId(1),
        occupancy,
        SquadRoles::from_manager(SquadManager::default()),
    )
}

#[test]
fn go_finishes_current_capture_before_attacking() {
    let mut world = world(false);
    let infantry = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(3, 2));
    spawn(&mut world, PlayerId(2), UnitType::Tank, pos(4, 2));
    let board = board(&mut world);
    let unit = UnitTactics::observe(&world, &board, infantry, Some(pos(20, 2))).unwrap();
    assert_eq!(unit.squad_target, Some(pos(3, 2)));
    let best = with_turn_distances(&mut world, &board, |world, distances| {
        best_action_for_unit(world, &board, &unit, None, true, distances, None)
    });
    assert!(
        matches!(best.command(), Some(AiCommand::Capture { target_pos }) if *target_pos == pos(3, 2))
    );
}

#[test]
fn go_retains_original_infantry_assignment() {
    let mut world = world(false);
    let infantry = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(1, 2));
    super::probe_tests::assign(&mut world, infantry, MissionType::Capture, pos(3, 2));
    let mut board = board(&mut world);
    board.squads = SquadRoles::from_manager(world.resource::<SquadManager>().clone());
    let unit = UnitTactics::observe(&world, &board, infantry, Some(pos(20, 2))).unwrap();
    assert_eq!(unit.squad_target, Some(pos(3, 2)));
}

#[test]
fn infantry_only_attacks_own_blocker_when_fire_support_is_unavailable() {
    let mut world = world(false);
    let infantry = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(1, 2));
    let blocker = spawn(&mut world, PlayerId(2), UnitType::Infantry, pos(3, 2));
    let unrelated = spawn(&mut world, PlayerId(2), UnitType::Infantry, pos(2, 1));
    let first_board = board(&mut world);
    let mut unit = UnitTactics::observe(&world, &first_board, infantry, Some(pos(20, 2))).unwrap();
    unit.squad_target = Some(pos(3, 2));
    assert!(assault_capture::needs_attack(
        &world,
        &first_board,
        &unit,
        blocker
    ));
    assert!(!assault_capture::needs_attack(
        &world,
        &first_board,
        &unit,
        unrelated
    ));
    spawn(&mut world, PlayerId(1), UnitType::Tank, pos(1, 3));
    let second_board = board(&mut world);
    assert!(!assault_capture::needs_attack(
        &world,
        &second_board,
        &unit,
        blocker
    ));
}

#[test]
fn unassigned_infantry_split_capture_targets() {
    let mut world = world(false);
    world.spawn((
        pos(5, 2),
        crate::components::Property::new(crate::resources::Terrain::City, Some(PlayerId(2)), 200),
    ));
    let a = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(0, 1));
    let b = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(0, 3));
    let board = board(&mut world);
    let mut first = UnitTactics::observe(&world, &board, a, Some(pos(20, 2))).unwrap();
    let mut second = UnitTactics::observe(&world, &board, b, Some(pos(20, 2))).unwrap();
    with_turn_distances(&mut world, &board, |_, distances| {
        let mut claimed = HashSet::default();
        assault_capture::assign_unclaimed(&board, &mut first, None, &mut claimed, distances);
        assault_capture::assign_unclaimed(&board, &mut second, None, &mut claimed, distances);
    });
    assert!(first.squad_target.is_some());
    assert!(second.squad_target.is_some());
    assert_ne!(first.squad_target, second.squad_target);
}

#[test]
fn go_keeps_damaged_tank_on_objective() {
    let mut world = world(false);
    let tank = spawn(&mut world, PlayerId(1), UnitType::MdTank, pos(7, 2));
    world.get_mut::<Health>(tank).unwrap().current = 46;
    let board = board(&mut world);
    let unit = UnitTactics::observe(&world, &board, tank, Some(pos(20, 2))).unwrap();
    assert_eq!(unit.assault_objective, Some(pos(20, 2)));
    assert_eq!(unit.squad_target, Some(pos(20, 2)));
}

#[test]
fn go_includes_indirect_support() {
    let mut world = world(false);
    let gun = spawn(&mut world, PlayerId(1), UnitType::Artillery, pos(7, 2));
    let board = board(&mut world);
    let unit = UnitTactics::observe(&world, &board, gun, Some(pos(20, 2))).unwrap();
    assert_eq!(unit.assault_objective, Some(pos(20, 2)));
}

#[test]
fn support_selects_fireable_screen_and_fires_before_direct_unit() {
    let mut world = world(false);
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(10, 2));
    let gun = spawn(&mut world, PlayerId(1), UnitType::Artillery, pos(5, 2));
    let screen = spawn(&mut world, PlayerId(2), UnitType::Infantry, pos(12, 2));
    spawn(&mut world, PlayerId(2), UnitType::Artillery, pos(23, 2));
    world
        .resource_mut::<crate::resources::DamageChart>()
        .insert_damage(UnitType::Artillery, UnitType::Infantry, 70);
    let board = board(&mut world);
    assert_eq!(
        assault_support::select_target(&world, &board, pos(24, 2)),
        Some((screen, pos(12, 2)))
    );
    let mut direct = UnitTactics::observe(&world, &board, tank, Some(pos(24, 2))).unwrap();
    let mut support = UnitTactics::observe(&world, &board, gun, Some(pos(24, 2))).unwrap();
    direct.assault_target = Some(screen);
    support.assault_target = Some(screen);
    assert!(
        attack_priority(&board, &support, screen, Some(pos(12, 2)), true)
            > attack_priority(&board, &direct, screen, Some(pos(12, 2)), true)
    );
}

#[test]
fn non_go_damaged_unit_keeps_original_behavior() {
    let mut world = world(false);
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(7, 2));
    world.get_mut::<Health>(tank).unwrap().current = 46;
    let board = board(&mut world);
    let unit = UnitTactics::observe(&world, &board, tank, None).unwrap();
    assert!(unit.assault_objective.is_none());
    assert!(unit.state.combat_ineffective);
}

#[test]
fn go_prioritizes_enemy_gun_over_local_infantry() {
    let mut world = world(false);
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(7, 2));
    let gun = spawn(&mut world, PlayerId(2), UnitType::Artillery, pos(15, 2));
    let infantry = spawn(&mut world, PlayerId(2), UnitType::Infantry, pos(8, 2));
    let board = board(&mut world);
    let unit = UnitTactics::observe(&world, &board, tank, Some(pos(20, 2))).unwrap();
    assert!(
        attack_priority(&board, &unit, gun, Some(pos(15, 2)), true)
            > attack_priority(&board, &unit, infantry, Some(pos(8, 2)), true)
    );
}
