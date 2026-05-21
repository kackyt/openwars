use crate::ai::islands::IslandMap;
use crate::ai::missions::{TransportMission, TransportMissionManager, TransportPhase};
use crate::ai::objectives::Objective;
use crate::components::{CargoCapacity, Faction, GridPosition, PlayerId, Property, UnitStats};
use crate::resources::UnitType;
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};

/// 敵本島（敵生産拠点がある島）への侵攻が許可されているか判定する。
///
/// - すでにその島に、自軍の戦闘ユニット（歩兵・重歩兵・輸送機・輸送船・補給車以外の、戦車や対空戦車など）が1基以上展開している（護衛の存在）
/// - または、自軍の戦闘ユニット総数が、敵の戦闘ユニット総数に対して圧倒的優勢（味方戦闘ユニット数 >= 敵戦闘ユニット数 * 1.2 + 2）である（圧倒的戦力優勢）
pub fn is_invasion_allowed(
    world: &mut World,
    player_id: PlayerId,
    _target_island_id: crate::ai::islands::IslandId,
    island: &crate::ai::islands::Island,
) -> bool {
    let mut own_escort_present = false;
    let mut own_combat_total = 0;
    let mut enemy_combat_total = 0;

    let mut query = world.query::<(&GridPosition, &Faction, &UnitStats)>();
    for (pos, faction, stats) in query.iter(world) {
        let is_combat_unit = !matches!(
            stats.unit_type,
            UnitType::Infantry
                | UnitType::Mech
                | UnitType::TransportHelicopter
                | UnitType::Lander
                | UnitType::SupplyTruck
        );

        if is_combat_unit {
            if faction.0 == player_id {
                own_combat_total += 1;
                // 目標の島にいるかチェック
                if island.tiles.contains(pos) {
                    own_escort_present = true;
                }
            } else {
                enemy_combat_total += 1;
            }
        }
    }

    if own_escort_present {
        return true;
    }

    // 圧倒的優勢判定: 自軍戦闘ユニット総数 >= 敵軍戦闘ユニット総数 * 1.2 + 2
    let threshold = ((enemy_combat_total as f64 * 1.2) as u32) + 2;
    if own_combat_total >= threshold {
        return true;
    }

    false
}

/// 戦略目標に基づいて輸送ミッションを割り当てる。
///
/// 1. 現在の全拠点を取得し、島ごとに分類する。
/// 2. 目標（Target Islands）の優先度を評価し、最も高い目標を選択する。
/// 3. 必要な歩兵数と輸送機数を算出し、フリーなユニットから割り当てる。
pub fn assign_transport_missions(world: &mut World, player_id: PlayerId) {
    // 進行中の自軍ミッションに割り当てられているユニットを収集
    let mut busy_transports = HashSet::new();
    let mut busy_infantry = HashSet::new();
    let mut missions_to_add = Vec::new();
    let mut enemy_invasion_islands = HashSet::new();

    if let Some(manager) = world.get_resource::<TransportMissionManager>() {
        for m in &manager.missions {
            if world
                .get::<Faction>(m.transport_entity)
                .is_some_and(|faction| faction.0 == player_id)
            {
                busy_transports.insert(m.transport_entity);
                // Return フェーズでは歩兵はすでに降ろされているため、
                // busy_infantry に登録せず通常のAI意思決定（占領など）に参加させる
                if m.phase != TransportPhase::Return {
                    busy_infantry.insert(m.cargo_entity);
                }
            }
        }
    }

    // 1. 全拠点情報を収集
    let mut properties_map = HashMap::new();
    let mut ownership_map = HashMap::new();
    {
        let mut query = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in query.iter(world) {
            properties_map.insert(*pos, *prop);
            ownership_map.insert(*pos, prop.owner_id);
        }
    }

    // 2. 目標の優先度評価と島情報の取得
    let mut objectives = Vec::new();
    let mut base_islands_cache = HashSet::new();

    world.resource_scope(|_world, island_map: Mut<IslandMap>| {
        let (base_islands, target_islands) = island_map.classify_islands(player_id, &ownership_map);

        if target_islands.is_empty() {
            return; // 目標がない
        }

        base_islands_cache.extend(base_islands.iter().copied());

        let registry = _world.resource::<crate::resources::MasterDataRegistry>();

        // 自軍の生産拠点（工場・首都）の座標を収集
        let mut own_production_bases = Vec::new();
        for (pos, prop) in &properties_map {
            if prop.owner_id == Some(player_id)
                && (prop.terrain == crate::resources::Terrain::Factory
                    || prop.terrain == crate::resources::Terrain::Capital)
            {
                own_production_bases.push(*pos);
            }
        }

        for target_id in target_islands {
            if let Some(island) = island_map.islands.iter().find(|i| i.id == target_id) {
                let mut island_props = Vec::new();
                let mut min_distance = i32::MAX;
                let mut enemy_production_count = 0;

                for tile in &island.tiles {
                    if let Some(prop) = properties_map.get(tile)
                        && prop.owner_id != Some(player_id)
                    {
                        // 各未占領拠点と自軍生産拠点との間の最短マンハッタン距離を計算
                        let mut local_min_dist = i32::MAX;
                        let mut nearest_base_pos = None;
                        for base_pos in &own_production_bases {
                            let dist = (tile.x as i32 - base_pos.x as i32).abs()
                                + (tile.y as i32 - base_pos.y as i32).abs();
                            if dist < local_min_dist {
                                local_min_dist = dist;
                                nearest_base_pos = Some(*base_pos);
                            }
                        }

                        // 徒歩で行く方が早い、または十分に近すぎる拠点は、輸送機を使う必要がない（無意味な短距離輸送の防止）
                        // 目標拠点と同じ島に自軍生産拠点があり、かつマンハッタン距離が6マス以下（徒歩2ターン以内で到達可能）の場合のみ除外する。
                        // 別島の場合は海を越える必要があるため、距離に関わらず輸送ミッションが必要。
                        if nearest_base_pos
                            .and_then(|pos| island_map.get_island_at(&pos))
                            .filter(|base_island| {
                                base_island.id == target_id && local_min_dist <= 6
                            })
                            .is_some()
                        {
                            continue;
                        }

                        island_props.push((*tile, prop.terrain));

                        // 敵勢力が所有している生産拠点（首都・工場）をカウント（軍事脅威ペナルティの判定用）
                        if prop.owner_id.is_some()
                            && (prop.terrain == crate::resources::Terrain::Factory
                                || prop.terrain == crate::resources::Terrain::Capital)
                        {
                            enemy_production_count += 1;
                        }

                        if local_min_dist < min_distance {
                            min_distance = local_min_dist;
                        }
                    }
                }

                let distance_to_nearest_base = if min_distance == i32::MAX {
                    10 // 生産拠点がない、または距離が測れない場合のデフォルト値
                } else {
                    min_distance
                };

                let objective = Objective::evaluate(
                    target_id,
                    &island_props,
                    distance_to_nearest_base,
                    enemy_production_count,
                    registry,
                );
                if enemy_production_count > 0 {
                    enemy_invasion_islands.insert(target_id);
                }
                objectives.push(objective);
            }
        }
    });

    if objectives.is_empty() {
        return;
    }

    // スコア降順でソート
    // 海を渡る必要がある「別の島」を最優先とし、その次にスコア降順でソート。
    // 同じ島（歩いて到達できる）への輸送は、余剰輸送機がある場合のみ行う。
    objectives.sort_by_key(|b| {
        let is_same_island = base_islands_cache.contains(&b.target_island);
        let group = if is_same_island { 1u8 } else { 0u8 }; // 別島=0(高優先), 同島=1(低優先)
        (group, std::cmp::Reverse(b.priority_score))
    });

    // 3. フリーなユニットの収集
    let mut free_empty_transports = Vec::new();
    let mut free_loaded_transports = Vec::new();
    {
        let mut query =
            world.query::<(Entity, &Faction, &UnitStats, &CargoCapacity, &GridPosition)>();
        for (entity, faction, stats, cargo, pos) in query.iter(world) {
            if faction.0 == player_id
                && (stats.unit_type == UnitType::TransportHelicopter
                    || stats.unit_type == UnitType::Lander)
                && !busy_transports.contains(&entity)
            {
                if cargo.loaded.is_empty() {
                    free_empty_transports.push((entity, *pos));
                } else if let Some(&cargo_entity) = cargo.loaded.first() {
                    free_loaded_transports.push((entity, cargo_entity, *pos));
                }
            }
        }
    }

    let mut free_infantry = Vec::new();
    world.resource_scope(|world, island_map: Mut<IslandMap>| {
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
                if let Some(island) = island_map.get_island_at(pos)
                    && base_islands_cache.contains(&island.id)
                {
                    free_infantry.push((entity, *pos));
                }
            }
        }
    });

    // 目標に対して割り当てを行う
    for objective in objectives {
        // 敵本島への侵攻の場合、護衛（随伴）条件をチェックする
        if enemy_invasion_islands.contains(&objective.target_island) {
            let allowed = world.resource_scope(|_world, island_map: Mut<IslandMap>| {
                if let Some(island) = island_map
                    .islands
                    .iter()
                    .find(|i| i.id == objective.target_island)
                {
                    is_invasion_allowed(_world, player_id, objective.target_island, island)
                } else {
                    false
                }
            });
            if !allowed {
                continue; // 侵攻条件を満たさないため、輸送割り当てをスキップ
            }
        }

        // 必要な部隊数
        let needed = objective.needed_infantry;

        // 現在割り当て中のミッション（同じTarget Islandへ向かっているもの）をカウント
        let mut current_assigned = 0;
        if let Some(manager) = world.get_resource::<TransportMissionManager>() {
            for m in &manager.missions {
                if world
                    .get::<Faction>(m.transport_entity)
                    .is_some_and(|faction| {
                        faction.0 == player_id && m.target_island == Some(objective.target_island)
                    })
                {
                    current_assigned += 1;
                }
            }
        }

        let mut to_assign = needed.0.saturating_sub(current_assigned);

        // 優先度1: すでに積載済みの輸送機を優先して割り当てる（フェーズは Transit から開始）
        while to_assign > 0 && !free_loaded_transports.is_empty() {
            let (transport_entity, cargo_entity, _) = free_loaded_transports.pop().unwrap();

            missions_to_add.push(TransportMission {
                transport_entity,
                cargo_entity,
                phase: TransportPhase::Transit, // すでに積載済みなので Transit から！
                target_island: Some(objective.target_island),
            });

            to_assign -= 1;
        }

        // 優先度2: 空の輸送機とフリーの歩兵をマッチングして新規割り当て（フェーズは Pickup から開始）
        while to_assign > 0 && !free_empty_transports.is_empty() && !free_infantry.is_empty() {
            let (transport_entity, _) = free_empty_transports.pop().unwrap();
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
        if free_empty_transports.is_empty() && free_loaded_transports.is_empty() {
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

#[cfg(test)]
mod tests {
    pub fn assign_test_transport_mission(world: &mut World, player_id: PlayerId) {
        super::assign_transport_missions(world, player_id);
    }

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
        world.insert_resource(crate::resources::MasterDataRegistry::load().unwrap());

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
        world.insert_resource(crate::resources::MasterDataRegistry::load().unwrap());

        // Target Islandとして認識させるため、敵(p2)の拠点を配置
        // 侵攻制限がかからないように、すべて生産能力のない都市(City)として配置します
        world.spawn((
            GridPosition { x: 5, y: 5 },
            Property::new(Terrain::City, Some(p2), 200),
        ));
        world.spawn((
            GridPosition { x: 6, y: 5 },
            Property::new(Terrain::City, Some(p2), 200),
        ));
        world.spawn((
            GridPosition { x: 7, y: 5 },
            Property::new(Terrain::City, Some(p2), 200),
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

    #[test]
    fn test_assign_transport_mission_prioritizes_neutral_island_over_dangerous_island() {
        let mut world = World::new();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        world.insert_resource(TransportMissionManager::default());

        // 3つの島を定義
        // 島0: 自軍基地島 (Base Island)
        let mut base_island_tiles = HashSet::new();
        base_island_tiles.insert(GridPosition { x: 0, y: 0 });

        // 島1: 敵の本島 (危険な島、敵の生産拠点がある)
        // 距離は近いが敵の生産拠点があるためペナルティがかかる
        let mut dangerous_island_tiles = HashSet::new();
        dangerous_island_tiles.insert(GridPosition { x: 2, y: 2 });

        // 島2: 中立の島 (安全な島、敵の生産拠点がない)
        // 距離は少し遠いが安全
        let mut neutral_island_tiles = HashSet::new();
        neutral_island_tiles.insert(GridPosition { x: 5, y: 5 });

        let island_map = IslandMap {
            islands: vec![
                Island {
                    id: IslandId(0),
                    tiles: base_island_tiles,
                },
                Island {
                    id: IslandId(1),
                    tiles: dangerous_island_tiles,
                },
                Island {
                    id: IslandId(2),
                    tiles: neutral_island_tiles,
                },
            ],
        };
        world.insert_resource(island_map);
        world.insert_resource(crate::resources::MasterDataRegistry::load().unwrap());

        // 島0: 自軍の生産拠点
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        // 島1 (危険島): 敵(p2)の生産拠点 (Factory) がある
        world.spawn((
            GridPosition { x: 2, y: 2 },
            Property::new(Terrain::Factory, Some(p2), 200),
        ));

        // 島2 (安全島): 中立の都市 (City) がある (中立は owner_id = None)
        world.spawn((
            GridPosition { x: 5, y: 5 },
            Property::new(Terrain::City, None, 200),
        ));

        // 輸送ユニットと歩兵を1体ずつ用意
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

        // 期待値計算：
        // 島1 (危険島): 収入=1000, 距離=4 (dx=2, dy=2), 敵生産拠点数=1 (ペナルティ 20.0 * 1 = 20)
        //   Score = 1000 / (1.0 + 4.0 + 20.0) = 1000 / 25 = 40
        // 島2 (安全島): 収入=1000, 距離=10 (dx=5, dy=5), 敵生産拠点数=0 (ペナルティ 0)
        //   Score = 1000 / (1.0 + 10.0 + 0.0) = 1000 / 11 = 90
        //
        // 安全な島2のスコア(90)のほうが危険な島1のスコア(40)より高くなるため、島2が優先されるはず。
        let manager = world.get_resource::<TransportMissionManager>().unwrap();
        assert_eq!(manager.missions.len(), 1);
        let m = &manager.missions[0];
        assert_eq!(m.transport_entity, transport);
        assert_eq!(m.cargo_entity, cargo);
        assert_eq!(m.target_island, Some(IslandId(2))); // 安全な中立島 (IslandId(2)) が優先されること
    }

    #[test]
    fn test_assign_transport_mission_respects_invasion_constraints() {
        let mut world = World::new();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        world.insert_resource(TransportMissionManager::default());

        // 島0: 自軍基地島 (Base Island)
        let mut base_island_tiles = HashSet::new();
        base_island_tiles.insert(GridPosition { x: 0, y: 0 });

        // 島1: 敵の本島 (危険な島、敵の生産拠点がある)
        let mut dangerous_island_tiles = HashSet::new();
        dangerous_island_tiles.insert(GridPosition { x: 2, y: 2 });

        let island_map = IslandMap {
            islands: vec![
                Island {
                    id: IslandId(0),
                    tiles: base_island_tiles,
                },
                Island {
                    id: IslandId(1),
                    tiles: dangerous_island_tiles,
                },
            ],
        };
        world.insert_resource(island_map);
        world.insert_resource(crate::resources::MasterDataRegistry::load().unwrap());

        // 島0: 自軍の生産拠点
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        // 島1 (危険島): 敵(p2)の生産拠点 (Factory) がある
        world.spawn((
            GridPosition { x: 2, y: 2 },
            Property::new(Terrain::Factory, Some(p2), 200),
        ));

        // 輸送ユニットと歩兵を1体ずつ用意
        world.spawn((
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
        ));

        world.spawn((
            p1,
            Faction(p1),
            GridPosition { x: 0, y: 0 },
            UnitStats {
                unit_type: UnitType::Infantry,
                ..UnitStats::mock()
            },
        ));

        // --- ケース1: 護衛なし・優勢でもない状態 ---
        assign_test_transport_mission(&mut world, p1);

        // 侵攻制限（護衛なし＆非優勢）により、ミッションは割り当てられないはず
        let manager = world.get_resource::<TransportMissionManager>().unwrap();
        assert_eq!(manager.missions.len(), 0);

        // --- ケース2: 島1に味方の戦闘ユニット（例：戦車）がすでに展開している（護衛の存在） ---
        let tank = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 2, y: 2 }, // 敵本島内
                UnitStats {
                    unit_type: UnitType::Tank, // 戦闘ユニット
                    ..UnitStats::mock()
                },
            ))
            .id();

        assign_test_transport_mission(&mut world, p1);

        // 護衛がいるため、侵攻ミッションが割り当てられるはず
        let manager = world.get_resource::<TransportMissionManager>().unwrap();
        assert_eq!(manager.missions.len(), 1);

        // クリーンアップして次のケースへ
        world.entity_mut(tank).despawn();
        world
            .get_resource_mut::<TransportMissionManager>()
            .unwrap()
            .missions
            .clear();

        // --- ケース3: 圧倒的優勢である（味方の戦闘ユニット総数 >= 敵戦闘ユニット * 1.2 + 2） ---
        // 敵の戦闘ユニットを1台配置
        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 2, y: 2 },
            UnitStats {
                unit_type: UnitType::Tank,
                ..UnitStats::mock()
            },
        ));

        // 味方の戦闘ユニットを十分な数（1.2 * 1 + 2 = 3.2 -> 4両以上）配置する
        for _ in 0..4 {
            world.spawn((
                p1,
                Faction(p1),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    ..UnitStats::mock()
                },
            ));
        }

        assign_test_transport_mission(&mut world, p1);

        // 圧倒的優勢であるため、侵攻ミッションが割り当てられるはず
        let manager = world.get_resource::<TransportMissionManager>().unwrap();
        assert_eq!(manager.missions.len(), 1);
    }
}
