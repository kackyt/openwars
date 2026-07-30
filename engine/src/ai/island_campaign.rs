use crate::ai::islands::IslandId;
use crate::ai::squad::TransportPhase;
use crate::components::{GridPosition, PlayerId};
use crate::resources::UnitType;
use bevy_ecs::prelude::{Entity, Resource};
use std::cmp::Reverse;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IslandCampaignState {
    Ignored,
    OpenNeutral,
    Secured,
    Threatened,
    Contested,
    EnemyHeld,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IslandCampaignDecision {
    Observe,
    Expand,
    Secure,
    Defend,
    Contest,
    Reinforce,
    Withdraw,
    Assault,
}

/// その分析呼び出しで作戦を停止した制御上の理由。診断文言とは分離して扱う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IslandCampaignPauseCause {
    DefensePreemption,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignFacts {
    pub island_id: IslandId,
    pub capturable_properties: u32,
    pub strategic_production_sites: u32,
    pub roi_production_sites: u32,
    pub neutral_properties: u32,
    pub friendly_properties: u32,
    pub enemy_properties: u32,
    pub friendly_units: u32,
    pub enemy_units: u32,
    pub friendly_combat_value: u32,
    pub enemy_combat_value: u32,
    pub friendly_arrival_eta: Option<u32>,
    pub enemy_arrival_eta: Option<u32>,
    pub friendly_capture_eta: Option<u32>,
    pub enemy_capture_eta: Option<u32>,
    pub transport_eta: Option<u32>,
    pub capture_turns: u32,
    pub island_income_per_turn: u32,
    pub missing_expansion_package_cost: u32,
    pub reachable: bool,
    pub has_unowned_properties: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignAssessment {
    pub island_id: IslandId,
    pub state: IslandCampaignState,
    pub decision: IslandCampaignDecision,
    pub state_reason: String,
    pub decision_reason: String,
    pub pause_cause: Option<IslandCampaignPauseCause>,
    pub neutral_properties: u32,
    pub friendly_properties: u32,
    pub enemy_properties: u32,
    pub friendly_combat_value: u32,
    pub enemy_combat_value: u32,
    pub friendly_arrival_eta: Option<u32>,
    pub enemy_arrival_eta: Option<u32>,
    pub friendly_capture_eta: Option<u32>,
    pub enemy_capture_eta: Option<u32>,
    pub expansion_payback_turns: Option<u32>,
    pub required_budget: u32,
    pub allocated_budget: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignRequirement {
    pub preferred_transport: Option<UnitType>,
    pub transport_slots: u32,
    pub capture_units: u32,
    pub combat_budget: u32,
    pub total_budget: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignAssignment {
    pub island_id: IslandId,
    pub decision: IslandCampaignDecision,
    pub target_position: GridPosition,
    pub requirement: IslandCampaignRequirement,
    pub purchase_shortfall: IslandCampaignRequirement,
    pub allocated_budget: u32,
    pub transport_entities: Vec<Entity>,
    pub capture_entities: Vec<Entity>,
    pub combat_entities: Vec<Entity>,
    pub operation_ready: bool,
    pub continued_from_existing_squad: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignShortfall {
    pub island_id: IslandId,
    pub decision: IslandCampaignDecision,
    pub light_transport_slots: u32,
    pub heavy_transport_slots: u32,
    pub capture_units: u32,
    pub combat_budget: u32,
    pub reserved_budget: u32,
    pub priority_rank: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IslandCampaignPortfolio {
    pub islands: Vec<IslandCampaignAssessment>,
    pub active_offensives: Vec<IslandCampaignAssignment>,
    pub defenses: Vec<IslandCampaignAssignment>,
}

/// V3が最後に再計算した島嶼キャンペーンをプレイヤー別に保持する診断専用Resource。
/// AI判断からは参照せず、作戦の永続ライフサイクル状態としても使用しない。
#[derive(Resource, Debug, Clone, Default)]
pub struct IslandCampaignDiagnostics {
    pub by_player: HashMap<PlayerId, IslandCampaignPortfolio>,
}

impl IslandCampaignPortfolio {
    pub fn assignment_for(&self, island_id: IslandId) -> Option<&IslandCampaignAssignment> {
        self.defenses
            .iter()
            .chain(self.active_offensives.iter())
            .find(|assignment| assignment.island_id == island_id)
    }

    pub fn offensive_target_positions(&self) -> Vec<GridPosition> {
        let mut positions = Vec::new();
        for assignment in &self.active_offensives {
            if !positions.contains(&assignment.target_position) {
                positions.push(assignment.target_position);
            }
        }
        positions
    }

    pub fn aggregate_missing_requirements(&self) -> Vec<IslandCampaignShortfall> {
        let mut shortfalls = Vec::new();
        for assignment in self.defenses.iter().chain(self.active_offensives.iter()) {
            let missing = &assignment.purchase_shortfall;
            if missing.total_budget == 0
                && missing.transport_slots == 0
                && missing.capture_units == 0
                && missing.combat_budget == 0
            {
                continue;
            }
            let priority_rank = match assignment.decision {
                IslandCampaignDecision::Defend => 0,
                _ if assignment.continued_from_existing_squad => 1,
                IslandCampaignDecision::Expand => 2,
                IslandCampaignDecision::Contest | IslandCampaignDecision::Reinforce => 3,
                IslandCampaignDecision::Assault => 4,
                _ => continue,
            };
            let (light_transport_slots, heavy_transport_slots) = match missing.preferred_transport {
                Some(UnitType::TransportHelicopter) => (missing.transport_slots, 0),
                Some(UnitType::Lander)
                    if assignment.decision == IslandCampaignDecision::Assault =>
                {
                    // AssaultはLander不足を先頭2枠、残りを同時必須の輸送ヘリ不足として表す。
                    let heavy = missing.transport_slots.min(2);
                    (missing.transport_slots.saturating_sub(heavy), heavy)
                }
                Some(UnitType::Lander) => (0, missing.transport_slots),
                _ => (0, 0),
            };
            shortfalls.push(IslandCampaignShortfall {
                island_id: assignment.island_id,
                decision: assignment.decision,
                light_transport_slots,
                heavy_transport_slots,
                capture_units: missing.capture_units,
                combat_budget: missing.combat_budget,
                reserved_budget: missing.total_budget,
                priority_rank,
            });
        }
        shortfalls.sort_by_key(|shortfall| (shortfall.priority_rank, shortfall.island_id.0));
        shortfalls
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CampaignUnitCandidate {
    pub(crate) entity: Entity,
    pub(crate) unit_type: UnitType,
    pub(crate) cost: u32,
    pub(crate) can_capture: bool,
    pub(crate) can_secure_local_property: bool,
    pub(crate) available_cargo_slots: u32,
    pub(crate) loaded_cargo_entities: Vec<Entity>,
    pub(crate) loadable_unit_types: Vec<UnitType>,
    pub(crate) is_transporting: bool,
    pub(crate) reachable_positions: Vec<GridPosition>,
    pub(crate) island_id: Option<IslandId>,
    pub(crate) assigned_island: Option<IslandId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CampaignResourcePool {
    pub(crate) available_funds: u32,
    pub(crate) units: Vec<CampaignUnitCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IslandCampaignCandidate {
    pub(crate) assessment: IslandCampaignAssessment,
    pub(crate) target_position: GridPosition,
    pub(crate) roi_production_sites: u32,
    pub(crate) transport_eta: Option<u32>,
    pub(crate) requirement: IslandCampaignRequirement,
    /// combat power不足が正の場合に予約すべき最小の実購入unit cost。
    pub(crate) minimum_combat_purchase_cost: Option<u32>,
    pub(crate) existing_operation: Option<ExistingCampaignOperation>,
}

/// 永続状態を追加せず、現在も生存しているSquadのIDと目標だけから作戦継続情報を復元する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExistingCampaignOperation {
    pub island_id: IslandId,
    pub target_position: GridPosition,
    pub transport_phase: Option<TransportPhase>,
    pub is_forming: bool,
    pub transport_entities: Vec<Entity>,
    pub capture_entities: Vec<Entity>,
    pub combat_entities: Vec<Entity>,
}

pub(crate) fn campaign_unit_type_rank(unit_type: UnitType) -> u8 {
    match unit_type {
        UnitType::Infantry => 0,
        UnitType::Mech => 1,
        UnitType::Recon => 2,
        UnitType::Tank => 3,
        UnitType::MdTank => 4,
        UnitType::TankZ => 5,
        UnitType::Artillery => 6,
        UnitType::LightSpGun => 7,
        UnitType::HeavySpGun => 8,
        UnitType::Rockets => 9,
        UnitType::AntiAir => 10,
        UnitType::Missiles => 11,
        UnitType::Fighter => 12,
        UnitType::HeavyFighter => 13,
        UnitType::Bomber => 14,
        UnitType::Bcopters => 15,
        UnitType::TransportHelicopter => 16,
        UnitType::Battleship => 17,
        UnitType::Carrier => 18,
        UnitType::Lander => 19,
        UnitType::SupplyTruck => 20,
    }
}

fn is_live_campaign_transport_phase(phase: Option<TransportPhase>) -> bool {
    phase.is_some_and(|phase| {
        matches!(
            phase,
            TransportPhase::Pickup | TransportPhase::Transit | TransportPhase::Drop
        )
    })
}

fn is_offshore_transport(unit_type: UnitType) -> bool {
    matches!(unit_type, UnitType::TransportHelicopter | UnitType::Lander)
}

fn is_campaign_support_unit(unit_type: UnitType) -> bool {
    matches!(
        unit_type,
        UnitType::TransportHelicopter | UnitType::Lander | UnitType::SupplyTruck
    )
}

type OffensivePriorityKey = (
    u8,
    u8,
    u32,
    Reverse<u32>,
    Reverse<u32>,
    u32,
    u32,
    u32,
    u32,
    usize,
);

/// decisionごとの固定keyへ正規化し、HashMapや入力順に依存しない攻勢優先順位を作る。
fn offensive_priority_key(candidate: &IslandCampaignCandidate) -> OffensivePriorityKey {
    let decision_rank = match candidate.assessment.decision {
        IslandCampaignDecision::Expand => 0,
        IslandCampaignDecision::Contest => 1,
        IslandCampaignDecision::Reinforce => 2,
        IslandCampaignDecision::Assault => 3,
        _ => u8::MAX,
    };
    let existing_rank = u8::from(candidate.existing_operation.is_none());
    match candidate.assessment.decision {
        IslandCampaignDecision::Expand => (
            decision_rank,
            existing_rank,
            candidate
                .assessment
                .expansion_payback_turns
                .unwrap_or(u32::MAX),
            Reverse(candidate.roi_production_sites),
            Reverse(candidate.assessment.neutral_properties),
            candidate.transport_eta.unwrap_or(u32::MAX),
            0,
            0,
            0,
            candidate.assessment.island_id.0,
        ),
        IslandCampaignDecision::Contest | IslandCampaignDecision::Reinforce => (
            decision_rank,
            existing_rank,
            0,
            Reverse(0),
            Reverse(0),
            candidate
                .assessment
                .friendly_capture_eta
                .unwrap_or(u32::MAX),
            candidate.assessment.enemy_combat_value,
            0,
            0,
            candidate.assessment.island_id.0,
        ),
        IslandCampaignDecision::Assault => (
            decision_rank,
            existing_rank,
            0,
            Reverse(0),
            Reverse(0),
            0,
            candidate.assessment.required_budget,
            candidate.assessment.enemy_combat_value,
            0,
            candidate.assessment.island_id.0,
        ),
        _ => (
            decision_rank,
            existing_rank,
            0,
            Reverse(0),
            Reverse(0),
            0,
            0,
            0,
            0,
            candidate.assessment.island_id.0,
        ),
    }
}

fn defense_priority_key(candidate: &IslandCampaignCandidate) -> (u32, Reverse<u32>, usize) {
    (
        candidate.assessment.enemy_arrival_eta.unwrap_or(u32::MAX),
        Reverse(candidate.assessment.enemy_combat_value),
        candidate.assessment.island_id.0,
    )
}

fn sorted_pool_units(
    pool: &CampaignResourcePool,
    target_island: IslandId,
) -> Vec<CampaignUnitCandidate> {
    let mut units: Vec<_> = pool
        .units
        .iter()
        .filter(|unit| {
            unit.assigned_island.is_none() || unit.assigned_island == Some(target_island)
        })
        .cloned()
        .collect();
    units.sort_by_key(|unit| {
        (
            u8::from(unit.island_id != Some(target_island)),
            unit.cost,
            campaign_unit_type_rank(unit.unit_type),
            unit.entity.to_bits(),
        )
    });
    units
}

fn remove_entity(pool: &mut CampaignResourcePool, entity: Entity) {
    pool.units.retain(|unit| unit.entity != entity);
}

fn push_unique_entity(entities: &mut Vec<Entity>, entity: Entity) {
    if !entities.contains(&entity) {
        entities.push(entity);
    }
}

fn search_campaign_transport_coverage(
    cargo: &[&CampaignUnitCandidate],
    transports: &[&CampaignUnitCandidate],
    cargo_index: usize,
    assigned_counts: &mut [u32],
) -> bool {
    if cargo_index == cargo.len() {
        return true;
    }
    let unit = cargo[cargo_index];
    for (index, transport) in transports.iter().enumerate() {
        if assigned_counts[index] >= transport.available_cargo_slots
            || transport.island_id != unit.island_id
            || !transport.loadable_unit_types.contains(&unit.unit_type)
        {
            continue;
        }
        assigned_counts[index] = assigned_counts[index].saturating_add(1);
        if search_campaign_transport_coverage(
            cargo,
            transports,
            cargo_index.saturating_add(1),
            assigned_counts,
        ) {
            return true;
        }
        assigned_counts[index] = assigned_counts[index].saturating_sub(1);
    }
    false
}

fn campaign_transport_package_covers(
    cargo_entities: &[Entity],
    transport_entities: &[Entity],
    catalog: &HashMap<Entity, CampaignUnitCandidate>,
) -> bool {
    if cargo_entities.is_empty() {
        return true;
    }
    let mut cargo: Vec<_> = cargo_entities
        .iter()
        .filter_map(|entity| catalog.get(entity))
        .collect();
    let mut transports: Vec<_> = transport_entities
        .iter()
        .filter_map(|entity| catalog.get(entity))
        .collect();
    if cargo.len() != cargo_entities.len() || transports.is_empty() {
        return false;
    }
    transports.sort_by_key(|transport| transport.entity.to_bits());
    let mut loaded_owner = HashMap::new();
    let mut assigned_counts = vec![0_u32; transports.len()];
    for (index, transport) in transports.iter().enumerate() {
        if transport.loaded_cargo_entities.len() as u32 > transport.available_cargo_slots {
            return false;
        }
        for loaded in &transport.loaded_cargo_entities {
            let Some(loaded_unit) = catalog.get(loaded) else {
                return false;
            };
            if !cargo_entities.contains(loaded)
                || transport.island_id != loaded_unit.island_id
                || !transport
                    .loadable_unit_types
                    .contains(&loaded_unit.unit_type)
                || loaded_owner.insert(*loaded, transport.entity).is_some()
            {
                return false;
            }
            assigned_counts[index] = assigned_counts[index].saturating_add(1);
        }
    }
    cargo.retain(|unit| !loaded_owner.contains_key(&unit.entity));
    cargo.sort_by_key(|unit| {
        let compatible = transports
            .iter()
            .enumerate()
            .filter(|(index, transport)| {
                assigned_counts[*index] < transport.available_cargo_slots
                    && transport.island_id == unit.island_id
                    && transport.loadable_unit_types.contains(&unit.unit_type)
            })
            .count();
        (compatible, unit.entity.to_bits())
    });
    search_campaign_transport_coverage(&cargo, &transports, 0, &mut assigned_counts)
}

fn minimum_purchase_floor(
    decision: IslandCampaignDecision,
    missing_transport_slots: u32,
    missing_lander: bool,
    missing_helicopter: bool,
    missing_capture_units: u32,
    missing_combat_budget: u32,
) -> u32 {
    let transport_floor = match decision {
        IslandCampaignDecision::Expand => {
            u32::from(missing_transport_slots > 0).saturating_mul(4_000)
        }
        IslandCampaignDecision::Assault => u32::from(missing_lander)
            .saturating_mul(16_500)
            .saturating_add(u32::from(missing_helicopter).saturating_mul(4_000)),
        _ => 0,
    };
    transport_floor
        .saturating_add(missing_capture_units.saturating_mul(1_000))
        .saturating_add(missing_combat_budget)
}

fn reserve_candidate(
    candidate: &IslandCampaignCandidate,
    pool: &CampaignResourcePool,
    catalog: &HashMap<Entity, CampaignUnitCandidate>,
) -> Option<(IslandCampaignAssignment, CampaignResourcePool)> {
    // 候補ごとのcloneへ全予約を適用し、完全編成を賄えない場合は元poolを一切変更しない。
    let mut provisional = pool.clone();
    let island_id = candidate.assessment.island_id;
    let requirement = &candidate.requirement;
    let mut transport_entities = Vec::new();
    let mut capture_entities = Vec::new();
    let mut combat_entities = Vec::new();
    let mut reserved_entity_value = 0_u32;
    let mut requirement_credit = 0_u32;
    let existing = candidate.existing_operation.as_ref();

    if let Some(operation) = existing {
        if operation.is_forming || is_live_campaign_transport_phase(operation.transport_phase) {
            for entity in &operation.transport_entities {
                if let Some(unit) = catalog.get(entity) {
                    push_unique_entity(&mut transport_entities, *entity);
                    reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
                    remove_entity(&mut provisional, *entity);
                }
            }
        } else {
            // Return/Completed輸送は同じ島への再予約候補としても扱わない。
            for entity in &operation.transport_entities {
                remove_entity(&mut provisional, *entity);
            }
        }
        for entity in &operation.capture_entities {
            if let Some(unit) = catalog.get(entity)
                && (candidate.assessment.decision != IslandCampaignDecision::Defend
                    || !unit.is_transporting
                        && unit
                            .reachable_positions
                            .contains(&candidate.target_position))
            {
                push_unique_entity(&mut capture_entities, *entity);
                reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
                remove_entity(&mut provisional, *entity);
            }
        }
        for entity in &operation.combat_entities {
            if let Some(unit) = catalog.get(entity)
                && (candidate.assessment.decision != IslandCampaignDecision::Defend
                    || !unit.is_transporting
                        && unit
                            .reachable_positions
                            .contains(&candidate.target_position))
            {
                push_unique_entity(&mut combat_entities, *entity);
                reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
                remove_entity(&mut provisional, *entity);
            }
        }
    }
    let continued_from_existing_squad = !transport_entities.is_empty()
        || !capture_entities.is_empty()
        || !combat_entities.is_empty();

    if candidate.assessment.decision == IslandCampaignDecision::Contest {
        let local_assets: Vec<_> = sorted_pool_units(&provisional, island_id)
            .into_iter()
            .filter(|unit| {
                unit.island_id == Some(island_id) && !is_campaign_support_unit(unit.unit_type)
            })
            .collect();
        for unit in local_assets {
            if unit.can_capture {
                push_unique_entity(&mut capture_entities, unit.entity);
            } else {
                push_unique_entity(&mut combat_entities, unit.entity);
            }
            reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
            remove_entity(&mut provisional, unit.entity);
        }
    }

    let mut remaining_transport_slots = requirement.transport_slots;
    let mut missing_lander = candidate.assessment.decision == IslandCampaignDecision::Assault;
    let mut missing_helicopter = matches!(
        candidate.assessment.decision,
        IslandCampaignDecision::Expand | IslandCampaignDecision::Assault
    );
    for entity in &transport_entities {
        let Some(unit) = catalog.get(entity) else {
            continue;
        };
        if unit.unit_type == UnitType::Lander {
            missing_lander = false;
        }
        if unit.unit_type == UnitType::TransportHelicopter {
            missing_helicopter = false;
        }
        let satisfies_transport_requirement = match candidate.assessment.decision {
            IslandCampaignDecision::Expand => unit.unit_type == UnitType::TransportHelicopter,
            _ => is_offshore_transport(unit.unit_type),
        };
        if satisfies_transport_requirement {
            let credited_slots = remaining_transport_slots.min(unit.available_cargo_slots);
            remaining_transport_slots = remaining_transport_slots.saturating_sub(credited_slots);
            requirement_credit = requirement_credit.saturating_add(unit.cost);
        }
    }

    let mut available = sorted_pool_units(&provisional, island_id);
    if candidate.assessment.decision == IslandCampaignDecision::Assault {
        for required_type in [UnitType::Lander, UnitType::TransportHelicopter] {
            let already_present = transport_entities
                .iter()
                .filter_map(|entity| catalog.get(entity))
                .any(|unit| unit.unit_type == required_type);
            if already_present {
                continue;
            }
            if let Some(index) = available.iter().position(|unit| {
                unit.unit_type == required_type && is_offshore_transport(unit.unit_type)
            }) {
                let unit = available.remove(index);
                push_unique_entity(&mut transport_entities, unit.entity);
                remaining_transport_slots =
                    remaining_transport_slots.saturating_sub(unit.available_cargo_slots);
                requirement_credit = requirement_credit.saturating_add(unit.cost);
                reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
                remove_entity(&mut provisional, unit.entity);
                if required_type == UnitType::Lander {
                    missing_lander = false;
                } else {
                    missing_helicopter = false;
                }
            }
        }
    } else {
        while remaining_transport_slots > 0 {
            let Some(index) = available.iter().position(|unit| {
                is_offshore_transport(unit.unit_type)
                    && requirement
                        .preferred_transport
                        .is_none_or(|preferred| unit.unit_type == preferred)
            }) else {
                break;
            };
            let unit = available.remove(index);
            push_unique_entity(&mut transport_entities, unit.entity);
            remaining_transport_slots =
                remaining_transport_slots.saturating_sub(unit.available_cargo_slots);
            requirement_credit = requirement_credit.saturating_add(unit.cost);
            reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
            remove_entity(&mut provisional, unit.entity);
            if unit.unit_type == UnitType::TransportHelicopter {
                missing_helicopter = false;
            }
        }
    }

    let mut remaining_capture_units = requirement.capture_units;
    let mut capture_entities_used_for_capture = 0_usize;
    for entity in &capture_entities {
        if remaining_capture_units == 0 {
            break;
        }
        if let Some(unit) = catalog.get(entity) {
            remaining_capture_units = remaining_capture_units.saturating_sub(1);
            capture_entities_used_for_capture = capture_entities_used_for_capture.saturating_add(1);
            requirement_credit = requirement_credit.saturating_add(unit.cost);
        }
    }
    available = sorted_pool_units(&provisional, island_id);
    while remaining_capture_units > 0 {
        let Some(index) = available.iter().position(|unit| unit.can_capture) else {
            break;
        };
        let unit = available.remove(index);
        push_unique_entity(&mut capture_entities, unit.entity);
        remaining_capture_units = remaining_capture_units.saturating_sub(1);
        capture_entities_used_for_capture = capture_entities_used_for_capture.saturating_add(1);
        requirement_credit = requirement_credit.saturating_add(unit.cost);
        reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
        remove_entity(&mut provisional, unit.entity);
    }

    let mut remaining_combat_budget = requirement.combat_budget;
    for entity in capture_entities
        .iter()
        .skip(capture_entities_used_for_capture)
        .chain(combat_entities.iter())
    {
        if remaining_combat_budget == 0 {
            break;
        }
        if let Some(unit) = catalog.get(entity) {
            // capture枠へ使わなかった既存歩兵は戦闘価値へ一度だけ充当する。
            let credited = remaining_combat_budget.min(unit.cost);
            remaining_combat_budget = remaining_combat_budget.saturating_sub(credited);
            requirement_credit = requirement_credit.saturating_add(credited);
        }
    }
    available = sorted_pool_units(&provisional, island_id);
    while remaining_combat_budget > 0 {
        let Some(index) = available.iter().position(|unit| {
            !is_campaign_support_unit(unit.unit_type)
                && (candidate.assessment.decision != IslandCampaignDecision::Defend
                    || !unit.is_transporting
                        && unit
                            .reachable_positions
                            .contains(&candidate.target_position))
        }) else {
            break;
        };
        let unit = available.remove(index);
        push_unique_entity(&mut combat_entities, unit.entity);
        let credited = remaining_combat_budget.min(unit.cost);
        remaining_combat_budget = remaining_combat_budget.saturating_sub(credited);
        requirement_credit = requirement_credit.saturating_add(credited);
        reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
        remove_entity(&mut provisional, unit.entity);
    }

    if candidate.assessment.decision == IslandCampaignDecision::Reinforce {
        let remote_cargo: Vec<_> = capture_entities
            .iter()
            .chain(combat_entities.iter())
            .copied()
            .filter(|entity| {
                catalog
                    .get(entity)
                    .is_some_and(|unit| !unit.is_transporting && unit.island_id != Some(island_id))
            })
            .collect();
        if !remote_cargo.is_empty() {
            // 洋上補強は、予約cargoと同じ出発島にいて全cargoを実搭載可能な輸送役も同時予約する。
            available = sorted_pool_units(&provisional, island_id);
            while !campaign_transport_package_covers(&remote_cargo, &transport_entities, catalog) {
                let index = available.iter().position(|transport| {
                    is_offshore_transport(transport.unit_type)
                        && remote_cargo.iter().any(|cargo| {
                            catalog.get(cargo).is_some_and(|cargo| {
                                cargo.island_id == transport.island_id
                                    && transport.loadable_unit_types.contains(&cargo.unit_type)
                            })
                        })
                })?;
                let transport = available.remove(index);
                push_unique_entity(&mut transport_entities, transport.entity);
                reserved_entity_value = reserved_entity_value.saturating_add(transport.cost);
                remove_entity(&mut provisional, transport.entity);
            }
        }
    }

    if candidate.assessment.decision == IslandCampaignDecision::Assault {
        remaining_transport_slots = u32::from(missing_lander)
            .saturating_mul(2)
            .saturating_add(u32::from(missing_helicopter).saturating_mul(2));
    }
    let combat_purchase_floor = if remaining_combat_budget > 0 {
        // combat powerの端数も、実際に購入可能な最小unit 1体分を予約できなければ受理しない。
        candidate.minimum_combat_purchase_cost?
    } else {
        0
    };
    // 高価な既存unitのcreditで輸送・占領など必須カテゴリの購入費が消えないよう下限を戻す。
    let structural_floor = minimum_purchase_floor(
        candidate.assessment.decision,
        remaining_transport_slots,
        missing_lander,
        missing_helicopter,
        remaining_capture_units,
        remaining_combat_budget,
    )
    .max(combat_purchase_floor);
    let purchase_budget = requirement
        .total_budget
        .saturating_sub(requirement_credit)
        .max(structural_floor);
    if provisional.available_funds < purchase_budget {
        return None;
    }
    provisional.available_funds = provisional.available_funds.saturating_sub(purchase_budget);

    let preferred_transport = if candidate.assessment.decision == IslandCampaignDecision::Assault {
        if missing_lander {
            Some(UnitType::Lander)
        } else if missing_helicopter {
            Some(UnitType::TransportHelicopter)
        } else {
            None
        }
    } else if remaining_transport_slots > 0 {
        requirement.preferred_transport
    } else {
        None
    };
    let purchase_shortfall = IslandCampaignRequirement {
        preferred_transport,
        transport_slots: remaining_transport_slots,
        capture_units: remaining_capture_units,
        combat_budget: remaining_combat_budget,
        total_budget: purchase_budget,
    };
    let operation_ready = purchase_shortfall.transport_slots == 0
        && purchase_shortfall.capture_units == 0
        && purchase_shortfall.combat_budget == 0;
    transport_entities.sort_by_key(|entity| entity.to_bits());
    capture_entities.sort_by_key(|entity| entity.to_bits());
    combat_entities.sort_by_key(|entity| entity.to_bits());

    Some((
        IslandCampaignAssignment {
            island_id,
            decision: candidate.assessment.decision,
            target_position: candidate.target_position,
            requirement: requirement.clone(),
            purchase_shortfall,
            allocated_budget: reserved_entity_value.saturating_add(purchase_budget),
            transport_entities,
            capture_entities,
            combat_entities,
            operation_ready,
            continued_from_existing_squad,
        },
        provisional,
    ))
}

fn update_assessment_for_assignment(
    assessments: &mut [IslandCampaignAssessment],
    assignment: &IslandCampaignAssignment,
) {
    if let Some(assessment) = assessments
        .iter_mut()
        .find(|assessment| assessment.island_id == assignment.island_id)
    {
        assessment.decision = assignment.decision;
        assessment.pause_cause = None;
        assessment.allocated_budget = assignment.allocated_budget;
    }
}

fn mark_unallocated(
    assessments: &mut [IslandCampaignAssessment],
    island_id: IslandId,
    decision: IslandCampaignDecision,
    reason: &str,
    pause_cause: Option<IslandCampaignPauseCause>,
) {
    if let Some(assessment) = assessments
        .iter_mut()
        .find(|assessment| assessment.island_id == island_id)
    {
        assessment.decision = decision;
        assessment.decision_reason = reason.to_owned();
        assessment.pause_cause = pause_cause;
        assessment.allocated_budget = 0;
    }
}

type ContestProtection = HashMap<IslandId, Vec<(Entity, Option<IslandId>)>>;

fn protect_contest_assets(
    candidates: &[IslandCampaignCandidate],
    pool: &mut CampaignResourcePool,
) -> ContestProtection {
    let mut protections: ContestProtection = HashMap::new();
    let mut contest_islands: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.assessment.decision == IslandCampaignDecision::Contest)
        .map(|candidate| candidate.assessment.island_id)
        .collect();
    contest_islands.sort_by_key(|island_id| island_id.0);
    contest_islands.dedup();
    for island_id in contest_islands {
        for unit in &mut pool.units {
            let can_preserve = unit.island_id == Some(island_id)
                && !is_campaign_support_unit(unit.unit_type)
                && (unit.assigned_island.is_none() || unit.assigned_island == Some(island_id));
            if can_preserve {
                protections
                    .entry(island_id)
                    .or_default()
                    .push((unit.entity, unit.assigned_island));
                // Contest成立に使った現地戦力を先行する他島攻勢から一時的に保護する。
                unit.assigned_island = Some(island_id);
            }
        }
    }
    protections
}

fn restore_contest_assets(
    island_id: IslandId,
    pool: &mut CampaignResourcePool,
    protections: &mut ContestProtection,
) {
    let Some(protected) = protections.remove(&island_id) else {
        return;
    };
    for (entity, prior_assignment) in protected {
        if let Some(unit) = pool.units.iter_mut().find(|unit| unit.entity == entity) {
            // cap超過や予約失敗時は一時markerだけを元へ戻し、後続Defendへ解放する。
            unit.assigned_island = prior_assignment;
        }
    }
}

fn reserve_secure_capture_units(
    candidates: &[IslandCampaignCandidate],
    pool: &mut CampaignResourcePool,
) {
    let mut secure_islands: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.assessment.decision == IslandCampaignDecision::Secure)
        .map(|candidate| candidate.assessment.island_id)
        .collect();
    secure_islands.sort_by_key(|island_id| island_id.0);
    secure_islands.dedup();
    for island_id in secure_islands {
        let mut local_capture_units: Vec<_> = pool
            .units
            .iter()
            .filter(|unit| {
                unit.island_id == Some(island_id) && unit.can_capture && !unit.is_transporting
            })
            .cloned()
            .collect();
        local_capture_units.sort_by_key(|unit| {
            (
                unit.cost,
                campaign_unit_type_rank(unit.unit_type),
                unit.entity.to_bits(),
            )
        });
        let entity = local_capture_units
            .iter()
            .find(|unit| unit.can_secure_local_property)
            .or_else(|| local_capture_units.first())
            .map(|unit| unit.entity);
        if let Some(entity) = entity {
            // Secureは攻勢枠を使わないが、現地占領要員1体だけは他島へ流用しない。
            remove_entity(pool, entity);
        }
    }
}

fn release_assignment(
    assignment: &IslandCampaignAssignment,
    pool: &mut CampaignResourcePool,
    catalog: &HashMap<Entity, CampaignUnitCandidate>,
) {
    pool.available_funds = pool
        .available_funds
        .saturating_add(assignment.purchase_shortfall.total_budget);
    for entity in assignment
        .transport_entities
        .iter()
        .chain(assignment.capture_entities.iter())
        .chain(assignment.combat_entities.iter())
    {
        if pool.units.iter().any(|unit| unit.entity == *entity) {
            continue;
        }
        if let Some(unit) = catalog.get(entity) {
            let mut released = unit.clone();
            released.assigned_island = None;
            pool.units.push(released);
        }
    }
}

/// 共有資金とEntity候補を1つの可変poolとして扱い、完全編成だけを決定的に予約する。
pub(crate) fn allocate_campaign_portfolio(
    candidates: Vec<IslandCampaignCandidate>,
    mut pool: CampaignResourcePool,
) -> IslandCampaignPortfolio {
    let catalog: HashMap<_, _> = pool
        .units
        .iter()
        .cloned()
        .map(|unit| (unit.entity, unit))
        .collect();
    let mut contest_protections = protect_contest_assets(&candidates, &mut pool);
    reserve_secure_capture_units(&candidates, &mut pool);
    let mut assessments: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.assessment.clone())
        .collect();
    let mut offenses: Vec<_> = candidates
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.assessment.decision,
                IslandCampaignDecision::Expand
                    | IslandCampaignDecision::Contest
                    | IslandCampaignDecision::Reinforce
                    | IslandCampaignDecision::Assault
            )
        })
        .cloned()
        .collect();
    offenses.sort_by_key(offensive_priority_key);
    let mut active_offensives = Vec::new();
    for candidate in offenses {
        if active_offensives.len() == 3 {
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                restore_contest_assets(
                    candidate.assessment.island_id,
                    &mut pool,
                    &mut contest_protections,
                );
            }
            mark_unallocated(
                &mut assessments,
                candidate.assessment.island_id,
                IslandCampaignDecision::Observe,
                "同時攻勢上限のため監視する",
                None,
            );
            continue;
        }
        if let Some((assignment, provisional)) = reserve_candidate(&candidate, &pool, &catalog) {
            pool = provisional;
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                contest_protections.remove(&candidate.assessment.island_id);
            }
            update_assessment_for_assignment(&mut assessments, &assignment);
            active_offensives.push(assignment);
        } else {
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                restore_contest_assets(
                    candidate.assessment.island_id,
                    &mut pool,
                    &mut contest_protections,
                );
            }
            let has_active_expansion = active_offensives
                .iter()
                .any(|assignment| assignment.decision == IslandCampaignDecision::Expand);
            let (decision, reason) = if candidate.assessment.decision
                == IslandCampaignDecision::Reinforce
                && has_active_expansion
            {
                (
                    IslandCampaignDecision::Withdraw,
                    "より投資回収効率のよい中立島作戦を優先する",
                )
            } else if candidate.assessment.decision == IslandCampaignDecision::Reinforce {
                (
                    IslandCampaignDecision::Contest,
                    "完全増援を予約できないため現地作戦を継続する",
                )
            } else {
                (
                    IslandCampaignDecision::Observe,
                    "完全編成を予約できないため監視する",
                )
            };
            mark_unallocated(
                &mut assessments,
                candidate.assessment.island_id,
                decision,
                reason,
                None,
            );
        }
    }

    let mut defense_candidates: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| candidate.assessment.decision == IslandCampaignDecision::Defend)
        .collect();
    defense_candidates.sort_by_key(defense_priority_key);
    let mut defenses = Vec::new();
    for candidate in defense_candidates {
        loop {
            if let Some((assignment, provisional)) = reserve_candidate(&candidate, &pool, &catalog)
            {
                pool = provisional;
                update_assessment_for_assignment(&mut assessments, &assignment);
                defenses.push(assignment);
                break;
            }
            let Some(released) = active_offensives.pop() else {
                mark_unallocated(
                    &mut assessments,
                    candidate.assessment.island_id,
                    IslandCampaignDecision::Observe,
                    "完全な防衛編成を予約できないため監視する",
                    None,
                );
                break;
            };
            release_assignment(&released, &mut pool, &catalog);
            mark_unallocated(
                &mut assessments,
                released.island_id,
                IslandCampaignDecision::Observe,
                "Threatened島防衛のため攻勢を一時停止",
                Some(IslandCampaignPauseCause::DefensePreemption),
            );
        }
    }

    assessments.sort_by_key(|assessment| assessment.island_id.0);
    IslandCampaignPortfolio {
        islands: assessments,
        active_offensives,
        defenses,
    }
}

/// 島状態は保存済みの遷移状態ではなく、各ターンの事実から再計算する一時的な分類である。
/// 判定順序そのものがドメインルールであり、交戦中を脅威状態より優先するなど、複数条件が
/// 同時に成立しても必ず排他的な状態を返すため、最初に一致した分類を直ちに返す。
pub fn classify_island(facts: &IslandCampaignFacts) -> IslandCampaignState {
    if facts.capturable_properties == 0
        || !facts.reachable
        || (facts.island_income_per_turn == 0 && facts.strategic_production_sites == 0)
    {
        return IslandCampaignState::Ignored;
    }
    if facts.friendly_units > 0 && facts.enemy_units > 0 {
        return IslandCampaignState::Contested;
    }
    if facts.enemy_units == 0
        && (facts.friendly_units > 0 || facts.friendly_properties > 0)
        && facts.enemy_arrival_eta.is_some_and(|eta| eta <= 2)
    {
        return IslandCampaignState::Threatened;
    }
    if facts.neutral_properties > 0
        && facts.friendly_properties == 0
        && facts.enemy_properties == 0
        && facts.friendly_units == 0
        && facts.enemy_units == 0
    {
        return IslandCampaignState::OpenNeutral;
    }
    if facts.enemy_units == 0 && (facts.friendly_units > 0 || facts.friendly_properties > 0) {
        return IslandCampaignState::Secured;
    }
    if facts.friendly_units == 0 && (facts.enemy_units > 0 || facts.enemy_properties > 0) {
        return IslandCampaignState::EnemyHeld;
    }
    IslandCampaignState::Ignored
}

fn ceil_div(numerator: u32, denominator: u32) -> u32 {
    numerator / denominator + u32::from(!numerator.is_multiple_of(denominator))
}

/// u64へ拡張してから敵戦力の120%を切り上げ計算し、u32の乗算オーバーフローを防ぐ。
fn ceil_scale_to_120_percent(value: u32) -> u64 {
    u64::from(value).saturating_mul(12).saturating_add(9) / 10
}

/// 輸送・占領・不足パッケージ回収を合わせた投資回収ターン数を計算する。
pub fn calculate_expansion_payback_turns(
    transport_eta: Option<u32>,
    capture_turns: u32,
    missing_package_cost: u32,
    island_income_per_turn: u32,
) -> Option<u32> {
    let transport_eta = transport_eta?;
    if island_income_per_turn == 0 {
        return None;
    }

    Some(
        transport_eta
            .saturating_add(capture_turns)
            .saturating_add(ceil_div(missing_package_cost, island_income_per_turn)),
    )
}

/// 固定費と敵戦力の120%を合算し、最低侵攻予算を下回らない値を返す。
pub fn required_assault_budget(enemy_combat_value: u32) -> u32 {
    const FIXED_COST: u64 = 22_500;
    const COMBAT_FLOOR: u64 = 10_200;

    // 合計がu32上限を超える場合だけ最終結果を飽和させる。
    let scaled_enemy_value = ceil_scale_to_120_percent(enemy_combat_value);
    let total = FIXED_COST.saturating_add(COMBAT_FLOOR.max(scaled_enemy_value));

    u32::try_from(total).unwrap_or(u32::MAX)
}

/// 交戦中の島について、共有資源配分側から渡された補強可否と代替投資有無で判断する。
pub fn decide_contested(
    facts: &IslandCampaignFacts,
    reinforced_friendly_power: u32,
    can_allocate_reinforcement: bool,
    has_better_open_neutral: bool,
) -> IslandCampaignDecision {
    let capture_race_is_competitive = facts
        .friendly_capture_eta
        .zip(facts.enemy_capture_eta)
        .is_some_and(|(friendly_eta, enemy_eta)| {
            friendly_eta <= enemy_eta.saturating_add(1)
                && facts.friendly_combat_value >= facts.enemy_combat_value
        });
    if capture_race_is_competitive {
        return IslandCampaignDecision::Contest;
    }

    // 補強後戦力は敵戦力の120%に届く完全パッケージの場合だけ採用する。
    let required_reinforced_power = ceil_scale_to_120_percent(facts.enemy_combat_value);
    if can_allocate_reinforcement
        && u64::from(reinforced_friendly_power) >= required_reinforced_power
    {
        return IslandCampaignDecision::Reinforce;
    }
    if has_better_open_neutral {
        return IslandCampaignDecision::Withdraw;
    }

    IslandCampaignDecision::Contest
}

fn ignored_state_reason(facts: &IslandCampaignFacts) -> &'static str {
    if facts.capturable_properties == 0 {
        "占領可能な拠点がないため対象外"
    } else if !facts.reachable {
        "到達できないため対象外"
    } else if facts.island_income_per_turn == 0 && facts.strategic_production_sites == 0 {
        "収益性と戦略価値がないため対象外"
    } else {
        "島嶼作戦の分類条件を満たさないため対象外"
    }
}

/// 各島の事実だけから状態と暫定判断を作る。共有資源による最終確定は配分側で行う。
pub fn assess_island(facts: &IslandCampaignFacts) -> IslandCampaignAssessment {
    let state = classify_island(facts);
    let expansion_payback_turns = if state == IslandCampaignState::OpenNeutral {
        calculate_expansion_payback_turns(
            facts.transport_eta,
            facts.capture_turns,
            facts.missing_expansion_package_cost,
            facts.island_income_per_turn,
        )
    } else {
        None
    };

    // 状態ごとの分岐に対応した固定文言を使い、構造体のDebug表現には依存しない。
    let (decision, state_reason, decision_reason, required_budget) = match state {
        IslandCampaignState::Ignored => (
            IslandCampaignDecision::Observe,
            ignored_state_reason(facts),
            "侵攻条件を満たさないため監視する",
            0,
        ),
        IslandCampaignState::OpenNeutral if expansion_payback_turns.is_some() => (
            IslandCampaignDecision::Expand,
            "未占領の中立島である",
            "投資回収ターンを算出できるため暫定的に拡張候補とする",
            0,
        ),
        IslandCampaignState::OpenNeutral => (
            IslandCampaignDecision::Observe,
            "未占領の中立島である",
            "投資回収ターンを算出できないため監視する",
            0,
        ),
        IslandCampaignState::Secured if facts.has_unowned_properties => (
            IslandCampaignDecision::Secure,
            "友軍が足場を確保している島である",
            "未所有の拠点を確保する",
            0,
        ),
        IslandCampaignState::Secured => (
            IslandCampaignDecision::Observe,
            "友軍が足場を確保している島である",
            "追加で確保する拠点がないため監視する",
            0,
        ),
        IslandCampaignState::Threatened => (
            IslandCampaignDecision::Defend,
            "敵軍の接近が予測される友軍拠点である",
            "敵軍到着に備えて防衛する",
            0,
        ),
        IslandCampaignState::Contested => (
            IslandCampaignDecision::Contest,
            "友軍と敵軍が同時に展開している島である",
            "共有資源配分前のため暫定的に現地で交戦を継続する",
            0,
        ),
        IslandCampaignState::EnemyHeld => (
            IslandCampaignDecision::Assault,
            "敵軍が支配している島である",
            "必要侵攻予算を算出し暫定的に強襲候補とする",
            required_assault_budget(facts.enemy_combat_value),
        ),
    };

    IslandCampaignAssessment {
        island_id: facts.island_id,
        state,
        decision,
        state_reason: state_reason.to_owned(),
        decision_reason: decision_reason.to_owned(),
        pause_cause: None,
        neutral_properties: facts.neutral_properties,
        friendly_properties: facts.friendly_properties,
        enemy_properties: facts.enemy_properties,
        friendly_combat_value: facts.friendly_combat_value,
        enemy_combat_value: facts.enemy_combat_value,
        friendly_arrival_eta: facts.friendly_arrival_eta,
        enemy_arrival_eta: facts.enemy_arrival_eta,
        friendly_capture_eta: facts.friendly_capture_eta,
        enemy_capture_eta: facts.enemy_capture_eta,
        expansion_payback_turns,
        required_budget,
        allocated_budget: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::islands::IslandId;
    use std::collections::HashSet;

    fn facts_for_empty_neutral_island() -> IslandCampaignFacts {
        IslandCampaignFacts {
            island_id: IslandId(0),
            capturable_properties: 1,
            strategic_production_sites: 0,
            roi_production_sites: 0,
            neutral_properties: 1,
            friendly_properties: 0,
            enemy_properties: 0,
            friendly_units: 0,
            enemy_units: 0,
            friendly_combat_value: 0,
            enemy_combat_value: 0,
            friendly_arrival_eta: None,
            enemy_arrival_eta: None,
            friendly_capture_eta: None,
            enemy_capture_eta: None,
            transport_eta: None,
            capture_turns: 1,
            island_income_per_turn: 1_000,
            missing_expansion_package_cost: 0,
            reachable: true,
            has_unowned_properties: true,
        }
    }

    fn facts_without_capturable_properties() -> IslandCampaignFacts {
        let mut facts = facts_for_empty_neutral_island();
        facts.capturable_properties = 0;
        facts.neutral_properties = 0;
        facts.has_unowned_properties = false;
        facts
    }

    fn facts_with_both_armies_present() -> IslandCampaignFacts {
        let mut facts = facts_for_empty_neutral_island();
        facts.friendly_units = 1;
        facts.enemy_units = 1;
        facts
    }

    fn facts_with_friendly_foothold_and_enemy_eta(eta: u32) -> IslandCampaignFacts {
        let mut facts = facts_for_safe_friendly_foothold();
        facts.enemy_arrival_eta = Some(eta);
        facts
    }

    fn facts_for_safe_friendly_foothold() -> IslandCampaignFacts {
        let mut facts = facts_for_empty_neutral_island();
        facts.neutral_properties = 0;
        facts.friendly_properties = 1;
        facts.friendly_units = 1;
        facts.has_unowned_properties = false;
        facts
    }

    fn facts_for_enemy_foothold() -> IslandCampaignFacts {
        let mut facts = facts_for_empty_neutral_island();
        facts.neutral_properties = 0;
        facts.enemy_properties = 1;
        facts.enemy_units = 1;
        facts.has_unowned_properties = false;
        facts
    }

    fn facts_with_capture_etas(
        friendly_capture_eta: Option<u32>,
        enemy_capture_eta: Option<u32>,
    ) -> IslandCampaignFacts {
        let mut facts = facts_with_both_armies_present();
        facts.friendly_combat_value = 10_000;
        facts.enemy_combat_value = 10_000;
        facts.friendly_capture_eta = friendly_capture_eta;
        facts.enemy_capture_eta = enemy_capture_eta;
        facts
    }

    fn facts_with_power(
        friendly_combat_value: u32,
        enemy_combat_value: u32,
    ) -> IslandCampaignFacts {
        let mut facts = facts_with_both_armies_present();
        facts.friendly_combat_value = friendly_combat_value;
        facts.enemy_combat_value = enemy_combat_value;
        facts
    }

    #[test]
    fn classifies_islands_in_required_precedence_order() {
        let cases = [
            (
                facts_without_capturable_properties(),
                IslandCampaignState::Ignored,
            ),
            (
                facts_with_both_armies_present(),
                IslandCampaignState::Contested,
            ),
            (
                facts_with_friendly_foothold_and_enemy_eta(2),
                IslandCampaignState::Threatened,
            ),
            (
                facts_for_empty_neutral_island(),
                IslandCampaignState::OpenNeutral,
            ),
            (
                facts_for_safe_friendly_foothold(),
                IslandCampaignState::Secured,
            ),
            (facts_for_enemy_foothold(), IslandCampaignState::EnemyHeld),
        ];

        for (facts, expected) in cases {
            assert_eq!(classify_island(&facts), expected);
        }
    }

    #[test]
    fn contested_precedes_threatened_when_both_armies_are_present() {
        let mut facts = facts_with_both_armies_present();
        facts.enemy_arrival_eta = Some(1);

        assert_eq!(classify_island(&facts), IslandCampaignState::Contested);
    }

    #[test]
    fn derives_state_transitions_from_current_facts() {
        let mut open = facts_for_empty_neutral_island();

        assert_eq!(classify_island(&open), IslandCampaignState::OpenNeutral);
        open.friendly_units = 1;
        open.friendly_properties = 1;
        assert_eq!(classify_island(&open), IslandCampaignState::Secured);
        open.enemy_arrival_eta = Some(2);
        assert_eq!(classify_island(&open), IslandCampaignState::Threatened);
        open.enemy_units = 1;
        assert_eq!(classify_island(&open), IslandCampaignState::Contested);
    }

    #[test]
    fn classifies_required_initial_island_states() {
        let own_home_island = facts_for_safe_friendly_foothold();
        let enemy_home_island = facts_for_enemy_foothold();
        let empty_neutral_island = facts_for_empty_neutral_island();
        let no_property_island = facts_without_capturable_properties();

        assert_eq!(
            classify_island(&own_home_island),
            IslandCampaignState::Secured
        );
        assert_eq!(
            classify_island(&enemy_home_island),
            IslandCampaignState::EnemyHeld
        );
        assert_eq!(
            classify_island(&empty_neutral_island),
            IslandCampaignState::OpenNeutral
        );
        assert_eq!(
            classify_island(&no_property_island),
            IslandCampaignState::Ignored
        );
    }

    #[test]
    fn calculates_open_neutral_payback_with_ceiling_division() {
        assert_eq!(
            calculate_expansion_payback_turns(Some(2), 3, 6_001, 1_000),
            Some(12),
        );
        assert_eq!(
            calculate_expansion_payback_turns(Some(2), 3, 6_000, 0),
            None
        );
        assert_eq!(
            calculate_expansion_payback_turns(None, 3, 6_000, 1_000),
            None
        );
        assert_eq!(
            calculate_expansion_payback_turns(Some(u32::MAX), 1, u32::MAX, 1),
            Some(u32::MAX),
        );
    }

    #[test]
    fn calculates_enemy_held_budget_floor_and_scaled_budget() {
        assert_eq!(required_assault_budget(0), 32_700);
        assert_eq!(required_assault_budget(8_500), 32_700);
        assert_eq!(required_assault_budget(10_000), 34_500);
        assert_eq!(required_assault_budget(u32::MAX), u32::MAX);
    }

    #[test]
    fn decides_contested_action_from_capture_race_and_complete_reinforcement() {
        assert_eq!(
            decide_contested(
                &facts_with_capture_etas(Some(3), Some(2)),
                10_000,
                false,
                false,
            ),
            IslandCampaignDecision::Contest,
        );
        assert_eq!(
            decide_contested(&facts_with_power(8_000, 10_000), 12_000, true, false),
            IslandCampaignDecision::Reinforce,
        );
        assert_eq!(
            decide_contested(&facts_with_power(8_000, 10_000), 8_000, false, true),
            IslandCampaignDecision::Withdraw,
        );
        assert_eq!(
            decide_contested(&facts_with_power(8_000, 10_000), 11_999, true, true),
            IslandCampaignDecision::Withdraw,
        );
        assert_eq!(
            decide_contested(&facts_with_power(8_000, 10_000), 8_000, false, false),
            IslandCampaignDecision::Contest,
        );
    }

    #[test]
    fn assesses_each_state_with_provisional_decision_and_local_values() {
        let ignored = assess_island(&facts_without_capturable_properties());
        assert_eq!(ignored.state, IslandCampaignState::Ignored);
        assert_eq!(ignored.decision, IslandCampaignDecision::Observe);
        assert_eq!(ignored.state_reason, "占領可能な拠点がないため対象外");
        assert_eq!(ignored.decision_reason, "侵攻条件を満たさないため監視する");

        let mut open = facts_for_empty_neutral_island();
        open.transport_eta = Some(2);
        open.capture_turns = 3;
        open.missing_expansion_package_cost = 6_001;
        let expansion = assess_island(&open);
        assert_eq!(expansion.state, IslandCampaignState::OpenNeutral);
        assert_eq!(expansion.decision, IslandCampaignDecision::Expand);
        assert_eq!(expansion.expansion_payback_turns, Some(12));
        assert_eq!(
            expansion.decision_reason,
            "投資回収ターンを算出できるため暫定的に拡張候補とする"
        );

        open.transport_eta = None;
        let unpriced_expansion = assess_island(&open);
        assert_eq!(unpriced_expansion.decision, IslandCampaignDecision::Observe);
        assert_eq!(unpriced_expansion.expansion_payback_turns, None);
        assert_eq!(
            unpriced_expansion.decision_reason,
            "投資回収ターンを算出できないため監視する"
        );

        let mut secured_facts = facts_for_safe_friendly_foothold();
        secured_facts.neutral_properties = 1;
        secured_facts.has_unowned_properties = true;
        let secured = assess_island(&secured_facts);
        assert_eq!(secured.state, IslandCampaignState::Secured);
        assert_eq!(secured.decision, IslandCampaignDecision::Secure);

        secured_facts.neutral_properties = 0;
        secured_facts.has_unowned_properties = false;
        assert_eq!(
            assess_island(&secured_facts).decision,
            IslandCampaignDecision::Observe
        );

        let threatened = assess_island(&facts_with_friendly_foothold_and_enemy_eta(2));
        assert_eq!(threatened.state, IslandCampaignState::Threatened);
        assert_eq!(threatened.decision, IslandCampaignDecision::Defend);

        let contested = assess_island(&facts_with_both_armies_present());
        assert_eq!(contested.state, IslandCampaignState::Contested);
        assert_eq!(contested.decision, IslandCampaignDecision::Contest);
        assert_eq!(
            contested.decision_reason,
            "共有資源配分前のため暫定的に現地で交戦を継続する"
        );

        let mut enemy_held_facts = facts_for_enemy_foothold();
        enemy_held_facts.enemy_combat_value = 10_000;
        let enemy_held = assess_island(&enemy_held_facts);
        assert_eq!(enemy_held.state, IslandCampaignState::EnemyHeld);
        assert_eq!(enemy_held.decision, IslandCampaignDecision::Assault);
        assert_eq!(enemy_held.required_budget, 34_500);
        assert_eq!(enemy_held.allocated_budget, 0);
        assert_eq!(
            enemy_held.decision_reason,
            "必要侵攻予算を算出し暫定的に強襲候補とする"
        );
    }

    fn allocation_assessment(
        island: usize,
        state: IslandCampaignState,
        decision: IslandCampaignDecision,
    ) -> IslandCampaignAssessment {
        IslandCampaignAssessment {
            island_id: IslandId(island),
            state,
            decision,
            state_reason: "test state".to_owned(),
            decision_reason: "test decision".to_owned(),
            pause_cause: None,
            neutral_properties: u32::from(state == IslandCampaignState::OpenNeutral),
            friendly_properties: u32::from(state == IslandCampaignState::Threatened),
            enemy_properties: u32::from(state == IslandCampaignState::EnemyHeld),
            friendly_combat_value: 0,
            enemy_combat_value: 0,
            friendly_arrival_eta: None,
            enemy_arrival_eta: None,
            friendly_capture_eta: None,
            enemy_capture_eta: None,
            expansion_payback_turns: None,
            required_budget: 0,
            allocated_budget: 0,
        }
    }

    fn expansion_candidate(island: usize, payback: u32) -> IslandCampaignCandidate {
        let mut assessment = allocation_assessment(
            island,
            IslandCampaignState::OpenNeutral,
            IslandCampaignDecision::Expand,
        );
        assessment.expansion_payback_turns = Some(payback);
        IslandCampaignCandidate {
            assessment,
            target_position: GridPosition { x: island, y: 0 },
            roi_production_sites: 0,
            transport_eta: Some(1),
            requirement: IslandCampaignRequirement {
                preferred_transport: Some(UnitType::TransportHelicopter),
                transport_slots: 2,
                capture_units: 2,
                combat_budget: 0,
                total_budget: 6_000,
            },
            minimum_combat_purchase_cost: Some(1_000),
            existing_operation: None,
        }
    }

    fn unit_candidate(
        entity: u32,
        unit_type: UnitType,
        cost: u32,
        can_capture: bool,
        available_cargo_slots: u32,
    ) -> CampaignUnitCandidate {
        CampaignUnitCandidate {
            entity: Entity::from_raw(entity),
            unit_type,
            cost,
            can_capture,
            can_secure_local_property: can_capture,
            available_cargo_slots,
            loaded_cargo_entities: Vec::new(),
            loadable_unit_types: match unit_type {
                UnitType::TransportHelicopter => vec![UnitType::Infantry, UnitType::Mech],
                UnitType::Lander => vec![
                    UnitType::Infantry,
                    UnitType::Mech,
                    UnitType::Recon,
                    UnitType::Tank,
                    UnitType::MdTank,
                    UnitType::Artillery,
                ],
                _ => Vec::new(),
            },
            is_transporting: false,
            reachable_positions: (0..=16).map(|x| GridPosition { x, y: 1 }).collect(),
            island_id: None,
            assigned_island: None,
        }
    }

    #[test]
    fn allocates_only_the_top_three_open_neutral_packages() {
        let candidates = vec![
            expansion_candidate(3, 7),
            expansion_candidate(1, 5),
            expansion_candidate(0, 4),
            expansion_candidate(2, 6),
        ];
        let initial_available_funds = 24_000;

        let portfolio = allocate_campaign_portfolio(
            candidates,
            CampaignResourcePool {
                available_funds: initial_available_funds,
                units: Vec::new(),
            },
        );

        assert_eq!(
            portfolio
                .active_offensives
                .iter()
                .map(|assignment| assignment.island_id)
                .collect::<Vec<_>>(),
            vec![IslandId(0), IslandId(1), IslandId(2)]
        );
        let reserved_funds = portfolio
            .active_offensives
            .iter()
            .fold(0_u32, |total, assignment| {
                total.saturating_add(assignment.purchase_shortfall.total_budget)
            });
        assert!(reserved_funds <= initial_available_funds);
        let rejected = portfolio
            .islands
            .iter()
            .find(|assessment| assessment.island_id == IslandId(3))
            .unwrap();
        assert_eq!(rejected.decision, IslandCampaignDecision::Observe);
        assert_eq!(rejected.allocated_budget, 0);
        assert!(portfolio.assignment_for(IslandId(3)).is_none());
    }

    #[test]
    fn allocates_each_transport_entity_to_only_one_island() {
        let candidates = vec![expansion_candidate(0, 4), expansion_candidate(1, 5)];
        let units = vec![
            unit_candidate(10, UnitType::TransportHelicopter, 4_000, false, 2),
            unit_candidate(11, UnitType::Infantry, 1_000, true, 0),
            unit_candidate(12, UnitType::Infantry, 1_000, true, 0),
            unit_candidate(13, UnitType::Infantry, 1_000, true, 0),
            unit_candidate(14, UnitType::Infantry, 1_000, true, 0),
        ];

        let portfolio = allocate_campaign_portfolio(
            candidates,
            CampaignResourcePool {
                available_funds: 4_000,
                units,
            },
        );

        let assigned_ids: Vec<u64> = portfolio
            .active_offensives
            .iter()
            .flat_map(|assignment| assignment.transport_entities.iter())
            .map(|entity| entity.to_bits())
            .collect();
        let unique_ids: HashSet<u64> = assigned_ids.iter().copied().collect();
        assert_eq!(assigned_ids.len(), unique_ids.len());
        assert_eq!(assigned_ids, vec![Entity::from_raw(10).to_bits()]);
        let capture_ids: Vec<u64> = portfolio
            .active_offensives
            .iter()
            .flat_map(|assignment| assignment.capture_entities.iter())
            .map(|entity| entity.to_bits())
            .collect();
        let unique_capture_ids: HashSet<u64> = capture_ids.iter().copied().collect();
        assert_eq!(capture_ids.len(), unique_capture_ids.len());
        assert_eq!(portfolio.active_offensives.len(), 2);
    }

    fn secure_candidate(island: usize) -> IslandCampaignCandidate {
        IslandCampaignCandidate {
            assessment: allocation_assessment(
                island,
                IslandCampaignState::Secured,
                IslandCampaignDecision::Secure,
            ),
            target_position: GridPosition { x: island, y: 0 },
            roi_production_sites: 0,
            transport_eta: Some(0),
            requirement: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                combat_budget: 0,
                total_budget: 0,
            },
            minimum_combat_purchase_cost: Some(1_000),
            existing_operation: None,
        }
    }

    fn contest_candidate(island: usize, friendly_value: u32) -> IslandCampaignCandidate {
        let mut assessment = allocation_assessment(
            island,
            IslandCampaignState::Contested,
            IslandCampaignDecision::Contest,
        );
        assessment.friendly_combat_value = friendly_value;
        assessment.enemy_combat_value = friendly_value;
        assessment.friendly_capture_eta = Some(1);
        assessment.enemy_capture_eta = Some(1);
        IslandCampaignCandidate {
            assessment,
            target_position: GridPosition { x: island, y: 1 },
            roi_production_sites: 0,
            transport_eta: Some(0),
            requirement: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                combat_budget: 0,
                total_budget: 0,
            },
            minimum_combat_purchase_cost: Some(1_000),
            existing_operation: None,
        }
    }

    fn reinforcement_candidate(island: usize, required_power: u32) -> IslandCampaignCandidate {
        let mut assessment = allocation_assessment(
            island,
            IslandCampaignState::Contested,
            IslandCampaignDecision::Reinforce,
        );
        assessment.enemy_combat_value = required_power;
        assessment.required_budget = required_power;
        IslandCampaignCandidate {
            assessment,
            target_position: GridPosition { x: island, y: 1 },
            roi_production_sites: 0,
            transport_eta: Some(0),
            requirement: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                combat_budget: required_power,
                total_budget: required_power,
            },
            minimum_combat_purchase_cost: Some(1_000),
            existing_operation: None,
        }
    }

    fn assault_candidate(island: usize) -> IslandCampaignCandidate {
        let mut assessment = allocation_assessment(
            island,
            IslandCampaignState::EnemyHeld,
            IslandCampaignDecision::Assault,
        );
        assessment.enemy_combat_value = 8_500;
        assessment.required_budget = 32_700;
        IslandCampaignCandidate {
            assessment,
            target_position: GridPosition { x: island, y: 1 },
            roi_production_sites: 0,
            transport_eta: Some(2),
            requirement: IslandCampaignRequirement {
                preferred_transport: Some(UnitType::Lander),
                transport_slots: 4,
                capture_units: 2,
                combat_budget: 10_200,
                total_budget: 32_700,
            },
            minimum_combat_purchase_cost: Some(1_000),
            existing_operation: None,
        }
    }

    fn defense_candidate(
        island: usize,
        enemy_eta: u32,
        enemy_value: u32,
    ) -> IslandCampaignCandidate {
        let mut assessment = allocation_assessment(
            island,
            IslandCampaignState::Threatened,
            IslandCampaignDecision::Defend,
        );
        assessment.enemy_arrival_eta = Some(enemy_eta);
        assessment.enemy_combat_value = enemy_value;
        IslandCampaignCandidate {
            assessment,
            target_position: GridPosition { x: island, y: 1 },
            roi_production_sites: 0,
            transport_eta: None,
            requirement: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                combat_budget: enemy_value,
                total_budget: enemy_value,
            },
            minimum_combat_purchase_cost: Some(1_000),
            existing_operation: None,
        }
    }

    #[test]
    fn rejects_transported_and_unreachable_entities_for_defense_reservation() {
        let mut unreachable = unit_candidate(13, UnitType::Tank, 7_000, false, 0);
        unreachable.island_id = Some(IslandId(0));
        // 同じ島の別tileへは到達できても、assignmentの正確な防衛座標へ届かなければ除外する。
        unreachable.reachable_positions = vec![GridPosition { x: 1, y: 1 }];
        let mut transported = unit_candidate(14, UnitType::Tank, 7_000, false, 0);
        transported.island_id = Some(IslandId(0));
        transported.reachable_positions = vec![GridPosition { x: 0, y: 1 }];
        transported.is_transporting = true;

        let portfolio = allocate_campaign_portfolio(
            vec![defense_candidate(0, 1, 7_000)],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![unreachable, transported],
            },
        );

        assert!(portfolio.defenses.is_empty());
        assert_eq!(
            portfolio
                .islands
                .iter()
                .find(|assessment| assessment.island_id == IslandId(0))
                .unwrap()
                .decision,
            IslandCampaignDecision::Observe
        );
    }

    #[test]
    fn allocates_same_island_defender_that_reaches_the_exact_assignment_target() {
        let defender = Entity::from_raw(15);
        let mut reachable = unit_candidate(15, UnitType::Tank, 7_000, false, 0);
        reachable.island_id = Some(IslandId(0));
        reachable.reachable_positions = vec![GridPosition { x: 0, y: 1 }];

        let portfolio = allocate_campaign_portfolio(
            vec![defense_candidate(0, 1, 7_000)],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![reachable],
            },
        );

        assert_eq!(portfolio.defenses.len(), 1);
        assert_eq!(portfolio.defenses[0].combat_entities, vec![defender]);
        assert!(portfolio.defenses[0].operation_ready);
    }

    #[test]
    fn allocates_no_secure_island_capture_unit_to_an_offshore_candidate() {
        let mut local_capture = unit_candidate(15, UnitType::Infantry, 1_000, true, 0);
        local_capture.island_id = Some(IslandId(0));
        local_capture.assigned_island = Some(IslandId(1));
        local_capture.can_secure_local_property = false;
        let portfolio = allocate_campaign_portfolio(
            vec![secure_candidate(0), expansion_candidate(1, 4)],
            CampaignResourcePool {
                available_funds: 1_000,
                units: vec![
                    local_capture,
                    unit_candidate(16, UnitType::TransportHelicopter, 4_000, false, 2),
                ],
            },
        );

        assert!(portfolio.active_offensives.is_empty());
        assert_eq!(
            portfolio.islands[0].decision,
            IslandCampaignDecision::Secure
        );
        assert_eq!(
            portfolio.islands[1].decision,
            IslandCampaignDecision::Observe
        );
    }

    #[test]
    fn allocates_threatened_defense_by_preempting_the_lowest_priority_offensive() {
        let candidates = vec![
            expansion_candidate(0, 4),
            expansion_candidate(1, 5),
            defense_candidate(2, 1, 6_000),
        ];

        let portfolio = allocate_campaign_portfolio(
            candidates,
            CampaignResourcePool {
                available_funds: 12_000,
                units: Vec::new(),
            },
        );

        assert_eq!(portfolio.active_offensives.len(), 1);
        assert_eq!(portfolio.active_offensives[0].island_id, IslandId(0));
        assert_eq!(portfolio.defenses.len(), 1);
        assert_eq!(portfolio.defenses[0].island_id, IslandId(2));
        assert_eq!(portfolio.defenses[0].purchase_shortfall.total_budget, 6_000);
        let preempted = portfolio
            .islands
            .iter()
            .find(|assessment| assessment.island_id == IslandId(1))
            .unwrap();
        assert_eq!(preempted.decision, IslandCampaignDecision::Observe);
        assert_eq!(
            preempted.pause_cause,
            Some(IslandCampaignPauseCause::DefensePreemption)
        );
        assert_eq!(
            preempted.decision_reason,
            "Threatened島防衛のため攻勢を一時停止"
        );
    }

    #[test]
    fn allocates_no_partial_package_when_funds_are_one_gold_short() {
        let portfolio = allocate_campaign_portfolio(
            vec![expansion_candidate(0, 4)],
            CampaignResourcePool {
                available_funds: 5_999,
                units: Vec::new(),
            },
        );

        assert!(portfolio.active_offensives.is_empty());
        assert_eq!(
            portfolio.islands[0].decision,
            IslandCampaignDecision::Observe
        );
        assert_eq!(portfolio.islands[0].allocated_budget, 0);
        assert!(portfolio.assignment_for(IslandId(0)).is_none());
        assert!(portfolio.aggregate_missing_requirements().is_empty());
    }

    #[test]
    fn allocates_offshore_transport_without_counting_recon_cargo_capacity() {
        let portfolio = allocate_campaign_portfolio(
            vec![expansion_candidate(0, 4)],
            CampaignResourcePool {
                available_funds: 4_000,
                units: vec![
                    unit_candidate(20, UnitType::Recon, 4_200, false, 2),
                    unit_candidate(21, UnitType::Infantry, 1_000, true, 0),
                    unit_candidate(22, UnitType::Infantry, 1_000, true, 0),
                ],
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert!(assignment.transport_entities.is_empty());
        assert_eq!(assignment.purchase_shortfall.transport_slots, 2);
        assert_eq!(assignment.purchase_shortfall.total_budget, 4_000);
        assert!(!assignment.operation_ready);
    }

    #[test]
    fn transport_coverage_locks_loaded_cargo_and_uses_only_remaining_slots() {
        let loaded = Entity::from_raw(23);
        let waiting = Entity::from_raw(24);
        let unrelated = Entity::from_raw(25);
        let transport = Entity::from_raw(26);
        let mut loaded_unit = unit_candidate(23, UnitType::Infantry, 1_000, true, 0);
        loaded_unit.island_id = Some(IslandId(0));
        let mut waiting_unit = unit_candidate(24, UnitType::Infantry, 1_000, true, 0);
        waiting_unit.island_id = Some(IslandId(0));
        let mut unrelated_unit = unit_candidate(25, UnitType::Infantry, 1_000, true, 0);
        unrelated_unit.island_id = Some(IslandId(0));
        let mut transport_unit = unit_candidate(26, UnitType::TransportHelicopter, 4_000, false, 2);
        transport_unit.island_id = Some(IslandId(0));
        transport_unit.loaded_cargo_entities = vec![loaded];
        let catalog = [
            loaded_unit,
            waiting_unit,
            unrelated_unit,
            transport_unit.clone(),
        ]
        .into_iter()
        .map(|unit| (unit.entity, unit))
        .collect();

        assert!(campaign_transport_package_covers(
            &[loaded, waiting],
            &[transport],
            &catalog,
        ));

        let mut unrelated_transport = transport_unit;
        unrelated_transport.loaded_cargo_entities = vec![unrelated];
        let unrelated_catalog = catalog
            .into_iter()
            .map(|(entity, unit)| {
                if entity == transport {
                    (entity, unrelated_transport.clone())
                } else {
                    (entity, unit)
                }
            })
            .collect();
        assert!(!campaign_transport_package_covers(
            &[loaded, waiting],
            &[transport],
            &unrelated_catalog,
        ));
    }

    #[test]
    fn allocates_deterministically_for_reversed_candidate_and_unit_order() {
        let candidates = vec![
            expansion_candidate(3, 7),
            expansion_candidate(1, 5),
            expansion_candidate(0, 4),
            expansion_candidate(2, 6),
        ];
        let units = vec![
            unit_candidate(30, UnitType::TransportHelicopter, 4_000, false, 2),
            unit_candidate(31, UnitType::Infantry, 1_000, true, 0),
            unit_candidate(32, UnitType::Infantry, 1_000, true, 0),
        ];
        let expected = allocate_campaign_portfolio(
            candidates.clone(),
            CampaignResourcePool {
                available_funds: 18_000,
                units: units.clone(),
            },
        );
        let mut reversed_candidates = candidates;
        reversed_candidates.reverse();
        let mut reversed_units = units;
        reversed_units.reverse();

        let actual = allocate_campaign_portfolio(
            reversed_candidates,
            CampaignResourcePool {
                available_funds: 18_000,
                units: reversed_units,
            },
        );

        assert_eq!(actual, expected);
    }

    #[test]
    fn allocates_units_already_assigned_to_the_same_target_only() {
        let mut same_target_transport =
            unit_candidate(35, UnitType::TransportHelicopter, 4_000, false, 2);
        same_target_transport.assigned_island = Some(IslandId(0));
        let mut other_target_capture = unit_candidate(36, UnitType::Infantry, 1_000, true, 0);
        other_target_capture.assigned_island = Some(IslandId(9));
        let portfolio = allocate_campaign_portfolio(
            vec![expansion_candidate(0, 4)],
            CampaignResourcePool {
                available_funds: 1_000,
                units: vec![
                    same_target_transport,
                    other_target_capture,
                    unit_candidate(37, UnitType::Infantry, 1_000, true, 0),
                ],
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert_eq!(assignment.transport_entities, vec![Entity::from_raw(35)]);
        assert_eq!(assignment.capture_entities, vec![Entity::from_raw(37)]);
        assert_eq!(assignment.purchase_shortfall.capture_units, 1);
        assert!(!assignment.capture_entities.contains(&Entity::from_raw(36)));
    }

    #[test]
    fn allocates_existing_capture_member_as_defense_combat_value() {
        let infantry = Entity::from_raw(41);
        let mut candidate = defense_candidate(0, 1, 1_000);
        candidate.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(0),
            target_position: candidate.target_position,
            transport_phase: None,
            is_forming: false,
            transport_entities: Vec::new(),
            capture_entities: vec![infantry],
            combat_entities: Vec::new(),
        });
        let mut unit = unit_candidate(41, UnitType::Infantry, 1_000, true, 0);
        unit.island_id = Some(IslandId(0));
        unit.assigned_island = Some(IslandId(0));

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![unit],
            },
        );

        let assignment = &portfolio.defenses[0];
        assert_eq!(assignment.capture_entities, vec![infantry]);
        assert!(assignment.combat_entities.is_empty());
        assert_eq!(assignment.purchase_shortfall.combat_budget, 0);
        assert_eq!(assignment.purchase_shortfall.total_budget, 0);
        assert!(assignment.operation_ready);
    }

    #[test]
    fn allocates_small_combat_remainder_with_a_real_unit_purchase_budget() {
        let infantry = Entity::from_raw(45);
        let mut candidate = defense_candidate(0, 1, 1_080);
        candidate.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(0),
            target_position: candidate.target_position,
            transport_phase: None,
            is_forming: false,
            transport_entities: Vec::new(),
            capture_entities: vec![infantry],
            combat_entities: Vec::new(),
        });
        let mut unit = unit_candidate(45, UnitType::Infantry, 1_000, true, 0);
        unit.island_id = Some(IslandId(0));
        unit.assigned_island = Some(IslandId(0));

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 1_000,
                units: vec![unit],
            },
        );

        let assignment = &portfolio.defenses[0];
        assert_eq!(assignment.purchase_shortfall.combat_budget, 80);
        assert_eq!(assignment.purchase_shortfall.total_budget, 1_000);
        assert!(!assignment.operation_ready);
    }

    #[test]
    fn rejects_combat_purchase_when_no_eligible_unit_is_producible() {
        let mut candidate = defense_candidate(0, 1, 1_080);
        candidate.minimum_combat_purchase_cost = None;

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 1_080,
                units: Vec::new(),
            },
        );

        assert!(portfolio.defenses.is_empty());
        assert!(portfolio.assignment_for(IslandId(0)).is_none());
        assert_eq!(
            portfolio.islands[0].decision,
            IslandCampaignDecision::Observe
        );
    }

    #[test]
    fn allocates_existing_capture_member_as_reinforcement_combat_value() {
        let infantry = Entity::from_raw(42);
        let mut candidate = reinforcement_candidate(0, 1_000);
        candidate.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(0),
            target_position: candidate.target_position,
            transport_phase: None,
            is_forming: false,
            transport_entities: Vec::new(),
            capture_entities: vec![infantry],
            combat_entities: Vec::new(),
        });
        let mut unit = unit_candidate(42, UnitType::Infantry, 1_000, true, 0);
        unit.island_id = Some(IslandId(0));
        unit.assigned_island = Some(IslandId(0));

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![unit],
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert_eq!(assignment.decision, IslandCampaignDecision::Reinforce);
        assert_eq!(assignment.capture_entities, vec![infantry]);
        assert!(assignment.combat_entities.is_empty());
        assert_eq!(assignment.purchase_shortfall.combat_budget, 0);
        assert_eq!(assignment.purchase_shortfall.total_budget, 0);
        assert!(assignment.operation_ready);
    }

    #[test]
    fn allocates_competitive_contest_assets_before_expand_can_consume_them() {
        let contested_infantry = Entity::from_raw(43);
        let mut infantry = unit_candidate(43, UnitType::Infantry, 1_000, true, 0);
        infantry.island_id = Some(IslandId(1));

        let candidates = vec![expansion_candidate(0, 4), contest_candidate(1, 1_000)];
        let units = vec![
            unit_candidate(44, UnitType::TransportHelicopter, 4_000, false, 2),
            infantry,
        ];
        let portfolio = allocate_campaign_portfolio(
            candidates.clone(),
            CampaignResourcePool {
                available_funds: 1_000,
                units: units.clone(),
            },
        );
        let mut reversed_candidates = candidates;
        reversed_candidates.reverse();
        let mut reversed_units = units;
        reversed_units.reverse();
        let reversed = allocate_campaign_portfolio(
            reversed_candidates,
            CampaignResourcePool {
                available_funds: 1_000,
                units: reversed_units,
            },
        );

        assert_eq!(reversed, portfolio);
        assert!(portfolio.assignment_for(IslandId(0)).is_none());
        let contest = portfolio.assignment_for(IslandId(1)).unwrap();
        assert_eq!(contest.decision, IslandCampaignDecision::Contest);
        assert_eq!(contest.capture_entities, vec![contested_infantry]);
        assert!(contest.operation_ready);
    }

    #[test]
    fn allocates_capped_contest_asset_to_defense_without_preempting_offensives() {
        let defender = Entity::from_raw(52);
        let mut contest_asset = unit_candidate(52, UnitType::Tank, 10_000, false, 0);
        contest_asset.island_id = Some(IslandId(3));
        let candidates = vec![
            expansion_candidate(0, 4),
            expansion_candidate(1, 5),
            expansion_candidate(2, 6),
            contest_candidate(3, 10_000),
            defense_candidate(4, 1, 10_000),
        ];

        let portfolio = allocate_campaign_portfolio(
            candidates,
            CampaignResourcePool {
                available_funds: 18_000,
                units: vec![contest_asset],
            },
        );

        assert_eq!(
            portfolio
                .active_offensives
                .iter()
                .map(|assignment| assignment.island_id)
                .collect::<Vec<_>>(),
            vec![IslandId(0), IslandId(1), IslandId(2)]
        );
        let capped_contest = portfolio
            .islands
            .iter()
            .find(|assessment| assessment.island_id == IslandId(3))
            .unwrap();
        assert_eq!(capped_contest.decision, IslandCampaignDecision::Observe);
        let defense = &portfolio.defenses[0];
        assert_eq!(defense.combat_entities, vec![defender]);
        assert_eq!(defense.purchase_shortfall.total_budget, 0);
        assert!(defense.operation_ready);
    }

    #[test]
    fn allocates_failed_contest_asset_to_defense_after_rollback() {
        let defender = Entity::from_raw(53);
        let mut contest = contest_candidate(0, 1_000);
        contest.requirement.combat_budget = 2_000;
        contest.requirement.total_budget = 2_000;
        let mut contest_asset = unit_candidate(53, UnitType::Infantry, 1_000, true, 0);
        contest_asset.island_id = Some(IslandId(0));

        let portfolio = allocate_campaign_portfolio(
            vec![contest, defense_candidate(1, 1, 1_000)],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![contest_asset],
            },
        );

        assert!(portfolio.active_offensives.is_empty());
        assert_eq!(
            portfolio.islands[0].decision,
            IslandCampaignDecision::Observe
        );
        let defense = &portfolio.defenses[0];
        assert_eq!(defense.combat_entities, vec![defender]);
        assert_eq!(defense.purchase_shortfall.total_budget, 0);
        assert!(defense.operation_ready);
    }

    #[test]
    fn allocates_live_lander_without_satisfying_expand_helicopter_requirement() {
        let lander = Entity::from_raw(45);
        let capture_a = Entity::from_raw(46);
        let capture_b = Entity::from_raw(47);
        let mut candidate = expansion_candidate(0, 4);
        candidate.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(0),
            target_position: candidate.target_position,
            transport_phase: Some(TransportPhase::Transit),
            is_forming: false,
            transport_entities: vec![lander],
            capture_entities: vec![capture_a, capture_b],
            combat_entities: Vec::new(),
        });
        let mut lander_unit = unit_candidate(45, UnitType::Lander, 16_500, false, 2);
        lander_unit.assigned_island = Some(IslandId(0));
        let mut capture_unit_a = unit_candidate(46, UnitType::Infantry, 1_000, true, 0);
        capture_unit_a.assigned_island = Some(IslandId(0));
        let mut capture_unit_b = unit_candidate(47, UnitType::Infantry, 1_000, true, 0);
        capture_unit_b.assigned_island = Some(IslandId(0));

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 4_000,
                units: vec![lander_unit, capture_unit_a, capture_unit_b],
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert_eq!(assignment.transport_entities, vec![lander]);
        assert_eq!(
            assignment.purchase_shortfall.preferred_transport,
            Some(UnitType::TransportHelicopter)
        );
        assert_eq!(assignment.purchase_shortfall.transport_slots, 2);
        assert_eq!(assignment.purchase_shortfall.total_budget, 4_000);
        assert!(!assignment.operation_ready);
    }

    #[test]
    fn allocates_assault_capture_units_without_double_crediting_combat_budget() {
        let lander = Entity::from_raw(48);
        let helicopter = Entity::from_raw(49);
        let capture_a = Entity::from_raw(50);
        let capture_b = Entity::from_raw(51);
        let mut candidate = assault_candidate(0);
        candidate.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(0),
            target_position: candidate.target_position,
            transport_phase: Some(TransportPhase::Transit),
            is_forming: false,
            transport_entities: vec![lander, helicopter],
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
        });
        let mut lander_unit = unit_candidate(48, UnitType::Lander, 16_500, false, 2);
        lander_unit.assigned_island = Some(IslandId(0));
        let mut helicopter_unit =
            unit_candidate(49, UnitType::TransportHelicopter, 4_000, false, 2);
        helicopter_unit.assigned_island = Some(IslandId(0));
        let units = vec![
            lander_unit,
            helicopter_unit,
            unit_candidate(50, UnitType::Infantry, 1_000, true, 0),
            unit_candidate(51, UnitType::Infantry, 1_000, true, 0),
        ];

        let underfunded = allocate_campaign_portfolio(
            vec![candidate.clone()],
            CampaignResourcePool {
                available_funds: 8_200,
                units: units.clone(),
            },
        );
        assert!(underfunded.active_offensives.is_empty());
        assert_eq!(
            underfunded.islands[0].decision,
            IslandCampaignDecision::Observe
        );

        let funded = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 10_200,
                units,
            },
        );
        let assignment = &funded.active_offensives[0];
        assert_eq!(assignment.capture_entities, vec![capture_a, capture_b]);
        assert!(assignment.combat_entities.is_empty());
        assert_eq!(assignment.purchase_shortfall.capture_units, 0);
        assert_eq!(assignment.purchase_shortfall.combat_budget, 10_200);
        assert_eq!(assignment.purchase_shortfall.total_budget, 10_200);
        assert_eq!(assignment.allocated_budget, 32_700);
        assert!(!assignment.operation_ready);
    }

    #[test]
    fn allocates_no_return_phase_transport_as_existing_or_held_capacity() {
        let transport = Entity::from_raw(38);
        let mut candidate = expansion_candidate(0, 4);
        candidate.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(0),
            target_position: candidate.target_position,
            transport_phase: Some(TransportPhase::Return),
            is_forming: false,
            transport_entities: vec![transport],
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
        });
        let mut returning_transport =
            unit_candidate(38, UnitType::TransportHelicopter, 4_000, false, 2);
        returning_transport.assigned_island = Some(IslandId(0));

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![
                    returning_transport,
                    unit_candidate(39, UnitType::Infantry, 1_000, true, 0),
                    unit_candidate(40, UnitType::Infantry, 1_000, true, 0),
                ],
            },
        );

        assert!(portfolio.active_offensives.is_empty());
        assert_eq!(
            portfolio.islands[0].decision,
            IslandCampaignDecision::Observe
        );
    }

    #[test]
    fn allocates_no_defense_combat_value_from_support_trucks() {
        let portfolio = allocate_campaign_portfolio(
            vec![defense_candidate(0, 1, 5_000)],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![unit_candidate(40, UnitType::SupplyTruck, 5_000, false, 1)],
            },
        );

        assert!(portfolio.defenses.is_empty());
        assert_eq!(
            portfolio.islands[0].decision,
            IslandCampaignDecision::Observe
        );
    }

    fn assignment_with_shortfall(
        island: usize,
        decision: IslandCampaignDecision,
        continued: bool,
        target_position: GridPosition,
    ) -> IslandCampaignAssignment {
        IslandCampaignAssignment {
            island_id: IslandId(island),
            decision,
            target_position,
            requirement: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                combat_budget: 1_000,
                total_budget: 1_000,
            },
            purchase_shortfall: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                combat_budget: 1_000,
                total_budget: 1_000,
            },
            allocated_budget: 1_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: continued,
        }
    }

    #[test]
    fn portfolio_aggregates_missing_requirements_in_production_priority_order() {
        let portfolio = IslandCampaignPortfolio {
            islands: Vec::new(),
            active_offensives: vec![
                assignment_with_shortfall(
                    4,
                    IslandCampaignDecision::Assault,
                    false,
                    GridPosition { x: 4, y: 0 },
                ),
                assignment_with_shortfall(
                    2,
                    IslandCampaignDecision::Expand,
                    false,
                    GridPosition { x: 2, y: 0 },
                ),
                assignment_with_shortfall(
                    1,
                    IslandCampaignDecision::Assault,
                    true,
                    GridPosition { x: 1, y: 0 },
                ),
                assignment_with_shortfall(
                    3,
                    IslandCampaignDecision::Reinforce,
                    false,
                    GridPosition { x: 3, y: 0 },
                ),
            ],
            defenses: vec![assignment_with_shortfall(
                0,
                IslandCampaignDecision::Defend,
                false,
                GridPosition { x: 0, y: 0 },
            )],
        };

        assert_eq!(
            portfolio
                .aggregate_missing_requirements()
                .iter()
                .map(|shortfall| (shortfall.priority_rank, shortfall.island_id))
                .collect::<Vec<_>>(),
            vec![
                (0, IslandId(0)),
                (1, IslandId(1)),
                (2, IslandId(2)),
                (3, IslandId(3)),
                (4, IslandId(4)),
            ]
        );
    }

    #[test]
    fn portfolio_reports_both_missing_assault_transport_types() {
        let mut assault = assignment_with_shortfall(
            4,
            IslandCampaignDecision::Assault,
            false,
            GridPosition { x: 4, y: 0 },
        );
        assault.purchase_shortfall.preferred_transport = Some(UnitType::Lander);
        assault.purchase_shortfall.transport_slots = 4;
        assault.purchase_shortfall.capture_units = 2;
        assault.purchase_shortfall.combat_budget = 10_200;
        assault.purchase_shortfall.total_budget = 32_700;
        let portfolio = IslandCampaignPortfolio {
            islands: Vec::new(),
            active_offensives: vec![assault],
            defenses: Vec::new(),
        };

        let shortfall = &portfolio.aggregate_missing_requirements()[0];

        assert_eq!(shortfall.light_transport_slots, 2);
        assert_eq!(shortfall.heavy_transport_slots, 2);
        assert_eq!(shortfall.reserved_budget, 32_700);
    }

    #[test]
    fn portfolio_returns_deduplicated_targets_in_assignment_order() {
        let duplicate = GridPosition { x: 2, y: 3 };
        let portfolio = IslandCampaignPortfolio {
            islands: Vec::new(),
            active_offensives: vec![
                assignment_with_shortfall(1, IslandCampaignDecision::Expand, false, duplicate),
                assignment_with_shortfall(2, IslandCampaignDecision::Reinforce, false, duplicate),
                assignment_with_shortfall(
                    3,
                    IslandCampaignDecision::Assault,
                    false,
                    GridPosition { x: 4, y: 5 },
                ),
            ],
            defenses: Vec::new(),
        };

        assert_eq!(
            portfolio.assignment_for(IslandId(2)).unwrap().island_id,
            IslandId(2)
        );
        assert_eq!(
            portfolio.offensive_target_positions(),
            vec![duplicate, GridPosition { x: 4, y: 5 }]
        );
    }

    #[test]
    fn allocates_assault_with_lander_helicopter_capture_and_combat_minimums() {
        let mut assessment = allocation_assessment(
            0,
            IslandCampaignState::EnemyHeld,
            IslandCampaignDecision::Assault,
        );
        assessment.enemy_combat_value = 8_500;
        assessment.required_budget = 32_700;
        let candidate = IslandCampaignCandidate {
            assessment,
            target_position: GridPosition { x: 0, y: 0 },
            roi_production_sites: 0,
            transport_eta: Some(2),
            requirement: IslandCampaignRequirement {
                preferred_transport: Some(UnitType::Lander),
                transport_slots: 4,
                capture_units: 2,
                combat_budget: 10_200,
                total_budget: 32_700,
            },
            minimum_combat_purchase_cost: Some(1_000),
            existing_operation: None,
        };

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 32_700,
                units: Vec::new(),
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert_eq!(
            assignment.purchase_shortfall.preferred_transport,
            Some(UnitType::Lander)
        );
        assert_eq!(assignment.purchase_shortfall.transport_slots, 4);
        assert_eq!(assignment.purchase_shortfall.capture_units, 2);
        assert_eq!(assignment.purchase_shortfall.combat_budget, 10_200);
        assert_eq!(assignment.purchase_shortfall.total_budget, 32_700);
        assert!(!assignment.operation_ready);
    }
}
