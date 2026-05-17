use crate::ai::islands::IslandMap;
use crate::ai::missions::{TransportMission, TransportMissionManager, TransportPhase};
use crate::components::{CargoCapacity, Faction, GridPosition, PlayerId, UnitStats};
use crate::resources::UnitType;
use bevy_ecs::prelude::*;

/// 輸送任務のテスト割り当てを行う
/// 1. 指定したプレイヤーの空の輸送ヘリを探す
/// 2. 搭載されていない（フリーな）歩兵を探す
/// 3. ヘリの現在位置とは異なる島をターゲットとして選ぶ
/// 4. 任務を作成し、TransportMissionManager に登録する
pub fn assign_test_transport_mission(world: &mut World, player_id: PlayerId) {
    // すでに進行中のミッションがある場合はスキップ (簡略化)
    let has_mission = world
        .get_resource::<TransportMissionManager>()
        .is_some_and(|manager| {
            manager.missions.iter().any(|m| {
                world
                    .get::<Faction>(m.transport_entity)
                    .is_some_and(|f| f.0 == player_id)
            })
        });

    if has_mission {
        return;
    }

    // 1. 空の輸送ヘリを探す
    let mut empty_heli = None;
    let mut heli_pos_cache = None;
    {
        let mut query =
            world.query::<(Entity, &Faction, &UnitStats, &CargoCapacity, &GridPosition)>();
        for (entity, faction, stats, cargo, pos) in query.iter(world) {
            if faction.0 == player_id
                && stats.unit_type == UnitType::TransportHelicopter
                && cargo.loaded.is_empty()
            {
                empty_heli = Some(entity);
                heli_pos_cache = Some(*pos);
                break;
            }
        }
    }

    let transport_entity = match empty_heli {
        Some(e) => e,
        None => return,
    };
    let heli_pos = heli_pos_cache.unwrap();

    // 2. フリーな歩兵を探す（他のCargoに入っていないかチェック）
    let mut free_infantry = None;
    {
        let mut query_inf = world.query::<(Entity, &Faction, &UnitStats, Option<&crate::components::Transporting>)>();
        for (entity, faction, stats, transporting_opt) in query_inf.iter(world) {
            if faction.0 == player_id
                && stats.unit_type == UnitType::Infantry
                && transporting_opt.is_none()
            {
                free_infantry = Some(entity);
                break;
            }
        }
    }

    let cargo_entity = match free_infantry {
        Some(e) => e,
        None => return,
    };

    // 3. ターゲットの島を探す（ヘリの現在地が含まれていない島）
    let mut target_island = None;
    if let Some(island_map) = world.get_resource::<IslandMap>() {
        for island in &island_map.islands {
            if !island.tiles.contains(&heli_pos) && !island.tiles.is_empty() {
                target_island = Some(island.id);
                break;
            }
        }
    }

    if target_island.is_none() {
        return;
    }

    // 4. ミッションを登録する
    let new_mission = TransportMission {
        transport_entity,
        cargo_entity,
        phase: TransportPhase::Pickup,
        target_island,
    };

    if let Some(mut manager) = world.get_resource_mut::<TransportMissionManager>() {
        manager.missions.push(new_mission);
    } else {
        let mut manager = TransportMissionManager::default();
        manager.missions.push(new_mission);
        world.insert_resource(manager);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::islands::{Island, IslandId, IslandMap};
    use crate::components::{Faction, GridPosition, PlayerId, UnitStats};
    use crate::resources::UnitType;

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
                GridPosition { x: 1, y: 0 },
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
}
