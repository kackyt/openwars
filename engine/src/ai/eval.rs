#![allow(clippy::collapsible_if)]
#![allow(clippy::clone_on_copy)]

use crate::ai::ai_version::{AiVersion, PlayerAiSettings};

use crate::components::{Ammo, Faction, GridPosition, Health, PlayerId, Property, UnitStats};
use crate::resources::{Map, Terrain, master_data::MasterDataRegistry};
use bevy_ecs::prelude::*;
use std::collections::HashMap;

const TERRITORY_WEIGHT: i32 = 2500;
/// V2 の ZOC 方式支配面積はマス単位のため、拠点単位の TERRITORY_WEIGHT より大幅に小さくする
const ZOC_TERRITORY_WEIGHT: i32 = 300;
const CONSOLIDATION_RADIUS_TURNS: u32 = 2;
const NPV_WEIGHT: f32 = 1.0;

/// マップ面積に基づく期待終了ターン (map_1(140マス)≈30T, map_3(900マス)≈61T)
fn expected_end_turn(map: &Map) -> u32 {
    25 + (map.width * map.height) as u32 / 25
}

/// NPV の ETA 計算対象となる占領可能ユニットの情報
struct CaptureUnitInfo {
    pos: GridPosition,
    movement_type: crate::resources::MovementType,
    max_movement: u32,
    faction: PlayerId,
    unit_type: crate::resources::UnitType,
}

/// 輸送ユニット（空きスロットあり）の情報
struct TransportInfo {
    pos: GridPosition,
    movement_type: crate::resources::MovementType,
    max_movement: u32,
    faction: PlayerId,
    loadable: Vec<crate::resources::UnitType>,
}

/// 占領可能ユニット1体が拠点へ到達する最短ターン数。
/// 徒歩に加え、輸送ユニットを使う場合の区間分解近似
/// (迎え + 運搬 + 降車1T + 降車後徒歩) を比較して最小を返す。
#[allow(clippy::too_many_arguments)]
fn capture_eta(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), crate::systems::movement::OccupantInfo>,
    unit: &CaptureUnitInfo,
    transports: &[TransportInfo],
    prop_pos: (usize, usize),
    turn_cache: &mut crate::ai::turn_distance::AiTurnCache,
) -> Option<u32> {
    // 徒歩 ETA
    let inf_map = crate::ai::turn_distance::calculate_all_turn_distances_cached(
        map,
        registry,
        unit_positions,
        prop_pos,
        unit.movement_type,
        unit.max_movement,
        0,
        unit.faction,
        turn_cache,
    );
    let mut best = inf_map.get(&unit.pos).map(|d| d.turns);

    for t in transports {
        if !t.loadable.contains(&unit.unit_type) {
            continue;
        }
        // 降車地点の近似: ヘリ・装甲車は拠点隣接に降車して徒歩1T、
        // 輸送船は拠点から3マス以内の岸に寄せて徒歩2T
        let (drop_range, drop_walk) = match t.movement_type {
            crate::resources::MovementType::Ship => (3, 2),
            _ => (1, 1),
        };
        // 迎え: 輸送ユニット現在地 → ユニットの隣接マス
        let pickup_map = crate::ai::turn_distance::calculate_all_turn_distances_cached(
            map,
            registry,
            unit_positions,
            (unit.pos.x, unit.pos.y),
            t.movement_type,
            t.max_movement,
            1,
            t.faction,
            turn_cache,
        );
        let Some(pickup) = pickup_map.get(&t.pos) else {
            continue;
        };
        // 運搬: ユニット位置 → 拠点近傍 (drop_range 以内)
        let carry_map = crate::ai::turn_distance::calculate_all_turn_distances_cached(
            map,
            registry,
            unit_positions,
            prop_pos,
            t.movement_type,
            t.max_movement,
            drop_range,
            t.faction,
            turn_cache,
        );
        let Some(carry) = carry_map.get(&unit.pos) else {
            continue;
        };
        let eta = pickup.turns + carry.turns + 1 + drop_walk;
        if best.map_or(true, |b| eta < b) {
            best = Some(eta);
        }
    }
    best
}

/// 片陣営の NPV 合計。
/// NPV(拠点) = 収入 × max(0, T_end − 現在ターン − ETA) / (ETA + 1)
#[allow(clippy::too_many_arguments)]
fn side_npv(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), crate::systems::movement::OccupantInfo>,
    properties: &[(GridPosition, Property)],
    exclude_owner: PlayerId,
    units: &[CaptureUnitInfo],
    transports: &[TransportInfo],
    current_turn: u32,
    t_end: u32,
    turn_cache: &mut crate::ai::turn_distance::AiTurnCache,
) -> i32 {
    let mut total = 0i32;
    for (p_pos, prop) in properties {
        if prop.owner_id == Some(exclude_owner) {
            continue;
        }
        let income = registry.landscape_income(prop.terrain.as_str());
        if income == 0 {
            continue;
        }
        let mut best_eta: Option<u32> = None;
        for u in units {
            if let Some(eta) = capture_eta(
                map,
                registry,
                unit_positions,
                u,
                transports,
                (p_pos.x, p_pos.y),
                turn_cache,
            ) {
                if best_eta.map_or(true, |b| eta < b) {
                    best_eta = Some(eta);
                }
            }
        }
        let Some(eta) = best_eta else {
            continue;
        };
        let remaining = t_end.saturating_sub(current_turn).saturating_sub(eta);
        total += (income * remaining / (eta + 1)) as i32;
    }
    total
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BoardMetrics {
    pub total_score: i32,
    pub my_dominated_count: i32,
    pub enemy_dominated_count: i32,
    pub npv_score: i32,
    pub roi_score: i32,
}

/// 盤面の静的評価関数。
/// プレイヤーごとの AI バージョン (V1 / V2) に応じて評価ロジックを切り替えます。
pub fn evaluate_board(
    world: &mut World,
    perspective_player: PlayerId,
    cache: Option<&mut crate::ai::turn_distance::AiTurnCache>,
) -> i32 {
    evaluate_board_with_metrics(world, perspective_player, cache).total_score
}

pub fn evaluate_board_with_metrics(
    world: &mut World,
    perspective_player: PlayerId,
    cache: Option<&mut crate::ai::turn_distance::AiTurnCache>,
) -> BoardMetrics {
    let ai_version = {
        let settings = world.get_resource::<PlayerAiSettings>();
        settings
            .map(|s| s.get_version(perspective_player))
            .unwrap_or(AiVersion::V1)
    };

    match ai_version {
        AiVersion::V1 => evaluate_board_v1(world, perspective_player),
        AiVersion::V2 => evaluate_board_v2(world, perspective_player, cache),
    }
}

// ==========================================
// 従来型 AI 用の簡易評価ロジック (V1)
// ==========================================
pub fn evaluate_board_v1(world: &mut World, perspective_player: PlayerId) -> BoardMetrics {
    let mut score = 0;

    let mut capturing_props = HashMap::new();
    let mut prop_query = world.query::<(&GridPosition, &Property)>();
    for (pos, prop) in prop_query.iter(world) {
        if prop.capture_points < prop.max_capture_points {
            capturing_props.insert(*pos, prop.capture_points);
        }
    }

    let mut query = world.query::<(
        &Faction,
        &Health,
        &UnitStats,
        Option<&GridPosition>,
        Option<&Ammo>,
    )>();
    for (faction, health, stats, pos_opt, ammo_opt) in query.iter(world) {
        let mut base_value = if health.max > 0 {
            stats.cost as f32 * (health.current as f32 / health.max as f32)
        } else {
            0.0
        };

        // 弾薬補正
        if let Some(ammo) = ammo_opt {
            if stats.max_ammo1 > 0 && ammo.ammo1 == 0 {
                if stats.max_ammo2 > 0 && ammo.ammo2 > 0 {
                    base_value *= 0.5;
                } else {
                    base_value *= 0.2;
                }
            }
        }

        let mut value = base_value as i32;

        if faction.0 == perspective_player {
            if let Some(pos) = pos_opt {
                if let Some(&capture_points) = capturing_props.get(pos) {
                    if capture_points <= health.current {
                        value += 2000;
                    } else {
                        value += 1000 / (capture_points as i32 + 1);
                    }
                }
            }
            score += value;
        } else {
            if let Some(pos) = pos_opt {
                if let Some(&capture_points) = capturing_props.get(pos) {
                    if capture_points <= health.current {
                        value += 2000;
                    } else {
                        value += 1000 / (capture_points as i32 + 1);
                    }
                }
            }
            score -= value;
        }
    }

    // 拠点評価
    for (_pos, prop) in prop_query.iter(world) {
        if let Some(owner) = prop.owner_id {
            let prop_value = match prop.terrain {
                Terrain::Capital => 10000,
                Terrain::Factory | Terrain::Airport => 2000,
                Terrain::City => 1000,
                _ => 0,
            };

            if owner == perspective_player {
                score += prop_value;
            } else {
                score -= prop_value;
            }
        }
    }

    // 領域支配スコア (簡易版：所有権のみで計算)
    let mut my_territory = 0;
    let mut enemy_territory = 0;
    for (_pos, prop) in prop_query.iter(world) {
        if let Some(owner) = prop.owner_id {
            if owner == perspective_player {
                my_territory += 1;
            } else {
                enemy_territory += 1;
            }
        }
    }
    score += (my_territory - enemy_territory) * TERRITORY_WEIGHT;

    BoardMetrics {
        total_score: score,
        my_dominated_count: my_territory,
        enemy_dominated_count: enemy_territory,
        npv_score: 0,
        roi_score: 0,
    }
}

// ==========================================
// 戦術部隊 AI 用の精緻な評価ロジック (V2)
// ==========================================
fn evaluate_board_v2(
    world: &mut World,
    perspective_player: PlayerId,
    cache: Option<&mut crate::ai::turn_distance::AiTurnCache>,
) -> BoardMetrics {
    let mut score = 0;

    let map = world.resource::<Map>().clone();
    let registry = world.resource::<MasterDataRegistry>().clone();

    let current_turn = world
        .get_resource::<crate::resources::MatchState>()
        .map(|ms| ms.current_turn_number.0)
        .unwrap_or(1);

    // 占有情報（TurnDistance用）
    let mut unit_positions = HashMap::new();
    let mut q_all_units = world.query::<(&Faction, &GridPosition, &UnitStats)>();
    for (faction, pos, stats) in q_all_units.iter(world) {
        unit_positions.insert(
            (pos.x, pos.y),
            crate::systems::movement::OccupantInfo {
                player_id: faction.0,
                is_transport: stats.max_cargo > 0,
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots: stats.max_cargo,
            },
        );
    }

    // AI専用の個別に確保した一時キャッシュを準備
    let mut local_cache = crate::ai::turn_distance::AiTurnCache::default();
    let turn_cache = match cache {
        Some(c) => c,
        None => &mut local_cache,
    };

    // 拠点のリスト化
    let mut properties = Vec::new();
    let mut capturing_props = HashMap::new();
    let mut my_production_bases = Vec::new();
    let mut enemy_production_bases = Vec::new();

    let mut prop_query = world.query::<(&GridPosition, &Property)>();
    for (pos, prop) in prop_query.iter(world) {
        properties.push((*pos, prop.clone()));
        if prop.capture_points < prop.max_capture_points {
            capturing_props.insert(*pos, prop.capture_points);
        }

        if let Some(owner) = prop.owner_id {
            if prop.terrain == Terrain::Factory || prop.terrain == Terrain::Capital {
                if owner == perspective_player {
                    my_production_bases.push(*pos);
                } else {
                    enemy_production_bases.push(*pos);
                }
            }
        }
    }

    // 1. ユニット戦力評価
    let mut my_unit_positions_list = Vec::new();
    let mut enemy_unit_positions_list = Vec::new();
    let mut my_capture_units: Vec<CaptureUnitInfo> = Vec::new();
    let mut enemy_capture_units: Vec<CaptureUnitInfo> = Vec::new();
    let mut my_transports: Vec<TransportInfo> = Vec::new();
    let mut enemy_transports: Vec<TransportInfo> = Vec::new();

    let mut query = world.query::<(
        Entity,
        &Faction,
        &Health,
        &UnitStats,
        Option<&GridPosition>,
        Option<&Ammo>,
        Option<&crate::components::Transporting>,
        Option<&crate::components::CargoCapacity>,
    )>();
    for (_entity, faction, health, stats, pos_opt, ammo_opt, transporting_opt, cargo_opt) in
        query.iter(world)
    {
        let is_my_unit = faction.0 == perspective_player;

        // 輸送中のユニットは盤外座標 (x=9999) を持つため、位置ベースの評価から除外する
        let pos_opt = if transporting_opt.is_some() {
            None
        } else {
            pos_opt
        };

        let mut base_value = if health.max > 0 {
            stats.cost as f32 * (health.current as f32 / health.max as f32)
        } else {
            0.0
        };

        // (A) 位置補正 ＆ 孤立ペナルティの本実装
        if let Some(pos) = pos_opt {
            // 最も近い自軍拠点 / 敵軍拠点との距離比較で「支配タイル」を判定
            let mut nearest_my_prop_dist = 999;
            let mut nearest_enemy_prop_dist = 999;

            for (p_pos, p_prop) in &properties {
                if let Some(owner) = p_prop.owner_id {
                    let d = (pos.x as i32 - p_pos.x as i32).abs()
                        + (pos.y as i32 - p_pos.y as i32).abs();
                    if owner == perspective_player {
                        if d < nearest_my_prop_dist {
                            nearest_my_prop_dist = d;
                        }
                    } else {
                        if d < nearest_enemy_prop_dist {
                            nearest_enemy_prop_dist = d;
                        }
                    }
                }
            }

            let position_modifier = if nearest_my_prop_dist <= nearest_enemy_prop_dist {
                1.2 // 自軍支配タイル
            } else {
                let is_offensive_or_transport = stats.movement_type
                    == crate::resources::MovementType::Air
                    || stats.movement_type == crate::resources::MovementType::Ship
                    || stats.max_cargo > 0;
                if is_offensive_or_transport { 1.0 } else { 0.7 } // 敵軍支配タイル
            };
            base_value *= position_modifier;

            // --- 生産拠点からの逆引き SSSP による孤立ペナルティ計算 ---
            let target_production_bases = if is_my_unit {
                &my_production_bases
            } else {
                &enemy_production_bases
            };
            let mut min_turn_dist = None;

            for &p_pos in target_production_bases {
                let p_turns_map = crate::ai::turn_distance::calculate_all_turn_distances_cached(
                    &map,
                    &registry,
                    &unit_positions,
                    (p_pos.x, p_pos.y),
                    stats.movement_type,
                    stats.max_movement,
                    0, // interaction_max_range
                    faction.0,
                    turn_cache,
                );
                if let Some(&turns) = p_turns_map.get(pos) {
                    if min_turn_dist.map_or(true, |m| turns < m) {
                        min_turn_dist = Some(turns);
                    }
                }
            }

            let min_turns = min_turn_dist.map(|d| d.turns).unwrap_or(99);

            let isolation_modifier = if min_turns > 5 {
                let is_offensive_or_transport = stats.movement_type
                    == crate::resources::MovementType::Air
                    || stats.movement_type == crate::resources::MovementType::Ship
                    || stats.max_cargo > 0;
                if is_offensive_or_transport { 1.0 } else { 0.7 }
            } else if min_turns >= 3 {
                let is_offensive_or_transport = stats.movement_type
                    == crate::resources::MovementType::Air
                    || stats.movement_type == crate::resources::MovementType::Ship
                    || stats.max_cargo > 0;
                if is_offensive_or_transport { 1.0 } else { 0.85 }
            } else {
                1.0
            };
            base_value *= isolation_modifier;

            // 領域支配スコア (ZOC) 用にユニット位置を保存
            if is_my_unit {
                my_unit_positions_list.push(*pos);
            } else {
                enemy_unit_positions_list.push(*pos);
            }

            // NPV の ETA 計算用に占領可能ユニット・空きスロットのある輸送ユニットを収集
            if stats.can_capture {
                let info = CaptureUnitInfo {
                    pos: *pos,
                    movement_type: stats.movement_type,
                    max_movement: stats.max_movement,
                    faction: faction.0,
                    unit_type: stats.unit_type,
                };
                if is_my_unit {
                    my_capture_units.push(info);
                } else {
                    enemy_capture_units.push(info);
                }
            }
            if stats.max_cargo > 0 {
                let free_slots = cargo_opt
                    .map(|c| c.max.saturating_sub(c.loaded.len() as u32))
                    .unwrap_or(stats.max_cargo);
                if free_slots > 0 {
                    let info = TransportInfo {
                        pos: *pos,
                        movement_type: stats.movement_type,
                        max_movement: stats.max_movement,
                        faction: faction.0,
                        loadable: stats.loadable_unit_types.clone(),
                    };
                    if is_my_unit {
                        my_transports.push(info);
                    } else {
                        enemy_transports.push(info);
                    }
                }
            }
        }

        // (B) 弾薬状態補正
        if let Some(ammo) = ammo_opt {
            if stats.max_ammo1 > 0 && ammo.ammo1 == 0 {
                if stats.max_ammo2 > 0 && ammo.ammo2 > 0 {
                    base_value *= 0.5;
                } else {
                    base_value *= 0.2;
                }
            }
        }

        let mut value = base_value as i32;

        // (C) 任務補正
        if let Some(pos) = pos_opt {
            if let Some(&capture_points) = capturing_props.get(pos) {
                if capture_points <= health.current {
                    value += 2000; // 次のターンに占領完了するボーナス
                } else {
                    value += 1000 / (capture_points as i32 + 1);
                }
            }
        }

        if is_my_unit {
            score += value;
        } else {
            score -= value;
        }
    }

    // 2. 拠点孤立度補正の本実装
    for (pos, prop) in &properties {
        if let Some(owner) = prop.owner_id {
            let base_prop_value = match prop.terrain {
                Terrain::Capital => 10000,
                Terrain::Factory | Terrain::Airport => 2000,
                Terrain::City => 1000,
                _ => 0,
            };

            // 拠点から MovementType::Infantry (基準の歩兵) で 2ターン以内の周辺の他の拠点を調査。
            // これも拠点 pos を始点とする SSSP 1回で $O(1)$ 判定可能。
            let p_turns_map = crate::ai::turn_distance::calculate_all_turn_distances_cached(
                &map,
                &registry,
                &unit_positions,
                (pos.x, pos.y),
                crate::resources::MovementType::Infantry,
                3,
                0, // interaction_max_range
                owner,
                turn_cache,
            );

            let mut total_nearby = 0;
            let mut friendly_nearby = 0;

            for (other_pos, other_prop) in &properties {
                if other_pos == pos {
                    continue;
                }

                if let Some(&turns) = p_turns_map.get(other_pos) {
                    if turns.turns <= CONSOLIDATION_RADIUS_TURNS {
                        total_nearby += 1;
                        if other_prop.owner_id == Some(owner) {
                            friendly_nearby += 1;
                        }
                    }
                }
            }

            let consolidation_ratio = if total_nearby > 0 {
                friendly_nearby as f32 / total_nearby as f32
            } else {
                1.0 // 周囲に他の拠点がない場合は孤立していないとみなす
            };

            let prop_value = (base_prop_value as f32 * (0.5 + consolidation_ratio)) as i32;

            if owner == perspective_player {
                score += prop_value;
            } else {
                score -= prop_value;
            }
        }
    }

    // 3. 領域支配スコア (ZOC 方式)
    // 支配領域 = ユニットのいるマス + その隣接マス (ZOC) + 占領済み拠点のマス
    let mut my_cells: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut enemy_cells: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();

    for pos in &my_unit_positions_list {
        my_cells.insert((pos.x, pos.y));
        for adj in map.get_adjacent(pos.x, pos.y) {
            my_cells.insert(adj);
        }
    }
    for pos in &enemy_unit_positions_list {
        enemy_cells.insert((pos.x, pos.y));
        for adj in map.get_adjacent(pos.x, pos.y) {
            enemy_cells.insert(adj);
        }
    }

    for (p_pos, prop) in &properties {
        if let Some(owner) = prop.owner_id {
            if owner == perspective_player {
                my_cells.insert((p_pos.x, p_pos.y));
            } else {
                enemy_cells.insert((p_pos.x, p_pos.y));
            }
        }
    }

    let my_dominated_count = my_cells.len() as i32;
    let enemy_dominated_count = enemy_cells.len() as i32;

    score += (my_dominated_count - enemy_dominated_count) * ZOC_TERRITORY_WEIGHT;

    // 4. NPV (正味現在価値) ベースの占領評価
    let t_end = expected_end_turn(&map);
    let my_npv = side_npv(
        &map,
        &registry,
        &unit_positions,
        &properties,
        perspective_player,
        &my_capture_units,
        &my_transports,
        current_turn,
        t_end,
        turn_cache,
    );
    let enemy_npv = match enemy_capture_units.first() {
        Some(u) => side_npv(
            &map,
            &registry,
            &unit_positions,
            &properties,
            u.faction,
            &enemy_capture_units,
            &enemy_transports,
            current_turn,
            t_end,
            turn_cache,
        ),
        None => 0,
    };
    let npv_score = my_npv - enemy_npv;
    score += (npv_score as f32 * NPV_WEIGHT) as i32;

    BoardMetrics {
        total_score: score,
        my_dominated_count,
        enemy_dominated_count,
        npv_score,
        roi_score: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Transporting;
    use crate::resources::Terrain;

    #[test]
    fn test_evaluate_board() {
        let mut world = World::new();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // V1/V2 用の PlayerAiSettings を登録 (デフォルト V1 に設定してテスト互換性を保つか、Mapを追加するか)
        // ここではV1としてテストするか、Mapを入れるかですが、V1のスコアをアサートしているのでV1に設定します。
        let mut settings = PlayerAiSettings::new();
        settings.set_version(p1, AiVersion::V1);
        settings.set_version(p2, AiVersion::V1);
        world.insert_resource(settings);

        // Friendly unit (full hp) -> 1000 cost * 10/10 = +1000
        world.spawn((
            Faction(p1),
            Health {
                current: 100,
                max: 100,
            },
            UnitStats {
                cost: 1000,
                ..UnitStats::mock()
            },
        ));

        // Friendly unit (half hp) -> 2000 cost * 5/10 = +1000
        world.spawn((
            Faction(p1),
            Health {
                current: 50,
                max: 100,
            },
            UnitStats {
                cost: 2000,
                ..UnitStats::mock()
            },
        ));

        // Enemy unit -> 1500 cost * 10/10 = -1500
        world.spawn((
            Faction(p2),
            Health {
                current: 100,
                max: 100,
            },
            UnitStats {
                cost: 1500,
                ..UnitStats::mock()
            },
        ));

        // Enemy transported unit
        world.spawn((
            Faction(p2),
            Health {
                current: 100,
                max: 100,
            },
            UnitStats {
                cost: 5000,
                ..UnitStats::mock()
            },
            Transporting(Entity::from_raw(999)),
        ));

        // Properties (位置情報 GridPosition がないと `prop_query` にマッチしなくなるため付与)
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 200),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::City, Some(p1), 200),
        ));
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(Terrain::Factory, Some(p2), 200),
        ));
        world.spawn((
            GridPosition { x: 3, y: 0 },
            Property::new(Terrain::City, None, 200),
        ));

        let score = evaluate_board(&mut world, p1, None);
        // 期待スコア内訳:
        // P1 ユニット価値: 1000 + 1000 = +2000
        // P2 ユニット価値: -1500 - 5000 = -6500
        // 拠点価値: 10000(Capital) + 1000(City) - 2000(Factory) = +9000
        // 領域支配: (2[P1所有] - 1[P2所有]) * 2500 = +2500
        // 合計: 2000 - 6500 + 9000 + 2500 = 7000
        assert_eq!(score, 7000);

        let score_p2 = evaluate_board(&mut world, p2, None);
        assert_eq!(score_p2, -7000);
    }

    #[test]
    fn test_evaluate_board_v2_dominated_area() {
        let mut world = World::new();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // V2 用の PlayerAiSettings を登録
        let mut settings = PlayerAiSettings::new();
        settings.set_version(p1, AiVersion::V2);
        settings.set_version(p2, AiVersion::V2);
        world.insert_resource(settings);

        // テスト用のマップを登録（幅10, 高さ10）
        let mut map = Map::new(10, 10, Terrain::Plains, crate::resources::GridTopology::Square);
        // 全て平原にしておく
        for x in 0..10 {
            for y in 0..10 {
                map.set_terrain(x, y, Terrain::Plains);
            }
        }
        world.insert_resource(map);

        // MasterDataRegistryを登録
        world.insert_resource(crate::resources::MasterDataRegistry::load().unwrap());
        world.insert_resource(crate::resources::MatchState::default());

        // 拠点の配置
        // 1. すでにP1が占領済みの拠点 (x=0, y=0) -> 敵が近くにいてもP1の支配領域になるはず
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::City, Some(p1), 200),
        ));
        // 2. すでにP2が占領済みの拠点 (x=9, y=9)
        world.spawn((
            GridPosition { x: 9, y: 9 },
            Property::new(Terrain::City, Some(p2), 200),
        ));
        // 3. 未占領（中立）の拠点 (x=5, y=5)
        world.spawn((
            GridPosition { x: 5, y: 5 },
            Property::new(Terrain::City, None, 200),
        ));

        // ユニットの配置
        // P1のユニットを中立拠点に近づける (x=4, y=5)
        world.spawn((
            Faction(p1),
            GridPosition { x: 4, y: 5 },
            Health { current: 100, max: 100 },
            UnitStats {
                movement_type: crate::resources::MovementType::Infantry,
                max_movement: 3,
                ..UnitStats::mock()
            },
        ));
        // P2のユニットを中立拠点から遠ざける (x=8, y=8)
        world.spawn((
            Faction(p2),
            GridPosition { x: 8, y: 8 },
            Health { current: 100, max: 100 },
            UnitStats {
                movement_type: crate::resources::MovementType::Infantry,
                max_movement: 3,
                ..UnitStats::mock()
            },
        ));
        // P2のユニットをP1の拠点に隣接させる (x=1, y=0)
        // 距離的にはP2の方がP1拠点に近いが、すでにP1所有なのでP1の支配領域になることを確認する
        world.spawn((
            Faction(p2),
            GridPosition { x: 1, y: 0 },
            Health { current: 100, max: 100 },
            UnitStats {
                movement_type: crate::resources::MovementType::Infantry,
                max_movement: 3,
                ..UnitStats::mock()
            },
        ));

        let metrics = super::evaluate_board_with_metrics(&mut world, p1, None);

        // 期待される結果 (ZOC方式: ユニットのマス + 隣接4マス + 占領済み拠点):
        // P1: ユニット(4,5) -> {(4,5),(3,5),(5,5),(4,4),(4,6)} の5マス + 所有拠点(0,0) = 6
        // P2: ユニット(8,8) -> 5マス、ユニット(1,0) -> {(1,0),(0,0),(2,0),(1,1)} の4マス(マップ端)、
        //     所有拠点(9,9) = 5 + 4 + 1 = 10
        assert_eq!(metrics.my_dominated_count, 6, "P1 should dominate 6 cells");
        assert_eq!(metrics.enemy_dominated_count, 10, "P2 should dominate 10 cells");
    }

    #[test]
    fn test_capture_eta_transport_advantage() {
        let map = Map::new(
            20,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = crate::resources::MasterDataRegistry::load().unwrap();
        let unit_positions = HashMap::new();
        let unit = CaptureUnitInfo {
            pos: GridPosition { x: 0, y: 0 },
            movement_type: crate::resources::MovementType::Infantry,
            max_movement: 3,
            faction: PlayerId(1),
            unit_type: crate::resources::UnitType::Infantry,
        };

        // 徒歩のみ: (0,0) -> (19,0) はコスト19、移動力3 -> ceil(19/3) = 7ターン
        let mut cache = crate::ai::turn_distance::AiTurnCache::default();
        let walk_eta = capture_eta(
            &map,
            &registry,
            &unit_positions,
            &unit,
            &[],
            (19, 0),
            &mut cache,
        )
        .unwrap();
        assert_eq!(walk_eta, 7);

        // 輸送ヘリ (移動力9) が隣にいる場合:
        // 迎え 0T + 運搬 ceil(18/9)=2T + 降車 1T + 徒歩 1T = 4ターン
        let heli = TransportInfo {
            pos: GridPosition { x: 1, y: 0 },
            movement_type: crate::resources::MovementType::Air,
            max_movement: 9,
            faction: PlayerId(1),
            loadable: vec![crate::resources::UnitType::Infantry],
        };
        let mut cache2 = crate::ai::turn_distance::AiTurnCache::default();
        let eta_with_heli = capture_eta(
            &map,
            &registry,
            &unit_positions,
            &unit,
            std::slice::from_ref(&heli),
            (19, 0),
            &mut cache2,
        )
        .unwrap();
        assert_eq!(eta_with_heli, 4);
        assert!(eta_with_heli < walk_eta, "輸送ヘリ活用でETAが短縮されること");
    }
}
