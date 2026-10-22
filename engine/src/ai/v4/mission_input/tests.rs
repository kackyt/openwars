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
