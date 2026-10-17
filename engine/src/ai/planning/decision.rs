//! 当手番で採用した戦略ポートフォリオの一時保持。手番境界で破棄する。
use crate::ai::islands::IslandId;
use crate::collections::{HashMap, HashSet};
use crate::components::PlayerId;

#[derive(Default)]
struct CampaignRoleIntent {
    capture: HashSet<Entity>,
    combat: HashSet<Entity>,
}
use bevy_ecs::prelude::*;

#[derive(Resource, Default)]
pub(crate) struct AdoptedTurnPortfolio {
    player_id: Option<PlayerId>,
    role_intents: HashMap<IslandId, CampaignRoleIntent>,
    committed_capture_reservations: HashMap<IslandId, HashSet<Entity>>,
    campaign_portfolio: Option<crate::ai::island_campaign::IslandCampaignPortfolio>,
}
impl AdoptedTurnPortfolio {
    pub(crate) fn set_campaign_portfolio(
        &mut self,
        player_id: PlayerId,
        portfolio: crate::ai::island_campaign::IslandCampaignPortfolio,
    ) {
        if self.player_id != Some(player_id) {
            self.clear();
            self.player_id = Some(player_id);
        }
        // 予約は採用時の意図として保存し、実所属の投影で対象を増やさない。
        self.role_intents.clear();
        self.committed_capture_reservations.clear();
        for campaign in portfolio
            .defenses
            .iter()
            .chain(portfolio.active_offensives.iter())
        {
            self.role_intents
                .entry(campaign.island_id)
                .or_insert_with(|| CampaignRoleIntent {
                    capture: campaign.capture_entities.iter().copied().collect(),
                    combat: campaign.combat_entities.iter().copied().collect(),
                });
        }
        self.campaign_portfolio = Some(portfolio);
    }
    pub(crate) fn campaign_portfolio(
        &self,
        player_id: PlayerId,
    ) -> Option<&crate::ai::island_campaign::IslandCampaignPortfolio> {
        (self.player_id == Some(player_id))
            .then_some(self.campaign_portfolio.as_ref())
            .flatten()
    }
    /// 編成前の候補Entity列を、commit後の実所属だけの投影へ置き換える。
    /// 目標・予算・購入不足は戦略の意図なので保持し、所属と混同しない。
    pub(crate) fn project_committed(
        &mut self,
        world: &World,
        player: PlayerId,
        manager: &crate::ai::planning::assignment::squad_model::SquadManager,
    ) {
        use crate::ai::operation_assignment::{
            OperationOwner, OperationUnitRole, UnitOperationRegistry,
        };
        use crate::ai::planning::assignment::squad_model::MissionType;
        use crate::collections::HashMap;
        use crate::components::{Faction, UnitStats};
        if self.player_id != Some(player) {
            return;
        }
        let Some(portfolio) = self.campaign_portfolio.as_mut() else {
            return;
        };
        self.committed_capture_reservations.clear();
        let assignments = world
            .get_resource::<UnitOperationRegistry>()
            .map(|r| r.player_assignments(player))
            .unwrap_or_default();
        let missions = manager
            .squads
            .iter()
            .filter(|s| s.plan.owner_id == Some(player))
            .map(|s| (s.id, &s.plan.mission_type))
            .collect::<HashMap<_, _>>();
        for campaign in portfolio
            .defenses
            .iter_mut()
            .chain(portfolio.active_offensives.iter_mut())
        {
            // Cargoの役割は不変の採用意図から復元し、再投影の結果に引きずられない。
            let intent = self.role_intents.get(&campaign.island_id);
            campaign.transport_entities.clear();
            campaign.capture_entities.clear();
            campaign.combat_entities.clear();
            for (entity, current) in &assignments {
                if current.owner
                    != (OperationOwner::Campaign {
                        player_id: player,
                        island_id: campaign.island_id,
                    })
                    || current.squad_id.is_none()
                    || world.get::<Faction>(*entity).is_none_or(|f| f.0 != player)
                {
                    continue;
                }
                let Some(mission) = current.squad_id.and_then(|id| missions.get(&id)) else {
                    continue;
                };
                let Some(stats) = world.get::<UnitStats>(*entity) else {
                    continue;
                };
                if current.role == OperationUnitRole::Transport {
                    campaign.transport_entities.push(*entity);
                } else if stats.can_capture
                    && match current.role {
                        // 攻撃・防衛・Reserveの歩兵を、占領能力だけでCapture充足へ数えない。
                        OperationUnitRole::Member => matches!(**mission, MissionType::Capture),
                        OperationUnitRole::Cargo | OperationUnitRole::DeliveredCargo => {
                            intent.is_some_and(|i| i.capture.contains(entity))
                                || !intent.is_some_and(|i| i.combat.contains(entity))
                        }
                        OperationUnitRole::Transport => false,
                    }
                {
                    campaign.capture_entities.push(*entity);
                } else {
                    campaign.combat_entities.push(*entity);
                }
            }
            // 生産予約は採用された担当と現在のCapture実所属の積集合に限定する。
            self.committed_capture_reservations
                .entry(campaign.island_id)
                .or_insert_with(|| {
                    campaign
                        .capture_entities
                        .iter()
                        .copied()
                        .filter(|e| intent.is_some_and(|i| i.capture.contains(e)))
                        .collect()
                });
            for entities in [
                &mut campaign.transport_entities,
                &mut campaign.capture_entities,
                &mut campaign.combat_entities,
            ] {
                entities.sort_by_key(|e| e.to_bits());
            }
        }
    }

    /// 未投影と、投影済みだが予約なしの状態を区別する。
    pub(crate) fn committed_capture_reservations(
        &self,
        player: PlayerId,
        island: IslandId,
    ) -> Option<&HashSet<Entity>> {
        (self.player_id == Some(player))
            .then(|| self.committed_capture_reservations.get(&island))
            .flatten()
    }

    pub(crate) fn clear(&mut self) {
        self.role_intents.clear();
        self.committed_capture_reservations.clear();
        self.player_id = None;
        self.campaign_portfolio = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::island_campaign::{
        IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignPortfolio,
        IslandCampaignRequirement,
    };
    use crate::ai::islands::IslandId;
    use crate::ai::operation_assignment::{
        OperationOwner, OperationUnitRole, UnitOperationAssignment, UnitOperationRegistry,
    };
    use crate::ai::planning::assignment::LedgerKey;
    use crate::ai::planning::assignment::squad_model::{MissionType, SquadId, SquadManager};
    use crate::components::{Faction, GridPosition, UnitStats};

    /// 競合の敗者・実行bindingなし・死亡Entityを採用一覧へ復活させず、購入意図は保持する。
    #[test]
    fn committed_portfolio_contains_only_live_current_campaign_bindings() {
        let player = PlayerId(1);
        let island = IslandId(7);
        let mut world = World::new();
        let current = world
            .spawn((
                Faction(player),
                UnitStats {
                    can_capture: true,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let detached = world.spawn((Faction(player), UnitStats::mock())).id();
        let other = world.spawn((Faction(player), UnitStats::mock())).id();
        let dead = world.spawn_empty().id();
        world.despawn(dead);
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 1,
            capture_units: 2,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 5000,
        };
        let assignment = IslandCampaignAssignment {
            island_id: island,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 2, y: 3 },
            capture_target_positions: vec![],
            priority_enemy_types: vec![],
            requirement: requirement.clone(),
            purchase_shortfall: requirement.clone(),
            allocated_budget: 5000,
            transport_entities: vec![],
            capture_entities: vec![other, dead],
            combat_entities: vec![detached],
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        let mut manager = SquadManager::new();
        manager.create_owned_squad(MissionType::Defense, player);
        {
            let mut proposals =
                crate::ai::planning::assignment::OperationLedger::proposals(&mut manager);
            let mut squad = proposals.create_owned(
                MissionType::Capture,
                player,
                std::collections::BTreeSet::from([current]),
                None,
                vec![],
            );
            assert_eq!(squad.id, SquadId(1));
            squad.plan.target_island = Some(island);
        }
        let mut registry = UnitOperationRegistry::default();
        for (entity, squad_id, owner) in [
            (
                current,
                Some(SquadId(1)),
                OperationOwner::Campaign {
                    player_id: player,
                    island_id: island,
                },
            ),
            (
                detached,
                None,
                OperationOwner::Campaign {
                    player_id: player,
                    island_id: island,
                },
            ),
            (
                other,
                Some(SquadId(2)),
                OperationOwner::TacticalSquad {
                    player_id: player,
                    squad_id: SquadId(2),
                },
            ),
            (
                dead,
                Some(SquadId(1)),
                OperationOwner::Campaign {
                    player_id: player,
                    island_id: island,
                },
            ),
        ] {
            registry.apply_assign(
                LedgerKey::fixture(),
                entity,
                UnitOperationAssignment {
                    owner,
                    squad_id,
                    role: OperationUnitRole::Member,
                    assigned_turn: 1,
                },
            );
        }
        world.insert_resource(registry);
        let mut cache = AdoptedTurnPortfolio::default();
        cache.set_campaign_portfolio(
            player,
            IslandCampaignPortfolio {
                active_offensives: vec![assignment],
                ..Default::default()
            },
        );
        cache.project_committed(&world, player, &manager);
        let projected = &cache.campaign_portfolio(player).unwrap().active_offensives[0];
        assert_eq!(projected.capture_entities, vec![current]);
        assert!(
            cache
                .committed_capture_reservations(player, island)
                .unwrap()
                .is_empty()
        );
        assert!(projected.transport_entities.is_empty());
        assert!(projected.combat_entities.is_empty());
        assert_eq!(projected.purchase_shortfall, requirement);
        assert_eq!(projected.target_position, GridPosition { x: 2, y: 3 });
        assert!(cache.campaign_portfolio(PlayerId(2)).is_none());
    }
    /// 占領能力と実際の任務を区別し、攻撃歩兵・Combat cargoで占領枠を埋めない。
    #[test]
    fn committed_capture_slots_exclude_attack_infantry_and_combat_cargo() {
        use crate::ai::planning::assignment::OperationLedger;
        use std::collections::BTreeSet;
        let player = PlayerId(1);
        let island = IslandId(7);
        let mut world = World::new();
        let [capture, attacker, combat_cargo, extra_capture] = [0, 1, 2, 3].map(|_| {
            world
                .spawn((
                    Faction(player),
                    UnitStats {
                        can_capture: true,
                        ..UnitStats::mock()
                    },
                ))
                .id()
        });
        let mut manager = SquadManager::new();
        let mut registry = UnitOperationRegistry::default();
        {
            let mut proposals = OperationLedger::proposals(&mut manager);
            for (entity, mission, role) in [
                (capture, MissionType::Capture, OperationUnitRole::Member),
                (
                    extra_capture,
                    MissionType::Capture,
                    OperationUnitRole::Member,
                ),
                (attacker, MissionType::Attack, OperationUnitRole::Member),
                (
                    combat_cargo,
                    MissionType::Transport,
                    OperationUnitRole::Cargo,
                ),
            ] {
                let (members, cargo) = if role == OperationUnitRole::Cargo {
                    (BTreeSet::new(), vec![entity])
                } else {
                    (BTreeSet::from([entity]), vec![])
                };
                let mut squad = proposals.create_owned(mission, player, members, None, cargo);
                squad.plan.target_island = Some(island);
                registry.apply_assign(
                    LedgerKey::fixture(),
                    entity,
                    UnitOperationAssignment {
                        owner: OperationOwner::Campaign {
                            player_id: player,
                            island_id: island,
                        },
                        squad_id: Some(squad.id),
                        role,
                        assigned_turn: 1,
                    },
                );
            }
        }
        world.insert_resource(registry);
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 2,
            ground_combat_units: 0,
            combat_units: 1,
            total_budget: 5000,
        };
        let campaign = IslandCampaignAssignment {
            island_id: island,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 2, y: 3 },
            capture_target_positions: vec![],
            priority_enemy_types: vec![],
            requirement: requirement.clone(),
            purchase_shortfall: requirement,
            allocated_budget: 5000,
            transport_entities: vec![],
            capture_entities: vec![capture, attacker],
            combat_entities: vec![combat_cargo],
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        let mut cache = AdoptedTurnPortfolio::default();
        cache.set_campaign_portfolio(
            player,
            IslandCampaignPortfolio {
                active_offensives: vec![campaign],
                ..Default::default()
            },
        );
        cache.project_committed(&world, player, &manager);
        let actual = cache.campaign_portfolio(player).unwrap().active_offensives[0].clone();
        let mut captures = vec![capture, extra_capture];
        captures.sort_by_key(|e| e.to_bits());
        assert_eq!(actual.capture_entities, captures);
        assert_eq!(
            cache
                .committed_capture_reservations(player, island)
                .unwrap(),
            &HashSet::from_iter([capture])
        );
        cache.project_committed(&world, player, &manager);
        assert_eq!(
            cache
                .committed_capture_reservations(player, island)
                .unwrap(),
            &HashSet::from_iter([capture])
        );
        let mut expected_combat = vec![attacker, combat_cargo];
        expected_combat.sort_by_key(|e| e.to_bits());
        assert_eq!(actual.combat_entities, expected_combat);
    }
}
