//! V4の通常経路で、占領の実行と敵生産口の占位を前面の戦闘より優先することを検証する。
use super::probe_tests::{assign, pos, spawn, world};
use super::*;
use crate::components::Property;
use crate::resources::{DamageChart, Map, Terrain, UnitType};

/// `at` を敵（P2）所有の `terrain` にする。
fn enemy_property(world: &mut World, at: GridPosition, terrain: Terrain) {
    let mut map = world.remove_resource::<Map>().unwrap();
    map.set_terrain(at.x, at.y, terrain).unwrap();
    world.insert_resource(map);
    world.spawn((at, Property::new(terrain, Some(PlayerId(2)), 200)));
}

fn infantry_duel_chart(world: &mut World) {
    let mut chart = world.remove_resource::<DamageChart>().unwrap();
    chart.insert_damage(UnitType::Infantry, UnitType::Infantry, 55);
    world.insert_resource(chart);
}

/// 敵首都の上にいる歩兵は、隣の敵を攻撃できても首都を占領する（勝敗を決める）。
/// 割当物件が首都でなくても、任務にかかわらず占領する。
#[test]
fn infantry_on_enemy_capital_captures_instead_of_attacking() {
    let mut world = world(false);
    enemy_property(&mut world, pos(6, 2), Terrain::Capital);
    infantry_duel_chart(&mut world);
    let infantry = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(6, 2));
    assign(&mut world, infantry, MissionType::Attack, pos(3, 2));
    spawn(&mut world, PlayerId(2), UnitType::Infantry, pos(7, 2));

    let decision = decide_action(&mut world, PlayerId(1), &HashSet::default());

    assert!(
        matches!(
            decision,
            Some((entity, AiCommand::Capture { target_pos }))
                if entity == infantry && target_pos == pos(6, 2)
        ),
        "{decision:?}"
    );
}

/// 占領任務の歩兵は、割当物件を占領できる手番には隣の敵の攻撃より占領を選ぶ。
#[test]
fn capture_squad_executes_its_assigned_capture_before_fighting() {
    let mut world = world(false);
    infantry_duel_chart(&mut world);
    let infantry = spawn(&mut world, PlayerId(1), UnitType::Infantry, pos(3, 2));
    assign(&mut world, infantry, MissionType::Capture, pos(3, 2));
    spawn(&mut world, PlayerId(2), UnitType::Infantry, pos(2, 2));

    let decision = decide_action(&mut world, PlayerId(1), &HashSet::default());

    assert!(
        matches!(
            decision,
            Some((entity, AiCommand::Capture { target_pos }))
                if entity == infantry && target_pos == pos(3, 2)
        ),
        "{decision:?}"
    );
}

/// 敵首都の生産範囲にある空いた敵工場は、作戦目標と逆方向でも占位して敵の補充を止める。
#[test]
fn combat_unit_occupies_an_empty_enemy_factory() {
    let mut world = world(false);
    enemy_property(&mut world, pos(10, 2), Terrain::Capital);
    enemy_property(&mut world, pos(12, 2), Terrain::Factory);
    let tank = spawn(&mut world, PlayerId(1), UnitType::Tank, pos(14, 2));
    assign(&mut world, tank, MissionType::Attack, pos(20, 2));

    let decision = decide_action(&mut world, PlayerId(1), &HashSet::default());

    assert!(
        matches!(
            decision,
            Some((entity, AiCommand::Wait { target_pos }))
                if entity == tank && target_pos == pos(12, 2)
        ),
        "{decision:?}"
    );
}
