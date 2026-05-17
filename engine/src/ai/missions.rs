use crate::ai::islands::IslandId;
use crate::components::{Faction, GridPosition, Property, UnitStats};
use crate::resources::Map;
use crate::resources::master_data::MasterDataRegistry;
use crate::systems::movement::{OccupantInfo, calculate_reachable_tiles};
use bevy_ecs::prelude::*;
use std::collections::HashMap;

/// 輸送ミッションの各フェーズ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportPhase {
    Pickup,  // 歩兵の回収に向かうフェーズ
    Transit, // 目標の島へ海上を移動するフェーズ
    Drop,    // 目標の島に歩兵を降ろすフェーズ
    Return,  // 任務完了後、拠点に帰還するフェーズ
}

/// 輸送ミッションの情報
#[derive(Debug, Clone, Copy)]
pub struct TransportMission {
    pub transport_entity: Entity,
    pub cargo_entity: Entity,
    pub phase: TransportPhase,
    pub target_island: Option<IslandId>,
}

#[derive(Resource, Default)]
pub struct TransportMissionManager {
    pub missions: Vec<TransportMission>,
}

pub fn execute_mission_step(
    world: &mut World,
    mission: &TransportMission,
) -> Option<super::engine::AiCommand> {
    // 輸送機の基本情報を取得
    let (t_pos, t_stats, t_fuel, t_faction) = {
        let t_pos = world
            .get::<GridPosition>(mission.transport_entity)
            .cloned()?;
        let t_stats = world.get::<UnitStats>(mission.transport_entity).cloned()?;
        let t_fuel = world
            .get::<crate::components::Fuel>(mission.transport_entity)
            .map(|f| f.current)?;
        let t_faction = world.get::<Faction>(mission.transport_entity).cloned()?;
        (t_pos, t_stats, t_fuel, t_faction.0)
    };


    // 経路探索のために他ユニットの占有情報を取得
    let mut unit_positions = HashMap::new();
    {
        let mut query = world.query::<(
            Entity,
            &GridPosition,
            &Faction,
            &UnitStats,
            Option<&crate::components::CargoCapacity>,
            Option<&crate::components::Transporting>,
        )>();
        for (_e, pos, faction, stats, cargo_opt, transporting_opt) in query.iter(world) {
            if transporting_opt.is_some() {
                continue;
            }
            let free_slots = cargo_opt
                .map(|c| c.max.saturating_sub(c.loaded.len() as u32))
                .unwrap_or(0);
            unit_positions.insert(
                (pos.x, pos.y),
                OccupantInfo {
                    player_id: faction.0,
                    is_transport: stats.max_cargo > 0,
                    unit_type: stats.unit_type,
                    loadable_types: stats.loadable_unit_types.clone(),
                    free_slots,
                },
            );
        }
    }

    let map = world.resource::<Map>();
    let registry = world.resource::<MasterDataRegistry>();

    let reachable = calculate_reachable_tiles(
        map,
        &unit_positions,
        (t_pos.x, t_pos.y),
        t_stats.movement_type,
        t_stats.max_movement,
        t_fuel,
        t_faction,
        t_stats.unit_type,
        registry,
    );

    match mission.phase {
        TransportPhase::Pickup => {
            let cargo_pos = world.get::<GridPosition>(mission.cargo_entity).cloned()?;

            // 対象の歩兵の現在位置に最も近いタイルへ移動して待機する
            let mut best_tile = t_pos;
            let mut min_dist = (t_pos.x as i32 - cargo_pos.x as i32).abs()
                + (t_pos.y as i32 - cargo_pos.y as i32).abs();

            for target_tile in &reachable {
                let dist = (target_tile.0 as i32 - cargo_pos.x as i32).abs()
                    + (target_tile.1 as i32 - cargo_pos.y as i32).abs();
                if dist < min_dist {
                    min_dist = dist;
                    best_tile = GridPosition {
                        x: target_tile.0,
                        y: target_tile.1,
                    };
                }
            }
            return Some(super::engine::AiCommand::Wait {
                target_pos: best_tile,
            });
        }
        TransportPhase::Transit => {
            if let Some(target_island_id) = mission.target_island {
                if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() {
                    if let Some(island) = island_map.islands.iter().find(|i| i.id == target_island_id) {
                        if let Some(target_pos) = island.tiles.iter().next() {
                            let mut best_tile = t_pos;
                            let mut min_dist = (t_pos.x as i32 - target_pos.x as i32).abs()
                                + (t_pos.y as i32 - target_pos.y as i32).abs();

                            for target_tile in &reachable {
                                let dist = (target_tile.0 as i32 - target_pos.x as i32).abs()
                                    + (target_tile.1 as i32 - target_pos.y as i32).abs();
                                if dist < min_dist {
                                    min_dist = dist;
                                    best_tile = GridPosition {
                                        x: target_tile.0,
                                        y: target_tile.1,
                                    };
                                }
                            }
                            return Some(super::engine::AiCommand::Wait {
                                target_pos: best_tile,
                            });
                        }
                    }
                }
            }
        }
        TransportPhase::Drop => {
            let drop_tiles = crate::systems::transport::get_droppable_tiles(
                world,
                mission.transport_entity,
                mission.cargo_entity,
            );
            if let Some(drop_tile) = drop_tiles.first() {
                return Some(super::engine::AiCommand::Drop {
                    target_pos: GridPosition {
                        x: drop_tile.0,
                        y: drop_tile.1,
                    },
                    cargo_entity: mission.cargo_entity,
                });
            } else {
                // 降ろせる場所がない場合は、待機して機を伺う
                return Some(super::engine::AiCommand::Wait { target_pos: t_pos });
            }
        }
        TransportPhase::Return => {
            let mut nearest_prop_pos = t_pos;
            let mut min_dist = 9999;
            let mut query = world.query::<(&GridPosition, &Property)>();
            for (pos, prop) in query.iter(world) {
                if prop.owner_id == Some(t_faction) {
                    let dist = (pos.x as i32 - t_pos.x as i32).abs()
                        + (pos.y as i32 - t_pos.y as i32).abs();
                    if dist < min_dist {
                        min_dist = dist;
                        nearest_prop_pos = *pos;
                    }
                }
            }

            let mut best_tile = t_pos;
            let mut best_dist = (t_pos.x as i32 - nearest_prop_pos.x as i32).abs()
                + (t_pos.y as i32 - nearest_prop_pos.y as i32).abs();

            for target_tile in &reachable {
                let dist = (target_tile.0 as i32 - nearest_prop_pos.x as i32).abs()
                    + (target_tile.1 as i32 - nearest_prop_pos.y as i32).abs();
                if dist < best_dist {
                    best_dist = dist;
                    best_tile = GridPosition {
                        x: target_tile.0,
                        y: target_tile.1,
                    };
                }
            }
            return Some(super::engine::AiCommand::Wait {
                target_pos: best_tile,
            });
        }
    }
    None
}
