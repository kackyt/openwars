//! マップ骨格スキャンと戦況に応じた 12 カテゴリ別・長期的兵種比率ガイド（ソフト誘導・貯蓄設計）。
//!
//! マスターデータ（`category_ai.csv`）の 12 カテゴリを SSOT とし、
//! マップ幾何（陸地面積・戦車通行面積・開闊度・隘路密度・山岳障壁率・物件密度・所有施設）と
//! 現在の戦況（未所有物件率・敵編成割合・頭数差）から「カテゴリ別の長期目標シェア（合計 1.0）」を算出する。
//! 毎ターンの生産ではその目標シェアへ**長期的に近づける連続的なスコア倍率**、
//! 同一カテゴリ内の軽車両／主力戦車の質的バランス補正、
//! および前線安定時の**上位突破ユニット向けソフト貯蓄ライン**を提供する。

use super::threats::{ThreatContext, is_immobile_unit, max_base_damage};
use super::{UnifiedBoardExtras, WeaponProfileTable};
use crate::ai::v4::BoardScan;
use crate::collections::HashSet;
use crate::components::{GridPosition, UnitStats};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{
    AI_UNIT_CATEGORY_COUNT, AiUnitCategory, DamageChart, MovementType, Terrain, UnitType,
};
use crate::systems::movement::get_valid_movement_cost;

/// ユニットの満タン HP。
const FULL_UNIT_HP: u32 = 100;

/// 対装甲（軽戦車級）に有効な打撃力とみなす基礎ダメージ下限。
const ARMOR_PENETRATION_THRESHOLD: u32 = 20;

/// 対空特化ユニットとみなす対空基礎ダメージ下限（3発以内撃墜相当）。
const SPECIALIZED_AA_THRESHOLD: u32 = 34;

/// 狭隘タイルとみなす隣接車両通行可能マス数の上限。
const NARROW_TILE_MAX_ADJACENT: usize = 3;

/// 悪路タイルとみなす戦車移動コストの下限。
const ROUGH_TERRAIN_MIN_COST: u32 = 2;

/// 物件密度正規化の基準値（1車両通行タイルあたり物件0.5個を密度1.0とする）。
const PROP_DENSITY_NORMALIZER: f32 = 0.50;

/// 敵編成比率計算時に小規模盤面で分母が極小にならないための下限総価値（Gold）。
const MIN_ENEMY_VALUE_BASE: u64 = 6_000;

/// ディリクレ平滑化の事前重み（体数：満タン 1 体 = 1.0）。
/// 総保有体数が小さい序盤や小規模テスト盤面では平滑化シェアが目標シェア付近に保たれ、
/// 部隊数が増える中盤以降に体数比率誘導が滑らかに効くようにする。
const PRIOR_UNIT_COUNT: f32 = 8.0;

/// 戦車カテゴリ内の軽車両／主力戦車比率計算用ディリクレ事前重み（Gold）。
const TANK_INTRA_PRIOR_GOLD: f32 = 8_000.0;

/// 平滑化シェア計算・比率演算におけるゼロ除算防止下限。
const MIN_EFFECTIVE_SHARE: f32 = 0.015;

/// カテゴリ比率差をスコア倍率へ変換するべき乗指数（穏やかな連続誘導）。
const CATEGORY_BOOST_EXPONENT: f32 = 0.75;

/// スコア倍率の下限（過剰カテゴリでも完全禁止にはせず、適度に抑制する）。
const CATEGORY_MULT_MIN: f32 = 0.35;

/// スコア倍率の上限。
const CATEGORY_MULT_MAX: f32 = 2.10;

/// 敵の拠点占領阻止（緊急打撃）時に維持するカテゴリ倍率の下限。
const URGENT_ANTI_CAP_MULT_FLOOR: f32 = 0.85;

/// 戦車カテゴリ内で過剰な軽車両（装甲車等）へ適用する倍率の下限。
const TANK_LIGHT_MULT_MIN: f32 = 0.28;

/// 敵高コスト航空機（戦闘機・爆撃機等）とみなす生産コスト基準（Gold）。
const HIGH_COST_AIR_THRESHOLD: u32 = 15_000;

/// 固定砲台の目標シェア付与を開始する最低地上生産施設数。
const ARTILLERY_MIN_GROUND_FACILITIES: usize = 3;

/// 固定砲台の目標シェア付与を開始する最低盤面渋滞度（真の隘路・高密度マップ向け）。
const ARTILLERY_MIN_CONGESTION: f32 = 0.55;

/// 対空ミサイルの配備を考慮する最低地上生産施設数。
const AA_MISSILE_MIN_GROUND_FACILITIES: usize = 2;

/// 間接火網の安全上限門数（マップ比率上限とは独立した暴走防止ガード）。
const HARD_MAX_INDIRECT_UNITS: usize = 8;

/// 機動自走砲が無条件で許容される最低保証門数（少数砲による膠着打開）。
const MIN_GUARANTEED_INDIRECT_UNITS: usize = 2;

/// 貯蓄検討・スロット圧縮優遇の対象となる高単価突破ユニット（ハンマー枠）の最低生産コスト（Gold）。
pub(super) const HAMMER_UNIT_MIN_COST: u32 = 16_000;

/// 大型重戦闘機の圧縮価値優遇対象となる最低生産コスト（Gold）。
const HEAVY_FIGHTER_MIN_COST: u32 = 24_000;

/// 大型重戦闘機に与えるスロット圧縮倍率。
const HEAVY_FIGHTER_COMPRESSION_BONUS: f32 = 1.10;

/// 低機動な重歩兵と判定する最大移動力。
const MECH_SLOW_MOVEMENT_CAP: u32 = 2;

/// 軽戦車1両分に相当する主力戦車未配備時の重歩兵抑制基準価値（Gold）。
const MECH_ARMOR_SHORTAGE_BASELINE_GOLD: u32 = 6_000;

/// 主力戦車未配備時に低機動重歩兵の直接戦闘生産を抑えて戦車生産へ譲る倍率。
const MECH_ARMOR_SHORTAGE_MULT: f32 = 0.65;

/// 前線頭数が充足した中盤以降、上位突破ユニットの1スロット当たり圧縮価値に与える倍率。
const HAMMER_SLOT_COMPRESSION_BONUS: f32 = 1.22;

/// 上位突破ユニット向けの繰越貯蓄を有効にする最低現在収入（Gold/ターン）。
const MIN_INCOME_FOR_HAMMER_SAVING: u32 = 8_000;

/// 次ターン合算での突破ユニット購入に向けた繰越目標額の上限（1〜2T以内に届く範囲に限定）。
const MAX_HAMMER_SAVING_FLOOR_GOLD: u32 = 7_500;

/// 目標超過判定を有効化する自軍総保有体数の下限（序盤の小部隊時に過剰抑制しない）。
const OVER_TARGET_MIN_TOTAL_UNITS: f32 = 8.0;

/// 中盤の質的誘導やハンマー貯蓄を有効化する最低直接戦闘部隊数。
const MIDGAME_MIN_DIRECT_UNITS: usize = 6;

/// ユニットが対地打撃能力を持つか判定する。
#[inline]
pub(super) fn can_hit_ground(chart: &DamageChart, ut: UnitType) -> bool {
    max_base_damage(chart, ut, UnitType::Infantry) > 0
        || max_base_damage(chart, ut, UnitType::Tank) > 0
}

/// 敵装甲車両（軽戦車級）に対して有効な打撃力（基礎ダメージ20以上）を持つか判定する。
#[inline]
pub(super) fn can_hit_armor(chart: &DamageChart, ut: UnitType) -> bool {
    max_base_damage(chart, ut, UnitType::Tank) >= ARMOR_PENETRATION_THRESHOLD
}

/// 対装甲能力を持たず、もっぱら対空迎撃を担う対空特化ユニット（対空戦車・ミサイル・戦闘機等）か判定する。
/// 歩兵のマシンガン等（対ヘリ15%以下）を誤って対空特化として数えないよう、有効な対空打撃力の有無も確認する。
#[inline]
pub(super) fn is_aa_specialized(chart: &DamageChart, ut: UnitType) -> bool {
    !can_hit_armor(chart, ut)
        && (max_base_damage(chart, ut, UnitType::Bcopters) >= SPECIALIZED_AA_THRESHOLD
            || max_base_damage(chart, ut, UnitType::Bomber) >= SPECIALIZED_AA_THRESHOLD
            || max_base_damage(chart, ut, UnitType::Fighter) >= SPECIALIZED_AA_THRESHOLD)
}

/// 戦闘役ではなく人員輸送を主目的とするユニットか判定する。
#[inline]
pub(super) fn is_non_combat_transport(chart: &DamageChart, stats: &UnitStats) -> bool {
    stats.max_cargo > 0
        && (matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
            || !can_hit_ground(chart, stats.unit_type))
}

/// マスターデータ（`category_ai.csv`）を正本としてユニットの AI 用 12 カテゴリを解決する。
/// 合成テスト等で `MasterDataRegistry::default()` が使われ未登録の場合のみ能力値から推定する。
pub(super) fn resolve_ai_category(
    master: &MasterDataRegistry,
    chart: &DamageChart,
    profiles: &WeaponProfileTable,
    stats: &UnitStats,
) -> AiUnitCategory {
    if let Some(cat) = master.ai_category_for(stats.unit_type) {
        return cat;
    }
    // 合成盤面向けフォールバック（標準対局では到達しない）
    let ut = stats.unit_type;
    if stats.can_supply {
        return AiUnitCategory::SupplyTruck;
    }
    if stats.can_capture {
        return AiUnitCategory::Infantry;
    }
    match stats.movement_type {
        MovementType::Ship => {
            if stats.max_cargo > 0 {
                let is_carrier_like = max_base_damage(chart, ut, UnitType::Bomber)
                    >= SPECIALIZED_AA_THRESHOLD
                    || max_base_damage(chart, ut, UnitType::Fighter) >= SPECIALIZED_AA_THRESHOLD;
                if is_carrier_like {
                    AiUnitCategory::Carrier
                } else {
                    AiUnitCategory::Lander
                }
            } else {
                AiUnitCategory::Battleship
            }
        }
        MovementType::Air => {
            if is_aa_specialized(chart, ut) {
                AiUnitCategory::Fighter
            } else {
                AiUnitCategory::AirForce
            }
        }
        _ => {
            if is_aa_specialized(chart, ut) {
                if profiles.is_indirect(ut) {
                    AiUnitCategory::AntiAirMissile
                } else {
                    AiUnitCategory::AntiAirTank
                }
            } else if is_immobile_unit(stats) {
                AiUnitCategory::Artillery
            } else if profiles.is_indirect(ut) {
                AiUnitCategory::SelfPropelledGun
            } else {
                AiUnitCategory::Tank
            }
        }
    }
}

/// マップ骨格と現在戦況から導出される 12 カテゴリ別の長期的目標シェアと誘導器。
#[derive(Debug, Clone)]
pub(super) struct CompositionGuidance {
    target_shares: [f32; AI_UNIT_CATEGORY_COUNT],
    openness: f32,
    congestion: f32,
    tank_impassable_ratio: f32,
    has_open_properties: bool,
    is_behind_on_properties: bool,
    e_vehicle_share: f32,
    e_armored_val: u32,
}

impl CompositionGuidance {
    /// 現在の盤面（地形幾何・山岳障壁率・物件密度・保有施設・未所有物件・敵編成割合・頭数差）から目標シェアを構築する。
    pub(super) fn build(
        scan: &BoardScan,
        extras: &UnifiedBoardExtras,
        profiles: &WeaponProfileTable,
        threat_ctx: &ThreatContext,
    ) -> Self {
        let map = &scan.map;
        let master = &scan.master_data;

        let mut land_tiles = 0_usize;
        let mut veh_land = 0_usize;
        let mut plains_road = 0_usize;
        let mut narrow_tiles = 0_usize;
        let mut rough_tiles = 0_usize;

        for y in 0..map.height {
            for x in 0..map.width {
                let Some(terrain) = map.get_terrain(x, y) else {
                    continue;
                };
                if get_valid_movement_cost(master, MovementType::Infantry, terrain).is_some() {
                    land_tiles += 1;
                }
                let Some(tank_cost) = get_valid_movement_cost(master, MovementType::Tank, terrain)
                else {
                    continue;
                };
                veh_land += 1;
                if matches!(terrain, Terrain::Plains | Terrain::Road) {
                    plains_road += 1;
                }
                let adj_veh = map
                    .get_adjacent(x, y)
                    .into_iter()
                    .filter(|&(nx, ny)| {
                        map.get_terrain(nx, ny).is_some_and(|nt| {
                            get_valid_movement_cost(master, MovementType::Tank, nt).is_some()
                        })
                    })
                    .count();
                if terrain == Terrain::Bridge || adj_veh <= NARROW_TILE_MAX_ADJACENT {
                    narrow_tiles += 1;
                }
                if tank_cost >= ROUGH_TERRAIN_MIN_COST {
                    rough_tiles += 1;
                }
            }
        }

        // 重複を除いて盤面上の全物件座標数を集計する
        let mut prop_coords: HashSet<GridPosition> = HashSet::default();
        for &(p, _) in extras
            .friendly_properties
            .iter()
            .chain(extras.enemy_properties.iter())
        {
            prop_coords.insert(p);
        }
        for &p in scan
            .neutral_properties
            .iter()
            .chain(scan.open_properties.iter())
        {
            prop_coords.insert(p);
        }
        let total_props = prop_coords.len().max(scan.property_entities.len());

        let veh_denom = veh_land.max(1) as f32;
        let impassable_land_tiles = land_tiles.saturating_sub(veh_land);
        let tank_impassable_ratio =
            (impassable_land_tiles as f32 / land_tiles.max(1) as f32).clamp(0.0, 0.60);
        let openness = ((plains_road as f32 / veh_denom)
            * (1.0 - 0.5 * (narrow_tiles as f32 / veh_denom)))
            .clamp(0.15, 0.85);
        let prop_density =
            (total_props as f32 / veh_denom / PROP_DENSITY_NORMALIZER).clamp(0.0, 1.0);
        let terrain_friction =
            ((narrow_tiles as f32 * 2.0 + rough_tiles as f32 * 0.5) / veh_denom).clamp(0.0, 1.0);
        let congestion = (0.55 * prop_density + 0.45 * terrain_friction).clamp(0.15, 0.90);

        // 所有施設の能力内訳を把握し、生産不能なカテゴリの目標シェアが 0 になるようゲーティングする
        let mut ground_fac_cnt = 0_usize;
        let mut port_cnt = 0_usize;
        for &(_, terrain) in &scan.production_facilities {
            match terrain {
                Terrain::Port => port_cnt += 1,
                Terrain::Airport => {}
                _ => ground_fac_cnt += 1,
            }
        }
        let has_ground_fac = ground_fac_cnt > 0;
        let airport_cnt = scan.owned_airport_count as usize;
        let enemy_has_airport = scan
            .enemy_facilities
            .iter()
            .any(|ef| ef.terrain == Terrain::Airport);

        // 敵編成の価値・体数集計（航空脅威・高価値地上脅威・装甲車両・艦船）
        let mut e_total = 0_u64;
        let mut e_total_cnt = 0_usize;
        let mut e_air_val = 0_u64;
        let mut e_air_combat_cnt = 0_usize;
        let mut e_has_high_air = false;
        let mut e_heavy_ground_val = 0_u64;
        let mut e_vehicle_val = 0_u64;
        let mut e_armored_val = 0_u32;
        let mut e_ship_cnt = 0_usize;

        for e in &threat_ctx.enemies {
            let val = u64::from(e.stats.cost.saturating_mul(e.hp) / FULL_UNIT_HP);
            e_total = e_total.saturating_add(val);
            e_total_cnt += 1;
            let e_cat = resolve_ai_category(master, &scan.damage_chart, profiles, &e.stats);
            if e.is_air && e.stats.max_cargo == 0 && profiles.has_any_weapon(e.stats.unit_type) {
                e_air_val = e_air_val.saturating_add(val);
                e_air_combat_cnt += 1;
                if e.stats.cost >= HIGH_COST_AIR_THRESHOLD {
                    e_has_high_air = true;
                }
            }
            if !e.is_air && e.stats.movement_type != MovementType::Ship && !e.stats.can_capture {
                e_vehicle_val = e_vehicle_val.saturating_add(val);
                if can_hit_armor(&scan.damage_chart, e.stats.unit_type) {
                    e_armored_val = e_armored_val.saturating_add(val as u32);
                }
            }
            if matches!(
                e_cat,
                AiUnitCategory::SelfPropelledGun | AiUnitCategory::Artillery
            ) || (e_cat == AiUnitCategory::Tank
                && can_hit_armor(&scan.damage_chart, e.stats.unit_type))
            {
                e_heavy_ground_val = e_heavy_ground_val.saturating_add(val);
            }
            if e.stats.movement_type == MovementType::Ship {
                e_ship_cnt += 1;
            }
        }
        let enemy_denom = e_total.max(MIN_ENEMY_VALUE_BASE) as f32;
        let e_air_share = (e_air_val as f32 / enemy_denom).clamp(0.0, 1.0);
        let e_air_unit_share = e_air_combat_cnt as f32 / (e_total_cnt.max(4) as f32);
        let e_air_blend = (0.5 * e_air_unit_share + 0.5 * e_air_share).clamp(0.0, 1.0);
        let e_heavy_ground_share = (e_heavy_ground_val as f32 / enemy_denom).clamp(0.0, 1.0);
        let e_vehicle_share = (e_vehicle_val as f32 / enemy_denom).clamp(0.0, 1.0);

        let mut weights = [0.0_f32; AI_UNIT_CATEGORY_COUNT];

        // 1. 歩兵（占領・前衛壁：体数割合ベース）
        if has_ground_fac {
            let mut w_infantry = 0.38 + 0.08 * congestion;
            let open_frac = scan.open_properties.len() as f32 / total_props.max(1) as f32;
            if open_frac > 0.10 {
                let exp = (open_frac / 0.50).clamp(0.0, 1.0);
                w_infantry += 0.10 * exp;
            }
            if threat_ctx.f_direct_units.len() < threat_ctx.e_direct_cnt {
                w_infantry += 0.05;
            }
            weights[AiUnitCategory::Infantry as usize] = w_infantry;
        }

        // 2. 自走砲（機動間接火網：狭隘度と敵重地上戦力比で増減）
        if has_ground_fac {
            let w_spg = (0.06 + 0.10 * congestion).clamp(0.07, 0.15) + 0.04 * e_heavy_ground_share;
            weights[AiUnitCategory::SelfPropelledGun as usize] = w_spg;
        }

        // 3. 砲台（固定長射程砲：ArtilleryFerry が動作する標準経路かつ狭隘・多工場盤面のみ）
        let w_artillery = if !crate::ai::code_path::is_extended()
            && has_ground_fac
            && ground_fac_cnt >= ARTILLERY_MIN_GROUND_FACILITIES
            && congestion >= ARTILLERY_MIN_CONGESTION
        {
            (0.06 * (congestion - 0.45)).clamp(0.02, 0.04)
        } else {
            0.0
        };
        weights[AiUnitCategory::Artillery as usize] = w_artillery;

        // 4. 補給輸送車（砲台運搬を行う標準経路かつ砲台枠があるときのみ）
        if !crate::ai::code_path::is_extended() && w_artillery > 0.0 && has_ground_fac {
            weights[AiUnitCategory::SupplyTruck as usize] = w_artillery * 0.50;
        }

        // 5. 対空3カテゴリ（戦闘機 / 対空ミサイル / 対空戦車）の地形・施設・敵航空に応じた配分
        let min_aa_alert = if enemy_has_airport && has_ground_fac {
            0.02
        } else {
            0.0
        };
        let w_aa_total = if e_air_val > 0 || e_air_combat_cnt > 0 || enemy_has_airport {
            (e_air_blend * 0.75).clamp(min_aa_alert, 0.18)
        } else {
            0.0
        };
        if w_aa_total > 0.0 {
            let use_fighters = airport_cnt > 0
                && (e_air_combat_cnt >= 2 || e_has_high_air || tank_impassable_ratio >= 0.18);
            let fighter_frac = if use_fighters {
                (0.35 + 0.30 * tank_impassable_ratio).clamp(0.35, 0.55)
            } else {
                0.0
            };
            let w_fighter = if airport_cnt > 0 {
                w_aa_total * fighter_frac
            } else {
                0.0
            };
            weights[AiUnitCategory::Fighter as usize] = w_fighter;

            let rem_aa_ground = if has_ground_fac {
                w_aa_total - w_fighter
            } else {
                // 地上工場がない場合、対空枠はすべて戦闘機へ寄せる
                weights[AiUnitCategory::Fighter as usize] =
                    if airport_cnt > 0 { w_aa_total } else { 0.0 };
                0.0
            };
            if rem_aa_ground > 0.0 {
                let missile_frac = if ground_fac_cnt >= AA_MISSILE_MIN_GROUND_FACILITIES {
                    (0.20 + 0.45 * (0.5 * congestion + 0.5 * tank_impassable_ratio))
                        .clamp(0.20, 0.55)
                } else {
                    0.0
                };
                weights[AiUnitCategory::AntiAirMissile as usize] = rem_aa_ground * missile_frac;
                weights[AiUnitCategory::AntiAirTank as usize] =
                    rem_aa_ground * (1.0 - missile_frac);
            }
        }

        // 6. 艦船3カテゴリ（港を持ち、渡洋が必要または敵艦船が存在する盤面のみ活性化）
        if port_cnt > 0 {
            if !threat_ctx.can_reach_enemy_via_land {
                weights[AiUnitCategory::Lander as usize] = 0.06;
            }
            if e_ship_cnt > 0 || (!threat_ctx.can_reach_enemy_via_land && scan.my_income >= 8_000) {
                weights[AiUnitCategory::Battleship as usize] = 0.03;
                weights[AiUnitCategory::Carrier as usize] = 0.01;
            }
        }

        // 7. 主力機動打撃2カテゴリ（戦車 / 航空部隊）：残余比率を空港有無と山岳障壁率で配分
        let fixed_sum: f32 = weights.iter().sum();
        let w_strike = (1.0 - fixed_sum).max(0.28);
        if airport_cnt > 0 && has_ground_fac {
            let air_frac =
                (0.30 + 0.08 * (airport_cnt.min(2) as f32 - 1.0) + 0.18 * tank_impassable_ratio)
                    .clamp(0.30, 0.48);
            weights[AiUnitCategory::AirForce as usize] = w_strike * air_frac;
            weights[AiUnitCategory::Tank as usize] = w_strike * (1.0 - air_frac);
        } else if airport_cnt > 0 {
            weights[AiUnitCategory::AirForce as usize] = w_strike;
        } else if has_ground_fac {
            weights[AiUnitCategory::Tank as usize] = w_strike;
        }

        let sum: f32 = weights.iter().sum();
        let target_shares = if sum > 0.0 {
            weights.map(|v| v / sum)
        } else {
            [1.0 / AI_UNIT_CATEGORY_COUNT as f32; AI_UNIT_CATEGORY_COUNT]
        };
        let is_behind_on_properties =
            extras.friendly_properties.len() < extras.enemy_properties.len();

        Self {
            target_shares,
            openness,
            congestion,
            tank_impassable_ratio,
            has_open_properties: !scan.open_properties.is_empty(),
            is_behind_on_properties,
            e_vehicle_share,
            e_armored_val,
        }
    }

    /// 特定カテゴリに対する長期目標シェアを返す。
    pub(super) fn target_share(&self, cat: AiUnitCategory) -> f32 {
        self.target_shares[cat as usize]
    }

    /// ディリクレ事前重み付き平滑化シェア（体数ベース）を計算する。
    pub(super) fn smoothed_share(
        &self,
        category_counts: &[f32; AI_UNIT_CATEGORY_COUNT],
        cat: AiUnitCategory,
    ) -> f32 {
        let total: f32 = category_counts.iter().sum();
        let t = self.target_shares[cat as usize];
        (category_counts[cat as usize] + PRIOR_UNIT_COUNT * t) / (total + PRIOR_UNIT_COUNT)
    }

    /// 現在のカテゴリ別保有構成（体数比）を目標シェアへ長期的に近づける連続スコア倍率を返す。
    pub(super) fn category_score_multiplier(
        &self,
        category_counts: &[f32; AI_UNIT_CATEGORY_COUNT],
        cat: AiUnitCategory,
        urgent_anti_capture: bool,
    ) -> f32 {
        let cur = self
            .smoothed_share(category_counts, cat)
            .max(MIN_EFFECTIVE_SHARE);
        let tgt = self.target_shares[cat as usize].max(MIN_EFFECTIVE_SHARE);
        let raw = (tgt / cur)
            .powf(CATEGORY_BOOST_EXPONENT)
            .clamp(CATEGORY_MULT_MIN, CATEGORY_MULT_MAX);
        if urgent_anti_capture {
            raw.max(URGENT_ANTI_CAP_MULT_FLOOR)
        } else {
            raw
        }
    }

    /// 同一カテゴリ内での質的バランス倍率を返す。
    /// `戦車` カテゴリに軽機動車両（装甲車）と主力戦車（軽戦車・中戦車・重戦車）が同居するため、
    /// 地形・敵装甲・既存保有比に応じて装甲車の過剰量産を抑えつつ緊急占領阻止は許容し、
    /// 前線安定時は高威力・高突破ユニットの 1 スロット当たり圧縮価値を反映する。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn intra_category_multiplier(
        &self,
        chart: &DamageChart,
        stats: &UnitStats,
        cat: AiUnitCategory,
        light_tank_cat_val: u32,
        armor_tank_cat_val: u32,
        light_tank_cnt: usize,
        my_direct_cnt: usize,
        e_direct_cnt: usize,
        urgent_anti_capture: bool,
    ) -> f32 {
        let is_hammer_tier = stats.cost >= HAMMER_UNIT_MIN_COST;
        let midgame_compressed_front =
            my_direct_cnt >= MIDGAME_MIN_DIRECT_UNITS && my_direct_cnt >= e_direct_cnt;

        match cat {
            AiUnitCategory::Tank => {
                let is_armor_capable = can_hit_armor(chart, stats.unit_type);
                let allowed_light_frac = (0.50
                    * self.openness
                    * (1.0 - 0.55 * self.congestion)
                    * (1.0 - 0.65 * self.e_vehicle_share)
                    * (1.0 - 0.70 * self.tank_impassable_ratio))
                    .clamp(0.10, 0.42);

                if !is_armor_capable {
                    if urgent_anti_capture {
                        return 1.0;
                    }
                    if light_tank_cnt < 2 && self.has_open_properties && self.e_vehicle_share < 0.35
                    {
                        return 1.0;
                    }
                    let total_tank = (light_tank_cat_val.saturating_add(armor_tank_cat_val)) as f32;
                    let cur_light = (light_tank_cat_val as f32
                        + TANK_INTRA_PRIOR_GOLD * allowed_light_frac)
                        / (total_tank + TANK_INTRA_PRIOR_GOLD);
                    let ratio_mult = (allowed_light_frac / cur_light.max(MIN_EFFECTIVE_SHARE))
                        .powf(1.35)
                        .clamp(TANK_LIGHT_MULT_MIN, 1.0);
                    let armor_shortage_penalty =
                        if armor_tank_cat_val < self.e_armored_val && light_tank_cnt > 0 {
                            0.65
                        } else {
                            1.0
                        };
                    (ratio_mult * armor_shortage_penalty).clamp(TANK_LIGHT_MULT_MIN, 1.0)
                } else {
                    let armor_deficit_boost = if self.e_armored_val > armor_tank_cat_val
                        || light_tank_cat_val > armor_tank_cat_val
                    {
                        let gap = (self.e_armored_val.saturating_sub(armor_tank_cat_val) as f32
                            / TANK_INTRA_PRIOR_GOLD)
                            .clamp(0.0, 0.35);
                        let light_skew = if light_tank_cat_val > armor_tank_cat_val {
                            0.20
                        } else {
                            0.0
                        };
                        (1.10 + gap + light_skew).clamp(1.0, 1.45)
                    } else {
                        1.0
                    };
                    let hammer_bonus = if is_hammer_tier && midgame_compressed_front {
                        HAMMER_SLOT_COMPRESSION_BONUS
                    } else {
                        1.0
                    };
                    (armor_deficit_boost * hammer_bonus).clamp(1.0, 1.65)
                }
            }
            AiUnitCategory::Infantry
                if stats.max_movement <= MECH_SLOW_MOVEMENT_CAP
                    && armor_tank_cat_val
                        < self.e_armored_val.max(MECH_ARMOR_SHORTAGE_BASELINE_GOLD) =>
            {
                MECH_ARMOR_SHORTAGE_MULT
            }
            AiUnitCategory::AirForce if is_hammer_tier && midgame_compressed_front => {
                HAMMER_SLOT_COMPRESSION_BONUS
            }
            AiUnitCategory::Fighter
                if stats.cost >= HEAVY_FIGHTER_MIN_COST && midgame_compressed_front =>
            {
                HEAVY_FIGHTER_COMPRESSION_BONUS
            }
            _ => 1.0,
        }
    }

    /// 指定された間接カテゴリ（`自走砲` / `砲台`）の追加生産が目標シェアと安全上限の範囲内か判定する。
    pub(super) fn allows_indirect_category(
        &self,
        category_counts: &[f32; AI_UNIT_CATEGORY_COUNT],
        cat: AiUnitCategory,
        my_indirect_cnt: usize,
    ) -> bool {
        if my_indirect_cnt >= HARD_MAX_INDIRECT_UNITS {
            return false;
        }
        let target_spg = self.target_shares[AiUnitCategory::SelfPropelledGun as usize];
        let target_art = self.target_shares[AiUnitCategory::Artillery as usize];
        if cat == AiUnitCategory::Artillery && target_art <= 0.0 {
            return false;
        }
        if cat == AiUnitCategory::SelfPropelledGun
            && my_indirect_cnt < MIN_GUARANTEED_INDIRECT_UNITS
        {
            return true;
        }
        let combined_smoothed = self
            .smoothed_share(category_counts, AiUnitCategory::SelfPropelledGun)
            + self.smoothed_share(category_counts, AiUnitCategory::Artillery);
        let combined_target = target_spg + target_art;
        combined_smoothed < combined_target * 1.15
            && self.smoothed_share(category_counts, cat) < self.target_shares[cat as usize] * 1.25
    }

    /// 指定カテゴリの平滑化シェアが目標水準（90%）以上に達しているか判定する。
    pub(super) fn is_category_satisfied(
        &self,
        category_counts: &[f32; AI_UNIT_CATEGORY_COUNT],
        cat: AiUnitCategory,
    ) -> bool {
        self.smoothed_share(category_counts, cat) >= self.target_shares[cat as usize] * 0.90
    }

    /// 自軍総保有体数が十分ある状態で、指定カテゴリが目標シェア（105%）を超過しているか判定する。
    pub(super) fn is_category_over_target(
        &self,
        category_counts: &[f32; AI_UNIT_CATEGORY_COUNT],
        cat: AiUnitCategory,
    ) -> bool {
        let total: f32 = category_counts.iter().sum();
        total >= OVER_TARGET_MIN_TOTAL_UNITS
            && self.smoothed_share(category_counts, cat) > self.target_shares[cat as usize] * 1.05
    }

    /// 前線優位・物件非劣勢時に次ターンの上位突破ハンマー（`MdTank`/`Bomber`/`HeavySpGun`/`TankZ`等）購入へ向けたソフト貯蓄下限額（Gold）を返す。
    pub(super) fn hammer_saving_floor(
        &self,
        scan: &BoardScan,
        category_counts: &[f32; AI_UNIT_CATEGORY_COUNT],
        armor_tank_cat_val: u32,
        my_direct_cnt: usize,
        e_direct_cnt: usize,
        profiles: &WeaponProfileTable,
    ) -> u32 {
        if my_direct_cnt < MIDGAME_MIN_DIRECT_UNITS
            || my_direct_cnt < e_direct_cnt + 2
            || self.is_behind_on_properties
            || !self.is_category_satisfied(category_counts, AiUnitCategory::Infantry)
            || scan.open_properties.len() > 3
            || scan.my_income < MIN_INCOME_FOR_HAMMER_SAVING
        {
            return 0;
        }

        let need_tank = self.smoothed_share(category_counts, AiUnitCategory::Tank)
            < self.target_shares[AiUnitCategory::Tank as usize] * 0.85
            || armor_tank_cat_val < self.e_armored_val;
        let need_air = self.target_shares[AiUnitCategory::AirForce as usize] > 0.0
            && self.smoothed_share(category_counts, AiUnitCategory::AirForce)
                < self.target_shares[AiUnitCategory::AirForce as usize] * 0.85;
        let need_spg = self.target_shares[AiUnitCategory::SelfPropelledGun as usize] > 0.0
            && self.smoothed_share(category_counts, AiUnitCategory::SelfPropelledGun)
                < self.target_shares[AiUnitCategory::SelfPropelledGun as usize] * 0.80;
        let need_fighter = self.target_shares[AiUnitCategory::Fighter as usize] > 0.0
            && self.smoothed_share(category_counts, AiUnitCategory::Fighter)
                < self.target_shares[AiUnitCategory::Fighter as usize] * 0.70;
        if !need_tank && !need_air && !need_spg && !need_fighter {
            return 0;
        }

        scan.available_types
            .iter()
            .filter(|(ut, stats)| {
                stats.cost >= HAMMER_UNIT_MIN_COST
                    && stats.cost > scan.funds
                    && scan
                        .production_facilities
                        .iter()
                        .any(|&(_, terrain)| scan.can_produce(terrain, *ut))
            })
            .filter(|(_, stats)| {
                let cat =
                    resolve_ai_category(&scan.master_data, &scan.damage_chart, profiles, stats);
                match cat {
                    AiUnitCategory::Tank => need_tank,
                    AiUnitCategory::AirForce => need_air,
                    AiUnitCategory::SelfPropelledGun => need_spg,
                    AiUnitCategory::Fighter => need_fighter,
                    _ => false,
                }
            })
            .filter_map(|(_, stats)| {
                let needed = stats.cost.saturating_sub(scan.my_income);
                (1..=MAX_HAMMER_SAVING_FLOOR_GOLD)
                    .contains(&needed)
                    .then_some(needed)
            })
            .min()
            .unwrap_or(0)
    }
}
