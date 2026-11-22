//! 占領・直接戦闘・対空・間接火網を単一目的関数で評価するポートフォリオ最適化。

use super::reach::{OCC_FRIENDLY, ReachMap, reachable_tiles, unit_distance_field};
use super::threats::{
    HORIZON, ROUND_DISCOUNT, ThreatContext, expected_damage, is_immobile_unit, max_base_damage,
    terrain_def_at,
};
use super::{UnifiedBoardExtras, WeaponProfileTable};
use crate::ai::islands::IslandMap;
use crate::ai::production::commit::PurchasePurpose;
use crate::ai::v4::operation::{OperationKind, SlotKind};
use crate::ai::v4::trace::{ProductionDecision, ProductionPlanTrace, ProductionStepTrace};
use crate::ai::v4::{
    BoardScan, PlannedDeployment, PlannedProduction, UnitSnapshot, campaign_execution, deployment,
};
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, PlayerId, UnitStats};
use crate::events::ProduceUnitCommand;
use crate::resources::{MovementType, Terrain, UnitType};
use std::collections::BTreeMap;

/// 資金 1,000G あたりの共通限界費用。
const LAM0_PER_1K: f32 = 10.5;
/// 空き施設1口あたりの通常運転資金基準。
const REF_WORKING_CAPITAL_PER_FAC: f32 = 2800.0;
/// 運転資金基準の下限（序盤や低資金盤面では基準限界費用を維持）。
const MIN_WORKING_CAPITAL: f32 = 18000.0;
/// 運転資金超過額がこの金額に達するごとに実効資金限界費用の分母を +1.0 増加させる基準額。
const SURPLUS_LAM_HALF_LIFE_FUNDS: f32 = 14000.0;
/// 過剰資金時にも無駄買いを抑止するための実効資金限界費用の下限（1,000Gあたり）。
const MIN_SURPLUS_LAM_PER_1K: f32 = 1.5;
/// 生スコア上位から確保するビーム枠数。
const BEAM_RAW_SLOTS: usize = 6;
/// 資金 1,000G あたりスコア（費用対効果）上位から確保するビーム枠数。
const BEAM_EFF_SLOTS: usize = 6;
/// ビームサーチ幅（生スコア上位枠 + 費用対効果上位枠）。
const BEAM_WIDTH: usize = BEAM_RAW_SLOTS + BEAM_EFF_SLOTS;
/// 同一兵種構成（施設配置違いのみ）のビーム占有上限。
const BEAM_MAX_PER_COMPOSITION: usize = 2;
/// 1 ターンあたりの最大生産数。
const MAX_PURCHASES_PER_TURN: usize = 8;
/// 役割横断の候補プール上限（合計 14 + 36 + 12 = 62 <= 64 でビットマスク収容）。
const MAX_POOL_CAPTURE: usize = 14;
const MAX_POOL_DIRECT: usize = 36;
const MAX_POOL_INDIRECT: usize = 12;
/// 直接・間接候補の1兵種あたり保持上限。
const MAX_CANDS_PER_DIRECT_TYPE: usize = 6;
const MAX_CANDS_PER_INDIRECT_TYPE: usize = 4;
/// 廉価前衛（歩兵）として多工場同時出荷枠を優先確保する上限コスト。
const CHEAP_FILLER_MAX_COST: u32 = 1200;
/// 単体採用候補の最低 net スコア足切り。
const DIRECT_MIN_SOLO_NET: f32 = -20.0;
const INDIRECT_MIN_SOLO_NET: f32 = -15.0;
/// 直接・間接1ユニットあたり保持する攻撃ターゲット数上限（探索枝刈り用）。
const MAX_ATTACKS_PER_DIRECT: usize = 10;
const MAX_TARGETS_PER_INDIRECT: usize = 12;

/// 占領経済価値の基準係数（標準都市 durability=200 の cap_turns=2 割引を補正）。
const CAPTURE_BASE_VALUE: f32 = 88.0;
/// 占領到着ターンの時間割引率。
const CAPTURE_DISCOUNT: f32 = 0.85;
/// 収入倍率の上限（首都 4,000G 等の過剰重み抑止）。
const MAX_CAPTURE_INCOME_MULT: f32 = 2.5;
/// 首都生産圏内の物件価値ボーナス倍率。
const PROD_MOUTH_VALUE_MULT: f32 = 1.6;
/// 敵所有物件・敵首都の占領価値倍率。
const ENEMY_PROP_VALUE_MULT: f32 = 1.4;
const ENEMY_CAPITAL_VALUE_MULT: f32 = 1.25;
/// 占領評価対象の上位物件数上限。
const MAX_CAPTURE_PROPS: usize = 24;
/// 物件未取得時のデフォルト耐久度・占領1回あたりの進行量。
const DEFAULT_PROPERTY_DURABILITY: u32 = 200;
const CAPTURE_PROGRESS_PER_TURN: u32 = 100;
/// 占領安全性判定の敵直接脅威・施設近接係数。
const CAP_THREAT_1T_LETHAL_DMG: u32 = 60;
const CAP_THREAT_1T_HIGH_DMG: u32 = 30;
const CAP_SAFETY_1T_LETHAL: f32 = 0.35;
const CAP_SAFETY_1T_HIGH: f32 = 0.60;
const CAP_THREAT_2T_HEAVY_DMG: u32 = 45;
const CAP_SAFETY_2T_HEAVY_MULT: f32 = 0.75;
const CAP_ENEMY_FAC_GUARD_RADIUS: u32 = 3;
const CAP_ENEMY_GUARD_MIN_UNITS: usize = 2;
const CAP_SAFETY_GUARDED_ENEMY_PROP_MULT: f32 = 0.25;
/// 敵占領ユニットの方が自軍より先に到達できる中立物件の占領安全性割引。
const CAP_SAFETY_ENEMY_CONTESTED_NEUTRAL_MULT: f32 = 0.25;
/// 統合評価で新規追加する航空輸送ユニットの既存上限。
const MAX_EXISTING_AIR_TRANSPORTS: usize = 2;
/// 航空輸送による積載・降下オーバーヘッド（ターン数）。
const AIRLIFT_LOAD_DROP_OVERHEAD_TURNS: u32 = 2;
/// 徒歩占領より空輸を優先するために必要な最小短縮ターン数。
const AIRLIFT_MIN_TURN_GAIN: u32 = 2;
/// 多積載・地形越え継続運用を加味した空輸占領の経済価値倍率。
const AIRLIFT_CAPTURE_VALUE_MULT: f32 = 1.35;

/// 既存味方によるターゲット充足判定の最小期待ダメージ（対空）。
const DIRECT_EXISTING_COV_MIN_DMG: u32 = 20;
/// 既存味方による地上敵のカバー充足判定の最小期待ダメージ。
const GROUND_EXISTING_COV_MIN_DMG: u32 = 30;
/// 航空敵に対する遠方・間接味方によるカバー判定の最小期待ダメージ・到達外基準寄与。
const AIR_EXISTING_COV_MIN_DMG: u32 = 35;
const AIR_EXISTING_COV_GLOBAL_PROX: f32 = 0.60;
/// 既存味方の充足度 1 体分あたりの航空ターゲット需要指数減衰係数。
const DIRECT_EXISTING_COV_EXP: f32 = 1.15;
/// 既存味方の充足度 1 体分あたりの地上ターゲット需要逆数飽和係数。
const GROUND_EXISTING_COV_DECAY: f32 = 0.45;
/// 敵占領ユニットに対して決定的火力（反撃を抑えて占領妨害可能）とみなす最小期待ダメージ。
const ANTI_CAPTURER_DECISIVE_DMG: u32 = 55;
/// 決定的火力未満の味方（自軍歩兵等）が敵占領ユニット需要を充足する際の減衰倍率。
const ANTI_CAPTURER_SOFT_COV_MULT: f32 = 0.40;

/// 直接ユニット1ターンあたりの攻撃可能ラウンド数上限。
const DIRECT_MAX_ENGAGEMENTS: u32 = 2;
/// 直接ユニットが戦闘継続する自己HP下限。
const DIRECT_MIN_CONTINUE_HP: u32 = 20;
/// 敵ターン反撃被弾の期待係数と集中砲火2体目の寄与係数。
const DIRECT_RETAL_FACTOR: f32 = 0.42;
/// 航空ユニット攻撃後に敵ターンで受ける対空・制空迎撃の期待係数（地形防御・ZOC遮蔽なし）。
const AIR_RETAL_FACTOR: f32 = 0.72;
const DIRECT_SECOND_RETAL_WEIGHT: f32 = 0.35;
/// 直接ユニット HP 損耗 100% あたりの資産価値損失（1,000Gあたり）。
const DIRECT_ATTRITION_PER_1K: f32 = 4.8;
/// 敵直接敵ユニットの反撃・移動投影における最大移動力上限。
const ENEMY_RETAL_MAX_MOVE: u32 = 6;
/// 撃破後・被弾有りの直接攻撃1回あたりの最低消耗HP。
const DIRECT_MIN_POSITIVE_WEAR: u32 = 5;

/// スクリーン（地上前線面）1体あたりのZOC・手数・肉壁基礎価値。
const BODY_SCREEN_BASE: f32 = 20.0;
/// 高耐久前衛の追加スクリーン価値係数。
const SCREEN_DURABILITY_BONUS: f32 = 10.0;
/// スクリーン機動力補正の基準移動力と下限・上限倍率。
const SCREEN_MOBILITY_REF: f32 = 4.0;
const SCREEN_MOBILITY_MIN_MULT: f32 = 0.65;
const SCREEN_MOBILITY_MAX_MULT: f32 = 1.25;
/// 移動力2以下の鈍足地上ユニットへのスクリーン価値割引。
const SCREEN_SLOW_MAX_MOVEMENT: u32 = 2;
const SCREEN_SLOW_UNIT_MULT: f32 = 0.65;
/// 自軍直接体数が敵より少ない際の補充緊急度倍率。
const SCREEN_OUTNUMBERED_MULT: f32 = 1.25;
/// 敵存在時に target_screen 到達後も維持する前線補充スクリーン価値の下限減衰率。
const SCREEN_SATURATED_FLOOR_FACTOR: f32 = 0.45;
/// 直接ユニットの最大評価到着ラウンド（HORIZON超過分は追加時間割引で評価）。
const MAX_DIRECT_ARRIVAL_ROUND: usize = 10;
/// 直接ターゲットスクリーン上限。
const MAX_TARGET_SCREEN: usize = 44;
/// 撃破未到達ダメージを無力化・削り価値へ変換する基準値。
const CRIPPLE_REF_DAMAGE: f32 = 85.0;
/// 間接ユニットの期待資産損耗係数（1,000Gあたり）。
const ASSET_RISK_PER_1K: f32 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateKind {
    Capture,
    Transport,
    Direct,
    Indirect,
}

#[derive(Debug, Clone)]
struct CombatAttackEntry {
    enemy_idx: usize,
    dmg: u32,
    cripple_factor: f32,
    /// 反撃側のHPが100%残っている仮定での反撃ダメージ。
    dmg_cnt_100: f32,
    /// 攻撃実行後の敵ターンに攻撃地点で受ける期待被弾ダメージ。
    retal_dmg: f32,
    /// `HORIZON` 超過分の追加時間割引率。
    late_disc: f32,
}

#[derive(Debug, Clone)]
struct IndirectGunSpec {
    delay: usize,
    max_ammo: u32,
    initial_ammo: u32,
    supplied: bool,
    surv: f32,
    /// round 0 で射撃可能な `(enemy_idx, dmg, cripple_factor)`。
    d0: Vec<(usize, u32, f32)>,
    /// round >= 1 で射撃可能な `(enemy_idx, dmg, cripple_factor)`。
    dn: Vec<(usize, u32, f32)>,
}

#[derive(Debug, Clone)]
struct DirectUnitSpec {
    hp: u32,
    cost: u32,
    /// 各敵ごとの到達遅延（ラウンド）と交戦効果。
    attacks: Vec<(usize, CombatAttackEntry)>,
}

#[derive(Debug, Clone)]
enum PoolPayload {
    Capture {
        /// 既存占領役より到着を早められる `(capture_props index, delta_cap)` の降順リスト。
        prop_deltas: Vec<(usize, f32)>,
        screen_value: f32,
    },
    Transport {
        /// 空輸によって早期化または到達可能になる `(capture_props index, delta_cap)` の降順リスト。
        prop_deltas: Vec<(usize, f32)>,
    },
    Direct {
        unit_spec: DirectUnitSpec,
        anchor: GridPosition,
        priority_enemies: Vec<bevy_ecs::prelude::Entity>,
        screen_value: f32,
    },
    Indirect {
        gun_spec: IndirectGunSpec,
        block_pen: f32,
        asset_risk: f32,
        anchor: GridPosition,
        priority_enemies: Vec<bevy_ecs::prelude::Entity>,
    },
}

#[derive(Debug, Clone)]
struct PoolCandidate {
    kind: CandidateKind,
    unit_type: UnitType,
    cost: u32,
    facility: GridPosition,
    /// `scan.free_facilities` に対応する施設インデックス（ビットマスク高速判定用）。
    fac_idx: usize,
    /// 間接射点（それ以外は施設座標）。
    pos_key: GridPosition,
    solo_net: f32,
    payload: PoolPayload,
}

#[derive(Debug, Clone)]
struct CapturePropertyInfo {
    pos: GridPosition,
    economic_value: f32,
    cap_turns: u32,
    base_arrival: u32,
}

#[derive(Debug, Clone)]
struct BeamState {
    score: f32,
    delta_combat: f32,
    delta_cap: f32,
    delta_screen: f32,
    block_pen: f32,
    asset_risk: f32,
    spent: u32,
    capture_count: usize,
    transport_count: usize,
    chosen_mask: u64,
    fac_mask: u64,
    chosen: Vec<usize>,
    used_facs: Vec<GridPosition>,
    used_indirect_pos: Vec<GridPosition>,
}

/// 戦闘シミュレーションで使い回す配列作業領域（ヒープ再確保回避用）。
struct CombatScratch {
    rem_hp: Vec<u32>,
    ammos: Vec<u32>,
    dir_hp: Vec<u32>,
    dir_engagements: Vec<u32>,
    ind_used: Vec<bool>,
    dir_used: Vec<bool>,
}

impl CombatScratch {
    fn with_enemy_len(enemy_len: usize) -> Self {
        Self {
            rem_hp: Vec::with_capacity(enemy_len),
            ammos: Vec::with_capacity(16),
            dir_hp: Vec::with_capacity(8),
            dir_engagements: Vec::with_capacity(8),
            ind_used: Vec::with_capacity(16),
            dir_used: Vec::with_capacity(8),
        }
    }
}

/// ビーム検索の組み合わせ評価で使い回す参照・割当バッファ。
struct BeamEvalBuffers<'a> {
    extra_inds: Vec<&'a IndirectGunSpec>,
    dirs: Vec<&'a DirectUnitSpec>,
    cap_cands: Vec<(usize, &'a [(usize, f32)])>,
    cap_used_cand: Vec<bool>,
    cap_used_prop: Vec<bool>,
}

impl<'a> BeamEvalBuffers<'a> {
    fn with_capacities(max_purchases: usize, num_props: usize) -> Self {
        Self {
            extra_inds: Vec::with_capacity(max_purchases),
            dirs: Vec::with_capacity(max_purchases),
            cap_cands: Vec::with_capacity(max_purchases),
            cap_used_cand: Vec::with_capacity(max_purchases),
            cap_used_prop: Vec::with_capacity(num_props),
        }
    }
}

/// 統合評価で当ターンの購入リストとトレースを生成する。
pub(in crate::ai::v4) fn plan_unified_production(
    scan: &BoardScan,
    extras: &UnifiedBoardExtras,
    player_id: PlayerId,
    turn: u32,
    allow_structural_slots: bool,
    recent_enemy: &[(u32, UnitType)],
) -> (Vec<PlannedProduction>, ProductionPlanTrace) {
    let start_instant = std::time::Instant::now();
    let mut plan_trace =
        ProductionPlanTrace::new(player_id, scan.funds, scan.free_facilities.len());
    if scan.free_facilities.is_empty() || scan.funds == 0 {
        return (Vec::new(), plan_trace);
    }

    let profiles = WeaponProfileTable::from_scan(scan);
    let threat_ctx = ThreatContext::build(scan, extras, &profiles, recent_enemy);
    let island_map = IslandMap::analyze(&scan.map);

    // 全 (始点, 移動タイプ) 向けの純地形距離場を必要時キャッシュする
    let mut dist_cache: HashMap<(GridPosition, MovementType), ReachMap> = HashMap::default();
    let mut dist_between =
        |from: GridPosition, to: GridPosition, mt: MovementType| -> Option<u32> {
            let field = dist_cache
                .entry((from, mt))
                .or_insert_with(|| unit_distance_field(&scan.map, &scan.master_data, from, mt));
            field.get(to)
        };

    // --- 1. 既存味方ユニットの仕様化とベースライン戦闘価値 ---
    let mut base_indirects = Vec::new();
    let mut existing_capturers = Vec::new();
    let mut f_dir_positions = HashSet::default();

    for u in &scan.my_units {
        let ut = u.stats.unit_type;
        if u.stats.can_capture {
            existing_capturers.push(u.clone());
        }
        if profiles.is_indirect(ut) {
            let (raw_surv, _) = threat_ctx.firing_tile_survival(scan, &profiles, u.pos, &u.stats);
            let ammo1 = u
                .entity
                .and_then(|e| extras.unit_ammo.get(&e).copied())
                .map_or(u.stats.max_ammo1, |a| a.0);
            let spec = build_indirect_gun_spec(
                scan,
                &profiles,
                &threat_ctx,
                &u.stats,
                u.pos,
                0,
                u.hp,
                ammo1,
                raw_surv.max(0.30),
            );
            base_indirects.push(spec);
        } else {
            f_dir_positions.insert(u.pos);
        }
    }

    // 既存直接味方による敵ごとのカバー充足度から、新規直接購入の需要倍率を算出
    let direct_need = compute_direct_need(scan, extras, &profiles, &threat_ctx, &mut dist_between);
    let mut combat_scratch = CombatScratch::with_enemy_len(threat_ctx.enemy_targets.len());

    // ベースライン戦闘価値は既存間接ユニットのみで計算（直接味方は direct_need 側で反映し二重計上を防ぐ）
    let base_combat_val = simulate_joint_combat(
        &threat_ctx.enemy_targets,
        &direct_need,
        &base_indirects,
        &[],
        &[],
        &mut combat_scratch,
    );

    // --- 2. 占領ベースライン ---
    let mut capture_props: Vec<CapturePropertyInfo> = Vec::new();
    if allow_structural_slots {
        capture_props = build_capture_baseline(
            scan,
            extras,
            &profiles,
            &existing_capturers,
            &mut dist_between,
        );
    }

    // 地上ユニットのスクリーン価値用の動的目標前線体数
    let existing_direct_cnt = f_dir_positions.len();
    let total_existing_indirect_cnt = base_indirects.len();
    let target_screen = if threat_ctx.e_direct_cnt == 0 {
        4
    } else {
        ((threat_ctx.e_direct_cnt * 5) / 4 + 6 + total_existing_indirect_cnt.saturating_mul(2))
            .clamp(8, MAX_TARGET_SCREEN)
    };
    let can_add_land_screen =
        threat_ctx.can_reach_enemy_via_land || threat_ctx.has_reachable_land_objective;
    let has_enemy = !scan.enemy_units.is_empty() || !extras.enemy_properties.is_empty();
    let eff_lam = effective_lam_per_1k(scan.funds, scan.free_facilities.len());

    // --- 3. 役割別候補の生成 ---
    let mut fac_idx_by_pos: HashMap<GridPosition, usize> = HashMap::default();
    for &(fac, _) in &scan.free_facilities {
        let next_idx = fac_idx_by_pos.len();
        fac_idx_by_pos.entry(fac).or_insert(next_idx);
    }

    let mut capture_pool: Vec<PoolCandidate> = Vec::new();
    let mut direct_pool: Vec<PoolCandidate> = Vec::new();
    let mut indirect_pool: Vec<PoolCandidate> = Vec::new();

    // 占領・空輸輸送候補（施設多様性を維持しつつ施設×兵種ごとに全改善物件リストを保持）
    if allow_structural_slots && !capture_props.is_empty() {
        let existing_air_transports = scan
            .my_units
            .iter()
            .filter(|u| u.stats.movement_type == MovementType::Air && u.stats.max_cargo > 0)
            .count();
        let can_add_air_transport = existing_air_transports < MAX_EXISTING_AIR_TRANSPORTS;
        let capture_unit_types: Vec<UnitType> = scan
            .available_types
            .iter()
            .filter(|(_, s)| s.can_capture)
            .map(|(ut, _)| *ut)
            .collect();

        let mut first_per_fac: Vec<PoolCandidate> = Vec::new();
        let mut extra_capture_cands: Vec<PoolCandidate> = Vec::new();
        for &(fac, terrain) in &scan.free_facilities {
            let fac_idx = fac_idx_by_pos[&fac];
            let mut per_fac: Vec<PoolCandidate> = Vec::new();
            for &(ut, ref stats) in &scan.available_types {
                if stats.cost == 0 || stats.cost > scan.funds || !scan.can_produce(terrain, ut) {
                    continue;
                }
                if stats.can_capture {
                    let scr_val = if can_add_land_screen {
                        screen_marginal_value(
                            scan,
                            threat_ctx.e_direct_cnt,
                            stats,
                            existing_direct_cnt,
                            target_screen,
                            has_enemy,
                        )
                    } else {
                        0.0
                    };
                    let mut prop_deltas: Vec<(usize, f32)> = Vec::new();
                    for (pidx, cp) in capture_props.iter().enumerate() {
                        let Some(cost_mp) = dist_between(fac, cp.pos, stats.movement_type) else {
                            continue;
                        };
                        let eta = cost_mp.div_ceil(stats.max_movement.max(1));
                        let new_arrival = eta.saturating_add(1).saturating_add(cp.cap_turns);
                        if new_arrival >= cp.base_arrival {
                            continue;
                        }
                        let base_disc = if cp.base_arrival == u32::MAX {
                            0.0
                        } else {
                            CAPTURE_DISCOUNT.powi(cp.base_arrival as i32)
                        };
                        let new_disc = CAPTURE_DISCOUNT.powi(new_arrival as i32);
                        let delta_cap = cp.economic_value * (new_disc - base_disc);
                        if delta_cap > 0.0 {
                            prop_deltas.push((pidx, delta_cap));
                        }
                    }
                    if prop_deltas.is_empty() {
                        continue;
                    }
                    prop_deltas.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                    let best_delta_cap = prop_deltas[0].1;
                    let eff_scr_val = scr_val
                        * screen_decay_factor(existing_direct_cnt, target_screen, has_enemy);
                    let solo_net = best_delta_cap + eff_scr_val - budget_cost(stats.cost, eff_lam);
                    per_fac.push(PoolCandidate {
                        kind: CandidateKind::Capture,
                        unit_type: ut,
                        cost: stats.cost,
                        facility: fac,
                        fac_idx,
                        pos_key: fac,
                        solo_net,
                        payload: PoolPayload::Capture {
                            prop_deltas,
                            screen_value: scr_val,
                        },
                    });
                } else if can_add_air_transport
                    && stats.movement_type == MovementType::Air
                    && stats.max_cargo > 0
                    && stats
                        .loadable_unit_types
                        .iter()
                        .any(|lt| capture_unit_types.contains(lt))
                {
                    let mut prop_deltas: Vec<(usize, f32)> = Vec::new();
                    for (pidx, cp) in capture_props.iter().enumerate() {
                        let Some(cost_mp) = dist_between(fac, cp.pos, MovementType::Air) else {
                            continue;
                        };
                        let air_eta = cost_mp.div_ceil(stats.max_movement.max(1));
                        let airlift_arrival = air_eta
                            .saturating_add(AIRLIFT_LOAD_DROP_OVERHEAD_TURNS)
                            .saturating_add(cp.cap_turns);
                        let accelerates_capture = cp.base_arrival == u32::MAX
                            || airlift_arrival.saturating_add(AIRLIFT_MIN_TURN_GAIN)
                                <= cp.base_arrival;
                        if !accelerates_capture {
                            continue;
                        }
                        let base_disc = if cp.base_arrival == u32::MAX {
                            0.0
                        } else {
                            CAPTURE_DISCOUNT.powi(cp.base_arrival as i32)
                        };
                        let new_disc = CAPTURE_DISCOUNT.powi(airlift_arrival as i32);
                        let delta_cap =
                            cp.economic_value * (new_disc - base_disc) * AIRLIFT_CAPTURE_VALUE_MULT;
                        if delta_cap > 0.0 {
                            prop_deltas.push((pidx, delta_cap));
                        }
                    }
                    if prop_deltas.is_empty() {
                        continue;
                    }
                    prop_deltas.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                    let best_delta_cap = prop_deltas[0].1;
                    let solo_net = best_delta_cap - budget_cost(stats.cost, eff_lam);
                    per_fac.push(PoolCandidate {
                        kind: CandidateKind::Transport,
                        unit_type: ut,
                        cost: stats.cost,
                        facility: fac,
                        fac_idx,
                        pos_key: fac,
                        solo_net,
                        payload: PoolPayload::Transport { prop_deltas },
                    });
                }
            }
            per_fac.sort_by(|a, b| {
                b.solo_net
                    .total_cmp(&a.solo_net)
                    .then_with(|| a.cost.cmp(&b.cost))
                    .then_with(|| a.unit_type.as_str().cmp(b.unit_type.as_str()))
            });
            let mut it = per_fac.into_iter();
            if let Some(best_cand) = it.next() {
                first_per_fac.push(best_cand);
                extra_capture_cands.extend(it);
            }
        }
        let sort_cap = |a: &PoolCandidate, b: &PoolCandidate| {
            b.solo_net
                .total_cmp(&a.solo_net)
                .then_with(|| a.cost.cmp(&b.cost))
                .then_with(|| a.facility.y.cmp(&b.facility.y))
                .then_with(|| a.facility.x.cmp(&b.facility.x))
                .then_with(|| a.unit_type.as_str().cmp(b.unit_type.as_str()))
        };
        first_per_fac.sort_by(sort_cap);
        extra_capture_cands.sort_by(sort_cap);
        capture_pool.extend(first_per_fac);
        if capture_pool.len() > MAX_POOL_CAPTURE {
            capture_pool.truncate(MAX_POOL_CAPTURE);
        } else {
            let rem = MAX_POOL_CAPTURE - capture_pool.len();
            capture_pool.extend(extra_capture_cands.into_iter().take(rem));
        }
    }

    // 直接・対空・航空候補（全候補生成後に空海・無武装輸送車の除外、廉価前衛枠先取り・兵種別枠を適用）
    let mut raw_direct_cands: Vec<(bool, PoolCandidate)> = Vec::new();
    for &(fac, terrain) in &scan.free_facilities {
        let fac_idx = fac_idx_by_pos[&fac];
        for &(ut, ref stats) in &scan.available_types {
            if stats.cost == 0
                || stats.cost > scan.funds
                || is_non_combat_transport(&scan.damage_chart, stats)
                || profiles.is_indirect(ut)
                || !profiles.has_any_weapon(ut)
                || !scan.can_produce(terrain, ut)
            {
                continue;
            }
            let is_air = stats.movement_type == MovementType::Air;
            let is_ship = stats.movement_type == MovementType::Ship;
            if !is_air && !is_ship && !can_add_land_screen {
                // 海を隔てた孤島盤面で島外の敵へ繋がらない地上直接戦闘兵は作らない
                let any_same_island_enemy = scan
                    .enemy_units
                    .iter()
                    .any(|e| dist_between(fac, e.pos, stats.movement_type).is_some());
                if !any_same_island_enemy {
                    continue;
                }
            }

            let spec = build_direct_spec(
                scan,
                &profiles,
                &threat_ctx,
                &direct_need,
                stats,
                fac,
                100,
                (stats.max_ammo1, stats.max_ammo2),
                &mut dist_between,
            );
            let hits_ground = can_hit_ground(&scan.damage_chart, ut);
            let scr_val = if !is_air && !is_ship && can_add_land_screen && hits_ground {
                screen_marginal_value(
                    scan,
                    threat_ctx.e_direct_cnt,
                    stats,
                    existing_direct_cnt,
                    target_screen,
                    has_enemy,
                )
            } else {
                0.0
            };
            if spec.attacks.is_empty() && scr_val <= 0.0 {
                continue;
            }

            let dir_slice = [&spec];
            let delta_c = simulate_joint_combat(
                &threat_ctx.enemy_targets,
                &direct_need,
                &base_indirects,
                &[],
                &dir_slice,
                &mut combat_scratch,
            ) - base_combat_val;
            // 対地能力を持たない純対空ユニットは余剰資金時の割引や赤字許容に便乗させない
            let cand_lam = if hits_ground { eff_lam } else { LAM0_PER_1K };
            let eff_scr_val =
                scr_val * screen_decay_factor(existing_direct_cnt, target_screen, has_enemy);
            let solo_net = delta_c + eff_scr_val - budget_cost(stats.cost, cand_lam);
            let min_net = if hits_ground {
                DIRECT_MIN_SOLO_NET
            } else {
                0.0
            };
            if solo_net <= min_net {
                continue;
            }

            let is_cheap_filler =
                stats.can_capture && stats.cost <= CHEAP_FILLER_MAX_COST && hits_ground;
            let (anchor, priority_enemies) =
                direct_deployment_target(scan, &threat_ctx, &direct_need, &spec, fac);
            raw_direct_cands.push((
                is_cheap_filler,
                PoolCandidate {
                    kind: CandidateKind::Direct,
                    unit_type: ut,
                    cost: stats.cost,
                    facility: fac,
                    fac_idx,
                    pos_key: fac,
                    solo_net,
                    payload: PoolPayload::Direct {
                        unit_spec: spec,
                        anchor,
                        priority_enemies,
                        screen_value: scr_val,
                    },
                },
            ));
        }
    }
    raw_direct_cands.sort_by(|a, b| {
        b.1.solo_net
            .total_cmp(&a.1.solo_net)
            .then_with(|| a.1.cost.cmp(&b.1.cost))
            .then_with(|| a.1.facility.y.cmp(&b.1.facility.y))
            .then_with(|| a.1.facility.x.cmp(&b.1.facility.x))
            .then_with(|| a.1.unit_type.as_str().cmp(b.1.unit_type.as_str()))
    });
    let mut direct_type_counts: HashMap<UnitType, usize> = HashMap::default();
    let mut taken_direct = vec![false; raw_direct_cands.len()];
    // 1パス目: 中盤の頭数補充のため、黒字の廉価前衛候補を最大 MAX_PURCHASES_PER_TURN 件まで先取り確保
    for (idx, &(is_filler, ref cand)) in raw_direct_cands.iter().enumerate() {
        if direct_pool.len() >= MAX_PURCHASES_PER_TURN {
            break;
        }
        if !is_filler || cand.solo_net <= 0.0 {
            continue;
        }
        let cnt = direct_type_counts.entry(cand.unit_type).or_insert(0);
        if *cnt < MAX_PURCHASES_PER_TURN {
            *cnt += 1;
            taken_direct[idx] = true;
            direct_pool.push(cand.clone());
        }
    }
    // 2パス目: 残り枠を solo_net 降順・兵種別上限付きで充填
    for (idx, (is_filler, cand)) in raw_direct_cands.into_iter().enumerate() {
        if direct_pool.len() >= MAX_POOL_DIRECT {
            break;
        }
        if taken_direct[idx] {
            continue;
        }
        let limit = if is_filler {
            MAX_PURCHASES_PER_TURN
        } else {
            MAX_CANDS_PER_DIRECT_TYPE
        };
        let cnt = direct_type_counts.entry(cand.unit_type).or_insert(0);
        if *cnt < limit {
            *cnt += 1;
            direct_pool.push(cand);
        }
    }

    // 間接候補（自走間接＋固定砲台、施設・射点組み合わせ上位）
    let vacatable_direct: HashSet<GridPosition> = f_dir_positions
        .iter()
        .copied()
        .filter(|pos| threat_ctx.is_ground_supply(*pos))
        .collect();

    for &(ut, ref stats) in &scan.available_types {
        if stats.cost == 0 || stats.cost > scan.funds || !profiles.is_indirect(ut) {
            continue;
        }
        let immobile = is_immobile_unit(stats);
        let hits_ground = can_hit_ground(&scan.damage_chart, ut);
        // (射点, 遅延ラウンド) -> 到達可能な生産施設の組
        let mut pos_to_facs: BTreeMap<((usize, usize), usize), Vec<GridPosition>> = BTreeMap::new();
        for &(fac, terrain) in &scan.free_facilities {
            if !scan.can_produce(terrain, ut) {
                continue;
            }
            pos_to_facs
                .entry(((fac.x, fac.y), 0))
                .or_default()
                .push(fac);
            if !immobile {
                let r1 = reachable_tiles(
                    &scan.map,
                    &scan.master_data,
                    &threat_ctx.occ,
                    fac,
                    stats.movement_type,
                    stats.max_movement,
                    stats.max_fuel,
                    OCC_FRIENDLY,
                    1,
                    Some(&vacatable_direct),
                );
                for (p, _) in r1.iter() {
                    if p != fac {
                        pos_to_facs.entry(((p.x, p.y), 1)).or_default().push(fac);
                    }
                }
            }
        }

        let mut scored_for_type: Vec<PoolCandidate> = Vec::new();
        for (((fx, fy), delay), facs) in pos_to_facs {
            let fpos = GridPosition { x: fx, y: fy };
            let block_pen = threat_ctx.facility_block_penalty(fpos, stats);
            if block_pen >= 1.0e8 {
                continue;
            }
            let (raw_surv, _) = threat_ctx.firing_tile_survival(scan, &profiles, fpos, stats);
            if raw_surv <= 0.25 {
                continue;
            }
            let eff_surv = raw_surv * ThreatContext::immobile_discount(stats);
            let asset_risk = (1.0 - raw_surv) * (stats.cost as f32 / 1000.0) * ASSET_RISK_PER_1K;
            let gun_spec = build_indirect_gun_spec(
                scan,
                &profiles,
                &threat_ctx,
                stats,
                fpos,
                delay,
                100,
                stats.max_ammo1,
                eff_surv,
            );
            if gun_spec.d0.is_empty() && gun_spec.dn.is_empty() {
                continue;
            }
            let ind_slice = [&gun_spec];
            let delta_c = simulate_joint_combat(
                &threat_ctx.enemy_targets,
                &direct_need,
                &base_indirects,
                &ind_slice,
                &[],
                &mut combat_scratch,
            ) - base_combat_val;
            // 対地能力を持たない純対空ユニットは余剰資金時の割引や赤字許容に便乗させない
            let cand_lam = if hits_ground { eff_lam } else { LAM0_PER_1K };
            let solo_net = delta_c - block_pen - asset_risk - budget_cost(stats.cost, cand_lam);
            let min_net = if hits_ground {
                INDIRECT_MIN_SOLO_NET
            } else {
                0.0
            };
            if solo_net <= min_net {
                continue;
            }
            let priority_enemies = indirect_priority_enemies(&threat_ctx, &gun_spec);

            for &fac in &facs {
                scored_for_type.push(PoolCandidate {
                    kind: CandidateKind::Indirect,
                    unit_type: ut,
                    cost: stats.cost,
                    facility: fac,
                    fac_idx: fac_idx_by_pos[&fac],
                    pos_key: fpos,
                    solo_net,
                    payload: PoolPayload::Indirect {
                        gun_spec: gun_spec.clone(),
                        block_pen,
                        asset_risk,
                        anchor: fpos,
                        priority_enemies: priority_enemies.clone(),
                    },
                });
            }
        }
        scored_for_type.sort_by(|a, b| {
            b.solo_net
                .total_cmp(&a.solo_net)
                .then_with(|| a.pos_key.y.cmp(&b.pos_key.y))
                .then_with(|| a.pos_key.x.cmp(&b.pos_key.x))
                .then_with(|| a.facility.y.cmp(&b.facility.y))
                .then_with(|| a.facility.x.cmp(&b.facility.x))
        });
        let mut kept_pos = HashSet::default();
        for cand in scored_for_type {
            if kept_pos.insert((cand.facility, cand.pos_key)) {
                indirect_pool.push(cand);
                if kept_pos.len() >= MAX_CANDS_PER_INDIRECT_TYPE {
                    break;
                }
            }
        }
    }

    indirect_pool.sort_by(|a, b| {
        b.solo_net
            .total_cmp(&a.solo_net)
            .then_with(|| a.cost.cmp(&b.cost))
            .then_with(|| a.pos_key.y.cmp(&b.pos_key.y))
            .then_with(|| a.pos_key.x.cmp(&b.pos_key.x))
            .then_with(|| a.facility.y.cmp(&b.facility.y))
            .then_with(|| a.facility.x.cmp(&b.facility.x))
            .then_with(|| a.unit_type.as_str().cmp(b.unit_type.as_str()))
    });
    indirect_pool.truncate(MAX_POOL_INDIRECT);

    let mut pool = Vec::with_capacity(capture_pool.len() + direct_pool.len() + indirect_pool.len());
    pool.extend(capture_pool);
    pool.extend(direct_pool);
    pool.extend(indirect_pool);

    // --- 4. 統合ビームサーチ ---
    let max_depth = scan.free_facilities.len().min(MAX_PURCHASES_PER_TURN);
    let mut eval_bufs = BeamEvalBuffers::with_capacities(max_depth, capture_props.len());
    let use_pool_mask = pool.len() <= 64;
    let use_fac_mask = fac_idx_by_pos.len() <= 64;

    let empty = BeamState {
        score: 0.0,
        delta_combat: 0.0,
        delta_cap: 0.0,
        delta_screen: 0.0,
        block_pen: 0.0,
        asset_risk: 0.0,
        spent: 0,
        capture_count: 0,
        transport_count: 0,
        chosen_mask: 0,
        fac_mask: 0,
        chosen: Vec::new(),
        used_facs: Vec::new(),
        used_indirect_pos: Vec::new(),
    };
    let mut best = empty.clone();
    let mut beam = vec![empty];

    for _depth in 0..max_depth {
        let mut next_beam: Vec<BeamState> = Vec::new();
        let mut seen_masks: HashSet<u64> = HashSet::default();
        let mut seen_keys: HashSet<Vec<usize>> = HashSet::default();

        for state in &beam {
            for (c_idx, cand) in pool.iter().enumerate() {
                if state.spent.saturating_add(cand.cost) > scan.funds {
                    continue;
                }
                let fac_used = if use_fac_mask {
                    (state.fac_mask & (1_u64 << cand.fac_idx)) != 0
                } else {
                    state.used_facs.contains(&cand.facility)
                };
                if fac_used {
                    continue;
                }
                if cand.kind == CandidateKind::Transport && state.transport_count >= 1 {
                    continue;
                }
                if matches!(cand.kind, CandidateKind::Capture | CandidateKind::Transport)
                    && state.capture_count >= capture_props.len()
                {
                    continue;
                }
                if cand.kind == CandidateKind::Indirect
                    && state.used_indirect_pos.contains(&cand.pos_key)
                {
                    continue;
                }

                let (next_mask, next_chosen) = if use_pool_mask {
                    let mask = state.chosen_mask | (1_u64 << c_idx);
                    if mask == state.chosen_mask || !seen_masks.insert(mask) {
                        continue;
                    }
                    let mut chosen = state.chosen.clone();
                    chosen.push(c_idx);
                    (mask, chosen)
                } else {
                    let mut chosen = state.chosen.clone();
                    chosen.push(c_idx);
                    chosen.sort_unstable();
                    if !seen_keys.insert(chosen.clone()) {
                        continue;
                    }
                    (0_u64, chosen)
                };

                // 占領・輸送候補の追加は戦闘出力を変えないため親状態の delta_combat を再利用する
                let reuse_combat =
                    matches!(cand.kind, CandidateKind::Capture | CandidateKind::Transport)
                        .then_some(state.delta_combat);
                let (score, dc, dcap, dscr, bp, ar, spent) = evaluate_combo(
                    &pool,
                    &next_chosen,
                    &threat_ctx,
                    &direct_need,
                    &base_indirects,
                    base_combat_val,
                    capture_props.len(),
                    existing_direct_cnt,
                    target_screen,
                    eff_lam,
                    reuse_combat,
                    &mut combat_scratch,
                    &mut eval_bufs,
                );
                let (next_fac_mask, used_facs) = if use_fac_mask {
                    (state.fac_mask | (1_u64 << cand.fac_idx), Vec::new())
                } else {
                    let mut uf = state.used_facs.clone();
                    uf.push(cand.facility);
                    (0_u64, uf)
                };
                let mut used_ind = state.used_indirect_pos.clone();
                if cand.kind == CandidateKind::Indirect {
                    used_ind.push(cand.pos_key);
                }
                let next_cap_cnt = state.capture_count
                    + usize::from(matches!(
                        cand.kind,
                        CandidateKind::Capture | CandidateKind::Transport
                    ));
                let next_transport_cnt =
                    state.transport_count + usize::from(cand.kind == CandidateKind::Transport);

                let next_state = BeamState {
                    score,
                    delta_combat: dc,
                    delta_cap: dcap,
                    delta_screen: dscr,
                    block_pen: bp,
                    asset_risk: ar,
                    spent,
                    capture_count: next_cap_cnt,
                    transport_count: next_transport_cnt,
                    chosen_mask: next_mask,
                    fac_mask: next_fac_mask,
                    chosen: next_chosen,
                    used_facs,
                    used_indirect_pos: used_ind,
                };
                if next_state.score > best.score {
                    best = next_state.clone();
                }
                next_beam.push(next_state);
            }
        }
        if next_beam.is_empty() {
            break;
        }
        beam = select_diverse_beam(&pool, next_beam);
    }

    best.chosen.sort_unstable();

    // --- 5. 下流契約（PlannedProduction / Trace）の組み立て ---
    // 占領候補と未所有物件の 1 対 1 割当を最終状態に対して確定する
    eval_bufs.cap_cands.clear();
    for &idx in &best.chosen {
        match &pool[idx].payload {
            PoolPayload::Capture { prop_deltas, .. } | PoolPayload::Transport { prop_deltas } => {
                eval_bufs.cap_cands.push((idx, prop_deltas.as_slice()));
            }
            PoolPayload::Direct { .. } | PoolPayload::Indirect { .. } => {}
        }
    }
    let capture_assignment: HashMap<usize, Option<usize>> = if eval_bufs.cap_cands.is_empty() {
        HashMap::default()
    } else {
        assign_capture_targets_with_map(
            &eval_bufs.cap_cands,
            capture_props.len(),
            &mut eval_bufs.cap_used_cand,
            &mut eval_bufs.cap_used_prop,
        )
    };

    let mut commands = Vec::with_capacity(best.chosen.len());
    let mut rem_funds = scan.funds;

    for &idx in &best.chosen {
        let cand = &pool[idx];
        let cmd = ProduceUnitCommand {
            player_id,
            target_x: cand.facility.x,
            target_y: cand.facility.y,
            unit_type: cand.unit_type,
        };
        let (role, purpose, deployment, capture_intent, op_kind, op_anchor) = match &cand.payload {
            PoolPayload::Capture { .. } => {
                let assigned_prop = capture_assignment.get(&idx).copied().flatten();
                if let Some(pidx) = assigned_prop {
                    let target = capture_props[pidx].pos;
                    let island_id = island_map
                        .get_island_at(&target)
                        .or_else(|| island_map.get_island_at(&cand.facility))
                        .map(|island| island.id)
                        .unwrap_or(crate::ai::islands::IslandId(0));
                    let intent = campaign_execution::CampaignProductionIntent {
                        command: cmd.clone(),
                        island_id,
                        role: campaign_execution::CampaignProductionRole::Capture,
                        mission_target: Some(target),
                    };
                    (
                        SlotKind::Capture,
                        PurchasePurpose::OperationSlot,
                        None,
                        Some(intent),
                        OperationKind::Capture,
                        target,
                    )
                } else {
                    // 全物件割当済みの追加歩兵は前線補強の Combat として展開する
                    let fallback_anchor = fallback_combat_anchor(scan, &threat_ctx, cand.facility);
                    let dep = PlannedDeployment {
                        anchor: fallback_anchor,
                        staging_anchor: fallback_anchor,
                        posture: deployment::DeploymentPosture::Execute,
                        slot_kind: SlotKind::Combat,
                        priority_enemies: Vec::new(),
                        threat_horizon: 4,
                        forecast: deployment::DeploymentForecast::default(),
                        plan_step: None,
                        operation_binding: None,
                    };
                    (
                        SlotKind::Combat,
                        PurchasePurpose::ImmediateReinforcement,
                        Some(dep),
                        None,
                        OperationKind::Capture,
                        fallback_anchor,
                    )
                }
            }
            PoolPayload::Transport { .. } => {
                let target = capture_assignment
                    .get(&idx)
                    .copied()
                    .flatten()
                    .map(|pidx| capture_props[pidx].pos)
                    .unwrap_or_else(|| capture_props.first().map_or(cand.facility, |p| p.pos));
                let island_id = island_map
                    .get_island_at(&target)
                    .or_else(|| island_map.get_island_at(&cand.facility))
                    .map(|island| island.id)
                    .unwrap_or(crate::ai::islands::IslandId(0));
                let intent = campaign_execution::CampaignProductionIntent {
                    command: cmd.clone(),
                    island_id,
                    role: campaign_execution::CampaignProductionRole::Transport,
                    mission_target: Some(target),
                };
                (
                    SlotKind::Transport,
                    PurchasePurpose::OperationSlot,
                    None,
                    Some(intent),
                    OperationKind::Capture,
                    target,
                )
            }
            PoolPayload::Direct {
                anchor,
                priority_enemies,
                ..
            }
            | PoolPayload::Indirect {
                anchor,
                priority_enemies,
                ..
            } => {
                let dep = PlannedDeployment {
                    anchor: *anchor,
                    staging_anchor: *anchor,
                    posture: deployment::DeploymentPosture::Execute,
                    slot_kind: SlotKind::Combat,
                    priority_enemies: priority_enemies.clone(),
                    threat_horizon: 4,
                    forecast: deployment::DeploymentForecast::default(),
                    plan_step: None,
                    operation_binding: None,
                };
                (
                    SlotKind::Combat,
                    PurchasePurpose::ImmediateReinforcement,
                    Some(dep),
                    None,
                    OperationKind::Capture,
                    *anchor,
                )
            }
        };

        plan_trace.steps.push(ProductionStepTrace {
            operation_kind: op_kind,
            operation_anchor: op_anchor,
            slot_kind: role,
            deficit_before: 1.0,
            deficit_after: 0.0,
            remaining_funds_before: rem_funds,
            decision: ProductionDecision::Produced {
                unit_type: cand.unit_type,
                cost: cand.cost,
                facility: cand.facility,
            },
        });
        rem_funds = rem_funds.saturating_sub(cand.cost);

        commands.push(PlannedProduction {
            command: cmd,
            role,
            cost: cand.cost,
            purpose,
            deployment,
            capture_intent,
        });
    }

    plan_trace.leftover_funds = rem_funds;
    plan_trace.uncommitted_funds = rem_funds;

    let elapsed_ms = start_instant.elapsed().as_secs_f64() * 1000.0;
    if std::env::var_os("OPENWARS_UNIFIED_PRODUCTION_TRACE").is_some() {
        let summary: Vec<String> = best
            .chosen
            .iter()
            .map(|&i| {
                let c = &pool[i];
                format!("{:?}@({},{})", c.unit_type, c.facility.x, c.facility.y)
            })
            .collect();
        let mut ranked_pool: Vec<&PoolCandidate> = pool.iter().collect();
        ranked_pool.sort_by(|a, b| b.solo_net.total_cmp(&a.solo_net));
        let top_pool: Vec<String> = ranked_pool
            .into_iter()
            .take(8)
            .map(|c| {
                format!(
                    "{:?}/{:?}@({},{}):{:.1}",
                    c.kind, c.unit_type, c.pos_key.x, c.pos_key.y, c.solo_net
                )
            })
            .collect();
        eprintln!(
            "UNIFIED_PROD P{} T{} funds={} spent={} elapsed_ms={:.3} score={:.1} (combat={:.1}, cap={:.1}, scr={:.1}, blk={:.1}, ar={:.1}) pool={} picked={:?} sample={:?}",
            player_id.0,
            turn,
            scan.funds,
            best.spent,
            elapsed_ms,
            best.score,
            best.delta_combat,
            best.delta_cap,
            best.delta_screen,
            best.block_pen,
            best.asset_risk,
            pool.len(),
            summary,
            top_pool,
        );
    }

    (commands, plan_trace)
}

/// 深さごとのビーム候補から、生スコア上位枠・費用対効果上位枠を組み合わせて兵種構成の多様性を保つ。
fn select_diverse_beam(pool: &[PoolCandidate], next_beam: Vec<BeamState>) -> Vec<BeamState> {
    if next_beam.len() <= BEAM_RAW_SLOTS {
        let mut sorted = next_beam;
        sorted.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.spent.cmp(&b.spent))
                .then_with(|| a.chosen.cmp(&b.chosen))
        });
        return sorted;
    }

    let n = next_beam.len();
    let comp_keys: Vec<Vec<UnitType>> = next_beam
        .iter()
        .map(|s| {
            let mut key: Vec<UnitType> = s.chosen.iter().map(|&idx| pool[idx].unit_type).collect();
            key.sort_unstable_by_key(|ut| ut.as_str());
            key
        })
        .collect();

    let mut raw_order: Vec<usize> = (0..n).collect();
    raw_order.sort_by(|&a, &b| {
        next_beam[b]
            .score
            .total_cmp(&next_beam[a].score)
            .then_with(|| next_beam[a].spent.cmp(&next_beam[b].spent))
            .then_with(|| next_beam[a].chosen.cmp(&next_beam[b].chosen))
    });

    let mut taken = vec![false; n];
    let mut comp_counts: HashMap<&[UnitType], usize> = HashMap::default();
    let mut selected_indices: Vec<usize> = Vec::with_capacity(BEAM_WIDTH);

    // 枠 1: 生スコア上位から同一兵種構成の上限を守りつつ確保
    let mut raw_picked = 0_usize;
    for &idx in &raw_order {
        if raw_picked >= BEAM_RAW_SLOTS {
            break;
        }
        let key = comp_keys[idx].as_slice();
        let cnt = comp_counts.get(key).copied().unwrap_or(0);
        if cnt >= BEAM_MAX_PER_COMPOSITION {
            continue;
        }
        taken[idx] = true;
        comp_counts.insert(key, cnt + 1);
        selected_indices.push(idx);
        raw_picked += 1;
    }

    // 枠 2: 正スコア候補を優先し、資金 1,000G あたりスコア（費用対効果）上位を追加確保
    let mut eff_order: Vec<usize> = (0..n).filter(|&i| !taken[i]).collect();
    let eff_score = |s: &BeamState| -> (bool, f32) {
        let is_pos = s.score > 0.0;
        let metric = if is_pos {
            s.score / (s.spent as f32 / 1000.0).max(1.0)
        } else {
            s.score
        };
        (is_pos, metric)
    };
    eff_order.sort_by(|&a, &b| {
        let (pa, ma) = eff_score(&next_beam[a]);
        let (pb, mb) = eff_score(&next_beam[b]);
        pb.cmp(&pa)
            .then_with(|| mb.total_cmp(&ma))
            .then_with(|| next_beam[b].score.total_cmp(&next_beam[a].score))
            .then_with(|| next_beam[a].spent.cmp(&next_beam[b].spent))
            .then_with(|| next_beam[a].chosen.cmp(&next_beam[b].chosen))
    });

    let mut eff_picked = 0_usize;
    for &idx in &eff_order {
        if eff_picked >= BEAM_EFF_SLOTS || selected_indices.len() >= BEAM_WIDTH {
            break;
        }
        let key = comp_keys[idx].as_slice();
        let cnt = comp_counts.get(key).copied().unwrap_or(0);
        if cnt >= BEAM_MAX_PER_COMPOSITION {
            continue;
        }
        taken[idx] = true;
        comp_counts.insert(key, cnt + 1);
        selected_indices.push(idx);
        eff_picked += 1;
    }

    // 枠 3: 構成上限により枠が余った場合は生スコア順の残りから補充
    if selected_indices.len() < BEAM_WIDTH {
        for &idx in &raw_order {
            if selected_indices.len() >= BEAM_WIDTH {
                break;
            }
            if !taken[idx] {
                taken[idx] = true;
                selected_indices.push(idx);
            }
        }
    }

    let mut opts: Vec<Option<BeamState>> = next_beam.into_iter().map(Some).collect();
    let mut selected: Vec<BeamState> = selected_indices
        .into_iter()
        .filter_map(|i| opts[i].take())
        .collect();
    selected.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.spent.cmp(&b.spent))
            .then_with(|| a.chosen.cmp(&b.chosen))
    });
    selected
}

#[allow(clippy::too_many_arguments)]
fn evaluate_combo<'a>(
    pool: &'a [PoolCandidate],
    chosen: &[usize],
    threat_ctx: &ThreatContext,
    direct_need: &[f32],
    base_indirects: &[IndirectGunSpec],
    base_combat_val: f32,
    num_props: usize,
    existing_direct_cnt: usize,
    target_screen: usize,
    lam_per_1k: f32,
    reuse_delta_combat: Option<f32>,
    combat_scratch: &mut CombatScratch,
    bufs: &mut BeamEvalBuffers<'a>,
) -> (f32, f32, f32, f32, f32, f32, u32) {
    bufs.extra_inds.clear();
    bufs.dirs.clear();
    bufs.cap_cands.clear();

    let has_enemy = !threat_ctx.enemy_targets.is_empty();
    let mut delta_screen = 0.0_f32;
    let mut land_added = 0_usize;
    let mut block_pen = 0.0_f32;
    let mut asset_risk = 0.0_f32;
    let mut spent = 0_u32;

    for &idx in chosen {
        let cand = &pool[idx];
        spent = spent.saturating_add(cand.cost);
        match &cand.payload {
            PoolPayload::Capture {
                prop_deltas,
                screen_value,
            } => {
                bufs.cap_cands.push((idx, prop_deltas.as_slice()));
                let factor = screen_decay_factor(
                    existing_direct_cnt.saturating_add(land_added),
                    target_screen,
                    has_enemy,
                );
                delta_screen += screen_value * factor;
                land_added += 1;
            }
            PoolPayload::Transport { prop_deltas } => {
                bufs.cap_cands.push((idx, prop_deltas.as_slice()));
            }
            PoolPayload::Direct {
                unit_spec,
                screen_value,
                ..
            } => {
                if reuse_delta_combat.is_none() {
                    bufs.dirs.push(unit_spec);
                }
                if *screen_value > 0.0 {
                    let factor = screen_decay_factor(
                        existing_direct_cnt.saturating_add(land_added),
                        target_screen,
                        has_enemy,
                    );
                    delta_screen += screen_value * factor;
                    land_added += 1;
                }
            }
            PoolPayload::Indirect {
                gun_spec,
                block_pen: bp,
                asset_risk: ar,
                ..
            } => {
                if reuse_delta_combat.is_none() {
                    bufs.extra_inds.push(gun_spec);
                }
                block_pen += bp;
                asset_risk += ar;
            }
        }
    }

    let delta_cap = if bufs.cap_cands.is_empty() {
        0.0
    } else {
        assign_capture_targets_fast(
            &bufs.cap_cands,
            num_props,
            &mut bufs.cap_used_cand,
            &mut bufs.cap_used_prop,
        )
    };

    let delta_combat = if let Some(cached) = reuse_delta_combat {
        cached
    } else if bufs.extra_inds.is_empty() && bufs.dirs.is_empty() {
        0.0
    } else {
        simulate_joint_combat(
            &threat_ctx.enemy_targets,
            direct_need,
            base_indirects,
            &bufs.extra_inds,
            &bufs.dirs,
            combat_scratch,
        ) - base_combat_val
    };
    let score = delta_combat + delta_cap + delta_screen
        - block_pen
        - asset_risk
        - budget_cost(spent, lam_per_1k);
    (
        score,
        delta_combat,
        delta_cap,
        delta_screen,
        block_pen,
        asset_risk,
        spent,
    )
}

/// 選択された占領候補群と未所有物件を最大 `delta_cap` 貪欲法で 1 対 1 割当し、合計改善値を返す。
fn assign_capture_targets_fast(
    cap_cands: &[(usize, &[(usize, f32)])],
    num_props: usize,
    used_cand: &mut Vec<bool>,
    used_prop: &mut Vec<bool>,
) -> f32 {
    let k = cap_cands.len();
    used_cand.resize(k, false);
    used_cand.fill(false);
    used_prop.resize(num_props, false);
    used_prop.fill(false);

    let mut total_delta = 0.0_f32;
    for _ in 0..k {
        let Some((ci, pidx, best_gain)) =
            pick_next_best_capture_pair(cap_cands, used_cand, used_prop)
        else {
            break;
        };
        used_cand[ci] = true;
        used_prop[pidx] = true;
        total_delta += best_gain;
    }
    total_delta
}

/// 最終採用状態向けに、各占領候補 `pool_idx` へ割り当てられた `capture_props` インデックスを返す。
fn assign_capture_targets_with_map(
    cap_cands: &[(usize, &[(usize, f32)])],
    num_props: usize,
    used_cand: &mut Vec<bool>,
    used_prop: &mut Vec<bool>,
) -> HashMap<usize, Option<usize>> {
    let k = cap_cands.len();
    used_cand.resize(k, false);
    used_cand.fill(false);
    used_prop.resize(num_props, false);
    used_prop.fill(false);

    let mut mapping: HashMap<usize, Option<usize>> = cap_cands
        .iter()
        .map(|&(pool_idx, _)| (pool_idx, None))
        .collect();
    for _ in 0..k {
        let Some((ci, pidx, _)) = pick_next_best_capture_pair(cap_cands, used_cand, used_prop)
        else {
            break;
        };
        used_cand[ci] = true;
        used_prop[pidx] = true;
        mapping.insert(cap_cands[ci].0, Some(pidx));
    }
    mapping
}

/// 未割当の (占領候補, 物件) のうち最大 `delta_cap` のペアを返す（`prop_deltas` は降順前提）。
fn pick_next_best_capture_pair(
    cap_cands: &[(usize, &[(usize, f32)])],
    used_cand: &[bool],
    used_prop: &[bool],
) -> Option<(usize, usize, f32)> {
    let mut best_gain = 0.0_f32;
    let mut best: Option<(usize, usize, f32)> = None;
    for (ci, (_, pdeltas)) in cap_cands.iter().enumerate() {
        if used_cand[ci] {
            continue;
        }
        for &(pidx, dcap) in *pdeltas {
            if pidx < used_prop.len() && !used_prop[pidx] {
                if dcap > best_gain {
                    best_gain = dcap;
                    best = Some((ci, pidx, dcap));
                }
                break;
            }
        }
    }
    best
}

fn fallback_combat_anchor(
    scan: &BoardScan,
    threat_ctx: &ThreatContext,
    origin: GridPosition,
) -> GridPosition {
    scan.open_properties
        .iter()
        .min_by_key(|p| scan.map.distance(origin.x, origin.y, p.x, p.y))
        .copied()
        .or_else(|| {
            threat_ctx
                .enemy_targets
                .iter()
                .filter(|e| !e.virtual_wave)
                .min_by_key(|e| {
                    scan.map
                        .distance(origin.x, origin.y, e.pos_proj.x, e.pos_proj.y)
                })
                .map(|e| e.pos_proj)
        })
        .unwrap_or(origin)
}

/// 既存味方による敵ごとのカバー充足度から、新規直接購入の需要倍率 `(0, 1]` を算出する。
fn compute_direct_need(
    scan: &BoardScan,
    extras: &UnifiedBoardExtras,
    profiles: &WeaponProfileTable,
    threat_ctx: &ThreatContext,
    dist_between: &mut impl FnMut(GridPosition, GridPosition, MovementType) -> Option<u32>,
) -> Vec<f32> {
    let n = threat_ctx.enemy_targets.len();
    let mut cov = vec![0.0_f32; n];
    if n == 0 {
        return Vec::new();
    }

    let map = &scan.map;
    let chart = &scan.damage_chart;
    let mut per_unit_weights: Vec<(usize, f32, f32)> = Vec::with_capacity(n);

    for u in &scan.my_units {
        let ut = u.stats.unit_type;
        if u.hp == 0 || !profiles.has_any_weapon(ut) {
            continue;
        }
        let is_ind = profiles.is_indirect(ut);
        let ammo = u
            .entity
            .and_then(|e| extras.unit_ammo.get(&e).copied())
            .unwrap_or((u.stats.max_ammo1, u.stats.max_ammo2));
        let mp = u.stats.max_movement.max(1);
        let test_dist = u.stats.min_range.max(1);
        per_unit_weights.clear();
        let mut sum_w = 0.0_f32;

        for (ei, et) in threat_ctx.enemy_targets.iter().enumerate() {
            let is_air = et.stats.movement_type == MovementType::Air;
            // 間接味方は地上敵の直接充足には数えず、航空敵（ミサイル等の対空カバー）のみカウントする
            if is_ind && !is_air {
                continue;
            }
            let dmg = expected_damage(
                chart,
                profiles,
                ut,
                100,
                ammo,
                et.stats.unit_type,
                et.def_proj,
                test_dist,
                false,
            );
            if is_air {
                if dmg < DIRECT_EXISTING_COV_MIN_DMG {
                    continue;
                }
                let prox = if is_ind {
                    let d = map.distance(u.pos.x, u.pos.y, et.pos_proj.x, et.pos_proj.y);
                    if d <= u.stats.max_range {
                        1.0
                    } else if d <= u.stats.max_range.saturating_add(mp) {
                        0.75
                    } else if dmg >= AIR_EXISTING_COV_MIN_DMG {
                        AIR_EXISTING_COV_GLOBAL_PROX
                    } else {
                        0.0
                    }
                } else {
                    let min_cost = map
                        .get_adjacent(et.pos_proj.x, et.pos_proj.y)
                        .into_iter()
                        .filter_map(|(ax, ay)| {
                            dist_between(
                                u.pos,
                                GridPosition { x: ax, y: ay },
                                u.stats.movement_type,
                            )
                        })
                        .min();
                    if let Some(cost_mp) = min_cost {
                        let move_turns = cost_mp.div_ceil(mp);
                        if move_turns <= 3 {
                            1.0
                        } else if move_turns <= 6 {
                            0.75
                        } else {
                            0.50
                        }
                    } else if dmg >= AIR_EXISTING_COV_MIN_DMG {
                        AIR_EXISTING_COV_GLOBAL_PROX
                    } else {
                        0.0
                    }
                };
                if prox <= 0.0 {
                    continue;
                }
                let w = (dmg as f32 / 100.0) * prox;
                sum_w += w;
                per_unit_weights.push((ei, w, 1.0));
            } else {
                if dmg < GROUND_EXISTING_COV_MIN_DMG {
                    continue;
                }
                let min_cost = map
                    .get_adjacent(et.pos_proj.x, et.pos_proj.y)
                    .into_iter()
                    .filter_map(|(ax, ay)| {
                        dist_between(u.pos, GridPosition { x: ax, y: ay }, u.stats.movement_type)
                    })
                    .min();
                let Some(cost_mp) = min_cost else {
                    continue;
                };
                let move_turns = cost_mp.div_ceil(mp);
                let prox = if move_turns <= 2 {
                    1.0
                } else if move_turns <= 4 {
                    0.5
                } else {
                    0.0
                };
                if prox <= 0.0 {
                    continue;
                }
                let w = (dmg as f32 / 100.0) * prox;
                // 占領歩兵同士の歩兵カバーだけでは決定的妨害にならず、高機動力対歩兵ユニットの需要を維持する
                let eff_mult = if et.stats.can_capture && dmg < ANTI_CAPTURER_DECISIVE_DMG {
                    ANTI_CAPTURER_SOFT_COV_MULT
                } else {
                    1.0
                };
                sum_w += w;
                per_unit_weights.push((ei, w, eff_mult));
            }
        }

        if per_unit_weights.is_empty() {
            continue;
        }
        let denom = sum_w.max(1.0);
        let hp_frac = u.hp as f32 / 100.0;
        for &(ei, w, eff_mult) in &per_unit_weights {
            cov[ei] += hp_frac * (w / denom) * eff_mult;
        }
    }

    cov.into_iter()
        .zip(threat_ctx.enemy_targets.iter())
        .map(|(c, et)| {
            if et.stats.movement_type == MovementType::Air {
                0.5_f32.powf(DIRECT_EXISTING_COV_EXP * c)
            } else {
                1.0 / (1.0 + GROUND_EXISTING_COV_DECAY * c)
            }
        })
        .collect()
}

/// 当該ユニット種別が対地目標（軽歩兵または軽戦車）へ基礎ダメージを持つか。
#[inline]
fn can_hit_ground(chart: &crate::resources::DamageChart, ut: UnitType) -> bool {
    max_base_damage(chart, ut, UnitType::Infantry) > 0
        || max_base_damage(chart, ut, UnitType::Tank) > 0
}

/// 空海輸送ユニットまたは対地攻撃力を持たない後方支援輸送車（直接戦闘候補から除外対象）か。
#[inline]
fn is_non_combat_transport(chart: &crate::resources::DamageChart, stats: &UnitStats) -> bool {
    stats.max_cargo > 0
        && (matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
            || !can_hit_ground(chart, stats.unit_type))
}

/// 運転資金を超過する余剰資金に応じて資金シャドウプライスを滑らかに引き下げる。
#[inline]
fn effective_lam_per_1k(funds: u32, free_fac_count: usize) -> f32 {
    let ref_budget = (free_fac_count as f32 * REF_WORKING_CAPITAL_PER_FAC).max(MIN_WORKING_CAPITAL);
    let excess = (funds as f32 - ref_budget).max(0.0);
    (LAM0_PER_1K / (1.0 + excess / SURPLUS_LAM_HALF_LIFE_FUNDS)).max(MIN_SURPLUS_LAM_PER_1K)
}

#[inline]
fn budget_cost(spent: u32, lam_per_1k: f32) -> f32 {
    lam_per_1k * (spent as f32 / 1000.0)
}

#[inline]
fn cripple_factor(dmg: u32) -> f32 {
    0.25 + 0.75 * (dmg as f32 / CRIPPLE_REF_DAMAGE).min(1.0)
}

fn screen_decay_factor(current_idx: usize, target_screen: usize, has_enemy: bool) -> f32 {
    if target_screen == 0 {
        return 0.0;
    }
    let floor = if has_enemy {
        SCREEN_SATURATED_FLOOR_FACTOR
    } else {
        0.0
    };
    if current_idx >= target_screen {
        return floor;
    }
    (((target_screen - current_idx) as f32 / target_screen as f32)
        .max(0.0)
        .sqrt())
    .max(floor)
}

/// 既存直接数と比較した地上前線維持（スクリーン）の単体価値。
fn screen_marginal_value(
    scan: &BoardScan,
    e_direct_cnt: usize,
    stats: &UnitStats,
    existing_direct_cnt: usize,
    target_screen: usize,
    has_enemy: bool,
) -> f32 {
    if !has_enemy && existing_direct_cnt >= target_screen {
        return 0.0;
    }
    let durability = if scan.enemy_units.is_empty() {
        0.85
    } else {
        let mut total_dmg = 0_u32;
        let mut cnt = 0_u32;
        for enemy in &scan.enemy_units {
            let bd = max_base_damage(&scan.damage_chart, enemy.stats.unit_type, stats.unit_type);
            total_dmg = total_dmg.saturating_add(bd);
            cnt = cnt.saturating_add(1);
        }
        let avg_dmg = total_dmg as f32 / cnt.max(1) as f32;
        (1.0 - avg_dmg / 140.0).clamp(0.45, 1.15)
    };
    let mob_bonus = (stats.max_movement as f32 / SCREEN_MOBILITY_REF)
        .clamp(SCREEN_MOBILITY_MIN_MULT, SCREEN_MOBILITY_MAX_MULT);
    let mut base = BODY_SCREEN_BASE + SCREEN_DURABILITY_BONUS * durability * mob_bonus;
    if stats.max_movement <= SCREEN_SLOW_MAX_MOVEMENT {
        base *= SCREEN_SLOW_UNIT_MULT;
    }
    let outnumbered_mult = if existing_direct_cnt < e_direct_cnt {
        SCREEN_OUTNUMBERED_MULT
    } else {
        1.0
    };
    base * outnumbered_mult
}

/// 既存占領役を未所有物件へ最短距離順に1対1割当し、各物件のベースライン到着ターンを決める。
fn build_capture_baseline(
    scan: &BoardScan,
    extras: &UnifiedBoardExtras,
    profiles: &WeaponProfileTable,
    existing_capturers: &[UnitSnapshot],
    dist_between: &mut impl FnMut(GridPosition, GridPosition, MovementType) -> Option<u32>,
) -> Vec<CapturePropertyInfo> {
    let map = &scan.map;
    let master = &scan.master_data;
    let chart = &scan.damage_chart;

    let capturer_set: HashSet<GridPosition> = existing_capturers.iter().map(|u| u.pos).collect();
    let enemy_prop_set: HashSet<GridPosition> =
        extras.enemy_properties.iter().map(|(p, _)| *p).collect();

    let ref_cap_mp = scan
        .reference_capture_unit()
        .map_or(3, |s| s.max_movement.max(1));
    let mut props: Vec<CapturePropertyInfo> = Vec::new();
    for &p in &scan.open_properties {
        if capturer_set.contains(&p) {
            continue;
        }
        let terrain = map.get_terrain(p.x, p.y).unwrap_or(Terrain::City);
        let landscape = master.get_landscape_by_name(terrain.as_str());
        let income = landscape.and_then(|r| r.income).unwrap_or(1000) as f32;
        let durability = landscape.map_or(DEFAULT_PROPERTY_DURABILITY, |r| r.durability);
        let cap_turns = durability.div_ceil(CAPTURE_PROGRESS_PER_TURN).max(1);
        let income_mult = (income / 1000.0).min(MAX_CAPTURE_INCOME_MULT);

        let is_prod_mouth = scan.capital_position.is_some_and(|cap| {
            map.distance(p.x, p.y, cap.x, cap.y) <= 3
                && landscape.and_then(|r| r.supply_type.as_deref()) == Some("地上部隊")
        });
        let prod_mouth_mult = if is_prod_mouth {
            PROD_MOUTH_VALUE_MULT
        } else {
            1.0
        };
        let is_enemy_prop = enemy_prop_set.contains(&p);
        let owner_mult = if terrain == Terrain::Capital && is_enemy_prop {
            ENEMY_CAPITAL_VALUE_MULT
        } else if is_enemy_prop {
            ENEMY_PROP_VALUE_MULT
        } else {
            1.0
        };

        // 1T・2T の敵脅威と敵本陣工場への近接状況から現実的な占領安全性を評価
        let max_1t_threat = scan
            .enemy_units
            .iter()
            .filter(|e| !profiles.is_indirect(e.stats.unit_type))
            .filter(|e| map.distance(e.pos.x, e.pos.y, p.x, p.y) <= e.stats.max_movement + 1)
            .map(|e| max_base_damage(chart, e.stats.unit_type, UnitType::Infantry))
            .max()
            .unwrap_or(0);
        let mut cap_safety = if max_1t_threat >= CAP_THREAT_1T_LETHAL_DMG {
            CAP_SAFETY_1T_LETHAL
        } else if max_1t_threat >= CAP_THREAT_1T_HIGH_DMG {
            CAP_SAFETY_1T_HIGH
        } else {
            1.0
        };
        let has_2t_heavy = scan
            .enemy_units
            .iter()
            .filter(|e| !profiles.is_indirect(e.stats.unit_type))
            .filter(|e| {
                map.distance(e.pos.x, e.pos.y, p.x, p.y)
                    <= e.stats.max_movement.saturating_mul(2).saturating_add(1)
            })
            .any(|e| {
                max_base_damage(chart, e.stats.unit_type, UnitType::Infantry)
                    >= CAP_THREAT_2T_HEAVY_DMG
            });
        if has_2t_heavy && max_1t_threat < CAP_THREAT_1T_HIGH_DMG {
            cap_safety *= CAP_SAFETY_2T_HEAVY_MULT;
        }
        if is_enemy_prop && scan.enemy_units.len() >= CAP_ENEMY_GUARD_MIN_UNITS {
            let min_enemy_fac_dist = scan
                .enemy_facilities
                .iter()
                .map(|ef| map.distance(ef.pos.x, ef.pos.y, p.x, p.y))
                .min()
                .unwrap_or(u32::MAX);
            let min_my_fac_dist = scan
                .production_facilities
                .iter()
                .map(|(fp, _)| map.distance(fp.x, fp.y, p.x, p.y))
                .min()
                .unwrap_or(u32::MAX);
            if min_enemy_fac_dist <= CAP_ENEMY_FAC_GUARD_RADIUS
                && min_enemy_fac_dist < min_my_fac_dist
            {
                cap_safety *= CAP_SAFETY_GUARDED_ENEMY_PROP_MULT;
            }
        }
        // 中立物件で敵占領ユニットの方が自軍より先に到達できる場合は先取失敗リスクを織り込む
        if !is_enemy_prop {
            let min_enemy_cap_eta = scan
                .enemy_units
                .iter()
                .filter(|e| e.stats.can_capture)
                .map(|e| {
                    map.distance(e.pos.x, e.pos.y, p.x, p.y)
                        .div_ceil(e.stats.max_movement.max(1))
                })
                .min()
                .unwrap_or(u32::MAX);
            let existing_cap_eta = existing_capturers
                .iter()
                .filter_map(|u| {
                    dist_between(u.pos, p, u.stats.movement_type)
                        .map(|c| c.div_ceil(u.stats.max_movement.max(1)))
                })
                .min()
                .unwrap_or(u32::MAX);
            let fac_cap_eta = scan
                .production_facilities
                .iter()
                .filter(|(_, f_terrain)| {
                    scan.available_types
                        .iter()
                        .any(|(ut, st)| st.can_capture && scan.can_produce(*f_terrain, *ut))
                })
                .map(|(fp, _)| {
                    map.distance(fp.x, fp.y, p.x, p.y)
                        .div_ceil(ref_cap_mp)
                        .saturating_add(1)
                })
                .min()
                .unwrap_or(u32::MAX);
            let my_min_eta = existing_cap_eta.min(fac_cap_eta);
            if min_enemy_cap_eta < my_min_eta {
                cap_safety *= CAP_SAFETY_ENEMY_CONTESTED_NEUTRAL_MULT;
            }
        }

        let economic_value =
            income_mult * prod_mouth_mult * owner_mult * CAPTURE_BASE_VALUE * cap_safety;
        props.push(CapturePropertyInfo {
            pos: p,
            economic_value,
            cap_turns,
            base_arrival: u32::MAX,
        });
    }

    // 経済価値降順に上位 MAX_CAPTURE_PROPS 件へ絞る
    props.sort_by(|a, b| {
        b.economic_value
            .total_cmp(&a.economic_value)
            .then_with(|| a.pos.y.cmp(&b.pos.y))
            .then_with(|| a.pos.x.cmp(&b.pos.x))
    });
    props.truncate(MAX_CAPTURE_PROPS);

    // 既存占領役の (到着ターン, 物件index, ユニットindex) を貪欲に1対1マッチ
    let mut pairs: Vec<(u32, usize, usize)> = Vec::new();
    for (ui, cap_u) in existing_capturers.iter().enumerate() {
        for (pi, prop) in props.iter().enumerate() {
            if let Some(cost_mp) = dist_between(cap_u.pos, prop.pos, cap_u.stats.movement_type) {
                let eta = cost_mp.div_ceil(cap_u.stats.max_movement.max(1)).max(1);
                pairs.push((eta.saturating_add(prop.cap_turns), pi, ui));
            }
        }
    }
    pairs.sort_unstable();
    let mut used_u = vec![false; existing_capturers.len()];
    for (base_arr, pi, ui) in pairs {
        if used_u[ui] || props[pi].base_arrival != u32::MAX {
            continue;
        }
        used_u[ui] = true;
        props[pi].base_arrival = base_arr;
    }

    props
}

/// 既存または新規間接ユニット 1 門分のホライズン内射撃仕様を生成する。
#[allow(clippy::too_many_arguments)]
fn build_indirect_gun_spec(
    scan: &BoardScan,
    profiles: &WeaponProfileTable,
    threat_ctx: &ThreatContext,
    stats: &UnitStats,
    pos: GridPosition,
    delay: usize,
    hp: u32,
    initial_ammo: u32,
    surv: f32,
) -> IndirectGunSpec {
    let map = &scan.map;
    let chart = &scan.damage_chart;
    let ut = stats.unit_type;
    let (rmin, rmax) = (stats.min_range, stats.max_range);
    let mut scored_d0: Vec<(f32, usize, u32, f32)> = Vec::new();
    let mut scored_dn: Vec<(f32, usize, u32, f32)> = Vec::new();

    if hp > 0 {
        for (ei, et) in threat_ctx.enemy_targets.iter().enumerate() {
            let dp = map.distance(pos.x, pos.y, et.pos_proj.x, et.pos_proj.y);
            let dmg_p = if dp >= rmin && dp <= rmax {
                expected_damage(
                    chart,
                    profiles,
                    ut,
                    hp,
                    (1, 0),
                    et.stats.unit_type,
                    et.def_proj,
                    dp,
                    false,
                )
            } else {
                0
            };
            if dmg_p > 0 {
                let cf = cripple_factor(dmg_p);
                let eff = dmg_p.min(et.hp);
                let eff_cf = if eff >= et.hp { 1.0 } else { cf };
                let score = eff as f32 * et.w_per_hp * eff_cf;
                scored_dn.push((score, ei, dmg_p, cf));
            }
            if !et.virtual_wave {
                let d_now = map.distance(pos.x, pos.y, et.pos0.x, et.pos0.y);
                let dmg_n = if d_now >= rmin && d_now <= rmax {
                    expected_damage(
                        chart,
                        profiles,
                        ut,
                        hp,
                        (1, 0),
                        et.stats.unit_type,
                        et.def0,
                        d_now,
                        false,
                    )
                } else {
                    0
                };
                let dm0 = dmg_p.max(dmg_n);
                if dm0 > 0 {
                    let cf = cripple_factor(dm0);
                    let eff = dm0.min(et.hp);
                    let eff_cf = if eff >= et.hp { 1.0 } else { cf };
                    let score = eff as f32 * et.w_per_hp * eff_cf;
                    scored_d0.push((score, ei, dm0, cf));
                }
            }
        }
    }

    let sort_trim = |mut v: Vec<(f32, usize, u32, f32)>| -> Vec<(usize, u32, f32)> {
        v.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        v.truncate(MAX_TARGETS_PER_INDIRECT);
        v.into_iter()
            .map(|(_, ei, dmg, cf)| (ei, dmg, cf))
            .collect()
    };

    IndirectGunSpec {
        delay,
        max_ammo: stats.max_ammo1,
        initial_ammo,
        supplied: threat_ctx.is_ground_supply(pos),
        surv,
        d0: sort_trim(scored_d0),
        dn: sort_trim(scored_dn),
    }
}

/// 新規直接ユニット 1 体分の交戦リストを生成する。
#[allow(clippy::too_many_arguments)]
fn build_direct_spec(
    scan: &BoardScan,
    profiles: &WeaponProfileTable,
    threat_ctx: &ThreatContext,
    direct_need: &[f32],
    stats: &UnitStats,
    origin: GridPosition,
    hp: u32,
    ammo: (u32, u32),
    dist_between: &mut impl FnMut(GridPosition, GridPosition, MovementType) -> Option<u32>,
) -> DirectUnitSpec {
    let map = &scan.map;
    let master = &scan.master_data;
    let chart = &scan.damage_chart;
    let ut = stats.unit_type;
    let mp = stats.max_movement.max(1);
    let is_air = stats.movement_type == MovementType::Air;
    let mut scored_attacks: Vec<(f32, usize, CombatAttackEntry)> = Vec::new();

    if hp == 0 {
        return DirectUnitSpec {
            hp,
            cost: stats.cost,
            attacks: Vec::new(),
        };
    }

    for (ei, et) in threat_ctx.enemy_targets.iter().enumerate() {
        let d_deal = expected_damage(
            chart,
            profiles,
            ut,
            hp,
            ammo,
            et.stats.unit_type,
            et.def_proj,
            1,
            false,
        );
        if d_deal == 0 {
            continue;
        }
        // 敵投影位置に隣接するマスのうち、最短移動コスト（同率時は地形防御最大）を攻撃位置に選ぶ
        let target_pos = et.pos_proj;
        let best_adj_choice = map
            .get_adjacent(target_pos.x, target_pos.y)
            .into_iter()
            .filter_map(|(ax, ay)| {
                let adj = GridPosition { x: ax, y: ay };
                dist_between(origin, adj, stats.movement_type).map(|cost_mp| {
                    let tdef = terrain_def_at(map, master, adj);
                    (cost_mp, tdef, adj)
                })
            })
            .min_by(|a, b| {
                (a.0, std::cmp::Reverse(a.1), a.2.y, a.2.x).cmp(&(
                    b.0,
                    std::cmp::Reverse(b.1),
                    b.2.y,
                    b.2.x,
                ))
            });
        let Some((cost_mp, atk_def, best_adj)) = best_adj_choice else {
            continue;
        };

        let move_turns = cost_mp.div_ceil(mp);
        // 地上直接ユニットは手前の敵前線スクリーンを超える分だけ到着ラウンドが遅延する
        let screen_delay = if is_air {
            0
        } else {
            let d_orig_et = map.distance(origin.x, origin.y, target_pos.x, target_pos.y);
            let screen_cnt = threat_ctx
                .enemy_targets
                .iter()
                .filter(|e| {
                    !e.virtual_wave
                        && !profiles.is_indirect(e.stats.unit_type)
                        && e.stats.movement_type != MovementType::Air
                })
                .filter(|e| {
                    let d_orig_e = map.distance(origin.x, origin.y, e.pos_proj.x, e.pos_proj.y);
                    d_orig_e < d_orig_et
                        && d_orig_e.saturating_add(map.distance(
                            e.pos_proj.x,
                            e.pos_proj.y,
                            target_pos.x,
                            target_pos.y,
                        )) <= d_orig_et.saturating_add(2)
                })
                .count();
            if screen_cnt >= 3 {
                2
            } else if screen_cnt >= 1 {
                1
            } else {
                0
            }
        };
        let eff_round = (move_turns.saturating_sub(1) as usize)
            .saturating_add(screen_delay)
            .max(et.arr_rnd);
        if eff_round > MAX_DIRECT_ARRIVAL_ROUND {
            continue;
        }
        let sim_round = eff_round.min(HORIZON - 1);
        let late_disc = if eff_round < HORIZON {
            1.0
        } else {
            ROUND_DISCOUNT.powi((eff_round - (HORIZON - 1)) as i32)
        };

        let dmg_cnt_100 = if profiles.is_indirect(et.stats.unit_type) {
            0.0
        } else {
            expected_damage(
                chart,
                profiles,
                et.stats.unit_type,
                100,
                et.ammo,
                ut,
                atk_def,
                1,
                true,
            ) as f32
        };

        // 攻撃マスに留まった次の敵ターンに周辺敵から受ける上位2撃の期待被弾リスク
        let mut top1 = 0.0_f32;
        let mut top2 = 0.0_f32;
        let target_killed = d_deal >= et.hp;
        for (ej, e2) in threat_ctx.enemy_targets.iter().enumerate() {
            if e2.virtual_wave || (ej == ei && target_killed) {
                continue;
            }
            let dist_e2 = map.distance(e2.pos_proj.x, e2.pos_proj.y, best_adj.x, best_adj.y);
            let e2_ut = e2.stats.unit_type;
            let dmg_e2 = if profiles.is_indirect(e2_ut) {
                if dist_e2 >= e2.stats.min_range && dist_e2 <= e2.stats.max_range {
                    expected_damage(
                        chart, profiles, e2_ut, e2.hp, e2.ammo, ut, atk_def, dist_e2, false,
                    ) as f32
                } else {
                    0.0
                }
            } else {
                let e2_max_move = if e2.stats.movement_type == MovementType::Air {
                    e2.stats.max_movement
                } else {
                    e2.stats.max_movement.min(ENEMY_RETAL_MAX_MOVE)
                };
                if dist_e2 <= e2_max_move.saturating_add(1) {
                    expected_damage(
                        chart, profiles, e2_ut, e2.hp, e2.ammo, ut, atk_def, 1, false,
                    ) as f32
                } else {
                    0.0
                }
            };
            if dmg_e2 > top1 {
                top2 = top1;
                top1 = dmg_e2;
            } else if dmg_e2 > top2 {
                top2 = dmg_e2;
            }
        }
        let retal_factor = if is_air {
            AIR_RETAL_FACTOR
        } else {
            DIRECT_RETAL_FACTOR
        };
        let retal_dmg = (retal_factor * (top1 + DIRECT_SECOND_RETAL_WEIGHT * top2)).min(100.0);

        // 100HP 基準の単発純利得が非正（赤字特攻）のターゲットは事前に枝刈りする
        let cf = cripple_factor(d_deal);
        let eff = d_deal.min(et.hp);
        let post_rhp = et.hp.saturating_sub(eff);
        let actual_cnt = if post_rhp == 0 {
            0.0
        } else {
            dmg_cnt_100 * (post_rhp as f32 / 100.0)
        };
        let step_hp_loss = (actual_cnt + retal_dmg).min(hp as f32);
        let attrition_cost =
            (step_hp_loss / 100.0) * DIRECT_ATTRITION_PER_1K * (stats.cost as f32 / 1000.0);
        let eff_cf = if eff >= et.hp { 1.0 } else { cf };
        let raw_gain = eff as f32 * et.w_per_hp * eff_cf * direct_need[ei];
        let net_gain = (raw_gain - attrition_cost) * late_disc;
        if net_gain <= 0.0 {
            continue;
        }

        scored_attacks.push((
            net_gain,
            sim_round,
            CombatAttackEntry {
                enemy_idx: ei,
                dmg: d_deal,
                cripple_factor: cf,
                dmg_cnt_100,
                retal_dmg,
                late_disc,
            },
        ));
    }

    scored_attacks.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.enemy_idx.cmp(&b.2.enemy_idx))
    });
    scored_attacks.truncate(MAX_ATTACKS_PER_DIRECT);
    let attacks = scored_attacks
        .into_iter()
        .map(|(_, rnd, entry)| (rnd, entry))
        .collect();

    DirectUnitSpec {
        hp,
        cost: stats.cost,
        attacks,
    }
}

/// 共有 `rem_hp` に対して既存＋新規の間接と新規の直接が共同で攻撃を行うシミュレーション。
fn simulate_joint_combat(
    enemies: &[super::threats::ProjectedEnemy],
    direct_need: &[f32],
    base_indirects: &[IndirectGunSpec],
    extra_indirects: &[&IndirectGunSpec],
    directs: &[&DirectUnitSpec],
    scratch: &mut CombatScratch,
) -> f32 {
    let total_ind = base_indirects.len() + extra_indirects.len();
    if enemies.is_empty() || (total_ind == 0 && directs.is_empty()) {
        return 0.0;
    }
    let ind_at = |gi: usize| -> &IndirectGunSpec {
        if gi < base_indirects.len() {
            &base_indirects[gi]
        } else {
            extra_indirects[gi - base_indirects.len()]
        }
    };

    scratch.rem_hp.clear();
    scratch.rem_hp.extend(enemies.iter().map(|e| e.hp));

    scratch.ammos.clear();
    scratch
        .ammos
        .extend(base_indirects.iter().map(|g| g.initial_ammo));
    scratch
        .ammos
        .extend(extra_indirects.iter().map(|g| g.initial_ammo));

    scratch.dir_hp.clear();
    scratch.dir_hp.extend(directs.iter().map(|d| d.hp));
    scratch.dir_engagements.resize(directs.len(), 0);
    scratch.dir_engagements.fill(0);

    scratch.ind_used.resize(total_ind, false);
    scratch.dir_used.resize(directs.len(), false);

    let mut total_val = 0.0_f32;

    for rnd in 0..HORIZON {
        let disc = ROUND_DISCOUNT.powi(rnd as i32);
        for gi in 0..total_ind {
            let g = ind_at(gi);
            if g.supplied && rnd >= g.delay {
                scratch.ammos[gi] = g.max_ammo;
            }
        }
        scratch.ind_used.fill(false);
        scratch.dir_used.fill(false);

        loop {
            let mut best_gain = 0.0_f32;
            // true=indirect, unit_idx, enemy_idx, expected_hp_reduction, self_wear
            let mut best_pick: Option<(bool, usize, usize, u32, u32)> = None;

            for gi in 0..total_ind {
                if scratch.ind_used[gi] || scratch.ammos[gi] == 0 {
                    continue;
                }
                let g = ind_at(gi);
                if rnd < g.delay {
                    continue;
                }
                let dlist = if rnd == 0 { &g.d0 } else { &g.dn };
                for &(ei, dmg, cf) in dlist {
                    if rnd < enemies[ei].arr_rnd {
                        continue;
                    }
                    let rhp = scratch.rem_hp[ei];
                    if rhp == 0 {
                        continue;
                    }
                    let eff = dmg.min(rhp);
                    let eff_cf = if eff >= rhp { 1.0 } else { cf };
                    let gain = eff as f32 * enemies[ei].w_per_hp * eff_cf * g.surv;
                    if gain > best_gain {
                        best_gain = gain;
                        let exp_red = ((eff as f32 * g.surv) + 0.5) as u32;
                        best_pick = Some((true, gi, ei, exp_red, 0));
                    }
                }
            }

            for (di, d) in directs.iter().enumerate() {
                if scratch.dir_used[di]
                    || scratch.dir_hp[di] <= DIRECT_MIN_CONTINUE_HP
                    || scratch.dir_engagements[di] >= DIRECT_MAX_ENGAGEMENTS
                {
                    continue;
                }
                let self_scale = scratch.dir_hp[di] as f32 / 100.0;
                for &(ready_r, ref atk) in &d.attacks {
                    if rnd < ready_r || rnd < enemies[atk.enemy_idx].arr_rnd {
                        continue;
                    }
                    let rhp = scratch.rem_hp[atk.enemy_idx];
                    if rhp == 0 {
                        continue;
                    }
                    let scaled_dmg = ((atk.dmg as f32 * self_scale) + 0.5) as u32;
                    if scaled_dmg == 0 {
                        continue;
                    }
                    let eff = scaled_dmg.min(rhp);
                    let post_rhp = rhp.saturating_sub(eff);
                    let actual_cnt = if post_rhp == 0 {
                        0.0
                    } else {
                        atk.dmg_cnt_100 * (post_rhp as f32 / 100.0)
                    };
                    let step_hp_loss = (actual_cnt + atk.retal_dmg).min(scratch.dir_hp[di] as f32);
                    let attrition_cost =
                        (step_hp_loss / 100.0) * DIRECT_ATTRITION_PER_1K * (d.cost as f32 / 1000.0);
                    let eff_cf = if eff >= rhp { 1.0 } else { atk.cripple_factor };
                    let raw_gain = eff as f32
                        * enemies[atk.enemy_idx].w_per_hp
                        * eff_cf
                        * direct_need[atk.enemy_idx];
                    let gain = (raw_gain - attrition_cost) * atk.late_disc;
                    if gain > best_gain {
                        best_gain = gain;
                        let self_wear = if step_hp_loss <= 0.0 {
                            0
                        } else {
                            ((step_hp_loss + 0.5) as u32).max(DIRECT_MIN_POSITIVE_WEAR)
                        };
                        best_pick = Some((false, di, atk.enemy_idx, eff, self_wear));
                    }
                }
            }

            let Some((is_ind, unit_idx, ei, red, self_wear)) = best_pick else {
                break;
            };
            if is_ind {
                scratch.ind_used[unit_idx] = true;
                scratch.ammos[unit_idx] = scratch.ammos[unit_idx].saturating_sub(1);
            } else {
                scratch.dir_used[unit_idx] = true;
                scratch.dir_engagements[unit_idx] =
                    scratch.dir_engagements[unit_idx].saturating_add(1);
                scratch.dir_hp[unit_idx] = scratch.dir_hp[unit_idx].saturating_sub(self_wear);
            }
            scratch.rem_hp[ei] = scratch.rem_hp[ei].saturating_sub(red);
            total_val += best_gain * disc;
        }
    }

    total_val
}

fn indirect_priority_enemies(
    threat_ctx: &ThreatContext,
    gun: &IndirectGunSpec,
) -> Vec<bevy_ecs::prelude::Entity> {
    let mut pairs: Vec<(f32, bevy_ecs::prelude::Entity)> = Vec::new();
    for &(ei, dmg, _) in gun.dn.iter().chain(gun.d0.iter()) {
        if let Some(ent) = threat_ctx.enemy_targets[ei].entity {
            let val = dmg as f32 * threat_ctx.enemy_targets[ei].w_per_hp;
            pairs.push((val, ent));
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut seen = HashSet::default();
    pairs
        .into_iter()
        .filter_map(|(_, e)| seen.insert(e).then_some(e))
        .collect()
}

fn direct_deployment_target(
    scan: &BoardScan,
    threat_ctx: &ThreatContext,
    direct_need: &[f32],
    spec: &DirectUnitSpec,
    fallback_origin: GridPosition,
) -> (GridPosition, Vec<bevy_ecs::prelude::Entity>) {
    let mut pairs: Vec<(f32, GridPosition, Option<bevy_ecs::prelude::Entity>)> = spec
        .attacks
        .iter()
        .map(|&(ready, ref atk)| {
            let et = &threat_ctx.enemy_targets[atk.enemy_idx];
            let eff = atk.dmg.min(et.hp);
            let post_rhp = et.hp.saturating_sub(eff);
            let actual_cnt = if post_rhp == 0 {
                0.0
            } else {
                atk.dmg_cnt_100 * (post_rhp as f32 / 100.0)
            };
            let step_hp_loss = (actual_cnt + atk.retal_dmg).min(spec.hp as f32);
            let attrition =
                (step_hp_loss / 100.0) * DIRECT_ATTRITION_PER_1K * (spec.cost as f32 / 1000.0);
            let raw_gain = eff as f32 * et.w_per_hp * direct_need[atk.enemy_idx];
            let score = (raw_gain - attrition) / (ready as f32 + 1.0);
            (score, et.pos_proj, et.entity)
        })
        .collect();
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));

    let anchor = pairs
        .first()
        .map(|p| p.1)
        .or_else(|| {
            scan.open_properties
                .iter()
                .min_by_key(|p| {
                    scan.map
                        .distance(fallback_origin.x, fallback_origin.y, p.x, p.y)
                })
                .copied()
        })
        .unwrap_or(fallback_origin);

    let mut seen = HashSet::default();
    let priority_enemies = pairs
        .into_iter()
        .filter_map(|(_, _, ent)| ent.filter(|e| seen.insert(*e)))
        .collect();
    (anchor, priority_enemies)
}
