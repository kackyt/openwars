//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::*;
    use crate::resources::master_data::*;
    use crate::resources::*;

    fn setup_test_world() -> World {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }

        let map = Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: GridTopology::Square,
        };

        world.insert_resource(map);
        world.insert_resource(crate::ai::ai_version::PlayerAiSettings::default());
        world
    }

    #[test]
    fn beam_search_does_not_redirect_transport_invasion() {
        let mut world = setup_test_world();
        let p1 = PlayerId(1);
        let map = world.resource::<Map>().clone();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map
            .get_island_at(&GridPosition { x: 8, y: 8 })
            .unwrap()
            .id;
        world.insert_resource(island_map);
        world.spawn((
            GridPosition { x: 1, y: 1 },
            crate::components::Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            GridPosition { x: 8, y: 8 },
            crate::components::Property::new(Terrain::City, Some(PlayerId(2)), 100),
        ));
        let transport = world
            .spawn((
                Faction(p1),
                GridPosition { x: 2, y: 2 },
                UnitStats {
                    unit_type: UnitType::Lander,
                    movement_type: MovementType::Ship,
                    max_movement: 6,
                    max_cargo: 2,
                    ..UnitStats::mock()
                },
            ))
            .id();

        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.target = Some(GridPosition { x: 8, y: 8 });
        squad.target_island = Some(island_id);
        squad.phase =
            crate::ai::squad::MissionPhase::Transport(crate::ai::squad::TransportPhase::Transit);
        world.insert_resource(manager);

        run_squad_beam_search(&mut world, p1);
        let manager = world.resource::<SquadManager>();
        assert_eq!(manager.squads[0].target, Some(GridPosition { x: 8, y: 8 }));
        assert_eq!(manager.squads[0].target_island, Some(island_id));
    }
}
