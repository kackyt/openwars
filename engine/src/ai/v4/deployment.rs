//! V4の生産意図を、生産された実Entityの局地任務まで引き継ぐ台帳。
//!
//! 生産判断とSquad再計画は別フレーム・別ターンで実行されるため、診断traceだけでは
//! 「何のために買ったか」が失われる。このモジュールは発注を生産完了イベントへ照合し、
//! 優先敵が有効な間だけ汎用目標探索から保護する。

use crate::ai::islands::IslandMap;
use crate::ai::planning::assignment::{
    OperationLedger, ProducedAssignmentDelta, ProducedAssignmentProposal, ProducedJoin,
    ProducedSquad,
};
use crate::ai::squad::{MissionPhase, MissionType, SquadId, SquadManager};
use crate::ai::turn_distance::TerrainConnectivity;
use crate::ai::v4::operation::SlotKind;
use crate::ai::v4::plan_contract::OperationBinding;
use crate::ai::v4::plan_revision::{ActiveDeploymentIntent, PlanId, PlanStepRef};
use crate::collections::{HashMap, HashSet};
use crate::components::{
    Ammo, Faction, GridPosition, Health, PlayerId, Property, Transporting, UnitStats,
};
use crate::events::{UnitAttackedEvent, UnitProducedEvent};
use crate::resources::{DamageChart, Map, MatchState, UnitType, master_data::MasterDataRegistry};
use bevy_ecs::event::EventReader;
use bevy_ecs::prelude::*;
use std::collections::VecDeque;
/// 生産済み戦力を作戦地点へ投入するか、編成完了まで集結させるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeploymentPosture {
    Execute,
    Forming,
}

/// 生産時の混成パッケージ予測。実績auditと同じEntityへ保持する。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeploymentForecast {
    pub first_attack_turn: Option<u32>,
    pub elimination_turn: Option<u32>,
    pub occupation_turn: Option<u32>,
    pub package_cost: u32,
    pub package_size: u32,
}

#[derive(Debug, Clone, Copy, Default)]
struct DeploymentAttackAudit {
    defender_can_capture: bool,
    defender_is_transport: bool,
    destroyed: bool,
    damage_value_dealt: u32,
    counter_value_received: u32,
    destroyed_value: u32,
}

/// 生産命令と作戦意図を照合するための発注情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingDeployment {
    pub player_id: PlayerId,
    pub turn: u32,
    pub order: u32,
    pub facility: GridPosition,
    pub unit_type: UnitType,
    pub anchor: GridPosition,
    pub staging_anchor: GridPosition,
    pub posture: DeploymentPosture,
    pub slot_kind: SlotKind,
    pub priority_enemies: Vec<Entity>,
    pub threat_horizon: u32,
    pub forecast: DeploymentForecast,
    pub plan_step: Option<PlanStepRef>,
    /// 発注と同時に予約した作戦Squadの受入slot。手書きのテスト入力ではNoneを許容し、
    /// `replace_turn_orders` が必ず実IDへ正規化する。
    pub forming_slot: Option<FormingSquadSlotId>,
    /// V4中央契約との軽量バインディング。
    pub operation_binding: Option<OperationBinding>,
}

/// 生産済みEntityへ結び付いた作戦意図。
#[derive(Debug, Clone)]
struct AssignedDeployment {
    entity: Entity,
    intent: PendingDeployment,
    squad_id: Option<SquadId>,
    current_target: Option<Entity>,
    active: bool,
    assigned_turn: u32,
    attack_count: u32,
    priority_attack_count: u32,
    mission_target_attack_count: u32,
    mission_target_kill_count: u32,
    mission_target_damage_value_dealt: u32,
    mission_target_counter_value_received: u32,
    mission_target_destroyed_value: u32,
    mission_target_first_attack_turn: Option<u32>,
    capture_unit_attack_count: u32,
    transport_unit_attack_count: u32,
    kill_count: u32,
    damage_value_dealt: u32,
    counter_value_received: u32,
    destroyed_value: u32,
    first_attack_turn: Option<u32>,
}

/// E2E評価へ公開する、撃破枠購入Entityの任務・攻撃実績。
#[derive(Debug, Clone)]
pub struct DeploymentAuditRecord {
    pub entity: Entity,
    pub unit_type: UnitType,
    pub slot_kind: SlotKind,
    pub anchor: GridPosition,
    pub priority_enemies: Vec<Entity>,
    pub squad_id: Option<SquadId>,
    pub current_target: Option<Entity>,
    pub active: bool,
    pub assigned_turn: u32,
    pub attack_count: u32,
    pub priority_attack_count: u32,
    pub mission_target_attack_count: u32,
    pub mission_target_kill_count: u32,
    pub mission_target_damage_value_dealt: u32,
    pub mission_target_counter_value_received: u32,
    pub mission_target_destroyed_value: u32,
    pub mission_target_first_attack_turn: Option<u32>,
    pub capture_unit_attack_count: u32,
    pub transport_unit_attack_count: u32,
    pub kill_count: u32,
    pub damage_value_dealt: u32,
    pub counter_value_received: u32,
    pub destroyed_value: u32,
    pub first_attack_turn: Option<u32>,
    pub forecast: DeploymentForecast,
    pub plan_step: Option<PlanStepRef>,
}

/// PlanId単位の予実へ戻す、当該生産Entityの全戦闘実績。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PlanCombatActuals {
    pub first_attack_turn: Option<u32>,
    pub attack_count: u32,
    pub priority_attack_count: u32,
    pub kill_count: u32,
    pub damage_value_dealt: u32,
    pub counter_value_received: u32,
    pub destroyed_value: u32,
}

impl DeploymentAuditRecord {
    /// 現在標的だけでなく、同一作戦圏内で切り替えた標的への攻撃も予実へ含める。
    pub(crate) fn plan_combat_actuals(&self) -> PlanCombatActuals {
        PlanCombatActuals {
            first_attack_turn: self.first_attack_turn,
            attack_count: self.attack_count,
            priority_attack_count: self.priority_attack_count,
            kill_count: self.kill_count,
            damage_value_dealt: self.damage_value_dealt,
            counter_value_received: self.counter_value_received,
            destroyed_value: self.destroyed_value,
        }
    }
}

/// Combat見積へ渡す、既存EntityのPlan所有権と現在の対象集合。
#[derive(Debug, Clone)]
pub(crate) struct ActiveTargetAssignment {
    pub plan_id: Option<PlanId>,
    /// Plan購入列由来か、Must後の即時Combat増援かを区別する。
    /// `None` のPlanIdだけでは旧Intercept等の別任務を混ぜてしまうため、既存戦力
    /// として再見積してよいのはCombat任務に限る。
    pub slot_kind: SlotKind,
    pub targets: HashSet<Entity>,
}

/// 生産命令時点で確保する、実Entityをまだ持たないSquad受入先の値オブジェクト。
///
/// Squad IDそのものはSquadManagerだけが発番する。ここでは発注の意図を安定IDで
/// 予約し、生産完了Eventで実Squadへ一度だけ解決する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FormingSquadSlotId(u64);

/// まだ実体が無いSquad slotの予約状態。発注と生産完了のあいだも作戦所有権を保つ。
#[derive(Debug, Clone)]
struct FormingSquadReservation {
    id: FormingSquadSlotId,
    player_id: PlayerId,
    turn: u32,
    squad_id: Option<SquadId>,
    entity: Option<Entity>,
}

/// V4の未照合発注と、生産済みEntityの局地任務を保持するリソース。
#[derive(Resource, Debug, Default)]
pub struct V4DeploymentRegistry {
    pending: VecDeque<PendingDeployment>,
    assigned: HashMap<Entity, AssignedDeployment>,
    forming_slots: HashMap<FormingSquadSlotId, FormingSquadReservation>,
    next_forming_slot: u64,
}

impl V4DeploymentRegistry {
    /// 同じ手番に再計画した古い発注を除き、新しい発注意図を順番どおり登録する。
    pub(crate) fn replace_turn_orders(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        orders: impl IntoIterator<Item = PendingDeployment>,
    ) {
        self.pending
            .retain(|pending| pending.player_id != player_id || pending.turn != turn);
        // 編成slotの予約は拡張経路だけが使う。通常経路はslotなしで発注をそのまま登録する。
        if !crate::ai::code_path::is_extended_at("v4/deployment.rs::replace_turn_orders") {
            self.pending.extend(orders);
            return;
        }
        // 同一手番の再計画で未発行slotを残すと、取り消された生産先へ後続Eventを
        // 誤接続する。その手番の予約はPendingと同じ粒度で置換する。
        self.forming_slots.retain(|_, reservation| {
            reservation.player_id != player_id
                || reservation.turn != turn
                || reservation.entity.is_some()
        });
        let mut normalized = orders.into_iter().collect::<Vec<_>>();
        normalized.sort_unstable_by_key(|pending| pending.order);
        for pending in &mut normalized {
            let slot = pending.forming_slot.unwrap_or_else(|| {
                let id = FormingSquadSlotId(self.next_forming_slot);
                self.next_forming_slot = self.next_forming_slot.saturating_add(1);
                id
            });
            pending.forming_slot = Some(slot);
            self.forming_slots
                .entry(slot)
                .or_insert(FormingSquadReservation {
                    id: slot,
                    player_id: pending.player_id,
                    turn: pending.turn,
                    squad_id: None,
                    entity: None,
                });
        }
        self.pending.extend(normalized);
    }

    /// 生産完了イベントを、同じ手番・施設・兵種の最初の発注へ決定的に照合する。
    /// 生産完了Eventを、同じ手番・施設・兵種の最初の発注へ決定的に照合する。
    ///
    /// 予約slotを持つ発注（拡張経路で登録）は、そのslotへ完成Entityを記録して返す。
    /// slotなしの発注は従来どおり台帳へ移すだけで `None` を返す。
    fn assign_produced(
        &mut self,
        event: &UnitProducedEvent,
        turn: u32,
    ) -> Option<FormingSquadSlotId> {
        let index = self.pending.iter().position(|pending| {
            pending.player_id == event.player_id
                && pending.turn == turn
                && pending.facility.x == event.target_x
                && pending.facility.y == event.target_y
                && pending.unit_type == event.unit_type
        })?;
        let intent = self
            .pending
            .remove(index)
            .expect("照合済みpending deploymentが存在する");
        let forming_slot = intent.forming_slot;
        self.assigned.insert(
            event.entity,
            AssignedDeployment {
                entity: event.entity,
                intent,
                squad_id: None,
                current_target: None,
                active: true,
                assigned_turn: turn,
                attack_count: 0,
                priority_attack_count: 0,
                mission_target_attack_count: 0,
                mission_target_kill_count: 0,
                mission_target_damage_value_dealt: 0,
                mission_target_counter_value_received: 0,
                mission_target_destroyed_value: 0,
                mission_target_first_attack_turn: None,
                capture_unit_attack_count: 0,
                transport_unit_attack_count: 0,
                kill_count: 0,
                damage_value_dealt: 0,
                counter_value_received: 0,
                destroyed_value: 0,
                first_attack_turn: None,
            },
        );
        if let Some(slot) = forming_slot
            && let Some(reservation) = self.forming_slots.get_mut(&slot)
        {
            reservation.entity = Some(event.entity);
        }
        forming_slot
    }

    /// 生産完了直後の予約slotを、受入Squadへの所属提案にする（SquadManagerは変更しない）。
    ///
    /// 実Entityは生成手番に行動できないが、次の再編まで`None`所属にしない。対象敵の
    /// 再探索など戦術的な詳細は次手番の`prepare_deployment_squads`が更新する。
    /// 同じ受付で同じslotが先に提案済みなら、そのSquad（確定前）へ合流させる。
    fn propose_produced_slot(
        &self,
        entity: Entity,
        manager: &SquadManager,
        island_map: Option<&IslandMap>,
        proposal: &mut ProducedAssignmentProposal,
        batch: &mut HashMap<FormingSquadSlotId, usize>,
    ) -> Option<ProducedSlotReceipt> {
        let snapshot = self.assigned.get(&entity)?;
        let slot = snapshot.intent.forming_slot?;
        let player = snapshot.intent.player_id;
        let mission = match snapshot.intent.posture {
            DeploymentPosture::Execute => MissionType::Attack,
            DeploymentPosture::Forming => MissionType::Defense,
        };
        let squad = if let Some(index) = batch.get(&slot) {
            ProducedSquad::Proposed(*index)
        } else if let Some(squad_id) = self
            .forming_slots
            .get(&slot)
            .and_then(|reservation| reservation.squad_id)
            .filter(|squad_id| {
                manager
                    .squads
                    .iter()
                    .any(|squad| squad.id == *squad_id && squad.owner_id == Some(player))
            })
        {
            ProducedSquad::Existing(squad_id)
        } else {
            ProducedSquad::Create(mission.clone())
        };
        let join = proposal.propose(ProducedJoin {
            player,
            entity,
            squad,
            fallback_mission: mission,
            target: Some(match snapshot.intent.posture {
                DeploymentPosture::Execute => snapshot.intent.anchor,
                DeploymentPosture::Forming => snapshot.intent.staging_anchor,
            }),
            target_island: island_map
                .and_then(|islands| islands.get_island_at(&snapshot.intent.anchor))
                .map(|island| island.id),
        });
        batch.insert(slot, join);
        Some(ProducedSlotReceipt { slot, entity, join })
    }

    /// 所属確定後に、slot予約と配備台帳へ受入SquadIdを記録する。
    fn bind_produced_squad(&mut self, receipt: &ProducedSlotReceipt, squad_id: SquadId) {
        if let Some(reservation) = self.forming_slots.get_mut(&receipt.slot) {
            debug_assert_eq!(reservation.id, receipt.slot);
            reservation.squad_id = Some(squad_id);
        }
        if let Some(deployment) = self.assigned.get_mut(&receipt.entity) {
            deployment.squad_id = Some(squad_id);
        }
    }

    /// テスト用: 1体のslotを提案→確定→書戻しまで行う。
    #[cfg(test)]
    fn resolve_produced_slot(
        &mut self,
        entity: Entity,
        manager: &mut SquadManager,
        island_map: Option<&IslandMap>,
    ) {
        let mut proposal = ProducedAssignmentProposal::default();
        let receipt = self.propose_produced_slot(
            entity,
            manager,
            island_map,
            &mut proposal,
            &mut HashMap::default(),
        );
        let delta = OperationLedger::commit_produced(manager, proposal);
        if let Some(receipt) = receipt {
            self.bind_produced_squad(&receipt, delta.squad_for(receipt.join));
        }
    }

    /// 現在playerの局地任務としてbeam searchから保護するSquad IDを返す。
    pub(crate) fn protected_squads(&self, player_id: PlayerId) -> HashSet<SquadId> {
        self.assigned
            .values()
            .filter(|deployment| deployment.intent.player_id == player_id && deployment.active)
            .filter_map(|deployment| deployment.squad_id)
            .collect()
    }

    /// 島嶼キャンペーンや汎用部隊へ再予約させない、作戦遂行中のEntityを返す。
    pub(crate) fn active_entities(&self, player_id: PlayerId) -> HashSet<Entity> {
        self.assigned
            .values()
            .filter(|deployment| deployment.intent.player_id == player_id && deployment.active)
            .map(|deployment| deployment.entity)
            .collect()
    }

    /// Combat見積で既存戦力として数えてよいEntityと、その実任務の対象を返す。
    ///
    /// Entityだけを返すと、1機を複数の島作戦へ同時に計上できてしまう。生産時の
    /// 優先敵と現在の再目標を併せて返し、対象敵が属する1作戦だけへ参加させる。
    pub(crate) fn active_target_assignments(
        &self,
        player_id: PlayerId,
    ) -> HashMap<Entity, ActiveTargetAssignment> {
        self.assigned
            .values()
            .filter(|deployment| deployment.intent.player_id == player_id && deployment.active)
            .map(|deployment| {
                let mut targets = deployment
                    .intent
                    .priority_enemies
                    .iter()
                    .copied()
                    .collect::<HashSet<_>>();
                targets.extend(deployment.current_target);
                (
                    deployment.entity,
                    ActiveTargetAssignment {
                        slot_kind: deployment.intent.slot_kind,
                        plan_id: deployment.intent.plan_step.map(|step| step.plan_id),
                        targets,
                    },
                )
            })
            .collect()
    }

    /// playerごとのdeployment実績をEntity順で返す。
    pub fn audit_records(&self, player_id: PlayerId) -> Vec<DeploymentAuditRecord> {
        let mut records = self
            .assigned
            .values()
            .filter(|deployment| deployment.intent.player_id == player_id)
            .map(|deployment| DeploymentAuditRecord {
                entity: deployment.entity,
                unit_type: deployment.intent.unit_type,
                slot_kind: deployment.intent.slot_kind,
                anchor: deployment.intent.anchor,
                priority_enemies: deployment.intent.priority_enemies.clone(),
                squad_id: deployment.squad_id,
                current_target: deployment.current_target,
                active: deployment.active,
                assigned_turn: deployment.assigned_turn,
                attack_count: deployment.attack_count,
                priority_attack_count: deployment.priority_attack_count,
                mission_target_attack_count: deployment.mission_target_attack_count,
                mission_target_kill_count: deployment.mission_target_kill_count,
                mission_target_damage_value_dealt: deployment.mission_target_damage_value_dealt,
                mission_target_counter_value_received: deployment
                    .mission_target_counter_value_received,
                mission_target_destroyed_value: deployment.mission_target_destroyed_value,
                mission_target_first_attack_turn: deployment.mission_target_first_attack_turn,
                capture_unit_attack_count: deployment.capture_unit_attack_count,
                transport_unit_attack_count: deployment.transport_unit_attack_count,
                kill_count: deployment.kill_count,
                damage_value_dealt: deployment.damage_value_dealt,
                counter_value_received: deployment.counter_value_received,
                destroyed_value: deployment.destroyed_value,
                first_attack_turn: deployment.first_attack_turn,
                forecast: deployment.intent.forecast,
                plan_step: deployment.intent.plan_step,
            })
            .collect::<Vec<_>>();
        records.sort_unstable_by_key(|record| record.entity.to_bits());
        records
    }

    /// 生産完了イベントと照合できていない発注数を返す。
    pub fn pending_count(&self, player_id: PlayerId) -> usize {
        self.pending
            .iter()
            .filter(|pending| pending.player_id == player_id)
            .count()
    }

    /// 実Entityへ照合できた永続計画stepを返す。次手番の予実突合に使用する。
    pub(crate) fn produced_plan_steps(&self, player_id: PlayerId) -> HashSet<PlanStepRef> {
        self.assigned
            .values()
            .filter(|deployment| deployment.intent.player_id == player_id)
            .filter_map(|deployment| deployment.intent.plan_step)
            .collect()
    }

    /// 完了・撤回された計画の局地任務保護を解除し、上位防衛や汎用任務へ返す。
    pub(crate) fn release_closed_plans(
        &mut self,
        plan_ids: &HashSet<super::plan_revision::PlanId>,
    ) {
        if plan_ids.is_empty() {
            return;
        }
        self.pending.retain(|pending| {
            pending
                .plan_step
                .is_none_or(|step| !plan_ids.contains(&step.plan_id))
        });
        for deployment in self.assigned.values_mut() {
            if deployment
                .intent
                .plan_step
                .is_some_and(|step| plan_ids.contains(&step.plan_id))
            {
                deployment.active = false;
                deployment.current_target = None;
            }
        }
    }

    /// 現行Planの増援・移動済み敵を、生産済みEntityと未照合発注の双方へ反映する。
    pub(crate) fn refresh_plan_intents(&mut self, intents: &[ActiveDeploymentIntent]) {
        let by_plan = intents
            .iter()
            .map(|intent| (intent.plan_id, intent))
            .collect::<HashMap<_, _>>();
        for pending in &mut self.pending {
            let Some(step) = pending.plan_step else {
                continue;
            };
            let Some(intent) = by_plan.get(&step.plan_id) else {
                continue;
            };
            pending.anchor = intent.anchor;
            pending.staging_anchor = intent.staging_anchor;
            pending.posture = intent.posture;
            pending.priority_enemies = intent.priority_enemies.clone();
            pending.threat_horizon = intent.threat_horizon;
        }
        for deployment in self.assigned.values_mut() {
            let Some(step) = deployment.intent.plan_step else {
                continue;
            };
            let Some(intent) = by_plan.get(&step.plan_id) else {
                continue;
            };
            deployment.intent.anchor = intent.anchor;
            deployment.intent.staging_anchor = intent.staging_anchor;
            deployment.intent.posture = intent.posture;
            deployment.intent.priority_enemies = intent.priority_enemies.clone();
            deployment.intent.threat_horizon = intent.threat_horizon;
            if deployment
                .current_target
                .is_some_and(|target| !intent.priority_enemies.contains(&target))
            {
                deployment.current_target = None;
            }
            // 一時的に敵が消えて待機へ移ったEntityも、増援追加時には同じPlanへ復帰させる。
            deployment.active = true;
        }
    }

    /// 行動評価で最優先する、現在有効な局地任務の敵Entityを返す。
    pub(crate) fn attack_target(&self, attacker: Entity) -> Option<Entity> {
        self.assigned
            .get(&attacker)
            .filter(|deployment| deployment.active)
            .and_then(|deployment| deployment.current_target)
    }

    fn record_attack(
        &mut self,
        attacker: Entity,
        defender: Entity,
        turn: u32,
        audit: DeploymentAttackAudit,
    ) {
        let Some(deployment) = self.assigned.get_mut(&attacker) else {
            return;
        };
        deployment.attack_count = deployment.attack_count.saturating_add(1);
        deployment.first_attack_turn.get_or_insert(turn);
        if deployment.current_target == Some(defender)
            || deployment.intent.priority_enemies.contains(&defender)
        {
            deployment.mission_target_attack_count =
                deployment.mission_target_attack_count.saturating_add(1);
            deployment
                .mission_target_first_attack_turn
                .get_or_insert(turn);
            deployment.mission_target_damage_value_dealt = deployment
                .mission_target_damage_value_dealt
                .saturating_add(audit.damage_value_dealt);
            deployment.mission_target_counter_value_received = deployment
                .mission_target_counter_value_received
                .saturating_add(audit.counter_value_received);
            deployment.mission_target_destroyed_value = deployment
                .mission_target_destroyed_value
                .saturating_add(audit.destroyed_value);
            if audit.destroyed {
                deployment.mission_target_kill_count =
                    deployment.mission_target_kill_count.saturating_add(1);
            }
        }
        if audit.defender_can_capture {
            deployment.capture_unit_attack_count =
                deployment.capture_unit_attack_count.saturating_add(1);
        }
        if audit.defender_is_transport {
            deployment.transport_unit_attack_count =
                deployment.transport_unit_attack_count.saturating_add(1);
        }
        if audit.destroyed {
            deployment.kill_count = deployment.kill_count.saturating_add(1);
        }
        deployment.damage_value_dealt = deployment
            .damage_value_dealt
            .saturating_add(audit.damage_value_dealt);
        deployment.counter_value_received = deployment
            .counter_value_received
            .saturating_add(audit.counter_value_received);
        deployment.destroyed_value = deployment
            .destroyed_value
            .saturating_add(audit.destroyed_value);
        if deployment.intent.priority_enemies.contains(&defender) {
            deployment.priority_attack_count = deployment.priority_attack_count.saturating_add(1);
        }
    }

    #[cfg(test)]
    pub(crate) fn assign_target_for_test(
        &mut self,
        player_id: PlayerId,
        attacker: Entity,
        target: Entity,
    ) {
        self.assigned.insert(
            attacker,
            AssignedDeployment {
                entity: attacker,
                intent: PendingDeployment {
                    forming_slot: None,
                    operation_binding: None,
                    player_id,
                    turn: 1,
                    order: 0,
                    facility: GridPosition { x: 0, y: 0 },
                    unit_type: UnitType::Fighter,
                    anchor: GridPosition { x: 0, y: 0 },
                    staging_anchor: GridPosition { x: 0, y: 0 },
                    posture: DeploymentPosture::Execute,
                    slot_kind: SlotKind::Combat,
                    priority_enemies: vec![target],
                    threat_horizon: 1,
                    forecast: DeploymentForecast::default(),
                    plan_step: None,
                },
                squad_id: None,
                current_target: Some(target),
                active: true,
                assigned_turn: 1,
                attack_count: 0,
                priority_attack_count: 0,
                mission_target_attack_count: 0,
                mission_target_kill_count: 0,
                mission_target_damage_value_dealt: 0,
                mission_target_counter_value_received: 0,
                mission_target_destroyed_value: 0,
                mission_target_first_attack_turn: None,
                capture_unit_attack_count: 0,
                transport_unit_attack_count: 0,
                kill_count: 0,
                damage_value_dealt: 0,
                counter_value_received: 0,
                destroyed_value: 0,
                first_attack_turn: None,
            },
        );
    }
}

/// 生産slotの所属提案と、確定後に書き戻す台帳上の位置。
#[derive(Debug)]
pub(super) struct ProducedSlotReceipt {
    slot: FormingSquadSlotId,
    entity: Entity,
    join: usize,
}

/// 実際に生産されたEntityをpending deploymentへ照合し、slotの所属を提案する。
///
/// 予約slotや作戦契約bindingを持つ発注（拡張経路だけが登録する）は、所属提案として
/// 受付へ渡す。slotもbindingもない通常経路の発注は台帳へ移すだけで、従来と同じ。
pub(super) fn propose_produced_events(
    events: &[&UnitProducedEvent],
    turn: u32,
    registry: &mut V4DeploymentRegistry,
    manager: Option<&SquadManager>,
    island_map: Option<&IslandMap>,
    proposal: &mut ProducedAssignmentProposal,
) -> Vec<ProducedSlotReceipt> {
    let mut receipts = Vec::new();
    let mut batch = HashMap::default();
    for event in events {
        let slot = registry.assign_produced(event, turn);
        if slot.is_some()
            && let Some(manager) = manager
            && let Some(receipt) = registry.propose_produced_slot(
                event.entity,
                manager,
                island_map,
                proposal,
                &mut batch,
            )
        {
            receipts.push(receipt);
        }
    }
    // 失敗した生産命令を翌ターンの同型発注へ誤照合しない。
    registry.pending.retain(|pending| pending.turn >= turn);
    receipts
}

/// 所属確定後の受入SquadIdを、提案順に配備台帳へ書き戻す。
pub(super) fn bind_produced_squads(
    registry: &mut V4DeploymentRegistry,
    receipts: &[ProducedSlotReceipt],
    delta: &ProducedAssignmentDelta,
) {
    for receipt in receipts {
        registry.bind_produced_squad(receipt, delta.squad_for(receipt.join));
    }
}

/// deployment Entityの攻撃実績を台帳へ記録し、任務接続の効果をEntity単位で評価可能にする。
pub fn audit_deployment_attacks_system(
    mut events: EventReader<UnitAttackedEvent>,
    match_state: Res<MatchState>,
    units: Query<(&UnitStats, &Health)>,
    mut registry: ResMut<V4DeploymentRegistry>,
) {
    let turn = match_state.current_turn_number.0;
    for event in events.read() {
        let defender = units.get(event.defender).ok();
        let attacker = units.get(event.attacker).ok();
        let damage_value_dealt = defender.map_or(0, |(stats, health)| {
            stats.cost.saturating_mul(
                event
                    .defender_hp_before
                    .saturating_sub(event.defender_hp_after),
            ) / health.max.max(1)
        });
        let counter_value_received = attacker.map_or(0, |(stats, health)| {
            stats.cost.saturating_mul(
                event
                    .attacker_hp_before
                    .saturating_sub(event.attacker_hp_after),
            ) / health.max.max(1)
        });
        let destroyed_value = defender
            .filter(|_| event.defender_hp_after == 0)
            .map_or(0, |(stats, _)| stats.cost);
        registry.record_attack(
            event.attacker,
            event.defender,
            turn,
            DeploymentAttackAudit {
                defender_can_capture: defender.is_some_and(|(stats, _)| stats.can_capture),
                defender_is_transport: defender.is_some_and(|(stats, _)| stats.max_cargo > 0),
                destroyed: event.defender_hp_after == 0,
                damage_value_dealt,
                counter_value_received,
                destroyed_value,
            },
        );
    }
}

/// 生産Entityが対象へ有効打を持ち、地形上自力で交戦地点へ到達できるかを判定する。
fn can_engage(
    world: &World,
    connectivity: &mut TerrainConnectivity,
    attacker: Entity,
    defender: Entity,
) -> bool {
    let (Some(attacker_pos), Some(attacker_stats), Some(attacker_faction)) = (
        world.get::<GridPosition>(attacker),
        world.get::<UnitStats>(attacker),
        world.get::<Faction>(attacker),
    ) else {
        return false;
    };
    let (Some(defender_pos), Some(defender_stats), Some(defender_faction)) = (
        world.get::<GridPosition>(defender),
        world.get::<UnitStats>(defender),
        world.get::<Faction>(defender),
    ) else {
        return false;
    };
    if attacker_faction.0 == defender_faction.0 || world.get::<Transporting>(defender).is_some() {
        return false;
    }

    let Some(chart) = world.get_resource::<DamageChart>() else {
        return false;
    };
    let ammo = world.get::<Ammo>(attacker);
    let primary_available = attacker_stats.max_ammo1 == 0 || ammo.is_none_or(|ammo| ammo.ammo1 > 0);
    let secondary_available =
        attacker_stats.max_ammo2 == 0 || ammo.is_none_or(|ammo| ammo.ammo2 > 0);
    let has_effective_weapon = (primary_available
        && chart
            .get_base_damage(attacker_stats.unit_type, defender_stats.unit_type)
            .unwrap_or(0)
            > 0)
        || (secondary_available
            && chart
                .get_base_damage_secondary(attacker_stats.unit_type, defender_stats.unit_type)
                .unwrap_or(0)
                > 0);
    if !has_effective_weapon {
        return false;
    }

    let (Some(map), Some(registry)) = (
        world.get_resource::<Map>(),
        world.get_resource::<MasterDataRegistry>(),
    ) else {
        return false;
    };
    connectivity.is_reachable(
        map,
        registry,
        (attacker_pos.x, attacker_pos.y),
        (defender_pos.x, defender_pos.y),
        attacker_stats.movement_type,
    )
}

/// 優先敵が無効になった場合に、同じ作戦期限内の局地敵を戦略カテゴリ順で選ぶ。
fn local_retarget(
    world: &mut World,
    connectivity: &mut TerrainConnectivity,
    deployment: &AssignedDeployment,
) -> Option<Entity> {
    let mut enemies = world.query::<(
        Entity,
        &Faction,
        &GridPosition,
        &UnitStats,
        Option<&Transporting>,
    )>();
    let map = world.get_resource::<Map>()?;
    let master_data = world.get_resource::<MasterDataRegistry>()?;
    let island_map = world
        .get_resource::<IslandMap>()
        .cloned()
        .unwrap_or_else(|| IslandMap::analyze(map));
    let operation_island = island_map
        .get_island_at(&deployment.intent.anchor)
        .map(|island| island.id);
    let mut candidates = Vec::new();
    for (entity, faction, position, stats, transporting) in enemies.iter(world) {
        if faction.0 == deployment.intent.player_id || transporting.is_some() {
            continue;
        }
        // 局地fallbackは作戦対象島の敵だけを選ぶ。別島の敵を追って作戦Entityが
        // 前線を離れると、島作戦の実行契約そのものが崩れる。海上・航空中の敵は
        // 島に属さないため、従来通りETAと到達可能性で局地性を判定する。
        let enemy_island = island_map.get_island_at(position).map(|island| island.id);
        if operation_island.is_some() && enemy_island.is_some() && enemy_island != operation_island
        {
            continue;
        }
        // 島IDを持たない海空目標は敵側ETAだけで局地扱いしない。配属unit自身が
        // 作戦horizon内に交戦距離へ入れなければ、遠ざかる目標を追跡しない。
        if enemy_island.is_none()
            && !assigned_unit_can_intercept_within_horizon(world, deployment, position)
        {
            continue;
        }
        let eta = map
            .distance(
                position.x,
                position.y,
                deployment.intent.anchor.x,
                deployment.intent.anchor.y,
            )
            .div_ceil(stats.max_movement.max(1));
        if eta > deployment.intent.threat_horizon
            || !connectivity.is_reachable(
                map,
                master_data,
                (position.x, position.y),
                (deployment.intent.anchor.x, deployment.intent.anchor.y),
                stats.movement_type,
            )
            || !can_engage(world, connectivity, deployment.entity, entity)
        {
            continue;
        }
        let category = if stats.can_capture {
            0
        } else if stats.max_cargo > 0 {
            1
        } else {
            2
        };
        candidates.push((
            category,
            eta,
            position.x,
            position.y,
            entity.to_bits(),
            entity,
        ));
    }
    candidates.sort_unstable_by_key(|candidate| {
        (
            candidate.0,
            candidate.1,
            candidate.2,
            candidate.3,
            candidate.4,
        )
    });
    candidates.first().map(|candidate| candidate.5)
}

/// 生産時に選んだ敵が移動しても、別の陸塊まで追跡して作戦島を離れない。
/// 海空上の目標は島IDを持たないため、作戦horizon内にいる場合だけ継続する。
fn target_within_operation(world: &World, deployment: &AssignedDeployment, enemy: Entity) -> bool {
    let (Some(enemy_position), Some(enemy_stats), Some(map)) = (
        world.get::<GridPosition>(enemy),
        world.get::<UnitStats>(enemy),
        world.get_resource::<Map>(),
    ) else {
        return false;
    };
    let island_map = world
        .get_resource::<IslandMap>()
        .cloned()
        .unwrap_or_else(|| IslandMap::analyze(map));
    let operation_island = island_map
        .get_island_at(&deployment.intent.anchor)
        .map(|island| island.id);
    let enemy_island = island_map
        .get_island_at(enemy_position)
        .map(|island| island.id);
    if operation_island.is_some() && enemy_island.is_some() && enemy_island != operation_island {
        return false;
    }
    if enemy_island.is_none()
        && !assigned_unit_can_intercept_within_horizon(world, deployment, enemy_position)
    {
        return false;
    }
    let eta = map
        .distance(
            enemy_position.x,
            enemy_position.y,
            deployment.intent.anchor.x,
            deployment.intent.anchor.y,
        )
        .div_ceil(enemy_stats.max_movement.max(1));
    eta <= deployment.intent.threat_horizon.max(1)
}

/// 海空目標までの距離を、配属unit自身の移動力と最大射程で手番換算する。
fn assigned_unit_can_intercept_within_horizon(
    world: &World,
    deployment: &AssignedDeployment,
    target: &GridPosition,
) -> bool {
    let (Some(position), Some(stats), Some(map)) = (
        world.get::<GridPosition>(deployment.entity),
        world.get::<UnitStats>(deployment.entity),
        world.get_resource::<Map>(),
    ) else {
        return false;
    };
    let travel_distance = map
        .distance(position.x, position.y, target.x, target.y)
        .saturating_sub(stats.max_range.max(1));
    let intercept_eta = travel_distance.div_ceil(stats.max_movement.max(1));
    intercept_eta <= deployment.intent.threat_horizon.max(1)
}

/// 優先敵の現在位置を追跡し、消滅・到達不能時だけ局地敵へ再目標化する。
fn resolve_target(
    world: &mut World,
    connectivity: &mut TerrainConnectivity,
    deployment: &AssignedDeployment,
) -> Option<(Entity, GridPosition)> {
    // 毎手番priority listの先頭へ戻すと、複数の敵へダメージを散らして撃破できない。
    // 現在標的が生存して交戦可能な限り固定し、撃破・到達不能時だけ次へ進む。
    if let Some(enemy) = deployment.current_target
        && deployment.intent.priority_enemies.contains(&enemy)
        && target_within_operation(world, deployment, enemy)
        && can_engage(world, connectivity, deployment.entity, enemy)
        && let Some(position) = world.get::<GridPosition>(enemy)
    {
        return Some((enemy, *position));
    }
    for &enemy in &deployment.intent.priority_enemies {
        if target_within_operation(world, deployment, enemy)
            && can_engage(world, connectivity, deployment.entity, enemy)
            && let Some(position) = world.get::<GridPosition>(enemy)
        {
            return Some((enemy, *position));
        }
    }
    local_retarget(world, connectivity, deployment).and_then(|enemy| {
        world
            .get::<GridPosition>(enemy)
            .copied()
            .map(|position| (enemy, position))
    })
}

/// 生産済みEntityを汎用free poolより先にV4局地Attack任務へ予約する。
///
/// 島嶼キャンペーンで既に予約されたEntityは既存作戦を優先し、次ターン再試行する。
pub(crate) fn prepare_deployment_squads(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    higher_priority_reserved: &HashSet<Entity>,
) -> HashSet<Entity> {
    prepare_deployment_squads_with_connectivity(
        world,
        manager,
        player_id,
        higher_priority_reserved,
        &mut TerrainConnectivity::default(),
    )
}

/// [`prepare_deployment_squads`] の地形連結cacheを呼出側と共有する版。
///
/// 地形連結は地図と移動種別だけで決まるため、同じ手番の部隊再編と同じcacheを使ってよい。
pub(crate) fn prepare_deployment_squads_with_connectivity(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    higher_priority_reserved: &HashSet<Entity>,
    connectivity: &mut TerrainConnectivity,
) -> HashSet<Entity> {
    // 拡張経路では、占領枠の直接任務化と、迎撃役の上限つき目標解決（H06）を使う。
    // Entityの走査順・Squadの付け替え・解放の手順は両経路で共通。
    let is_extended_path =
        crate::ai::code_path::is_extended_at("v4/deployment.rs::prepare_deployment_squads");
    let mut target_commitments: HashMap<Entity, usize> = HashMap::default();
    let mut interceptors = 0_usize;
    let mut registry = world
        .remove_resource::<V4DeploymentRegistry>()
        .unwrap_or_default();
    let mut reserved = HashSet::default();
    let mut releases = Vec::new();
    let island_map = world
        .get_resource::<IslandMap>()
        .cloned()
        .or_else(|| world.get_resource::<Map>().map(IslandMap::analyze));
    let mut entities: Vec<_> = registry
        .assigned
        .iter()
        .filter(|(_, deployment)| deployment.intent.player_id == player_id && deployment.active)
        .map(|(&entity, _)| entity)
        .collect();
    entities.sort_unstable_by_key(|entity| entity.to_bits());

    for entity in entities {
        let Some(snapshot) = registry.assigned.get(&entity).cloned() else {
            continue;
        };
        if world.get_entity(entity).is_err() {
            releases.push((entity, snapshot.squad_id));
            continue;
        }
        if higher_priority_reserved.contains(&entity) {
            if let Some(deployment) = registry.assigned.get_mut(&entity) {
                deployment.current_target = None;
            }
            continue;
        }
        let (target_entity, target, mission_type) = match snapshot.intent.posture {
            // 首都攻略パッケージが揃う前は、個々の生産Entityを敵地へ逐次投入しない。
            DeploymentPosture::Forming => {
                (None, snapshot.intent.staging_anchor, MissionType::Defense)
            }
            // 拡張経路の占領枠は敵の目標解決を行わず、anchor物件への占領任務にする。
            DeploymentPosture::Execute
                if is_extended_path && snapshot.intent.slot_kind == SlotKind::Capture =>
            {
                // anchor物件が既に自軍所有なら占領任務は達成済み。任務を出し続けると
                // 占領役が物件上に留まり続け、生産マス前の通路を塞ぐため解放する。
                if extended::anchor_is_owned_by(world, snapshot.intent.anchor, player_id) {
                    releases.push((entity, snapshot.squad_id));
                    continue;
                }
                (None, snapshot.intent.anchor, MissionType::Capture)
            }
            DeploymentPosture::Execute => {
                let resolved = if is_extended_path {
                    extended::resolve_target(
                        world,
                        connectivity,
                        &snapshot,
                        &target_commitments,
                        interceptors,
                    )
                } else {
                    resolve_target(world, connectivity, &snapshot)
                        .map(|(target_entity, target)| (target_entity, target, false))
                };
                match resolved {
                    Some((target_entity, target, is_wall)) => {
                        if is_extended_path {
                            // 同じ敵への割当数と壁役の数は、拡張経路の目標解決だけが参照する。
                            *target_commitments.entry(target_entity).or_insert(0) += 1;
                            if is_wall {
                                interceptors += 1;
                            }
                        }
                        (Some(target_entity), target, MissionType::Attack)
                    }
                    // Combat排除後も対象拠点の占領完了まではPlanを閉じない。
                    // 生産戦力をfree poolへ返さず、anchorで反撃増援を待ち受ける。
                    None if snapshot.intent.plan_step.is_some() => {
                        (None, snapshot.intent.anchor, MissionType::Defense)
                    }
                    None => {
                        releases.push((entity, snapshot.squad_id));
                        continue;
                    }
                }
            }
        };

        // ID列を先に集め、1つのkeyed確定口で旧参照除去→受入の順序を保つ。
        let other_ids = manager
            .squads
            .iter()
            .filter(|squad| {
                squad.owner_id == Some(player_id) && Some(squad.id) != snapshot.squad_id
            })
            .map(|squad| squad.id)
            .collect::<Vec<_>>();
        let existing_id = snapshot.squad_id.filter(|id| {
            manager
                .squads
                .iter()
                .any(|squad| squad.id == *id && squad.owner_id == Some(player_id))
        });
        let squad_id = {
            let mut proposals = OperationLedger::proposals(manager);
            for id in other_ids {
                proposals.leave(id, entity);
            }
            let id = existing_id.unwrap_or_else(|| {
                proposals
                    .create_owned(
                        MissionType::Attack,
                        player_id,
                        Default::default(),
                        None,
                        Vec::new(),
                    )
                    .id
            });
            proposals.clear_members(id);
            proposals.join(id, entity);
            let squad = proposals.squad_mut(id).expect("確定した配備Squad");
            squad.mission_type = mission_type;
            squad.target = Some(target);
            // Planと同じcampaign ownerを保ち、排他reconcileとの競合を防ぐ。
            squad.target_island = island_map
                .as_ref()
                .and_then(|islands| islands.get_island_at(&snapshot.intent.anchor))
                .map(|island| island.id);
            squad.phase = MissionPhase::MovingToTarget;
            id
        };
        if let Some(deployment) = registry.assigned.get_mut(&entity) {
            deployment.squad_id = Some(squad_id);
            deployment.current_target = target_entity;
        }
        reserved.insert(entity);
    }

    for (entity, squad_id) in releases {
        if let Some(deployment) = registry.assigned.get_mut(&entity) {
            deployment.active = false;
            deployment.squad_id = None;
            deployment.current_target = None;
        }
        if let Some(squad_id) = squad_id {
            OperationLedger::squad_list(manager).retain(|squad| squad.id != squad_id);
        }
    }
    world.insert_resource(registry);
    reserved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::islands::IslandMap;
    use crate::resources::{GridTopology, MovementType, Terrain};

    fn pending(order: u32) -> PendingDeployment {
        PendingDeployment {
            forming_slot: None,
            operation_binding: None,
            player_id: PlayerId(1),
            turn: 3,
            order,
            facility: GridPosition { x: 2, y: 4 },
            unit_type: UnitType::Fighter,
            anchor: GridPosition { x: 8, y: 8 },
            staging_anchor: GridPosition { x: 2, y: 4 },
            posture: DeploymentPosture::Execute,
            slot_kind: SlotKind::Combat,
            priority_enemies: vec![Entity::from_raw(order + 10)],
            threat_horizon: 4,
            forecast: DeploymentForecast::default(),
            plan_step: None,
        }
    }

    /// 同手番の再計画で発注を置き換えたとき、編成slotを予約するのは拡張経路だけ
    /// （F22の経路差を固定する）。拡張経路は未実体のslotをPendingと同じ粒度で置換する。
    #[test]
    fn replace_turn_orders_reserves_forming_slots_only_on_extended_path() {
        for (path, reserves_slots) in [
            (crate::ai::code_path::CodePath::MainCompatible, false),
            (crate::ai::code_path::CodePath::Extended, true),
        ] {
            let _guard = crate::ai::code_path::enter(path);
            let mut registry = V4DeploymentRegistry::default();
            registry.replace_turn_orders(PlayerId(1), 3, [pending(0), pending(1)]);
            // 同じ手番に発注Aを取り消してBだけを残す。
            registry.replace_turn_orders(PlayerId(1), 3, [pending(1)]);

            assert_eq!(registry.pending.len(), 1, "{path:?}");
            assert_eq!(
                registry.pending[0].forming_slot.is_some(),
                reserves_slots,
                "{path:?}"
            );
            let expected_slots = usize::from(reserves_slots);
            assert_eq!(registry.forming_slots.len(), expected_slots, "{path:?}");
        }
    }

    /// `pending` の発注と同じ施設(2,4)・兵種で完成した生産Eventを作る。
    fn produced_event(entity: Entity) -> UnitProducedEvent {
        UnitProducedEvent {
            player_id: PlayerId(1),
            target_x: 2,
            target_y: 4,
            unit_type: UnitType::Fighter,
            entity,
        }
    }

    /// 拡張経路でA取消後にBが完成したとき、生産EventはBの発注意図と予約slotへ接続し、
    /// 取り消したAの予約は残らない（F22の受入条件）。
    #[test]
    fn cancelled_order_slot_is_not_bound_to_the_next_produced_unit() {
        let _guard = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let mut registry = V4DeploymentRegistry::default();
        registry.replace_turn_orders(PlayerId(1), 3, [pending(0), pending(1)]);
        let cancelled_slot = registry.pending[0]
            .forming_slot
            .expect("拡張経路は発注Aにslotを予約する");
        registry.replace_turn_orders(PlayerId(1), 3, [pending(1)]);
        let kept_slot = registry.pending[0]
            .forming_slot
            .expect("拡張経路は発注Bにslotを予約する");
        assert_ne!(kept_slot, cancelled_slot);

        let produced = Entity::from_raw(100);
        let bound_slot = registry.assign_produced(&produced_event(produced), 3);

        assert_eq!(bound_slot, Some(kept_slot));
        let assigned = registry.assigned.get(&produced).expect("Bの完成Entity");
        assert_eq!(
            assigned.intent.order, 1,
            "取り消したAの発注意図へ紐付けない"
        );
        assert_eq!(assigned.intent.forming_slot, Some(kept_slot));
        assert!(!registry.forming_slots.contains_key(&cancelled_slot));
        assert_eq!(
            registry
                .forming_slots
                .get(&kept_slot)
                .and_then(|reservation| reservation.entity),
            Some(produced)
        );
    }

    /// 完成済みEntityを持つslotは、同手番の再計画で発注を置き換えても撤回しない。
    #[test]
    fn produced_slot_survives_same_turn_order_replacement() {
        let _guard = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let mut registry = V4DeploymentRegistry::default();
        registry.replace_turn_orders(PlayerId(1), 3, [pending(0)]);
        let produced = Entity::from_raw(100);
        let produced_slot = registry
            .assign_produced(&produced_event(produced), 3)
            .expect("発注Aは予約slotへ接続する");

        registry.replace_turn_orders(PlayerId(1), 3, [pending(1)]);

        assert_eq!(
            registry
                .forming_slots
                .get(&produced_slot)
                .and_then(|reservation| reservation.entity),
            Some(produced),
            "完成済み予約は残る"
        );
        let new_slot = registry.pending[0]
            .forming_slot
            .expect("後続発注Bには新しいslotを予約する");
        assert_ne!(new_slot, produced_slot);
        assert_eq!(registry.forming_slots.len(), 2);
    }

    /// 段階3以前の `resolve_produced_slot`（イベントごとに直接Squadへ書く実装）の参照実装。
    /// 所属fieldは現在テストfixture経由でしか直接書けないため、書込みだけfixture口を使う。
    fn legacy_resolve_produced_slot(
        registry: &mut V4DeploymentRegistry,
        entity: Entity,
        manager: &mut SquadManager,
        island_map: Option<&IslandMap>,
    ) {
        let Some(snapshot) = registry.assigned.get(&entity).cloned() else {
            return;
        };
        let Some(slot) = snapshot.intent.forming_slot else {
            return;
        };
        let existing = registry
            .forming_slots
            .get(&slot)
            .and_then(|reservation| reservation.squad_id)
            .and_then(|squad_id| {
                manager.squads.iter().position(|squad| {
                    squad.id == squad_id && squad.owner_id == Some(snapshot.intent.player_id)
                })
            });
        let squad = if let Some(index) = existing {
            &mut manager.squads[index]
        } else {
            manager.create_owned_squad(
                match snapshot.intent.posture {
                    DeploymentPosture::Execute => MissionType::Attack,
                    DeploymentPosture::Forming => MissionType::Defense,
                },
                snapshot.intent.player_id,
            )
        };
        squad.members.insert(entity);
        squad.target = Some(match snapshot.intent.posture {
            DeploymentPosture::Execute => snapshot.intent.anchor,
            DeploymentPosture::Forming => snapshot.intent.staging_anchor,
        });
        squad.target_island = island_map
            .and_then(|islands| islands.get_island_at(&snapshot.intent.anchor))
            .map(|island| island.id);
        squad.phase = MissionPhase::Forming;
        let squad_id = squad.id;
        if let Some(reservation) = registry.forming_slots.get_mut(&slot) {
            reservation.squad_id = Some(squad_id);
        }
        if let Some(deployment) = registry.assigned.get_mut(&entity) {
            deployment.squad_id = Some(squad_id);
        }
    }

    /// 同じ受付で同じslotの2体が完成したとき、1回の確定でも段階3以前の逐次解決
    /// （参照実装）と同じく先行提案のSquadへ後続を合流させ、SquadIdと書戻しも一致する。
    ///
    /// 本番の `replace_turn_orders` は発注ごとに新しいslotを発番するため（呼出し元は
    /// すべて `forming_slot: None`）、同じslotの共有はこのテストで指定した場合だけ起きる。
    /// 提案の `Proposed` 合流が逐次実装と等価であることの契約として固定する。
    #[test]
    fn shared_slot_in_one_receipt_matches_sequential_resolution() {
        let _guard = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let setup = || {
            let mut registry = V4DeploymentRegistry::default();
            let shared = Some(FormingSquadSlotId(77));
            let orders = [pending(0), pending(1)].map(|mut order| {
                order.forming_slot = shared;
                order
            });
            registry.replace_turn_orders(PlayerId(1), 3, orders);
            let mut manager = SquadManager::default();
            // 既存Squadがあっても新設idは発番順どおりになることを確かめる。
            manager.create_owned_squad(MissionType::Reserve, PlayerId(1));
            (registry, manager)
        };
        let entities = [Entity::from_raw(100), Entity::from_raw(101)];

        // 段階3以前: イベントごとに解決し、その場でSquadへ書き込む。
        let (mut sequential, mut sequential_manager) = setup();
        for entity in entities {
            sequential.assign_produced(&produced_event(entity), 3);
            legacy_resolve_produced_slot(&mut sequential, entity, &mut sequential_manager, None);
        }

        // 新: 全件を提案してから1回だけ確定する。
        let (mut registry, mut manager) = setup();
        let events = entities.map(produced_event);
        let events = events.iter().collect::<Vec<_>>();
        let mut proposal = ProducedAssignmentProposal::default();
        let receipts = propose_produced_events(
            &events,
            3,
            &mut registry,
            Some(&manager),
            None,
            &mut proposal,
        );
        // 確定前はSquadManagerを変更しない。
        assert_eq!(manager.squads.len(), 1);
        let delta = OperationLedger::commit_produced(&mut manager, proposal);
        bind_produced_squads(&mut registry, &receipts, &delta);

        assert_eq!(manager.squads.len(), 2);
        assert_eq!(manager.squads.len(), sequential_manager.squads.len());
        for (left, right) in manager.squads.iter().zip(&sequential_manager.squads) {
            assert_eq!(left.id, right.id);
            assert_eq!(left.members, *right.members);
            assert_eq!(left.mission_type, right.mission_type);
            assert_eq!(left.target, right.target);
            assert_eq!(left.phase, right.phase);
        }
        assert_eq!(
            manager.squads[1]
                .members
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            entities.to_vec()
        );
        for entity in entities {
            assert_eq!(
                registry.assigned[&entity].squad_id,
                sequential.assigned[&entity].squad_id
            );
        }
        assert_eq!(
            registry.forming_slots[&FormingSquadSlotId(77)].squad_id,
            Some(manager.squads[1].id)
        );
    }

    /// 発注照合Systemは経路によらず1本で動き、予約slotを持つ発注だけを生産直後に
    /// 実Squadへ解決する（T4：slotの有無で処理を選ぶ）。通常経路の発注はslotを
    /// 持たないため、従来どおり台帳へ移すだけでSquadを作らない。
    #[test]
    fn reconcile_system_resolves_squads_only_for_slotted_orders() {
        for (path, creates_squad) in [
            (crate::ai::code_path::CodePath::MainCompatible, false),
            (crate::ai::code_path::CodePath::Extended, true),
        ] {
            let _guard = crate::ai::code_path::enter(path);
            let mut registry = V4DeploymentRegistry::default();
            registry.replace_turn_orders(PlayerId(1), 3, [pending(0)]);

            let mut world = World::new();
            world.insert_resource(MatchState {
                current_turn_number: crate::resources::TurnNumber(3),
                ..MatchState::default()
            });
            world.insert_resource(registry);
            world.insert_resource(SquadManager::default());
            world.init_resource::<Events<UnitProducedEvent>>();
            let produced = Entity::from_raw(100);
            world.send_event(produced_event(produced));
            let mut schedule = Schedule::default();
            schedule
                .add_systems(crate::ai::v4::production_events::reconcile_production_events_system);
            schedule.run(&mut world);

            let registry = world.resource::<V4DeploymentRegistry>();
            let assigned = registry
                .assigned
                .get(&produced)
                .expect("完成Entityを照合する");
            assert_eq!(assigned.squad_id.is_some(), creates_squad, "{path:?}");
            let squads = &world.resource::<SquadManager>().squads;
            assert_eq!(squads.len(), usize::from(creates_squad), "{path:?}");
            assert!(
                squads.iter().all(|squad| squad.members.contains(&produced)),
                "{path:?}"
            );
        }
    }

    #[test]
    fn issue95_produced_entities_match_same_type_orders_deterministically() {
        let mut registry = V4DeploymentRegistry::default();
        registry.replace_turn_orders(PlayerId(1), 3, [pending(0), pending(1)]);
        let first = Entity::from_raw(100);
        let second = Entity::from_raw(101);
        for entity in [first, second] {
            registry.assign_produced(
                &UnitProducedEvent {
                    player_id: PlayerId(1),
                    target_x: 2,
                    target_y: 4,
                    unit_type: UnitType::Fighter,
                    entity,
                },
                3,
            );
        }

        assert_eq!(registry.assigned[&first].intent.order, 0);
        assert_eq!(registry.assigned[&second].intent.order, 1);
        assert!(registry.pending.is_empty());
    }

    #[test]
    fn closed_plan_releases_assigned_entity_from_mission_protection() {
        use crate::ai::v4::plan_revision::{PlanId, PlanRevision, PlanStepId};

        let mut registry = V4DeploymentRegistry::default();
        let mut order = pending(0);
        let plan_id = PlanId(7);
        order.plan_step = Some(PlanStepRef {
            plan_id,
            revision: PlanRevision(2),
            step_id: PlanStepId(0),
        });
        registry.replace_turn_orders(PlayerId(1), 3, [order]);
        let entity = Entity::from_raw(100);
        registry.assign_produced(
            &UnitProducedEvent {
                player_id: PlayerId(1),
                target_x: 2,
                target_y: 4,
                unit_type: UnitType::Fighter,
                entity,
            },
            3,
        );
        assert!(registry.active_entities(PlayerId(1)).contains(&entity));

        registry.release_closed_plans(&HashSet::from_iter([plan_id]));

        assert!(!registry.active_entities(PlayerId(1)).contains(&entity));
        assert!(!registry.assigned[&entity].active);
    }

    #[test]
    fn issue95_old_failed_order_does_not_match_next_turn() {
        let mut registry = V4DeploymentRegistry::default();
        registry.replace_turn_orders(PlayerId(1), 3, [pending(0)]);
        registry.assign_produced(
            &UnitProducedEvent {
                player_id: PlayerId(1),
                target_x: 2,
                target_y: 4,
                unit_type: UnitType::Fighter,
                entity: Entity::from_raw(100),
            },
            4,
        );

        assert!(registry.assigned.is_empty());
    }

    #[test]
    fn issue95_attack_audit_tracks_first_and_priority_attacks() {
        let mut registry = V4DeploymentRegistry::default();
        registry.replace_turn_orders(PlayerId(1), 3, [pending(0)]);
        let attacker = Entity::from_raw(100);
        let priority = Entity::from_raw(10);
        registry.assign_produced(
            &UnitProducedEvent {
                player_id: PlayerId(1),
                target_x: 2,
                target_y: 4,
                unit_type: UnitType::Fighter,
                entity: attacker,
            },
            3,
        );

        registry.record_attack(
            attacker,
            priority,
            5,
            DeploymentAttackAudit {
                defender_can_capture: true,
                damage_value_dealt: 400,
                counter_value_received: 100,
                ..DeploymentAttackAudit::default()
            },
        );
        registry.record_attack(
            attacker,
            Entity::from_raw(99),
            6,
            DeploymentAttackAudit {
                defender_is_transport: true,
                destroyed: true,
                damage_value_dealt: 600,
                destroyed_value: 5_000,
                ..DeploymentAttackAudit::default()
            },
        );

        let record = registry.audit_records(PlayerId(1)).pop().unwrap();
        assert_eq!(record.attack_count, 2);
        assert_eq!(record.priority_attack_count, 1);
        assert_eq!(record.mission_target_attack_count, 1);
        assert_eq!(record.mission_target_kill_count, 0);
        assert_eq!(record.mission_target_damage_value_dealt, 400);
        assert_eq!(record.mission_target_counter_value_received, 100);
        assert_eq!(record.mission_target_destroyed_value, 0);
        assert_eq!(record.mission_target_first_attack_turn, Some(5));
        assert_eq!(record.first_attack_turn, Some(5));
        assert_eq!(record.capture_unit_attack_count, 1);
        assert_eq!(record.transport_unit_attack_count, 1);
        assert_eq!(record.kill_count, 1);
        assert_eq!(record.damage_value_dealt, 1_000);
        assert_eq!(record.counter_value_received, 100);
        assert_eq!(record.destroyed_value, 5_000);
        assert_eq!(
            record.plan_combat_actuals(),
            PlanCombatActuals {
                first_attack_turn: Some(5),
                attack_count: 2,
                priority_attack_count: 1,
                kill_count: 1,
                damage_value_dealt: 1_000,
                counter_value_received: 100,
                destroyed_value: 5_000,
            },
            "Plan予実は現在標的以外へ切り替えた2回目の攻撃も含む"
        );
    }

    fn combat_stats(unit_type: UnitType, can_capture: bool, max_cargo: u32) -> UnitStats {
        UnitStats {
            unit_type,
            movement_type: MovementType::Infantry,
            max_movement: 3,
            max_ammo1: 9,
            can_capture,
            max_cargo,
            cost: 5_000,
            ..UnitStats::mock()
        }
    }

    fn deployment_world() -> (World, Entity, Entity, Entity, Entity) {
        let mut world = World::new();
        world.insert_resource(Map::new(10, 1, Terrain::Plains, GridTopology::Square));
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Tank, UnitType::Infantry, 60);
        chart.insert_damage(UnitType::Tank, UnitType::TransportHelicopter, 40);
        world.insert_resource(chart);
        let attacker = world
            .spawn((
                Faction(PlayerId(1)),
                GridPosition { x: 0, y: 0 },
                combat_stats(UnitType::Tank, false, 0),
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 0,
                    max_ammo2: 0,
                },
            ))
            .id();
        let priority = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 3, y: 0 },
                combat_stats(UnitType::Infantry, true, 0),
            ))
            .id();
        // より近い輸送より占領可能ユニットを局地fallbackで優先する。
        let transport = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 2, y: 0 },
                combat_stats(UnitType::TransportHelicopter, false, 2),
            ))
            .id();
        let capture = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 4, y: 0 },
                combat_stats(UnitType::Infantry, true, 0),
            ))
            .id();
        let mut registry = V4DeploymentRegistry::default();
        registry.assigned.insert(
            attacker,
            AssignedDeployment {
                entity: attacker,
                intent: PendingDeployment {
                    forming_slot: None,
                    operation_binding: None,
                    player_id: PlayerId(1),
                    turn: 3,
                    order: 0,
                    facility: GridPosition { x: 0, y: 0 },
                    unit_type: UnitType::Tank,
                    anchor: GridPosition { x: 5, y: 0 },
                    staging_anchor: GridPosition { x: 0, y: 0 },
                    posture: DeploymentPosture::Execute,
                    slot_kind: SlotKind::Combat,
                    priority_enemies: vec![priority],
                    threat_horizon: 3,
                    forecast: DeploymentForecast::default(),
                    plan_step: None,
                },
                squad_id: None,
                current_target: None,
                active: true,
                assigned_turn: 3,
                attack_count: 0,
                priority_attack_count: 0,
                mission_target_attack_count: 0,
                mission_target_kill_count: 0,
                mission_target_damage_value_dealt: 0,
                mission_target_counter_value_received: 0,
                mission_target_destroyed_value: 0,
                mission_target_first_attack_turn: None,
                capture_unit_attack_count: 0,
                transport_unit_attack_count: 0,
                kill_count: 0,
                damage_value_dealt: 0,
                counter_value_received: 0,
                destroyed_value: 0,
                first_attack_turn: None,
            },
        );
        world.insert_resource(registry);
        (world, attacker, priority, transport, capture)
    }

    #[test]
    fn capital_formation_waits_at_staging_without_targeting_the_enemy() {
        let (mut world, attacker, priority, _, _) = deployment_world();
        {
            let mut registry = world.resource_mut::<V4DeploymentRegistry>();
            let deployment = registry.assigned.get_mut(&attacker).unwrap();
            deployment.intent.posture = DeploymentPosture::Forming;
            deployment.intent.staging_anchor = GridPosition { x: 1, y: 0 };
        }
        let mut manager = SquadManager::default();

        let reserved =
            prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::default());

        assert!(reserved.contains(&attacker));
        let squad = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&attacker))
            .unwrap();
        assert_eq!(squad.mission_type, MissionType::Defense);
        assert_eq!(squad.target, Some(GridPosition { x: 1, y: 0 }));
        assert_eq!(
            squad.target_island,
            IslandMap::analyze(world.resource::<Map>())
                .get_island_at(&GridPosition { x: 5, y: 0 })
                .map(|island| island.id),
            "deployment Squadは生産Planと同じcampaign ownerを持つ"
        );
        let registry = world.resource::<V4DeploymentRegistry>();
        assert_ne!(registry.assigned[&attacker].current_target, Some(priority));
    }

    #[test]
    fn issue95_priority_enemy_is_followed_and_beam_squad_is_protected() {
        let (mut world, attacker, priority, _, _) = deployment_world();
        let mut manager = SquadManager::default();
        let reserved =
            prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::default());
        assert_eq!(reserved, HashSet::from_iter([attacker]));
        let squad = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&attacker))
            .unwrap();
        assert_eq!(squad.target, Some(GridPosition { x: 3, y: 0 }));
        let squad_id = squad.id;
        assert!(
            world
                .resource::<V4DeploymentRegistry>()
                .protected_squads(PlayerId(1))
                .contains(&squad_id)
        );
        assert_eq!(
            world
                .resource::<V4DeploymentRegistry>()
                .attack_target(attacker),
            Some(priority)
        );
        assert_eq!(
            world
                .resource::<V4DeploymentRegistry>()
                .active_entities(PlayerId(1)),
            HashSet::from_iter([attacker])
        );

        *world.get_mut::<GridPosition>(priority).unwrap() = GridPosition { x: 6, y: 0 };
        prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::default());
        let squad = manager
            .squads
            .iter()
            .find(|squad| squad.id == squad_id)
            .unwrap();
        assert_eq!(squad.target, Some(GridPosition { x: 6, y: 0 }));
    }

    #[test]
    fn current_target_is_kept_until_destroyed_or_unreachable() {
        let (mut world, attacker, priority, transport, _) = deployment_world();
        {
            let mut registry = world.resource_mut::<V4DeploymentRegistry>();
            let deployment = registry.assigned.get_mut(&attacker).unwrap();
            // priority listの先頭が変わっても、生存中の攻撃対象へ火力を集中する。
            deployment.intent.priority_enemies = vec![transport, priority];
            deployment.current_target = Some(priority);
        }
        let mut manager = SquadManager::default();

        prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::default());

        assert_eq!(
            world
                .resource::<V4DeploymentRegistry>()
                .attack_target(attacker),
            Some(priority)
        );
        let squad = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&attacker))
            .unwrap();
        assert_eq!(squad.target, Some(GridPosition { x: 3, y: 0 }));
    }

    #[test]
    fn issue95_missing_priority_retargets_capture_then_releases() {
        let (mut world, attacker, priority, transport, capture) = deployment_world();
        let mut manager = SquadManager::default();
        prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::default());
        world.despawn(priority);
        prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::default());
        let squad = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&attacker))
            .unwrap();
        assert_eq!(squad.target, Some(GridPosition { x: 4, y: 0 }));
        assert_eq!(
            world
                .resource::<V4DeploymentRegistry>()
                .attack_target(attacker),
            Some(capture)
        );

        world.despawn(capture);
        world.despawn(transport);
        let reserved =
            prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::default());
        assert!(reserved.is_empty());
        assert!(!world.resource::<V4DeploymentRegistry>().assigned[&attacker].active);
        assert!(
            manager
                .squads
                .iter()
                .all(|squad| !squad.members.contains(&attacker))
        );
    }

    #[test]
    fn local_retarget_does_not_leave_the_operation_island() {
        let mut world = World::new();
        let mut map = Map::new(7, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(2, 0, Terrain::Sea).unwrap();
        world.insert_resource(IslandMap::analyze(&map));
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 60);
        world.insert_resource(chart);
        let mut attacker_stats = combat_stats(UnitType::Bcopters, false, 0);
        attacker_stats.movement_type = MovementType::Air;
        attacker_stats.max_movement = 6;
        let attacker = world
            .spawn((
                Faction(PlayerId(1)),
                GridPosition { x: 5, y: 0 },
                attacker_stats,
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 0,
                    max_ammo2: 0,
                },
            ))
            .id();
        let remote_enemy = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 1, y: 0 },
                combat_stats(UnitType::Infantry, true, 0),
            ))
            .id();
        let mut deployment = AssignedDeployment {
            entity: attacker,
            intent: PendingDeployment {
                forming_slot: None,
                operation_binding: None,
                player_id: PlayerId(1),
                turn: 3,
                order: 0,
                facility: GridPosition { x: 5, y: 0 },
                unit_type: UnitType::Bcopters,
                anchor: GridPosition { x: 5, y: 0 },
                staging_anchor: GridPosition { x: 5, y: 0 },
                posture: DeploymentPosture::Execute,
                slot_kind: SlotKind::Combat,
                priority_enemies: Vec::new(),
                threat_horizon: 4,
                forecast: DeploymentForecast::default(),
                plan_step: None,
            },
            squad_id: None,
            current_target: None,
            active: true,
            assigned_turn: 3,
            attack_count: 0,
            priority_attack_count: 0,
            mission_target_attack_count: 0,
            mission_target_kill_count: 0,
            mission_target_damage_value_dealt: 0,
            mission_target_counter_value_received: 0,
            mission_target_destroyed_value: 0,
            mission_target_first_attack_turn: None,
            capture_unit_attack_count: 0,
            transport_unit_attack_count: 0,
            kill_count: 0,
            damage_value_dealt: 0,
            counter_value_received: 0,
            destroyed_value: 0,
            first_attack_turn: None,
        };

        assert_eq!(
            local_retarget(&mut world, &mut TerrainConnectivity::default(), &deployment),
            None,
            "別島の敵を局地fallbackで追跡してはならない"
        );

        let local_enemy = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 6, y: 0 },
                combat_stats(UnitType::Infantry, true, 0),
            ))
            .id();
        deployment.intent.priority_enemies = vec![remote_enemy];
        deployment.current_target = Some(remote_enemy);
        assert_eq!(
            resolve_target(&mut world, &mut TerrainConnectivity::default(), &deployment)
                .map(|target| target.0),
            Some(local_enemy),
            "生産時の優先敵が別島へ移動したら同じ島の敵へ再目標化する"
        );
    }

    #[test]
    fn islandless_target_must_be_interceptable_by_the_assigned_unit() {
        let mut world = World::new();
        let mut map = Map::new(7, 1, Terrain::Plains, GridTopology::Square);
        for x in 2..7 {
            map.set_terrain(x, 0, Terrain::Sea).unwrap();
        }
        world.insert_resource(IslandMap::analyze(&map));
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bcopters, UnitType::Bcopters, 40);
        world.insert_resource(chart);

        let mut attacker_stats = combat_stats(UnitType::Bcopters, false, 0);
        attacker_stats.movement_type = MovementType::Air;
        attacker_stats.max_movement = 1;
        attacker_stats.max_range = 1;
        let attacker = world
            .spawn((
                Faction(PlayerId(1)),
                GridPosition { x: 0, y: 0 },
                attacker_stats,
            ))
            .id();
        let mut enemy_stats = combat_stats(UnitType::Bcopters, false, 0);
        enemy_stats.movement_type = MovementType::Air;
        enemy_stats.max_movement = 6;
        let remote_enemy = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 6, y: 0 },
                enemy_stats.clone(),
            ))
            .id();
        let deployment = AssignedDeployment {
            entity: attacker,
            intent: PendingDeployment {
                forming_slot: None,
                operation_binding: None,
                player_id: PlayerId(1),
                turn: 3,
                order: 0,
                facility: GridPosition { x: 0, y: 0 },
                unit_type: UnitType::Bcopters,
                anchor: GridPosition { x: 0, y: 0 },
                staging_anchor: GridPosition { x: 0, y: 0 },
                posture: DeploymentPosture::Execute,
                slot_kind: SlotKind::Combat,
                priority_enemies: vec![remote_enemy],
                threat_horizon: 2,
                forecast: DeploymentForecast::default(),
                plan_step: None,
            },
            squad_id: None,
            current_target: Some(remote_enemy),
            active: true,
            assigned_turn: 3,
            attack_count: 0,
            priority_attack_count: 0,
            mission_target_attack_count: 0,
            mission_target_kill_count: 0,
            mission_target_damage_value_dealt: 0,
            mission_target_counter_value_received: 0,
            mission_target_destroyed_value: 0,
            mission_target_first_attack_turn: None,
            capture_unit_attack_count: 0,
            transport_unit_attack_count: 0,
            kill_count: 0,
            damage_value_dealt: 0,
            counter_value_received: 0,
            destroyed_value: 0,
            first_attack_turn: None,
        };

        assert!(!target_within_operation(&world, &deployment, remote_enemy));
        assert_eq!(
            local_retarget(&mut world, &mut TerrainConnectivity::default(), &deployment),
            None
        );

        let nearby_enemy = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 2, y: 0 },
                enemy_stats,
            ))
            .id();
        assert_eq!(
            local_retarget(&mut world, &mut TerrainConnectivity::default(), &deployment),
            Some(nearby_enemy)
        );
    }
}

// 小規模マップ戦略など拡張経路の実装。main互換経路と本文が異なる関数と拡張専用の項目を置く。
pub(crate) mod extended;
#[allow(unused_imports)]
pub use self::extended::*;
