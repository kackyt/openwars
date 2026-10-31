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
    let mut assignments: HashMap<Entity, ActiveTargetAssignment> = manager
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
            let route_members = &manager.route_operation_members;
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
                    // 進軍路作戦の間接砲はSquad目標が射撃マスになるため、作戦の要衝を正本として返す。
                    let objective = route_members.get(&entity).copied().unwrap_or(target);
                    (
                        entity,
                        ActiveTargetAssignment {
                            plan_id: None,
                            slot_kind: SlotKind::Combat,
                            objective: Some(objective),
                            targets: HashSet::default(),
                        },
                    )
                })
        })
        .collect();

    // 補給輸送車に積載中の砲台はSquadから一時的に外れているが、既存戦力として作戦に残す。
    let defend_points = world
        .get_resource::<crate::ai::v4::capital_route::force_demand::RoutePostureMemory>()
        .and_then(|memory| memory.by_player.get(&player))
        .map(|decision| {
            decision
                .orders
                .iter()
                .filter(|order| order.defend)
                .map(|order| order.key_point)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let map = world.get_resource::<crate::resources::Map>();
    for entity_ref in world.iter_entities() {
        let entity = entity_ref.id();
        if assignments.contains_key(&entity) {
            continue;
        }
        let (Some(faction), Some(transporting), Some(stats)) = (
            entity_ref.get::<Faction>(),
            entity_ref.get::<crate::components::Transporting>(),
            entity_ref.get::<crate::components::UnitStats>(),
        ) else {
            continue;
        };
        if faction.0 != player || !super::artillery_ferry::is_ferried_gun(stats) {
            continue;
        }
        let objective = manager
            .route_operation_members
            .get(&entity)
            .copied()
            .or_else(|| super::artillery_ferry::assigned_attack_route(world, player, entity))
            .or_else(|| {
                let carrier_pos = world.get::<crate::components::GridPosition>(transporting.0)?;
                let map = map?;
                defend_points
                    .iter()
                    .min_by_key(|point| {
                        (
                            map.distance(carrier_pos.x, carrier_pos.y, point.x, point.y),
                            point.y,
                            point.x,
                        )
                    })
                    .copied()
            });
        if let Some(target) = objective {
            assignments.insert(
                entity,
                ActiveTargetAssignment {
                    plan_id: None,
                    slot_kind: SlotKind::Combat,
                    objective: Some(target),
                    targets: HashSet::default(),
                },
            );
        }
    }
    assignments
}

#[cfg(test)]
mod tests;
