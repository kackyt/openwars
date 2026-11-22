//! 敵脅威の位置投影・有界非対称重み・間接射点の安全評価。
//!
//! Stage 1 (V9) で 5 マップ横断検証した物理モデル（1T/2T 敵移動投影、
//! 過去3ターン実生産に基づく2波増援、対車両反撃力ベースの2ターン突破リスク、
//! 前線近接施設ブロックペナルティ、固定砲台の永久封鎖コスト）を提供する。

use super::reach::{
    OCC_ENEMY, OCC_FRIENDLY, OccupancyGrid, ReachMap, reachable_pierce, reachable_tiles,
    shortest_dist_field,
};
use super::{UnifiedBoardExtras, WeaponProfileTable};
use crate::ai::v4::{BoardScan, UnitSnapshot};
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, UnitStats};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Map, MovementType, Terrain, UnitType};
use crate::systems::combat::calculate_damage_formula;
use bevy_ecs::prelude::Entity;

/// 評価ホライズン（ラウンド数）。
pub(super) const HORIZON: usize = 6;
/// ホライズン内の各ラウンド時間割引。
pub(super) const ROUND_DISCOUNT: f32 = 0.92;

/// 仮想第1波（敵最近生産傾向）の到着ラウンド。
const VIRTUAL_WAVE1_ROUND: usize = 2;
/// 仮想第2波（敵最近生産傾向）の到着ラウンド。
const VIRTUAL_WAVE2_ROUND: usize = 4;

/// 1T以内に敵直接部隊が隣接可能な場合の ZOC 拘束最低リスク。
const ZOC_LOCK_MIN_RISK: f32 = 55.0;
/// 敵前線投影位置から距離1以内の射点へのペナルティ。
const FRONT_ADJ_PENALTY: f32 = 55.0;
/// 敵前線投影位置から距離2かつ非補給マスへのペナルティ。
const FRONT_NEAR_NON_SUPPLY_PENALTY: f32 = 35.0;
/// 遮蔽された 2T 到達重脅威の係数。
const BREACH_BLOCKED_WEIGHT: f32 = 0.35;
/// 薄い護衛を突破する未遮蔽 2T 到達重脅威の係数。
const BREACH_PIERCE_WEIGHT: f32 = 0.55;
/// 突破リスク計算で「重攻撃」とみなす最小基礎ダメージ。
const BREACH_MIN_BASE_DAMAGE: u32 = 20;
/// 突破リスク計算で無視しない最小実ダメージ。
const BREACH_MIN_ACTUAL_DAMAGE: u32 = 25;
/// 局所護衛としてフル評価する味方直接ユニットの最小対敵基礎ダメージ。
const SCREEN_STRONG_COUNTER_BASE_DAMAGE: u32 = 25;
/// 対敵基礎ダメージが弱い歩兵等が持つ最小遮蔽寄与倍率。
const SCREEN_SOFT_WEIGHT: f32 = 0.18;
/// 局所護衛 HP 合計の正規化分母。
const SCREEN_FULL_COVERAGE_HP: f32 = 220.0;
/// 厚い護衛下でも残す最小突破漏れ率。
const MIN_SCREEN_LEAK: f32 = 0.10;
/// 敵間接による対砲兵リスク係数。
const COUNTER_BATTERY_WEIGHT: f32 = 0.30;
/// 非補給マスの生存率補正。
const NON_SUPPLY_SURV_MULT: f32 = 0.70;
/// 固定砲台の柔軟性（再配置不能）割引。
const IMMOBILE_SURV_DISCOUNT: f32 = 0.78;
/// 施設占有ペナルティの基準値。
const FACILITY_BLOCK_BASE: f32 = 75.0;
/// 固定砲台（生産口永久封鎖）の追加ホライズン乗数。
const IMMOBILE_BLOCK_HORIZON_WEIGHT: f32 = 2.2;
/// 移動不可ユニット判定の最大燃料閾値（燃料1の砲台等）。
const IMMOBILE_MAX_FUEL_THRESHOLD: u32 = 2;
/// 自軍ユニット不在時に代表被ダメージ計算へ使う基準地形防御（都市相当）。
const DEFAULT_REF_TERRAIN_DEF: u32 = 15;
/// 自軍ユニット不在時のフォールバック代表与ダメージ。
const INITIAL_EMPTY_ARMY_FALLBACK_AEFF: f32 = 25.0;
/// 自軍ユニット不在時の初期地上直接敵に対する固定非対称倍率。
const INITIAL_EMPTY_ARMY_GROUND_ASYM: f32 = 1.25;
/// 既存味方が敵脅威を質的に抑えられる（強いカウンター役）とみなす最小期待ダメージ。
const STRONG_COUNTER_MIN_DMG: f32 = 35.0;
/// 中立/自軍物件上に居座る敵占領ユニットの占領妨害優先ボーナス。
const CAPTURE_DENIAL_ON_PROP_BONUS: f32 = 55.0;
/// 1ターン以内に中立/自軍物件へ到達可能な敵占領ユニットの占領妨害ボーナス。
const CAPTURE_DENIAL_NEAR_PROP_BONUS: f32 = 35.0;
/// 対空カウンター 1 体が抑止できる敵航空戦力の HP 換算倍率。
const AIR_COUNTER_CAPACITY_PER_UNIT: f32 = 1.5;
/// 航空敵に対する基準非対称倍率。
const AIR_ASYM_BASE: f32 = 1.1;
/// 対空不足時の追加非対称倍率幅（最大 3.0）。
const AIR_ASYM_SPAN: f32 = 1.9;
/// 対空充足とみなすカバー率閾値。
const AIR_COV_SATURATED_THRESHOLD: f32 = 0.9;
/// 間接ユニット前衛護衛比の敵直接部隊数下限基準。
const INDIRECT_SCREEN_MIN_ENEMY_REF: usize = 4;
/// 敵直接部隊に対する十分な味方前衛直接部隊の目標比率。
const INDIRECT_SCREEN_RATIO: f32 = 1.1;
/// 前衛不足時の間接射点生存率補正の下限。
const INDIRECT_MIN_SCREEN_SUPPORT: f32 = 0.55;

/// 投影済み・重み付け済みの敵ターゲット 1 体。
#[derive(Debug, Clone)]
pub(super) struct ProjectedEnemy {
    pub(super) entity: Option<Entity>,
    pub(super) stats: UnitStats,
    pub(super) pos0: GridPosition,
    pub(super) pos_proj: GridPosition,
    pub(super) def0: u32,
    pub(super) def_proj: u32,
    pub(super) hp: u32,
    pub(super) ammo: (u32, u32),
    pub(super) w_per_hp: f32,
    pub(super) virtual_wave: bool,
    pub(super) arr_rnd: usize,
}

/// 実在敵ごとの事前計算済み到達情報（安全評価で再利用）。
struct LiveEnemyReach {
    unit: UnitSnapshot,
    ammo: (u32, u32),
    reach_1t: ReachMap,
    seen_2t_blk: Vec<bool>,
    seen_2t_prc: Vec<bool>,
}

/// 盤面の脅威・施設重み・安全性を保持するコンテキスト。
pub(super) struct ThreatContext {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) occ: OccupancyGrid,
    pub(super) supply_for_unit_domain: Vec<bool>,
    pub(super) ground_facility_set: HashSet<GridPosition>,
    pub(super) non_ground_facility_set: HashSet<GridPosition>,
    pub(super) free_ground_fac_count: usize,
    pub(super) enemy_targets: Vec<ProjectedEnemy>,
    pub(super) f_direct_units: Vec<UnitSnapshot>,
    pub(super) e_direct_cnt: usize,
    pub(super) can_reach_enemy_via_land: bool,
    pub(super) has_reachable_land_objective: bool,
    live_reaches: Vec<LiveEnemyReach>,
    enemy_1t_direct_adj: Vec<bool>,
    proj_direct_enemy_tiles: Vec<GridPosition>,
    fac_front_dists: HashMap<GridPosition, u32>,
    fac_dist_min: u32,
    fac_dist_max: u32,
}

impl ThreatContext {
    /// 盤面スキャンと直近の観測済み敵生産履歴から `ThreatContext` を構築する。
    pub(super) fn build(
        scan: &BoardScan,
        extras: &UnifiedBoardExtras,
        profiles: &WeaponProfileTable,
        recent_enemy: &[(u32, UnitType)],
    ) -> Self {
        let map = &scan.map;
        let master = &scan.master_data;
        let chart = &scan.damage_chart;
        let width = map.width;
        let height = map.height;
        let grid_len = width.saturating_mul(height);

        let occ = OccupancyGrid::from_units(
            map,
            scan.my_units.iter().map(|u| u.pos),
            scan.enemy_units.iter().map(|u| u.pos),
        );

        let mut is_ground_supply = vec![false; grid_len];
        for &(pos, terrain) in &extras.friendly_properties {
            if pos.x < width
                && pos.y < height
                && let Some(record) = master.get_landscape_by_name(terrain.as_str())
                && record.supply_type.as_deref() == Some("地上部隊")
            {
                is_ground_supply[pos.y * width + pos.x] = true;
            }
        }

        let ground_facility_set: HashSet<GridPosition> = scan
            .production_facilities
            .iter()
            .filter(|(_, terrain)| {
                master
                    .get_landscape_by_name(terrain.as_str())
                    .and_then(|r| r.supply_type.as_deref())
                    == Some("地上部隊")
            })
            .map(|(pos, _)| *pos)
            .collect();
        let non_ground_facility_set: HashSet<GridPosition> = scan
            .production_facilities
            .iter()
            .filter(|(_, terrain)| {
                master
                    .get_landscape_by_name(terrain.as_str())
                    .and_then(|r| r.supply_type.as_deref())
                    .is_some_and(|s| s != "地上部隊")
            })
            .map(|(pos, _)| *pos)
            .collect();
        let free_ground_fac_count = scan
            .free_facilities
            .iter()
            .filter(|(pos, _)| ground_facility_set.contains(pos))
            .count()
            .max(1);

        let f_direct_units: Vec<UnitSnapshot> = scan
            .my_units
            .iter()
            .filter(|u| !profiles.is_indirect(u.stats.unit_type))
            .cloned()
            .collect();
        let f_positions: Vec<GridPosition> = scan.my_units.iter().map(|u| u.pos).collect();
        let mut direct_goals: Vec<GridPosition> = Vec::new();
        let mut goal_seen = vec![false; grid_len];

        let mut goal_seeds: Vec<GridPosition> = f_direct_units.iter().map(|u| u.pos).collect();
        if goal_seeds.is_empty() {
            goal_seeds.extend(f_positions.iter().copied());
        }
        goal_seeds.extend(extras.friendly_properties.iter().map(|(p, _)| *p));
        for g in goal_seeds {
            for (nx, ny) in map.get_adjacent(g.x, g.y) {
                let npos = GridPosition { x: nx, y: ny };
                let idx = ny * width + nx;
                if !goal_seen[idx] && occ.at(npos) != OCC_FRIENDLY {
                    goal_seen[idx] = true;
                    direct_goals.push(npos);
                }
            }
        }

        let move_types = [
            MovementType::Infantry,
            MovementType::Tank,
            MovementType::ArmoredCar,
            MovementType::Artillery,
            MovementType::Air,
            MovementType::Ship,
        ];
        let mut dist_fields: HashMap<MovementType, ReachMap> = HashMap::default();
        for mt in move_types {
            dist_fields.insert(
                mt,
                shortest_dist_field(map, master, &occ, &direct_goals, mt, OCC_ENEMY),
            );
        }

        // 陸続き判定（生産施設から敵・未所有物件へ地上移動タイプで到達可能か）
        let mut connectivity = crate::ai::turn_distance::TerrainConnectivity::default();
        let can_reach_enemy_via_land = scan.production_facilities.iter().any(|(fac, terrain)| {
            scan.enemy_units.iter().any(|enemy| {
                !matches!(
                    enemy.stats.movement_type,
                    MovementType::Air | MovementType::Ship
                ) && scan.available_types.iter().any(|(ut, stats)| {
                    !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
                        && scan.can_produce(*terrain, *ut)
                        && connectivity.is_reachable(
                            map,
                            master,
                            (fac.x, fac.y),
                            (enemy.pos.x, enemy.pos.y),
                            stats.movement_type,
                        )
                })
            })
        });
        let has_reachable_land_objective =
            scan.production_facilities.iter().any(|(fac, terrain)| {
                scan.open_properties.iter().any(|prop| {
                    scan.available_types.iter().any(|(ut, stats)| {
                        !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
                            && scan.can_produce(*terrain, *ut)
                            && connectivity.is_reachable(
                                map,
                                master,
                                (fac.x, fac.y),
                                (prop.x, prop.y),
                                stats.movement_type,
                            )
                    })
                })
            });

        // 過去3ターンの敵生産履歴から仮想2波を構築する
        let stats_by_type: HashMap<UnitType, &UnitStats> = scan
            .available_types
            .iter()
            .map(|(ut, s)| (*ut, s))
            .collect();
        let mut enemy_items: Vec<(UnitSnapshot, (u32, u32), bool, usize)> = scan
            .enemy_units
            .iter()
            .map(|u| {
                let ammo = u
                    .entity
                    .and_then(|e| extras.unit_ammo.get(&e).copied())
                    .unwrap_or((u.stats.max_ammo1, u.stats.max_ammo2));
                (u.clone(), ammo, false, 0)
            })
            .collect();

        if !recent_enemy.is_empty() && !scan.enemy_facilities.is_empty() {
            let distinct_turns: HashSet<u32> = recent_enemy.iter().map(|(t, _)| *t).collect();
            let n_win = distinct_turns.len().clamp(1, 3) as f32;
            let mut counts: HashMap<UnitType, usize> = HashMap::default();
            for &(_, ut) in recent_enemy {
                *counts.entry(ut).or_insert(0) += 1;
            }
            let mut sorted_counts: Vec<(UnitType, usize)> = counts.into_iter().collect();
            sorted_counts.sort_unstable_by(|a, b| {
                b.1.cmp(&a.1).then_with(|| a.0.as_str().cmp(b.0.as_str()))
            });

            let mut sorted_enemy_facs = scan.enemy_facilities.clone();
            sorted_enemy_facs.sort_unstable_by_key(|f| (f.pos.y, f.pos.x));

            for (wave_idx, arr_rnd) in [
                (1_usize, VIRTUAL_WAVE1_ROUND),
                (2_usize, VIRTUAL_WAVE2_ROUND),
            ] {
                let mut fac_offset = wave_idx;
                for &(ut, cnt) in &sorted_counts {
                    let Some(stats) = stats_by_type.get(&ut).copied() else {
                        continue;
                    };
                    if stats.movement_type == MovementType::Ship {
                        continue;
                    }
                    let matching_facs: Vec<GridPosition> = sorted_enemy_facs
                        .iter()
                        .filter(|fac| master.can_produce_unit(fac.terrain.as_str(), ut))
                        .map(|fac| fac.pos)
                        .collect();
                    if matching_facs.is_empty() {
                        continue;
                    }
                    let avg = cnt as f32 / n_win;
                    let n_spawn = if wave_idx == 1 {
                        (avg + 0.45) as usize
                    } else {
                        avg as usize
                    };
                    for _ in 0..n_spawn.min(matching_facs.len()) {
                        let fac_pos = matching_facs[fac_offset % matching_facs.len()];
                        fac_offset += 1;
                        let snap = UnitSnapshot {
                            entity: None,
                            pos: fac_pos,
                            stats: (*stats).clone(),
                            hp: 100,
                            free_cargo: stats.max_cargo,
                        };
                        enemy_items.push((snap, (stats.max_ammo1, stats.max_ammo2), true, arr_rnd));
                    }
                }
            }
        }

        // 決定的な並び順（実在優先・到着順・距離場・X/Y座標）
        enemy_items.sort_by(|a, b| {
            let da = dist_fields
                .get(&a.0.stats.movement_type)
                .and_then(|df| df.get(a.0.pos))
                .unwrap_or(999);
            let db = dist_fields
                .get(&b.0.stats.movement_type)
                .and_then(|df| df.get(b.0.pos))
                .unwrap_or(999);
            (a.2, a.3, da, a.0.pos.x, a.0.pos.y).cmp(&(b.2, b.3, db, b.0.pos.x, b.0.pos.y))
        });

        let combat_air_hp_equiv: f32 = enemy_items
            .iter()
            .filter(|(u, _, virt, _)| {
                !*virt
                    && u.stats.movement_type == MovementType::Air
                    && max_base_damage(chart, u.stats.unit_type, UnitType::Infantry) >= 30
            })
            .map(|(u, _, _, _)| u.hp as f32 / 100.0)
            .sum();

        let e_direct_cnt = scan
            .enemy_units
            .iter()
            .filter(|u| !profiles.is_indirect(u.stats.unit_type))
            .count();

        let mut claimed_proj = vec![false; grid_len];
        // 自軍ユニット不在時に代表被ダメージ計算へ使う生産可能地上兵種リストを事前抽出
        let initial_ref_types: Vec<UnitType> = if scan.my_units.is_empty() {
            let mut refs: Vec<UnitType> = scan
                .available_types
                .iter()
                .filter(|(cand_ut, st)| {
                    !profiles.is_indirect(*cand_ut)
                        && (st.max_cargo == 0
                            || max_base_damage(chart, *cand_ut, UnitType::Infantry) > 0
                            || max_base_damage(chart, *cand_ut, UnitType::Tank) > 0)
                        && !matches!(st.movement_type, MovementType::Air | MovementType::Ship)
                        && scan
                            .production_facilities
                            .iter()
                            .any(|(_, terrain)| scan.can_produce(*terrain, *cand_ut))
                })
                .map(|(cand_ut, _)| *cand_ut)
                .collect();
            if refs.is_empty() {
                refs = scan.available_types.iter().map(|(ut, _)| *ut).collect();
            }
            refs
        } else {
            Vec::new()
        };
        let mut enemy_targets = Vec::with_capacity(enemy_items.len());
        let mut live_reaches = Vec::new();
        let mut enemy_1t_direct_adj = vec![false; grid_len];

        for (u, ammo, virtual_wave, arr_rnd) in enemy_items {
            let ut = u.stats.unit_type;
            let pos = u.pos;
            let is_ind = profiles.is_indirect(ut);
            let is_air = u.stats.movement_type == MovementType::Air;
            let (rmin, rmax) = (u.stats.min_range, u.stats.max_range);

            let r1 = reachable_tiles(
                map,
                master,
                &occ,
                pos,
                u.stats.movement_type,
                u.stats.max_movement,
                u.stats.max_fuel,
                OCC_ENEMY,
                1,
                None,
            );
            let r2 = reachable_tiles(
                map,
                master,
                &occ,
                pos,
                u.stats.movement_type,
                u.stats.max_movement,
                u.stats.max_fuel,
                OCC_ENEMY,
                2,
                None,
            );

            if !virtual_wave {
                if !is_ind && profiles.has_any_weapon(ut) {
                    for (p, _) in r1.iter() {
                        for (nx, ny) in map.get_adjacent(p.x, p.y) {
                            enemy_1t_direct_adj[ny * width + nx] = true;
                        }
                    }
                }
                let r2p = reachable_pierce(
                    map,
                    master,
                    pos,
                    u.stats.movement_type,
                    u.stats.max_movement,
                    u.stats.max_fuel,
                    2,
                );
                let mut seen_2t_blk = vec![false; grid_len];
                for (p, _) in r2.iter() {
                    for (nx, ny) in map.get_adjacent(p.x, p.y) {
                        seen_2t_blk[ny * width + nx] = true;
                    }
                }
                let mut seen_2t_prc = vec![false; grid_len];
                for (p, _) in r2p.iter() {
                    for (nx, ny) in map.get_adjacent(p.x, p.y) {
                        seen_2t_prc[ny * width + nx] = true;
                    }
                }
                live_reaches.push(LiveEnemyReach {
                    unit: u.clone(),
                    ammo,
                    reach_1t: r1.clone(),
                    seen_2t_blk,
                    seen_2t_prc,
                });
            }

            let df = &dist_fields[&u.stats.movement_type];
            let pos_proj = if is_air {
                pos
            } else if is_ind {
                if !virtual_wave
                    && f_positions.iter().any(|fp| {
                        let d = map.distance(pos.x, pos.y, fp.x, fp.y);
                        d >= rmin && d <= rmax
                    })
                {
                    pos
                } else {
                    let mut best: Option<(u32, u8, i32, i32, i32, GridPosition)> = None;
                    for (p, _) in r2.iter() {
                        let hits = f_positions
                            .iter()
                            .filter(|fp| {
                                let d = map.distance(p.x, p.y, fp.x, fp.y);
                                d >= rmin && d <= rmax
                            })
                            .count() as i32;
                        if hits > 0 {
                            let min_fd = f_positions
                                .iter()
                                .map(|fp| map.distance(p.x, p.y, fp.x, fp.y))
                                .min()
                                .unwrap_or(9);
                            let d_move = map.distance(p.x, p.y, pos.x, pos.y);
                            let taken = u8::from(claimed_proj[p.y * width + p.x]);
                            let tdef = terrain_def_at(map, master, p) as i32;
                            let cand = (d_move, taken, -i32::from(min_fd >= 2), -tdef, -hits, p);
                            if best.is_none_or(|b| {
                                (cand.0, cand.1, cand.2, cand.3, cand.4, cand.5.x, cand.5.y)
                                    < (b.0, b.1, b.2, b.3, b.4, b.5.x, b.5.y)
                            }) {
                                best = Some(cand);
                            }
                        }
                    }
                    best.map(|b| b.5).unwrap_or_else(|| {
                        r2.iter()
                            .min_by_key(|&(p, _)| {
                                (
                                    u8::from(claimed_proj[p.y * width + p.x]),
                                    df.get(p).unwrap_or(100_000),
                                    map.distance(p.x, p.y, pos.x, pos.y),
                                    p.y,
                                    p.x,
                                )
                            })
                            .map_or(pos, |(p, _)| p)
                    })
                }
            } else if !virtual_wave
                && f_positions
                    .iter()
                    .any(|fp| map.distance(pos.x, pos.y, fp.x, fp.y) <= 1)
            {
                pos
            } else {
                let r1_hits_goal = r1.iter().any(|(p, _)| df.get(p) == Some(0));
                let pool = if r1_hits_goal { &r1 } else { &r2 };
                pool.iter()
                    .min_by_key(|&(p, _)| {
                        let base_d = df.get(p).unwrap_or(100_000);
                        let claim_pen = if claimed_proj[p.y * width + p.x] {
                            3
                        } else {
                            0
                        };
                        let tdef = terrain_def_at(map, master, p) as i32;
                        (
                            base_d.saturating_add(claim_pen),
                            map.distance(p.x, p.y, pos.x, pos.y),
                            -tdef,
                            p.y,
                            p.x,
                        )
                    })
                    .map_or(pos, |(p, _)| p)
            };

            if !virtual_wave && pos_proj.x < width && pos_proj.y < height {
                claimed_proj[pos_proj.y * width + pos_proj.x] = true;
            }
            let e_def = terrain_def_at(map, master, pos_proj);
            let e_def0 = terrain_def_at(map, master, pos);

            // 味方ユニット群に対する代表与ダメージ a_eff（上位半分平均）
            let mut a_eff = if scan.my_units.is_empty() {
                if initial_ref_types.is_empty() {
                    INITIAL_EMPTY_ARMY_FALLBACK_AEFF
                } else {
                    let mut a_vals: Vec<u32> = initial_ref_types
                        .iter()
                        .map(|cand_ut| {
                            let bd = max_base_damage(chart, ut, *cand_ut);
                            calculate_damage_formula(bd, 100, DEFAULT_REF_TERRAIN_DEF, false)
                        })
                        .collect();
                    a_vals.sort_unstable_by(|x, y| y.cmp(x));
                    let k_half = a_vals.len().div_ceil(2);
                    let sum: u32 = a_vals.iter().take(k_half.max(1)).sum();
                    sum as f32 / k_half.max(1) as f32
                }
            } else {
                let mut a_vals: Vec<u32> = scan
                    .my_units
                    .iter()
                    .map(|f| {
                        let bd = max_base_damage(chart, ut, f.stats.unit_type);
                        let fdef = terrain_def_at(map, master, f.pos);
                        calculate_damage_formula(bd, 100, fdef, false)
                    })
                    .collect();
                a_vals.sort_unstable_by(|x, y| y.cmp(x));
                let k_half = a_vals.len().div_ceil(2);
                let sum: u32 = a_vals.iter().take(k_half.max(1)).sum();
                sum as f32 / k_half.max(1) as f32
            };

            // 既存味方による質的カウンター力（航空敵には間接対空ユニットも含めて集計）
            let counter_pool = if is_air {
                scan.my_units.as_slice()
            } else {
                f_direct_units.as_slice()
            };
            let (best_c, strong_cnt) = if is_ind || counter_pool.is_empty() {
                (0.0_f32, 0.0_f32)
            } else {
                let mut best = 0.0_f32;
                let mut strong = 0.0_f32;
                for f in counter_pool {
                    let bd = max_base_damage(chart, f.stats.unit_type, ut);
                    let dmg_f = calculate_damage_formula(bd, f.hp, e_def, false) as f32;
                    if dmg_f > best {
                        best = dmg_f;
                    }
                    if dmg_f >= STRONG_COUNTER_MIN_DMG {
                        strong += f.hp as f32 / 100.0;
                    }
                }
                (best, strong)
            };

            let asym_mult = if is_ind {
                (2.2 + a_eff / 35.0).min(3.8)
            } else if is_air {
                let air_cov = (strong_cnt * AIR_COUNTER_CAPACITY_PER_UNIT
                    / combat_air_hp_equiv.max(1.0))
                .min(1.0);
                if air_cov >= AIR_COV_SATURATED_THRESHOLD {
                    AIR_ASYM_BASE
                } else {
                    AIR_ASYM_BASE + (1.0 - air_cov) * AIR_ASYM_SPAN
                }
            } else if scan.my_units.is_empty() {
                INITIAL_EMPTY_ARMY_GROUND_ASYM
            } else {
                let qual_cov = (strong_cnt / 2.0).min(1.0);
                let eff_c = best_c * qual_cov;
                let ratio = (a_eff - eff_c).max(0.0) / eff_c.max(20.0);
                (1.0 + ratio).min(3.0)
            };

            // 自軍航空ゼロかつ地上威力の低い輸送ヘリ等の脅威を抑制
            if is_air
                && max_base_damage(chart, ut, UnitType::Tank) < 20
                && max_base_damage(chart, ut, UnitType::Infantry) <= 20
                && !scan
                    .my_units
                    .iter()
                    .any(|f| f.stats.movement_type == MovementType::Air)
            {
                a_eff = a_eff.min(15.0);
            }

            let v_scale = if virtual_wave {
                if arr_rnd == VIRTUAL_WAVE1_ROUND {
                    0.55
                } else {
                    0.45
                }
            } else {
                1.0
            };
            let urgency = if virtual_wave {
                0.85
            } else if is_ind || is_air {
                let min_fd = f_positions
                    .iter()
                    .map(|fp| map.distance(pos.x, pos.y, fp.x, fp.y))
                    .min()
                    .unwrap_or(9);
                if min_fd <= 6 { 1.0 } else { 0.75 }
            } else {
                let d_now = df.get(pos).unwrap_or(10);
                if d_now <= 4 {
                    1.0
                } else if d_now <= 7 {
                    0.75
                } else {
                    0.45
                }
            };

            // 中立または自軍物件を占領・脅かす敵占領ユニットには占領妨害ボーナスを加算
            let cap_denial_bonus = if !virtual_wave && u.stats.can_capture {
                let min_prop_dist = scan
                    .neutral_properties
                    .iter()
                    .copied()
                    .chain(extras.friendly_properties.iter().map(|(p, _)| *p))
                    .map(|p| {
                        map.distance(pos.x, pos.y, p.x, p.y)
                            .min(map.distance(pos_proj.x, pos_proj.y, p.x, p.y))
                    })
                    .min()
                    .unwrap_or(u32::MAX);
                if min_prop_dist == 0 {
                    CAPTURE_DENIAL_ON_PROP_BONUS
                } else if min_prop_dist <= u.stats.max_movement {
                    CAPTURE_DENIAL_NEAR_PROP_BONUS
                } else {
                    0.0
                }
            } else {
                0.0
            };

            let w_full = a_eff.max(8.0) * asym_mult * v_scale * urgency + cap_denial_bonus;
            enemy_targets.push(ProjectedEnemy {
                entity: u.entity,
                stats: u.stats,
                pos0: pos,
                pos_proj,
                def0: e_def0,
                def_proj: e_def,
                hp: u.hp,
                ammo,
                w_per_hp: w_full / 100.0,
                virtual_wave,
                arr_rnd,
            });
        }

        let proj_direct_enemy_tiles: Vec<GridPosition> = enemy_targets
            .iter()
            .filter(|et| !et.virtual_wave && !profiles.is_indirect(et.stats.unit_type))
            .map(|et| et.pos_proj)
            .collect();

        let e_front_pts: Vec<GridPosition> = enemy_targets
            .iter()
            .filter(|et| !et.virtual_wave)
            .map(|et| et.pos_proj)
            .collect();
        let mut fac_front_dists = HashMap::default();
        let mut fac_dist_min = u32::MAX;
        let mut fac_dist_max = 0_u32;
        for &fac in &ground_facility_set {
            let d = e_front_pts
                .iter()
                .map(|ep| map.distance(fac.x, fac.y, ep.x, ep.y))
                .min()
                .unwrap_or(5);
            fac_front_dists.insert(fac, d);
            fac_dist_min = fac_dist_min.min(d);
            fac_dist_max = fac_dist_max.max(d);
        }
        if fac_front_dists.is_empty() {
            fac_dist_min = 0;
        }

        Self {
            width,
            height,
            occ,
            supply_for_unit_domain: is_ground_supply,
            ground_facility_set,
            non_ground_facility_set,
            free_ground_fac_count,
            enemy_targets,
            f_direct_units,
            e_direct_cnt,
            can_reach_enemy_via_land,
            has_reachable_land_objective,
            live_reaches,
            enemy_1t_direct_adj,
            proj_direct_enemy_tiles,
            fac_front_dists,
            fac_dist_min,
            fac_dist_max,
        }
    }

    #[inline]
    pub(super) fn is_ground_supply(&self, pos: GridPosition) -> bool {
        pos.x < self.width
            && pos.y < self.height
            && self.supply_for_unit_domain[pos.y * self.width + pos.x]
    }

    /// 間接ユニットの射点生存率と 2ターン突破リスクを算出する。
    pub(super) fn firing_tile_survival(
        &self,
        scan: &BoardScan,
        profiles: &WeaponProfileTable,
        pos: GridPosition,
        stats: &UnitStats,
    ) -> (f32, f32) {
        let map = &scan.map;
        let master = &scan.master_data;
        let chart = &scan.damage_chart;
        let ut = stats.unit_type;
        let t_def = terrain_def_at(map, master, pos);
        let tile_idx = pos.y * self.width + pos.x;
        let adj = map.get_adjacent(pos.x, pos.y);

        // 1. 1T 以内に到達する直接敵の最大ダメージ合算 + ZOC 最低リスク
        let mut direct_dmg = 0.0_f32;
        for lr in &self.live_reaches {
            let e_ut = lr.unit.stats.unit_type;
            if profiles.is_indirect(e_ut) {
                continue;
            }
            let can_hit = adj
                .iter()
                .any(|&(nx, ny)| lr.reach_1t.contains(GridPosition { x: nx, y: ny }));
            if can_hit {
                let d = expected_damage(
                    chart, profiles, e_ut, lr.unit.hp, lr.ammo, ut, t_def, 1, false,
                ) as f32;
                direct_dmg += d;
            }
        }
        if self.enemy_1t_direct_adj[tile_idx] {
            direct_dmg = direct_dmg.max(ZOC_LOCK_MIN_RISK);
        }

        // 2. 前線近接ペナルティ
        let is_sup = self.is_ground_supply(pos);
        let front_dist = self
            .proj_direct_enemy_tiles
            .iter()
            .map(|ep| map.distance(pos.x, pos.y, ep.x, ep.y))
            .min()
            .unwrap_or(9);
        let front_pen = if front_dist <= 1 {
            FRONT_ADJ_PENALTY
        } else if front_dist == 2 && !is_sup {
            FRONT_NEAR_NON_SUPPLY_PENALTY
        } else {
            0.0
        };

        // 3. 対車両反撃力ベースの 2ターン突破リスク
        let mut d2_blk = 0_u32;
        let mut prc_threats: Vec<(&LiveEnemyReach, u32)> = Vec::new();
        for lr in &self.live_reaches {
            let e_ut = lr.unit.stats.unit_type;
            if profiles.is_indirect(e_ut)
                || max_base_damage(chart, e_ut, ut) < BREACH_MIN_BASE_DAMAGE
            {
                continue;
            }
            if lr.seen_2t_blk[tile_idx] {
                let d_b = expected_damage(
                    chart, profiles, e_ut, lr.unit.hp, lr.ammo, ut, t_def, 1, false,
                );
                if d_b > d2_blk {
                    d2_blk = d_b;
                }
            }
            if lr.seen_2t_prc[tile_idx] {
                let d_p = expected_damage(
                    chart, profiles, e_ut, lr.unit.hp, lr.ammo, ut, t_def, 1, false,
                );
                if d_p >= BREACH_MIN_ACTUAL_DAMAGE {
                    prc_threats.push((lr, d_p));
                }
            }
        }
        let mut best_pierce_risk = 0.0_f32;
        for (lr, d_p) in prc_threats {
            let extra = d_p.saturating_sub(d2_blk);
            if extra == 0 {
                continue;
            }
            let e_ut = lr.unit.stats.unit_type;
            let eff_screen: f32 = self
                .f_direct_units
                .iter()
                .filter(|f| f.pos != pos && map.distance(f.pos.x, f.pos.y, pos.x, pos.y) <= 2)
                .map(|f| {
                    let bd = max_base_damage(chart, f.stats.unit_type, e_ut);
                    let w = if bd >= SCREEN_STRONG_COUNTER_BASE_DAMAGE {
                        1.0
                    } else {
                        SCREEN_SOFT_WEIGHT
                    };
                    f.hp as f32 * w
                })
                .sum();
            let leak = (1.0 - eff_screen / SCREEN_FULL_COVERAGE_HP).max(MIN_SCREEN_LEAK);
            let pr = BREACH_PIERCE_WEIGHT * extra as f32 * leak;
            if pr > best_pierce_risk {
                best_pierce_risk = pr;
            }
        }
        let breach_risk = BREACH_BLOCKED_WEIGHT * d2_blk as f32 + best_pierce_risk;

        // 4. 対砲兵リスク
        let mut cb_dmg = 0.0_f32;
        for et in &self.enemy_targets {
            if et.virtual_wave || !profiles.is_indirect(et.stats.unit_type) {
                continue;
            }
            let d_now = map.distance(et.pos0.x, et.pos0.y, pos.x, pos.y);
            let d_proj = map.distance(et.pos_proj.x, et.pos_proj.y, pos.x, pos.y);
            let dmg_now = expected_damage(
                chart,
                profiles,
                et.stats.unit_type,
                et.hp,
                et.ammo,
                ut,
                t_def,
                d_now,
                false,
            ) as f32;
            let dmg_proj = 0.8
                * expected_damage(
                    chart,
                    profiles,
                    et.stats.unit_type,
                    et.hp,
                    et.ammo,
                    ut,
                    t_def,
                    d_proj,
                    false,
                ) as f32;
            cb_dmg += dmg_now.max(dmg_proj);
        }

        let risk = direct_dmg + front_pen + breach_risk + COUNTER_BATTERY_WEIGHT * cb_dmg;
        let mut surv = (1.0 - risk / 100.0).max(0.0).powi(2);
        if !is_sup {
            surv *= NON_SUPPLY_SURV_MULT;
        }
        // 前衛直接部隊の数が敵直接部隊数に対して薄い局面では、地上間接ユニットの射点生存率を割り引く
        if !matches!(stats.movement_type, MovementType::Air | MovementType::Ship) {
            let enemy_ref = self.e_direct_cnt.max(INDIRECT_SCREEN_MIN_ENEMY_REF) as f32;
            let screen_support = (self.f_direct_units.len() as f32
                / (enemy_ref * INDIRECT_SCREEN_RATIO))
                .clamp(INDIRECT_MIN_SCREEN_SUPPORT, 1.0);
            surv *= screen_support;
        }
        (surv, breach_risk)
    }

    /// 射点による自軍生産口の占有ペナルティを返す。
    pub(super) fn facility_block_penalty(&self, pos: GridPosition, stats: &UnitStats) -> f32 {
        let is_ground = !matches!(stats.movement_type, MovementType::Air | MovementType::Ship);
        if is_ground && self.non_ground_facility_set.contains(&pos) {
            return 1.0e9;
        }
        if self.ground_facility_set.contains(&pos) {
            let front_w = if self.fac_dist_max > self.fac_dist_min {
                let my_d = self
                    .fac_front_dists
                    .get(&pos)
                    .copied()
                    .unwrap_or(self.fac_dist_max);
                let frac = (self.fac_dist_max - my_d) as f32
                    / (self.fac_dist_max - self.fac_dist_min) as f32;
                0.60 + 0.80 * frac
            } else {
                1.0
            };
            let base_bp = (FACILITY_BLOCK_BASE / self.free_ground_fac_count as f32) * front_w;
            if is_immobile_unit(stats) {
                return base_bp * (1.0 + IMMOBILE_BLOCK_HORIZON_WEIGHT * (HORIZON - 1) as f32);
            }
            return base_bp;
        }
        0.0
    }

    /// ユニットが固定砲台のように生産口から動けない（移動力1または燃料2以下）か。
    pub(super) fn immobile_discount(stats: &UnitStats) -> f32 {
        if is_immobile_unit(stats) {
            IMMOBILE_SURV_DISCOUNT
        } else {
            1.0
        }
    }
}

#[inline]
pub(super) fn is_immobile_unit(stats: &UnitStats) -> bool {
    stats.max_movement <= 1 || stats.max_fuel <= IMMOBILE_MAX_FUEL_THRESHOLD
}

/// 盤面位置の地形防御ボーナスを取得する。
#[inline]
pub(super) fn terrain_def_at(map: &Map, master: &MasterDataRegistry, pos: GridPosition) -> u32 {
    let terrain = map.get_terrain(pos.x, pos.y).unwrap_or(Terrain::Plains);
    master.get_terrain_defense_bonus(terrain)
}

/// 2兵種間の最大基礎ダメージ（武器1・武器2の最大）。
#[inline]
pub(super) fn max_base_damage(chart: &DamageChart, att: UnitType, def: UnitType) -> u32 {
    let p = chart.get_base_damage(att, def).unwrap_or(0);
    let s = chart.get_base_damage_secondary(att, def).unwrap_or(0);
    p.max(s)
}

/// 武器1（残弾判定あり）と武器2（無制限副武装）の射程に従った期待ダメージを返す。
#[allow(clippy::too_many_arguments)]
#[inline]
pub(super) fn expected_damage(
    chart: &DamageChart,
    profiles: &WeaponProfileTable,
    att_type: UnitType,
    att_hp: u32,
    att_ammo: (u32, u32),
    def_type: UnitType,
    def_bonus: u32,
    dist: u32,
    is_counter: bool,
) -> u32 {
    if att_hp == 0 {
        return 0;
    }
    let Some(prof) = profiles.get(att_type) else {
        return 0;
    };
    if is_counter && dist > 1 {
        return 0;
    }
    // 武器1を先に試行（残弾ありかつ射程内かつ基礎ダメ > 0）
    if let Some((rmin, rmax)) = prof.w1_range
        && att_ammo.0 > 0
        && dist >= rmin
        && dist <= rmax
        && let Some(bd) = chart.get_base_damage(att_type, def_type)
        && bd > 0
    {
        return calculate_damage_formula(bd, att_hp, def_bonus, is_counter);
    }
    // 武器2を試行（射程内かつ基礎ダメ > 0、実エンジン準拠で弾薬チェックなし）
    if let Some((rmin, rmax)) = prof.w2_range
        && dist >= rmin
        && dist <= rmax
        && let Some(bd) = chart.get_base_damage_secondary(att_type, def_type)
        && bd > 0
    {
        return calculate_damage_formula(bd, att_hp, def_bonus, is_counter);
    }
    0
}
