//! 作戦に予約可能な既存戦力と輸送容量の観測。
use super::campaign_analysis::pool_audit_enabled;
use super::{
    AssignedTransportState, CampaignResourcePool, CampaignUnitCandidate, Entity,
    ExistingCampaignOperation, HashMap, HashSet, IslandId, IslandMap, Map, MasterDataRegistry,
    PlayerId, PropertySnapshot, TerrainConnectivity, TransportPhase, UnitSnapshot, UnitType,
    campaign_unit_type_rank, is_live_transport_phase, merge_campaign_assignment_sources,
};

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
    let unit_by_entity: HashMap<_, _> = units.iter().map(|unit| (unit.entity, unit)).collect();
    let assigned_by_entity =
        merge_campaign_assignment_sources(roadmap_assignments, operations, produced_assignments);
    let mut assigned_entities_by_island: HashMap<IslandId, HashSet<Entity>> = HashMap::default();
    for (entity, island_id) in &assigned_by_entity {
        assigned_entities_by_island
            .entry(*island_id)
            .or_default()
            .insert(*entity);
    }

    let mut candidates = Vec::new();
    // 地形連結成分は移動種別ごとに1回だけ構築する。候補マスごとに再構築すると、
    // ユニット数×全島タイル数だけマップ全走査が発生する。
    let mut terrain_connectivity = TerrainConnectivity::default();
    for unit in units.iter().filter(|unit| unit.faction == player_id) {
        let assigned_island = assigned_by_entity.get(&unit.entity).copied();
        let transport_state = assigned_transport_phases.get(&unit.entity);
        let loaded_cargo_is_assignment_owned = assigned_island.is_some_and(|island_id| {
            assigned_entities_by_island
                .get(&island_id)
                .is_some_and(|assigned| {
                    unit.loaded_cargo_entities.iter().all(|cargo| {
                        assigned.contains(cargo)
                            && unit_by_entity.get(cargo).is_some_and(|cargo_unit| {
                                cargo_unit.transporting == Some(unit.entity)
                            })
                    })
                })
        });
        let transport_is_available = match transport_state {
            None => unit.loaded_cargo_entities.is_empty(),
            Some(AssignedTransportState::Forming(target, cargo_is_assigned)) => {
                target.is_some()
                    && *target == assigned_island
                    && *cargo_is_assigned
                    && loaded_cargo_is_assignment_owned
            }
            Some(AssignedTransportState::Phase(phase, target, cargo_is_assigned)) => {
                (is_live_transport_phase(*phase)
                    || (*phase == TransportPhase::Return && unit.loaded_cargo_entities.is_empty()))
                    && target.is_some()
                    && *target == assigned_island
                    && ((*phase == TransportPhase::Return && unit.loaded_cargo_entities.is_empty())
                        || (*cargo_is_assigned && loaded_cargo_is_assignment_owned))
            }
            Some(AssignedTransportState::Other) => false,
        };
        let is_offshore_transport = matches!(
            unit.stats.unit_type,
            UnitType::TransportHelicopter | UnitType::Lander
        );
        let loaded_cargo_entities = if loaded_cargo_is_assignment_owned {
            unit.loaded_cargo_entities.clone()
        } else {
            Vec::new()
        };
        let available_cargo_slots = unit
            .free_cargo_slots
            .saturating_add(loaded_cargo_entities.len() as u32);
        // targetless safe Drop、Completed、関連外cargo、実capacity無しの輸送役は再予約しない。
        // 空Returnだけは次便のpickupへ直接向け直し、帰投だけの空走を挟まない。
        // 【一時的な診断専用】除外理由を先に判定してから継続する。
        let exclusion_reason = if unavailable_entities.contains(&unit.entity) {
            Some("unavailable_set")
        } else if is_offshore_transport && unit.fuel.is_some_and(|fuel| fuel == 0) {
            Some("transport_fuel_zero")
        } else if is_offshore_transport && !transport_is_available {
            Some("transport_state_unavailable")
        } else if is_offshore_transport && available_cargo_slots == 0 {
            Some("transport_no_cargo_slot")
        } else if unit.transporting.is_some() && assigned_island.is_none() {
            Some("loaded_cargo_without_assignment")
        } else {
            None
        };
        if pool_audit_enabled() {
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
        if exclusion_reason.is_some() {
            continue;
        }
        let island_id = island_map
            .get_island_at(&unit.position)
            .map(|island| island.id);
        let can_secure_local_property = unit.stats.can_capture
            && unit.transporting.is_none()
            && island_id.is_some_and(|island_id| {
                properties.iter().any(|snapshot| {
                    snapshot.property.owner_id != Some(player_id)
                        && island_map
                            .get_island_at(&snapshot.position)
                            .is_some_and(|island| island.id == island_id)
                        && terrain_connectivity.is_reachable(
                            map,
                            registry,
                            (unit.position.x, unit.position.y),
                            (snapshot.position.x, snapshot.position.y),
                            unit.stats.movement_type,
                        )
                })
            });
        let mut reachable_positions: Vec<_> = island_map
            .islands
            .iter()
            .flat_map(|island| island.tiles.iter())
            .filter(|target| {
                terrain_connectivity.is_reachable(
                    map,
                    registry,
                    (unit.position.x, unit.position.y),
                    (target.x, target.y),
                    unit.stats.movement_type,
                )
            })
            .copied()
            .collect();
        reachable_positions.sort_by_key(|position| (position.y, position.x));
        candidates.push(CampaignUnitCandidate {
            entity: unit.entity,
            unit_type: unit.stats.unit_type,
            cost: unit.stats.cost,
            can_capture: unit.stats.can_capture,
            can_secure_local_property,
            available_cargo_slots,
            loaded_cargo_entities,
            loadable_unit_types: unit.stats.loadable_unit_types.clone(),
            is_transporting: unit.transporting.is_some(),
            reachable_positions,
            island_id,
            assigned_island,
        });
    }
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
