//! 生産完了イベントの受付。
//!
//! 一度読んだ同じイベント列を両台帳へ渡す。各台帳は照合事実を記録し、受入slotの
//! 所属を提案するだけで、SquadManagerを直接変更しない。提案は配備の全件→campaignの
//! 全件の順に積み、`OperationLedger::commit_produced` で1回だけ確定する。
//! これにより従来の発番順（イベントごとに交互適用しない）を保つ。
//! 両台帳が同じEntityを提案した場合も両方を適用し、所属競合は正規化resolveで解決する。

use super::campaign_execution::{self, V4CampaignExecutionRegistry};
use super::deployment::{self, V4DeploymentRegistry};
use crate::ai::islands::IslandMap;
use crate::ai::planning::assignment::{OperationLedger, ProducedAssignmentProposal};
use crate::ai::squad::SquadManager;
use crate::events::UnitProducedEvent;
use crate::resources::MatchState;
use bevy_ecs::prelude::*;

/// 生産イベントを受け取る唯一のSystem。各台帳の適用順と任意resourceの扱いを維持する。
pub(crate) fn reconcile_production_events_system(
    mut events: EventReader<UnitProducedEvent>,
    match_state: Res<MatchState>,
    deployment: Option<ResMut<V4DeploymentRegistry>>,
    campaign: Option<ResMut<V4CampaignExecutionRegistry>>,
    mut manager: Option<ResMut<SquadManager>>,
    island_map: Option<Res<IslandMap>>,
) {
    let turn = match_state.current_turn_number.0;
    // Eventをcloneせず、readerから借用した列を2段階へ渡す。
    let events = events.read().collect::<Vec<_>>();
    let mut deployment = deployment;
    let mut campaign = campaign;
    let mut proposal = ProducedAssignmentProposal::default();
    let deployment_receipts = deployment.as_deref_mut().map(|registry| {
        deployment::propose_produced_events(
            &events,
            turn,
            registry,
            manager.as_deref(),
            island_map.as_deref(),
            &mut proposal,
        )
    });
    let campaign_receipts = campaign.as_deref_mut().map(|registry| {
        campaign_execution::propose_produced_events(
            &events,
            turn,
            registry,
            manager.as_deref(),
            &mut proposal,
        )
    });
    // SquadManagerが無ければ提案も無い（照合事実だけを記録する）。
    let Some(manager) = manager.as_deref_mut() else {
        return;
    };
    let delta = OperationLedger::commit_produced(manager, proposal);
    if let (Some(registry), Some(receipts)) = (deployment.as_deref_mut(), &deployment_receipts) {
        deployment::bind_produced_squads(registry, receipts, &delta);
    }
    if let (Some(registry), Some(receipts)) = (campaign.as_deref_mut(), &campaign_receipts) {
        campaign_execution::bind_produced_squads(registry, receipts, &delta);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::islands::IslandId;
    use crate::ai::squad::{MissionType, SquadId};
    use crate::ai::v4::campaign_execution::{
        CampaignFormingReservation, CampaignProductionIntent, CampaignProductionRole,
    };
    use crate::ai::v4::deployment::{DeploymentForecast, DeploymentPosture, PendingDeployment};
    use crate::ai::v4::operation::SlotKind;
    use crate::components::{GridPosition, PlayerId};
    use crate::events::ProduceUnitCommand;
    use crate::resources::{TurnNumber, UnitType};

    #[test]
    fn one_receipt_preserves_all_deployment_then_all_campaign_squad_ids() {
        let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let island = IslandId(0);
        let turn = 3;
        let entities = [Entity::from_raw(100), Entity::from_raw(101)];
        let mut deployment = V4DeploymentRegistry::default();
        let mut campaign = V4CampaignExecutionRegistry::default();
        let positions = [GridPosition { x: 0, y: 0 }, GridPosition { x: 4, y: 0 }];
        deployment.replace_turn_orders(
            player,
            turn,
            positions
                .into_iter()
                .enumerate()
                .map(|(order, facility)| PendingDeployment {
                    player_id: player,
                    turn,
                    order: order as u32,
                    facility,
                    unit_type: UnitType::Infantry,
                    anchor: facility,
                    staging_anchor: facility,
                    posture: DeploymentPosture::Forming,
                    slot_kind: SlotKind::Combat,
                    priority_enemies: Vec::new(),
                    threat_horizon: 1,
                    forecast: DeploymentForecast::default(),
                    plan_step: None,
                    forming_slot: None,
                    operation_binding: None,
                }),
        );
        let intents = positions.map(|facility| CampaignProductionIntent {
            command: ProduceUnitCommand {
                player_id: player,
                target_x: facility.x,
                target_y: facility.y,
                unit_type: UnitType::Infantry,
            },
            island_id: island,
            role: CampaignProductionRole::Capture,
            mission_target: None,
        });
        campaign.replace_turn_intents(
            player,
            turn,
            &intents,
            CampaignFormingReservation::Immediate,
        );
        let mut world = World::new();
        world.insert_resource(MatchState {
            current_turn_number: TurnNumber(turn),
            ..Default::default()
        });
        world.insert_resource(deployment);
        world.insert_resource(campaign);
        world.insert_resource(SquadManager::default());
        world.init_resource::<Events<UnitProducedEvent>>();
        for (entity, position) in entities.into_iter().zip(positions) {
            world.send_event(UnitProducedEvent {
                player_id: player,
                entity,
                target_x: position.x,
                target_y: position.y,
                unit_type: UnitType::Infantry,
            });
        }
        let mut schedule = Schedule::default();
        schedule.add_systems(reconcile_production_events_system);
        schedule.run(&mut world);
        let squads = &world.resource::<SquadManager>().squads;
        assert_eq!(squads.len(), 4);
        // eventごとに交互適用するとD1,C1,D2,C2になり、この契約を満たさない。
        for (index, (mission, entity)) in [
            (MissionType::Defense, entities[0]),
            (MissionType::Defense, entities[1]),
            (MissionType::Capture, entities[0]),
            (MissionType::Capture, entities[1]),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(squads[index].id, SquadId(index as u32));
            assert_eq!(squads[index].mission_type, mission);
            assert_eq!(squads[index].owner_id, Some(player));
            assert_eq!(
                squads[index].members.iter().copied().collect::<Vec<_>>(),
                vec![entity]
            );
        }
        let records = world
            .resource::<V4CampaignExecutionRegistry>()
            .records_for(player, island);
        assert_eq!(records[0].squad_id, Some(SquadId(2)));
        assert_eq!(records[1].squad_id, Some(SquadId(3)));
        // readerが同じイベントを再消費せず、次フレームでは発番も照合事実も変わらない。
        schedule.run(&mut world);
        let next = world
            .resource_mut::<SquadManager>()
            .create_owned_squad(MissionType::Attack, player)
            .id;
        assert_eq!(next, SquadId(4));
    }

    fn campaign_intent(player: PlayerId, facility: GridPosition) -> CampaignProductionIntent {
        CampaignProductionIntent {
            command: ProduceUnitCommand {
                player_id: player,
                target_x: facility.x,
                target_y: facility.y,
                unit_type: UnitType::Infantry,
            },
            island_id: IslandId(0),
            role: CampaignProductionRole::Capture,
            mission_target: None,
        }
    }

    fn deployment_order(
        player: PlayerId,
        turn: u32,
        order: u32,
        facility: GridPosition,
    ) -> PendingDeployment {
        PendingDeployment {
            player_id: player,
            turn,
            order,
            facility,
            unit_type: UnitType::Infantry,
            anchor: facility,
            staging_anchor: facility,
            posture: DeploymentPosture::Forming,
            slot_kind: SlotKind::Combat,
            priority_enemies: Vec::new(),
            threat_horizon: 1,
            forecast: DeploymentForecast::default(),
            plan_step: None,
            forming_slot: None,
            operation_binding: None,
        }
    }

    fn produced(player: PlayerId, entity: Entity, facility: GridPosition) -> UnitProducedEvent {
        UnitProducedEvent {
            player_id: player,
            entity,
            target_x: facility.x,
            target_y: facility.y,
            unit_type: UnitType::Infantry,
        }
    }

    fn world_at(turn: u32) -> World {
        let mut world = World::new();
        world.insert_resource(MatchState {
            current_turn_number: TurnNumber(turn),
            ..Default::default()
        });
        world.init_resource::<Events<UnitProducedEvent>>();
        world
    }

    /// 同じ完成Eventが同じ受付に2回届いても、campaign recordの2回目の提案は先行提案の
    /// Squad（確定前）へ合流し、従来の逐次解決と同じく1 Squadだけになる。
    #[test]
    fn duplicate_campaign_event_in_one_receipt_joins_the_first_proposal() {
        let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let facility = GridPosition { x: 1, y: 1 };
        let entity = Entity::from_raw(100);
        let mut campaign = V4CampaignExecutionRegistry::default();
        campaign.replace_turn_intents(
            player,
            3,
            &[campaign_intent(player, facility)],
            CampaignFormingReservation::Immediate,
        );
        let mut world = world_at(3);
        world.insert_resource(campaign);
        world.insert_resource(SquadManager::default());
        world.send_event(produced(player, entity, facility));
        world.send_event(produced(player, entity, facility));
        let mut schedule = Schedule::default();
        schedule.add_systems(reconcile_production_events_system);
        schedule.run(&mut world);
        let squads = &world.resource::<SquadManager>().squads;
        assert_eq!(squads.len(), 1);
        assert_eq!(
            squads[0].members.iter().copied().collect::<Vec<_>>(),
            vec![entity]
        );
        let records = world
            .resource::<V4CampaignExecutionRegistry>()
            .records_for(player, IslandId(0));
        assert_eq!(records[0].squad_id, Some(squads[0].id));
    }

    /// 異なるplayerのEventが交互に届いても、発番は配備の全件→campaignの全件の順で、
    /// 各Squadの所有者はEventのplayerになる。
    #[test]
    fn mixed_players_keep_receipt_order_and_owners() {
        let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let players = [PlayerId(1), PlayerId(2)];
        let facilities = [GridPosition { x: 0, y: 0 }, GridPosition { x: 5, y: 5 }];
        let entities = [Entity::from_raw(100), Entity::from_raw(101)];
        let mut deployment = V4DeploymentRegistry::default();
        let mut campaign = V4CampaignExecutionRegistry::default();
        for (player, facility) in players.into_iter().zip(facilities) {
            deployment.replace_turn_orders(player, 3, [deployment_order(player, 3, 0, facility)]);
            campaign.replace_turn_intents(
                player,
                3,
                &[campaign_intent(player, facility)],
                CampaignFormingReservation::Immediate,
            );
        }
        let mut world = world_at(3);
        world.insert_resource(deployment);
        world.insert_resource(campaign);
        world.insert_resource(SquadManager::default());
        for ((player, facility), entity) in players.into_iter().zip(facilities).zip(entities) {
            world.send_event(produced(player, entity, facility));
        }
        let mut schedule = Schedule::default();
        schedule.add_systems(reconcile_production_events_system);
        schedule.run(&mut world);
        let squads = &world.resource::<SquadManager>().squads;
        let observed = squads
            .iter()
            .map(|squad| (squad.id, squad.owner_id, squad.mission_type.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            vec![
                (SquadId(0), Some(players[0]), MissionType::Defense),
                (SquadId(1), Some(players[1]), MissionType::Defense),
                (SquadId(2), Some(players[0]), MissionType::Capture),
                (SquadId(3), Some(players[1]), MissionType::Capture),
            ]
        );
    }

    /// SquadManagerが無い場合は照合事実だけを記録し、所属提案も確定もしない。
    #[test]
    fn without_squad_manager_only_ledger_facts_are_recorded() {
        let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let facility = GridPosition { x: 1, y: 1 };
        let entity = Entity::from_raw(100);
        let mut deployment = V4DeploymentRegistry::default();
        deployment.replace_turn_orders(player, 3, [deployment_order(player, 3, 0, facility)]);
        let mut campaign = V4CampaignExecutionRegistry::default();
        campaign.replace_turn_intents(
            player,
            3,
            &[campaign_intent(player, facility)],
            CampaignFormingReservation::Immediate,
        );
        let mut world = world_at(3);
        world.insert_resource(deployment);
        world.insert_resource(campaign);
        world.send_event(produced(player, entity, facility));
        let mut schedule = Schedule::default();
        schedule.add_systems(reconcile_production_events_system);
        schedule.run(&mut world);
        assert!(world.get_resource::<SquadManager>().is_none());
        assert!(
            world
                .resource::<V4DeploymentRegistry>()
                .active_entities(player)
                .contains(&entity)
        );
        let records = world
            .resource::<V4CampaignExecutionRegistry>()
            .records_for(player, IslandId(0));
        assert_eq!(records[0].entity, Some(entity));
        assert_eq!(records[0].squad_id, None);
    }

    /// 既存Squadの所有者が変わっていたら、そのSquadへは合流せず新設する（従来と同じ）。
    #[test]
    fn existing_squad_with_other_owner_is_not_reused() {
        let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let facility = GridPosition { x: 1, y: 1 };
        let entity = Entity::from_raw(100);
        let mut campaign = V4CampaignExecutionRegistry::default();
        campaign.replace_turn_intents(
            player,
            3,
            &[campaign_intent(player, facility)],
            CampaignFormingReservation::Immediate,
        );
        let mut world = world_at(3);
        world.insert_resource(campaign);
        world.insert_resource(SquadManager::default());
        world.send_event(produced(player, entity, facility));
        let mut schedule = Schedule::default();
        schedule.add_systems(reconcile_production_events_system);
        schedule.run(&mut world);
        let first = world.resource::<SquadManager>().squads[0].id;
        world.resource_mut::<SquadManager>().squads_mut()[0].owner_id = Some(PlayerId(2));
        // 同じEventの再受信: record.squad_idは他player所有なのでExistingにならない。
        world.send_event(produced(player, entity, facility));
        schedule.run(&mut world);
        let squads = &world.resource::<SquadManager>().squads;
        assert_eq!(squads.len(), 2);
        assert_eq!(squads[1].owner_id, Some(player));
        assert!(squads[1].members.contains(&entity));
        let records = world
            .resource::<V4CampaignExecutionRegistry>()
            .records_for(player, IslandId(0));
        assert_ne!(records[0].squad_id, Some(first));
        assert_eq!(records[0].squad_id, Some(squads[1].id));
    }
}
