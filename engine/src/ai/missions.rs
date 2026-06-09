#![allow(clippy::collapsible_if)]

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

/// 輸送ユニットの移動タイプに応じて、目標島に対する最適な目的地タイルを返します。
/// 航空ユニットの場合は、島の中で最も現在地に最も近いタイルを直接ターゲットにします。
/// 船舶ユニット（Landerなど）の場合は、島内の陸地タイルに隣接し、かつ船舶が進入可能な海・港タイルの中から
/// 最も現在地に最も近いものをターゲット（接岸可能タイル）として返します。
fn get_target_position_for_island(
    map: &Map,
    registry: &MasterDataRegistry,
    island: &crate::ai::islands::Island,
    t_pos: GridPosition,
    movement_type: crate::resources::MovementType,
) -> Option<GridPosition> {
    if movement_type == crate::resources::MovementType::Ship {
        let mut best_dock_tile = None;
        let mut min_dist = 9999;

        for tile in &island.tiles {
            // 隣接4マスを取得
            for (ax, ay) in map.get_adjacent(tile.x, tile.y) {
                if let Some(terrain) = map.get_terrain(ax, ay) {
                    // 船舶が進入可能（移動コスト < 99）かチェック
                    if crate::systems::movement::get_valid_movement_cost(
                        registry,
                        movement_type,
                        terrain,
                    )
                    .is_some()
                    {
                        let dist =
                            (ax as i32 - t_pos.x as i32).abs() + (ay as i32 - t_pos.y as i32).abs();
                        if dist < min_dist {
                            min_dist = dist;
                            best_dock_tile = Some(GridPosition { x: ax, y: ay });
                        }
                    }
                }
            }
        }
        best_dock_tile
    } else {
        island
            .tiles
            .iter()
            .min_by_key(|p| {
                (
                    (p.x as i32 - t_pos.x as i32).abs() + (p.y as i32 - t_pos.y as i32).abs(),
                    p.x,
                    p.y,
                )
            })
            .cloned()
    }
}

pub fn execute_mission_step(
    world: &mut World,
    mission: &TransportMission,
) -> Vec<(Entity, super::engine::AiCommand)> {
    // 輸送機の基本情報を取得
    let t_pos = if let Some(p) = world.get::<GridPosition>(mission.transport_entity).cloned() {
        p
    } else {
        return vec![];
    };
    let t_stats = if let Some(s) = world.get::<UnitStats>(mission.transport_entity).cloned() {
        s
    } else {
        return vec![];
    };
    let t_fuel = if let Some(f) = world.get::<crate::components::Fuel>(mission.transport_entity) {
        f.current
    } else {
        return vec![];
    };
    let t_faction = if let Some(f) = world.get::<Faction>(mission.transport_entity).cloned() {
        f.0
    } else {
        return vec![];
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

    let reachable = {
        let map = world.resource::<Map>();
        let registry = world.resource::<MasterDataRegistry>();
        calculate_reachable_tiles(
            map,
            &unit_positions,
            (t_pos.x, t_pos.y),
            t_stats.movement_type,
            t_stats.max_movement,
            t_fuel,
            t_faction,
            t_stats.unit_type,
            registry,
        )
    };

    match mission.phase {
        TransportPhase::Pickup => {
            let cargo_pos =
                if let Some(p) = world.get::<GridPosition>(mission.cargo_entity).cloned() {
                    p
                } else {
                    return vec![];
                };

            // Cargo (Infantry) が到達可能な島のタイルセットを取得
            let mut cargo_island_tiles = None;
            if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() {
                for island in &island_map.islands {
                    if island.tiles.contains(&cargo_pos) {
                        cargo_island_tiles = Some(island.tiles.clone());
                        break;
                    }
                }
            }

            let map = world.resource::<Map>();
            let registry = world.resource::<MasterDataRegistry>();

            let mut best_meetup_tile = cargo_pos;
            if t_stats.movement_type == crate::resources::MovementType::Ship {
                // Lander (Ship): Find the best dock tile for the island
                let mut best_dock_tile = None;
                let mut min_dist = 9999;

                // cargo_island_tiles is optional, so we use it if available
                if let Some(tiles) = &cargo_island_tiles {
                    for tile in tiles {
                        for (ax, ay) in map.get_adjacent(tile.x, tile.y) {
                            if let Some(terrain) = map.get_terrain(ax, ay) {
                                if crate::systems::movement::get_valid_movement_cost(
                                    registry,
                                    t_stats.movement_type,
                                    terrain,
                                )
                                .is_some()
                                {
                                    let dist = (ax as i32 - t_pos.x as i32).abs()
                                        + (ay as i32 - t_pos.y as i32).abs();
                                    if dist < min_dist {
                                        min_dist = dist;
                                        best_dock_tile = Some(GridPosition { x: ax, y: ay });
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some(dock_tile) = best_dock_tile {
                    best_meetup_tile = dock_tile;
                }
            } else {
                // Helicopter (Air): Find a tile both can enter
                let mut valid_meetup_tiles = vec![];
                for x in 0..map.width {
                    for y in 0..map.height {
                        let pos = GridPosition { x, y };
                        if let Some(tiles) = &cargo_island_tiles {
                            if !tiles.contains(&pos) {
                                continue;
                            }
                        }
                        if let Some(terrain) = map.get_terrain(x, y) {
                            if let Some(c_stats) = world.get::<UnitStats>(mission.cargo_entity) {
                                let t_can_enter =
                                    crate::systems::movement::get_valid_movement_cost(
                                        registry,
                                        t_stats.movement_type,
                                        terrain,
                                    )
                                    .is_some();
                                let c_can_enter =
                                    crate::systems::movement::get_valid_movement_cost(
                                        registry,
                                        c_stats.movement_type,
                                        terrain,
                                    )
                                    .is_some();
                                if t_can_enter && c_can_enter {
                                    valid_meetup_tiles.push(pos);
                                }
                            }
                        }
                    }
                }

                best_meetup_tile = valid_meetup_tiles
                    .iter()
                    .min_by_key(|&p| {
                        let t_d = (t_pos.x as i32 - p.x as i32).abs()
                            + (t_pos.y as i32 - p.y as i32).abs();
                        let c_d = (cargo_pos.x as i32 - p.x as i32).abs()
                            + (cargo_pos.y as i32 - p.y as i32).abs();
                        (t_d + c_d, (t_d - c_d).abs())
                    })
                    .cloned()
                    .unwrap_or(cargo_pos);
            }

            let dist_t_to_meetup = (t_pos.x as i32 - best_meetup_tile.x as i32).abs()
                + (t_pos.y as i32 - best_meetup_tile.y as i32).abs();
            let _dist_c_to_meetup = (cargo_pos.x as i32 - best_meetup_tile.x as i32).abs()
                + (cargo_pos.y as i32 - best_meetup_tile.y as i32).abs();

            let mut cmds = vec![];

            let t_exhausted = world
                .get::<crate::components::ActionCompleted>(mission.transport_entity)
                .map(|a| a.0)
                .unwrap_or(false);
            let c_exhausted = world
                .get::<crate::components::ActionCompleted>(mission.cargo_entity)
                .map(|a| a.0)
                .unwrap_or(false);

            // 1. 輸送機の処理
            if !t_exhausted {
                let mut best_t_tile = t_pos;
                let mut turn_cache = crate::ai::turn_distance::TurnDistanceCache::default();
                let mut min_t_dist = crate::ai::turn_distance::calculate_turn_distance(
                    map,
                    registry,
                    &unit_positions,
                    (t_pos.x, t_pos.y),
                    (best_meetup_tile.x, best_meetup_tile.y),
                    t_stats.movement_type,
                    t_stats.max_movement,
                    0,
                    t_faction,
                    &mut turn_cache,
                );
                for target_tile in &reachable {
                    let d = crate::ai::turn_distance::calculate_turn_distance(
                        map,
                        registry,
                        &unit_positions,
                        (target_tile.0, target_tile.1),
                        (best_meetup_tile.x, best_meetup_tile.y),
                        t_stats.movement_type,
                        t_stats.max_movement,
                        0,
                        t_faction,
                        &mut turn_cache,
                    );
                    if d < min_t_dist {
                        min_t_dist = d;
                        best_t_tile = GridPosition {
                            x: target_tile.0,
                            y: target_tile.1,
                        };
                    }
                }
                cmds.push((
                    mission.transport_entity,
                    super::engine::AiCommand::Wait {
                        target_pos: best_t_tile,
                    },
                ));
            }

            // 2. 歩兵の処理
            if !c_exhausted {
                let mut can_load = false;
                if dist_t_to_meetup == 0 {
                    // 輸送機が合流地点にいる場合、乗り込めるかチェック
                    if let Some(c_stats) = world.get::<UnitStats>(mission.cargo_entity) {
                        let c_fuel = world
                            .get::<crate::components::Fuel>(mission.cargo_entity)
                            .map(|f| f.current)
                            .unwrap_or(99);
                        let c_reachable = calculate_reachable_tiles(
                            map,
                            &unit_positions,
                            (cargo_pos.x, cargo_pos.y),
                            c_stats.movement_type,
                            c_stats.max_movement,
                            c_fuel,
                            t_faction,
                            c_stats.unit_type,
                            registry,
                        );
                        let dist = (cargo_pos.x as i32 - t_pos.x as i32).abs()
                            + (cargo_pos.y as i32 - t_pos.y as i32).abs();
                        if c_reachable.contains(&(t_pos.x, t_pos.y))
                            || cargo_pos == t_pos
                            || dist <= 1
                        {
                            can_load = true;
                        }
                    }
                }

                if can_load {
                    cmds.push((
                        mission.cargo_entity,
                        super::engine::AiCommand::Load {
                            transport_entity: mission.transport_entity,
                            target_pos: t_pos,
                        },
                    ));
                } else {
                    // 乗り込めない場合は近づく
                    if let Some(c_stats) = world.get::<UnitStats>(mission.cargo_entity) {
                        let c_fuel = world
                            .get::<crate::components::Fuel>(mission.cargo_entity)
                            .map(|f| f.current)
                            .unwrap_or(99);
                        let c_reachable = calculate_reachable_tiles(
                            map,
                            &unit_positions,
                            (cargo_pos.x, cargo_pos.y),
                            c_stats.movement_type,
                            c_stats.max_movement,
                            c_fuel,
                            t_faction,
                            c_stats.unit_type,
                            registry,
                        );
                        let mut best_c_tile = cargo_pos;
                        // 合流地点（best_meetup_tile）または 輸送機（t_pos）に近づく
                        let target = if dist_t_to_meetup == 0 {
                            t_pos
                        } else {
                            best_meetup_tile
                        };
                        let mut turn_cache = crate::ai::turn_distance::TurnDistanceCache::default();
                        let mut min_dist = crate::ai::turn_distance::calculate_turn_distance(
                            map,
                            registry,
                            &unit_positions,
                            (cargo_pos.x, cargo_pos.y),
                            (target.x, target.y),
                            c_stats.movement_type,
                            c_stats.max_movement,
                            0,
                            t_faction,
                            &mut turn_cache,
                        );

                        for target_tile in &c_reachable {
                            let d = crate::ai::turn_distance::calculate_turn_distance(
                                map,
                                registry,
                                &unit_positions,
                                (target_tile.0, target_tile.1),
                                (target.x, target.y),
                                c_stats.movement_type,
                                c_stats.max_movement,
                                0,
                                t_faction,
                                &mut turn_cache,
                            );
                            if d < min_dist {
                                min_dist = d;
                                best_c_tile = GridPosition {
                                    x: target_tile.0,
                                    y: target_tile.1,
                                };
                            }
                        }
                        cmds.push((
                            mission.cargo_entity,
                            super::engine::AiCommand::Wait {
                                target_pos: best_c_tile,
                            },
                        ));
                    }
                }
            }

            if cmds.is_empty() {
                cmds.push((
                    mission.transport_entity,
                    super::engine::AiCommand::Wait { target_pos: t_pos },
                ));
            }
            return cmds;
        }
        TransportPhase::Transit => {
            if let Some(target_island_id) = mission.target_island {
                // 借用チェッカーを回避するため、必要なデータをスコープ内で事前にコピーまたはクローンします。
                let (island_tiles, target_pos) = {
                    if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>()
                    {
                        if let Some(island) =
                            island_map.islands.iter().find(|i| i.id == target_island_id)
                        {
                            let map = world.resource::<Map>();
                            let registry = world.resource::<MasterDataRegistry>();
                            let target_pos = get_target_position_for_island(
                                map,
                                registry,
                                island,
                                t_pos,
                                t_stats.movement_type,
                            );
                            (Some(island.tiles.clone()), target_pos)
                        } else {
                            (None, None)
                        }
                    } else {
                        (None, None)
                    }
                };

                if let (Some(island_tiles), Some(target_pos)) = (island_tiles, target_pos) {
                    // ここからは world を安全に借用できます。
                    let mut best_drop_tile_pair = None;
                    let mut min_drop_dist = 9999;

                    for &(rx, ry) in &reachable {
                        let test_pos = GridPosition { x: rx, y: ry };
                        let drop_targets = crate::systems::transport::get_droppable_tiles_at(
                            world,
                            mission.transport_entity,
                            mission.cargo_entity,
                            test_pos,
                        );
                        for drop_target in drop_targets {
                            let drop_pos = GridPosition {
                                x: drop_target.0,
                                y: drop_target.1,
                            };
                            // 降車先が目標島に含まれているかチェック
                            if island_tiles.contains(&drop_pos) {
                                let dist = (rx as i32 - t_pos.x as i32).abs()
                                    + (ry as i32 - t_pos.y as i32).abs();
                                if dist < min_drop_dist {
                                    min_drop_dist = dist;
                                    best_drop_tile_pair = Some((test_pos, drop_pos));
                                }
                            }
                        }
                    }

                    // 降車可能な場所が見つかった場合は、移動と降車を同一ターンで行う AiCommand::Drop を即座に発行する
                    if let Some((trans_pos, drop_pos)) = best_drop_tile_pair {
                        return vec![(
                            mission.transport_entity,
                            super::engine::AiCommand::Drop {
                                transport_target_pos: trans_pos,
                                cargo_drop_pos: drop_pos,
                                cargo_entity: mission.cargo_entity,
                            },
                        )];
                    }

                    // 降車可能な場所が見つからない場合は、従来通り目標島に最も近づく
                    let mut best_tile = t_pos;
                    let mut min_dist = u32::MAX;
                    let map = world.resource::<Map>();
                    let registry = world.resource::<MasterDataRegistry>();
                    let mut turn_cache = crate::ai::turn_distance::TurnDistanceCache::default();

                    for target_tile in &reachable {
                        let dist = crate::ai::turn_distance::calculate_turn_distance(
                            map,
                            registry,
                            &unit_positions,
                            (target_tile.0, target_tile.1),
                            (target_pos.x, target_pos.y),
                            t_stats.movement_type,
                            t_stats.max_movement,
                            0,
                            t_faction,
                            &mut turn_cache,
                        );
                        if dist.turns < min_dist {
                            min_dist = dist.turns;
                            best_tile = GridPosition {
                                x: target_tile.0,
                                y: target_tile.1,
                            };
                        }
                    }
                    return vec![(
                        mission.transport_entity,
                        super::engine::AiCommand::Wait {
                            target_pos: best_tile,
                        },
                    )];
                }
            }
        }
        TransportPhase::Drop => {
            // 移動せず現在地から降車できる場合
            let drop_tiles = crate::systems::transport::get_droppable_tiles(
                world,
                mission.transport_entity,
                mission.cargo_entity,
            );
            if let Some(drop_tile) = drop_tiles.first() {
                return vec![(
                    mission.transport_entity,
                    super::engine::AiCommand::Drop {
                        transport_target_pos: t_pos,
                        cargo_drop_pos: GridPosition {
                            x: drop_tile.0,
                            y: drop_tile.1,
                        },
                        cargo_entity: mission.cargo_entity,
                    },
                )];
            } else {
                // 現在位置から降ろせない場合、降車可能な移動先を探す
                let mut best_drop_tile_pair = None;
                let mut min_drop_dist = 9999;

                for &(rx, ry) in &reachable {
                    let test_pos = GridPosition { x: rx, y: ry };
                    let drop_targets = crate::systems::transport::get_droppable_tiles_at(
                        world,
                        mission.transport_entity,
                        mission.cargo_entity,
                        test_pos,
                    );
                    if let Some(drop_target) = drop_targets.first() {
                        let dist =
                            (rx as i32 - t_pos.x as i32).abs() + (ry as i32 - t_pos.y as i32).abs();
                        // 可能な限り移動距離が少ない（または目標に近い）ものを選ぶが、ここでは距離優先
                        if dist < min_drop_dist {
                            min_drop_dist = dist;
                            best_drop_tile_pair = Some((
                                test_pos,
                                GridPosition {
                                    x: drop_target.0,
                                    y: drop_target.1,
                                },
                            ));
                        }
                    }
                }

                if let Some((trans_pos, drop_pos)) = best_drop_tile_pair {
                    return vec![(
                        mission.transport_entity,
                        super::engine::AiCommand::Drop {
                            transport_target_pos: trans_pos,
                            cargo_drop_pos: drop_pos,
                            cargo_entity: mission.cargo_entity,
                        },
                    )];
                }

                // それでも降ろせる場所が見つからない場合は、目標島に向かってとりあえず近づく
                if let Some(target_island_id) = mission.target_island {
                    if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>()
                    {
                        if let Some(island) =
                            island_map.islands.iter().find(|i| i.id == target_island_id)
                        {
                            let map = world.resource::<Map>();
                            let registry = world.resource::<MasterDataRegistry>();
                            if let Some(target_pos) = get_target_position_for_island(
                                map,
                                registry,
                                island,
                                t_pos,
                                t_stats.movement_type,
                            ) {
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
                                return vec![(
                                    mission.transport_entity,
                                    super::engine::AiCommand::Wait {
                                        target_pos: best_tile,
                                    },
                                )];
                            }
                        }
                    }
                }
                // フォールバック：現在位置で待機
                return vec![(
                    mission.transport_entity,
                    super::engine::AiCommand::Wait { target_pos: t_pos },
                )];
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
            return vec![(
                mission.transport_entity,
                super::engine::AiCommand::Wait {
                    target_pos: best_tile,
                },
            )];
        }
    }
    vec![]
}

/// 輸送ミッションのフェーズ遷移と完了判定を行う
/// ミッションが完了（削除すべき）になった場合は true を返す
pub fn update_mission_phase(world: &mut World, mission: &mut TransportMission) -> bool {
    // 輸送機が存在しない場合はミッション削除
    if world
        .get::<GridPosition>(mission.transport_entity)
        .is_none()
    {
        return true;
    }

    // Return以外のフェーズで、cargo_entityが存在しない場合はミッション削除
    if mission.phase != TransportPhase::Return
        && world.get::<GridPosition>(mission.cargo_entity).is_none()
    {
        return true;
    }

    match mission.phase {
        TransportPhase::Pickup => {
            // cargo_entity が transport_entity に積載されているかチェック
            let loaded = if let Some(cargo) =
                world.get::<crate::components::CargoCapacity>(mission.transport_entity)
            {
                cargo.loaded.contains(&mission.cargo_entity)
            } else {
                false
            };
            let transporting = world
                .get::<crate::components::Transporting>(mission.cargo_entity)
                .is_some_and(|t| t.0 == mission.transport_entity);

            if loaded || transporting {
                mission.phase = TransportPhase::Transit;
            }
        }
        TransportPhase::Transit => {
            // 移動＋降車が同一ターンで完了した場合に備え、積載状態と輸送中フラグをチェック
            let loaded = if let Some(cargo) =
                world.get::<crate::components::CargoCapacity>(mission.transport_entity)
            {
                cargo.loaded.contains(&mission.cargo_entity)
            } else {
                false
            };
            let transporting = world
                .get::<crate::components::Transporting>(mission.cargo_entity)
                .is_some_and(|t| t.0 == mission.transport_entity);

            if !loaded && !transporting {
                // すでに歩兵がヘリに積載されておらず輸送中でない場合、降下完了したとみなし Return へ直接遷移する
                mission.phase = TransportPhase::Return;
            } else {
                // ヘリが target_island のいずれかのタイルと隣接している、あるいはその島にいる（距離1以下）
                if let Some(target_island_id) = mission.target_island {
                    if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>()
                    {
                        if let Some(island) =
                            island_map.islands.iter().find(|i| i.id == target_island_id)
                        {
                            if let Some(t_pos) =
                                world.get::<GridPosition>(mission.transport_entity).cloned()
                            {
                                let map = world.resource::<Map>();
                                let registry = world.resource::<MasterDataRegistry>();
                                let t_stats =
                                    world.get::<UnitStats>(mission.transport_entity).unwrap();
                                if let Some(target_pos) = get_target_position_for_island(
                                    map,
                                    registry,
                                    island,
                                    t_pos,
                                    t_stats.movement_type,
                                ) {
                                    if (target_pos.x as i32 - t_pos.x as i32).abs()
                                        + (target_pos.y as i32 - t_pos.y as i32).abs()
                                        <= 1
                                    {
                                        mission.phase = TransportPhase::Drop;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        TransportPhase::Drop => {
            // 歩兵がすでにヘリから降ろされたか
            let loaded = if let Some(cargo) =
                world.get::<crate::components::CargoCapacity>(mission.transport_entity)
            {
                cargo.loaded.contains(&mission.cargo_entity)
            } else {
                false
            };
            let transporting = world
                .get::<crate::components::Transporting>(mission.cargo_entity)
                .is_some_and(|t| t.0 == mission.transport_entity);

            if !loaded && !transporting {
                mission.phase = TransportPhase::Return;
            }
        }
        TransportPhase::Return => {
            // 自軍のいずれかの拠点（都市、首都、空港など）に到達したか
            if let Some(t_pos) = world.get::<GridPosition>(mission.transport_entity).cloned() {
                if let Some(t_faction) = world.get::<Faction>(mission.transport_entity).map(|f| f.0)
                {
                    let mut query = world.query::<(&GridPosition, &Property)>();
                    let at_base = query.iter(world).any(|(pos, prop)| {
                        pos.x == t_pos.x && pos.y == t_pos.y && prop.owner_id == Some(t_faction)
                    });
                    if at_base {
                        return true; // 完了。削除。
                    }
                }
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::islands::{Island, IslandMap};
    use crate::components::{CargoCapacity, Fuel, Health, PlayerId, Transporting};
    use crate::resources::{GridTopology, Terrain};

    fn setup_test_world() -> (World, Entity, Entity) {
        let mut world = World::new();

        // 必須リソースのロード
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: GridTopology::Square,
        });

        MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);

        let transport = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: crate::resources::UnitType::TransportHelicopter,
                    max_movement: 5,
                    movement_type: crate::resources::MovementType::Air,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        let cargo = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 1, y: 0 },
                UnitStats {
                    unit_type: crate::resources::UnitType::Infantry,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        (world, transport, cargo)
    }

    #[test]
    fn test_update_mission_phase_transitions() {
        let (mut world, transport, cargo) = setup_test_world();
        let island_map = IslandMap {
            islands: vec![
                Island {
                    id: IslandId(0),
                    tiles: vec![GridPosition { x: 0, y: 0 }, GridPosition { x: 1, y: 0 }]
                        .into_iter()
                        .collect(),
                },
                Island {
                    id: IslandId(1),
                    tiles: vec![GridPosition { x: 5, y: 5 }].into_iter().collect(),
                },
            ],
        };
        world.insert_resource(island_map);

        let mut mission = TransportMission {
            transport_entity: transport,
            cargo_entity: cargo,
            phase: TransportPhase::Pickup,
            target_island: Some(IslandId(1)),
        };

        // 最初は Pickup
        assert_eq!(mission.phase, TransportPhase::Pickup);
        assert!(!update_mission_phase(&mut world, &mut mission));

        // ロードされると Transit へ移行するはず
        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .push(cargo);
        world.entity_mut(cargo).insert(Transporting(transport));
        assert!(!update_mission_phase(&mut world, &mut mission));
        assert_eq!(mission.phase, TransportPhase::Transit);

        // 目標の島に隣接（または到達）すると Drop へ移行するはず
        // ターゲット島は (5,5) なので、ヘリを (5,4) に移動させる（距離1）
        *world.get_mut::<GridPosition>(transport).unwrap() = GridPosition { x: 5, y: 4 };
        assert!(!update_mission_phase(&mut world, &mut mission));
        assert_eq!(mission.phase, TransportPhase::Drop);

        // 降車すると Return へ移行するはず
        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .clear();
        world.entity_mut(cargo).remove::<Transporting>();
        assert!(!update_mission_phase(&mut world, &mut mission));
        assert_eq!(mission.phase, TransportPhase::Return);

        // 自軍の拠点に到達するとミッション完了（trueが返る）になるはず
        // (0,0) に自軍の都市を配置する
        let p1 = PlayerId(1);
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::City, Some(p1), 200),
        ));
        // ヘリを (0,0) に戻す
        *world.get_mut::<GridPosition>(transport).unwrap() = GridPosition { x: 0, y: 0 };
        assert!(update_mission_phase(&mut world, &mut mission));
    }

    #[test]
    fn test_execute_mission_step_pickup() {
        let (mut world, transport, cargo) = setup_test_world();
        // 歩兵の位置を (2,0) に移動させる（ヘリは (0,0) にいて、(2,0) に近づくために (1,0) へ向かうはず）
        *world.get_mut::<GridPosition>(cargo).unwrap() = GridPosition { x: 2, y: 0 };

        let mission = TransportMission {
            transport_entity: transport,
            cargo_entity: cargo,
            phase: TransportPhase::Pickup,
            target_island: Some(IslandId(1)),
        };

        let cmds = execute_mission_step(&mut world, &mission);
        assert!(!cmds.is_empty());
        if let Some((_entity, crate::ai::engine::AiCommand::Wait { target_pos })) =
            cmds.into_iter().next()
        {
            // ヘリは (0,0) にいて、歩兵は (2,0) にいる。ヘリは (2,0) に隣接するマス（(1,0), (3,0), (2,1) など）へ向かうべき
            let dist = (target_pos.x as i32 - 2).abs() + (target_pos.y as i32).abs();
            assert_eq!(dist, 1);
        } else {
            panic!("Expected Wait command, got empty");
        }
    }

    #[test]
    fn test_execute_mission_step_transit() {
        let (mut world, transport, cargo) = setup_test_world();
        let island_map = IslandMap {
            islands: vec![Island {
                id: IslandId(1),
                tiles: vec![GridPosition { x: 5, y: 5 }].into_iter().collect(),
            }],
        };
        world.insert_resource(island_map);

        let mission = TransportMission {
            transport_entity: transport,
            cargo_entity: cargo,
            phase: TransportPhase::Transit,
            target_island: Some(IslandId(1)),
        };

        let cmds = execute_mission_step(&mut world, &mission);
        assert!(!cmds.is_empty());
        if let Some((_entity, crate::ai::engine::AiCommand::Wait { target_pos })) =
            cmds.into_iter().next()
        {
            // (0,0) から (5,5) へ移動可能な最大範囲内で最も近い場所へ向かうはず
            // ヘリの最大移動力は 5 なので、(5,0) などが選ばれるはず（マンハッタン距離で最も近いところ）
            let dist_to_target = (target_pos.x as i32 - 5).abs() + (target_pos.y as i32 - 5).abs();
            assert!(dist_to_target < 10); // 最初(10)より近づいているはず
        } else {
            panic!("Expected Wait command, got empty");
        }
    }

    #[test]
    fn test_execute_mission_step_drop_success() {
        let (mut world, transport, cargo) = setup_test_world();
        // 降車テストのため、歩兵をロード状態にする
        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .push(cargo);
        world.entity_mut(cargo).insert(Transporting(transport));
        // ヘリを (5,4) に置き、(5,5) の陸地に隣接させる
        *world.get_mut::<GridPosition>(transport).unwrap() = GridPosition { x: 5, y: 4 };

        let mission = TransportMission {
            transport_entity: transport,
            cargo_entity: cargo,
            phase: TransportPhase::Drop,
            target_island: Some(IslandId(1)),
        };

        let cmds = execute_mission_step(&mut world, &mission);
        assert!(!cmds.is_empty());
        if let Some((
            _entity,
            crate::ai::engine::AiCommand::Drop {
                transport_target_pos,
                cargo_drop_pos,
                cargo_entity,
            },
        )) = cmds.into_iter().next()
        {
            assert_eq!(cargo_entity, cargo);
            assert_eq!(transport_target_pos, GridPosition { x: 5, y: 4 });
            let dist = (cargo_drop_pos.x as i32 - 5).abs() + (cargo_drop_pos.y as i32 - 4).abs();
            assert_eq!(dist, 1);
        } else {
            panic!("Expected Drop command, got empty");
        }
    }

    #[test]
    fn test_execute_mission_step_drop_fallback() {
        let (mut world, transport, cargo) = setup_test_world();
        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .push(cargo);
        world.entity_mut(cargo).insert(Transporting(transport));

        // 降車テストのため、ヘリの周囲をすべて海にし、ヘリが降車可能なタイルがない状態にする
        // マップをすべて海にする
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Sea; 100],
            topology: GridTopology::Square,
        });

        let mission = TransportMission {
            transport_entity: transport,
            cargo_entity: cargo,
            phase: TransportPhase::Drop,
            target_island: Some(IslandId(1)),
        };

        let cmds = execute_mission_step(&mut world, &mission);
        assert!(!cmds.is_empty());
        if let Some((_entity, crate::ai::engine::AiCommand::Wait { target_pos })) =
            cmds.into_iter().next()
        {
            // 降車不可時の Wait フォールバック。現在位置で待機する
            assert_eq!(target_pos.x, 0);
            assert_eq!(target_pos.y, 0);
        } else {
            panic!("Expected Wait command, got empty");
        }
    }

    #[test]
    fn test_execute_mission_step_return() {
        let (mut world, transport, cargo) = setup_test_world();
        let p1 = PlayerId(1);

        // 拠点 (0,0)
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::City, Some(p1), 200),
        ));

        // ヘリは遠く (5,5) にいる
        *world.get_mut::<GridPosition>(transport).unwrap() = GridPosition { x: 5, y: 5 };

        let mission = TransportMission {
            transport_entity: transport,
            cargo_entity: cargo,
            phase: TransportPhase::Return,
            target_island: Some(IslandId(1)),
        };

        let cmds = execute_mission_step(&mut world, &mission);
        assert!(!cmds.is_empty());
        if let Some((_entity, crate::ai::engine::AiCommand::Wait { target_pos })) =
            cmds.into_iter().next()
        {
            // 拠点 (0,0) に最も近づく方向へ移動するはず
            let dist_to_base = target_pos.x as i32 + target_pos.y as i32;
            assert!(dist_to_base < 10); // 最初(10)より近づいているはず
        } else {
            panic!("Expected Wait command, got empty");
        }
    }

    #[test]
    fn test_execute_mission_step_transit_lander() {
        let mut world = World::new();

        // 船舶ユニットなので、陸(Plains等)には侵入できず、海(Sea)しか進めないマップを作る
        // (0,0)-(2,0)は海で、(3,0)は平地(陸地)
        let mut map = Map::new(5, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(3, 0, Terrain::Plains).unwrap();
        map.set_terrain(4, 0, Terrain::Plains).unwrap();
        world.insert_resource(map);

        MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);

        // Lander（船）を (0,0) に配置
        let transport = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: crate::resources::UnitType::Lander,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Ship,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
                CargoCapacity {
                    max: 2,
                    loaded: vec![],
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        // 積載する歩兵
        let cargo = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 9999, y: 9999 }, // マップ外
                UnitStats {
                    unit_type: crate::resources::UnitType::Infantry,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        // ターゲットの島は (3,0) と (4,0)
        let island_map = IslandMap {
            islands: vec![Island {
                id: IslandId(1),
                tiles: vec![GridPosition { x: 3, y: 0 }, GridPosition { x: 4, y: 0 }]
                    .into_iter()
                    .collect(),
            }],
        };
        world.insert_resource(island_map);

        let mission = TransportMission {
            transport_entity: transport,
            cargo_entity: cargo,
            phase: TransportPhase::Transit,
            target_island: Some(IslandId(1)),
        };

        let cmds = execute_mission_step(&mut world, &mission);
        assert!(!cmds.is_empty());
        if let Some((_entity, crate::ai::engine::AiCommand::Wait { target_pos })) =
            cmds.into_iter().next()
        {
            // 目標島内の陸地 (3,0) 自体ではなく、隣接する海マスである (2,0) をターゲットにし、
            // 最大移動力 3 の範囲内（(0,0)から(2,0)は距離2）で到達可能なため、(2,0) が移動先となるべき
            assert_eq!(target_pos.x, 2);
            assert_eq!(target_pos.y, 0);
        } else {
            panic!("Expected Wait command, got empty");
        }
    }

    #[test]
    fn test_execute_mission_step_transit_to_direct_drop() {
        let (mut world, transport, cargo) = setup_test_world();
        // 降車テストのため、歩兵をロード状態にする
        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .push(cargo);
        world.entity_mut(cargo).insert(Transporting(transport));

        // ターゲットの島は (5,5)
        let island_map = IslandMap {
            islands: vec![Island {
                id: IslandId(1),
                tiles: vec![GridPosition { x: 5, y: 5 }].into_iter().collect(),
            }],
        };
        world.insert_resource(island_map);

        // ヘリを (0,0) に配置
        *world.get_mut::<GridPosition>(transport).unwrap() = GridPosition { x: 0, y: 0 };
        // ヘリの最大移動力を 10 に設定して確実に (5,4) に移動できるようにする
        world.get_mut::<UnitStats>(transport).unwrap().max_movement = 10;

        let mut mission = TransportMission {
            transport_entity: transport,
            cargo_entity: cargo,
            phase: TransportPhase::Transit,
            target_island: Some(IslandId(1)),
        };

        // execute_mission_step を実行すると、Wait ではなく Drop コマンドが返るはず！
        let cmds = execute_mission_step(&mut world, &mission);
        assert!(!cmds.is_empty());
        if let Some((
            _entity,
            crate::ai::engine::AiCommand::Drop {
                transport_target_pos,
                cargo_drop_pos,
                cargo_entity,
            },
        )) = cmds.clone().into_iter().next()
        {
            assert_eq!(cargo_entity, cargo);
            assert!(
                transport_target_pos == GridPosition { x: 5, y: 4 }
                    || transport_target_pos == GridPosition { x: 4, y: 5 },
                "Expected transport_target_pos to be (5,4) or (4,5), got {:?}",
                transport_target_pos
            );
            assert_eq!(cargo_drop_pos, GridPosition { x: 5, y: 5 });
        } else {
            panic!(
                "Expected Drop command (due to direct drop from transit), got {:?}",
                cmds
            );
        }

        // 移動＋降車が実行されて cargo がヘリから降ろされた状態にする
        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .clear();
        world.entity_mut(cargo).remove::<Transporting>();

        // この状態で update_mission_phase を呼び出すと、Transit から Return へ直接遷移するはず
        assert!(!update_mission_phase(&mut world, &mut mission));
        assert_eq!(mission.phase, TransportPhase::Return);
    }
}
