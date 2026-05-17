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
                    if let Some(island) =
                        island_map.islands.iter().find(|i| i.id == target_island_id)
                    {
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
                .is_some();

            if loaded || transporting {
                mission.phase = TransportPhase::Transit;
            }
        }
        TransportPhase::Transit => {
            // ヘリが target_island のいずれかのタイルと隣接している、あるいはその島にいる（距離1以下）
            if let Some(target_island_id) = mission.target_island {
                if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() {
                    if let Some(island) =
                        island_map.islands.iter().find(|i| i.id == target_island_id)
                    {
                        if let Some(t_pos) =
                            world.get::<GridPosition>(mission.transport_entity).cloned()
                        {
                            let is_adjacent = island.tiles.iter().any(|tile| {
                                (tile.x as i32 - t_pos.x as i32).abs()
                                    + (tile.y as i32 - t_pos.y as i32).abs()
                                    <= 1
                            });
                            if is_adjacent {
                                mission.phase = TransportPhase::Drop;
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
                .is_some();

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

        let cmd = execute_mission_step(&mut world, &mission);
        assert!(cmd.is_some());
        if let Some(crate::ai::engine::AiCommand::Wait { target_pos }) = cmd {
            // ヘリは (0,0) にいて、歩兵は (2,0) にいる。ヘリは (2,0) に隣接するマス（(1,0), (3,0), (2,1) など）へ向かうべき
            let dist = (target_pos.x as i32 - 2).abs() + (target_pos.y as i32 - 0).abs();
            assert_eq!(dist, 1);
        } else {
            panic!("Expected Wait command, got {:?}", cmd);
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

        let cmd = execute_mission_step(&mut world, &mission);
        assert!(cmd.is_some());
        if let Some(crate::ai::engine::AiCommand::Wait { target_pos }) = cmd {
            // (0,0) から (5,5) へ移動可能な最大範囲内で最も近い場所へ向かうはず
            // ヘリの最大移動力は 5 なので、(5,0) などが選ばれるはず（マンハッタン距離で最も近いところ）
            let dist_to_target = (target_pos.x as i32 - 5).abs() + (target_pos.y as i32 - 5).abs();
            assert!(dist_to_target < 10); // 最初(10)より近づいているはず
        } else {
            panic!("Expected Wait command, got {:?}", cmd);
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

        let cmd = execute_mission_step(&mut world, &mission);
        assert!(cmd.is_some());
        if let Some(crate::ai::engine::AiCommand::Drop {
            target_pos,
            cargo_entity,
        }) = cmd
        {
            assert_eq!(cargo_entity, cargo);
            // 降車先は隣接するタイル（通常は平地 (5,5) 等）
            // get_droppable_tiles は隣接する平地に降車可能かを判定する
            // transportの周囲のタイルのうち平地の箇所に降車する
            let dist = (target_pos.x as i32 - 5).abs() + (target_pos.y as i32 - 4).abs();
            assert_eq!(dist, 1);
        } else {
            panic!("Expected Drop command, got {:?}", cmd);
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

        let cmd = execute_mission_step(&mut world, &mission);
        assert!(cmd.is_some());
        if let Some(crate::ai::engine::AiCommand::Wait { target_pos }) = cmd {
            // 降車不可時の Wait フォールバック。現在位置で待機する
            assert_eq!(target_pos.x, 0);
            assert_eq!(target_pos.y, 0);
        } else {
            panic!("Expected Wait command, got {:?}", cmd);
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

        let cmd = execute_mission_step(&mut world, &mission);
        assert!(cmd.is_some());
        if let Some(crate::ai::engine::AiCommand::Wait { target_pos }) = cmd {
            // 拠点 (0,0) に最も近づく方向へ移動するはず
            let dist_to_base = target_pos.x as i32 + target_pos.y as i32;
            assert!(dist_to_base < 10); // 最初(10)より近づいているはず
        } else {
            panic!("Expected Wait command, got {:?}", cmd);
        }
    }
}
