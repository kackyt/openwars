use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;

pub fn produce_unit_system(
    mut commands: Commands,
    mut produce_events: EventReader<ProduceUnitCommand>,
    mut players: ResMut<Players>,
    match_state: Res<MatchState>,
    map: Res<Map>,
    q_properties: Query<(&GridPosition, &Property)>,
    unit_registry: Res<UnitRegistry>,
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Production {
        return;
    }
    let active_player_id = players.0[match_state.active_player_index].id;

    for event in produce_events.read() {
        if event.player_id != active_player_id {
            continue;
        }

        let mut is_valid_property = false;
        let mut owned_capitals = Vec::new();

        for (pos, prop) in q_properties.iter() {
            if prop.owner_id == Some(event.player_id) {
                if pos.x == event.target_x && pos.y == event.target_y {
                    if prop.terrain == Terrain::City || prop.terrain == Terrain::Airport {
                        is_valid_property = true;
                    }
                }
                if prop.terrain == Terrain::Capital {
                    owned_capitals.push((pos.x, pos.y));
                }
            }
        }

        let mut has_nearby_capital = false;
        for (cx, cy) in owned_capitals {
            if let Some(distance) = map.distance(event.target_x, event.target_y, cx, cy) {
                if distance <= 3 {
                    has_nearby_capital = true;
                    break;
                }
            }
        }

        if !is_valid_property || !has_nearby_capital {
            continue;
        }

        let canonical_stats = match unit_registry.units.get(&event.stats.unit_type) {
            Some(s) => s,
            None => continue,
        };

        let player = players
            .0
            .iter_mut()
            .find(|p| p.id == event.player_id)
            .unwrap();
        if player.funds < canonical_stats.cost {
            continue; // Insufficient funds
        }

        player.funds -= canonical_stats.cost;

        commands.spawn((
            GridPosition {
                x: event.target_x,
                y: event.target_y,
            },
            Faction(event.player_id),
            Health {
                current: 100,
                max: 100,
            },
            Fuel {
                current: canonical_stats.max_fuel,
                max: canonical_stats.max_fuel,
            },
            Ammo {
                ammo1: canonical_stats.max_ammo1,
                max_ammo1: canonical_stats.max_ammo1,
                ammo2: canonical_stats.max_ammo2,
                max_ammo2: canonical_stats.max_ammo2,
            },
            canonical_stats.clone(),
            HasMoved(true), // Produced units cannot move immediately
            ActionCompleted(true),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_produce_unit_system() {
        let mut world = World::new();

        world.insert_resource(MatchState::default());
        world.insert_resource(Players(vec![
            Player {
                id: 1,
                name: "P1".to_string(),
                funds: 2000,
            },
            Player {
                id: 2,
                name: "P2".to_string(),
                funds: 0,
            },
        ]));

        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(2, 0, Terrain::City).unwrap();
        world.insert_resource(map);

        world.insert_resource(Events::<ProduceUnitCommand>::default());

        // Spawn properties
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(1)),
        ));
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(Terrain::City, Some(1)),
        ));

        let stats = UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1000,
            max_movement: 3,
            movement_type: MovementType::Foot,
            max_fuel: 99,
            max_ammo1: 0,
            max_ammo2: 0,
            min_range: 1,
            max_range: 1,
            daily_fuel_consumption: 0,
            can_capture: true,
            can_supply: false,
            max_cargo: 0,
            loadable_unit_types: vec![],
        };

        let mut unit_registry = UnitRegistry::default();
        unit_registry
            .units
            .insert(UnitType::Infantry, stats.clone());
        world.insert_resource(unit_registry);

        world.send_event(ProduceUnitCommand {
            player_id: 1,
            target_x: 2,
            target_y: 0,
            stats: stats.clone(),
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(produce_unit_system);
        schedule.run(&mut world);

        // Check if unit was spawned
        let mut query = world.query::<(&Faction, &UnitStats, &GridPosition)>();
        let mut iter = query.iter(&world);
        let (faction, spawned_stats, pos) = iter.next().expect("Unit should have been spawned");
        assert_eq!(faction.0, 1);
        assert_eq!(pos.x, 2);
        assert_eq!(pos.y, 0);
        assert_eq!(spawned_stats.unit_type, UnitType::Infantry);

        // Check if funds were deducted
        let players = world.resource::<Players>();
        assert_eq!(players.0[0].funds, 1000); // 2000 - 1000
    }
}
