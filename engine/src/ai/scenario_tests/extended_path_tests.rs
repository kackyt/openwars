//! 拡張経路（小規模マップ戦略）で実行する複数ターンのシナリオ回帰テスト。

use crate::ai::engine::*;
use crate::components::*;
use crate::resources::master_data::*;
use crate::resources::*;
use bevy_ecs::prelude::*;

#[test]
fn test_v3_multi_cargo_invasion_loads_transits_and_lands() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let master_data = MasterDataRegistry::load().unwrap();
    let (mut world, mut schedule) =
        crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
    let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
    for entity in entities {
        world.despawn(entity);
    }

    let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
    map.set_terrain(0, 1, Terrain::Port).unwrap();
    map.set_terrain(3, 1, Terrain::Shoal).unwrap();
    for position in [(3, 0), (4, 0), (4, 1), (4, 2)] {
        map.set_terrain(position.0, position.1, Terrain::Plains)
            .unwrap();
    }
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let target_island = island_map
        .get_island_at(&GridPosition { x: 4, y: 0 })
        .unwrap()
        .id;
    world.insert_resource(map);
    world.insert_resource(island_map);

    let p1 = PlayerId(1);
    let p2 = PlayerId(2);
    world.spawn((
        GridPosition { x: 0, y: 1 },
        Property::new(Terrain::Capital, Some(p1), 100),
    ));
    world.spawn((
        GridPosition { x: 4, y: 0 },
        Property::new(Terrain::Capital, Some(p2), 100),
    ));
    world.spawn((
        Faction(p2),
        GridPosition { x: 4, y: 2 },
        UnitStats {
            unit_type: UnitType::Tank,
            movement_type: MovementType::Tank,
            max_movement: 6,
            ..UnitStats::mock()
        },
        Health {
            current: 100,
            max: 100,
        },
    ));

    let capture = world
        .spawn((
            p1,
            Faction(p1),
            GridPosition { x: 0, y: 1 },
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                can_capture: true,
                cost: 1000,
                ..UnitStats::mock()
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
        ))
        .id();
    let combat = world
        .spawn((
            p1,
            Faction(p1),
            GridPosition { x: 0, y: 1 },
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                max_movement: 6,
                cost: 7000,
                ..UnitStats::mock()
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
        ))
        .id();
    let transport = world
        .spawn((
            p1,
            Faction(p1),
            GridPosition { x: 0, y: 1 },
            UnitStats {
                unit_type: UnitType::Lander,
                movement_type: MovementType::Ship,
                max_movement: 6,
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
        ))
        .id();

    let mut manager = crate::ai::squad::SquadManager::new();
    let squad = manager.create_squad(crate::ai::squad::MissionType::Transport);
    squad.members.insert(transport);
    squad.transport_entity = Some(transport);
    squad.cargo_entities = vec![capture, combat];
    squad.pickup_position = Some(GridPosition { x: 0, y: 1 });
    squad.target_island = Some(target_island);
    squad.target = Some(GridPosition { x: 4, y: 0 });
    squad.phase =
        crate::ai::squad::MissionPhase::Transport(crate::ai::squad::TransportPhase::Pickup);
    world.insert_resource(manager);

    let execute_transport = |world: &mut World, schedule: &mut Schedule| {
        let mut manager = world
            .remove_resource::<crate::ai::squad::SquadManager>()
            .unwrap();
        let squad = manager
            .squads
            .iter_mut()
            .find(|squad| squad.mission_type == crate::ai::squad::MissionType::Transport)
            .unwrap();
        let (entity, command) = crate::ai::squad::execute_transport_squad_step(
            world,
            squad,
            &std::collections::HashSet::new(),
        )
        .expect("transport should produce a command");
        let result = command.clone();
        world.insert_resource(manager);
        crate::ai::engine::execute_ai_command(world, entity, command);
        schedule.run(world);
        result
    };
    let reset_actions = |world: &mut World| {
        let mut query = world.query::<(&mut ActionCompleted, &mut HasMoved)>();
        for (mut action, mut moved) in query.iter_mut(world) {
            action.0 = false;
            moved.0 = false;
        }
    };

    assert!(matches!(
        execute_transport(&mut world, &mut schedule),
        AiCommand::Load { .. }
    ));
    crate::ai::squad::update_squads(&mut world, p1);
    assert_eq!(
        world.get::<CargoCapacity>(transport).unwrap().loaded.len(),
        1
    );
    reset_actions(&mut world);
    assert!(matches!(
        execute_transport(&mut world, &mut schedule),
        AiCommand::Load { .. }
    ));
    crate::ai::squad::update_squads(&mut world, p1);
    assert_eq!(
        world.get::<CargoCapacity>(transport).unwrap().loaded.len(),
        2
    );
    assert_eq!(
        world
            .resource::<crate::ai::squad::SquadManager>()
            .squads
            .iter()
            .find(|squad| squad.mission_type == crate::ai::squad::MissionType::Transport)
            .unwrap()
            .phase,
        crate::ai::squad::MissionPhase::Transport(crate::ai::squad::TransportPhase::Transit)
    );

    reset_actions(&mut world);
    assert!(matches!(
        execute_transport(&mut world, &mut schedule),
        AiCommand::Drop { .. }
    ));
    crate::ai::squad::update_squads(&mut world, p1);
    assert_eq!(
        world.get::<CargoCapacity>(transport).unwrap().loaded.len(),
        1
    );
    assert!(
        world
            .resource::<crate::ai::squad::SquadManager>()
            .squads
            .iter()
            .any(|squad| squad.mission_type == crate::ai::squad::MissionType::Capture)
    );

    reset_actions(&mut world);
    assert!(matches!(
        execute_transport(&mut world, &mut schedule),
        AiCommand::Drop { .. }
    ));
    crate::ai::squad::update_squads(&mut world, p1);
    let manager = world.resource::<crate::ai::squad::SquadManager>();
    let transport_squad = manager
        .squads
        .iter()
        .find(|squad| squad.mission_type == crate::ai::squad::MissionType::Transport)
        .unwrap();
    assert_eq!(
        transport_squad.phase,
        crate::ai::squad::MissionPhase::Transport(crate::ai::squad::TransportPhase::Return)
    );
    assert!(transport_squad.cargo_entities.is_empty());
    assert!(
        manager
            .squads
            .iter()
            .any(|squad| squad.mission_type == crate::ai::squad::MissionType::Attack)
    );
    assert!(world.get::<GridPosition>(capture).unwrap().x >= 3);
    assert!(world.get::<GridPosition>(combat).unwrap().x >= 3);
}
