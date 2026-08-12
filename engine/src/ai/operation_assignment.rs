use crate::ai::emergency::EmergencyMissionId;
use crate::ai::islands::IslandId;
use crate::ai::squad::SquadId;
use crate::components::PlayerId;
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};

/// 1体のunitを所有できる作戦の一意な識別子。
///
/// 戦術Squadは作戦の実行形であり、島作戦は輸送・占領など複数Squadへ分割できる。
/// そのためEntityの正本はSquadIdではなく、この作戦ownerで保持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationOwner {
    Campaign {
        player_id: PlayerId,
        island_id: IslandId,
    },
    Emergency {
        player_id: PlayerId,
        mission_id: EmergencyMissionId,
    },
    TacticalSquad {
        player_id: PlayerId,
        squad_id: SquadId,
    },
}

impl OperationOwner {
    pub fn player_id(self) -> PlayerId {
        match self {
            Self::Campaign { player_id, .. }
            | Self::Emergency { player_id, .. }
            | Self::TacticalSquad { player_id, .. } => player_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationUnitRole {
    Member,
    Transport,
    Cargo,
    DeliveredCargo,
}

/// Entityから唯一の作戦所有者を平均O(1)で引ける正規化済み割当。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitOperationAssignment {
    pub owner: OperationOwner,
    pub squad_id: Option<SquadId>,
    pub role: OperationUnitRole,
    pub assigned_turn: u32,
}

/// 作戦割当の正本。逆引きも持ち、作戦終了時の解放を所属unit数O(k)で行う。
#[derive(Resource, Debug, Default)]
pub struct UnitOperationRegistry {
    by_entity: HashMap<Entity, UnitOperationAssignment>,
    entities_by_owner: HashMap<OperationOwner, HashSet<Entity>>,
    rejected_conflicts: u64,
    last_reconcile_visits: usize,
}

impl UnitOperationRegistry {
    pub fn assignment(&self, entity: Entity) -> Option<UnitOperationAssignment> {
        self.by_entity.get(&entity).copied()
    }

    pub fn campaign_entity_assignments(&self, player_id: PlayerId) -> HashMap<Entity, IslandId> {
        self.by_entity
            .iter()
            .filter_map(|(entity, assignment)| match assignment.owner {
                OperationOwner::Campaign {
                    player_id: owner,
                    island_id,
                } if owner == player_id => Some((*entity, island_id)),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn player_assignments(
        &self,
        player_id: PlayerId,
    ) -> HashMap<Entity, UnitOperationAssignment> {
        self.by_entity
            .iter()
            .filter(|(_, assignment)| assignment.owner.player_id() == player_id)
            .map(|(entity, assignment)| (*entity, *assignment))
            .collect()
    }

    pub fn rejected_conflicts(&self) -> u64 {
        self.rejected_conflicts
    }

    pub fn assigned_count(&self, player_id: PlayerId) -> usize {
        self.by_entity
            .values()
            .filter(|assignment| assignment.owner.player_id() == player_id)
            .count()
    }

    /// playerのcampaign ownerだけを逆引き索引から取得する。
    /// Entity全件を走査せず、終了作戦を所属数O(k)で解放するために使う。
    pub(crate) fn campaign_owners(&self, player_id: PlayerId) -> Vec<OperationOwner> {
        self.entities_by_owner
            .keys()
            .filter(|owner| {
                matches!(
                    owner,
                    OperationOwner::Campaign {
                        player_id: owner_player,
                        ..
                    } if *owner_player == player_id
                )
            })
            .copied()
            .collect()
    }

    pub fn last_reconcile_visits(&self) -> usize {
        self.last_reconcile_visits
    }

    pub(crate) fn note_reconcile_visits(&mut self, visits: usize) {
        self.last_reconcile_visits = visits;
    }

    pub(crate) fn note_rejected_conflict(&mut self) {
        self.rejected_conflicts = self.rejected_conflicts.saturating_add(1);
    }

    /// callerが選んだ唯一の勝者へ割当を更新する。旧ownerの逆引きも同時に外す。
    pub(crate) fn assign(&mut self, entity: Entity, assignment: UnitOperationAssignment) {
        if let Some(previous) = self.by_entity.insert(entity, assignment)
            && previous.owner != assignment.owner
            && let Some(entities) = self.entities_by_owner.get_mut(&previous.owner)
        {
            entities.remove(&entity);
            if entities.is_empty() {
                self.entities_by_owner.remove(&previous.owner);
            }
        }
        self.entities_by_owner
            .entry(assignment.owner)
            .or_default()
            .insert(entity);
    }

    pub(crate) fn release_entity(&mut self, entity: Entity) {
        let Some(previous) = self.by_entity.remove(&entity) else {
            return;
        };
        if let Some(entities) = self.entities_by_owner.get_mut(&previous.owner) {
            entities.remove(&entity);
            if entities.is_empty() {
                self.entities_by_owner.remove(&previous.owner);
            }
        }
    }

    pub fn release_operation(&mut self, owner: OperationOwner) {
        let Some(entities) = self.entities_by_owner.remove(&owner) else {
            return;
        };
        for entity in entities {
            self.by_entity.remove(&entity);
        }
    }

    pub(crate) fn retain_live_entities(&mut self, live_entities: &HashSet<Entity>) {
        let stale = self
            .by_entity
            .keys()
            .filter(|entity| !live_entities.contains(entity))
            .copied()
            .collect::<Vec<_>>();
        for entity in stale {
            self.release_entity(entity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigning_new_owner_removes_old_reverse_membership() {
        let player = PlayerId(1);
        let entity = Entity::from_raw(42);
        let old = OperationOwner::Campaign {
            player_id: player,
            island_id: IslandId(2),
        };
        let new = OperationOwner::Campaign {
            player_id: player,
            island_id: IslandId(3),
        };
        let mut registry = UnitOperationRegistry::default();
        registry.assign(
            entity,
            UnitOperationAssignment {
                owner: old,
                squad_id: Some(SquadId(1)),
                role: OperationUnitRole::Cargo,
                assigned_turn: 1,
            },
        );
        registry.assign(
            entity,
            UnitOperationAssignment {
                owner: new,
                squad_id: Some(SquadId(2)),
                role: OperationUnitRole::Member,
                assigned_turn: 2,
            },
        );

        assert_eq!(registry.assignment(entity).unwrap().owner, new);
        registry.release_operation(old);
        assert_eq!(registry.assignment(entity).unwrap().owner, new);
        registry.release_operation(new);
        assert!(registry.assignment(entity).is_none());
    }
}
