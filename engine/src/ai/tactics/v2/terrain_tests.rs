//! V4の通常経路で、直接攻撃unitが敵の直接攻撃圏の防御地形を（上限つきで）評価することを検証する。
use super::probe_tests::{pos, spawn, world};
use super::*;
use crate::ai::tactics::board::TacticalBoard;
use crate::ai::tactics::scoring::{direct_threat_terrain_score, terrain_defense_bonus};
use crate::ai::tactics::unit::UnitState;
use crate::resources::{Map, Terrain, UnitType};

fn observe_board(world: &mut World) -> TacticalBoard {
    let (_, occupancy) = movable_units_and_occupancy(world, PlayerId(1), &HashSet::default(), None);
    TacticalBoard::observe(
        world,
        PlayerId(1),
        occupancy,
        SquadRoles::from_manager(SquadManager::default()),
    )
}

#[test]
fn v4_direct_unit_prefers_defensive_terrain_in_enemy_reach() {
    let mut world = world(false);
    let mut map = world.remove_resource::<Map>().unwrap();
    map.set_terrain(10, 2, Terrain::Forest).unwrap();
    map.set_terrain(10, 1, Terrain::Road).unwrap();
    map.set_terrain(10, 3, Terrain::Mountain).unwrap();
    world.insert_resource(map);
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(8, 2));
    spawn(&mut world, PlayerId(2), UnitType::Tank, pos(13, 2));
    let mut chart = world
        .remove_resource::<crate::resources::DamageChart>()
        .unwrap();
    chart.insert_damage(UnitType::Tank, UnitType::Tank, 50);
    world.insert_resource(chart);
    let terrain_scores = |world: &mut World| {
        let board = observe_board(world);
        let unit = UnitState::observe(world, &board, tank).unwrap();
        [pos(10, 2), pos(10, 1), pos(10, 3), pos(2, 2)].map(|position| {
            let defense = terrain_defense_bonus(&board, position);
            direct_threat_terrain_score(&board, &unit, position, defense)
        })
    };
    // 本番式で51HP→森42HP・山36HP。避ける損失は9×60=540、15×60=900。
    // 道路および敵の直接攻撃圏の外（格子距離11）は0。
    assert_eq!(terrain_scores(&mut world), [540, 0, 900, 0]);
    // V3は基準線として位置取りを変えない。
    world
        .resource_mut::<crate::ai::PlayerAiSettings>()
        .set_version(PlayerId(1), crate::ai::AiVersion::V3);
    assert_eq!(terrain_scores(&mut world), [0; 4]);
}
