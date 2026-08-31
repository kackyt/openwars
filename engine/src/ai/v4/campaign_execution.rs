use crate::ai::islands::IslandId;
use crate::ai::squad::{MissionPhase, MissionType, SquadId, SquadManager};
use crate::components::{GridPosition, PlayerId};
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
    /// 物件レースなど、生産時点で成立性を検証した具体的な任務先。
    /// Noneの通常Campaignは、従来どおり次回Roadmap分析へ選定を委ねる。
    pub(crate) mission_target: Option<GridPosition>,
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
    pub(crate) mission_target: Option<GridPosition>,
    pub(crate) facility_x: usize,
    pub(crate) facility_y: usize,
    pub(crate) unit_type: UnitType,
    pub(crate) status: CampaignProductionStatus,
    /// 発注時に確保したCampaign用Forming Squad slot。Entityが完成したら同じslotの
    /// Squadへ直ちに入れるため、次手番まで作戦未所属にはしない。
    pub(crate) forming_slot: u64,
    pub(crate) squad_id: Option<SquadId>,
    pub(crate) entity: Option<Entity>,
    pub(crate) resolved_turn: Option<u32>,
}

#[derive(Resource, Debug, Default)]
pub struct V4CampaignExecutionRegistry {
    records: Vec<CampaignProductionRecord>,
    next_forming_slot: u64,
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
        for intent in intents {
            let forming_slot = self.next_forming_slot;
            self.next_forming_slot = self.next_forming_slot.saturating_add(1);
            self.records.push(CampaignProductionRecord {
                player_id,
                planned_turn: turn,
                island_id: intent.island_id,
                role: intent.role,
                mission_target: intent.mission_target,
                facility_x: intent.command.target_x,
                facility_y: intent.command.target_y,
                unit_type: intent.command.unit_type,
                status: CampaignProductionStatus::Planned,
                forming_slot,
                squad_id: None,
                entity: None,
                resolved_turn: None,
            });
        }
    }

    /// 汎用V4生産が選んだCapture枠を、既存の島Campaign発注と競合させずに追記する。
    ///
    /// 陸続き前線のCaptureはRollingPlan側で生産候補を選ぶが、完成した歩兵まで
    /// 汎用Reserveへ落とすとRoadmap Nodeの占領指令を失う。同じ施設・兵種の意図を
    /// 二重登録せず、既にあるCampaign発注は置換しない。
    pub(crate) fn append_turn_intents(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        intents: &[CampaignProductionIntent],
    ) {
        for intent in intents {
            let already_planned = self.records.iter().any(|record| {
                record.player_id == player_id
                    && record.planned_turn == turn
                    && record.facility_x == intent.command.target_x
                    && record.facility_y == intent.command.target_y
                    && record.unit_type == intent.command.unit_type
                    && matches!(
                        record.status,
                        CampaignProductionStatus::Planned | CampaignProductionStatus::Issued
                    )
            });
            if already_planned {
                // RollingPlanが期限付き物件レースを具体化した場合は、同一発注を二重登録せず
                // 既存Campaign意図へ対象物件だけを昇格させる。
                if let Some(mission_target) = intent.mission_target
                    && let Some(record) = self.records.iter_mut().find(|record| {
                        record.player_id == player_id
                            && record.planned_turn == turn
                            && record.facility_x == intent.command.target_x
                            && record.facility_y == intent.command.target_y
                            && record.unit_type == intent.command.unit_type
                            && matches!(
                                record.status,
                                CampaignProductionStatus::Planned
                                    | CampaignProductionStatus::Issued
                            )
                    })
                {
                    record.mission_target = Some(mission_target);
                }
                continue;
            }
            let forming_slot = self.next_forming_slot;
            self.next_forming_slot = self.next_forming_slot.saturating_add(1);
            self.records.push(CampaignProductionRecord {
                player_id,
                planned_turn: turn,
                island_id: intent.island_id,
                role: intent.role,
                mission_target: intent.mission_target,
                facility_x: intent.command.target_x,
                facility_y: intent.command.target_y,
                unit_type: intent.command.unit_type,
                status: CampaignProductionStatus::Planned,
                forming_slot,
                squad_id: None,
                entity: None,
                resolved_turn: None,
            });
        }
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

    /// Campaign生産の受入slotを、完成Eventと同じ更新で実Squadへ解決する。
    ///
    /// 目標座標は次回Roadmap TurnPlanが詳細化するが、島・役割・Forming状態はここで
    /// 固定する。輸送unitも独立した汎用unitにはせず、明示Transport Squadに残す。
    fn resolve_produced_slot(&mut self, entity: Entity, manager: &mut SquadManager) {
        let Some(index) = self.records.iter().position(|record| {
            record.entity == Some(entity) && record.status == CampaignProductionStatus::Produced
        }) else {
            return;
        };
        let record = &mut self.records[index];
        let squad_index = record.squad_id.and_then(|squad_id| {
            manager
                .squads
                .iter()
                .position(|squad| squad.id == squad_id && squad.owner_id == Some(record.player_id))
        });
        let squad = if let Some(index) = squad_index {
            &mut manager.squads[index]
        } else {
            manager.create_owned_squad(
                match record.role {
                    CampaignProductionRole::Transport => MissionType::Transport,
                    CampaignProductionRole::Capture => MissionType::Capture,
                    CampaignProductionRole::Combat => MissionType::Attack,
                },
                record.player_id,
            )
        };
        squad.members.insert(entity);
        squad.target_island = Some(record.island_id);
        squad.target = record.mission_target;
        squad.phase = MissionPhase::Forming;
        record.squad_id = Some(squad.id);
        // slotはrecordと同じ寿命を持つ。debug時に発番漏れを検知できるよう利用する。
        debug_assert!(record.forming_slot < self.next_forming_slot);
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
    manager: Option<ResMut<SquadManager>>,
) {
    let Some(mut registry) = registry else {
        return;
    };
    let turn = match_state.current_turn_number.0;
    let mut manager = manager;
    for event in produced.read() {
        registry.assign_produced(event, turn);
        if let Some(manager) = manager.as_deref_mut() {
            registry.resolve_produced_slot(event.entity, manager);
        }
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
                mission_target: None,
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
    fn produced_campaign_entity_resolves_its_forming_squad_slot() {
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
                command: command.clone(),
                island_id: island,
                role: CampaignProductionRole::Capture,
                mission_target: None,
            }],
        );
        registry.mark_issued(player, 2, &command);
        let entity = Entity::from_raw(42);
        registry.assign_produced(
            &UnitProducedEvent {
                player_id: player,
                target_x: 4,
                target_y: 5,
                unit_type: UnitType::Infantry,
                entity,
            },
            2,
        );
        let mut manager = SquadManager::default();

        registry.resolve_produced_slot(entity, &mut manager);

        let record = registry.records_for(player, island)[0];
        let squad_id = record.squad_id.expect("Campaign slotをSquadへ解決する");
        assert!(record.forming_slot < registry.next_forming_slot);
        assert!(manager.squads.iter().any(|squad| {
            squad.id == squad_id
                && squad.mission_type == MissionType::Capture
                && squad.target_island == Some(island)
                && squad.members.contains(&entity)
        }));
    }

    #[test]
    fn appended_rolling_capture_intent_keeps_campaign_ownership_until_squad_resolution() {
        let player = PlayerId(2);
        let island = IslandId(3);
        let mission_target = GridPosition { x: 8, y: 2 };
        let existing_command = ProduceUnitCommand {
            player_id: player,
            target_x: 3,
            target_y: 5,
            unit_type: UnitType::Mech,
        };
        let capture_command = ProduceUnitCommand {
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
                command: existing_command,
                island_id: island,
                role: CampaignProductionRole::Combat,
                mission_target: None,
            }],
        );
        // RollingPlan由来のCapture枠を同じ手番に重ねても、既存Campaign発注を消さない。
        let capture_intent = CampaignProductionIntent {
            command: capture_command.clone(),
            island_id: island,
            role: CampaignProductionRole::Capture,
            mission_target: Some(mission_target),
        };
        registry.append_turn_intents(player, 2, &[capture_intent.clone(), capture_intent]);
        registry.mark_issued(player, 2, &capture_command);
        let entity = Entity::from_raw(42);
        registry.assign_produced(
            &UnitProducedEvent {
                player_id: player,
                target_x: 4,
                target_y: 5,
                unit_type: UnitType::Infantry,
                entity,
            },
            2,
        );
        let mut manager = SquadManager::default();

        registry.resolve_produced_slot(entity, &mut manager);

        let records = registry.records_for(player, island);
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|record| {
            record.role == CampaignProductionRole::Combat
                && record.status == CampaignProductionStatus::Planned
        }));
        let capture_record = records
            .iter()
            .find(|record| record.entity == Some(entity))
            .expect("RollingPlanのCapture発注を実Entityへ照合する");
        let squad_id = capture_record
            .squad_id
            .expect("Capture発注をCapture Squadへ解決する");
        assert!(manager.squads.iter().any(|squad| {
            squad.id == squad_id
                && squad.mission_type == MissionType::Capture
                && squad.target_island == Some(island)
                && squad.target == Some(mission_target)
                && squad.members.contains(&entity)
        }));
    }

    #[test]
    fn explicit_capture_target_upgrades_an_identical_campaign_order() {
        let player = PlayerId(2);
        let island = IslandId(3);
        let mission_target = GridPosition { x: 8, y: 2 };
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
                command: command.clone(),
                island_id: island,
                role: CampaignProductionRole::Capture,
                mission_target: None,
            }],
        );

        registry.append_turn_intents(
            player,
            2,
            &[CampaignProductionIntent {
                command,
                island_id: island,
                role: CampaignProductionRole::Capture,
                mission_target: Some(mission_target),
            }],
        );

        let records = registry.records_for(player, island);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].mission_target, Some(mission_target));
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
                mission_target: None,
            }],
        );
        registry.replace_turn_intents(player, 3, &[]);

        assert_eq!(
            registry.records_for(player, island)[0].status,
            CampaignProductionStatus::Delayed
        );
    }
}
