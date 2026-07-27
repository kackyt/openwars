use std::collections::HashMap;

use bevy_ecs::event::EventCursor;
use bevy_ecs::prelude::*;
use serde::Serialize;

use engine::ai::islands::IslandMap;
use engine::ai::squad::{MissionPhase, MissionType, SquadManager};
use engine::components::{CargoCapacity, Faction, GridPosition};
use engine::events::{
    PropertyCaptureProgressedEvent, UnitAttackedEvent, UnitLoadedEvent, UnitUnloadedEvent,
};

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
        x: usize,
        y: usize,
        island_id: Option<usize>,
    },
    UnitAttacked {
        turn: u32,
        player_id: u32,
        step: usize,
        attacker_id: u64,
        defender_id: u64,
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

pub struct InvasionTraceCollector {
    loaded_cursor: EventCursor<UnitLoadedEvent>,
    unloaded_cursor: EventCursor<UnitUnloadedEvent>,
    attacked_cursor: EventCursor<UnitAttackedEvent>,
    capture_cursor: EventCursor<PropertyCaptureProgressedEvent>,
}

impl InvasionTraceCollector {
    pub fn new(world: &World) -> Self {
        Self {
            loaded_cursor: world.resource::<Events<UnitLoadedEvent>>().get_cursor(),
            unloaded_cursor: world.resource::<Events<UnitUnloadedEvent>>().get_cursor(),
            attacked_cursor: world.resource::<Events<UnitAttackedEvent>>().get_cursor(),
            capture_cursor: world
                .resource::<Events<PropertyCaptureProgressedEvent>>()
                .get_cursor(),
        }
    }

    pub fn collect_step(
        &mut self,
        world: &World,
        turn: u32,
        player_id: u32,
        step: usize,
        positions_before: &HashMap<u64, GridPosition>,
    ) -> Vec<InvasionEvent> {
        let mut result = Vec::new();
        let island_map = world.resource::<IslandMap>();

        for event in self
            .loaded_cursor
            .read(world.resource::<Events<UnitLoadedEvent>>())
        {
            let position = positions_before.get(&event.cargo.to_bits()).copied();
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
            let position = GridPosition {
                x: event.target_x,
                y: event.target_y,
            };
            result.push(InvasionEvent::UnitUnloaded {
                turn,
                player_id,
                step,
                transport_id: event.transport.to_bits(),
                cargo_id: event.cargo.to_bits(),
                x: event.target_x,
                y: event.target_y,
                island_id: island_map
                    .get_island_at(&position)
                    .map(|island| island.id.0),
            });
        }
        for event in self
            .attacked_cursor
            .read(world.resource::<Events<UnitAttackedEvent>>())
        {
            result.push(InvasionEvent::UnitAttacked {
                turn,
                player_id,
                step,
                attacker_id: event.attacker.to_bits(),
                defender_id: event.defender.to_bits(),
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

pub fn snapshot_unit_positions(world: &mut World) -> HashMap<u64, GridPosition> {
    let mut query = world.query::<(Entity, &GridPosition)>();
    query
        .iter(world)
        .map(|(entity, position)| (entity.to_bits(), *position))
        .collect()
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
