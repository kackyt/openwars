use crate::ai::islands::IslandId;
use crate::components::PlayerId;
use crate::events::{ProduceUnitCommand, UnitProducedEvent};
use crate::resources::UnitType;
use bevy_ecs::prelude::*;
use std::collections::HashMap;

/// 島作戦のために発注したunitの役割。汎用生産へ目的を漏らさず、実Entityまで引き継ぐ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CampaignProductionRole {
    Transport,
    Capture,
    Combat,
}

/// 生産計画器が選んだ命令と、その命令が満たす島作戦上の不足を結び付ける。
#[derive(Debug, Clone)]
pub(crate) struct CampaignProductionIntent {
    pub(crate) command: ProduceUnitCommand,
    pub(crate) island_id: IslandId,
    pub(crate) role: CampaignProductionRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CampaignProductionStatus {
    Planned,
    Issued,
    Produced,
    Delayed,
    Lost,
    Assigned,
}

/// 生産予定から実Entityへの照合結果。再分析時にも作戦所有権を失わない。
#[derive(Debug, Clone)]
pub(crate) struct CampaignProductionRecord {
    pub(crate) player_id: PlayerId,
    pub(crate) planned_turn: u32,
    pub(crate) island_id: IslandId,
    pub(crate) role: CampaignProductionRole,
    pub(crate) facility_x: usize,
    pub(crate) facility_y: usize,
    pub(crate) unit_type: UnitType,
    pub(crate) status: CampaignProductionStatus,
    pub(crate) entity: Option<Entity>,
    pub(crate) resolved_turn: Option<u32>,
}

#[derive(Resource, Debug, Default)]
pub struct V4CampaignExecutionRegistry {
    records: Vec<CampaignProductionRecord>,
}

impl V4CampaignExecutionRegistry {
    /// 同じ手番の再探索を置換し、過去手番の未照合発注は遅延として確定する。
    pub(crate) fn replace_turn_intents(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        intents: &[CampaignProductionIntent],
    ) {
        for record in self.records.iter_mut().filter(|record| {
            record.player_id == player_id
                && record.planned_turn < turn
                && matches!(
                    record.status,
                    CampaignProductionStatus::Planned | CampaignProductionStatus::Issued
                )
        }) {
            record.status = CampaignProductionStatus::Delayed;
        }
        self.records.retain(|record| {
            record.player_id != player_id
                || record.planned_turn != turn
                || !matches!(
                    record.status,
                    CampaignProductionStatus::Planned | CampaignProductionStatus::Issued
                )
        });
        self.records
            .extend(intents.iter().map(|intent| CampaignProductionRecord {
                player_id,
                planned_turn: turn,
                island_id: intent.island_id,
                role: intent.role,
                facility_x: intent.command.target_x,
                facility_y: intent.command.target_y,
                unit_type: intent.command.unit_type,
                status: CampaignProductionStatus::Planned,
                entity: None,
                resolved_turn: None,
            }));
    }

    pub(crate) fn mark_issued(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        command: &ProduceUnitCommand,
    ) {
        if let Some(record) = self.records.iter_mut().find(|record| {
            record.player_id == player_id
                && record.planned_turn == turn
                && record.facility_x == command.target_x
                && record.facility_y == command.target_y
                && record.unit_type == command.unit_type
                && record.status == CampaignProductionStatus::Planned
        }) {
            record.status = CampaignProductionStatus::Issued;
        }
    }

    fn assign_produced(&mut self, event: &UnitProducedEvent, turn: u32) {
        if let Some(record) = self.records.iter_mut().find(|record| {
            record.player_id == event.player_id
                && record.planned_turn == turn
                && record.facility_x == event.target_x
                && record.facility_y == event.target_y
                && record.unit_type == event.unit_type
                && matches!(
                    record.status,
                    CampaignProductionStatus::Issued | CampaignProductionStatus::Planned
                )
        }) {
            record.status = CampaignProductionStatus::Produced;
            record.entity = Some(event.entity);
            record.resolved_turn = Some(turn);
        }
    }

    pub(crate) fn mark_destroyed(&mut self, entity: Entity, turn: u32) {
        for record in self
            .records
            .iter_mut()
            .filter(|record| record.entity == Some(entity))
        {
            record.status = CampaignProductionStatus::Lost;
            record.resolved_turn = Some(turn);
        }
    }

    /// 生産時anchorと異なる作戦へ再配置された場合も、生産命令から実Entityへの照合は完了する。
    /// 現在の配属先はVictoryRoadmap/UnitOperationRegistryを正本とし、旧anchorを永久拘束にしない。
    pub(crate) fn mark_assigned(&mut self, entity: Entity, turn: u32) {
        for record in self.records.iter_mut().filter(|record| {
            record.entity == Some(entity) && record.status == CampaignProductionStatus::Produced
        }) {
            record.status = CampaignProductionStatus::Assigned;
            record.resolved_turn = Some(turn);
        }
    }

    /// 生産直後でまだSquadへ取り込まれていないEntityも、発注元の島へ排他的に予約する。
    pub(crate) fn produced_entity_assignments(
        &self,
        player_id: PlayerId,
    ) -> HashMap<Entity, IslandId> {
        self.records
            .iter()
            .filter(|record| {
                record.player_id == player_id && record.status == CampaignProductionStatus::Produced
            })
            .filter_map(|record| record.entity.map(|entity| (entity, record.island_id)))
            .collect()
    }

    pub(crate) fn records_for(
        &self,
        player_id: PlayerId,
        island_id: IslandId,
    ) -> Vec<&CampaignProductionRecord> {
        self.records
            .iter()
            .filter(|record| record.player_id == player_id && record.island_id == island_id)
            .collect()
    }
}

/// 生産完了Eventを島作戦の発注意図へ照合する。
pub fn reconcile_campaign_production_system(
    match_state: Res<crate::resources::MatchState>,
    mut produced: EventReader<UnitProducedEvent>,
    registry: Option<ResMut<V4CampaignExecutionRegistry>>,
) {
    let Some(mut registry) = registry else {
        return;
    };
    let turn = match_state.current_turn_number.0;
    for event in produced.read() {
        registry.assign_produced(event, turn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produced_entity_keeps_its_campaign_island_until_assignment() {
        let player = PlayerId(2);
        let island = IslandId(3);
        let command = ProduceUnitCommand {
            player_id: player,
            target_x: 4,
            target_y: 5,
            unit_type: UnitType::TransportHelicopter,
        };
        let mut registry = V4CampaignExecutionRegistry::default();
        registry.replace_turn_intents(
            player,
            2,
            &[CampaignProductionIntent {
                command: command.clone(),
                island_id: island,
                role: CampaignProductionRole::Transport,
            }],
        );
        registry.mark_issued(player, 2, &command);
        let entity = Entity::from_raw(42);
        registry.assign_produced(
            &UnitProducedEvent {
                player_id: player,
                target_x: 4,
                target_y: 5,
                unit_type: UnitType::TransportHelicopter,
                entity,
            },
            2,
        );

        assert_eq!(
            registry.produced_entity_assignments(player).get(&entity),
            Some(&island)
        );
        registry.mark_assigned(entity, 3);
        assert!(
            !registry
                .produced_entity_assignments(player)
                .contains_key(&entity)
        );
    }

    #[test]
    fn unproduced_previous_turn_order_is_classified_as_delayed() {
        let player = PlayerId(2);
        let island = IslandId(3);
        let command = ProduceUnitCommand {
            player_id: player,
            target_x: 4,
            target_y: 5,
            unit_type: UnitType::Infantry,
        };
        let mut registry = V4CampaignExecutionRegistry::default();
        registry.replace_turn_intents(
            player,
            2,
            &[CampaignProductionIntent {
                command,
                island_id: island,
                role: CampaignProductionRole::Capture,
            }],
        );
        registry.replace_turn_intents(player, 3, &[]);

        assert_eq!(
            registry.records_for(player, island)[0].status,
            CampaignProductionStatus::Delayed
        );
    }
}
