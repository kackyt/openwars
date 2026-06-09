#![allow(clippy::needless_range_loop)]
#![allow(clippy::unnecessary_cast)]

use crate::components::{Faction, GridPosition, PlayerId, UnitStats};
use bevy_ecs::prelude::*;
use std::collections::HashSet;

/// 敵ユニットのクラスター。
/// 2ターン以内に相互支援できるユニット群。
#[derive(Debug, Clone)]
pub struct AttackCluster {
    pub units: HashSet<Entity>,
    pub threat_level: u32,
    pub total_value: u32,
    pub center: GridPosition,
}

/// 盤面から敵ユニットのクラスターを検出します。
pub fn detect_enemy_clusters(
    world: &mut World,
    perspective_player: PlayerId,
) -> Vec<AttackCluster> {
    let mut enemy_units = Vec::new();

    let mut query = world.query::<(Entity, &Faction, &GridPosition, &UnitStats)>();
    for (entity, faction, pos, stats) in query.iter(world) {
        if faction.0 != perspective_player {
            enemy_units.push((entity, *pos, stats.clone(), faction.0));
        }
    }

    let mut unit_positions = std::collections::HashMap::new();
    let mut q_all_units = world.query::<(&Faction, &GridPosition, &UnitStats)>();
    for (faction, pos, stats) in q_all_units.iter(world) {
        unit_positions.insert(
            (pos.x, pos.y),
            crate::systems::movement::OccupantInfo {
                player_id: faction.0,
                is_transport: false,
                unit_type: stats.unit_type,
                loadable_types: vec![],
                free_slots: 0,
            },
        );
    }

    let mut clusters = Vec::new();
    let mut visited = HashSet::new();

    let map = world.resource::<crate::resources::Map>();
    let registry = world
        .get_resource::<crate::resources::master_data::MasterDataRegistry>()
        .unwrap();
    let mut turn_cache = crate::ai::turn_distance::TurnDistanceCache::default();

    for i in 0..enemy_units.len() {
        if visited.contains(&enemy_units[i].0) {
            continue;
        }

        let mut cluster_units = HashSet::new();
        let mut total_threat = 0;
        let mut total_value = 0;
        let mut sum_x = 0;
        let mut sum_y = 0;

        let mut queue = vec![enemy_units[i].clone()];
        visited.insert(enemy_units[i].0);

        while let Some((e, pos, stats, enemy_player_id)) = queue.pop() {
            cluster_units.insert(e);
            total_value += stats.cost;
            total_threat += stats.cost / 1000;
            sum_x += pos.x;
            sum_y += pos.y;

            for j in 0..enemy_units.len() {
                if !visited.contains(&enemy_units[j].0) {
                    let other_pos = enemy_units[j].1;
                    let other_stats = &enemy_units[j].2;
                    let other_player_id = enemy_units[j].3;

                    let turn_dist = crate::ai::turn_distance::calculate_turn_distance(
                        map,
                        registry,
                        &unit_positions,
                        (pos.x, pos.y),
                        (other_pos.x, other_pos.y),
                        stats.movement_type,
                        stats.max_movement,
                        1,
                        enemy_player_id,
                        &mut turn_cache,
                    );

                    let turn_dist_rev = crate::ai::turn_distance::calculate_turn_distance(
                        map,
                        registry,
                        &unit_positions,
                        (other_pos.x, other_pos.y),
                        (pos.x, pos.y),
                        other_stats.movement_type,
                        other_stats.max_movement,
                        1,
                        other_player_id,
                        &mut turn_cache,
                    );

                    if turn_dist.turns <= 2 && turn_dist_rev.turns <= 2 {
                        visited.insert(enemy_units[j].0);
                        queue.push(enemy_units[j].clone());
                    }
                }
            }
        }

        let center = GridPosition {
            x: sum_x / cluster_units.len() as usize,
            y: sum_y / cluster_units.len() as usize,
        };

        clusters.push(AttackCluster {
            units: cluster_units,
            threat_level: total_threat,
            total_value,
            center,
        });
    }

    clusters
}

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
        world
    }

    #[test]
    fn test_detect_enemy_clusters_single_group() {
        let mut world = setup_test_world();
        let p1 = PlayerId(1); // Perspective player
        let p2 = PlayerId(2); // Enemy

        // Spawn 3 enemy infantry close to each other
        for i in 0..3 {
            world.spawn((
                p2,
                Faction(p2),
                GridPosition { x: 5 + i, y: 5 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    cost: 1000,
                    ..UnitStats::mock()
                },
            ));
        }

        let clusters = detect_enemy_clusters(&mut world, p1);

        assert_eq!(clusters.len(), 1, "Should detect exactly 1 cluster");
        assert_eq!(clusters[0].units.len(), 3, "Cluster should contain 3 units");
        assert_eq!(clusters[0].total_value, 3000, "Total value should be 3000");
        assert_eq!(clusters[0].threat_level, 3, "Threat level should be 3");
        assert_eq!(
            clusters[0].center,
            GridPosition { x: 6, y: 5 },
            "Center should be average of (5,5), (6,5), (7,5)"
        );
    }

    #[test]
    fn test_detect_enemy_clusters_multiple_groups() {
        let mut world = setup_test_world();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // Group 1: 2 units at (1,1) and (1,2)
        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 1, y: 1 },
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                cost: 1000,
                ..UnitStats::mock()
            },
        ));
        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 1, y: 2 },
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                cost: 1000,
                ..UnitStats::mock()
            },
        ));

        // Group 2: 1 unit far away at (8,8)
        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 8, y: 8 },
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                max_movement: 6,
                cost: 7000,
                ..UnitStats::mock()
            },
        ));

        // Also add a friendly unit (should be ignored)
        world.spawn((
            p1,
            Faction(p1),
            GridPosition { x: 5, y: 5 },
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                ..UnitStats::mock()
            },
        ));

        let mut clusters = detect_enemy_clusters(&mut world, p1);

        // Sort clusters by center X to make assertions deterministic
        clusters.sort_by_key(|c| c.center.x);

        assert_eq!(clusters.len(), 2, "Should detect 2 distinct clusters");

        assert_eq!(clusters[0].units.len(), 2, "Group 1 should have 2 units");
        assert_eq!(
            clusters[0].center,
            GridPosition { x: 1, y: 1 },
            "Group 1 center (integer div: (1+1)/2, (1+2)/2)"
        );
        assert_eq!(clusters[0].total_value, 2000);

        assert_eq!(clusters[1].units.len(), 1, "Group 2 should have 1 unit");
        assert_eq!(
            clusters[1].center,
            GridPosition { x: 8, y: 8 },
            "Group 2 center"
        );
        assert_eq!(clusters[1].total_value, 7000);
    }
}
