#![allow(clippy::collapsible_if)]
#![allow(clippy::clone_on_copy)]

use crate::ai::ai_version::{AiVersion, PlayerAiSettings};

use crate::components::{Ammo, Faction, GridPosition, Health, PlayerId, Property, UnitStats};
use crate::resources::{Map, Terrain, master_data::MasterDataRegistry};
use bevy_ecs::prelude::*;
use std::collections::HashMap;

const TERRITORY_WEIGHT: i32 = 2500;
const CONSOLIDATION_RADIUS_TURNS: u32 = 2;

/// 盤面の静的評価関数。
/// プレイヤーごとの AI バージョン (V1 / V2) に応じて評価ロジックを切り替えます。
pub fn evaluate_board(
    world: &mut World,
    perspective_player: PlayerId,
    cache: Option<&mut crate::ai::turn_distance::AiTurnCache>,
) -> i32 {
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
fn evaluate_board_v1(world: &mut World, perspective_player: PlayerId) -> i32 {
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

    score
}

// ==========================================
// 戦術部隊 AI 用の精緻な評価ロジック (V2)
// ==========================================
fn evaluate_board_v2(
    world: &mut World,
    perspective_player: PlayerId,
    cache: Option<&mut crate::ai::turn_distance::AiTurnCache>,
) -> i32 {
    let mut score = 0;

    let map = world.resource::<Map>().clone();
    let registry = world
        .get_resource::<MasterDataRegistry>()
        .cloned()
        .unwrap_or_default();

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

    // 1. ユニット戦力評価と SSSP テーブル構築
    let mut my_unit_distances = Vec::new();
    let mut enemy_unit_distances = Vec::new();

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
    for (_entity, faction, health, stats, pos_opt, ammo_opt, _transporting_opt, _cargo_opt) in
        query.iter(world)
    {
        let is_my_unit = faction.0 == perspective_player;

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
            let mut min_turn_dist = 99;

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
                    if turns.turns < min_turn_dist {
                        min_turn_dist = turns.turns;
                    }
                }
            }

            let isolation_modifier = if min_turn_dist > 5 {
                let is_offensive_or_transport = stats.movement_type
                    == crate::resources::MovementType::Air
                    || stats.movement_type == crate::resources::MovementType::Ship
                    || stats.max_cargo > 0;
                if is_offensive_or_transport { 1.0 } else { 0.7 }
            } else if min_turn_dist >= 3 {
                let is_offensive_or_transport = stats.movement_type
                    == crate::resources::MovementType::Air
                    || stats.movement_type == crate::resources::MovementType::Ship
                    || stats.max_cargo > 0;
                if is_offensive_or_transport { 1.0 } else { 0.85 }
            } else {
                1.0
            };
            base_value *= isolation_modifier;

            // 領域支配スコア用の情報を保存（SSSPテーブルはここでは保持しない。不要なクローンを避けるため movement_type と max_movement のみ保持）
            if is_my_unit {
                my_unit_distances.push((*pos, stats.movement_type, stats.max_movement, faction.0));
            } else {
                enemy_unit_distances.push((
                    *pos,
                    stats.movement_type,
                    stats.max_movement,
                    faction.0,
                ));
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

    // 3. 領域支配スコアの本実装 (最短到達ターン数での支配者判定)
    let mut my_dominated_count = 0;
    let mut enemy_dominated_count = 0;

    for (p_pos, _) in &properties {
        // 自軍の最短到達ターン数を計算 (拠点始点 SSSP から逆引き)
        let mut min_my_turns = 99;
        for (u_pos, u_movement_type, u_max_movement, u_faction) in &my_unit_distances {
            let p_turns_map = crate::ai::turn_distance::calculate_all_turn_distances_cached(
                &map,
                &registry,
                &unit_positions,
                (p_pos.x, p_pos.y),
                *u_movement_type,
                *u_max_movement,
                0, // interaction_max_range
                *u_faction,
                turn_cache,
            );
            if let Some(&turns) = p_turns_map.get(u_pos) {
                if turns.turns < min_my_turns {
                    min_my_turns = turns.turns;
                }
            }
        }

        // 敵軍の最短到達ターン数を計算 (拠点始点 SSSP から逆引き)
        let mut min_enemy_turns = 99;
        for (u_pos, u_movement_type, u_max_movement, u_faction) in &enemy_unit_distances {
            let p_turns_map = crate::ai::turn_distance::calculate_all_turn_distances_cached(
                &map,
                &registry,
                &unit_positions,
                (p_pos.x, p_pos.y),
                *u_movement_type,
                *u_max_movement,
                0, // interaction_max_range
                *u_faction,
                turn_cache,
            );
            if let Some(&turns) = p_turns_map.get(u_pos) {
                if turns.turns < min_enemy_turns {
                    min_enemy_turns = turns.turns;
                }
            }
        }

        if min_my_turns < min_enemy_turns {
            my_dominated_count += 1;
        } else if min_enemy_turns < min_my_turns {
            enemy_dominated_count += 1;
        }
    }

    score += (my_dominated_count - enemy_dominated_count) * TERRITORY_WEIGHT;

    score
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
}
