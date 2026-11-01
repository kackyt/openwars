//! campaign目標から遠ざかる待機の判定が、格子距離ではなく経路長で比べることを検証する。
//! 山の壁を回り込む前進は格子距離が一時的に増えるが、後退として拒否しない。
use super::probe_tests::{assign, pos, spawn, world};
use super::*;
use crate::ai::islands::IslandMap;
use crate::resources::{Map, Terrain, UnitType};

/// 目標都市 (3,2) と戦車の間に山の壁を置き、右端 x=16 で折り返す通路だけを残す。
///
/// ```text
///   x: 0123456789...16
/// y0:  ....M..........
/// y1:  ....M..........
/// y2:  ...CMT.........    C=目標都市, T=戦車, M=山
/// y3:  ....MMMMMMMMMMM.
/// y4:  ................
/// ```
fn walled_world() -> World {
    let mut world = world(true);
    let mut map = world.remove_resource::<Map>().unwrap();
    for y in 0..=3 {
        map.set_terrain(5, y, Terrain::Mountain).unwrap();
    }
    for x in 6..=15 {
        map.set_terrain(x, 3, Terrain::Mountain).unwrap();
    }
    world.insert_resource(IslandMap::analyze(&map));
    world.insert_resource(map);
    world
}

#[test]
fn detour_around_mountain_remains_a_wait_candidate() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut world = walled_world();
    let start = pos(6, 2);
    let target = pos(3, 2);
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, start);
    assign(&mut world, tank, MissionType::Defense, target);
    crate::ai::wait_probe::begin_step(&mut world);
    crate::ai::wait_probe::stage(&mut world, "normal_tactics");
    let (entity, command) = decide_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
    assert_eq!(entity, tank);
    crate::ai::execute_ai_command(&mut world, entity, command);
    let probe = crate::ai::wait_probe::take_issued(&mut world).unwrap();
    let evaluation = &probe["evaluation"];

    // 通路の先へ進む待機は、格子距離では目標から遠ざかるが候補に残る。
    let moving = &evaluation["best_moving_wait"];
    assert!(
        moving.is_object(),
        "迂回の前進Waitが候補に残る: {evaluation}"
    );
    let x = moving["position"][0].as_u64().unwrap() as usize;
    let y = moving["position"][1].as_u64().unwrap() as usize;
    let map = world.resource::<Map>();
    assert!(
        map.distance(x, y, target.x, target.y) > map.distance(start.x, start.y, target.x, target.y),
        "格子距離では遠ざかる位置: ({x},{y})"
    );

    // 拒否されるのは経路長でも遠ざかる移動（壁際の袋小路）だけ。
    if let Some(samples) =
        evaluation["rejection_samples"]["wait_move_away_from_campaign"].as_array()
    {
        for sample in samples {
            let facts = &sample["facts"];
            assert!(
                facts["candidate_path_mp"].as_u64().unwrap()
                    > facts["current_path_mp"].as_u64().unwrap(),
                "経路長で近づく移動を拒否しない: {sample}"
            );
        }
    }
}

#[test]
fn infantry_crosses_long_bridge_over_multiple_turns() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut world = world(true);
    let mut map = Map::new(10, 3, Terrain::Sea, crate::resources::GridTopology::Square);
    map.set_terrain(0, 1, Terrain::Plains).unwrap();
    for x in 1..=7 {
        map.set_terrain(x, 1, Terrain::Bridge).unwrap();
    }
    map.set_terrain(8, 1, Terrain::Plains).unwrap();
    world.insert_resource(IslandMap::analyze(&map));
    world.insert_resource(map);
    let infantry = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(0, 1));
    assign(&mut world, infantry, MissionType::Capture, pos(8, 1));
    world.insert_resource(crate::resources::Players(vec![
        crate::resources::Player::new(1, "P1".into()),
    ]));
    let mut schedule = Schedule::default();
    schedule.add_systems(crate::systems::movement::move_unit_system);
    // 一手番では渡り切れない橋でも、実際の移動を繰り返して目標へ到達する。
    for _ in 0..4 {
        let (entity, command) =
            decide_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        assert_eq!(entity, infantry);
        crate::ai::execute_ai_command(&mut world, infantry, command);
        schedule.run(&mut world);
        world.entity_mut(infantry).insert((
            crate::components::HasMoved(false),
            crate::components::ActionCompleted(false),
        ));
    }
    assert_eq!(*world.get::<GridPosition>(infantry).unwrap(), pos(8, 1));
}

#[test]
fn bridge_move_competes_with_actual_vehicle_attack() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let mut world = world(true);
    let mut map = Map::new(
        10,
        3,
        Terrain::Plains,
        crate::resources::GridTopology::Square,
    );
    map.set_terrain(1, 1, Terrain::Bridge).unwrap();
    world.insert_resource(IslandMap::analyze(&map));
    world.insert_resource(map);
    let infantry = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(0, 1));
    assign(&mut world, infantry, MissionType::Capture, pos(8, 1));
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(0, 0));
    assign(&mut world, tank, MissionType::Attack, pos(8, 1));
    let enemy = spawn(&mut world, PlayerId(2), UnitType::Infantry, pos(2, 1));
    // 車両の合法な攻撃も、歩兵の橋への移動と同じ決定器で比較される。
    let (entity, command) = decide_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
    assert_eq!(entity, tank);
    assert!(matches!(command, AiCommand::Attack { target_entity, .. } if target_entity == enemy));
}
