//! 作戦に予約可能な既存戦力と輸送容量の観測。
use super::campaign_analysis::pool_audit_enabled;
use super::{
    AssignedTransportState, CampaignResourcePool, CampaignUnitCandidate, Entity,
    ExistingCampaignOperation, HashMap, HashSet, IslandId, IslandMap, Map, MasterDataRegistry,
    PlayerId, PropertySnapshot, TerrainConnectivity, TransportPhase, UnitSnapshot, UnitType,
    campaign_unit_type_rank, is_live_transport_phase, merge_campaign_assignment_sources,
};
use crate::components::GridPosition;

#[cfg(test)]
mod tests;

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_standard_campaign_resource_pool(
    player_id: PlayerId,
    map: &Map,
    registry: &MasterDataRegistry,
    island_map: &IslandMap,
    properties: &[PropertySnapshot],
    units: &[UnitSnapshot],
    operations: &[ExistingCampaignOperation],
    produced_assignments: &HashMap<Entity, IslandId>,
    roadmap_assignments: &HashMap<Entity, IslandId>,
    assigned_transport_phases: &HashMap<Entity, AssignedTransportState>,
    unavailable_entities: &HashSet<Entity>,
    available_funds: u32,
) -> CampaignResourcePool {
    let mut observation = PoolObservation::new(
        player_id,
        map,
        registry,
        island_map,
        properties,
        units,
        operations,
        produced_assignments,
        roadmap_assignments,
    );
    let mut candidates = Vec::new();
    for unit in units.iter().filter(|unit| unit.faction == player_id) {
        let availability = observation
            .assignments
            .observe_availability(unit, assigned_transport_phases.get(&unit.entity));
        // 通常経路の除外条件と短絡順は、診断を持つ小規模経路から独立させる。
        if unavailable_entities.contains(&unit.entity)
            || availability.is_offshore_transport && unit.fuel.is_some_and(|fuel| fuel == 0)
            || availability.is_offshore_transport
                && (!availability.transport_is_available || availability.available_cargo_slots == 0)
            || (unit.transporting.is_some() && availability.assigned_island.is_none())
        {
            continue;
        }
        candidates.push(observation.candidate(unit, availability));
    }
    finalize_pool(available_funds, candidates)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_campaign_resource_pool(
    player_id: PlayerId,
    map: &Map,
    registry: &MasterDataRegistry,
    island_map: &IslandMap,
    properties: &[PropertySnapshot],
    units: &[UnitSnapshot],
    operations: &[ExistingCampaignOperation],
    produced_assignments: &HashMap<Entity, IslandId>,
    roadmap_assignments: &HashMap<Entity, IslandId>,
    assigned_transport_phases: &HashMap<Entity, AssignedTransportState>,
    unavailable_entities: &HashSet<Entity>,
    available_funds: u32,
    audit_turn: u32,
) -> CampaignResourcePool {
    let mut observation = PoolObservation::new(
        player_id,
        map,
        registry,
        island_map,
        properties,
        units,
        operations,
        produced_assignments,
        roadmap_assignments,
    );
    let mut candidates = Vec::new();
    for unit in units.iter().filter(|unit| unit.faction == player_id) {
        let availability = observation
            .assignments
            .observe_availability(unit, assigned_transport_phases.get(&unit.entity));
        let exclusion_reason =
            small_map_exclusion_reason(unit, &availability, unavailable_entities);
        // 除外対象も含め、従来どおり候補構築より前に診断を出す。
        if pool_audit_enabled() {
            audit_unit(
                player_id,
                audit_turn,
                island_map,
                unit,
                availability.assigned_island,
                exclusion_reason,
            );
        }
        if exclusion_reason.is_some() {
            continue;
        }
        candidates.push(observation.candidate(unit, availability));
    }
    finalize_pool(available_funds, candidates)
}

// 所属索引は観測中に不変とし、積荷の逆参照は入力snapshotへ借用で辿る。
struct PoolAssignments<'a> {
    unit_by_entity: HashMap<Entity, &'a UnitSnapshot>,
    assigned_by_entity: HashMap<Entity, IslandId>,
    assigned_entities_by_island: HashMap<IslandId, HashSet<Entity>>,
}

impl<'a> PoolAssignments<'a> {
    fn new(
        units: &'a [UnitSnapshot],
        operations: &[ExistingCampaignOperation],
        produced_assignments: &HashMap<Entity, IslandId>,
        roadmap_assignments: &HashMap<Entity, IslandId>,
    ) -> Self {
        let unit_by_entity = units.iter().map(|unit| (unit.entity, unit)).collect();
        let assigned_by_entity = merge_campaign_assignment_sources(
            roadmap_assignments,
            operations,
            produced_assignments,
        );
        let mut assigned_entities_by_island: HashMap<IslandId, HashSet<Entity>> =
            HashMap::default();
        for (entity, island_id) in &assigned_by_entity {
            assigned_entities_by_island
                .entry(*island_id)
                .or_default()
                .insert(*entity);
        }
        Self {
            unit_by_entity,
            assigned_by_entity,
            assigned_entities_by_island,
        }
    }

    fn loaded_cargo_is_assignment_owned(
        &self,
        unit: &UnitSnapshot,
        assigned_island: Option<IslandId>,
    ) -> bool {
        assigned_island.is_some_and(|island_id| {
            self.assigned_entities_by_island
                .get(&island_id)
                .is_some_and(|assigned| {
                    unit.loaded_cargo_entities.iter().all(|cargo| {
                        assigned.contains(cargo)
                            && self.unit_by_entity.get(cargo).is_some_and(|cargo_unit| {
                                cargo_unit.transporting == Some(unit.entity)
                            })
                    })
                })
        })
    }

    fn observe_availability(
        &self,
        unit: &UnitSnapshot,
        transport_state: Option<&AssignedTransportState>,
    ) -> UnitAvailability {
        let assigned_island = self.assigned_by_entity.get(&unit.entity).copied();
        let cargo_is_owned = self.loaded_cargo_is_assignment_owned(unit, assigned_island);
        let transport_is_available =
            transport_is_available(unit, transport_state, assigned_island, cargo_is_owned);
        let is_offshore_transport = matches!(
            unit.stats.unit_type,
            UnitType::TransportHelicopter | UnitType::Lander
        );
        let loaded_cargo_entities = if cargo_is_owned {
            unit.loaded_cargo_entities.clone()
        } else {
            Vec::new()
        };
        let available_cargo_slots = unit
            .free_cargo_slots
            .saturating_add(loaded_cargo_entities.len() as u32);
        UnitAvailability {
            assigned_island,
            transport_is_available,
            is_offshore_transport,
            loaded_cargo_entities,
            available_cargo_slots,
        }
    }
}

struct UnitAvailability {
    assigned_island: Option<IslandId>,
    transport_is_available: bool,
    is_offshore_transport: bool,
    loaded_cargo_entities: Vec<Entity>,
    available_cargo_slots: u32,
}

fn transport_is_available(
    unit: &UnitSnapshot,
    transport_state: Option<&AssignedTransportState>,
    assigned_island: Option<IslandId>,
    cargo_is_owned: bool,
) -> bool {
    match transport_state {
        None => unit.loaded_cargo_entities.is_empty(),
        Some(AssignedTransportState::Forming(target, cargo_is_assigned)) => {
            target.is_some() && *target == assigned_island && *cargo_is_assigned && cargo_is_owned
        }
        Some(AssignedTransportState::Phase(phase, target, cargo_is_assigned)) => {
            (is_live_transport_phase(*phase) || is_empty_return(unit, *phase))
                && target.is_some()
                && *target == assigned_island
                && (is_empty_return(unit, *phase) || (*cargo_is_assigned && cargo_is_owned))
        }
        Some(AssignedTransportState::Other) => false,
    }
}

// 空Returnだけは次便のpickupへ直接向け直し、帰投だけの空走を挟まない。
fn is_empty_return(unit: &UnitSnapshot, phase: TransportPhase) -> bool {
    phase == TransportPhase::Return && unit.loaded_cargo_entities.is_empty()
}

// targetless safe Drop、Completed、関連外cargo、実capacity無しは再予約しない。
fn small_map_exclusion_reason(
    unit: &UnitSnapshot,
    availability: &UnitAvailability,
    unavailable_entities: &HashSet<Entity>,
) -> Option<&'static str> {
    if unavailable_entities.contains(&unit.entity) {
        Some("unavailable_set")
    } else if availability.is_offshore_transport && unit.fuel.is_some_and(|fuel| fuel == 0) {
        Some("transport_fuel_zero")
    } else if availability.is_offshore_transport && !availability.transport_is_available {
        Some("transport_state_unavailable")
    } else if availability.is_offshore_transport && availability.available_cargo_slots == 0 {
        Some("transport_no_cargo_slot")
    } else if unit.transporting.is_some() && availability.assigned_island.is_none() {
        Some("loaded_cargo_without_assignment")
    } else {
        None
    }
}

struct PoolObservation<'a> {
    player_id: PlayerId,
    map: &'a Map,
    registry: &'a MasterDataRegistry,
    island_map: &'a IslandMap,
    properties: &'a [PropertySnapshot],
    assignments: PoolAssignments<'a>,
    terrain_connectivity: TerrainConnectivity,
}

impl<'a> PoolObservation<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        player_id: PlayerId,
        map: &'a Map,
        registry: &'a MasterDataRegistry,
        island_map: &'a IslandMap,
        properties: &'a [PropertySnapshot],
        units: &'a [UnitSnapshot],
        operations: &[ExistingCampaignOperation],
        produced_assignments: &HashMap<Entity, IslandId>,
        roadmap_assignments: &HashMap<Entity, IslandId>,
    ) -> Self {
        let assignments =
            PoolAssignments::new(units, operations, produced_assignments, roadmap_assignments);
        // 移動種別ごとの連結成分は候補全体で再利用し、物件・マスごとに再構築しない。
        Self {
            player_id,
            map,
            registry,
            island_map,
            properties,
            assignments,
            terrain_connectivity: TerrainConnectivity::default(),
        }
    }

    fn candidate(
        &mut self,
        unit: &UnitSnapshot,
        availability: UnitAvailability,
    ) -> CampaignUnitCandidate {
        let island_id = self
            .island_map
            .get_island_at(&unit.position)
            .map(|island| island.id);
        let can_secure_local_property = self.can_secure_local_property(unit, island_id);
        let reachable_positions = self.reachable_positions(unit);
        CampaignUnitCandidate {
            entity: unit.entity,
            unit_type: unit.stats.unit_type,
            cost: unit.stats.cost,
            can_capture: unit.stats.can_capture,
            can_secure_local_property,
            available_cargo_slots: availability.available_cargo_slots,
            loaded_cargo_entities: availability.loaded_cargo_entities,
            loadable_unit_types: unit.stats.loadable_unit_types.clone(),
            is_transporting: unit.transporting.is_some(),
            reachable_positions,
            island_id,
            assigned_island: availability.assigned_island,
        }
    }

    fn can_secure_local_property(
        &mut self,
        unit: &UnitSnapshot,
        island_id: Option<IslandId>,
    ) -> bool {
        unit.stats.can_capture
            && unit.transporting.is_none()
            && island_id.is_some_and(|island_id| {
                self.properties.iter().any(|snapshot| {
                    snapshot.property.owner_id != Some(self.player_id)
                        && self
                            .island_map
                            .get_island_at(&snapshot.position)
                            .is_some_and(|island| island.id == island_id)
                        && self.terrain_connectivity.is_reachable(
                            self.map,
                            self.registry,
                            (unit.position.x, unit.position.y),
                            (snapshot.position.x, snapshot.position.y),
                            unit.stats.movement_type,
                        )
                })
            })
    }

    fn reachable_positions(&mut self, unit: &UnitSnapshot) -> Vec<GridPosition> {
        let mut positions: Vec<_> = self
            .island_map
            .islands
            .iter()
            .flat_map(|island| island.tiles.iter())
            .filter(|target| {
                self.terrain_connectivity.is_reachable(
                    self.map,
                    self.registry,
                    (unit.position.x, unit.position.y),
                    (target.x, target.y),
                    unit.stats.movement_type,
                )
            })
            .copied()
            .collect();
        positions.sort_by_key(|position| (position.y, position.x));
        positions
    }
}

fn finalize_pool(
    available_funds: u32,
    mut candidates: Vec<CampaignUnitCandidate>,
) -> CampaignResourcePool {
    candidates.sort_by_key(|unit| {
        (
            unit.assigned_island.map(|island| island.0),
            unit.island_id.map(|island| island.0),
            unit.cost,
            campaign_unit_type_rank(unit.unit_type),
            unit.entity.to_bits(),
        )
    });
    CampaignResourcePool {
        available_funds,
        units: candidates,
    }
}

fn audit_unit(
    player_id: PlayerId,
    audit_turn: u32,
    island_map: &IslandMap,
    unit: &UnitSnapshot,
    assigned_island: Option<IslandId>,
    exclusion_reason: Option<&str>,
) {
    eprintln!(
        "POOL_AUDIT stage=producer turn={} player={:?} entity={:?} type={:?} can_capture={} pos=({},{}) island={:?} assigned_island={:?} transporting={:?} excluded={:?}",
        audit_turn,
        player_id,
        unit.entity,
        unit.stats.unit_type,
        unit.stats.can_capture,
        unit.position.x,
        unit.position.y,
        island_map
            .get_island_at(&unit.position)
            .map(|island| island.id),
        assigned_island,
        unit.transporting,
        exclusion_reason,
    );
}
