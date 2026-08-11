//! V4の生産意図を、生産された実Entityの局地任務まで引き継ぐ台帳。
//!
//! 生産判断とSquad再計画は別フレーム・別ターンで実行されるため、診断traceだけでは
//! 「何のために買ったか」が失われる。このモジュールは発注を生産完了イベントへ照合し、
//! 優先敵が有効な間だけ汎用目標探索から保護する。

use crate::ai::islands::IslandMap;
use crate::ai::squad::{MissionPhase, MissionType, SquadId, SquadManager};
use crate::ai::turn_distance::TerrainConnectivity;
use crate::ai::v4::operation::SlotKind;
use crate::ai::v4::plan_revision::{ActiveDeploymentIntent, PlanId, PlanStepRef};
use crate::components::{Ammo, Faction, GridPosition, Health, PlayerId, Transporting, UnitStats};
use crate::events::{UnitAttackedEvent, UnitProducedEvent};
use crate::resources::{DamageChart, Map, MatchState, UnitType, master_data::MasterDataRegistry};
use bevy_ecs::event::EventReader;
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};

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

/// Combat見積へ渡す、既存EntityのPlan所有権と現在の対象集合。
#[derive(Debug, Clone)]
pub(crate) struct ActiveTargetAssignment {
    pub plan_id: Option<PlanId>,
    pub targets: HashSet<Entity>,
}

/// V4の未照合発注と、生産済みEntityの局地任務を保持するリソース。
#[derive(Resource, Debug, Default)]
pub struct V4DeploymentRegistry {
    pending: VecDeque<PendingDeployment>,
    assigned: HashMap<Entity, AssignedDeployment>,
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
        self.pending.extend(orders);
    }

    /// 生産完了イベントを、同じ手番・施設・兵種の最初の発注へ決定的に照合する。
    fn assign_produced(&mut self, event: &UnitProducedEvent, turn: u32) {
        let Some(index) = self.pending.iter().position(|pending| {
            pending.player_id == event.player_id
                && pending.turn == turn
                && pending.facility.x == event.target_x
                && pending.facility.y == event.target_y
                && pending.unit_type == event.unit_type
        }) else {
            return;
        };
        let intent = self
            .pending
            .remove(index)
            .expect("照合済みpending deploymentが存在する");
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
        if deployment.current_target == Some(defender) {
            deployment.mission_target_attack_count =
                deployment.mission_target_attack_count.saturating_add(1);
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

/// 実際に生産されたEntityをpending deploymentへ照合する。
pub fn reconcile_pending_deployments_system(
    mut events: EventReader<UnitProducedEvent>,
    match_state: Res<MatchState>,
    mut registry: ResMut<V4DeploymentRegistry>,
) {
    let turn = match_state.current_turn_number.0;
    for event in events.read() {
        registry.assign_produced(event, turn);
    }
    // 失敗した生産命令を翌ターンの同型発注へ誤照合しない。
    registry.pending.retain(|pending| pending.turn >= turn);
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

/// 優先敵の現在位置を追跡し、消滅・到達不能時だけ局地敵へ再目標化する。
fn resolve_target(
    world: &mut World,
    connectivity: &mut TerrainConnectivity,
    deployment: &AssignedDeployment,
) -> Option<(Entity, GridPosition)> {
    for &enemy in &deployment.intent.priority_enemies {
        if can_engage(world, connectivity, deployment.entity, enemy)
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
/// 緊急迎撃・島嶼キャンペーンで既に予約されたEntityは上位責務を優先し、次ターン再試行する。
pub(crate) fn prepare_deployment_squads(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    higher_priority_reserved: &HashSet<Entity>,
) -> HashSet<Entity> {
    let mut registry = world
        .remove_resource::<V4DeploymentRegistry>()
        .unwrap_or_default();
    let mut connectivity = TerrainConnectivity::default();
    let mut reserved = HashSet::new();
    let mut releases = Vec::new();
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
            DeploymentPosture::Execute => {
                match resolve_target(world, &mut connectivity, &snapshot) {
                    Some((target_entity, target)) => {
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

        // 過去の汎用Squadへ混入していた場合は、このEntityだけを切り離す。
        for squad in &mut manager.squads {
            if squad.owner_id == Some(player_id) && Some(squad.id) != snapshot.squad_id {
                squad.members.remove(&entity);
            }
        }
        let squad_index = snapshot.squad_id.and_then(|id| {
            manager
                .squads
                .iter()
                .position(|squad| squad.id == id && squad.owner_id == Some(player_id))
        });
        let squad = if let Some(index) = squad_index {
            &mut manager.squads[index]
        } else {
            manager.create_owned_squad(MissionType::Attack, player_id)
        };
        squad.members.clear();
        squad.members.insert(entity);
        squad.mission_type = mission_type;
        squad.target = Some(target);
        squad.target_island = None;
        squad.phase = MissionPhase::MovingToTarget;
        let squad_id = squad.id;
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
            manager.squads.retain(|squad| squad.id != squad_id);
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

        registry.release_closed_plans(&HashSet::from([plan_id]));

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
        assert_eq!(record.first_attack_turn, Some(5));
        assert_eq!(record.capture_unit_attack_count, 1);
        assert_eq!(record.transport_unit_attack_count, 1);
        assert_eq!(record.kill_count, 1);
        assert_eq!(record.damage_value_dealt, 1_000);
        assert_eq!(record.counter_value_received, 100);
        assert_eq!(record.destroyed_value, 5_000);
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
            prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::new());

        assert!(reserved.contains(&attacker));
        let squad = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&attacker))
            .unwrap();
        assert_eq!(squad.mission_type, MissionType::Defense);
        assert_eq!(squad.target, Some(GridPosition { x: 1, y: 0 }));
        let registry = world.resource::<V4DeploymentRegistry>();
        assert_ne!(registry.assigned[&attacker].current_target, Some(priority));
    }

    #[test]
    fn issue95_priority_enemy_is_followed_and_beam_squad_is_protected() {
        let (mut world, attacker, priority, _, _) = deployment_world();
        let mut manager = SquadManager::default();
        let reserved =
            prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::new());
        assert_eq!(reserved, HashSet::from([attacker]));
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
            HashSet::from([attacker])
        );

        *world.get_mut::<GridPosition>(priority).unwrap() = GridPosition { x: 6, y: 0 };
        prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::new());
        let squad = manager
            .squads
            .iter()
            .find(|squad| squad.id == squad_id)
            .unwrap();
        assert_eq!(squad.target, Some(GridPosition { x: 6, y: 0 }));
    }

    #[test]
    fn issue95_missing_priority_retargets_capture_then_releases() {
        let (mut world, attacker, priority, transport, capture) = deployment_world();
        let mut manager = SquadManager::default();
        prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::new());
        world.despawn(priority);
        prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::new());
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
            prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::new());
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
        world.spawn((
            Faction(PlayerId(2)),
            GridPosition { x: 1, y: 0 },
            combat_stats(UnitType::Infantry, true, 0),
        ));
        let deployment = AssignedDeployment {
            entity: attacker,
            intent: PendingDeployment {
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
    }
}
