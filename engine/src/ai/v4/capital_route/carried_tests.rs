//! 実マップの二経路で、入口通過後の方面を目標更新・再編・占領で失わない再現。
use super::commitment::{carried_axis_for_entity, refresh_capital_route_path_commitments};
use super::model::CapitalRoutePathRegistry;
use super::prepare_capital_route_topologies;
use super::topology::capital_route_topology_for;
use crate::ai::v4::test_support::*;
use crate::ai::v4::{
    Entity, Faction, GridPosition, Health, MasterDataRegistry, MissionType, PlayerId, Property,
    SquadId, UnitStats, UnitType, World,
};
use crate::resources::GridTopology;

pub(crate) fn refresh(world: &mut World) {
    let mut manager = world
        .remove_resource::<crate::ai::squad::SquadManager>()
        .unwrap();
    refresh_capital_route_path_commitments(world, PlayerId(1), &mut manager);
    world.insert_resource(manager);
}

fn set_owned(world: &mut World, cells: &[GridPosition]) {
    for mut entity in world.iter_entities_mut() {
        if entity
            .get::<GridPosition>()
            .is_some_and(|at| cells.contains(at))
            && let Some(mut property) = entity.get_mut::<Property>()
        {
            property.owner_id = Some(PlayerId(1));
        }
    }
}

pub(crate) fn fixture() -> (World, Entity, SquadId) {
    let master = MasterDataRegistry::load().unwrap();
    let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
        &master,
        "map_20",
        GridTopology::Hex,
    )
    .unwrap();
    let mut settings = crate::ai::PlayerAiSettings::new();
    settings.set_version(PlayerId(1), crate::ai::AiVersion::V4);
    world.insert_resource(settings);
    set_owned(
        &mut world,
        &[pos(2, 18), pos(2, 19), pos(6, 17), pos(10, 18)],
    );
    prepare_capital_route_topologies(&mut world, PlayerId(1));
    let stats = master
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Tank.as_str().to_owned(),
        ))
        .unwrap();
    let unit = world
        .spawn((
            Faction(PlayerId(1)),
            pos(9, 18),
            stats,
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    let squad = assign_route_squad(&mut world, PlayerId(1), MissionType::Attack, &[unit]);
    world
        .resource_mut::<crate::ai::squad::SquadManager>()
        .squads
        .iter_mut()
        .find(|s| s.id == squad)
        .unwrap()
        .plan
        .target = Some(pos(16, 17));
    refresh(&mut world);
    assert_eq!(
        carried_axis_for_entity(&world, PlayerId(1), unit),
        Some(pos(8, 18)),
        "南の橋出口まで実進出した部隊の方面を記録する"
    );
    (world, unit, squad)
}

#[test]
fn advancing_south_group_keeps_its_axis_after_squad_and_capital_target_changes() {
    let (mut world, unit, old) = fixture();
    world
        .resource_mut::<crate::ai::squad::SquadManager>()
        .edit_each()
        .find(|s| s.id == old)
        .unwrap()
        .members
        .remove(&unit);
    let new = assign_route_squad(&mut world, PlayerId(1), MissionType::Attack, &[unit]);
    world
        .resource_mut::<crate::ai::squad::SquadManager>()
        .squads
        .iter_mut()
        .find(|s| s.id == new)
        .unwrap()
        .plan
        .target = Some(pos(21, 16));
    refresh(&mut world);
    let registry = world.resource::<CapitalRoutePathRegistry>();
    let order = &registry.commitments[&new];
    let topology = capital_route_topology_for(&world, PlayerId(1), order.island_id).unwrap();
    assert_eq!(
        topology.routes[order.route.0].front,
        pos(8, 18),
        "首都目標へ更新しても北へ振り替えない"
    );
}

#[test]
fn capturing_an_intermediate_region_does_not_release_the_south_axis() {
    let (mut world, unit, squad) = fixture();
    let before = world.resource::<CapitalRoutePathRegistry>().commitments[&squad].clone();
    let targets = before.capture_targets.clone();
    assert!(
        !targets.is_empty(),
        "実際に未占領の中間地域を確保する再現にする"
    );
    set_owned(&mut world, &targets);
    refresh(&mut world);
    assert_eq!(
        carried_axis_for_entity(&world, PlayerId(1), unit),
        Some(pos(8, 18)),
        "中間目標を占領しても方面を失効させない"
    );
    let registry = world.resource::<CapitalRoutePathRegistry>();
    let order = &registry.commitments[&squad];
    let topology = capital_route_topology_for(&world, PlayerId(1), order.island_id).unwrap();
    assert_eq!(topology.routes[order.route.0].front, pos(8, 18));
    assert_ne!(
        order.target_anchor, before.target_anchor,
        "最終目的地の首都は保ち、確保した中間地域から次の実行地域へ進む"
    );
}

#[test]
fn explicit_rear_guard_keeps_its_hold_target_through_the_dag() {
    let (mut world, _, squad) = fixture();
    {
        let mut manager = world.resource_mut::<crate::ai::squad::SquadManager>();
        let mut guard = manager.edit_each().find(|s| s.id == squad).unwrap();
        guard.plan_mut().mission_type = MissionType::Defense;
        guard.plan_mut().target = Some(pos(10, 18));
    }
    refresh(&mut world);
    let registry = world.resource::<CapitalRoutePathRegistry>();
    let order = &registry.commitments[&squad];
    assert_eq!(
        order.phase,
        super::model::CapitalRouteExecutionPhase::Hold,
        "後方の明示守備をDAGが攻勢へ上書きしない"
    );
    assert_eq!(order.execution_target, pos(10, 18));
}

#[test]
fn new_units_and_changed_owners_do_not_inherit_a_progressed_axis() {
    let (mut world, unit, _) = fixture();
    let stats = world.get::<UnitStats>(unit).unwrap().clone();
    let fresh = world
        .spawn((
            Faction(PlayerId(1)),
            pos(11, 4),
            stats,
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    assert!(carried_axis_for_entity(&world, PlayerId(1), fresh).is_none());
    world.get_mut::<Faction>(unit).unwrap().0 = PlayerId(2);
    assert!(carried_axis_for_entity(&world, PlayerId(1), unit).is_none());
    assert!(carried_axis_for_entity(&world, PlayerId(2), unit).is_none());
}

/// 出撃前の配分変更は許すが、出撃後の側方展開や首都目標への更新で方面を失わない。
#[test]
fn deployment_reassignment_is_allowed_but_departed_off_path_units_keep_their_axis() {
    let (mut world, veteran, _) = fixture();
    let stats = world.get::<UnitStats>(veteran).unwrap().clone();
    let unit = world
        .spawn((
            Faction(PlayerId(1)),
            pos(11, 4),
            stats,
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    let squad = assign_route_squad(&mut world, PlayerId(1), MissionType::Attack, &[unit]);
    for target in [pos(20, 3), pos(16, 17)] {
        world
            .resource_mut::<crate::ai::squad::SquadManager>()
            .squads
            .iter_mut()
            .find(|s| s.id == squad)
            .unwrap()
            .plan
            .target = Some(target);
        refresh(&mut world);
        assert!(
            carried_axis_for_entity(&world, PlayerId(1), unit).is_none(),
            "出撃前は方面を固定しない"
        );
    }
    world
        .get_mut::<GridPosition>(unit)
        .unwrap()
        .clone_from(&pos(12, 17));
    world
        .resource_mut::<crate::ai::squad::SquadManager>()
        .squads
        .iter_mut()
        .find(|s| s.id == squad)
        .unwrap()
        .plan
        .target = Some(pos(21, 16));
    refresh(&mut world);
    assert_eq!(
        carried_axis_for_entity(&world, PlayerId(1), unit),
        Some(pos(8, 18))
    );
    let registry = world.resource::<CapitalRoutePathRegistry>();
    let order = &registry.commitments[&squad];
    assert!(
        !order.path.contains(&pos(12, 17)),
        "経路上の厳密な停止位置に依存しない再現"
    );
    let graph = capital_route_topology_for(&world, PlayerId(1), order.island_id).unwrap();
    assert_eq!(graph.routes[order.route.0].front, pos(8, 18));
}
