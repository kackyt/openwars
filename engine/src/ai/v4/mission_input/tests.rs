use super::*;
use crate::ai::operation_assignment::{OperationOwner, OperationUnitRole, UnitOperationAssignment};
use crate::ai::planning::assignment::{LedgerKey, OperationLedger};
use std::collections::BTreeSet;

#[test]
fn existing_force_uses_only_the_current_squad_binding() {
    let player = PlayerId(1);
    let mut world = World::new();
    let combat = world.spawn(Faction(player)).id();
    let capture = world.spawn(Faction(player)).id();
    let mut manager = SquadManager::default();
    let target = crate::components::GridPosition { x: 8, y: 8 };
    let mut proposals = OperationLedger::proposals(&mut manager);
    let stale = {
        let mut squad = proposals.create_owned(
            MissionType::Attack,
            player,
            BTreeSet::from_iter([combat]),
            None,
            Vec::new(),
        );
        squad.plan_mut().target = Some(crate::components::GridPosition { x: 1, y: 1 });
        squad.id
    };
    let current = {
        let mut squad = proposals.create_owned(
            MissionType::Defense,
            player,
            BTreeSet::from_iter([combat]),
            None,
            Vec::new(),
        );
        squad.plan_mut().target = Some(target);
        squad.id
    };
    let capture_squad = proposals
        .create_owned(
            MissionType::Capture,
            player,
            BTreeSet::from_iter([capture]),
            None,
            Vec::new(),
        )
        .id;
    let mut bindings = UnitOperationRegistry::default();
    for (entity, id) in [(combat, current), (capture, capture_squad)] {
        bindings.apply_assign(
            LedgerKey::fixture(),
            entity,
            UnitOperationAssignment {
                owner: OperationOwner::TacticalSquad {
                    player_id: player,
                    squad_id: id,
                },
                squad_id: Some(id),
                role: OperationUnitRole::Member,
                assigned_turn: 1,
            },
        );
    }
    world.insert_resource(bindings);
    world.insert_resource(manager);
    let assignments = combat_assignments(&world, player);
    assert_eq!(
        assignments.len(),
        1,
        "旧roster・占領任務を戦闘戦力へ重複計上しない"
    );
    assert_eq!(assignments[&combat].objective, Some(target));
    assert_eq!(
        assignments[&combat].plan_id, None,
        "現在任務は購入PlanIdを必要としない"
    );
    assert_ne!(stale, current);
    assert_eq!(
        active_entities(&world, player),
        HashSet::from_iter([combat, capture])
    );
}

#[test]
fn route_operation_indirect_and_loaded_gun_report_route_key_point() {
    let player = PlayerId(1);
    let mut world = World::new();
    let firing_tile = crate::components::GridPosition { x: 2, y: 2 };
    let key_point = crate::components::GridPosition { x: 8, y: 8 };
    let indirect = world.spawn(Faction(player)).id();
    let truck = world.spawn(Faction(player)).id();
    let loaded_gun = world
        .spawn((
            Faction(player),
            crate::components::Transporting(truck),
            crate::components::UnitStats {
                unit_type: crate::resources::UnitType::Artillery,
                min_range: 3,
                max_range: 6,
                max_movement: 1,
                ..crate::components::UnitStats::mock()
            },
        ))
        .id();
    let mut manager = SquadManager::default();
    manager.route_operation_members.insert(indirect, key_point);
    manager
        .route_operation_members
        .insert(loaded_gun, key_point);
    let mut proposals = OperationLedger::proposals(&mut manager);
    let squad_id = {
        let mut squad = proposals.create_owned(
            MissionType::Defense,
            player,
            BTreeSet::from_iter([indirect]),
            None,
            Vec::new(),
        );
        squad.plan_mut().target = Some(firing_tile);
        squad.id
    };
    let mut bindings = UnitOperationRegistry::default();
    bindings.apply_assign(
        LedgerKey::fixture(),
        indirect,
        UnitOperationAssignment {
            owner: OperationOwner::TacticalSquad {
                player_id: player,
                squad_id,
            },
            squad_id: Some(squad_id),
            role: OperationUnitRole::Member,
            assigned_turn: 1,
        },
    );
    world.insert_resource(bindings);
    world.insert_resource(manager);

    let assignments = combat_assignments(&world, player);
    assert_eq!(assignments[&indirect].objective, Some(key_point));
    assert_eq!(
        assignments.get(&loaded_gun).and_then(|a| a.objective),
        Some(key_point)
    );
}

#[test]
fn deployed_unassigned_gun_is_retained_for_production_planning() {
    let player = PlayerId(1);
    let mut world = World::new();
    let position = crate::components::GridPosition { x: 2, y: 3 };
    let gun = world
        .spawn((
            Faction(player),
            position,
            crate::components::UnitStats {
                unit_type: crate::resources::UnitType::Artillery,
                min_range: 3,
                max_range: 6,
                max_movement: 1,
                ..crate::components::UnitStats::mock()
            },
        ))
        .id();
    let defend = crate::components::GridPosition { x: 8, y: 8 };
    let mut memory = crate::ai::v4::capital_route::force_demand::RoutePostureMemory::default();
    memory.by_player.insert(
        player,
        crate::ai::v4::capital_route::force_demand::RoutePostureDecision {
            main: None,
            regions: vec![],
            orders: vec![crate::ai::v4::capital_route::force_demand::RouteOrder {
                key_point: defend,
                staging: defend,
                defend: true,
                force: 0,
            }],
        },
    );
    world.insert_resource(memory);
    world.insert_resource(crate::resources::Map::new(
        10,
        10,
        crate::resources::Terrain::Plains,
        crate::resources::GridTopology::Square,
    ));
    world.insert_resource(SquadManager::default());
    world.insert_resource(UnitOperationRegistry::default());
    assert_eq!(
        combat_assignments(&world, player)[&gun].objective,
        Some(position)
    );
}
