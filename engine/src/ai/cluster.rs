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

    let map = world.resource::<crate::resources::Map>().clone();
    let registry = world
        .get_resource::<crate::resources::master_data::MasterDataRegistry>()
        .cloned()
        .unwrap_or_default();
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

                    let turn_dist = crate::ai::turn_distance::calculate_turn_distance(
                        &map,
                        &registry,
                        &unit_positions,
                        (pos.x, pos.y),
                        (other_pos.x, other_pos.y),
                        stats.movement_type,
                        stats.max_movement,
                        enemy_player_id,
                        &mut turn_cache,
                    );

                    if turn_dist <= 2 {
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
