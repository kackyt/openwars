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
/// #49 (V3): 回復インフラの条件付き価値の重み。
/// 毀損価値 (cost × 欠損HP率) のうち、最寄り回復拠点への到達しやすさに応じて
/// 回収可能とみなす割合。収入NPVとは独立したモデルのため控えめに設定する。
const RECOVERY_INFRA_WEIGHT: f32 = 0.5;

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

    // 徒歩で十分近い場合、輸送経由 (最短でも 迎え0 + 運搬1 + 降車1 + 徒歩1 = 3T) が
    // 上回ることはほぼないため、SSSP の追加実行を省略する
    const TRANSPORT_SKIP_WALK_TURNS: u32 = 4;
    if best.is_some_and(|b| b <= TRANSPORT_SKIP_WALK_TURNS) {
        return best;
    }

    for t in transports {
        if !t.loadable.contains(&unit.unit_type) {
            continue;
        }
        if t.max_movement == 0 {
            continue;
        }
        // 降車地点の近似: ヘリ・装甲車は拠点隣接に降車して徒歩1T、
        // 輸送船は拠点から3マス以内の岸に寄せて徒歩2T
        let (drop_range, drop_walk) = match t.movement_type {
            crate::resources::MovementType::Ship => (3, 2),
            _ => (1, 1),
        };
        // 迎え: 輸送ユニット現在地 → ユニット隣接。
        // ユニット位置はビーム探索中に頻繁に変わり SSSP キャッシュが効かないため、
        // マンハッタン距離 / 移動力 の近似で済ませる（思考時間 200ms 制約対策）
        let pickup_dist = unit.pos.x.abs_diff(t.pos.x) + unit.pos.y.abs_diff(t.pos.y);
        let pickup_turns = (pickup_dist.saturating_sub(1) as u32).div_ceil(t.max_movement);
        // 運搬: ユニット位置 → 拠点近傍 (drop_range 以内)。拠点起点なのでキャッシュが効く
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
        let eta = pickup_turns + carry.turns + 1 + drop_walk;
        if best.is_none_or(|b| eta < b) {
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
                if best_eta.is_none_or(|b| eta < b) {
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

/// AI の主観評価値（探索に使うスコア）の内訳。
/// AI バージョンごとに定義が異なるため、バージョン間の比較には ObjectiveMetrics を使うこと。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BoardMetrics {
    pub total_score: i32,
    /// ユニット価値スコア (cost × HP残率 ± 各種補正)
    pub unit_score: i32,
    /// 拠点価値スコア
    pub property_score: i32,
    /// 領域支配スコア (V1: 拠点数ベース, V2: ZOCマス数ベース)
    pub territory_score: i32,
    /// NPV スコア (V2 のみ。my_npv − enemy_npv)
    pub npv_score: i32,
    /// #49 (V3 のみ): 回復インフラの条件付き価値 (my − enemy)
    pub recovery_score: i32,
    pub my_dominated_count: i32,
    pub enemy_dominated_count: i32,
}

/// AI バージョンに依存しない客観的な盤面計測値。
/// V1 / V2 を同じ物差しで比較するための検証用メトリクス。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ObjectiveMetrics {
    /// ZOC 方式の支配面積 (ユニットのマス + 隣接マス + 占領済み拠点)
    pub zoc_area: i32,
    /// 所有拠点数
    pub owned_properties: i32,
    /// 1ターンあたりの収入合計
    pub income_per_turn: i32,
    /// 未占領・敵拠点の獲得機会価値 (NPV) 合計
    pub npv: i32,
    /// 開始からの累計与ダメージ価値 (ゴールド換算)
    pub combat_value_dealt: i64,
    /// 開始からの累計被ダメージ価値 (ゴールド換算)
    pub combat_value_received: i64,
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
        // V3 は V2 の評価をベースに、#49 の回復インフラ評価項を追加する
        AiVersion::V2 | AiVersion::V3 => evaluate_board_v2(
            world,
            perspective_player,
            cache,
            ai_version.uses_v3_tactics(),
        ),
    }
}

// ==========================================
// 従来型 AI 用の簡易評価ロジック (V1)
// ==========================================
pub fn evaluate_board_v1(world: &mut World, perspective_player: PlayerId) -> BoardMetrics {
    let mut unit_score = 0;
    let mut property_score = 0;

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
            unit_score += value;
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
            unit_score -= value;
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
                property_score += prop_value;
            } else {
                property_score -= prop_value;
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
    let territory_score = (my_territory - enemy_territory) * TERRITORY_WEIGHT;

    BoardMetrics {
        total_score: unit_score + property_score + territory_score,
        unit_score,
        property_score,
        territory_score,
        npv_score: 0,
        recovery_score: 0,
        my_dominated_count: my_territory,
        enemy_dominated_count: enemy_territory,
    }
}

/// #49 (V3): 損傷ユニット1体分の回復インフラ条件付き価値。
/// 毀損価値 (cost × 欠損HP率) を、最寄りの回復可能な自軍拠点への
/// 到達ターン数 (ETA, マンハッタン距離/移動力の近似) で割り引く。
/// 回復拠点が存在しなければ価値は 0 (インフラを失うと損傷が回収不能になる)。
struct DamagedUnitInfo {
    pos: GridPosition,
    unit_type: crate::resources::UnitType,
    max_movement: u32,
    faction: PlayerId,
    /// cost × 欠損HP率 (ゴールド換算の毀損価値)
    lost_value: i32,
}

fn side_recovery_infra_value(
    registry: &MasterDataRegistry,
    properties: &[(GridPosition, Property)],
    damaged_units: &[DamagedUnitInfo],
) -> i32 {
    let mut total = 0i32;
    for u in damaged_units {
        // 最寄りの「このユニットを回復できる」自軍拠点までの距離
        let mut best_dist: Option<u32> = None;
        for (p_pos, prop) in properties {
            if prop.owner_id != Some(u.faction) {
                continue;
            }
            if !registry.can_repair_on_terrain(u.unit_type, prop.terrain) {
                continue;
            }
            let d = (p_pos.x.abs_diff(u.pos.x) + p_pos.y.abs_diff(u.pos.y)) as u32;
            if best_dist.is_none_or(|b| d < b) {
                best_dist = Some(d);
            }
        }
        let Some(dist) = best_dist else {
            continue;
        };
        // ETA = 距離 / 移動力 (切り上げ)。移動力 0 のユニットはその場から動けないため対象外
        if u.max_movement == 0 {
            continue;
        }
        let eta = dist.div_ceil(u.max_movement);
        total += (u.lost_value as f32 * RECOVERY_INFRA_WEIGHT / (eta + 1) as f32) as i32;
    }
    total
}

// ==========================================
// 戦術部隊 AI 用の精緻な評価ロジック (V2 / V3)
// ==========================================
fn evaluate_board_v2(
    world: &mut World,
    perspective_player: PlayerId,
    cache: Option<&mut crate::ai::turn_distance::AiTurnCache>,
    // V3 のみ true。#49 の回復インフラ評価項を追加する
    with_recovery_infra: bool,
) -> BoardMetrics {
    let mut unit_score = 0;
    let mut property_score = 0;

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
    // #49 (V3): 回復インフラ評価用の損傷ユニット収集
    let mut my_damaged_units: Vec<DamagedUnitInfo> = Vec::new();
    let mut enemy_damaged_units: Vec<DamagedUnitInfo> = Vec::new();

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
                    if min_turn_dist.is_none_or(|m| turns < m) {
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

            // #49 (V3): 損傷しているユニットを回復インフラ評価の対象として収集
            if with_recovery_infra && health.current < health.max && health.max > 0 {
                let lost_value = (stats.cost as f32 * (health.max - health.current) as f32
                    / health.max as f32) as i32;
                let info = DamagedUnitInfo {
                    pos: *pos,
                    unit_type: stats.unit_type,
                    max_movement: stats.max_movement,
                    faction: faction.0,
                    lost_value,
                };
                if is_my_unit {
                    my_damaged_units.push(info);
                } else {
                    enemy_damaged_units.push(info);
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
            unit_score += value;
        } else {
            unit_score -= value;
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
                property_score += prop_value;
            } else {
                property_score -= prop_value;
            }
        }
    }

    // 3. 領域支配スコア (ZOC 方式)
    // 支配領域 = ユニットのいるマス + その隣接マス (ZOC) + 占領済み拠点のマス
    let mut my_cells: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut enemy_cells: std::collections::HashSet<(usize, usize)> =
        std::collections::HashSet::new();

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

    let territory_score = (my_dominated_count - enemy_dominated_count) * ZOC_TERRITORY_WEIGHT;

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
    let npv_contribution = (npv_score as f32 * NPV_WEIGHT) as i32;

    // 5. #49 (V3): 回復インフラの条件付き価値。
    // 損傷ユニットが多く、かつ回復拠点が近いほど価値が高い。
    // 敵側も同様に評価し、差分をスコアへ加算する。
    let recovery_score = if with_recovery_infra {
        side_recovery_infra_value(&registry, &properties, &my_damaged_units)
            - side_recovery_infra_value(&registry, &properties, &enemy_damaged_units)
    } else {
        0
    };

    BoardMetrics {
        total_score: unit_score
            + property_score
            + territory_score
            + npv_contribution
            + recovery_score,
        unit_score,
        property_score,
        territory_score,
        npv_score,
        recovery_score,
        my_dominated_count,
        enemy_dominated_count,
    }
}

/// AI バージョンに依存しない客観メトリクスを計算する。
/// V1 / V2 を同じ物差しで比較する検証・分析用であり、AI の探索評価には使用しない
/// （毎ターン1回程度の呼び出しを想定）。
pub fn compute_objective_metrics(world: &mut World, player: PlayerId) -> ObjectiveMetrics {
    let map = world.resource::<Map>().clone();
    let registry = world.resource::<MasterDataRegistry>().clone();
    let current_turn = world
        .get_resource::<crate::resources::MatchState>()
        .map(|ms| ms.current_turn_number.0)
        .unwrap_or(1);

    // ユニット走査: 占有情報・ZOC・占領可能ユニット・輸送ユニットの収集
    let mut unit_positions = HashMap::new();
    let mut my_positions: Vec<GridPosition> = Vec::new();
    let mut capture_units: Vec<CaptureUnitInfo> = Vec::new();
    let mut transports: Vec<TransportInfo> = Vec::new();

    let mut q = world.query::<(
        &Faction,
        &GridPosition,
        &UnitStats,
        Option<&crate::components::Transporting>,
        Option<&crate::components::CargoCapacity>,
    )>();
    for (faction, pos, stats, transporting_opt, cargo_opt) in q.iter(world) {
        // 輸送中ユニットは盤外座標 (x=9999) のため除外
        if transporting_opt.is_some() {
            continue;
        }
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
        if faction.0 != player {
            continue;
        }
        my_positions.push(*pos);
        if stats.can_capture {
            capture_units.push(CaptureUnitInfo {
                pos: *pos,
                movement_type: stats.movement_type,
                max_movement: stats.max_movement,
                faction: faction.0,
                unit_type: stats.unit_type,
            });
        }
        if stats.max_cargo > 0 {
            let free_slots = cargo_opt
                .map(|c| c.max.saturating_sub(c.loaded.len() as u32))
                .unwrap_or(stats.max_cargo);
            if free_slots > 0 {
                transports.push(TransportInfo {
                    pos: *pos,
                    movement_type: stats.movement_type,
                    max_movement: stats.max_movement,
                    faction: faction.0,
                    loadable: stats.loadable_unit_types.clone(),
                });
            }
        }
    }

    // ZOC 支配面積 (ユニットのマス + 隣接マス)
    let mut cells: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    for pos in &my_positions {
        cells.insert((pos.x, pos.y));
        for adj in map.get_adjacent(pos.x, pos.y) {
            cells.insert(adj);
        }
    }

    // 拠点: 所有数・収入・支配セル追加
    let mut properties = Vec::new();
    let mut owned_properties = 0;
    let mut income_per_turn = 0i32;
    let mut prop_query = world.query::<(&GridPosition, &Property)>();
    for (pos, prop) in prop_query.iter(world) {
        properties.push((*pos, prop.clone()));
        if prop.owner_id == Some(player) {
            owned_properties += 1;
            income_per_turn += registry.landscape_income(prop.terrain.as_str()) as i32;
            cells.insert((pos.x, pos.y));
        }
    }

    // NPV (獲得機会価値)
    let mut cache = crate::ai::turn_distance::AiTurnCache::default();
    let npv = side_npv(
        &map,
        &registry,
        &unit_positions,
        &properties,
        player,
        &capture_units,
        &transports,
        current_turn,
        expected_end_turn(&map),
        &mut cache,
    );

    // 戦闘損益 (累計、ゴールド換算)
    let record = world
        .get_resource::<crate::resources::CombatLedger>()
        .and_then(|l| l.records.get(&player).copied())
        .unwrap_or_default();

    ObjectiveMetrics {
        zoc_area: cells.len() as i32,
        owned_properties,
        income_per_turn,
        npv,
        combat_value_dealt: record.value_dealt,
        combat_value_received: record.value_received,
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
        let mut map = Map::new(
            10,
            10,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        // 全て平原にしておく
        for x in 0..10 {
            for y in 0..10 {
                let _ = map.set_terrain(x, y, Terrain::Plains);
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
            Health {
                current: 100,
                max: 100,
            },
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
            Health {
                current: 100,
                max: 100,
            },
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
            Health {
                current: 100,
                max: 100,
            },
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
        assert_eq!(
            metrics.enemy_dominated_count, 10,
            "P2 should dominate 10 cells"
        );
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
        assert!(
            eta_with_heli < walk_eta,
            "輸送ヘリ活用でETAが短縮されること"
        );
    }

    /// Issue #49: V3 では損傷ユニットの近くに回復インフラ (首都等) があるほど
    /// recovery_score が高く評価され、V2 では常に 0 であることを検証する
    #[test]
    fn test_v3_recovery_infra_value() {
        // capital_x: 回復拠点 (首都) の x 座標
        let build_metrics = |version: AiVersion, capital_x: usize| -> BoardMetrics {
            let mut world = World::new();
            let p1 = PlayerId(1);
            let p2 = PlayerId(2);

            let mut settings = PlayerAiSettings::new();
            settings.set_version(p1, version);
            settings.set_version(p2, version);
            world.insert_resource(settings);

            let map = Map::new(
                12,
                1,
                Terrain::Plains,
                crate::resources::GridTopology::Square,
            );
            world.insert_resource(map);
            world.insert_resource(crate::resources::MasterDataRegistry::load().unwrap());
            world.insert_resource(crate::resources::MatchState::default());

            // 損傷した自軍戦車 (cost 6000, HP 50/100, 移動4) at x=0
            world.spawn((
                Faction(p1),
                GridPosition { x: 0, y: 0 },
                Health {
                    current: 50,
                    max: 100,
                },
                UnitStats {
                    unit_type: crate::resources::UnitType::Tank,
                    cost: 6000,
                    max_movement: 4,
                    movement_type: crate::resources::MovementType::Tank,
                    ..UnitStats::mock()
                },
            ));

            // 自軍の回復拠点 (首都: 地上部隊を補給・回復できる)
            world.spawn((
                GridPosition { x: capital_x, y: 0 },
                Property::new(Terrain::Capital, Some(p1), 200),
            ));

            evaluate_board_with_metrics(&mut world, p1, None)
        };

        // V2: 回復インフラ評価は常に 0
        let v2 = build_metrics(AiVersion::V2, 1);
        assert_eq!(v2.recovery_score, 0, "V2 では recovery_score は 0");

        // V3: 回復拠点が近い (x=1, ETA 1) ほうが遠い (x=11, ETA 3) より高評価
        let v3_near = build_metrics(AiVersion::V3, 1);
        let v3_far = build_metrics(AiVersion::V3, 11);
        assert!(
            v3_near.recovery_score > 0,
            "損傷ユニット + 回復拠点があれば正の価値 (actual: {})",
            v3_near.recovery_score
        );
        assert!(
            v3_near.recovery_score > v3_far.recovery_score,
            "回復拠点が近いほど価値が高い (near: {}, far: {})",
            v3_near.recovery_score,
            v3_far.recovery_score
        );
    }
}
