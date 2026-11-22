//! 敵脅威の位置投影・空間制約（`firing_slots`）・物理到達圏解析。
//!
//! 異種単位の点数定数を廃し、実在敵の 1T/2T 到達圏・地形防御込み期待ダメージ・
//! 物件占領脅威・同時隣接可能枠数（`firing_slots`）など、盤面とマスターデータから
//! 直接観測できる物理量だけを提供する。

use super::reach::{
    OCC_ENEMY, OCC_FRIENDLY, OccupancyGrid, ReachMap, reachable_pierce, reachable_tiles,
    shortest_dist_field,
};
use super::{UnifiedBoardExtras, WeaponProfileTable};
use crate::ai::v4::{BoardScan, UnitSnapshot};
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, UnitStats};
use crate::resources::master_data::{MasterDataRegistry, supply_types};
use crate::resources::{DamageChart, Map, MovementType, Terrain, UnitType};
use crate::systems::combat::calculate_damage_formula;
use crate::systems::movement::get_valid_movement_cost;
use bevy_ecs::prelude::Entity;

/// 実在敵 1 体の投影情報と空間制約。
#[derive(Debug, Clone)]
pub(super) struct ObservedEnemy {
    pub(super) entity: Option<Entity>,
    pub(super) stats: UnitStats,
    pub(super) pos0: GridPosition,
    pub(super) pos_proj: GridPosition,
    pub(super) def0: u32,
    pub(super) def_proj: u32,
    pub(super) hp: u32,
    pub(super) ammo: (u32, u32),
    pub(super) is_air: bool,
    pub(super) is_indirect: bool,
    /// 投影位置周囲で地上直接ユニットが進入・隣接攻撃できる空きマス数。
    pub(super) firing_slots: usize,
    /// 中立または自軍物件上、あるいは 1T 到達圏にいて占領完了が迫る敵占領ユニットか。
    pub(super) threatens_property: bool,
}

/// 実在敵ごとの事前計算済み到達圏（射点・空港安全判定で再利用）。
pub(super) struct LiveEnemyReach {
    pub(super) unit: UnitSnapshot,
    pub(super) ammo: (u32, u32),
    pub(super) reach_1t: ReachMap,
    pub(super) seen_2t_blk: Vec<bool>,
    pub(super) seen_2t_prc: Vec<bool>,
}

/// 射点における物理的な敵脅威の集計値（%ダメージ）。
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct FiringTileThreat {
    /// 1T 以内に隣接可能な敵直接部隊の期待ダメージ合計。
    pub(super) direct_1t_dmg: u32,
    /// 遮蔽あり 2T 以内に到達可能な敵直接部隊の最大期待ダメージ。
    pub(super) breach_2t_blk_dmg: u32,
    /// 遮蔽無視 2T 到達範囲内で近接味方護衛が不十分な場合の最大期待ダメージ。
    pub(super) breach_2t_prc_dmg: u32,
    /// 敵間接部隊からの対砲兵期待ダメージ合計。
    pub(super) counter_battery_dmg: u32,
    /// 射点周囲2マス以内で対抗可能な味方前衛直接ユニットの HP 合計。
    pub(super) nearby_screen_hp: u32,
}

/// 盤面の物理脅威・供給網・陸路到達性を保持するコンテキスト。
pub(super) struct ThreatContext {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) occ: OccupancyGrid,
    pub(super) supply_for_unit_domain: Vec<bool>,
    pub(super) ground_facility_set: HashSet<GridPosition>,
    pub(super) enemies: Vec<ObservedEnemy>,
    pub(super) f_direct_units: Vec<UnitSnapshot>,
    pub(super) e_direct_cnt: usize,
    pub(super) can_reach_enemy_via_land: bool,
    pub(super) live_reaches: Vec<LiveEnemyReach>,
}

impl ThreatContext {
    /// 盤面スキャンと追加盤面情報から `ThreatContext` を構築する。
    pub(super) fn build(
        scan: &BoardScan,
        extras: &UnifiedBoardExtras,
        profiles: &WeaponProfileTable,
    ) -> Self {
        let map = &scan.map;
        let master = &scan.master_data;
        let width = map.width;
        let height = map.height;
        let grid_len = width.saturating_mul(height);

        let occ = OccupancyGrid::from_units(
            map,
            scan.my_units.iter().map(|u| u.pos),
            scan.enemy_units.iter().map(|u| u.pos),
        );

        let mut supply_for_unit_domain = vec![false; grid_len];
        for &(pos, terrain) in &extras.friendly_properties {
            if pos.x < width
                && pos.y < height
                && master
                    .get_landscape_by_name(terrain.as_str())
                    .and_then(|r| r.supply_type.as_deref())
                    == Some(supply_types::GROUND)
            {
                supply_for_unit_domain[pos.y * width + pos.x] = true;
            }
        }

        let ground_facility_set: HashSet<GridPosition> = scan
            .production_facilities
            .iter()
            .filter(|(_, terrain)| {
                master
                    .get_landscape_by_name(terrain.as_str())
                    .and_then(|r| r.supply_type.as_deref())
                    == Some(supply_types::GROUND)
            })
            .map(|(pos, _)| *pos)
            .collect();

        let f_direct_units: Vec<UnitSnapshot> = scan
            .my_units
            .iter()
            .filter(|u| !profiles.is_indirect(u.stats.unit_type))
            .cloned()
            .collect();
        let f_positions: Vec<GridPosition> = scan.my_units.iter().map(|u| u.pos).collect();

        // 敵が目指す自軍ユニット・自軍物件隣接タイルへの最短コスト場を構築する
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

        // 陸続き判定（自軍生産施設から敵へ地上ユニットで到達できるか）
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

        // 中立物件集合（BoardScan::collect が空の場合でも enemy_properties の補集合から判定する）
        let enemy_prop_set: HashSet<GridPosition> =
            extras.enemy_properties.iter().map(|(p, _)| *p).collect();
        let mut is_neutral_prop = vec![false; grid_len];
        for p in &scan.neutral_properties {
            if p.x < width && p.y < height {
                is_neutral_prop[p.y * width + p.x] = true;
            }
        }
        if scan.neutral_properties.is_empty() && !extras.enemy_properties.is_empty() {
            for p in &scan.open_properties {
                if !enemy_prop_set.contains(p) && p.x < width && p.y < height {
                    is_neutral_prop[p.y * width + p.x] = true;
                }
            }
        }

        // 決定的な順序で実在敵を並べる
        let mut enemy_items: Vec<(UnitSnapshot, (u32, u32))> = scan
            .enemy_units
            .iter()
            .map(|u| {
                let ammo = u
                    .entity
                    .and_then(|e| extras.unit_ammo.get(&e).copied())
                    .unwrap_or((u.stats.max_ammo1, u.stats.max_ammo2));
                (u.clone(), ammo)
            })
            .collect();
        enemy_items.sort_by(|a, b| {
            let da = dist_fields
                .get(&a.0.stats.movement_type)
                .and_then(|df| df.get(a.0.pos))
                .unwrap_or(999);
            let db = dist_fields
                .get(&b.0.stats.movement_type)
                .and_then(|df| df.get(b.0.pos))
                .unwrap_or(999);
            (da, a.0.pos.y, a.0.pos.x).cmp(&(db, b.0.pos.y, b.0.pos.x))
        });

        let e_direct_cnt = scan
            .enemy_units
            .iter()
            .filter(|u| !profiles.is_indirect(u.stats.unit_type))
            .count();

        let mut claimed_proj = vec![false; grid_len];
        let mut enemies = Vec::with_capacity(enemy_items.len());
        let mut live_reaches = Vec::with_capacity(enemy_items.len());

        for (u, ammo) in enemy_items {
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

            // 敵の予想行動先（1T/2T 前進先）を投影する
            let df = &dist_fields[&u.stats.movement_type];
            let pos_proj = if is_air {
                pos
            } else if is_ind {
                if f_positions.iter().any(|fp| {
                    let d = map.distance(pos.x, pos.y, fp.x, fp.y);
                    d >= rmin && d <= rmax
                }) {
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
            } else if f_positions
                .iter()
                .any(|fp| map.distance(pos.x, pos.y, fp.x, fp.y) <= 1)
            {
                pos
            } else {
                let neutral_cap_proj = if u.stats.can_capture {
                    r1.iter()
                        .filter(|&(p, _)| {
                            p.x < width
                                && p.y < height
                                && is_neutral_prop[p.y * width + p.x]
                                && !claimed_proj[p.y * width + p.x]
                        })
                        .min_by_key(|&(p, _)| (map.distance(pos.x, pos.y, p.x, p.y), p.y, p.x))
                        .map(|(p, _)| p)
                } else {
                    None
                };
                if let Some(np) = neutral_cap_proj {
                    np
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
                }
            };

            if pos_proj.x < width && pos_proj.y < height {
                claimed_proj[pos_proj.y * width + pos_proj.x] = true;
            }
            let def0 = terrain_def_at(map, master, pos);
            let def_proj = terrain_def_at(map, master, pos_proj);

            // 敵投影位置の周囲で地上直接ユニットが侵入・攻撃できる空きスロット数を数える
            let firing_slots = if is_air {
                map.get_adjacent(pos_proj.x, pos_proj.y).len()
            } else {
                map.get_adjacent(pos_proj.x, pos_proj.y)
                    .into_iter()
                    .filter(|&(ax, ay)| {
                        map.get_terrain(ax, ay).is_some_and(|t| {
                            get_valid_movement_cost(master, MovementType::Infantry, t).is_some()
                        })
                    })
                    .count()
                    .max(1)
            };

            // 中立または自軍物件上／1T圏にいて占領が迫っている敵占領役かを判定
            let threatens_property = u.stats.can_capture
                && (scan
                    .neutral_properties
                    .iter()
                    .copied()
                    .chain(extras.friendly_properties.iter().map(|(p, _)| *p))
                    .chain(
                        is_neutral_prop
                            .iter()
                            .enumerate()
                            .filter(|&(_, &flag)| flag)
                            .map(|(idx, _)| GridPosition {
                                x: idx % width,
                                y: idx / width,
                            }),
                    )
                    .any(|p| {
                        let pos_on = pos == p || pos_proj == p;
                        pos_on || r1.contains(p)
                    }));

            enemies.push(ObservedEnemy {
                entity: u.entity,
                stats: u.stats,
                pos0: pos,
                pos_proj,
                def0,
                def_proj,
                hp: u.hp,
                ammo,
                is_air,
                is_indirect: is_ind,
                firing_slots,
                threatens_property,
            });
        }

        Self {
            width,
            height,
            occ,
            supply_for_unit_domain,
            ground_facility_set,
            enemies,
            f_direct_units,
            e_direct_cnt,
            can_reach_enemy_via_land,
            live_reaches,
        }
    }

    #[inline]
    pub(super) fn is_ground_supply(&self, pos: GridPosition) -> bool {
        pos.x < self.width
            && pos.y < self.height
            && self.supply_for_unit_domain[pos.y * self.width + pos.x]
    }

    /// 射点における 1T 直接脅威・2T 遮蔽突破脅威・対砲兵脅威の物理ダメージ（%）を算出する。
    pub(super) fn firing_tile_threat(
        &self,
        scan: &BoardScan,
        profiles: &WeaponProfileTable,
        pos: GridPosition,
        stats: &UnitStats,
    ) -> FiringTileThreat {
        let map = &scan.map;
        let master = &scan.master_data;
        let chart = &scan.damage_chart;
        let ut = stats.unit_type;
        let t_def = terrain_def_at(map, master, pos);
        let tile_idx = pos.y * self.width + pos.x;
        let adj = map.get_adjacent(pos.x, pos.y);

        let mut direct_1t_dmg = 0_u32;
        let mut breach_2t_blk_dmg = 0_u32;
        let mut breach_2t_prc_dmg = 0_u32;

        // 射点周囲の味方直接前衛 HP 合計（2T 貫通脅威に対する物理遮蔽の有無判定に使用）
        let nearby_screen_hp: u32 = self
            .f_direct_units
            .iter()
            .filter(|f| f.pos != pos && map.distance(f.pos.x, f.pos.y, pos.x, pos.y) <= 2)
            .filter(|f| {
                self.enemies.iter().any(|e| {
                    max_base_damage(chart, f.stats.unit_type, e.stats.unit_type)
                        >= max_base_damage(chart, e.stats.unit_type, ut) / 2
                })
            })
            .map(|f| f.hp)
            .sum();

        for lr in &self.live_reaches {
            let e_ut = lr.unit.stats.unit_type;
            if profiles.is_indirect(e_ut) {
                continue;
            }
            let hit_1t = adj
                .iter()
                .any(|&(nx, ny)| lr.reach_1t.contains(GridPosition { x: nx, y: ny }));
            let d = expected_damage(
                chart, profiles, e_ut, lr.unit.hp, lr.ammo, ut, t_def, 1, false,
            );
            if hit_1t {
                direct_1t_dmg = direct_1t_dmg.saturating_add(d);
            }
            if lr.seen_2t_blk[tile_idx] {
                breach_2t_blk_dmg = breach_2t_blk_dmg.max(d);
            }
            if lr.seen_2t_prc[tile_idx] && nearby_screen_hp < 100 {
                breach_2t_prc_dmg = breach_2t_prc_dmg.max(d);
            }
        }

        let mut counter_battery_dmg = 0_u32;
        for et in &self.enemies {
            if !et.is_indirect {
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
            );
            let dmg_proj = expected_damage(
                chart,
                profiles,
                et.stats.unit_type,
                et.hp,
                et.ammo,
                ut,
                t_def,
                d_proj,
                false,
            );
            counter_battery_dmg = counter_battery_dmg.saturating_add(dmg_now.max(dmg_proj));
        }

        FiringTileThreat {
            direct_1t_dmg,
            breach_2t_blk_dmg,
            breach_2t_prc_dmg,
            counter_battery_dmg,
            nearby_screen_hp,
        }
    }
}

/// 移動力 1 または燃料が極めて少なく自力再配置できない固定砲台ユニットか。
#[inline]
pub(super) fn is_immobile_unit(stats: &UnitStats) -> bool {
    stats.max_movement <= 1 || stats.max_fuel <= 2
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
