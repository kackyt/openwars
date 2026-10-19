//! 現在のSquad任務を、生産の不足見積へ渡す読み取り専用入力。
//! 購入Planの履歴を現在所属として扱わず、新しい所属台帳も持たない。

use super::deployment::ActiveTargetAssignment;
use super::operation::SlotKind;
use crate::ai::operation_assignment::UnitOperationRegistry;
use crate::ai::squad::{MissionPhase, MissionType, SquadManager};
use crate::collections::{HashMap, HashSet};
use crate::components::{Faction, PlayerId};
use bevy_ecs::prelude::*;

/// 購入実績に付記する現在任務中のEntityも、履歴の古いSquadIdではなく正本から読む。
pub(super) fn active_entities(world: &World, player: PlayerId) -> HashSet<Entity> {
    let Some(manager) = world.get_resource::<SquadManager>() else {
        return HashSet::default();
    };
    let Some(registry) = world.get_resource::<UnitOperationRegistry>() else {
        return HashSet::default();
    };
    let active = manager
        .squads
        .iter()
        .filter(|squad| {
            squad.plan.owner_id == Some(player)
                && squad.plan.mission_type != MissionType::Reserve
                && squad.plan.phase != MissionPhase::Completed
        })
        .map(|squad| squad.id)
        .collect::<HashSet<_>>();
    registry
        .bound_entities()
        .into_iter()
        .filter(|entity| {
            registry
                .assignment(*entity)
                .and_then(|binding| binding.squad_id)
                .is_some_and(|id| active.contains(&id))
        })
        .collect()
}

pub(super) fn combat_assignments(
    world: &World,
    player: PlayerId,
) -> HashMap<Entity, ActiveTargetAssignment> {
    let Some(manager) = world.get_resource::<SquadManager>() else {
        return HashMap::default();
    };
    let Some(registry) = world.get_resource::<UnitOperationRegistry>() else {
        return HashMap::default();
    };
    manager
        .squads
        .iter()
        .filter(|squad| {
            squad.plan.owner_id == Some(player)
                && matches!(
                    squad.plan.mission_type,
                    MissionType::Attack | MissionType::Defense
                )
                && squad.plan.phase != MissionPhase::Completed
        })
        .filter_map(|squad| squad.plan.target.map(|target| (squad, target)))
        .flat_map(|(squad, target)| {
            squad
                .members
                .iter()
                .copied()
                .filter(move |entity| {
                    world
                        .get::<Faction>(*entity)
                        .is_some_and(|faction| faction.0 == player)
                        && registry
                            .assignment(*entity)
                            .is_some_and(|binding| binding.squad_id == Some(squad.id))
                })
                .map(move |entity| {
                    (
                        entity,
                        ActiveTargetAssignment {
                            plan_id: None,
                            slot_kind: SlotKind::Combat,
                            objective: Some(target),
                            targets: HashSet::default(),
                        },
                    )
                })
        })
        .collect()
}

#[cfg(test)]
mod tests;
