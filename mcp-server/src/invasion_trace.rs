use std::collections::HashMap;

use bevy_ecs::event::EventCursor;
use bevy_ecs::prelude::*;
use serde::Serialize;

use engine::ai::emergency::{CriticalSiteThreatKind, EmergencyMissionPlan, EmergencyResponse};
use engine::ai::idle_audit::IdleAuditDiagnostics;
use engine::ai::island_campaign::{
    IslandCampaignAssessment, IslandCampaignAssignment, IslandCampaignDecision,
    IslandCampaignDiagnostics, IslandCampaignPortfolio, IslandCampaignRequirement,
    IslandCampaignState,
};
use engine::ai::islands::IslandMap;
use engine::ai::squad::{MissionPhase, MissionType, SquadManager};
use engine::ai::v4::deployment::V4DeploymentRegistry;
use engine::ai::v4::trace::{ProductionDecision, ProductionTraceDiagnostics};
use engine::components::{CargoCapacity, Faction, GridPosition, Health, PlayerId, UnitStats};
use engine::events::{
    PropertyCaptureProgressedEvent, UnitAttackedEvent, UnitLoadedEvent, UnitProducedEvent,
    UnitUnloadedEvent,
};
use engine::resources::UnitType;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitTraceSnapshot {
    pub player_id: u32,
    pub unit_type: UnitType,
    pub health: u32,
    pub max_health: u32,
    pub cost: u32,
    pub max_cargo: u32,
    pub can_capture: bool,
    pub position: GridPosition,
}

pub fn calculate_damage_value(
    before: &UnitTraceSnapshot,
    after: Option<&UnitTraceSnapshot>,
) -> i64 {
    // 事後に存在しない場合は撃破済みなので、攻撃前の残存 HP 全体を損害とする。
    let after_health = after.map_or(0, |snapshot| snapshot.health);
    let lost_hp = before.health.saturating_sub(after_health);
    i64::from(before.cost) * i64::from(lost_hp) / i64::from(before.max_health.max(1))
}

pub fn snapshot_units(world: &mut World) -> HashMap<u64, UnitTraceSnapshot> {
    let mut query = world.query::<(Entity, &Faction, &GridPosition, &Health, &UnitStats)>();
    query
        .iter(world)
        .map(|(entity, faction, position, health, stats)| {
            (
                entity.to_bits(),
                UnitTraceSnapshot {
                    player_id: faction.0.0,
                    unit_type: stats.unit_type,
                    health: health.current,
                    max_health: health.max,
                    cost: stats.cost,
                    max_cargo: stats.max_cargo,
                    can_capture: stats.can_capture,
                    position: *position,
                },
            )
        })
        .collect()
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InvasionEvent {
    UnitLoaded {
        turn: u32,
        player_id: u32,
        step: usize,
        transport_id: u64,
        cargo_id: u64,
        x: usize,
        y: usize,
        island_id: Option<usize>,
    },
    UnitUnloaded {
        turn: u32,
        player_id: u32,
        step: usize,
        transport_id: u64,
        cargo_id: u64,
        unit_type: String,
        can_capture: bool,
        x: usize,
        y: usize,
        island_id: Option<usize>,
    },
    UnitProduced {
        turn: u32,
        player_id: u32,
        step: usize,
        unit_id: u64,
        unit_type: String,
        cost: u32,
        max_cargo: u32,
        can_capture: bool,
        x: usize,
        y: usize,
    },
    UnitAttacked {
        turn: u32,
        player_id: u32,
        step: usize,
        attacker_id: u64,
        attacker_player_id: u32,
        attacker_unit_type: String,
        defender_id: u64,
        defender_player_id: u32,
        defender_unit_type: String,
        damage_value_dealt: i64,
        counter_value_received: i64,
    },
    PropertyCaptureProgressed {
        turn: u32,
        player_id: u32,
        step: usize,
        unit_id: u64,
        x: usize,
        y: usize,
        island_id: Option<usize>,
        previous_capture_points: u32,
        remaining_capture_points: u32,
        completed: bool,
    },
}

#[derive(Debug, Serialize)]
pub struct TransportSquadSnapshot {
    pub squad_id: u32,
    pub player_id: Option<u32>,
    pub phase: String,
    pub transport_id: u64,
    pub x: Option<usize>,
    pub y: Option<usize>,
    pub target_island_id: Option<usize>,
    pub planned_cargo_ids: Vec<u64>,
    pub loaded_cargo_ids: Vec<u64>,
}

/// 遊兵1体分の記録。分類は排他ではない（actionable は no_mission / mission_stalled の上位集合）。
#[derive(Debug, Serialize)]
pub struct IdleUnitSnapshot {
    pub unit_id: u64,
    pub unit_type: UnitType,
    pub x: usize,
    pub y: usize,
    pub squad_id: Option<u32>,
    /// 分類A: どのSquadにも属さない。
    pub no_mission: bool,
    /// 分類B: Squadには属するがそのターン一度も行動しなかった。
    pub mission_stalled: bool,
    /// 分類C: 行動可能なままターンを終えた。
    pub actionable: bool,
}

/// #76: 生産口封鎖に対して実際に割り当てられた解除任務。
#[derive(Debug, Serialize)]
pub struct FactoryReliefMissionSnapshot {
    pub assigned_entity: u64,
    pub threat_entity: u64,
    pub site_x: usize,
    pub site_y: usize,
    pub site_terrain: String,
    pub response: String,
}

pub fn snapshot_factory_relief_plan(
    world: &World,
    player_id: PlayerId,
) -> Vec<FactoryReliefMissionSnapshot> {
    let Some(plan) = world.get_resource::<EmergencyMissionPlan>() else {
        return Vec::new();
    };
    plan.missions
        .iter()
        .filter(|mission| {
            mission.owner_id == player_id
                && mission.threat.kind == CriticalSiteThreatKind::ProductionBlockade
        })
        .map(|mission| FactoryReliefMissionSnapshot {
            assigned_entity: mission.assigned_entity.to_bits(),
            threat_entity: mission.threat.threat_entity.to_bits(),
            site_x: mission.threat.site_position.x,
            site_y: mission.threat.site_position.y,
            site_terrain: mission.threat.site_terrain.as_str().to_string(),
            response: match mission.response {
                EmergencyResponse::EliminateThreat => "eliminate",
                EmergencyResponse::OccupySite => "occupy",
                EmergencyResponse::BlockRoute => "block_route",
            }
            .to_string(),
        })
        .collect()
}

/// Squad 単位のダイジェスト。分類D（停滞Squad）はこの列のターン間差分で判定する。
#[derive(Debug, Serialize)]
pub struct IdleSquadSnapshot {
    pub squad_id: u32,
    pub mission_type: String,
    pub phase: String,
    pub target_x: Option<usize>,
    pub target_y: Option<usize>,
    pub target_island_id: Option<usize>,
    pub member_count: usize,
    pub acted_count: usize,
}

/// 「遊兵ゼロ」指標のターン単位スナップショット。
#[derive(Debug, Serialize)]
pub struct IdleAuditSnapshot {
    pub player_id: u32,
    /// 母数（盤上に実体がある自軍ユニット数）。
    pub total_units: usize,
    /// 分類A: 任務なし。
    pub no_mission_count: usize,
    /// 分類B: 任務はあるが命令が出ない。
    pub mission_stalled_count: usize,
    /// 分類C: 行動可能なまま終了。
    pub actionable_count: usize,
    pub units: Vec<IdleUnitSnapshot>,
    pub squads: Vec<IdleSquadSnapshot>,
}

/// 生産ループ1反復分。`decision` が "produced" 以外なら unit_type/cost は補助情報。
#[derive(Debug, Serialize)]
pub struct ProductionStepSnapshot {
    pub operation_kind: String,
    pub anchor_x: usize,
    pub anchor_y: usize,
    pub slot_kind: String,
    pub deficit_before: f32,
    pub deficit_after: f32,
    pub remaining_funds_before: u32,
    /// "produced" | "slot_cleared" | "deferred"
    pub decision: String,
    pub unit_type: Option<UnitType>,
    pub cost: Option<u32>,
    pub facility_x: Option<usize>,
    pub facility_y: Option<usize>,
}

/// 作戦1件が要求していた枠。どの枠が発注を駆動したかの突き合わせに使う。
#[derive(Debug, Serialize)]
pub struct ProductionOperationSnapshot {
    pub kind: String,
    pub anchor_x: usize,
    pub anchor_y: usize,
    pub capture_units: u32,
    pub escort_units: u32,
    pub destroy_budget: u32,
    pub transport_slots: u32,
    pub intercept_budget: u32,
    pub requires_transport: bool,
    pub enemy_combat_value: u32,
    pub enemy_reinforcement_budget: u32,
    pub minimum_combat_unit_cost: u32,
    pub friendly_combat_value_committed: u32,
    pub deploy_lead_time: u32,
}

/// V4 生産判断のターン単位スナップショット。
#[derive(Debug, Serialize)]
pub struct ProductionPlanSnapshot {
    pub player_id: u32,
    pub funds: u32,
    pub free_facility_count: usize,
    /// 作戦が立たず fallback に落ちたか
    pub fallback: bool,
    /// 使い切れずに残った資金
    pub leftover_funds: u32,
    pub operations: Vec<ProductionOperationSnapshot>,
    pub steps: Vec<ProductionStepSnapshot>,
}

/// 生産意図から実Entityへ接続された局地任務の実行実績。
#[derive(Debug, Serialize)]
pub struct DeploymentAuditSnapshot {
    pub player_id: u32,
    pub pending_count: usize,
    pub assigned_count: usize,
    pub active_count: usize,
    pub attacked_count: usize,
    pub records: Vec<DeploymentAuditRecordSnapshot>,
}

#[derive(Debug, Serialize)]
pub struct DeploymentAuditRecordSnapshot {
    pub entity_id: u64,
    pub unit_type: UnitType,
    pub slot_kind: String,
    pub anchor_x: usize,
    pub anchor_y: usize,
    pub priority_enemy_ids: Vec<u64>,
    pub squad_id: Option<u32>,
    pub current_target_id: Option<u64>,
    pub active: bool,
    pub assigned_turn: u32,
    pub attack_count: u32,
    pub priority_attack_count: u32,
    pub mission_target_attack_count: u32,
    pub capture_unit_attack_count: u32,
    pub transport_unit_attack_count: u32,
    pub kill_count: u32,
    pub first_attack_turn: Option<u32>,
    pub first_attack_eta: Option<u32>,
}

/// 緊急迎撃が何を守るために、どの戦力をpreemptしたかを示す診断。
#[derive(Debug, Serialize)]
pub struct EmergencyPlanSnapshot {
    pub player_id: u32,
    pub missions: Vec<EmergencyMissionSnapshot>,
}

#[derive(Debug, Serialize)]
pub struct EmergencyMissionSnapshot {
    pub assigned_entity_id: u64,
    pub assigned_unit_type: Option<UnitType>,
    pub threat_entity_id: u64,
    pub threat_unit_type: Option<UnitType>,
    pub threat_x: usize,
    pub threat_y: usize,
    pub site_x: usize,
    pub site_y: usize,
    pub site_terrain: String,
    pub site_owner_id: Option<u32>,
    pub eta: u32,
    pub response: String,
}

#[derive(Debug, Serialize)]
pub struct IslandCampaignSnapshot {
    pub player_id: u32,
    pub islands: Vec<IslandCampaignAssessmentSnapshot>,
    pub active_offensives: Vec<IslandCampaignAssignmentSnapshot>,
    pub defenses: Vec<IslandCampaignAssignmentSnapshot>,
}

#[derive(Debug, Serialize)]
pub struct IslandCampaignAssessmentSnapshot {
    pub island_id: usize,
    pub state: String,
    pub decision: String,
    pub state_reason: String,
    pub decision_reason: String,
    pub neutral_properties: u32,
    pub friendly_properties: u32,
    pub enemy_properties: u32,
    pub friendly_combat_value: u32,
    pub enemy_combat_value: u32,
    pub friendly_arrival_eta: Option<u32>,
    pub enemy_arrival_eta: Option<u32>,
    pub friendly_capture_eta: Option<u32>,
    pub enemy_capture_eta: Option<u32>,
    pub roi_production_sites: u32,
    pub transport_eta: Option<u32>,
    pub expansion_payback_turns: Option<u32>,
    pub required_budget: u32,
    pub allocated_budget: u32,
}

#[derive(Debug, Serialize)]
pub struct IslandCampaignAssignmentSnapshot {
    pub island_id: usize,
    pub decision: String,
    pub target_x: usize,
    pub target_y: usize,
    pub requirement: IslandCampaignRequirementSnapshot,
    pub purchase_shortfall: IslandCampaignRequirementSnapshot,
    pub allocated_budget: u32,
    pub transport_entity_ids: Vec<u64>,
    pub capture_entity_ids: Vec<u64>,
    pub combat_entity_ids: Vec<u64>,
    pub operation_ready: bool,
    pub continued_from_existing_squad: bool,
}

#[derive(Debug, Serialize)]
pub struct IslandCampaignRequirementSnapshot {
    pub preferred_transport: Option<String>,
    pub transport_slots: u32,
    pub capture_units: u32,
    pub combat_budget: u32,
    pub total_budget: u32,
}

pub struct InvasionTraceCollector {
    loaded_cursor: EventCursor<UnitLoadedEvent>,
    unloaded_cursor: EventCursor<UnitUnloadedEvent>,
    attacked_cursor: EventCursor<UnitAttackedEvent>,
    produced_cursor: EventCursor<UnitProducedEvent>,
    capture_cursor: EventCursor<PropertyCaptureProgressedEvent>,
}

impl InvasionTraceCollector {
    pub fn new(world: &World) -> Self {
        Self {
            loaded_cursor: world.resource::<Events<UnitLoadedEvent>>().get_cursor(),
            unloaded_cursor: world.resource::<Events<UnitUnloadedEvent>>().get_cursor(),
            attacked_cursor: world.resource::<Events<UnitAttackedEvent>>().get_cursor(),
            produced_cursor: world.resource::<Events<UnitProducedEvent>>().get_cursor(),
            capture_cursor: world
                .resource::<Events<PropertyCaptureProgressedEvent>>()
                .get_cursor(),
        }
    }

    /// 各シミュレーションステップにおける侵攻関連イベントを収集します。
    pub fn collect_step(
        &mut self,
        world: &mut World,
        turn: u32,
        player_id: u32,
        step: usize,
        units_before: &HashMap<u64, UnitTraceSnapshot>,
    ) -> Vec<InvasionEvent> {
        let units_after = snapshot_units(world);
        let mut result = Vec::new();
        let island_map = world.resource::<IslandMap>();

        // 積載後はカーゴが盤面から外れるため、行動前スナップショットから位置を取得する。
        for event in self
            .loaded_cursor
            .read(world.resource::<Events<UnitLoadedEvent>>())
        {
            let position = units_before
                .get(&event.cargo.to_bits())
                .map(|unit| unit.position);
            result.push(InvasionEvent::UnitLoaded {
                turn,
                player_id,
                step,
                transport_id: event.transport.to_bits(),
                cargo_id: event.cargo.to_bits(),
                x: position.map_or(0, |position| position.x),
                y: position.map_or(0, |position| position.y),
                island_id: position.and_then(|position| {
                    island_map
                        .get_island_at(&position)
                        .map(|island| island.id.0)
                }),
            });
        }

        for event in self
            .unloaded_cursor
            .read(world.resource::<Events<UnitUnloadedEvent>>())
        {
            let Some(cargo) = units_after.get(&event.cargo.to_bits()) else {
                continue;
            };
            result.push(InvasionEvent::UnitUnloaded {
                turn,
                player_id,
                step,
                transport_id: event.transport.to_bits(),
                cargo_id: event.cargo.to_bits(),
                unit_type: cargo.unit_type.as_str().to_string(),
                can_capture: cargo.can_capture,
                x: cargo.position.x,
                y: cargo.position.y,
                island_id: island_map
                    .get_island_at(&cargo.position)
                    .map(|island| island.id.0),
            });
        }

        for event in self
            .produced_cursor
            .read(world.resource::<Events<UnitProducedEvent>>())
        {
            let Some(unit) = units_after.get(&event.entity.to_bits()) else {
                continue;
            };
            result.push(InvasionEvent::UnitProduced {
                turn,
                player_id: event.player_id.0,
                step,
                unit_id: event.entity.to_bits(),
                unit_type: unit.unit_type.as_str().to_string(),
                cost: unit.cost,
                max_cargo: unit.max_cargo,
                can_capture: unit.can_capture,
                x: unit.position.x,
                y: unit.position.y,
            });
        }

        for event in self
            .attacked_cursor
            .read(world.resource::<Events<UnitAttackedEvent>>())
        {
            let Some(attacker_before) = units_before.get(&event.attacker.to_bits()) else {
                continue;
            };
            let Some(defender_before) = units_before.get(&event.defender.to_bits()) else {
                continue;
            };
            result.push(InvasionEvent::UnitAttacked {
                turn,
                player_id,
                step,
                attacker_id: event.attacker.to_bits(),
                attacker_player_id: attacker_before.player_id,
                attacker_unit_type: attacker_before.unit_type.as_str().to_string(),
                defender_id: event.defender.to_bits(),
                defender_player_id: defender_before.player_id,
                defender_unit_type: defender_before.unit_type.as_str().to_string(),
                damage_value_dealt: calculate_damage_value(
                    defender_before,
                    units_after.get(&event.defender.to_bits()),
                ),
                counter_value_received: calculate_damage_value(
                    attacker_before,
                    units_after.get(&event.attacker.to_bits()),
                ),
            });
        }

        for event in self
            .capture_cursor
            .read(world.resource::<Events<PropertyCaptureProgressedEvent>>())
        {
            let position = GridPosition {
                x: event.x,
                y: event.y,
            };
            result.push(InvasionEvent::PropertyCaptureProgressed {
                turn,
                player_id,
                step,
                unit_id: event.unit.to_bits(),
                x: event.x,
                y: event.y,
                island_id: island_map
                    .get_island_at(&position)
                    .map(|island| island.id.0),
                previous_capture_points: event.previous_capture_points,
                remaining_capture_points: event.remaining_capture_points,
                completed: event.completed,
            });
        }
        result
    }
}

fn island_campaign_state_name(state: IslandCampaignState) -> &'static str {
    match state {
        IslandCampaignState::Ignored => "Ignored",
        IslandCampaignState::OpenNeutral => "OpenNeutral",
        IslandCampaignState::Secured => "Secured",
        IslandCampaignState::Threatened => "Threatened",
        IslandCampaignState::Contested => "Contested",
        IslandCampaignState::EnemyHeld => "EnemyHeld",
    }
}

fn island_campaign_decision_name(decision: IslandCampaignDecision) -> &'static str {
    match decision {
        IslandCampaignDecision::Observe => "Observe",
        IslandCampaignDecision::Expand => "Expand",
        IslandCampaignDecision::Secure => "Secure",
        IslandCampaignDecision::Defend => "Defend",
        IslandCampaignDecision::Contest => "Contest",
        IslandCampaignDecision::Reinforce => "Reinforce",
        IslandCampaignDecision::Withdraw => "Withdraw",
        IslandCampaignDecision::Assault => "Assault",
    }
}

fn island_campaign_unit_type_name(unit_type: UnitType) -> &'static str {
    // 表示用の日本語master名ではなく、評価スクリプトが安定して比較できる識別子を返す。
    match unit_type {
        UnitType::Infantry => "Infantry",
        UnitType::Mech => "Mech",
        UnitType::Recon => "Recon",
        UnitType::Tank => "Tank",
        UnitType::MdTank => "MdTank",
        UnitType::TankZ => "TankZ",
        UnitType::Artillery => "Artillery",
        UnitType::LightSpGun => "LightSpGun",
        UnitType::HeavySpGun => "HeavySpGun",
        UnitType::Rockets => "Rockets",
        UnitType::AntiAir => "AntiAir",
        UnitType::Missiles => "Missiles",
        UnitType::Fighter => "Fighter",
        UnitType::HeavyFighter => "HeavyFighter",
        UnitType::Bomber => "Bomber",
        UnitType::Bcopters => "Bcopters",
        UnitType::TransportHelicopter => "TransportHelicopter",
        UnitType::Battleship => "Battleship",
        UnitType::Carrier => "Carrier",
        UnitType::Lander => "Lander",
        UnitType::SupplyTruck => "SupplyTruck",
    }
}

fn snapshot_campaign_assessment(
    assessment: &IslandCampaignAssessment,
) -> IslandCampaignAssessmentSnapshot {
    IslandCampaignAssessmentSnapshot {
        island_id: assessment.island_id.0,
        state: island_campaign_state_name(assessment.state).to_string(),
        decision: island_campaign_decision_name(assessment.decision).to_string(),
        state_reason: assessment.state_reason.clone(),
        decision_reason: assessment.decision_reason.clone(),
        neutral_properties: assessment.neutral_properties,
        friendly_properties: assessment.friendly_properties,
        enemy_properties: assessment.enemy_properties,
        friendly_combat_value: assessment.friendly_combat_value,
        enemy_combat_value: assessment.enemy_combat_value,
        friendly_arrival_eta: assessment.friendly_arrival_eta,
        enemy_arrival_eta: assessment.enemy_arrival_eta,
        friendly_capture_eta: assessment.friendly_capture_eta,
        enemy_capture_eta: assessment.enemy_capture_eta,
        roi_production_sites: assessment.roi_production_sites,
        transport_eta: assessment.transport_eta,
        expansion_payback_turns: assessment.expansion_payback_turns,
        required_budget: assessment.required_budget,
        allocated_budget: assessment.allocated_budget,
    }
}

fn snapshot_campaign_requirement(
    requirement: &IslandCampaignRequirement,
) -> IslandCampaignRequirementSnapshot {
    IslandCampaignRequirementSnapshot {
        preferred_transport: requirement
            .preferred_transport
            .map(|unit_type| island_campaign_unit_type_name(unit_type).to_string()),
        transport_slots: requirement.transport_slots,
        capture_units: requirement.capture_units,
        combat_budget: requirement.combat_budget,
        total_budget: requirement.total_budget,
    }
}

fn snapshot_campaign_assignment(
    assignment: &IslandCampaignAssignment,
) -> IslandCampaignAssignmentSnapshot {
    let mut transport_entity_ids: Vec<_> = assignment
        .transport_entities
        .iter()
        .map(|entity| entity.to_bits())
        .collect();
    let mut capture_entity_ids: Vec<_> = assignment
        .capture_entities
        .iter()
        .map(|entity| entity.to_bits())
        .collect();
    let mut combat_entity_ids: Vec<_> = assignment
        .combat_entities
        .iter()
        .map(|entity| entity.to_bits())
        .collect();
    // ECSのspawn順やallocator内部順をテレメトリの配列順へ漏らさない。
    transport_entity_ids.sort_unstable();
    capture_entity_ids.sort_unstable();
    combat_entity_ids.sort_unstable();

    IslandCampaignAssignmentSnapshot {
        island_id: assignment.island_id.0,
        decision: island_campaign_decision_name(assignment.decision).to_string(),
        target_x: assignment.target_position.x,
        target_y: assignment.target_position.y,
        requirement: snapshot_campaign_requirement(&assignment.requirement),
        purchase_shortfall: snapshot_campaign_requirement(&assignment.purchase_shortfall),
        allocated_budget: assignment.allocated_budget,
        transport_entity_ids,
        capture_entity_ids,
        combat_entity_ids,
        operation_ready: assignment.operation_ready,
        continued_from_existing_squad: assignment.continued_from_existing_squad,
    }
}

pub fn snapshot_island_campaign(
    player_id: PlayerId,
    portfolio: &IslandCampaignPortfolio,
) -> IslandCampaignSnapshot {
    let mut islands: Vec<_> = portfolio
        .islands
        .iter()
        .map(snapshot_campaign_assessment)
        .collect();
    // assignment列はallocatorの優先順位そのものなので、島IDで並べ替えず保持する。
    let active_offensives: Vec<_> = portfolio
        .active_offensives
        .iter()
        .map(snapshot_campaign_assignment)
        .collect();
    let defenses: Vec<_> = portfolio
        .defenses
        .iter()
        .map(snapshot_campaign_assignment)
        .collect();
    islands.sort_by_key(|island| island.island_id);

    IslandCampaignSnapshot {
        player_id: player_id.0,
        islands,
        active_offensives,
        defenses,
    }
}

pub fn snapshot_island_campaign_for_player(
    world: &World,
    player_id: PlayerId,
) -> Option<IslandCampaignSnapshot> {
    if !engine::ai::resolve_player_ai_version(world, player_id).uses_v3_tactics() {
        return None;
    }
    world
        .get_resource::<IslandCampaignDiagnostics>()?
        .by_player
        .get(&player_id)
        .map(|portfolio| snapshot_island_campaign(player_id, portfolio))
}

/// 直近ターンの遊兵計測結果（engine 側の `IdleAuditDiagnostics`）をトレース用に写し取る。
/// 判定ロジックは engine に閉じており、ここでは形式変換だけを行う。
pub fn snapshot_idle_audit_for_player(
    world: &World,
    player_id: PlayerId,
) -> Option<IdleAuditSnapshot> {
    let audit = world
        .get_resource::<IdleAuditDiagnostics>()?
        .by_player
        .get(&player_id)?;

    let units = audit
        .records
        .iter()
        .map(|record| IdleUnitSnapshot {
            unit_id: record.entity.to_bits(),
            unit_type: record.unit_type,
            x: record.position.x,
            y: record.position.y,
            squad_id: record.squad_id.map(|id| id.0),
            no_mission: record.no_mission,
            mission_stalled: record.mission_stalled,
            actionable: record.actionable,
        })
        .collect();

    let squads = audit
        .squads
        .iter()
        .map(|digest| IdleSquadSnapshot {
            squad_id: digest.squad_id.0,
            mission_type: format!("{:?}", digest.mission_type),
            phase: format!("{:?}", digest.phase),
            target_x: digest.target.map(|target| target.x),
            target_y: digest.target.map(|target| target.y),
            target_island_id: digest.target_island.map(|island| island.0),
            member_count: digest.member_count,
            acted_count: digest.acted_count,
        })
        .collect();

    Some(IdleAuditSnapshot {
        player_id: audit.player_id.0,
        total_units: audit.total_units,
        no_mission_count: audit.no_mission_count(),
        mission_stalled_count: audit.mission_stalled_count(),
        actionable_count: audit.actionable_count(),
        units,
        squads,
    })
}

/// 直近ターンの V4 生産判断トレースを写し取る。V1〜V3 では記録が無いため None を返す。
pub fn snapshot_production_plan_for_player(
    world: &World,
    player_id: PlayerId,
) -> Option<ProductionPlanSnapshot> {
    let plan = world
        .get_resource::<ProductionTraceDiagnostics>()?
        .by_player
        .get(&player_id)?;

    let operations = plan
        .operations
        .iter()
        .map(|op| ProductionOperationSnapshot {
            kind: format!("{:?}", op.kind),
            anchor_x: op.anchor.x,
            anchor_y: op.anchor.y,
            capture_units: op.slots.capture_units,
            escort_units: op.slots.escort_units,
            destroy_budget: op.slots.destroy_budget,
            transport_slots: op.slots.transport_slots,
            intercept_budget: op.slots.intercept_budget,
            requires_transport: op.requires_transport,
            enemy_combat_value: op.enemy_combat_value,
            enemy_reinforcement_budget: op.enemy_reinforcement_budget,
            minimum_combat_unit_cost: op.minimum_combat_unit_cost,
            friendly_combat_value_committed: op.friendly_combat_value_committed,
            deploy_lead_time: op.deploy_lead_time,
        })
        .collect();

    let steps = plan
        .steps
        .iter()
        .map(|step| {
            // 結末ごとに埋まるフィールドが違うため、ここで平坦化する。
            let (decision, unit_type, cost, facility) = match &step.decision {
                ProductionDecision::Produced {
                    unit_type,
                    cost,
                    facility,
                } => ("produced", Some(*unit_type), Some(*cost), Some(*facility)),
                ProductionDecision::SlotCleared => ("slot_cleared", None, None, None),
                ProductionDecision::Deferred { unit_type, cost } => {
                    ("deferred", Some(*unit_type), Some(*cost), None)
                }
            };
            ProductionStepSnapshot {
                operation_kind: format!("{:?}", step.operation_kind),
                anchor_x: step.operation_anchor.x,
                anchor_y: step.operation_anchor.y,
                slot_kind: format!("{:?}", step.slot_kind),
                deficit_before: step.deficit_before,
                deficit_after: step.deficit_after,
                remaining_funds_before: step.remaining_funds_before,
                decision: decision.to_string(),
                unit_type,
                cost,
                facility_x: facility.map(|pos| pos.x),
                facility_y: facility.map(|pos| pos.y),
            }
        })
        .collect();

    Some(ProductionPlanSnapshot {
        player_id: plan.player_id.0,
        funds: plan.funds,
        free_facility_count: plan.free_facility_count,
        fallback: plan.fallback,
        leftover_funds: plan.leftover_funds,
        operations,
        steps,
    })
}

/// V4の発注意図が実Entityへ接続され、攻撃まで進んだかを写し取る。
pub fn snapshot_deployment_audit_for_player(
    world: &World,
    player_id: PlayerId,
) -> Option<DeploymentAuditSnapshot> {
    let registry = world.get_resource::<V4DeploymentRegistry>()?;
    let records = registry
        .audit_records(player_id)
        .into_iter()
        .map(|record| DeploymentAuditRecordSnapshot {
            entity_id: record.entity.to_bits(),
            unit_type: record.unit_type,
            slot_kind: format!("{:?}", record.slot_kind),
            anchor_x: record.anchor.x,
            anchor_y: record.anchor.y,
            priority_enemy_ids: record
                .priority_enemies
                .into_iter()
                .map(Entity::to_bits)
                .collect(),
            squad_id: record.squad_id.map(|id| id.0),
            current_target_id: record.current_target.map(Entity::to_bits),
            active: record.active,
            assigned_turn: record.assigned_turn,
            attack_count: record.attack_count,
            priority_attack_count: record.priority_attack_count,
            mission_target_attack_count: record.mission_target_attack_count,
            capture_unit_attack_count: record.capture_unit_attack_count,
            transport_unit_attack_count: record.transport_unit_attack_count,
            kill_count: record.kill_count,
            first_attack_turn: record.first_attack_turn,
            first_attack_eta: record
                .first_attack_turn
                .map(|turn| turn.saturating_sub(record.assigned_turn)),
        })
        .collect::<Vec<_>>();
    Some(DeploymentAuditSnapshot {
        player_id: player_id.0,
        pending_count: registry.pending_count(player_id),
        assigned_count: records.len(),
        active_count: records.iter().filter(|record| record.active).count(),
        attacked_count: records
            .iter()
            .filter(|record| record.attack_count > 0)
            .count(),
        records,
    })
}

/// 現在手番の緊急迎撃について、対象拠点の所有者も含めて写し取る。
pub fn snapshot_emergency_plan_for_player(
    world: &World,
    player_id: PlayerId,
) -> Option<EmergencyPlanSnapshot> {
    let plan = world.get_resource::<EmergencyMissionPlan>()?;
    let missions = plan
        .missions
        .iter()
        .filter(|mission| mission.owner_id == player_id)
        .map(|mission| EmergencyMissionSnapshot {
            assigned_entity_id: mission.assigned_entity.to_bits(),
            assigned_unit_type: world
                .get::<UnitStats>(mission.assigned_entity)
                .map(|stats| stats.unit_type),
            threat_entity_id: mission.threat.threat_entity.to_bits(),
            threat_unit_type: world
                .get::<UnitStats>(mission.threat.threat_entity)
                .map(|stats| stats.unit_type),
            threat_x: mission.threat.threat_position.x,
            threat_y: mission.threat.threat_position.y,
            site_x: mission.threat.site_position.x,
            site_y: mission.threat.site_position.y,
            site_terrain: format!("{:?}", mission.threat.site_terrain),
            site_owner_id: mission.threat.site_owner_id.map(|owner| owner.0),
            eta: mission.threat.eta,
            response: format!("{:?}", mission.response),
        })
        .collect();
    Some(EmergencyPlanSnapshot {
        player_id: player_id.0,
        missions,
    })
}

pub fn snapshot_transport_squads(world: &World) -> Vec<TransportSquadSnapshot> {
    let Some(manager) = world.get_resource::<SquadManager>() else {
        return Vec::new();
    };
    let mut snapshots = Vec::new();
    for squad in &manager.squads {
        if squad.mission_type != MissionType::Transport {
            continue;
        }
        let Some(transport) = squad.transport_entity else {
            continue;
        };
        let phase = match squad.phase {
            MissionPhase::Transport(phase) => format!("{phase:?}"),
            _ => continue,
        };
        let position = world.get::<GridPosition>(transport).copied();
        let mut planned_cargo_ids: Vec<_> = squad
            .cargo_entities
            .iter()
            .map(|entity| entity.to_bits())
            .collect();
        planned_cargo_ids.sort_unstable();
        let mut loaded_cargo_ids: Vec<_> = world
            .get::<CargoCapacity>(transport)
            .map(|capacity| {
                capacity
                    .loaded
                    .iter()
                    .map(|entity| entity.to_bits())
                    .collect()
            })
            .unwrap_or_default();
        loaded_cargo_ids.sort_unstable();
        snapshots.push(TransportSquadSnapshot {
            squad_id: squad.id.0,
            player_id: world.get::<Faction>(transport).map(|faction| faction.0.0),
            phase,
            transport_id: transport.to_bits(),
            x: position.map(|position| position.x),
            y: position.map(|position| position.y),
            target_island_id: squad.target_island.map(|island| island.0),
            planned_cargo_ids,
            loaded_cargo_ids,
        });
    }
    snapshots.sort_by_key(|snapshot| snapshot.squad_id);
    snapshots
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai::island_campaign::{
        IslandCampaignAssessment, IslandCampaignAssignment, IslandCampaignDecision,
        IslandCampaignPortfolio, IslandCampaignRequirement, IslandCampaignState,
    };
    use engine::ai::islands::IslandId;
    use engine::components::PlayerId;
    use engine::resources::UnitType;

    fn snapshot(health: u32, cost: u32) -> UnitTraceSnapshot {
        UnitTraceSnapshot {
            player_id: 1,
            unit_type: UnitType::Battleship,
            health,
            max_health: 100,
            cost,
            max_cargo: 0,
            can_capture: false,
            position: GridPosition { x: 1, y: 1 },
        }
    }

    #[test]
    fn damage_value_uses_hp_loss_and_unit_cost() {
        let before = snapshot(100, 28_000);
        let after = snapshot(75, 28_000);
        assert_eq!(7_000, calculate_damage_value(&before, Some(&after)));
    }

    #[test]
    fn destroyed_unit_counts_remaining_hp_as_loss() {
        let before = snapshot(40, 28_000);
        assert_eq!(11_200, calculate_damage_value(&before, None));
    }

    #[test]
    fn healing_or_unchanged_hp_never_creates_negative_damage() {
        let before = snapshot(50, 28_000);
        let after = snapshot(80, 28_000);
        assert_eq!(0, calculate_damage_value(&before, Some(&after)));
    }

    #[test]
    fn serializes_island_campaign_snapshot() {
        let assessment = IslandCampaignAssessment {
            island_id: IslandId(3),
            state: IslandCampaignState::OpenNeutral,
            decision: IslandCampaignDecision::Expand,
            state_reason: "中立拠点が残っている".to_string(),
            decision_reason: "投資回収可能なため拡張する".to_string(),
            pause_cause: None,
            neutral_properties: 2,
            friendly_properties: 1,
            enemy_properties: 0,
            friendly_combat_value: 4_000,
            enemy_combat_value: 0,
            friendly_arrival_eta: Some(2),
            enemy_arrival_eta: None,
            friendly_capture_eta: Some(4),
            enemy_capture_eta: None,
            roi_production_sites: 2,
            transport_eta: Some(1),
            expansion_payback_turns: Some(6),
            required_budget: 6_000,
            allocated_budget: 5_000,
        };
        let transport_entities = vec![Entity::from_raw(9), Entity::from_raw(2)];
        let capture_entities = vec![Entity::from_raw(7), Entity::from_raw(1)];
        let combat_entities = vec![Entity::from_raw(6), Entity::from_raw(4)];
        let assignment = IslandCampaignAssignment {
            island_id: IslandId(3),
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 8, y: 4 },
            capture_target_positions: vec![GridPosition { x: 8, y: 4 }],
            requirement: IslandCampaignRequirement {
                preferred_transport: Some(UnitType::TransportHelicopter),
                transport_slots: 2,
                capture_units: 2,
                combat_budget: 0,
                total_budget: 6_000,
            },
            purchase_shortfall: IslandCampaignRequirement {
                preferred_transport: Some(UnitType::TransportHelicopter),
                transport_slots: 1,
                capture_units: 1,
                combat_budget: 0,
                total_budget: 3_000,
            },
            allocated_budget: 5_000,
            transport_entities: transport_entities.clone(),
            capture_entities: capture_entities.clone(),
            combat_entities: combat_entities.clone(),
            operation_ready: false,
            continued_from_existing_squad: true,
        };
        let mut lower_priority_assignment = assignment.clone();
        lower_priority_assignment.island_id = IslandId(1);
        let portfolio = IslandCampaignPortfolio {
            islands: vec![assessment],
            active_offensives: vec![assignment, lower_priority_assignment],
            defenses: Vec::new(),
        };

        let value = serde_json::to_value(snapshot_island_campaign(PlayerId(1), &portfolio))
            .expect("島キャンペーン診断をJSONへ変換できること");
        let island = &value["islands"][0];
        for key in [
            "island_id",
            "state",
            "decision",
            "state_reason",
            "decision_reason",
            "neutral_properties",
            "friendly_properties",
            "enemy_properties",
            "friendly_combat_value",
            "enemy_combat_value",
            "friendly_arrival_eta",
            "enemy_arrival_eta",
            "friendly_capture_eta",
            "enemy_capture_eta",
            "roi_production_sites",
            "transport_eta",
            "expansion_payback_turns",
            "required_budget",
            "allocated_budget",
        ] {
            assert!(island.get(key).is_some(), "missing island key: {key}");
        }
        assert_eq!(island["state"], "OpenNeutral");
        assert_eq!(island["decision"], "Expand");
        assert_eq!(island["roi_production_sites"], 2);
        assert_eq!(island["transport_eta"], 1);

        assert_eq!(value["active_offensives"][0]["island_id"], 3);
        assert_eq!(value["active_offensives"][1]["island_id"], 1);
        let assignment = &value["active_offensives"][0];
        for key in [
            "island_id",
            "decision",
            "allocated_budget",
            "transport_entity_ids",
            "capture_entity_ids",
            "combat_entity_ids",
            "purchase_shortfall",
            "operation_ready",
            "continued_from_existing_squad",
        ] {
            assert!(
                assignment.get(key).is_some(),
                "missing assignment key: {key}"
            );
        }
        let sorted_bits = |entities: &[Entity]| {
            let mut ids: Vec<_> = entities.iter().map(|entity| entity.to_bits()).collect();
            ids.sort_unstable();
            ids
        };
        assert_eq!(
            assignment["purchase_shortfall"]["preferred_transport"],
            "TransportHelicopter"
        );
        assert_eq!(
            assignment["transport_entity_ids"],
            serde_json::json!(sorted_bits(&transport_entities))
        );
        assert_eq!(
            assignment["capture_entity_ids"],
            serde_json::json!(sorted_bits(&capture_entities))
        );
        assert_eq!(
            assignment["combat_entity_ids"],
            serde_json::json!(sorted_bits(&combat_entities))
        );
    }

    #[test]
    fn missing_island_campaign_diagnostics_returns_none() {
        let mut world = World::new();
        assert!(snapshot_island_campaign_for_player(&world, PlayerId(1)).is_none());

        let mut diagnostics = IslandCampaignDiagnostics::default();
        diagnostics
            .by_player
            .insert(PlayerId(2), IslandCampaignPortfolio::default());
        world.insert_resource(diagnostics);

        assert!(snapshot_island_campaign_for_player(&world, PlayerId(1)).is_none());
        assert!(snapshot_island_campaign_for_player(&world, PlayerId(2)).is_some());

        let mut settings = engine::ai::PlayerAiSettings::new();
        settings.set_version(PlayerId(2), engine::ai::AiVersion::V1);
        world.insert_resource(settings);
        assert!(snapshot_island_campaign_for_player(&world, PlayerId(2)).is_none());

        world
            .resource_mut::<engine::ai::PlayerAiSettings>()
            .set_version(PlayerId(2), engine::ai::AiVersion::V3);
        assert!(snapshot_island_campaign_for_player(&world, PlayerId(2)).is_some());
    }
}
