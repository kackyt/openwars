use crate::ai::island_campaign::{
    CampaignResourcePool, CampaignSustainmentCoverage, CampaignSustainmentTargets,
    CampaignTransportBlueprint, CampaignUnitCandidate, ExistingCampaignOperation,
    IslandCampaignCandidate, IslandCampaignDecision, IslandCampaignFacts, IslandCampaignPortfolio,
    IslandCampaignRequirement, IslandCampaignState, allocate_campaign_portfolio, assess_island,
    campaign_unit_type_rank, promote_logistics_prerequisite,
};
use crate::ai::islands::{Island, IslandId, IslandMap};
use crate::ai::squad::{MissionPhase, MissionType, SquadManager, TransportPhase};
use crate::ai::turn_distance::{
    TerrainConnectivity, TurnDistanceCache, calculate_turn_distance, is_terrain_reachable,
};
use crate::components::{
    CargoCapacity, Faction, Fuel, GridPosition, Health, PlayerId, Property, Transporting, UnitStats,
};
use crate::resources::master_data::{MasterDataRegistry, UnitName};
use crate::resources::{Map, MatchState, MovementType, Players, Terrain, UnitType};
use crate::systems::movement::{OccupantInfo, get_valid_movement_cost};
use bevy_ecs::prelude::{Entity, World};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
struct PropertySnapshot {
    position: GridPosition,
    property: Property,
}

#[derive(Debug, Clone)]
struct UnitSnapshot {
    entity: Entity,
    faction: PlayerId,
    position: GridPosition,
    stats: UnitStats,
    health: Health,
    fuel: Option<u32>,
    transporting: Option<Entity>,
    free_cargo_slots: u32,
    loaded_cargo_entities: Vec<Entity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssignedTransportState {
    Forming(Option<IslandId>, bool),
    Phase(TransportPhase, Option<IslandId>, bool),
    Other,
}

#[derive(Debug, Clone)]
struct CargoAssignment {
    island_id: IslandId,
    cargo_entities: Vec<Entity>,
    pickup_position: Option<GridPosition>,
    drop_position: Option<GridPosition>,
    transport_entity: Entity,
    phase: TransportPhase,
}

#[derive(Debug, Clone)]
struct IslandAccumulator {
    facts: IslandCampaignFacts,
    properties: Vec<PropertySnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportSource {
    Held(Entity),
    Producible(GridPosition),
}

#[derive(Debug, Clone, Copy)]
struct TransportOption {
    unit_type: UnitType,
    cost: u32,
    eta: u32,
    source: TransportSource,
}

fn min_option(current: Option<u32>, candidate: Option<u32>) -> Option<u32> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (None, Some(candidate)) => Some(candidate),
        (current, None) => current,
    }
}

fn ceil_div(numerator: u32, denominator: u32) -> u32 {
    if denominator == 0 {
        return u32::MAX;
    }
    numerator / denominator + u32::from(!numerator.is_multiple_of(denominator))
}

fn unit_stats_for_type(registry: &MasterDataRegistry, unit_type: UnitType) -> Option<UnitStats> {
    registry
        .create_unit_stats(&UnitName(unit_type.as_str().to_owned()))
        .ok()
}

fn is_support_only(unit_type: UnitType) -> bool {
    matches!(
        unit_type,
        UnitType::TransportHelicopter | UnitType::Lander | UnitType::SupplyTruck
    )
}

fn is_live_transport_phase(phase: TransportPhase) -> bool {
    matches!(
        phase,
        TransportPhase::Pickup | TransportPhase::Transit | TransportPhase::Drop
    )
}

fn capture_turns(property: &Property, health: Health) -> u32 {
    let display_hp = health.current.saturating_add(9) / 10;
    let capture_power = display_hp.saturating_mul(10);
    ceil_div(property.capture_points, capture_power)
}

fn default_capture_turns(property: &Property) -> u32 {
    ceil_div(property.capture_points, 100)
}

fn sorted_island_tiles(island: &Island) -> Vec<GridPosition> {
    let mut tiles: Vec<_> = island.tiles.iter().copied().collect();
    tiles.sort_by_key(|position| (position.y, position.x));
    tiles
}

fn movement_targets_for_island(
    map: &Map,
    registry: &MasterDataRegistry,
    island: &Island,
    movement_type: MovementType,
) -> Vec<GridPosition> {
    let mut targets = Vec::new();
    for tile in sorted_island_tiles(island) {
        if map
            .get_terrain(tile.x, tile.y)
            .and_then(|terrain| get_valid_movement_cost(registry, movement_type, terrain))
            .is_some()
        {
            targets.push(tile);
        }
        if movement_type == MovementType::Ship {
            for (x, y) in map.get_adjacent(tile.x, tile.y) {
                if map
                    .get_terrain(x, y)
                    .and_then(|terrain| get_valid_movement_cost(registry, movement_type, terrain))
                    .is_some()
                {
                    targets.push(GridPosition { x, y });
                }
            }
        }
    }
    targets.sort_by_key(|position| (position.y, position.x));
    targets.dedup();
    targets
}

#[allow(clippy::too_many_arguments)]
fn eta_between(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    connectivity: &mut TerrainConnectivity,
    cache: &mut TurnDistanceCache,
    player_id: PlayerId,
    start: GridPosition,
    target: GridPosition,
    stats: &UnitStats,
) -> Option<u32> {
    if start.x >= map.width || start.y >= map.height {
        return None;
    }
    if !connectivity.is_reachable(
        map,
        registry,
        (start.x, start.y),
        (target.x, target.y),
        stats.movement_type,
    ) {
        return None;
    }
    let distance = calculate_turn_distance(
        map,
        registry,
        unit_positions,
        (start.x, start.y),
        (target.x, target.y),
        stats.movement_type,
        stats.max_movement,
        0,
        player_id,
        cache,
    );
    (distance.used_mp != u32::MAX).then_some(distance.turns)
}

#[allow(clippy::too_many_arguments)]
fn unit_eta_to_island(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    connectivity: &mut TerrainConnectivity,
    cache: &mut TurnDistanceCache,
    unit: &UnitSnapshot,
    island: &Island,
) -> Option<u32> {
    if island.tiles.contains(&unit.position) {
        return Some(0);
    }
    movement_targets_for_island(map, registry, island, unit.stats.movement_type)
        .into_iter()
        .filter_map(|target| {
            eta_between(
                map,
                registry,
                unit_positions,
                connectivity,
                cache,
                unit.faction,
                unit.position,
                target,
                &unit.stats,
            )
        })
        .min()
}

#[allow(clippy::too_many_arguments)]
fn unit_eta_to_drop(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    connectivity: &mut TerrainConnectivity,
    cache: &mut TurnDistanceCache,
    unit: &UnitSnapshot,
    drop_position: GridPosition,
) -> Option<u32> {
    let mut targets = Vec::new();
    if map
        .get_terrain(drop_position.x, drop_position.y)
        .and_then(|terrain| get_valid_movement_cost(registry, unit.stats.movement_type, terrain))
        .is_some()
    {
        targets.push(drop_position);
    }
    if unit.stats.movement_type == MovementType::Ship {
        for (x, y) in map.get_adjacent(drop_position.x, drop_position.y) {
            if map
                .get_terrain(x, y)
                .and_then(|terrain| {
                    get_valid_movement_cost(registry, unit.stats.movement_type, terrain)
                })
                .is_some()
            {
                targets.push(GridPosition { x, y });
            }
        }
    }
    targets.sort_by_key(|target| (target.y, target.x));
    targets.dedup();
    targets
        .into_iter()
        .filter_map(|target| {
            eta_between(
                map,
                registry,
                unit_positions,
                connectivity,
                cache,
                unit.faction,
                unit.position,
                target,
                &unit.stats,
            )
        })
        .min()
}

#[allow(clippy::too_many_arguments)]
fn capture_eta_from_position(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    connectivity: &mut TerrainConnectivity,
    cache: &mut TurnDistanceCache,
    unit: &UnitSnapshot,
    start: GridPosition,
    properties: &[PropertySnapshot],
) -> Option<u32> {
    if !unit.stats.can_capture {
        return None;
    }
    properties
        .iter()
        .filter(|snapshot| snapshot.property.owner_id != Some(unit.faction))
        .filter_map(|snapshot| {
            let movement_eta = eta_between(
                map,
                registry,
                unit_positions,
                connectivity,
                cache,
                unit.faction,
                start,
                snapshot.position,
                &unit.stats,
            )?;
            Some(movement_eta.saturating_add(capture_turns(&snapshot.property, unit.health)))
        })
        .min()
}

#[allow(clippy::too_many_arguments)]
fn conservative_capture_eta_from_island(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    connectivity: &mut TerrainConnectivity,
    cache: &mut TurnDistanceCache,
    unit: &UnitSnapshot,
    island: &Island,
    properties: &[PropertySnapshot],
) -> Option<u32> {
    let mut worst_eta = None;
    for landing in sorted_island_tiles(island) {
        if map
            .get_terrain(landing.x, landing.y)
            .and_then(|terrain| {
                get_valid_movement_cost(registry, unit.stats.movement_type, terrain)
            })
            .is_none()
        {
            continue;
        }
        // Drop地点が未確定なら、候補島内の最も遅い地上経路を採用して楽観的なETAを避ける。
        let eta = capture_eta_from_position(
            map,
            registry,
            unit_positions,
            connectivity,
            cache,
            unit,
            landing,
            properties,
        )?;
        worst_eta = Some(worst_eta.map_or(eta, |current: u32| current.max(eta)));
    }
    worst_eta
}

fn collect_unit_snapshots(world: &mut World) -> Vec<UnitSnapshot> {
    let mut snapshots = Vec::new();
    let mut query = world.query::<(
        Entity,
        &Faction,
        &GridPosition,
        &UnitStats,
        &Health,
        Option<&Fuel>,
        Option<&Transporting>,
        Option<&CargoCapacity>,
    )>();
    for (entity, faction, position, stats, health, fuel, transporting, cargo) in query.iter(world) {
        let free_cargo_slots = cargo
            .map(|cargo| cargo.max.saturating_sub(cargo.loaded.len() as u32))
            .unwrap_or(0);
        let mut loaded_cargo_entities = cargo.map(|cargo| cargo.loaded.clone()).unwrap_or_default();
        loaded_cargo_entities.sort_by_key(|entity| entity.to_bits());
        loaded_cargo_entities.dedup();
        snapshots.push(UnitSnapshot {
            entity,
            faction: faction.0,
            position: *position,
            stats: stats.clone(),
            health: *health,
            fuel: fuel.map(|fuel| fuel.current),
            transporting: transporting.map(|transporting| transporting.0),
            free_cargo_slots,
            loaded_cargo_entities,
        });
    }
    snapshots.sort_by_key(|snapshot| snapshot.entity.to_bits());
    snapshots
}

fn collect_faction_snapshots(world: &mut World) -> Vec<(Entity, PlayerId)> {
    let mut snapshots = Vec::new();
    let mut query = world.query::<(Entity, &Faction)>();
    for (entity, faction) in query.iter(world) {
        snapshots.push((entity, faction.0));
    }
    snapshots.sort_by_key(|(entity, faction)| (entity.to_bits(), faction.0));
    snapshots
}

fn mixed_owner_squad_entities(
    manager: &SquadManager,
    factions: &[(Entity, PlayerId)],
) -> HashSet<Entity> {
    let faction_by_entity: HashMap<_, _> = factions.iter().copied().collect();
    let mut squads: Vec<_> = manager.squads.iter().collect();
    squads.sort_by_key(|squad| squad.id.0);
    let mut unavailable = HashSet::new();
    for squad in squads {
        let mut entities: Vec<_> = squad
            .transport_entity
            .into_iter()
            .chain(squad.members.iter().copied())
            .chain(squad.cargo_entities.iter().copied())
            .chain(squad.delivered_cargo.iter().copied())
            .collect();
        entities.sort_by_key(|entity| entity.to_bits());
        entities.dedup();
        let mut owners: Vec<_> = squad
            .owner_id
            .into_iter()
            .chain(
                entities
                    .iter()
                    .filter_map(|entity| faction_by_entity.get(entity).copied()),
            )
            .collect();
        owners.sort_by_key(|owner| owner.0);
        owners.dedup();
        if owners.len() > 1 {
            // 明示ownerと参照Factionの競合もmixedとして扱い、どのplayer分析でも二重予約しない。
            unavailable.extend(entities);
        }
    }
    unavailable
}

fn campaign_unit_snapshots(
    manager: &SquadManager,
    units: Vec<UnitSnapshot>,
    factions: &[(Entity, PlayerId)],
) -> Vec<UnitSnapshot> {
    let unavailable = mixed_owner_squad_entities(manager, factions);
    units
        .into_iter()
        .filter(|unit| !unavailable.contains(&unit.entity))
        .collect()
}

fn collect_property_snapshots(world: &mut World) -> Vec<PropertySnapshot> {
    let mut snapshots = Vec::new();
    let mut query = world.query::<(&GridPosition, &Property)>();
    for (position, property) in query.iter(world) {
        snapshots.push(PropertySnapshot {
            position: *position,
            property: *property,
        });
    }
    snapshots.sort_by_key(|snapshot| (snapshot.position.y, snapshot.position.x));
    snapshots
}

fn assigned_transport_state_rank(state: AssignedTransportState) -> u8 {
    match state {
        AssignedTransportState::Phase(phase, _, _) if is_live_transport_phase(phase) => 0,
        AssignedTransportState::Forming(_, _) => 1,
        AssignedTransportState::Phase(_, _, _) => 2,
        AssignedTransportState::Other => 3,
    }
}

fn collect_assigned_transport_phases(
    manager: &SquadManager,
    units: &[UnitSnapshot],
    player_id: PlayerId,
) -> HashMap<Entity, AssignedTransportState> {
    let unit_by_entity: HashMap<_, _> = units.iter().map(|unit| (unit.entity, unit)).collect();
    let mut squads: Vec<_> = manager.squads.iter().collect();
    squads.sort_by_key(|squad| squad.id.0);
    let mut phases: HashMap<Entity, AssignedTransportState> = HashMap::new();
    for squad in squads {
        let Some(transport) = squad.transport_entity.filter(|transport| {
            unit_by_entity
                .get(transport)
                .is_some_and(|unit| unit.faction == player_id)
        }) else {
            continue;
        };
        let loaded_cargo_is_assigned = unit_by_entity.get(&transport).is_some_and(|unit| {
            unit.loaded_cargo_entities.iter().all(|cargo| {
                squad.cargo_entities.contains(cargo)
                    && unit_by_entity
                        .get(cargo)
                        .is_some_and(|cargo_unit| cargo_unit.transporting == Some(transport))
            })
        });
        let state = match &squad.phase {
            MissionPhase::Transport(phase) => {
                AssignedTransportState::Phase(*phase, squad.target_island, loaded_cargo_is_assigned)
            }
            MissionPhase::Forming => {
                AssignedTransportState::Forming(squad.target_island, loaded_cargo_is_assigned)
            }
            _ => AssignedTransportState::Other,
        };
        phases
            .entry(transport)
            .and_modify(|current| {
                if assigned_transport_state_rank(state) < assigned_transport_state_rank(*current) {
                    *current = state;
                }
            })
            .or_insert(state);
    }
    phases
}

fn transport_cargo_is_associated(
    phase: &MissionPhase,
    transport: Entity,
    cargo: Entity,
    unit_by_entity: &HashMap<Entity, &UnitSnapshot>,
) -> bool {
    let Some(transport_unit) = unit_by_entity.get(&transport) else {
        return false;
    };
    let Some(cargo_unit) = unit_by_entity.get(&cargo) else {
        return false;
    };
    let physically_loaded = cargo_unit.transporting == Some(transport)
        && transport_unit.loaded_cargo_entities.contains(&cargo);
    match phase {
        MissionPhase::Forming | MissionPhase::Transport(TransportPhase::Pickup) => {
            cargo_unit.transporting.is_none() || physically_loaded
        }
        MissionPhase::Transport(TransportPhase::Transit | TransportPhase::Drop) => {
            physically_loaded
        }
        MissionPhase::Transport(TransportPhase::Return)
        | MissionPhase::MovingToTarget
        | MissionPhase::Executing
        | MissionPhase::Completed => false,
    }
}

fn collect_existing_operations(
    island_map: &IslandMap,
    manager: &SquadManager,
    units: &[UnitSnapshot],
    player_id: PlayerId,
) -> (
    Vec<ExistingCampaignOperation>,
    HashMap<Entity, CargoAssignment>,
) {
    let unit_by_entity: HashMap<_, _> = units.iter().map(|unit| (unit.entity, unit)).collect();
    let mut squads: Vec<_> = manager.squads.iter().collect();
    squads.sort_by_key(|squad| squad.id.0);
    let mut operations: HashMap<IslandId, ExistingCampaignOperation> = HashMap::new();
    let mut cargo_assignments = HashMap::new();

    for squad in squads {
        // Reserveは次作戦へ再徴用できる待機配置であり、島作戦へcommit済みの
        // 戦力として不足数やoperation_readyへ数えてはならない。
        if squad.mission_type == MissionType::Reserve {
            continue;
        }
        let owned_transport = squad.transport_entity.filter(|transport| {
            unit_by_entity
                .get(transport)
                .is_some_and(|unit| unit.faction == player_id)
        });
        let mut owned_assigned_entities: Vec<_> = squad
            .members
            .iter()
            .filter(|entity| {
                unit_by_entity
                    .get(entity)
                    .is_some_and(|unit| unit.faction == player_id)
            })
            .copied()
            .collect();
        if let Some(transport) = owned_transport {
            owned_assigned_entities.extend(squad.cargo_entities.iter().filter_map(|cargo| {
                unit_by_entity
                    .get(cargo)
                    .is_some_and(|unit| unit.faction == player_id)
                    .then_some(())?;
                transport_cargo_is_associated(&squad.phase, transport, *cargo, &unit_by_entity)
                    .then_some(*cargo)
            }));
        } else if squad.mission_type == MissionType::Transport
            && squad.phase == MissionPhase::Forming
        {
            // 部分出航後に残った「cargoだけの後続便」も作戦継続の正本である。
            // transportが未調達だからとここで捨てると、次手番に別島へ再予約される。
            owned_assigned_entities.extend(squad.cargo_entities.iter().filter_map(|cargo| {
                unit_by_entity
                    .get(cargo)
                    .is_some_and(|unit| unit.faction == player_id)
                    .then_some(*cargo)
            }));
        }
        owned_assigned_entities.extend(squad.delivered_cargo.iter().filter_map(|cargo| {
            unit_by_entity
                .get(cargo)
                .is_some_and(|unit| unit.faction == player_id && unit.transporting.is_none())
                .then_some(*cargo)
        }));
        owned_assigned_entities.sort_by_key(|entity| entity.to_bits());
        owned_assigned_entities.dedup();
        if owned_transport.is_none() && owned_assigned_entities.is_empty() {
            continue;
        }

        let island_id = squad.target_island.or_else(|| {
            squad
                .target
                .and_then(|target| island_map.get_island_at(&target).map(|island| island.id))
        });
        let Some(island_id) = island_id else {
            continue;
        };
        let Some(island) = island_map
            .islands
            .iter()
            .find(|island| island.id == island_id)
        else {
            continue;
        };
        let target_position = squad
            .target
            .or_else(|| sorted_island_tiles(island).first().copied());
        let Some(target_position) = target_position else {
            continue;
        };
        let squad_transport_phase = match (&squad.phase, owned_transport) {
            (MissionPhase::Transport(phase), Some(_)) => Some(*phase),
            _ => None,
        };
        let squad_is_forming =
            squad.mission_type == MissionType::Transport && squad.phase == MissionPhase::Forming;
        let operation = operations
            .entry(island_id)
            .or_insert_with(|| ExistingCampaignOperation {
                island_id,
                target_position,
                transport_phase: squad_transport_phase,
                is_forming: squad_is_forming,
                transport_entities: Vec::new(),
                capture_entities: Vec::new(),
                combat_entities: Vec::new(),
            });
        if (target_position.y, target_position.x)
            < (operation.target_position.y, operation.target_position.x)
        {
            operation.target_position = target_position;
        }
        operation.is_forming |= squad_is_forming;
        if let Some(phase) = squad_transport_phase {
            let current_is_live = operation.transport_phase.is_some_and(|current| {
                matches!(
                    current,
                    TransportPhase::Pickup | TransportPhase::Transit | TransportPhase::Drop
                )
            });
            let phase_is_live = matches!(
                phase,
                TransportPhase::Pickup | TransportPhase::Transit | TransportPhase::Drop
            );
            if operation.transport_phase.is_none() || (!current_is_live && phase_is_live) {
                operation.transport_phase = Some(phase);
            }
        }

        let squad_has_live_continuity = if squad.mission_type == MissionType::Transport {
            let reusable_empty_return = squad_transport_phase == Some(TransportPhase::Return)
                && owned_transport
                    .and_then(|transport| unit_by_entity.get(&transport))
                    .is_some_and(|unit| unit.loaded_cargo_entities.is_empty());
            squad_is_forming
                || squad_transport_phase.is_some_and(is_live_transport_phase)
                || reusable_empty_return
        } else {
            squad.phase != MissionPhase::Completed
        };
        if squad_has_live_continuity {
            if let Some(transport) = owned_transport {
                operation.transport_entities.push(transport);
            }
            for entity in &owned_assigned_entities {
                let Some(unit) = unit_by_entity.get(entity) else {
                    continue;
                };
                if Some(*entity) == owned_transport || is_support_only(unit.stats.unit_type) {
                    continue;
                }
                if unit.stats.can_capture {
                    operation.capture_entities.push(*entity);
                } else {
                    operation.combat_entities.push(*entity);
                }
            }
        }

        if let (MissionPhase::Transport(phase), Some(transport_entity), true) =
            (&squad.phase, owned_transport, squad_has_live_continuity)
        {
            let mut live_cargo: Vec<_> = squad
                .cargo_entities
                .iter()
                .filter(|cargo| {
                    unit_by_entity
                        .get(cargo)
                        .is_some_and(|unit| unit.faction == player_id)
                        && transport_cargo_is_associated(
                            &squad.phase,
                            transport_entity,
                            **cargo,
                            &unit_by_entity,
                        )
                })
                .copied()
                .collect();
            live_cargo.sort_by_key(|entity| entity.to_bits());
            live_cargo.dedup();
            for cargo in &live_cargo {
                cargo_assignments.insert(
                    *cargo,
                    CargoAssignment {
                        island_id,
                        cargo_entities: live_cargo.clone(),
                        pickup_position: squad.pickup_position,
                        drop_position: squad.drop_position,
                        transport_entity,
                        phase: *phase,
                    },
                );
            }
        }
    }

    let mut operations: Vec<_> = operations.into_values().collect();
    for operation in &mut operations {
        operation
            .transport_entities
            .sort_by_key(|entity| entity.to_bits());
        operation.transport_entities.dedup();
        operation
            .capture_entities
            .sort_by_key(|entity| entity.to_bits());
        operation.capture_entities.dedup();
        operation
            .combat_entities
            .sort_by_key(|entity| entity.to_bits());
        operation.combat_entities.dedup();
    }
    operations.sort_by_key(|operation| operation.island_id.0);
    (operations, cargo_assignments)
}

fn all_master_unit_types(registry: &MasterDataRegistry) -> Vec<UnitType> {
    let mut unit_types = Vec::new();
    for name in &registry.unit_order {
        if let Ok(unit_type) = registry.unit_type_for_name(&name.0) {
            unit_types.push(unit_type);
        }
    }
    unit_types.sort_by_key(|unit_type| campaign_unit_type_rank(*unit_type));
    unit_types.dedup();
    unit_types
}

fn is_strategic_site(terrain: Terrain) -> bool {
    matches!(
        terrain,
        Terrain::Capital | Terrain::Factory | Terrain::Port | Terrain::Airport
    )
}

fn is_roi_site(terrain: Terrain) -> bool {
    matches!(terrain, Terrain::Factory | Terrain::Port | Terrain::Airport)
}

/// 島内の占領可能施設を、地上・航空・海上ユニットの補給カテゴリ別に集計する。
/// 首都・都市・工場は地上、空港は航空、港は海上の継戦拠点として扱う。
fn sustainment_site_counts(island: &Island, properties: &[PropertySnapshot]) -> (u32, u32, u32) {
    let mut ground = 0_u32;
    let mut air = 0_u32;
    let mut sea = 0_u32;
    for snapshot in properties
        .iter()
        .filter(|snapshot| island.tiles.contains(&snapshot.position))
        .filter(|snapshot| snapshot.property.max_capture_points > 0)
    {
        match snapshot.property.terrain {
            Terrain::Capital | Terrain::City | Terrain::Factory => {
                ground = ground.saturating_add(1)
            }
            Terrain::Airport => air = air.saturating_add(1),
            Terrain::Port => sea = sea.saturating_add(1),
            _ => {}
        }
    }
    (ground, air, sea)
}

fn sustainment_targets(
    island: &Island,
    properties: &[PropertySnapshot],
    player_id: PlayerId,
) -> CampaignSustainmentTargets {
    let mut targets = CampaignSustainmentTargets::default();
    let mut snapshots: Vec<_> = properties
        .iter()
        .filter(|snapshot| island.tiles.contains(&snapshot.position))
        .filter(|snapshot| {
            snapshot.property.max_capture_points > 0
                && snapshot.property.owner_id != Some(player_id)
        })
        .collect();
    snapshots.sort_by_key(|snapshot| (snapshot.position.y, snapshot.position.x));
    for snapshot in snapshots {
        match snapshot.property.terrain {
            Terrain::Capital | Terrain::City | Terrain::Factory => {
                targets.ground.get_or_insert(snapshot.position);
            }
            Terrain::Airport => {
                targets.air.get_or_insert(snapshot.position);
            }
            Terrain::Port => {
                targets.sea.get_or_insert(snapshot.position);
            }
            _ => {}
        }
    }
    targets
}

#[allow(clippy::too_many_arguments)]
fn transport_options_for_island(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    connectivity: &mut TerrainConnectivity,
    cache: &mut TurnDistanceCache,
    player_id: PlayerId,
    island: &Island,
    units: &[UnitSnapshot],
    assigned_transport_phases: &HashMap<Entity, AssignedTransportState>,
    owned_properties: &[PropertySnapshot],
) -> Vec<TransportOption> {
    let mut options = Vec::new();
    for unit in units.iter().filter(|unit| {
        let phase_allows_continuity = match assigned_transport_phases.get(&unit.entity) {
            None => unit.loaded_cargo_entities.is_empty() && unit.free_cargo_slots > 0,
            Some(AssignedTransportState::Forming(target, cargo_is_assigned)) => {
                *cargo_is_assigned && *target == Some(island.id)
            }
            Some(AssignedTransportState::Phase(phase, target, cargo_is_assigned)) => {
                *cargo_is_assigned && is_live_transport_phase(*phase) && *target == Some(island.id)
            }
            Some(AssignedTransportState::Other) => false,
        };
        let has_actual_capacity =
            unit.free_cargo_slots > 0 || !unit.loaded_cargo_entities.is_empty();
        unit.faction == player_id
            && unit.transporting.is_none()
            && unit.fuel.is_none_or(|fuel| fuel > 0)
            && phase_allows_continuity
            && has_actual_capacity
            && matches!(
                unit.stats.unit_type,
                UnitType::TransportHelicopter | UnitType::Lander
            )
    }) {
        if let Some(eta) = unit_eta_to_island(
            map,
            registry,
            unit_positions,
            connectivity,
            cache,
            unit,
            island,
        ) {
            options.push(TransportOption {
                unit_type: unit.stats.unit_type,
                cost: unit.stats.cost,
                eta,
                source: TransportSource::Held(unit.entity),
            });
        }
    }

    for unit_type in [UnitType::TransportHelicopter, UnitType::Lander] {
        let Some(stats) = unit_stats_for_type(registry, unit_type) else {
            continue;
        };
        for property in owned_properties.iter().filter(|property| {
            registry.can_produce_unit(property.property.terrain.as_str(), unit_type)
        }) {
            let production_unit = UnitSnapshot {
                entity: Entity::PLACEHOLDER,
                faction: player_id,
                position: property.position,
                stats: stats.clone(),
                health: Health {
                    current: 100,
                    max: 100,
                },
                fuel: Some(stats.max_fuel),
                transporting: None,
                free_cargo_slots: stats.max_cargo,
                loaded_cargo_entities: Vec::new(),
            };
            if let Some(eta) = unit_eta_to_island(
                map,
                registry,
                unit_positions,
                connectivity,
                cache,
                &production_unit,
                island,
            ) {
                options.push(TransportOption {
                    unit_type,
                    cost: stats.cost,
                    // 生産可能ユニットは盤上にまだ存在しないため、完成までの1ターンを保守的に加える。
                    eta: eta.saturating_add(1),
                    source: TransportSource::Producible(property.position),
                });
            }
        }
    }
    options.sort_by_key(transport_fallback_key);
    options
}

fn transport_source_key(source: TransportSource) -> (u8, u64, usize, usize) {
    match source {
        TransportSource::Held(entity) => (0, entity.to_bits(), 0, 0),
        TransportSource::Producible(position) => (1, 0, position.y, position.x),
    }
}

fn transport_fallback_key(option: &TransportOption) -> (u32, u8, u8, u64, usize, usize, u32) {
    let (source_rank, entity_bits, y, x) = transport_source_key(option.source);
    (
        option.cost,
        campaign_unit_type_rank(option.unit_type),
        source_rank,
        entity_bits,
        y,
        x,
        option.eta,
    )
}

fn select_transport_option(
    options: &[TransportOption],
    prefer_producible_helicopter: bool,
) -> Option<TransportOption> {
    let helicopter_is_producible = options.iter().any(|option| {
        option.unit_type == UnitType::TransportHelicopter
            && matches!(option.source, TransportSource::Producible(_))
    });
    if prefer_producible_helicopter && helicopter_is_producible {
        // OpenNeutralだけは生産可能性を確認した上で輸送ヘリmodeを優先し、保有機があれば先に使う。
        return options
            .iter()
            .filter(|option| option.unit_type == UnitType::TransportHelicopter)
            .min_by_key(|option| {
                let (source_rank, entity_bits, y, x) = transport_source_key(option.source);
                (source_rank, option.cost, entity_bits, y, x, option.eta)
            })
            .copied();
    }
    options
        .iter()
        .min_by_key(|option| transport_fallback_key(option))
        .copied()
}

#[allow(clippy::too_many_arguments)]
fn operation_transport_eta(
    assignment: CargoAssignment,
    units: &[UnitSnapshot],
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    connectivity: &mut TerrainConnectivity,
    cache: &mut TurnDistanceCache,
    island: &Island,
) -> Option<u32> {
    if assignment.phase == TransportPhase::Return {
        return None;
    }
    if assignment.phase == TransportPhase::Drop {
        return Some(0);
    }
    let transport = units
        .iter()
        .find(|unit| unit.entity == assignment.transport_entity)?;
    if assignment.phase == TransportPhase::Pickup {
        let pickup_position = assignment.pickup_position?;
        let pickup_eta = eta_between(
            map,
            registry,
            unit_positions,
            connectivity,
            cache,
            transport.faction,
            transport.position,
            pickup_position,
            &transport.stats,
        )?;
        let mut cargo_eta = 0;
        for cargo_entity in &assignment.cargo_entities {
            let cargo = units.iter().find(|unit| unit.entity == *cargo_entity)?;
            let eta = if cargo.transporting == Some(transport.entity) {
                0
            } else {
                eta_between(
                    map,
                    registry,
                    unit_positions,
                    connectivity,
                    cache,
                    cargo.faction,
                    cargo.position,
                    pickup_position,
                    &cargo.stats,
                )?
            };
            cargo_eta = cargo_eta.max(eta);
        }
        let mut after_pickup = transport.clone();
        after_pickup.position = pickup_position;
        let transit_eta = if let Some(drop_position) = assignment.drop_position {
            unit_eta_to_drop(
                map,
                registry,
                unit_positions,
                connectivity,
                cache,
                &after_pickup,
                drop_position,
            )
        } else {
            unit_eta_to_island(
                map,
                registry,
                unit_positions,
                connectivity,
                cache,
                &after_pickup,
                island,
            )
        }?;
        // transportとcargoの遅い側が合流するまで待ち、搭載後に目標へ向かう。
        return Some(
            pickup_eta
                .max(cargo_eta)
                .saturating_add(1)
                .saturating_add(transit_eta),
        );
    }
    if let Some(drop_position) = assignment.drop_position {
        unit_eta_to_drop(
            map,
            registry,
            unit_positions,
            connectivity,
            cache,
            transport,
            drop_position,
        )
    } else {
        unit_eta_to_island(
            map,
            registry,
            unit_positions,
            connectivity,
            cache,
            transport,
            island,
        )
    }
}

fn missing_expansion_package_cost(
    registry: &MasterDataRegistry,
    transport: Option<TransportOption>,
    operation: Option<&ExistingCampaignOperation>,
    units: &[UnitSnapshot],
    required_capture_units: u32,
) -> u32 {
    let capture_cost = [UnitType::Infantry, UnitType::Mech]
        .into_iter()
        .filter_map(|unit_type| unit_stats_for_type(registry, unit_type))
        .map(|stats| stats.cost)
        .min()
        .unwrap_or(0);
    let transport_purchase_cost = transport.map_or(0, |transport| {
        let cargo_slots = unit_stats_for_type(registry, transport.unit_type)
            .map(|stats| stats.max_cargo.max(1))
            .unwrap_or(1);
        let required_transports = required_capture_units.div_ceil(cargo_slots);
        let held_transports = u32::from(matches!(transport.source, TransportSource::Held(_)));
        required_transports
            .saturating_sub(held_transports)
            .saturating_mul(transport.cost)
    });
    let mut missing =
        transport_purchase_cost.saturating_add(capture_cost.saturating_mul(required_capture_units));
    let Some(operation) = operation else {
        return missing;
    };
    if !operation.is_forming
        && !operation
            .transport_phase
            .is_some_and(|phase| is_live_transport_phase(phase) || phase == TransportPhase::Return)
    {
        return missing;
    }
    let continued_transport_cost = transport.map_or(0, |transport| {
        if matches!(transport.source, TransportSource::Held(_)) {
            return 0;
        }
        u32::try_from(operation.transport_entities.len())
            .unwrap_or(u32::MAX)
            .saturating_mul(transport.cost)
    });
    missing = missing.saturating_sub(continued_transport_cost);
    for entity in operation
        .capture_entities
        .iter()
        .take(usize::try_from(required_capture_units).unwrap_or(usize::MAX))
    {
        if let Some(unit) = units.iter().find(|unit| unit.entity == *entity) {
            missing = missing.saturating_sub(unit.stats.cost.min(capture_cost));
        }
    }
    missing
}

/// 現在のECS盤面と生存Squadだけを読み、島別の評価入力を毎回決定的に再構築する。
pub fn collect_island_campaign_facts(
    world: &mut World,
    player_id: PlayerId,
) -> Vec<IslandCampaignFacts> {
    // ResourceとQueryの可変借用を重ねないよう、軽量な盤面スナップショットを先に複製する。
    // lifecycle更新は呼び出し側の責務とし、分析はSquad phaseやWorld resourceを変更しない。
    let map = world.resource::<Map>().clone();
    let island_map = world
        .get_resource::<IslandMap>()
        .cloned()
        .unwrap_or_else(|| IslandMap::analyze(&map));
    let registry = world
        .get_resource::<MasterDataRegistry>()
        .cloned()
        .unwrap_or_default();
    let manager = world
        .get_resource::<SquadManager>()
        .cloned()
        .unwrap_or_default();
    let properties = collect_property_snapshots(world);
    let factions = collect_faction_snapshots(world);
    let units = campaign_unit_snapshots(&manager, collect_unit_snapshots(world), &factions);
    let assigned_transport_phases = collect_assigned_transport_phases(&manager, &units, player_id);
    let (operations, cargo_assignments) =
        collect_existing_operations(&island_map, &manager, &units, player_id);
    let operations_by_island: HashMap<_, _> = operations
        .iter()
        .map(|operation| (operation.island_id, operation))
        .collect();

    let mut islands = island_map.islands.clone();
    islands.sort_by_key(|island| island.id.0);
    let mut accumulators: Vec<_> = islands
        .iter()
        .map(|island| IslandAccumulator {
            facts: IslandCampaignFacts {
                island_id: island.id,
                capturable_properties: 0,
                strategic_production_sites: 0,
                roi_production_sites: 0,
                neutral_properties: 0,
                friendly_properties: 0,
                enemy_properties: 0,
                friendly_units: 0,
                enemy_units: 0,
                friendly_combat_units: 0,
                enemy_combat_units: 0,
                friendly_arrival_eta: None,
                enemy_arrival_eta: None,
                friendly_capture_eta: None,
                enemy_capture_eta: None,
                transport_eta: None,
                capture_turns: 0,
                island_income_per_turn: 0,
                missing_expansion_package_cost: 0,
                reachable: false,
                has_unowned_properties: false,
            },
            properties: Vec::new(),
        })
        .collect();
    let accumulator_index: HashMap<_, _> = accumulators
        .iter()
        .enumerate()
        .map(|(index, accumulator)| (accumulator.facts.island_id, index))
        .collect();
    let master_unit_types = all_master_unit_types(&registry);

    for snapshot in properties {
        let Some(island) = island_map.get_island_at(&snapshot.position) else {
            continue;
        };
        let Some(&index) = accumulator_index.get(&island.id) else {
            continue;
        };
        let accumulator = &mut accumulators[index];
        let property = snapshot.property;
        if property.max_capture_points > 0 {
            accumulator.facts.capturable_properties =
                accumulator.facts.capturable_properties.saturating_add(1);
            match property.owner_id {
                None => {
                    accumulator.facts.neutral_properties =
                        accumulator.facts.neutral_properties.saturating_add(1)
                }
                Some(owner) if owner == player_id => {
                    accumulator.facts.friendly_properties =
                        accumulator.facts.friendly_properties.saturating_add(1)
                }
                Some(_) => {
                    accumulator.facts.enemy_properties =
                        accumulator.facts.enemy_properties.saturating_add(1)
                }
            }
            if property.owner_id != Some(player_id) {
                accumulator.facts.has_unowned_properties = true;
                let turns = default_capture_turns(&property);
                accumulator.facts.capture_turns = if accumulator.facts.capture_turns == 0 {
                    turns
                } else {
                    accumulator.facts.capture_turns.min(turns)
                };
            }
        }
        accumulator.facts.island_income_per_turn = accumulator
            .facts
            .island_income_per_turn
            .saturating_add(registry.landscape_income(property.terrain.as_str()));
        let can_produce_any = master_unit_types
            .iter()
            .any(|unit_type| registry.can_produce_unit(property.terrain.as_str(), *unit_type));
        if is_strategic_site(property.terrain) && can_produce_any {
            accumulator.facts.strategic_production_sites = accumulator
                .facts
                .strategic_production_sites
                .saturating_add(1);
        }
        if is_roi_site(property.terrain) && can_produce_any {
            accumulator.facts.roi_production_sites =
                accumulator.facts.roi_production_sites.saturating_add(1);
        }
        accumulator.properties.push(snapshot);
    }

    let mut unit_positions = HashMap::new();
    for unit in &units {
        if unit.transporting.is_some()
            || unit.position.x >= map.width
            || unit.position.y >= map.height
        {
            continue;
        }
        unit_positions.insert(
            (unit.position.x, unit.position.y),
            OccupantInfo {
                player_id: unit.faction,
                is_transport: unit.stats.max_cargo > 0,
                unit_type: unit.stats.unit_type,
                loadable_types: unit.stats.loadable_unit_types.clone(),
                free_slots: unit.free_cargo_slots,
            },
        );
    }

    // 共有cacheは占有情報をkeyに含めないため触らず、分析呼び出し専用の空cacheを使う。
    let mut cache = TurnDistanceCache::default();
    let mut connectivity = TerrainConnectivity::default();

    for unit in &units {
        if cargo_assignments.contains_key(&unit.entity) || unit.transporting.is_some() {
            continue;
        }
        let local_index = island_map
            .get_island_at(&unit.position)
            .and_then(|island| accumulator_index.get(&island.id).copied());
        if let Some(index) = local_index {
            let accumulator = &mut accumulators[index];
            if unit.faction == player_id {
                accumulator.facts.friendly_units =
                    accumulator.facts.friendly_units.saturating_add(1);
                if unit.health.current > 0 && !is_support_only(unit.stats.unit_type) {
                    accumulator.facts.friendly_combat_units =
                        accumulator.facts.friendly_combat_units.saturating_add(1);
                }
                accumulator.facts.friendly_arrival_eta = Some(0);
            } else {
                accumulator.facts.enemy_units = accumulator.facts.enemy_units.saturating_add(1);
                if unit.health.current > 0 && !is_support_only(unit.stats.unit_type) {
                    accumulator.facts.enemy_combat_units =
                        accumulator.facts.enemy_combat_units.saturating_add(1);
                }
                accumulator.facts.enemy_arrival_eta = Some(0);
            }
        }

        for island in &islands {
            let Some(&index) = accumulator_index.get(&island.id) else {
                continue;
            };
            let eta = unit_eta_to_island(
                &map,
                &registry,
                &unit_positions,
                &mut connectivity,
                &mut cache,
                unit,
                island,
            );
            let accumulator = &mut accumulators[index];
            if unit.faction == player_id {
                accumulator.facts.friendly_arrival_eta =
                    min_option(accumulator.facts.friendly_arrival_eta, eta);
            } else {
                accumulator.facts.enemy_arrival_eta =
                    min_option(accumulator.facts.enemy_arrival_eta, eta);
            }
            let capture_eta = capture_eta_from_position(
                &map,
                &registry,
                &unit_positions,
                &mut connectivity,
                &mut cache,
                unit,
                unit.position,
                &accumulator.properties,
            );
            if unit.faction == player_id {
                accumulator.facts.friendly_capture_eta =
                    min_option(accumulator.facts.friendly_capture_eta, capture_eta);
            } else {
                accumulator.facts.enemy_capture_eta =
                    min_option(accumulator.facts.enemy_capture_eta, capture_eta);
            }
        }
    }

    for unit in &units {
        let Some(assignment) = cargo_assignments.get(&unit.entity).cloned() else {
            continue;
        };
        let Some(island) = islands
            .iter()
            .find(|island| island.id == assignment.island_id)
        else {
            continue;
        };
        let Some(&index) = accumulator_index.get(&assignment.island_id) else {
            continue;
        };
        let arrival_eta = operation_transport_eta(
            assignment.clone(),
            &units,
            &map,
            &registry,
            &unit_positions,
            &mut connectivity,
            &mut cache,
            island,
        );
        let accumulator = &mut accumulators[index];
        if unit.health.current > 0 && !is_support_only(unit.stats.unit_type) {
            accumulator.facts.friendly_combat_units =
                accumulator.facts.friendly_combat_units.saturating_add(1);
        }
        accumulator.facts.friendly_arrival_eta =
            min_option(accumulator.facts.friendly_arrival_eta, arrival_eta);
        if let Some(arrival_eta) = arrival_eta {
            // Drop済み位置が判明している場合だけ実地点を使い、未確定なら島内候補の保守値を使う。
            let post_landing_eta = if let Some(landing_position) = assignment.drop_position {
                capture_eta_from_position(
                    &map,
                    &registry,
                    &unit_positions,
                    &mut connectivity,
                    &mut cache,
                    unit,
                    landing_position,
                    &accumulator.properties,
                )
            } else {
                conservative_capture_eta_from_island(
                    &map,
                    &registry,
                    &unit_positions,
                    &mut connectivity,
                    &mut cache,
                    unit,
                    island,
                    &accumulator.properties,
                )
            };
            accumulator.facts.friendly_capture_eta = min_option(
                accumulator.facts.friendly_capture_eta,
                post_landing_eta.map(|eta| arrival_eta.saturating_add(eta)),
            );
        }
    }

    let owned_properties: Vec<_> = accumulators
        .iter()
        .flat_map(|accumulator| accumulator.properties.iter())
        .filter(|snapshot| snapshot.property.owner_id == Some(player_id))
        .cloned()
        .collect();
    for (index, island) in islands.iter().enumerate() {
        let facts = &mut accumulators[index].facts;
        let has_foothold = facts.friendly_properties > 0 || facts.friendly_units > 0;
        if has_foothold {
            facts.reachable = true;
            facts.transport_eta = Some(0);
        } else {
            let options = transport_options_for_island(
                &map,
                &registry,
                &unit_positions,
                &mut connectivity,
                &mut cache,
                player_id,
                island,
                &units,
                &assigned_transport_phases,
                &owned_properties,
            );
            let is_open_neutral = facts.neutral_properties > 0
                && facts.friendly_properties == 0
                && facts.enemy_properties == 0
                && facts.friendly_units == 0
                && facts.enemy_units == 0;
            let selected = select_transport_option(&options, is_open_neutral);
            facts.reachable = selected.is_some();
            facts.transport_eta = selected.map(|option| option.eta);
            facts.missing_expansion_package_cost = missing_expansion_package_cost(
                &registry,
                selected,
                operations_by_island.get(&island.id).copied(),
                &units,
                facts
                    .neutral_properties
                    .saturating_add(facts.enemy_properties)
                    .max(2),
            );
        }
    }

    accumulators
        .into_iter()
        .map(|accumulator| accumulator.facts)
        .collect()
}

fn contested_is_competitive(facts: &IslandCampaignFacts) -> bool {
    facts
        .friendly_capture_eta
        .zip(facts.enemy_capture_eta)
        .is_some_and(|(friendly_eta, enemy_eta)| friendly_eta <= enemy_eta.saturating_add(1))
}

fn requirement_for_assessment(
    facts: &IslandCampaignFacts,
    assessment: &mut crate::ai::island_campaign::IslandCampaignAssessment,
) -> IslandCampaignRequirement {
    // 未所有施設は互いに独立した占領対象なので、残数に応じて並行投入する。
    let remaining_properties = facts
        .neutral_properties
        .saturating_add(facts.enemy_properties);
    // 上陸・交戦中は1体撃破だけで占領能力を失わない最小2体を維持する。
    let expedition_capture_units = remaining_properties.max(2);
    match assessment.state {
        IslandCampaignState::OpenNeutral
            if assessment.decision == IslandCampaignDecision::Expand =>
        {
            let capture_budget = expedition_capture_units.saturating_mul(1_000);
            let transport_units = expedition_capture_units.div_ceil(2);
            let transport_budget = transport_units.saturating_mul(4_000);
            IslandCampaignRequirement {
                preferred_transport: Some(UnitType::TransportHelicopter),
                transport_slots: expedition_capture_units,
                capture_units: expedition_capture_units,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: capture_budget.saturating_add(transport_budget),
            }
        }
        IslandCampaignState::Secured if assessment.decision == IslandCampaignDecision::Secure => {
            let capture_budget = remaining_properties.saturating_mul(1_000);
            assessment.required_budget = capture_budget;
            IslandCampaignRequirement {
                // 現地要員は輸送不要。allocatorが遠隔要員を選んだ場合だけ実cargoから輸送を要求する。
                preferred_transport: None,
                transport_slots: 0,
                capture_units: remaining_properties,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: capture_budget,
            }
        }
        IslandCampaignState::Threatened => {
            let combat_units = facts.enemy_combat_units;
            let capture_budget = remaining_properties.saturating_mul(1_000);
            assessment.required_budget = capture_budget;
            IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                // 接近脅威は未所有施設を放棄する理由ではない。既着の占領兵を
                // assignmentへ残し、戦闘Entityとは独立して施設確保を続行する。
                capture_units: remaining_properties,
                ground_combat_units: 0,
                combat_units,
                total_budget: capture_budget,
            }
        }
        IslandCampaignState::Contested if contested_is_competitive(facts) => {
            let capture_budget = expedition_capture_units.saturating_mul(1_000);
            assessment.decision = IslandCampaignDecision::Contest;
            assessment.decision_reason =
                "占領競争の時間内に介入できるため作戦を継続する".to_owned();
            IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: expedition_capture_units,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: capture_budget,
            }
        }
        IslandCampaignState::Contested => {
            let capture_budget = expedition_capture_units.saturating_mul(1_000);
            let combat_units = facts.enemy_combat_units;
            assessment.decision = IslandCampaignDecision::Reinforce;
            assessment.decision_reason =
                "観測敵Entityを対象にターン別撃破計画を要求する".to_owned();
            assessment.required_budget = capture_budget;
            IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: expedition_capture_units,
                ground_combat_units: 0,
                combat_units,
                total_budget: capture_budget,
            }
        }
        IslandCampaignState::EnemyHeld => {
            let combat_units = facts.enemy_combat_units;
            // 敵兵数・生産拠点・配置可能枠は島の実盤面を走査した後で確定する。
            let capture_budget = expedition_capture_units.saturating_mul(1_000);
            let total_budget = capture_budget;
            assessment.required_budget = total_budget;
            IslandCampaignRequirement {
                preferred_transport: Some(UnitType::Lander),
                transport_slots: expedition_capture_units,
                capture_units: expedition_capture_units,
                ground_combat_units: 0,
                combat_units,
                total_budget,
            }
        }
        _ => IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 0,
        },
    }
}

/// 敵首都島でも、盤面上の未所有施設をすべて継続目標として保持する。
///
/// 同時に動かす占領兵を固定3体などへ切り詰めると、広い陸塊や複数の上陸地点で
/// 取得機会を失う。必要数は現在の未所有施設数から導き、実際に同時進行できる数は
/// 生産・輸送・移動scheduleと既存Entityの割当に委ねる。敵撃破戦力は別途V4の
/// 時系列Combat Planが決めるため、ここでは価格や敵数を占領兵数へ換算しない。
fn prepare_capital_assault_wave(candidate: &mut IslandCampaignCandidate) {
    if candidate.assessment.decision != IslandCampaignDecision::Assault
        || candidate
            .existing_operation
            .as_ref()
            .is_some_and(|operation| {
                matches!(
                    operation.transport_phase,
                    Some(TransportPhase::Pickup | TransportPhase::Transit | TransportPhase::Drop)
                )
            })
    {
        return;
    }
    let remaining_properties = candidate
        .assessment
        .neutral_properties
        .saturating_add(candidate.assessment.enemy_properties);
    if remaining_properties == 0 {
        return;
    }
    let capture_units = remaining_properties;
    candidate.requirement.capture_units = capture_units;
    candidate.requirement.transport_slots = capture_units;
    candidate.requirement.ground_combat_units = 0;
    candidate.requirement.combat_units = 0;
    candidate.requirement.total_budget = capture_units.saturating_mul(1_000);
    candidate.assessment.required_budget = candidate.requirement.total_budget;
    candidate.assault_transport_types = adapt_assault_requirement_to_production_route(
        &mut candidate.assessment,
        &mut candidate.requirement,
        &candidate.producible_transports,
    );
}

/// 首都作戦だけを集結状態へ戻す。局地Captureや防衛assignmentは変更しないため、
/// 兵站の一時崩壊で全軍の命令が停止することはない。
fn gate_capital_assault_launch(
    portfolio: &mut IslandCampaignPortfolio,
    capital_island: IslandId,
    logistics_ready: bool,
    combat_plan_ready: bool,
) {
    if logistics_ready && combat_plan_ready {
        return;
    }
    if let Some(assignment) = portfolio
        .active_offensives
        .iter_mut()
        .find(|assignment| assignment.island_id == capital_island)
    {
        assignment.operation_ready = false;
    }
    if let Some(assessment) = portfolio
        .islands
        .iter_mut()
        .find(|assessment| assessment.island_id == capital_island)
    {
        assessment.decision_reason = if !logistics_ready {
            "選択兵站路が未完了または崩壊したため、首都第一波だけを集結状態に戻す".to_owned()
        } else {
            "現在盤面から再計算した首都戦闘計画が未完成のため、第一波を集結させる".to_owned()
        };
    }
}

/// 同じ出発島で生産できる輸送手段から、占領要員を運べる暫定編成を返す。
///
/// これは中立島向けの初便を止めないための互換経路であり、敵領Assaultの必要総数や
/// 一波上限ではない。敵領の必要戦力は時系列作戦計画から導出する。
fn producible_assault_transport_package(
    blueprints: &[CampaignTransportBlueprint],
    capture_units: u32,
) -> Option<(Vec<UnitType>, u32, u32)> {
    let mut source_islands: Vec<_> = blueprints
        .iter()
        .map(|blueprint| blueprint.source_island)
        .collect();
    source_islands.sort_unstable_by_key(|island| island.0);
    source_islands.dedup();

    let mut packages = Vec::new();
    for source_island in source_islands {
        let mut landers = blueprints.iter().filter(|blueprint| {
            blueprint.source_island == source_island && blueprint.unit_type == UnitType::Lander
        });
        let mut helicopters = blueprints.iter().filter(|blueprint| {
            blueprint.source_island == source_island
                && blueprint.unit_type == UnitType::TransportHelicopter
        });
        let mut selected = Vec::new();
        let mut remaining_capture_units = capture_units;
        while remaining_capture_units > 0 {
            let capture_transport = helicopters.next().or_else(|| landers.next());
            let Some(capture_transport) = capture_transport else {
                break;
            };
            selected.push(capture_transport);
            remaining_capture_units =
                remaining_capture_units.saturating_sub(capture_transport.cargo_slots);
        }
        if selected.is_empty() || remaining_capture_units > 0 {
            continue;
        }
        let unit_types = selected
            .iter()
            .map(|blueprint| blueprint.unit_type)
            .collect();
        let cost = selected.iter().fold(0_u32, |total, blueprint| {
            total.saturating_add(blueprint.cost)
        });
        let slots = selected.iter().fold(0_u32, |total, blueprint| {
            total.saturating_add(blueprint.cargo_slots)
        });
        packages.push((cost, source_island.0, unit_types, slots));
    }
    packages.sort_by_key(|(cost, island, _, _)| (*cost, *island));
    packages
        .into_iter()
        .next()
        .map(|(cost, _, unit_types, slots)| (unit_types, cost, slots))
}

fn adapt_assault_requirement_to_production_route(
    assessment: &mut crate::ai::island_campaign::IslandCampaignAssessment,
    requirement: &mut IslandCampaignRequirement,
    blueprints: &[CampaignTransportBlueprint],
) -> Vec<UnitType> {
    if assessment.decision != IslandCampaignDecision::Assault {
        return Vec::new();
    }
    let Some((unit_types, transport_cost, transport_slots)) =
        producible_assault_transport_package(blueprints, requirement.capture_units)
    else {
        // 生産拠点を失った一時局面では、既存輸送を再利用できる従来編成を維持する。
        return vec![UnitType::Lander, UnitType::TransportHelicopter];
    };
    requirement.preferred_transport = unit_types.first().copied();
    requirement.transport_slots = transport_slots;
    requirement.total_budget =
        transport_cost.saturating_add(requirement.capture_units.saturating_mul(1_000));
    assessment.required_budget = requirement.total_budget;
    unit_types
}

/// 自軍の実生産拠点ごとに、増援作戦が要求できる輸送手段の諸元を構築する。
fn producible_campaign_transports(
    map: &Map,
    registry: &MasterDataRegistry,
    island_map: &IslandMap,
    properties: &[PropertySnapshot],
    player_id: PlayerId,
) -> Vec<CampaignTransportBlueprint> {
    let capital_positions: Vec<_> = properties
        .iter()
        .filter(|property| {
            property.property.owner_id == Some(player_id)
                && property.property.terrain == Terrain::Capital
        })
        .map(|property| property.position)
        .collect();
    let mut blueprints = Vec::new();
    for property in properties
        .iter()
        .filter(|property| property.property.owner_id == Some(player_id))
    {
        if !crate::systems::production::is_within_production_range(
            &capital_positions,
            property.position.x,
            property.position.y,
            map.topology,
        ) {
            continue;
        }
        let Some(source_island) = island_map
            .get_island_at(&property.position)
            .map(|island| island.id)
        else {
            continue;
        };
        for unit_type in [UnitType::TransportHelicopter, UnitType::Lander] {
            if !registry.can_produce_unit(property.property.terrain.as_str(), unit_type) {
                continue;
            }
            let Some(stats) = unit_stats_for_type(registry, unit_type) else {
                continue;
            };
            let blueprint = CampaignTransportBlueprint {
                unit_type,
                cost: stats.cost,
                cargo_slots: stats.max_cargo,
                loadable_unit_types: stats.loadable_unit_types,
                source_island,
            };
            // 同型・同島でも施設1つにつき1件を保持する。一波は「1施設が1手番に
            // 生産できる1機/隻」で上限を決めるため、ここでdedupしてはならない。
            blueprints.push(blueprint);
        }
    }
    blueprints.sort_by_key(|blueprint| {
        (
            blueprint.source_island.0,
            campaign_unit_type_rank(blueprint.unit_type),
            blueprint.cost,
        )
    });
    blueprints
}

fn candidate_target_position(
    map: &Map,
    registry: &MasterDataRegistry,
    island: &Island,
    properties: &[PropertySnapshot],
    units: &[UnitSnapshot],
    player_id: PlayerId,
    existing_operation: Option<&ExistingCampaignOperation>,
) -> Option<GridPosition> {
    let origins = properties
        .iter()
        .filter(|snapshot| island.tiles.contains(&snapshot.position))
        .filter(|snapshot| snapshot.property.owner_id == Some(player_id))
        .map(|snapshot| snapshot.position)
        .collect::<Vec<_>>();
    let mut targets = properties
        .iter()
        .filter(|snapshot| island.tiles.contains(&snapshot.position))
        .filter(|snapshot| {
            snapshot.property.max_capture_points > 0
                && snapshot.property.owner_id != Some(player_id)
        })
        .map(|snapshot| snapshot.position)
        .collect::<Vec<_>>();
    let target_priority = |position: GridPosition| {
        capture_front_priority(
            map, registry, island, properties, units, player_id, &origins, position,
        )
    };
    targets.sort_unstable_by_key(|position| target_priority(*position));
    let nearest = targets.first().copied();

    // 進行中目標も無条件には固定しない。より近い前線より2手以上遠くなった目標は
    // 敵の再占領や担当損耗で前提が崩れたものとして、現在の最短前線へ戻す。
    if let (Some(operation), Some(nearest)) = (existing_operation, nearest)
        && targets.contains(&operation.target_position)
    {
        let distance_from_front = |position: GridPosition| target_priority(position).1;
        if origins.is_empty()
            || distance_from_front(operation.target_position)
                <= distance_from_front(nearest).saturating_add(2)
        {
            return Some(operation.target_position);
        }
    }
    nearest.or_else(|| sorted_island_tiles(island).first().copied())
}

fn candidate_capture_target_positions(
    map: &Map,
    registry: &MasterDataRegistry,
    island: &Island,
    properties: &[PropertySnapshot],
    units: &[UnitSnapshot],
    player_id: PlayerId,
    primary_target: GridPosition,
) -> Vec<GridPosition> {
    let origins = properties
        .iter()
        .filter(|snapshot| island.tiles.contains(&snapshot.position))
        .filter(|snapshot| snapshot.property.owner_id == Some(player_id))
        .map(|snapshot| snapshot.position)
        .collect::<Vec<_>>();
    let mut targets: Vec<_> = properties
        .iter()
        .filter(|snapshot| island.tiles.contains(&snapshot.position))
        .filter(|snapshot| {
            snapshot.property.max_capture_points > 0
                && snapshot.property.owner_id != Some(player_id)
        })
        .map(|snapshot| snapshot.position)
        .collect();
    let target_priority = |position: GridPosition| {
        capture_front_priority(
            map, registry, island, properties, units, player_id, &origins, position,
        )
    };
    targets.sort_by_key(|position| target_priority(*position));

    // 既に足場がある陸塊では、最短前線とその隣接帯だけを同時実行対象にする。
    // 全未所有施設は毎手番再観測するため捨てず、前線取得後に次の帯へ昇格する。
    // 足場のない島は輸送波の目的一覧を縮めると再上陸が遅れるため従来どおり保持する。
    if !origins.is_empty()
        && let Some(nearest_distance) = targets.first().map(|target| target_priority(*target).1)
    {
        targets.retain(|target| target_priority(*target).1 <= nearest_distance.saturating_add(2));
    }
    if let Some(index) = targets
        .iter()
        .position(|position| *position == primary_target)
    {
        targets.remove(index);
        targets.insert(0, primary_target);
    }
    targets
}

/// 自軍前線からの占領ETAを第一基準、敵占領兵の到着ETAを同距離帯の競争優先度にする。
/// 座標は決定論的な最終tie-breakにだけ使うため、鏡像mapでも前線側を選択できる。
#[allow(clippy::too_many_arguments)]
fn capture_front_priority(
    map: &Map,
    registry: &MasterDataRegistry,
    island: &Island,
    properties: &[PropertySnapshot],
    units: &[UnitSnapshot],
    player_id: PlayerId,
    friendly_origins: &[GridPosition],
    target: GridPosition,
) -> (u32, u32, u32, usize, usize) {
    let friendly_movement = [UnitType::Infantry, UnitType::Mech]
        .into_iter()
        .filter_map(|unit_type| unit_stats_for_type(registry, unit_type))
        .min_by_key(|stats| stats.cost)
        .map_or(1, |stats| stats.max_movement.max(1));
    let occupation_turns = properties
        .iter()
        .find(|snapshot| snapshot.position == target)
        .map_or(1, |snapshot| default_capture_turns(&snapshot.property));
    let front_distance = friendly_origins
        .iter()
        .map(|origin| map.distance(origin.x, origin.y, target.x, target.y))
        .min()
        .unwrap_or(u32::MAX);
    let friendly_eta = front_distance
        .div_ceil(friendly_movement)
        .saturating_add(occupation_turns);
    let enemy_eta = units
        .iter()
        .filter(|unit| unit.faction != player_id && unit.stats.can_capture)
        .filter(|unit| unit.transporting.is_none() && island.tiles.contains(&unit.position))
        .map(|unit| {
            map.distance(unit.position.x, unit.position.y, target.x, target.y)
                .div_ceil(unit.stats.max_movement.max(1))
                .saturating_add(occupation_turns)
        })
        .min()
        .unwrap_or(u32::MAX);
    (friendly_eta, front_distance, enemy_eta, target.y, target.x)
}

fn operation_has_live_capability(operation: &ExistingCampaignOperation) -> bool {
    ((operation.is_forming
        || operation.transport_phase.is_some_and(|phase| {
            is_live_transport_phase(phase) || phase == TransportPhase::Return
        }))
        && !operation.transport_entities.is_empty())
        || !operation.capture_entities.is_empty()
        || !operation.combat_entities.is_empty()
}

fn unavailable_campaign_entities(
    manager: &SquadManager,
    units: &[UnitSnapshot],
    player_id: PlayerId,
) -> HashSet<Entity> {
    let unit_by_entity: HashMap<_, _> = units.iter().map(|unit| (unit.entity, unit)).collect();
    // SoloFallbackは同じplan_squads呼び出しでcampaign資源へ再予約しない。
    let mut unavailable = manager.solo_fallbacks.clone();
    for squad in &manager.squads {
        let is_return_or_completed = matches!(
            squad.phase,
            MissionPhase::Transport(TransportPhase::Return) | MissionPhase::Completed
        );
        let is_targetless_transport = squad.mission_type == MissionType::Transport
            && squad.target_island.is_none()
            && squad.target.is_none();
        if is_return_or_completed {
            if let Some(transport) = squad.transport_entity.filter(|transport| {
                unit_by_entity
                    .get(transport)
                    .is_some_and(|unit| unit.faction == player_id)
            }) {
                // 空のReturn輸送役は、帰投完了を待たず同じ／次の補充便へ再予約できる。
                // cargoが残るReturn（実際にはDropへ戻すべき状態）とCompletedは保護する。
                let reusable_empty_return =
                    matches!(squad.phase, MissionPhase::Transport(TransportPhase::Return))
                        && unit_by_entity
                            .get(&transport)
                            .is_some_and(|unit| unit.loaded_cargo_entities.is_empty());
                if !reusable_empty_return {
                    unavailable.insert(transport);
                }
            }
            unavailable.extend(
                squad
                    .cargo_entities
                    .iter()
                    .chain(squad.delivered_cargo.iter())
                    .filter(|entity| {
                        unit_by_entity
                            .get(entity)
                            .is_some_and(|unit| unit.faction == player_id)
                    })
                    .copied(),
            );
        } else if is_targetless_transport {
            unavailable.extend(squad.delivered_cargo.iter().filter_map(|entity| {
                unit_by_entity
                    .get(entity)
                    .is_some_and(|unit| unit.faction == player_id)
                    .then_some(*entity)
            }));
        }
    }
    unavailable
}

#[allow(clippy::too_many_arguments)]
fn collect_campaign_resource_pool(
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
    let unit_by_entity: HashMap<_, _> = units.iter().map(|unit| (unit.entity, unit)).collect();
    let assigned_by_entity =
        merge_campaign_assignment_sources(roadmap_assignments, operations, produced_assignments);
    let mut assigned_entities_by_island: HashMap<IslandId, HashSet<Entity>> = HashMap::new();
    for (entity, island_id) in &assigned_by_entity {
        assigned_entities_by_island
            .entry(*island_id)
            .or_default()
            .insert(*entity);
    }

    let mut candidates = Vec::new();
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
        if unavailable_entities.contains(&unit.entity)
            || is_offshore_transport && unit.fuel.is_some_and(|fuel| fuel == 0)
            || is_offshore_transport && (!transport_is_available || available_cargo_slots == 0)
            || (unit.transporting.is_some() && assigned_island.is_none())
        {
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
                        && is_terrain_reachable(
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
                is_terrain_reachable(
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

fn merge_campaign_assignment_sources(
    roadmap_assignments: &HashMap<Entity, IslandId>,
    operations: &[ExistingCampaignOperation],
    produced_assignments: &HashMap<Entity, IslandId>,
) -> HashMap<Entity, IslandId> {
    // 生産直後の発注意図は未配属Entityの初期seedに限る。現在Squad、前手番Roadmapの
    // 確定bindingの順で上書きし、再配置済みEntityを旧anchorへ巻き戻さない。
    let mut assigned_by_entity = produced_assignments.clone();
    for operation in operations
        .iter()
        .filter(|operation| operation_has_live_capability(operation))
    {
        for entity in operation
            .transport_entities
            .iter()
            .chain(operation.capture_entities.iter())
            .chain(operation.combat_entities.iter())
        {
            assigned_by_entity.insert(*entity, operation.island_id);
        }
    }
    assigned_by_entity.extend(
        roadmap_assignments
            .iter()
            .map(|(entity, island_id)| (*entity, *island_id)),
    );
    assigned_by_entity
}

/// 全島評価と現在の共有資源を毎回盤面から再構築し、純粋allocatorへ一括で渡す。
pub fn analyze_island_campaign(world: &mut World, player_id: PlayerId) -> IslandCampaignPortfolio {
    analyze_island_campaign_excluding(world, player_id, &HashSet::new())
}

/// 既存の実行任務へ予約済みのEntityを共有資源から除外して島嶼作戦を分析します。
pub fn analyze_island_campaign_excluding(
    world: &mut World,
    player_id: PlayerId,
    reserved_entities: &HashSet<Entity>,
) -> IslandCampaignPortfolio {
    let facts = collect_island_campaign_facts(world, player_id);
    let map = world.resource::<Map>().clone();
    let island_map = world
        .get_resource::<IslandMap>()
        .cloned()
        .unwrap_or_else(|| IslandMap::analyze(&map));
    let manager = world
        .get_resource::<SquadManager>()
        .cloned()
        .unwrap_or_default();
    let registry = world
        .get_resource::<MasterDataRegistry>()
        .cloned()
        .unwrap_or_default();
    let properties = collect_property_snapshots(world);
    let (friendly_income_per_turn, enemy_income_per_turn) = properties.iter().fold(
        (0_u32, 0_u32),
        |(friendly_income, enemy_income), snapshot| {
            let income = registry.landscape_income(snapshot.property.terrain.as_str());
            match snapshot.property.owner_id {
                Some(owner) if owner == player_id => {
                    (friendly_income.saturating_add(income), enemy_income)
                }
                Some(_) => (friendly_income, enemy_income.saturating_add(income)),
                None => (friendly_income, enemy_income),
            }
        },
    );
    let home_property = properties.iter().find(|snapshot| {
        snapshot.property.owner_id == Some(player_id)
            && snapshot.property.terrain == Terrain::Capital
    });
    let home_position = home_property.map(|snapshot| snapshot.position);
    let home_island = home_property
        .and_then(|snapshot| island_map.get_island_at(&snapshot.position))
        .map(|island| island.id);
    let enemy_capital = properties
        .iter()
        .find(|snapshot| {
            snapshot.property.terrain == Terrain::Capital
                && snapshot.property.owner_id != Some(player_id)
        })
        .and_then(|snapshot| {
            island_map
                .get_island_at(&snapshot.position)
                .map(|island| (island.id, snapshot.position))
        });
    let mut forward_sustainment_coverage = CampaignSustainmentCoverage::default();
    if let Some(home_island) = home_island {
        for snapshot in properties.iter().filter(|snapshot| {
            snapshot.property.owner_id == Some(player_id)
                && snapshot.property.max_capture_points > 0
                && island_map
                    .get_island_at(&snapshot.position)
                    .is_some_and(|island| island.id != home_island)
        }) {
            match snapshot.property.terrain {
                Terrain::Capital | Terrain::City | Terrain::Factory => {
                    forward_sustainment_coverage.ground = true
                }
                Terrain::Airport => forward_sustainment_coverage.air = true,
                Terrain::Port => forward_sustainment_coverage.sea = true,
                _ => {}
            }
        }
    }
    let producible_transports =
        producible_campaign_transports(&map, &registry, &island_map, &properties, player_id);
    let factions = collect_faction_snapshots(world);
    let units = campaign_unit_snapshots(&manager, collect_unit_snapshots(world), &factions);
    let assigned_transport_phases = collect_assigned_transport_phases(&manager, &units, player_id);
    let (operations, _) = collect_existing_operations(&island_map, &manager, &units, player_id);
    let operations_by_island: HashMap<_, _> = operations
        .iter()
        .filter(|operation| operation_has_live_capability(operation))
        .map(|operation| (operation.island_id, operation.clone()))
        .collect();
    let available_funds = world
        .get_resource::<Players>()
        .and_then(|players| players.0.iter().find(|player| player.id == player_id))
        .map_or(0, |player| player.funds);
    let produced_assignments = world
        .get_resource::<crate::ai::v4::campaign_execution::V4CampaignExecutionRegistry>()
        .map(|registry| registry.produced_entity_assignments(player_id))
        .unwrap_or_default();
    let roadmap_assignments = world
        .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
        .map(|registry| registry.campaign_entity_assignments(player_id))
        .unwrap_or_default();
    let mut unavailable_entities = unavailable_campaign_entities(&manager, &units, player_id);
    unavailable_entities.extend(reserved_entities.iter().copied());
    let pool = collect_campaign_resource_pool(
        player_id,
        &map,
        &registry,
        &island_map,
        &properties,
        &units,
        &operations,
        &produced_assignments,
        &roadmap_assignments,
        &assigned_transport_phases,
        &unavailable_entities,
        available_funds,
    );

    let facts_by_island: HashMap<_, _> = facts
        .iter()
        .map(|island_facts| (island_facts.island_id, island_facts))
        .collect();
    let mut islands = island_map.islands.clone();
    islands.sort_by_key(|island| island.id.0);
    // 島ごとの候補構築は同じ盤面snapshotを読むだけで互いに独立している。
    // nativeでは並列化し、IslandId順の結果を維持することでV3/V4の判断順を変えない。
    let mut candidates = crate::ai::deterministic_parallel::map_ordered(islands, |island| {
        let island_facts = facts_by_island.get(&island.id).copied()?;
        let mut assessment = assess_island(island_facts);
        // 陸塊が1つだけでも島嶼mapと同じ継続campaignとして扱う。違いは
        // `reachable` と輸送routeから輸送工程が必要になるかどうかだけである。
        let mut requirement = requirement_for_assessment(island_facts, &mut assessment);
        let existing_operation = operations_by_island.get(&island.id).cloned();
        let assault_wave_is_live = existing_operation.as_ref().is_some_and(|operation| {
            matches!(
                operation.transport_phase,
                Some(TransportPhase::Pickup | TransportPhase::Transit | TransportPhase::Drop)
            )
        });
        let assault_transport_types =
            if assessment.decision == IslandCampaignDecision::Assault && assault_wave_is_live {
                let operation = existing_operation.as_ref().expect("live operation exists");
                let mut transport_types = operation
                    .transport_entities
                    .iter()
                    .filter_map(|entity| {
                        units
                            .iter()
                            .find(|unit| unit.entity == *entity)
                            .map(|unit| unit.stats.unit_type)
                    })
                    .collect::<Vec<_>>();
                transport_types.sort_by_key(|unit_type| campaign_unit_type_rank(*unit_type));
                let entity_cost = |entity: &Entity| {
                    units
                        .iter()
                        .find(|unit| unit.entity == *entity)
                        .map_or(0, |unit| unit.stats.cost)
                };
                requirement.preferred_transport = transport_types.first().copied();
                requirement.transport_slots = operation
                    .transport_entities
                    .iter()
                    .filter_map(|entity| units.iter().find(|unit| unit.entity == *entity))
                    .fold(0_u32, |total, unit| {
                        total.saturating_add(unit.free_cargo_slots)
                    });
                requirement.capture_units =
                    u32::try_from(operation.capture_entities.len()).unwrap_or(u32::MAX);
                // Combat必要数は時系列planの出力であり、既存便の台数から再構築しない。
                requirement.ground_combat_units = 0;
                requirement.combat_units =
                    u32::try_from(operation.combat_entities.len()).unwrap_or(u32::MAX);
                requirement.total_budget = operation
                    .transport_entities
                    .iter()
                    .chain(&operation.capture_entities)
                    .fold(0_u32, |total, entity| {
                        total.saturating_add(entity_cost(entity))
                    });
                assessment.required_budget = requirement.total_budget;
                transport_types
            } else {
                adapt_assault_requirement_to_production_route(
                    &mut assessment,
                    &mut requirement,
                    &producible_transports,
                )
            };
        let mut priority_enemy_types = units
            .iter()
            .filter(|unit| unit.faction != player_id && unit.transporting.is_none())
            // 地上揚陸波の編成対象だけを渡す。航空・艦船はV4 rolling planが
            // 別経路で処理し、地上unitに不可能な対空・対艦要件を課さない。
            .filter(|unit| {
                !matches!(
                    unit.stats.movement_type,
                    MovementType::Air | MovementType::Ship
                )
            })
            .filter(|unit| island.tiles.contains(&unit.position))
            .map(|unit| unit.stats.unit_type)
            .collect::<Vec<_>>();
        priority_enemy_types.sort_by_key(|unit_type| campaign_unit_type_rank(*unit_type));
        priority_enemy_types.dedup();
        let target_position = candidate_target_position(
            &map,
            &registry,
            &island,
            &properties,
            &units,
            player_id,
            existing_operation.as_ref(),
        )?;
        let capture_target_positions = candidate_capture_target_positions(
            &map,
            &registry,
            &island,
            &properties,
            &units,
            player_id,
            target_position,
        );
        let has_foothold = properties.iter().any(|property| {
            island.tiles.contains(&property.position)
                && property.property.owner_id == Some(player_id)
        });
        if has_foothold {
            // 同一陸塊の全未所有施設ではなく、今手番に選んだ前線帯だけを同時要求する。
            // 後方の施設は前線取得後に毎手番再評価するため、将来分の占領兵費用を
            // 現在作戦へ先取りしない。
            let active_capture_units =
                u32::try_from(capture_target_positions.len()).unwrap_or(u32::MAX);
            let released_capture_units = requirement
                .capture_units
                .saturating_sub(active_capture_units);
            requirement.capture_units = active_capture_units;
            requirement.transport_slots = requirement.transport_slots.min(active_capture_units);
            requirement.total_budget = requirement
                .total_budget
                .saturating_sub(released_capture_units.saturating_mul(1_000));
            assessment.required_budget = requirement.total_budget;
        }
        let (ground_sustainment_sites, air_sustainment_sites, sea_sustainment_sites) =
            sustainment_site_counts(&island, &properties);
        let sustainment_targets = if Some(island.id) == home_island {
            CampaignSustainmentTargets::default()
        } else {
            sustainment_targets(&island, &properties, player_id)
        };
        Some(IslandCampaignCandidate {
            assessment,
            target_position,
            capture_target_positions,
            priority_enemy_types,
            roi_production_sites: island_facts.roi_production_sites,
            transport_eta: island_facts.transport_eta,
            ground_sustainment_sites,
            air_sustainment_sites,
            sea_sustainment_sites,
            sustainment_targets,
            island_income_per_turn: island_facts.island_income_per_turn,
            logistics_prerequisite: false,
            logistics_priority_rank: None,
            requirement,
            assault_transport_types,
            producible_transports: producible_transports.clone(),
            existing_operation,
        })
    })
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let uses_operation_driven_production =
        crate::ai::resolve_player_ai_version(world, player_id).uses_operation_driven_production();
    let mut capital_launch_gate = None;
    if uses_operation_driven_production {
        let turn = world
            .get_resource::<MatchState>()
            .map_or(0, |state| state.current_turn_number.0);
        let mut logistics_registry = world
            .remove_resource::<crate::ai::v4::logistics_plan::V4LogisticsPlanRegistry>()
            .unwrap_or_default();
        crate::ai::v4::logistics_plan::apply_persistent_logistics_plan(
            &mut logistics_registry,
            player_id,
            turn,
            &mut candidates,
            &map,
            friendly_income_per_turn,
            enemy_income_per_turn,
            available_funds,
            forward_sustainment_coverage,
            home_position,
            enemy_capital,
        );
        let logistics_ready = logistics_registry
            .plan(player_id)
            .is_some_and(|plan| plan.selected_islands.is_empty());
        if let Some((capital_island, _)) = enemy_capital {
            if let Some(candidate) = candidates
                .iter_mut()
                .find(|candidate| candidate.assessment.island_id == capital_island)
            {
                prepare_capital_assault_wave(candidate);
            }
            let combat_plan_ready = world
                .get_resource::<crate::ai::v4::plan_revision::V4RollingPlanRegistry>()
                .is_some_and(|registry| registry.operation_launch_ready(player_id, capital_island));
            capital_launch_gate = Some((capital_island, logistics_ready, combat_plan_ready));
        }
        world.insert_resource(logistics_registry);
    } else {
        promote_logistics_prerequisite(
            &mut candidates,
            &map,
            friendly_income_per_turn,
            enemy_income_per_turn,
            forward_sustainment_coverage,
            home_position,
        );
    }
    let mut portfolio = allocate_campaign_portfolio(candidates, pool);
    if let Some((capital_island, logistics_ready, combat_plan_ready)) = capital_launch_gate {
        gate_capital_assault_launch(
            &mut portfolio,
            capital_island,
            logistics_ready,
            combat_plan_ready,
        );
    }
    portfolio
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::island_campaign::{IslandCampaignDecision, IslandCampaignState};
    use crate::ai::islands::IslandMap;
    use crate::components::{
        ActionCompleted, Ammo, CargoCapacity, Faction, Fuel, GridPosition, HasMoved, Health,
        PlayerId, Property, Transporting, UnitStats,
    };
    use crate::resources::master_data::{MasterDataRegistry, UnitName};
    use crate::resources::{GameRng, GridTopology, Map, MatchState, Players, Terrain, UnitType};
    use bevy_ecs::prelude::{Entity, World};

    const TEST_SEED: u64 = 42;

    #[test]
    fn mirrored_land_fronts_choose_the_nearest_property_instead_of_coordinate_order() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let map = Map::new(11, 1, Terrain::Plains, GridTopology::Square);
        let island = IslandMap::analyze(&map).islands.remove(0);
        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let property = |x, owner| PropertySnapshot {
            position: GridPosition { x, y: 0 },
            property: Property::new(Terrain::City, owner, 200),
        };

        let left_properties = vec![
            property(0, Some(player)),
            property(2, Some(enemy)),
            property(8, Some(enemy)),
        ];
        let left_target = candidate_target_position(
            &map,
            &master_data,
            &island,
            &left_properties,
            &[],
            player,
            None,
        );
        assert_eq!(left_target, Some(GridPosition { x: 2, y: 0 }));

        let right_properties = vec![
            property(10, Some(player)),
            property(2, Some(enemy)),
            property(8, Some(enemy)),
        ];
        let right_target = candidate_target_position(
            &map,
            &master_data,
            &island,
            &right_properties,
            &[],
            player,
            None,
        );
        assert_eq!(right_target, Some(GridPosition { x: 8, y: 0 }));
        assert_eq!(
            candidate_capture_target_positions(
                &map,
                &master_data,
                &island,
                &right_properties,
                &[],
                player,
                right_target.expect("右側前線"),
            ),
            vec![GridPosition { x: 8, y: 0 }]
        );
    }

    #[test]
    fn roadmap_then_live_squad_override_produced_anchor() {
        let entity = Entity::from_raw(42);
        let roadmap_island = IslandId(1);
        let squad_island = IslandId(2);
        let intended_island = IslandId(3);
        let roadmap = HashMap::from([(entity, roadmap_island)]);
        let operations = vec![ExistingCampaignOperation {
            island_id: squad_island,
            target_position: GridPosition { x: 2, y: 2 },
            transport_phase: None,
            is_forming: true,
            transport_entities: vec![entity],
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
        }];
        let produced = HashMap::from([(entity, intended_island)]);

        let assignments = merge_campaign_assignment_sources(&roadmap, &operations, &produced);

        assert_eq!(
            assignments.get(&entity),
            Some(&roadmap_island),
            "生産時anchorは現在Roadmap bindingを上書きしない"
        );

        let assignments = merge_campaign_assignment_sources(
            &HashMap::from([(entity, roadmap_island)]),
            &operations,
            &HashMap::new(),
        );
        assert_eq!(
            assignments.get(&entity),
            Some(&roadmap_island),
            "直前に確定したRoadmap bindingは古いSquadより優先する"
        );
    }

    #[test]
    fn transportless_forming_cargo_remains_an_existing_operation() {
        let player = PlayerId(2);
        let map = Map::new(4, 1, Terrain::Plains, GridTopology::Square);
        let island_map = IslandMap::analyze(&map);
        let island_id = island_map.islands[0].id;
        let cargo = Entity::from_raw(42);
        let mut manager = SquadManager::default();
        let placeholder = manager.create_owned_squad(MissionType::Transport, player);
        placeholder.target_island = Some(island_id);
        placeholder.target = Some(GridPosition { x: 3, y: 0 });
        placeholder.phase = MissionPhase::Forming;
        placeholder.cargo_entities.push(cargo);
        let units = vec![UnitSnapshot {
            entity: cargo,
            faction: player,
            position: GridPosition { x: 0, y: 0 },
            stats: UnitStats {
                can_capture: true,
                ..UnitStats::mock()
            },
            health: Health {
                current: 100,
                max: 100,
            },
            fuel: None,
            transporting: None,
            free_cargo_slots: 0,
            loaded_cargo_entities: Vec::new(),
        }];

        let (operations, _) = collect_existing_operations(&island_map, &manager, &units, player);

        assert_eq!(operations.len(), 1);
        assert!(operations[0].is_forming);
        assert!(operations[0].transport_entities.is_empty());
        assert_eq!(operations[0].capture_entities, vec![cargo]);
    }

    #[test]
    fn assault_transport_package_uses_each_real_airport_once() {
        let blueprints = vec![
            CampaignTransportBlueprint {
                unit_type: UnitType::TransportHelicopter,
                cost: 4_000,
                cargo_slots: 2,
                loadable_unit_types: vec![UnitType::Infantry],
                source_island: IslandId(7),
            },
            CampaignTransportBlueprint {
                unit_type: UnitType::TransportHelicopter,
                cost: 4_000,
                cargo_slots: 2,
                loadable_unit_types: vec![UnitType::Infantry],
                source_island: IslandId(7),
            },
        ];

        let package = producible_assault_transport_package(&blueprints, 4);

        assert_eq!(
            package,
            Some((vec![UnitType::TransportHelicopter; 2], 8_000, 4))
        );
    }

    #[test]
    fn assault_transport_package_does_not_mix_different_source_islands() {
        let blueprints = vec![
            CampaignTransportBlueprint {
                unit_type: UnitType::Lander,
                cost: 16_500,
                cargo_slots: 2,
                loadable_unit_types: vec![UnitType::Infantry],
                source_island: IslandId(1),
            },
            CampaignTransportBlueprint {
                unit_type: UnitType::TransportHelicopter,
                cost: 4_000,
                cargo_slots: 2,
                loadable_unit_types: vec![UnitType::Infantry],
                source_island: IslandId(2),
            },
        ];

        let package = producible_assault_transport_package(&blueprints, 4);

        assert_eq!(package, None);
    }

    #[test]
    fn assault_transport_package_does_not_reserve_a_ground_combat_wave() {
        let blueprints = vec![
            CampaignTransportBlueprint {
                unit_type: UnitType::Lander,
                cost: 16_500,
                cargo_slots: 2,
                loadable_unit_types: vec![UnitType::Infantry, UnitType::Tank],
                source_island: IslandId(1),
            },
            CampaignTransportBlueprint {
                unit_type: UnitType::TransportHelicopter,
                cost: 4_000,
                cargo_slots: 2,
                loadable_unit_types: vec![UnitType::Infantry],
                source_island: IslandId(1),
            },
            CampaignTransportBlueprint {
                unit_type: UnitType::TransportHelicopter,
                cost: 4_000,
                cargo_slots: 2,
                loadable_unit_types: vec![UnitType::Infantry],
                source_island: IslandId(1),
            },
        ];

        let package = producible_assault_transport_package(&blueprints, 4);

        assert_eq!(
            package,
            Some((vec![UnitType::TransportHelicopter; 2], 8_000, 4))
        );
    }

    fn spawn_test_unit(
        world: &mut World,
        player: PlayerId,
        position: GridPosition,
        stats: UnitStats,
    ) -> Entity {
        let max_fuel = stats.max_fuel.max(1);
        let max_ammo1 = stats.max_ammo1;
        let max_ammo2 = stats.max_ammo2;
        world
            .spawn((
                player,
                Faction(player),
                position,
                stats,
                HasMoved(false),
                ActionCompleted(false),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: max_fuel,
                    max: max_fuel,
                },
                Ammo {
                    ammo1: max_ammo1,
                    max_ammo1,
                    ammo2: max_ammo2,
                    max_ammo2,
                },
            ))
            .id()
    }

    #[test]
    fn explicit_squad_owner_conflicts_quarantine_every_referenced_entity() {
        let player_a = PlayerId(1);
        let player_b = PlayerId(2);
        let entity_a = Entity::from_raw(1);
        let entity_b = Entity::from_raw(2);
        let mut manager = SquadManager::new();
        let owned_by_a = manager.create_owned_squad(MissionType::Capture, player_a);
        owned_by_a.members.insert(entity_b);
        let owned_by_b = manager.create_owned_squad(MissionType::Transport, player_b);
        owned_by_b.cargo_entities.push(entity_a);

        let unavailable =
            mixed_owner_squad_entities(&manager, &[(entity_a, player_a), (entity_b, player_b)]);

        assert_eq!(unavailable, HashSet::from([entity_a, entity_b]));
    }

    #[test]
    fn reconstructs_initial_state_for_every_island() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let own_position = GridPosition { x: 0, y: 1 };
        let enemy_position = GridPosition { x: 2, y: 1 };
        let neutral_position = GridPosition { x: 4, y: 1 };
        let ignored_position = GridPosition { x: 6, y: 1 };
        let mut map = Map::new(7, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(own_position.x, own_position.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(enemy_position.x, enemy_position.y, Terrain::Capital)
            .unwrap();
        map.set_terrain(neutral_position.x, neutral_position.y, Terrain::City)
            .unwrap();
        map.set_terrain(ignored_position.x, ignored_position.y, Terrain::Plains)
            .unwrap();
        let island_map = IslandMap::analyze(&map);
        let own_island = island_map.get_island_at(&own_position).unwrap().id;
        let enemy_island = island_map.get_island_at(&enemy_position).unwrap().id;
        let neutral_island = island_map.get_island_at(&neutral_position).unwrap().id;
        let ignored_island = island_map.get_island_at(&ignored_position).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.insert_resource(GameRng::new(TEST_SEED));
        world.insert_resource(MatchState::default());

        world.spawn((
            own_position,
            Property::new(Terrain::Airport, Some(player), 100),
        ));
        world.spawn((
            enemy_position,
            Property::new(Terrain::Capital, Some(enemy), 100),
        ));
        world.spawn((neutral_position, Property::new(Terrain::City, None, 100)));

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .expect("infantry stats should exist");
        spawn_test_unit(&mut world, player, own_position, infantry.clone());
        spawn_test_unit(&mut world, enemy, enemy_position, infantry);

        let portfolio = analyze_island_campaign(&mut world, player);
        let states: Vec<_> = portfolio
            .islands
            .iter()
            .map(|assessment| (assessment.island_id, assessment.state))
            .collect();

        assert_eq!(
            states,
            vec![
                (own_island, IslandCampaignState::Secured),
                (enemy_island, IslandCampaignState::EnemyHeld),
                (neutral_island, IslandCampaignState::OpenNeutral),
                (ignored_island, IslandCampaignState::Ignored),
            ]
        );
        assert!(portfolio.active_offensives.len() <= 3);
        assert!(portfolio.defenses.is_empty());
    }

    #[test]
    fn sole_mainland_contested_keeps_multi_property_campaign_allocation() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let own_position = GridPosition { x: 1, y: 1 };
        let center = GridPosition { x: 2, y: 1 };
        let enemy_position = GridPosition { x: 3, y: 1 };
        let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(own_position.x, own_position.y, Terrain::Capital)
            .unwrap();
        map.set_terrain(center.x, center.y, Terrain::Plains)
            .unwrap();
        map.set_terrain(enemy_position.x, enemy_position.y, Terrain::City)
            .unwrap();
        let island_map = IslandMap::analyze(&map);
        let mainland = island_map.get_island_at(&own_position).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.insert_resource(GameRng::new(TEST_SEED));
        world.insert_resource(MatchState::default());

        world.spawn((
            own_position,
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((
            enemy_position,
            Property::new(Terrain::City, Some(enemy), 100),
        ));
        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .expect("infantry stats should exist");
        let tank = master_data
            .create_unit_stats(&UnitName(UnitType::Tank.as_str().to_owned()))
            .expect("tank stats should exist");
        spawn_test_unit(&mut world, player, own_position, infantry);
        spawn_test_unit(&mut world, enemy, enemy_position, tank);

        let facts = collect_island_campaign_facts(&mut world, player);
        let mainland_facts = facts
            .iter()
            .find(|facts| facts.island_id == mainland)
            .expect("mainland facts should exist");
        assert_eq!(mainland_facts.enemy_combat_units, 1);
        assert_eq!(mainland_facts.friendly_combat_units, 1);

        let portfolio = analyze_island_campaign(&mut world, player);
        let assessment = portfolio
            .islands
            .iter()
            .find(|assessment| assessment.island_id == mainland)
            .expect("mainland assessment should remain visible");

        assert_eq!(assessment.state, IslandCampaignState::Contested);
        assert_ne!(assessment.decision, IslandCampaignDecision::Observe);
        let assignment = portfolio
            .assignment_for(mainland)
            .expect("単一大陸でも継続campaignを生成する");
        assert_eq!(assignment.capture_target_positions, vec![enemy_position]);
        assert!(assignment.requirement.capture_units >= 1);
        assert_eq!(assignment.requirement.transport_slots, 0);
        assert!(!portfolio.active_offensives.is_empty());
        assert!(portfolio.defenses.is_empty());
        assert!(!portfolio.aggregate_missing_requirements().is_empty());
    }

    #[test]
    fn counts_combat_entities_without_support_units_or_price_weighting() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let position = GridPosition { x: 1, y: 1 };
        let mut map = Map::new(3, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(position.x, position.y, Terrain::Factory)
            .unwrap();
        let island_map = IslandMap::analyze(&map);
        let island_id = island_map.get_island_at(&position).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((position, Property::new(Terrain::Factory, Some(player), 100)));

        let mut infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        infantry.cost = 1_000;
        let damaged = spawn_test_unit(&mut world, player, position, infantry.clone());
        world.get_mut::<Health>(damaged).unwrap().current = 50;
        spawn_test_unit(&mut world, enemy, position, infantry);

        let mut helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        helicopter.cost = 4_000;
        spawn_test_unit(&mut world, player, position, helicopter);

        let facts = collect_island_campaign_facts(&mut world, player);
        let island = facts
            .iter()
            .find(|facts| facts.island_id == island_id)
            .unwrap();

        assert_eq!(island.friendly_units, 2);
        assert_eq!(island.enemy_units, 1);
        assert_eq!(island.friendly_combat_units, 1);
        assert_eq!(island.enemy_combat_units, 1);
        assert_eq!(island.friendly_properties, 1);
        assert_eq!(island.strategic_production_sites, 1);
        assert_eq!(island.roi_production_sites, 1);
    }

    #[test]
    fn attributes_loaded_cargo_to_the_live_transport_target_once() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let origin = GridPosition { x: 0, y: 1 };
        let target = GridPosition { x: 4, y: 1 };
        let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = IslandMap::analyze(&map);
        let origin_island = island_map.get_island_at(&origin).unwrap().id;
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((origin, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, None, 100)));

        let mut infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        infantry.cost = 1_000;
        let cargo = spawn_test_unit(&mut world, player, origin, infantry);
        let mut helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        helicopter.cost = 4_000;
        let transport = spawn_test_unit(&mut world, player, origin, helicopter.clone());
        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: vec![cargo],
        });
        world
            .entity_mut(cargo)
            .insert((GridPosition { x: 9_999, y: 9_999 }, Transporting(transport)));

        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities.push(cargo);
        squad.target_island = Some(target_island);
        squad.target = Some(target);
        squad.phase = MissionPhase::Transport(TransportPhase::Transit);
        world.insert_resource(manager);

        let facts = collect_island_campaign_facts(&mut world, player);
        let origin_facts = facts
            .iter()
            .find(|facts| facts.island_id == origin_island)
            .unwrap();
        let target_facts = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();

        assert_eq!(origin_facts.friendly_combat_units, 0);
        assert_eq!(target_facts.friendly_units, 0);
        assert_eq!(target_facts.friendly_combat_units, 1);
        assert!(target_facts.friendly_arrival_eta.is_some());
        assert!(target_facts.friendly_capture_eta.is_some());
    }

    #[test]
    fn mismatched_loaded_cargo_is_not_attributed_to_the_claiming_transport_target() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let origin = GridPosition { x: 0, y: 0 };
        let target = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((origin, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, None, 100)));

        let mut infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        infantry.cost = 1_000;
        let cargo = spawn_test_unit(
            &mut world,
            player,
            GridPosition { x: 9_999, y: 9_999 },
            infantry,
        );
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let claiming_transport = spawn_test_unit(&mut world, player, origin, helicopter.clone());
        world.entity_mut(claiming_transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: Vec::new(),
        });
        let actual_transport = spawn_test_unit(&mut world, player, origin, helicopter.clone());
        world.entity_mut(actual_transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: vec![cargo],
        });
        world
            .entity_mut(cargo)
            .insert(Transporting(actual_transport));

        let mut manager = SquadManager::new();
        let stale = manager.create_squad(MissionType::Transport);
        stale.members.insert(claiming_transport);
        stale.transport_entity = Some(claiming_transport);
        stale.cargo_entities = vec![cargo];
        stale.target_island = Some(target_island);
        stale.target = Some(target);
        stale.phase = MissionPhase::Transport(TransportPhase::Transit);
        world.insert_resource(manager);

        let facts = collect_island_campaign_facts(&mut world, player);
        let target_facts = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();
        assert_eq!(target_facts.friendly_combat_units, 0);
        assert_eq!(target_facts.friendly_capture_eta, None);
    }

    #[test]
    fn opponent_transport_operation_is_not_friendly_campaign_continuity() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let opponent = PlayerId(2);
        for entry in &mut world.resource_mut::<Players>().0 {
            if entry.id == player {
                entry.funds = 0;
            }
        }
        let origin = GridPosition { x: 0, y: 0 };
        let target = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((origin, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, None, 100)));

        let mut infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        infantry.cost = 1_000;
        let cargo = spawn_test_unit(&mut world, opponent, origin, infantry);
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let transport = spawn_test_unit(&mut world, opponent, origin, helicopter.clone());
        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: vec![cargo],
        });
        world
            .entity_mut(cargo)
            .insert((GridPosition { x: 9_999, y: 9_999 }, Transporting(transport)));

        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities.push(cargo);
        squad.target_island = Some(target_island);
        squad.target = Some(target);
        squad.phase = MissionPhase::Transport(TransportPhase::Transit);
        world.insert_resource(manager);

        let facts = collect_island_campaign_facts(&mut world, player);
        let target_facts = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();
        let portfolio = analyze_island_campaign(&mut world, player);

        assert_eq!(target_facts.friendly_combat_units, 0);
        assert_eq!(target_facts.friendly_arrival_eta, None);
        assert_eq!(target_facts.friendly_capture_eta, None);
        assert!(portfolio.assignment_for(target_island).is_none());
    }

    #[test]
    fn treats_a_foothold_as_reachable_but_rejects_missing_transport_modes() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let foothold = GridPosition { x: 0, y: 1 };
        let unreachable = GridPosition { x: 2, y: 1 };
        let mut map = Map::new(3, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(foothold.x, foothold.y, Terrain::Factory)
            .unwrap();
        map.set_terrain(unreachable.x, unreachable.y, Terrain::City)
            .unwrap();
        let island_map = IslandMap::analyze(&map);
        let foothold_island = island_map.get_island_at(&foothold).unwrap().id;
        let unreachable_island = island_map.get_island_at(&unreachable).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((foothold, Property::new(Terrain::Factory, Some(player), 100)));
        world.spawn((unreachable, Property::new(Terrain::City, None, 100)));

        let facts = collect_island_campaign_facts(&mut world, player);
        let foothold_facts = facts
            .iter()
            .find(|facts| facts.island_id == foothold_island)
            .unwrap();
        let unreachable_facts = facts
            .iter()
            .find(|facts| facts.island_id == unreachable_island)
            .unwrap();

        assert!(foothold_facts.reachable);
        assert!(!unreachable_facts.reachable);
        assert_eq!(unreachable_facts.transport_eta, None);
    }

    #[test]
    fn pickup_phase_eta_includes_pickup_transit_and_capture() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let transport_position = GridPosition { x: 0, y: 1 };
        let pickup_position = GridPosition { x: 2, y: 1 };
        let target_position = GridPosition { x: 4, y: 1 };
        let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(transport_position.x, transport_position.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(pickup_position.x, pickup_position.y, Terrain::Plains)
            .unwrap();
        map.set_terrain(target_position.x, target_position.y, Terrain::City)
            .unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target_position).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((
            transport_position,
            Property::new(Terrain::Airport, Some(player), 100),
        ));
        world.spawn((target_position, Property::new(Terrain::City, None, 100)));

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let cargo = spawn_test_unit(&mut world, player, pickup_position, infantry);
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let transport = spawn_test_unit(&mut world, player, transport_position, helicopter.clone());
        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: Vec::new(),
        });

        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities.push(cargo);
        squad.pickup_position = Some(pickup_position);
        squad.target_island = Some(target_island);
        squad.target = Some(target_position);
        squad.phase = MissionPhase::Transport(TransportPhase::Pickup);
        world.insert_resource(manager);

        let facts = collect_island_campaign_facts(&mut world, player);
        let target_facts = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();

        assert_eq!(target_facts.friendly_capture_eta, Some(4));
    }

    #[test]
    fn drop_phase_capture_eta_starts_from_the_live_drop_position() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let origin = GridPosition { x: 0, y: 1 };
        let drop_position = GridPosition { x: 3, y: 1 };
        let target_position = GridPosition { x: 4, y: 1 };
        let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(drop_position.x, drop_position.y, Terrain::Plains)
            .unwrap();
        map.set_terrain(target_position.x, target_position.y, Terrain::City)
            .unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target_position).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((origin, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((target_position, Property::new(Terrain::City, None, 100)));

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let cargo = spawn_test_unit(&mut world, player, origin, infantry);
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let transport = spawn_test_unit(&mut world, player, drop_position, helicopter.clone());
        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: vec![cargo],
        });
        world
            .entity_mut(cargo)
            .insert((GridPosition { x: 9_999, y: 9_999 }, Transporting(transport)));

        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities.push(cargo);
        squad.drop_position = Some(drop_position);
        squad.target_island = Some(target_island);
        squad.target = Some(target_position);
        squad.phase = MissionPhase::Transport(TransportPhase::Drop);
        world.insert_resource(manager);

        let facts = collect_island_campaign_facts(&mut world, player);
        let target_facts = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();

        assert_eq!(target_facts.friendly_capture_eta, Some(2));
    }

    #[test]
    fn transit_capture_eta_is_conservative_before_drop_selection() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let origin = GridPosition { x: 0, y: 1 };
        let coast = GridPosition { x: 2, y: 1 };
        let middle = GridPosition { x: 3, y: 1 };
        let target = GridPosition { x: 4, y: 1 };
        let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(coast.x, coast.y, Terrain::Plains).unwrap();
        map.set_terrain(middle.x, middle.y, Terrain::Plains)
            .unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((origin, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, None, 100)));

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let cargo = spawn_test_unit(&mut world, player, origin, infantry);
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let transport = spawn_test_unit(&mut world, player, origin, helicopter.clone());
        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: vec![cargo],
        });
        world
            .entity_mut(cargo)
            .insert((GridPosition { x: 9_999, y: 9_999 }, Transporting(transport)));

        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities.push(cargo);
        squad.target_island = Some(target_island);
        squad.target = Some(target);
        squad.phase = MissionPhase::Transport(TransportPhase::Transit);
        world.insert_resource(manager);

        let facts = collect_island_campaign_facts(&mut world, player);
        let target_facts = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();

        assert_eq!(target_facts.friendly_capture_eta, Some(3));
    }

    #[test]
    fn repeated_analysis_does_not_mutate_squads_or_shared_distance_cache() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let origin = GridPosition { x: 0, y: 1 };
        let target = GridPosition { x: 2, y: 1 };
        let mut map = Map::new(3, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((origin, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, None, 100)));

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let cargo = spawn_test_unit(&mut world, player, origin, infantry);
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let transport = spawn_test_unit(&mut world, player, origin, helicopter.clone());
        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: vec![cargo],
        });
        world
            .entity_mut(cargo)
            .insert((GridPosition { x: 9_999, y: 9_999 }, Transporting(transport)));

        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities.push(cargo);
        squad.pickup_position = Some(origin);
        squad.target_island = Some(target_island);
        squad.target = Some(target);
        squad.phase = MissionPhase::Transport(TransportPhase::Pickup);
        world.insert_resource(manager);

        let cache_key = (0, 0, 0, 0, MovementType::Air, 1, 0, 0, player);
        let sentinel = crate::ai::turn_distance::TurnDistance {
            turns: 77,
            used_mp: 88,
        };
        let mut shared_cache = TurnDistanceCache::default();
        shared_cache.cache.insert(cache_key, sentinel);
        world.insert_resource(shared_cache);

        let first = collect_island_campaign_facts(&mut world, player);
        let second = collect_island_campaign_facts(&mut world, player);

        assert_eq!(first, second);
        assert_eq!(
            world.resource::<SquadManager>().squads[0].phase,
            MissionPhase::Transport(TransportPhase::Pickup)
        );
        assert_eq!(
            world.resource::<TurnDistanceCache>().cache.get(&cache_key),
            Some(&sentinel)
        );
        assert_eq!(world.resource::<TurnDistanceCache>().cache.len(), 1);
    }

    fn transport_preference_world(
        target_owner: Option<PlayerId>,
        held_transport: UnitType,
        held_cost: u32,
    ) -> (World, IslandId) {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let port = GridPosition { x: 0, y: 1 };
        let airport = GridPosition { x: 0, y: 0 };
        let target = GridPosition { x: 4, y: 1 };
        let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(port.x, port.y, Terrain::Port).unwrap();
        map.set_terrain(airport.x, airport.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(target.x, target.y, Terrain::Port).unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((port, Property::new(Terrain::Port, Some(player), 100)));
        world.spawn((airport, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, target_owner, 100)));
        if let Some(owner) = target_owner {
            let infantry = master_data
                .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
                .unwrap();
            spawn_test_unit(&mut world, owner, target, infantry);
        }

        let mut stats = master_data
            .create_unit_stats(&UnitName(held_transport.as_str().to_owned()))
            .unwrap();
        stats.cost = held_cost;
        let position = if held_transport == UnitType::Lander {
            port
        } else {
            airport
        };
        let max_cargo = stats.max_cargo;
        let transport = spawn_test_unit(&mut world, player, position, stats);
        world.entity_mut(transport).insert(CargoCapacity {
            max: max_cargo,
            loaded: Vec::new(),
        });
        (world, target_island)
    }

    #[test]
    fn open_neutral_prefers_producible_helicopter_over_cheaper_held_lander() {
        let player = PlayerId(1);
        let (mut world, target_island) = transport_preference_world(None, UnitType::Lander, 1);

        let facts = collect_island_campaign_facts(&mut world, player);
        let target = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();

        assert_eq!(target.missing_expansion_package_cost, 6_000);
    }

    #[test]
    fn enemy_held_uses_cheapest_reachable_transport_fallback() {
        let player = PlayerId(1);
        let (mut world, target_island) =
            transport_preference_world(Some(PlayerId(2)), UnitType::Lander, 1);

        let facts = collect_island_campaign_facts(&mut world, player);
        let target = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();

        assert_eq!(target.missing_expansion_package_cost, 2_000);
    }

    #[test]
    fn held_helicopter_is_not_charged_as_a_producible_transport() {
        let player = PlayerId(1);
        let (mut world, target_island) =
            transport_preference_world(None, UnitType::TransportHelicopter, 4_000);

        let facts = collect_island_campaign_facts(&mut world, player);
        let target = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();

        assert_eq!(target.missing_expansion_package_cost, 2_000);
    }

    #[test]
    fn transport_stats_without_cargo_capacity_do_not_satisfy_campaign_package() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let origin = GridPosition { x: 0, y: 0 };
        let target = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::City).unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((origin, Property::new(Terrain::City, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, None, 100)));
        world.resource_mut::<Players>().0[0].funds = 0;

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        spawn_test_unit(&mut world, player, origin, infantry.clone());
        spawn_test_unit(&mut world, player, origin, infantry);
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let transport = spawn_test_unit(&mut world, player, origin, helicopter.clone());

        let without_capacity = collect_island_campaign_facts(&mut world, player);
        let target_facts = without_capacity
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();
        assert!(!target_facts.reachable);
        assert!(
            analyze_island_campaign(&mut world, player)
                .assignment_for(target_island)
                .is_none()
        );

        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: Vec::new(),
        });
        let with_capacity = analyze_island_campaign(&mut world, player);
        let assignment = with_capacity
            .assignment_for(target_island)
            .expect("CargoCapacity-backed transport must satisfy the package");
        assert!(assignment.operation_ready);
        assert_eq!(assignment.transport_entities, vec![transport]);

        world.get_mut::<Fuel>(transport).unwrap().current = 0;
        let without_fuel = collect_island_campaign_facts(&mut world, player);
        let target_facts = without_fuel
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();
        assert!(!target_facts.reachable);
        assert!(
            analyze_island_campaign(&mut world, player)
                .assignment_for(target_island)
                .is_none(),
            "zero-fuel transport must not satisfy a campaign package"
        );
    }

    #[test]
    fn pickup_eta_waits_for_distant_cargo_before_loading() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let cargo_position = GridPosition { x: 0, y: 1 };
        let pickup = GridPosition { x: 6, y: 1 };
        let target = GridPosition { x: 8, y: 1 };
        let mut map = Map::new(9, 3, Terrain::Sea, GridTopology::Square);
        for x in 0..=5 {
            map.set_terrain(x, 1, Terrain::Plains).unwrap();
        }
        map.set_terrain(pickup.x, pickup.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((pickup, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, None, 100)));

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let cargo = spawn_test_unit(&mut world, player, cargo_position, infantry);
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let transport = spawn_test_unit(&mut world, player, pickup, helicopter.clone());
        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: Vec::new(),
        });

        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities.push(cargo);
        squad.pickup_position = Some(pickup);
        squad.target_island = Some(target_island);
        squad.target = Some(target);
        squad.phase = MissionPhase::Transport(TransportPhase::Pickup);
        world.insert_resource(manager);

        let facts = collect_island_campaign_facts(&mut world, player);
        let target = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();

        assert_eq!(target.friendly_capture_eta, Some(5));
    }

    #[test]
    fn pickup_eta_is_none_when_assigned_cargo_cannot_reach_rendezvous() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let cargo_position = GridPosition { x: 0, y: 1 };
        let pickup = GridPosition { x: 2, y: 1 };
        let target = GridPosition { x: 4, y: 1 };
        let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(cargo_position.x, cargo_position.y, Terrain::Plains)
            .unwrap();
        map.set_terrain(pickup.x, pickup.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((pickup, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, None, 100)));

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let cargo = spawn_test_unit(&mut world, player, cargo_position, infantry.clone());
        let reachable_cargo = spawn_test_unit(&mut world, player, pickup, infantry);
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let transport = spawn_test_unit(&mut world, player, pickup, helicopter.clone());
        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: Vec::new(),
        });

        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities.push(cargo);
        squad.cargo_entities.push(reachable_cargo);
        squad.pickup_position = Some(pickup);
        squad.target_island = Some(target_island);
        squad.target = Some(target);
        squad.phase = MissionPhase::Transport(TransportPhase::Pickup);
        world.insert_resource(manager);

        let facts = collect_island_campaign_facts(&mut world, player);
        let target = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();

        assert_eq!(target.friendly_capture_eta, None);
    }

    #[test]
    fn transit_eta_uses_planned_drop_instead_of_nearest_island_tile() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let origin = GridPosition { x: 0, y: 1 };
        let nearest_coast = GridPosition { x: 2, y: 1 };
        let planned_drop = GridPosition { x: 8, y: 1 };
        let mut map = Map::new(9, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::Airport)
            .unwrap();
        for x in nearest_coast.x..=planned_drop.x {
            map.set_terrain(x, 1, Terrain::Plains).unwrap();
        }
        map.set_terrain(planned_drop.x, planned_drop.y, Terrain::City)
            .unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&planned_drop).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((origin, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((planned_drop, Property::new(Terrain::City, None, 100)));

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let cargo = spawn_test_unit(&mut world, player, origin, infantry);
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let transport = spawn_test_unit(&mut world, player, origin, helicopter.clone());
        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: vec![cargo],
        });
        world
            .entity_mut(cargo)
            .insert((GridPosition { x: 9_999, y: 9_999 }, Transporting(transport)));

        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities.push(cargo);
        squad.drop_position = Some(planned_drop);
        squad.target_island = Some(target_island);
        squad.target = Some(planned_drop);
        squad.phase = MissionPhase::Transport(TransportPhase::Transit);
        world.insert_resource(manager);

        let facts = collect_island_campaign_facts(&mut world, player);
        let target = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();

        assert_eq!(target.friendly_capture_eta, Some(3));
    }

    #[test]
    fn empty_return_transport_is_reused_but_loaded_return_is_not() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let origin = GridPosition { x: 0, y: 1 };
        let target = GridPosition { x: 2, y: 1 };
        let mut map = Map::new(3, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((origin, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, None, 100)));

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let cargo = spawn_test_unit(&mut world, player, origin, infantry);
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let transport = spawn_test_unit(&mut world, player, origin, helicopter.clone());
        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: vec![cargo],
        });
        world
            .entity_mut(cargo)
            .insert((GridPosition { x: 9_999, y: 9_999 }, Transporting(transport)));
        let active_transport = spawn_test_unit(&mut world, player, origin, helicopter.clone());
        world.entity_mut(active_transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: Vec::new(),
        });
        let reusable_return = spawn_test_unit(&mut world, player, origin, helicopter.clone());
        world.entity_mut(reusable_return).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: Vec::new(),
        });

        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities.push(cargo);
        squad.target_island = Some(target_island);
        squad.target = Some(target);
        squad.phase = MissionPhase::Transport(TransportPhase::Return);
        let reusable_squad = manager.create_squad(MissionType::Transport);
        reusable_squad.members.insert(reusable_return);
        reusable_squad.transport_entity = Some(reusable_return);
        reusable_squad.target_island = Some(target_island);
        reusable_squad.target = Some(target);
        reusable_squad.phase = MissionPhase::Transport(TransportPhase::Return);
        let active_squad = manager.create_squad(MissionType::Transport);
        active_squad.members.insert(active_transport);
        active_squad.transport_entity = Some(active_transport);
        active_squad.target_island = Some(target_island);
        active_squad.target = Some(target);
        active_squad.phase = MissionPhase::Transport(TransportPhase::Transit);
        world.insert_resource(manager);

        let facts = collect_island_campaign_facts(&mut world, player);
        let target = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();

        assert_eq!(target.missing_expansion_package_cost, 2_000);
        let portfolio = analyze_island_campaign(&mut world, player);
        let assignment = portfolio.assignment_for(target_island).unwrap();
        assert!(assignment.transport_entities.contains(&reusable_return));
        assert!(!assignment.transport_entities.contains(&transport));
    }

    #[test]
    fn targetless_live_drop_with_unrelated_cargo_is_unavailable_to_new_campaigns() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let origin = GridPosition { x: 0, y: 0 };
        let target = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::City).unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((origin, Property::new(Terrain::City, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, None, 100)));
        world.resource_mut::<Players>().0[0].funds = 0;

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        spawn_test_unit(&mut world, player, origin, infantry.clone());
        spawn_test_unit(&mut world, player, origin, infantry.clone());
        let unrelated_cargo = spawn_test_unit(
            &mut world,
            player,
            GridPosition { x: 9_999, y: 9_999 },
            infantry,
        );
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let transport = spawn_test_unit(&mut world, player, origin, helicopter.clone());
        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: vec![unrelated_cargo],
        });
        world
            .entity_mut(unrelated_cargo)
            .insert(Transporting(transport));

        let mut manager = SquadManager::new();
        let safe_drop = manager.create_squad(MissionType::Transport);
        safe_drop.members.insert(transport);
        safe_drop.transport_entity = Some(transport);
        safe_drop.cargo_entities = vec![unrelated_cargo];
        safe_drop.target_island = None;
        safe_drop.target = None;
        safe_drop.phase = MissionPhase::Transport(TransportPhase::Drop);
        world.insert_resource(manager.clone());

        let facts = collect_island_campaign_facts(&mut world, player);
        let target_facts = facts
            .iter()
            .find(|facts| facts.island_id == target_island)
            .unwrap();
        assert!(!target_facts.reachable);
        assert_eq!(target_facts.transport_eta, None);

        let first = analyze_island_campaign(&mut world, player);
        let second = analyze_island_campaign(&mut world, player);
        assert!(first.assignment_for(target_island).is_none());
        assert_eq!(second, first);
        let retained = &world.resource::<SquadManager>().squads[0];
        assert_eq!(retained.transport_entity, Some(transport));
        assert_eq!(retained.cargo_entities, vec![unrelated_cargo]);
        assert_eq!(retained.target_island, None);
        assert_eq!(retained.target, None);
        assert_eq!(
            retained.phase,
            MissionPhase::Transport(TransportPhase::Drop)
        );
    }

    #[test]
    fn campaign_requirements_reserve_full_target_power_before_adding_reinforcements() {
        let mut threatened_facts = IslandCampaignFacts {
            island_id: IslandId(0),
            capturable_properties: 1,
            strategic_production_sites: 0,
            roi_production_sites: 0,
            neutral_properties: 0,
            friendly_properties: 1,
            enemy_properties: 0,
            friendly_units: 1,
            enemy_units: 0,
            friendly_combat_units: 1,
            enemy_combat_units: 2,
            friendly_arrival_eta: Some(0),
            enemy_arrival_eta: Some(1),
            friendly_capture_eta: None,
            enemy_capture_eta: None,
            transport_eta: Some(0),
            capture_turns: 0,
            island_income_per_turn: 1_000,
            missing_expansion_package_cost: 0,
            reachable: true,
            has_unowned_properties: false,
        };
        let mut defense = assess_island(&threatened_facts);
        let defense_requirement = requirement_for_assessment(&threatened_facts, &mut defense);
        assert_eq!(defense_requirement.combat_units, 2);

        threatened_facts.friendly_units = 1;
        threatened_facts.enemy_units = 1;
        threatened_facts.friendly_properties = 0;
        threatened_facts.friendly_capture_eta = Some(5);
        threatened_facts.enemy_capture_eta = Some(2);
        let mut reinforcement = assess_island(&threatened_facts);
        let reinforcement_requirement =
            requirement_for_assessment(&threatened_facts, &mut reinforcement);
        assert_eq!(reinforcement.decision, IslandCampaignDecision::Reinforce);
        assert_eq!(reinforcement_requirement.combat_units, 2);

        threatened_facts.friendly_units = 0;
        threatened_facts.enemy_units = 1;
        threatened_facts.enemy_properties = 1;
        threatened_facts.friendly_capture_eta = None;
        threatened_facts.enemy_capture_eta = None;
        let mut assault = assess_island(&threatened_facts);
        let assault_requirement = requirement_for_assessment(&threatened_facts, &mut assault);
        assert_eq!(assault.decision, IslandCampaignDecision::Assault);
        assert_eq!(assault_requirement.combat_units, 2);
        // 純粋な状態評価では占領・戦闘だけを積み、輸送費と地上枠は実盤面・生産経路の
        // 分析後に加える。ここで固定のLander＋helicopter費を先取りしない。
        assert_eq!(assault_requirement.ground_combat_units, 0);
        assert_eq!(assault_requirement.total_budget, 2_000);
    }

    #[test]
    fn campaign_requirements_keep_one_parallel_capturer_per_unowned_property() {
        let mut secured_facts = IslandCampaignFacts {
            island_id: IslandId(0),
            capturable_properties: 4,
            strategic_production_sites: 0,
            roi_production_sites: 0,
            neutral_properties: 3,
            friendly_properties: 1,
            enemy_properties: 0,
            friendly_units: 1,
            enemy_units: 0,
            friendly_combat_units: 1,
            enemy_combat_units: 0,
            friendly_arrival_eta: Some(0),
            enemy_arrival_eta: None,
            friendly_capture_eta: Some(1),
            enemy_capture_eta: None,
            transport_eta: Some(0),
            capture_turns: 1,
            island_income_per_turn: 4_000,
            missing_expansion_package_cost: 0,
            reachable: true,
            has_unowned_properties: true,
        };
        let mut secure = assess_island(&secured_facts);
        let secure_requirement = requirement_for_assessment(&secured_facts, &mut secure);
        assert_eq!(secure.decision, IslandCampaignDecision::Secure);
        assert_eq!(secure_requirement.capture_units, 3);
        assert_eq!(secure_requirement.total_budget, 3_000);

        secured_facts.enemy_arrival_eta = Some(1);
        let mut threatened = assess_island(&secured_facts);
        let threatened_requirement = requirement_for_assessment(&secured_facts, &mut threatened);
        assert_eq!(threatened.decision, IslandCampaignDecision::Defend);
        assert_eq!(threatened_requirement.capture_units, 3);
        assert_eq!(threatened_requirement.total_budget, 3_000);

        secured_facts.enemy_units = 1;
        secured_facts.friendly_combat_units = 3;
        secured_facts.enemy_combat_units = 2;
        secured_facts.enemy_capture_eta = Some(2);
        let mut contest = assess_island(&secured_facts);
        let contest_requirement = requirement_for_assessment(&secured_facts, &mut contest);
        assert_eq!(contest.decision, IslandCampaignDecision::Contest);
        assert_eq!(contest_requirement.capture_units, 3);
        assert_eq!(contest_requirement.total_budget, 3_000);
    }

    #[test]
    fn analyze_reserves_a_complete_held_expansion_package_without_mutating_world() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let origin = GridPosition { x: 0, y: 1 };
        let target = GridPosition { x: 2, y: 1 };
        let mut map = Map::new(3, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((origin, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, None, 100)));

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let capture_a = spawn_test_unit(&mut world, player, origin, infantry.clone());
        let capture_b = spawn_test_unit(&mut world, player, origin, infantry);
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let transport = spawn_test_unit(&mut world, player, origin, helicopter.clone());
        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: Vec::new(),
        });
        world.resource_mut::<Players>().0[0].funds = 0;

        let before_funds = world.resource::<Players>().0[0].funds;
        let portfolio = analyze_island_campaign(&mut world, player);

        let assignment = portfolio.assignment_for(target_island).unwrap();
        assert_eq!(assignment.decision, IslandCampaignDecision::Expand);
        assert_eq!(assignment.transport_entities, vec![transport]);
        let mut expected_capture_entities = vec![capture_a, capture_b];
        expected_capture_entities.sort_by_key(|entity| entity.to_bits());
        assert_eq!(assignment.capture_entities, expected_capture_entities);
        assert_eq!(assignment.purchase_shortfall.total_budget, 0);
        assert!(assignment.operation_ready);
        assert_eq!(world.resource::<Players>().0[0].funds, before_funds);
        assert_eq!(world.get::<GridPosition>(transport), Some(&origin));
        assert_eq!(world.get::<GridPosition>(capture_a), Some(&origin));
        assert_eq!(world.get::<GridPosition>(capture_b), Some(&origin));
    }

    #[test]
    fn solo_fallback_capture_is_not_reused_by_campaign_pool() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let origin = GridPosition { x: 0, y: 1 };
        let target = GridPosition { x: 2, y: 1 };
        let mut map = Map::new(3, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((origin, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, None, 100)));

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let fallback = spawn_test_unit(&mut world, player, origin, infantry.clone());
        spawn_test_unit(&mut world, player, origin, infantry);
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let transport = spawn_test_unit(&mut world, player, origin, helicopter.clone());
        world.entity_mut(transport).insert(CargoCapacity {
            max: helicopter.max_cargo,
            loaded: Vec::new(),
        });
        world.resource_mut::<Players>().0[0].funds = 0;
        let mut manager = SquadManager::new();
        manager.solo_fallbacks.insert(fallback);
        world.insert_resource(manager);

        let portfolio = analyze_island_campaign(&mut world, player);

        assert!(portfolio.assignment_for(target_island).is_none());
        assert!(
            portfolio
                .defenses
                .iter()
                .chain(portfolio.active_offensives.iter())
                .flat_map(|assignment| {
                    assignment
                        .transport_entities
                        .iter()
                        .chain(assignment.capture_entities.iter())
                        .chain(assignment.combat_entities.iter())
                })
                .all(|entity| *entity != fallback)
        );
    }

    #[test]
    fn analyze_does_not_reuse_return_phase_delivered_cargo_as_a_held_candidate() {
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let origin = GridPosition { x: 0, y: 1 };
        let target = GridPosition { x: 2, y: 1 };
        let mut map = Map::new(3, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((origin, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::City, None, 100)));

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let returned_cargo = spawn_test_unit(&mut world, player, origin, infantry);
        let helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let returning_transport = spawn_test_unit(&mut world, player, origin, helicopter.clone());
        let active_transport = spawn_test_unit(&mut world, player, origin, helicopter);

        let mut manager = SquadManager::new();
        let returning = manager.create_squad(MissionType::Transport);
        returning.members.insert(returning_transport);
        returning.transport_entity = Some(returning_transport);
        returning.delivered_cargo.push(returned_cargo);
        returning.target_island = Some(target_island);
        returning.target = Some(target);
        returning.phase = MissionPhase::Transport(TransportPhase::Return);
        let active = manager.create_squad(MissionType::Transport);
        active.members.insert(active_transport);
        active.transport_entity = Some(active_transport);
        active.target_island = Some(target_island);
        active.target = Some(target);
        active.phase = MissionPhase::Transport(TransportPhase::Transit);
        world.insert_resource(manager);
        world.resource_mut::<Players>().0[0].funds = 1_000;

        let portfolio = analyze_island_campaign(&mut world, player);

        assert!(portfolio.assignment_for(target_island).is_none());
        let target_assessment = portfolio
            .islands
            .iter()
            .find(|assessment| assessment.island_id == target_island)
            .unwrap();
        assert_eq!(target_assessment.decision, IslandCampaignDecision::Observe);
    }
}
