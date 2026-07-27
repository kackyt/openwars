use std::collections::HashSet;

use bevy_ecs::prelude::*;

use crate::ai::{AiVersion, PlayerAiSettings};
use crate::components::*;
use crate::events::{
    PropertyCaptureProgressedEvent, UnitAttackedEvent, UnitLoadedEvent, UnitUnloadedEvent,
};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::*;

const MAX_ROUNDS: usize = 12;
const MAX_ACTIONS_PER_PHASE: usize = 32;
const TEST_SEED: u64 = 42;

#[test]
fn v3_invasion_reaches_combat_or_capture_after_landing() {
    let master_data = MasterDataRegistry::load().expect("master data should load");
    let (mut world, mut schedule) =
        crate::setup::initialize_world_from_master_data(&master_data, "map_1")
            .expect("test world should initialize");
    let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
    for entity in entities {
        world.despawn(entity);
    }

    let p1 = PlayerId(1);
    let p2 = PlayerId(2);
    let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
    map.set_terrain(0, 1, Terrain::Port).unwrap();
    map.set_terrain(3, 1, Terrain::Shoal).unwrap();
    map.set_terrain(3, 0, Terrain::Plains).unwrap();
    map.set_terrain(4, 0, Terrain::Capital).unwrap();
    map.set_terrain(4, 1, Terrain::Plains).unwrap();
    map.set_terrain(4, 2, Terrain::Plains).unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let departure_island = island_map
        .get_island_at(&GridPosition { x: 0, y: 1 })
        .expect("departure island should exist")
        .id;
    let enemy_island = island_map
        .get_island_at(&GridPosition { x: 4, y: 0 })
        .expect("enemy island should exist")
        .id;
    assert_ne!(departure_island, enemy_island);
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.insert_resource(GameRng::new(TEST_SEED));
    world.insert_resource(MatchState::default());

    let mut settings = PlayerAiSettings::default();
    settings.set_version(p1, AiVersion::V3);
    settings.set_version(p2, AiVersion::V1);
    world.insert_resource(settings);
    for player in &mut world.resource_mut::<Players>().0 {
        player.funds = 0;
    }

    world.spawn((
        GridPosition { x: 0, y: 1 },
        // 乗降地形はPort、勝敗判定上は自軍Capitalとして扱う。
        Property::new(Terrain::Capital, Some(p1), 100),
    ));
    world.spawn((
        GridPosition { x: 4, y: 0 },
        Property::new(Terrain::Capital, Some(p2), 200),
    ));

    let capture = spawn_test_unit(
        &mut world,
        p1,
        GridPosition { x: 0, y: 1 },
        UnitStats {
            unit_type: UnitType::Infantry,
            movement_type: MovementType::Infantry,
            max_movement: 3,
            max_fuel: 99,
            max_ammo1: 9,
            min_range: 1,
            max_range: 1,
            can_capture: true,
            cost: 1000,
            ..UnitStats::mock()
        },
    );
    let combat = spawn_test_unit(
        &mut world,
        p1,
        GridPosition { x: 0, y: 1 },
        UnitStats {
            unit_type: UnitType::Tank,
            movement_type: MovementType::Tank,
            max_movement: 6,
            max_fuel: 70,
            max_ammo1: 9,
            min_range: 1,
            max_range: 1,
            cost: 7000,
            ..UnitStats::mock()
        },
    );
    let transport = world
        .spawn((
            p1,
            Faction(p1),
            GridPosition { x: 0, y: 1 },
            UnitStats {
                unit_type: UnitType::Lander,
                movement_type: MovementType::Ship,
                max_movement: 6,
                max_fuel: 99,
                max_cargo: 2,
                loadable_unit_types: vec![UnitType::Infantry, UnitType::Tank],
                ..UnitStats::mock()
            },
            CargoCapacity {
                max: 2,
                loaded: Vec::new(),
            },
            HasMoved(false),
            ActionCompleted(false),
            Health {
                current: 100,
                max: 100,
            },
            Fuel {
                current: 99,
                max: 99,
            },
            Ammo {
                ammo1: 0,
                max_ammo1: 0,
                ammo2: 0,
                max_ammo2: 0,
            },
        ))
        .id();
    spawn_test_unit(
        &mut world,
        p2,
        GridPosition { x: 4, y: 2 },
        UnitStats {
            unit_type: UnitType::Tank,
            movement_type: MovementType::Tank,
            max_movement: 6,
            max_fuel: 70,
            max_ammo1: 9,
            min_range: 1,
            max_range: 1,
            cost: 7000,
            ..UnitStats::mock()
        },
    );

    let mut loaded_cursor = world.resource::<Events<UnitLoadedEvent>>().get_cursor();
    let mut unloaded_cursor = world.resource::<Events<UnitUnloadedEvent>>().get_cursor();
    let mut attacked_cursor = world.resource::<Events<UnitAttackedEvent>>().get_cursor();
    let mut capture_cursor = world
        .resource::<Events<PropertyCaptureProgressedEvent>>()
        .get_cursor();
    let cargo_entities = HashSet::from([capture, combat]);
    let mut loaded_cargo = HashSet::new();
    let mut unloaded_cargo = HashSet::new();
    let mut invasion_started = false;
    let mut return_without_cargo = false;

    'rounds: for _ in 0..MAX_ROUNDS {
        for _ in 0..2 {
            let active_player = active_player(&world);
            for _ in 0..MAX_ACTIONS_PER_PHASE {
                let action = crate::ai::engine::execute_ai_turn(&mut world, active_player);
                schedule.run(&mut world);

                for event in loaded_cursor.read(world.resource::<Events<UnitLoadedEvent>>()) {
                    if event.transport == transport && cargo_entities.contains(&event.cargo) {
                        loaded_cargo.insert(event.cargo);
                    }
                }
                for event in unloaded_cursor.read(world.resource::<Events<UnitUnloadedEvent>>()) {
                    if event.transport == transport && cargo_entities.contains(&event.cargo) {
                        let island = world
                            .resource::<crate::ai::islands::IslandMap>()
                            .get_island_at(&GridPosition {
                                x: event.target_x,
                                y: event.target_y,
                            })
                            .expect("unload tile should belong to an island")
                            .id;
                        assert_eq!(island, enemy_island);
                        unloaded_cargo.insert(event.cargo);
                    }
                }
                for event in attacked_cursor.read(world.resource::<Events<UnitAttackedEvent>>()) {
                    if unloaded_cargo.contains(&event.attacker)
                        || unloaded_cargo.contains(&event.defender)
                    {
                        invasion_started = true;
                    }
                }
                for event in
                    capture_cursor.read(world.resource::<Events<PropertyCaptureProgressedEvent>>())
                {
                    if unloaded_cargo.contains(&event.unit) {
                        assert_eq!((event.x, event.y), (4, 0));
                        invasion_started = true;
                    }
                }

                if let Some(squad) = world
                    .get_resource::<crate::ai::squad::SquadManager>()
                    .and_then(|manager| {
                        manager.squads.iter().find(|squad| {
                            squad.transport_entity == Some(transport)
                                && squad.mission_type == crate::ai::squad::MissionType::Transport
                        })
                    })
                    .filter(|squad| {
                        squad.phase
                            == crate::ai::squad::MissionPhase::Transport(
                                crate::ai::squad::TransportPhase::Return,
                            )
                    })
                {
                    assert!(squad.cargo_entities.is_empty());
                    assert!(
                        world
                            .get::<CargoCapacity>(transport)
                            .expect("transport should retain cargo component")
                            .loaded
                            .is_empty()
                    );
                    return_without_cargo = true;
                }

                if loaded_cargo == cargo_entities
                    && unloaded_cargo == cargo_entities
                    && invasion_started
                    && return_without_cargo
                {
                    break 'rounds;
                }
                if action.is_none() {
                    break;
                }
            }
        }
    }

    assert_eq!(loaded_cargo, cargo_entities, "both cargo units must load");
    assert_eq!(unloaded_cargo, cargo_entities, "both cargo units must land");
    assert!(invasion_started, "landed cargo must fight or start capture");
    assert!(
        return_without_cargo,
        "transport must enter Return only after unloading all cargo"
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

fn active_player(world: &World) -> PlayerId {
    let state = world.resource::<MatchState>();
    world.resource::<Players>().0[state.active_player_index.0].id
}
