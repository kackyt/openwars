use crate::ai::islands::IslandId;
#[cfg(test)]
use crate::ai::planning::assignment::OperationLedger;
use crate::ai::planning::assignment::{
    ProducedAssignmentDelta, ProducedAssignmentProposal, ProducedJoin, ProducedSquad,
};
use crate::ai::squad::{MissionType, SquadId, SquadManager};
use crate::collections::HashMap;
use crate::components::{GridPosition, PlayerId};
use crate::events::{ProduceUnitCommand, UnitProducedEvent};
use crate::resources::UnitType;
use bevy_ecs::prelude::*;
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

/// 生産完成時にSquadへ即時接続するかの方針。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CampaignFormingReservation {
    /// 通常経路の発注。次回再編まで即時Squad解決を行わない。
    Deferred,
    /// 拡張経路の発注。完成Eventと同時にSquadへ即時接続する。
    Immediate,
}

/// Campaign発注の受入予約。SquadIdは実体完成後にSquadManagerが発番する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CampaignFormingSlotId(u64);

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
    /// 発注時に確保したCampaign用Forming Squad slot。
    /// - `Some`: 完成Eventと同時にSquadへ即時接続する（拡張経路）。
    /// - `None`: 通常経路の発注。次回再編まで即時Squad解決を行わない。
    forming_slot: Option<CampaignFormingSlotId>,
    pub(crate) squad_id: Option<SquadId>,
    pub(crate) entity: Option<Entity>,
    pub(crate) resolved_turn: Option<u32>,
}

impl CampaignProductionRecord {
    /// 指定されたプレイヤー・手番・施設・兵種の未完了発注（PlannedまたはIssued）に一致するか。
    fn matches_open_command(
        &self,
        player_id: PlayerId,
        turn: u32,
        command: &ProduceUnitCommand,
    ) -> bool {
        self.player_id == player_id
            && self.planned_turn == turn
            && self.facility_x == command.target_x
            && self.facility_y == command.target_y
            && self.unit_type == command.unit_type
            && matches!(
                self.status,
                CampaignProductionStatus::Planned | CampaignProductionStatus::Issued
            )
    }

    /// 指定されたプレイヤー・手番・施設・兵種の生産完了イベントに一致するか。
    fn matches_produced_event(&self, event: &UnitProducedEvent, turn: u32) -> bool {
        self.player_id == event.player_id
            && self.planned_turn == turn
            && self.facility_x == event.target_x
            && self.facility_y == event.target_y
            && self.unit_type == event.unit_type
            && matches!(
                self.status,
                CampaignProductionStatus::Planned | CampaignProductionStatus::Issued
            )
    }
}

#[derive(Resource, Debug, Default)]
pub struct V4CampaignExecutionRegistry {
    records: Vec<CampaignProductionRecord>,
    next_forming_slot: u64,
}

impl V4CampaignExecutionRegistry {
    /// 発注専用の予約IDを取得する。0も有効なIDで、予約なしはOptionで区別する。
    fn reserve_forming_slot(&mut self) -> CampaignFormingSlotId {
        let slot = CampaignFormingSlotId(self.next_forming_slot);
        self.next_forming_slot = self.next_forming_slot.saturating_add(1);
        slot
    }

    /// 同じ手番の再探索を置換し、過去手番の未照合発注は遅延として確定する。
    /// 予約方針（Immediate / Deferred）は呼び出し側から明示的に指定する。
    pub(crate) fn replace_turn_intents(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        intents: &[CampaignProductionIntent],
        reservation: CampaignFormingReservation,
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
        // 即時編成の必要性は引数から決定する。
        let reserves_slots = matches!(reservation, CampaignFormingReservation::Immediate);
        for intent in intents {
            let forming_slot = reserves_slots.then(|| self.reserve_forming_slot());
            self.records.push(CampaignProductionRecord {
                mission_target: None,
                forming_slot,
                squad_id: None,
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
        // 一度照合したEntityの完成Eventは再消費しない。再計画後に同じ施設・兵種へ
        // 後続発注があっても、古い完成Eventをその発注へ誤接続してはならない。
        if self
            .records
            .iter()
            .any(|record| record.entity == Some(event.entity))
        {
            return;
        }
        if let Some(record) = self
            .records
            .iter_mut()
            .find(|record| record.matches_produced_event(event, turn))
        {
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

    /// 汎用V4生産が選んだCapture枠を、既存の島Campaign発注と競合させずに追記する。
    ///
    /// 拡張経路専用であり、常にForming Slot（Immediate）を予約する。
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
            if let Some(record) = self
                .records
                .iter_mut()
                .find(|record| record.matches_open_command(player_id, turn, &intent.command))
            {
                // RollingPlanが期限付き物件レースを具体化した場合は、同一発注を二重登録せず
                // 既存Campaign意図へ対象物件だけを昇格させる。
                if let Some(mission_target) = intent.mission_target {
                    record.mission_target = Some(mission_target);
                }
                continue;
            }
            let forming_slot = Some(self.reserve_forming_slot());
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

    /// Campaign生産の受入slotを、受入Squadへの所属提案にする（SquadManagerは変更しない）。
    ///
    /// 目標座標は次回Roadmap TurnPlanが詳細化するが、島・役割・Forming状態はここで
    /// 固定する。輸送unitも独立した汎用unitにはせず、明示Transport Squadに残す。
    /// 同じ受付で同じrecordが先に提案済みなら、そのSquad（確定前）へ合流させる。
    fn propose_produced_slot(
        &self,
        entity: Entity,
        manager: &SquadManager,
        proposal: &mut ProducedAssignmentProposal,
        batch: &mut HashMap<usize, usize>,
    ) -> Option<ProducedRecordReceipt> {
        let index = self.records.iter().position(|record| {
            record.entity == Some(entity)
                && record.status == CampaignProductionStatus::Produced
                && record.forming_slot.is_some()
        })?;
        let record = &self.records[index];
        // slotはrecordと同じ寿命を持つ。debug時に発番漏れを検知できるよう利用する。
        debug_assert!(
            record
                .forming_slot
                .is_some_and(|slot| slot.0 < self.next_forming_slot)
        );
        let mission = match record.role {
            CampaignProductionRole::Transport => MissionType::Transport,
            CampaignProductionRole::Capture => MissionType::Capture,
            CampaignProductionRole::Combat => MissionType::Attack,
        };
        let squad = if let Some(join) = batch.get(&index) {
            ProducedSquad::Proposed(*join)
        } else if let Some(squad_id) = record.squad_id.filter(|squad_id| {
            manager
                .squads
                .iter()
                .any(|squad| squad.id == *squad_id && squad.owner_id == Some(record.player_id))
        }) {
            ProducedSquad::Existing(squad_id)
        } else {
            ProducedSquad::Create(mission.clone())
        };
        let join = proposal.propose(ProducedJoin {
            player: record.player_id,
            entity,
            squad,
            fallback_mission: mission,
            target: record.mission_target,
            target_island: Some(record.island_id),
        });
        batch.insert(index, join);
        Some(ProducedRecordReceipt {
            record: index,
            join,
        })
    }

    /// テスト用: 1体のslotを提案→確定→書戻しまで行う。
    #[cfg(test)]
    fn resolve_produced_slot(&mut self, entity: Entity, manager: &mut SquadManager) {
        let mut proposal = ProducedAssignmentProposal::default();
        let receipt =
            self.propose_produced_slot(entity, manager, &mut proposal, &mut HashMap::default());
        let delta = OperationLedger::commit_produced(manager, proposal);
        if let Some(receipt) = receipt {
            self.records[receipt.record].squad_id = Some(delta.squad_for(receipt.join));
        }
    }
}

/// campaign生産recordの所属提案と、確定後に書き戻すrecord位置。
#[derive(Debug)]
pub(super) struct ProducedRecordReceipt {
    record: usize,
    join: usize,
}

/// 生産完了Eventを島作戦の発注意図へ照合し、受入slotの所属を提案する。
pub(super) fn propose_produced_events(
    events: &[&UnitProducedEvent],
    turn: u32,
    registry: &mut V4CampaignExecutionRegistry,
    manager: Option<&SquadManager>,
    proposal: &mut ProducedAssignmentProposal,
) -> Vec<ProducedRecordReceipt> {
    let mut receipts = Vec::new();
    let mut batch = HashMap::default();
    for event in events {
        registry.assign_produced(event, turn);
        // 予約のない通常発注はpropose側で除外する。両経路が混在しても他方の
        // プレイヤーを即時編成せず、同じEventの再受信でも既存Squadを再利用する。
        if let Some(manager) = manager
            && let Some(receipt) =
                registry.propose_produced_slot(event.entity, manager, proposal, &mut batch)
        {
            receipts.push(receipt);
        }
    }
    receipts
}

/// 所属確定後の受入SquadIdを、提案順にcampaign生産recordへ書き戻す。
pub(super) fn bind_produced_squads(
    registry: &mut V4CampaignExecutionRegistry,
    receipts: &[ProducedRecordReceipt],
    delta: &ProducedAssignmentDelta,
) {
    for receipt in receipts {
        registry.records[receipt.record].squad_id = Some(delta.squad_for(receipt.join));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生産直後にSquadを作るのは拡張経路だけ。通常経路は次回再編へ任せる。
    #[test]
    fn production_system_keeps_each_paths_squad_resolution_behavior() {
        for (path, resolves_squad) in [
            (crate::ai::code_path::CodePath::MainCompatible, false),
            (crate::ai::code_path::CodePath::Extended, true),
        ] {
            let _guard = crate::ai::code_path::enter(path);
            let player = PlayerId(1);
            let entity = Entity::from_raw(42);
            let mut registry = V4CampaignExecutionRegistry::default();
            // 両経路とも同じ発注APIを使い、登録された予約の有無で受入を区別する。
            let intent = CampaignProductionIntent {
                command: ProduceUnitCommand {
                    player_id: player,
                    target_x: 4,
                    target_y: 5,
                    unit_type: UnitType::Infantry,
                },
                island_id: IslandId(3),
                role: CampaignProductionRole::Capture,
                mission_target: Some(GridPosition { x: 8, y: 2 }),
            };
            let reservation = if resolves_squad {
                CampaignFormingReservation::Immediate
            } else {
                CampaignFormingReservation::Deferred
            };
            registry.replace_turn_intents(player, 2, &[intent], reservation);
            let mut world = World::new();
            world.insert_resource(registry);
            world.insert_resource(SquadManager::default());
            world.insert_resource(crate::resources::MatchState {
                current_turn_number: crate::resources::TurnNumber(2),
                ..Default::default()
            });
            world.init_resource::<Events<UnitProducedEvent>>();
            world.send_event(UnitProducedEvent {
                player_id: player,
                target_x: 4,
                target_y: 5,
                unit_type: UnitType::Infantry,
                entity,
            });
            let mut schedule = Schedule::default();
            schedule
                .add_systems(crate::ai::v4::production_events::reconcile_production_events_system);
            schedule.run(&mut world);
            let record = &world.resource::<V4CampaignExecutionRegistry>().records[0];
            assert_eq!(record.status, CampaignProductionStatus::Produced);
            assert_eq!(record.entity, Some(entity));
            assert_eq!(record.squad_id.is_some(), resolves_squad, "{path:?}");
            assert_eq!(
                world.resource::<SquadManager>().squads.len(),
                usize::from(resolves_squad)
            );
        }
    }

    /// 同じ施設・兵種を使う受入テスト用の発注。島の違いで発注意図の接続先を区別する。
    fn capture_intent(player: PlayerId, island: IslandId) -> CampaignProductionIntent {
        CampaignProductionIntent {
            command: ProduceUnitCommand {
                player_id: player,
                target_x: 4,
                target_y: 5,
                unit_type: UnitType::Infantry,
            },
            island_id: island,
            role: CampaignProductionRole::Capture,
            mission_target: None,
        }
    }

    fn produced_capture(player: PlayerId, entity: Entity) -> UnitProducedEvent {
        UnitProducedEvent {
            player_id: player,
            target_x: 4,
            target_y: 5,
            unit_type: UnitType::Infantry,
            entity,
        }
    }

    /// 同一対局の通常・拡張プレイヤーの発注は、イベント処理時の経路に関係なく
    /// 個々の予約有無で処理する。同じイベントを再受信してもSquadを二重生成しない。
    #[test]
    fn mixed_players_resolve_only_reserved_campaign_orders_once() {
        let main_player = PlayerId(1);
        let extended_player = PlayerId(2);
        let mut registry = V4CampaignExecutionRegistry::default();
        for (player, reservation) in [
            (main_player, CampaignFormingReservation::Deferred),
            (extended_player, CampaignFormingReservation::Immediate),
        ] {
            registry.replace_turn_intents(
                player,
                2,
                &[capture_intent(player, IslandId(3))],
                reservation,
            );
        }
        assert!(registry.records[0].forming_slot.is_none());
        assert!(registry.records[1].forming_slot.is_some());
        let main_entity = Entity::from_raw(41);
        let extended_entity = Entity::from_raw(42);
        let mut world = World::new();
        world.insert_resource(registry);
        world.insert_resource(SquadManager::default());
        world.insert_resource(crate::resources::MatchState {
            current_turn_number: crate::resources::TurnNumber(2),
            ..Default::default()
        });
        world.init_resource::<Events<UnitProducedEvent>>();
        let mut schedule = Schedule::default();
        schedule.add_systems(crate::ai::v4::production_events::reconcile_production_events_system);
        // Systemは経路やCodePathRegistryを参照しないことも確認する。
        let _guard = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
        for _ in 0..2 {
            world.send_event(produced_capture(main_player, main_entity));
            world.send_event(produced_capture(extended_player, extended_entity));
            schedule.run(&mut world);
            let registry = world.resource::<V4CampaignExecutionRegistry>();
            assert_eq!(registry.records[0].entity, Some(main_entity));
            assert!(registry.records[0].squad_id.is_none());
            let squad_id = registry.records[1]
                .squad_id
                .expect("予約済み発注だけを編成");
            let manager = world.resource::<SquadManager>();
            assert_eq!(manager.squads.len(), 1);
            let squad = &manager.squads[0];
            assert_eq!(squad.id, squad_id);
            assert_eq!(squad.owner_id, Some(extended_player));
            assert_eq!(squad.mission_type, MissionType::Capture);
            assert_eq!(squad.target_island, Some(IslandId(3)));
            assert!(squad.members.contains(&extended_entity));
            assert!(!squad.members.contains(&main_entity));
        }
    }

    /// A取消後のB完成はBの島へ接続し、完成済みBの予約は再計画でも保持する。
    #[test]
    fn campaign_replacement_discards_unproduced_slots_but_keeps_produced_slots() {
        let _guard = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let mut registry = V4CampaignExecutionRegistry::default();
        registry.replace_turn_intents(
            player,
            2,
            &[capture_intent(player, IslandId(3))],
            CampaignFormingReservation::Immediate,
        );
        let cancelled_slot = registry.records[0].forming_slot.expect("Aの予約");
        registry.replace_turn_intents(
            player,
            2,
            &[capture_intent(player, IslandId(4))],
            CampaignFormingReservation::Immediate,
        );
        assert_eq!(registry.records.len(), 1);
        let completed_slot = registry.records[0].forming_slot.expect("Bの予約");
        assert_ne!(cancelled_slot, completed_slot);
        let entity = Entity::from_raw(42);
        registry.assign_produced(&produced_capture(player, entity), 2);
        let mut manager = SquadManager::default();
        registry.resolve_produced_slot(entity, &mut manager);
        assert_eq!(manager.squads[0].target_island, Some(IslandId(4)));
        let squad_id = registry.records[0].squad_id;

        // 同手番の再発注Cは別slotとなり、完成済みBのEntity・所属を変更しない。
        registry.replace_turn_intents(
            player,
            2,
            &[capture_intent(player, IslandId(5))],
            CampaignFormingReservation::Immediate,
        );
        assert_eq!(registry.records.len(), 2);
        assert!(
            registry
                .records
                .iter()
                .all(|record| record.forming_slot != Some(cancelled_slot))
        );
        let completed = registry
            .records
            .iter()
            .find(|record| record.entity == Some(entity))
            .unwrap();
        assert_eq!(completed.forming_slot, Some(completed_slot));
        assert_eq!(completed.squad_id, squad_id);
        assert_eq!(completed.status, CampaignProductionStatus::Produced);
        assert_ne!(registry.records[1].forming_slot, Some(completed_slot));
    }

    /// 再計画で同じ施設・兵種へ後続発注が入っても、古い完成Eventを再照合しない。
    /// 両経路に共通の不具合修正（C）：完成Entityの二重計上と後続発注の消失を防ぐ。
    #[test]
    fn duplicate_production_event_does_not_consume_a_followup_order() {
        for path in [
            crate::ai::code_path::CodePath::MainCompatible,
            crate::ai::code_path::CodePath::Extended,
        ] {
            let _guard = crate::ai::code_path::enter(path);
            let player = PlayerId(1);
            let reservation = match path {
                crate::ai::code_path::CodePath::MainCompatible => {
                    CampaignFormingReservation::Deferred
                }
                crate::ai::code_path::CodePath::Extended => CampaignFormingReservation::Immediate,
            };
            let mut registry = V4CampaignExecutionRegistry::default();
            registry.replace_turn_intents(
                player,
                2,
                &[capture_intent(player, IslandId(3))],
                reservation,
            );
            let entity = Entity::from_raw(42);
            registry.assign_produced(&produced_capture(player, entity), 2);
            registry.replace_turn_intents(
                player,
                2,
                &[capture_intent(player, IslandId(4))],
                reservation,
            );
            registry.assign_produced(&produced_capture(player, entity), 2);

            assert_eq!(registry.records.len(), 2, "{path:?}");
            assert_eq!(
                registry.records[0].status,
                CampaignProductionStatus::Produced
            );
            assert_eq!(registry.records[0].entity, Some(entity));
            assert_eq!(
                registry.records[1].status,
                CampaignProductionStatus::Planned
            );
            assert_eq!(registry.records[1].entity, None);
            // 本当の後続完成Eventは引き続き照合できる。
            let next_entity = Entity::from_raw(43);
            registry.assign_produced(&produced_capture(player, next_entity), 2);
            assert_eq!(
                registry.records[1].status,
                CampaignProductionStatus::Produced
            );
            assert_eq!(registry.records[1].entity, Some(next_entity));
        }
    }

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
                mission_target: None,
                command: command.clone(),
                island_id: island,
                role: CampaignProductionRole::Transport,
            }],
            CampaignFormingReservation::Deferred,
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
                mission_target: None,
                command,
                island_id: island,
                role: CampaignProductionRole::Capture,
            }],
            CampaignFormingReservation::Deferred,
        );
        registry.replace_turn_intents(player, 3, &[], CampaignFormingReservation::Deferred);

        assert_eq!(
            registry.records_for(player, island)[0].status,
            CampaignProductionStatus::Delayed
        );
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
            CampaignFormingReservation::Deferred,
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
            CampaignFormingReservation::Deferred,
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
}
