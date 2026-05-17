use crate::ai::islands::IslandMap;
use crate::ai::missions::{TransportMission, TransportMissionManager, TransportPhase};
use crate::ai::objectives::Objective;
use crate::components::{CargoCapacity, Faction, GridPosition, PlayerId, Property, UnitStats};
use crate::resources::UnitType;
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};

/// 戦略目標に基づいて輸送ミッションを割り当てる
/// 1. 現在の全拠点を取得し、島ごとに分類
/// 2. 目標（Target Islands）の優先度を評価し、最も高い目標を選択
/// 3. 必要な歩兵数と輸送機数を算出し、フリーなユニットから割り当てる
pub fn assign_transport_missions(world: &mut World, player_id: PlayerId) {
    // 進行中の自軍ミッションに割り当てられているユニットを収集
    let mut busy_transports = HashSet::new();
    let mut busy_infantry = HashSet::new();
    let mut missions_to_add = Vec::new();

    if let Some(manager) = world.get_resource::<TransportMissionManager>() {
        for m in &manager.missions {
            #[allow(clippy::collapsible_if)]
            if let Some(faction) = world.get::<Faction>(m.transport_entity) {
                if faction.0 == player_id {
                    busy_transports.insert(m.transport_entity);
                    busy_infantry.insert(m.cargo_entity);
                }
            }
        }
    }

    // 1. 全拠点情報を収集
    let mut properties_map = HashMap::new();
    {
        let mut query = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in query.iter(world) {
            properties_map.insert(*pos, *prop);
        }
    }

    // 島情報の取得と分類
    let (base_islands, target_islands) = {
        if let Some(island_map) = world.get_resource::<IslandMap>() {
            island_map.classify_islands(player_id, &properties_map)
        } else {
            return;
        }
    };

    if target_islands.is_empty() {
        return; // 目標がない
    }

    // 2. 目標の優先度評価
    let island_map = world.get_resource::<IslandMap>().unwrap().clone();
    let mut objectives = Vec::new();

    // 簡易的な前線距離計算のため、自軍拠点の平均座標（重心）を求める
    let mut base_center_x = 0;
    let mut base_center_y = 0;
    let mut base_count = 0;
    for base_id in &base_islands {
        if let Some(island) = island_map.islands.iter().find(|i| i.id == *base_id) {
            for tile in &island.tiles {
                #[allow(clippy::collapsible_if)]
                if let Some(prop) = properties_map.get(tile) {
                    if prop.owner_id == Some(player_id) {
                        base_center_x += tile.x;
                        base_center_y += tile.y;
                        base_count += 1;
                    }
                }
            }
        }
    }

    if base_count > 0 {
        base_center_x /= base_count;
        base_center_y /= base_count;
    }

    for target_id in target_islands {
        if let Some(island) = island_map.islands.iter().find(|i| i.id == target_id) {
            let mut island_props = Vec::new();
            let mut target_center_x = 0;
            let mut target_center_y = 0;
            let mut target_prop_count = 0;

            for tile in &island.tiles {
                #[allow(clippy::collapsible_if)]
                if let Some(prop) = properties_map.get(tile) {
                    if prop.owner_id != Some(player_id) {
                        island_props.push((*tile, prop.terrain));
                        target_center_x += tile.x;
                        target_center_y += tile.y;
                        target_prop_count += 1;
                    }
                }
            }

            if target_prop_count > 0 {
                target_center_x /= target_prop_count;
                target_center_y /= target_prop_count;
            }

            // 自軍重心からのマンハッタン距離をペナルティとして計算（遠いほど優先度減）
            let distance_penalty = (target_center_x as i32 - base_center_x as i32).abs()
                + (target_center_y as i32 - base_center_y as i32).abs();

            let objective = Objective::evaluate(target_id, &island_props, distance_penalty);
            objectives.push(objective);
        }
    }

    // スコア降順でソート
    objectives.sort_by(|a, b| b.priority_score.cmp(&a.priority_score));

    // 3. フリーなユニットの収集
    let mut free_transports = Vec::new();
    {
        let mut query =
            world.query::<(Entity, &Faction, &UnitStats, &CargoCapacity, &GridPosition)>();
        for (entity, faction, stats, cargo, pos) in query.iter(world) {
            if faction.0 == player_id
                && (stats.unit_type == UnitType::TransportHelicopter
                    || stats.unit_type == UnitType::Lander)
                && cargo.loaded.is_empty()
                && !busy_transports.contains(&entity)
            {
                free_transports.push((entity, *pos));
            }
        }
    }

    let mut free_infantry = Vec::new();
    {
        let mut query = world.query::<(
            Entity,
            &Faction,
            &UnitStats,
            &GridPosition,
            Option<&crate::components::Transporting>,
        )>();
        for (entity, faction, stats, pos, transporting_opt) in query.iter(world) {
            if faction.0 == player_id
                && (stats.unit_type == UnitType::Infantry || stats.unit_type == UnitType::Mech)
                && transporting_opt.is_none()
                && !busy_infantry.contains(&entity)
            {
                // 歩兵が「自軍の島」にいるか確認（すでに別の目標島で戦闘中の場合は再割り当てしない）
                #[allow(clippy::collapsible_if)]
                if let Some(island) = island_map.get_island_at(pos) {
                    if base_islands.contains(&island.id) {
                        free_infantry.push((entity, *pos));
                    }
                }
            }
        }
    }

    // 目標に対して割り当てを行う
    for objective in objectives {
        // 必要な部隊数
        let needed = objective.needed_infantry;

        // 現在割り当て中のミッション（同じTarget Islandへ向かっているもの）をカウント
        let mut current_assigned = 0;
        if let Some(manager) = world.get_resource::<TransportMissionManager>() {
            for m in &manager.missions {
                #[allow(clippy::collapsible_if)]
                if let Some(faction) = world.get::<Faction>(m.transport_entity) {
                    if faction.0 == player_id && m.target_island == Some(objective.target_island) {
                        current_assigned += 1;
                    }
                }
            }
        }

        let mut to_assign = needed.saturating_sub(current_assigned);

        while to_assign > 0 && !free_transports.is_empty() && !free_infantry.is_empty() {
            let (transport_entity, _) = free_transports.pop().unwrap();
            let (cargo_entity, _) = free_infantry.pop().unwrap();

            missions_to_add.push(TransportMission {
                transport_entity,
                cargo_entity,
                phase: TransportPhase::Pickup,
                target_island: Some(objective.target_island),
            });

            to_assign -= 1;
        }

        // ユニットが尽きたら終了
        if free_transports.is_empty() || free_infantry.is_empty() {
            break;
        }
    }

    // 新規ミッションの登録
    if !missions_to_add.is_empty() {
        if let Some(mut manager) = world.get_resource_mut::<TransportMissionManager>() {
            manager.missions.extend(missions_to_add);
        } else {
            world.insert_resource(TransportMissionManager {
                missions: missions_to_add,
            });
        }
    }
}

pub fn assign_test_transport_mission(world: &mut World, player_id: PlayerId) {
    assign_transport_missions(world, player_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::islands::{Island, IslandId, IslandMap};
    use crate::components::{Faction, GridPosition, PlayerId, Property, UnitStats};
    use crate::resources::{Terrain, UnitType};

    use std::collections::HashSet;

    #[test]
    fn test_assign_transport_mission() {
        let mut world = World::new();
        let p1 = PlayerId(1);

        world.insert_resource(TransportMissionManager::default());

        let mut island1_tiles = HashSet::new();
        island1_tiles.insert(GridPosition { x: 0, y: 0 });
        let mut island2_tiles = HashSet::new();
        island2_tiles.insert(GridPosition { x: 10, y: 10 });

        let island_map = IslandMap {
            islands: vec![
                Island {
                    id: IslandId(0),
                    tiles: island1_tiles,
                },
                Island {
                    id: IslandId(1),
                    tiles: island2_tiles,
                },
            ],
        };
        world.insert_resource(island_map);

        // Target Islandとして認識させるため、敵(p2)の拠点を配置
        let p2 = PlayerId(2);
        world.spawn((
            GridPosition { x: 10, y: 10 },
            Property::new(Terrain::City, Some(p2), 200),
        ));

        // Base Islandとして認識させるため、自軍の拠点を配置
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        let transport = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
            ))
            .id();

        let cargo = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    ..UnitStats::mock()
                },
            ))
            .id();

        assign_test_transport_mission(&mut world, p1);

        let manager = world.get_resource::<TransportMissionManager>().unwrap();
        assert_eq!(manager.missions.len(), 1);
        let m = &manager.missions[0];
        assert_eq!(m.transport_entity, transport);
        assert_eq!(m.cargo_entity, cargo);
        assert_eq!(m.phase, TransportPhase::Pickup);
        assert_eq!(m.target_island, Some(IslandId(1))); // IslandId(0) is where transport is
    }

    #[test]
    fn test_assign_multiple_transport_missions() {
        let mut world = World::new();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        world.insert_resource(TransportMissionManager::default());

        let mut base_island_tiles = HashSet::new();
        base_island_tiles.insert(GridPosition { x: 0, y: 0 });
        base_island_tiles.insert(GridPosition { x: 1, y: 0 });

        let mut target_island_tiles = HashSet::new();
        target_island_tiles.insert(GridPosition { x: 5, y: 5 });
        target_island_tiles.insert(GridPosition { x: 6, y: 5 });
        target_island_tiles.insert(GridPosition { x: 7, y: 5 }); // Target has 3 properties

        let island_map = IslandMap {
            islands: vec![
                Island {
                    id: IslandId(0),
                    tiles: base_island_tiles,
                },
                Island {
                    id: IslandId(1),
                    tiles: target_island_tiles,
                },
            ],
        };
        world.insert_resource(island_map);

        // Target Islandとして認識させるため、敵(p2)の拠点を配置
        world.spawn((
            GridPosition { x: 5, y: 5 },
            Property::new(Terrain::City, Some(p2), 200),
        ));
        world.spawn((
            GridPosition { x: 6, y: 5 },
            Property::new(Terrain::Factory, Some(p2), 200),
        ));
        world.spawn((
            GridPosition { x: 7, y: 5 },
            Property::new(Terrain::Capital, Some(p2), 200),
        ));

        // Base Islandとして認識させるため、自軍の拠点を配置
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        // 複数の輸送機と歩兵を用意
        for _ in 0..3 {
            world.spawn((
                p1,
                Faction(p1),
                GridPosition { x: 0, y: 0 }, // All on base island
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
            ));
            world.spawn((
                p1,
                Faction(p1),
                GridPosition { x: 1, y: 0 }, // All on base island
                UnitStats {
                    unit_type: UnitType::Infantry,
                    ..UnitStats::mock()
                },
            ));
        }

        assign_test_transport_mission(&mut world, p1);

        // 3つの拠点が敵島にあるので、必要な歩兵数は3（または拠点数に応じた数）になるはず
        // そのため、ミッションは3つ生成されるべき
        let manager = world.get_resource::<TransportMissionManager>().unwrap();
        assert_eq!(manager.missions.len(), 3);
        for m in &manager.missions {
            assert_eq!(m.phase, TransportPhase::Pickup);
            assert_eq!(m.target_island, Some(IslandId(1)));
        }
    }
}
