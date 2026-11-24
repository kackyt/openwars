//! 5段階の役割別・制約ベース統合生産配分（異種価値の換算足し算不使用）。
//!
//! 占領・対空・間接・直接・資金を同一スケールへ換算する重み定数やビームサーチを廃し、
//! 盤面から直接観測できる物理量（敵航空機数・未所有物件の到着ターン差・前衛体数・
//! 同時隣接可能数 `firing_slots`・射点の 1T/2T 被弾・残枠あたり予算）のみに基づき、
//! 優先順位順に空き施設へ兵種を割り当てる。

use super::reach::{OCC_ENEMY, OCC_FRIENDLY, ReachMap, reachable_tiles, unit_turn_distance_field};
use super::threats::{
    ThreatContext, expected_damage, is_immobile_unit, max_base_damage, terrain_def_at,
};
use super::{UnifiedBoardExtras, WeaponProfileTable};
use crate::ai::islands::IslandMap;
use crate::ai::production::commit::PurchasePurpose;
use crate::ai::v4::operation::{OperationKind, SlotKind};
use crate::ai::v4::plan_revision::PlanStepRef;
use crate::ai::v4::property_control::capture_power_from_hp;
use crate::ai::v4::trace::{ProductionDecision, ProductionPlanTrace, ProductionStepTrace};
use crate::ai::v4::{
    BoardScan, PlannedDeployment, PlannedProduction, UnitSnapshot, campaign_execution, deployment,
};
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, PlayerId, UnitStats};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::supply_types;
use crate::resources::{MovementType, Terrain, UnitType};
use crate::systems::production::PRODUCTION_RANGE;
use bevy_ecs::prelude::Entity;

/// ユニットの満タン HP（生産時の初期 HP）。
const FULL_UNIT_HP: u32 = 100;

/// 対空・迎撃枠の候補として認める、満タン敵航空機の最大撃墜攻撃回数（3発以内）。
const MAX_AA_STRIKES_TO_KILL: u32 = 3;

/// 同一盤面内で `(始点, 移動タイプ, 移動力, 燃料)` ごとの最小ターン数場をキャッシュする。
#[derive(Default)]
struct TurnDistCache {
    fields: HashMap<(GridPosition, MovementType, u32, u32), ReachMap>,
}

impl TurnDistCache {
    fn turns_between(
        &mut self,
        scan: &BoardScan,
        from: GridPosition,
        to: GridPosition,
        movement_type: MovementType,
        max_movement: u32,
        max_fuel: u32,
    ) -> Option<u32> {
        let key = (from, movement_type, max_movement.max(1), max_fuel);
        let field = self.fields.entry(key).or_insert_with(|| {
            unit_turn_distance_field(
                &scan.map,
                &scan.master_data,
                from,
                movement_type,
                max_movement,
                max_fuel,
            )
        });
        field.get(to)
    }
}

/// 占領対象物件の物理情報。
#[derive(Debug, Clone)]
struct CapturePropInfo {
    pos: GridPosition,
    income: u32,
    cap_turns: u32,
    is_prod_mouth: bool,
    is_enemy_prop: bool,
    base_arrival: u32,
    best_foot_fac_arrival: u32,
}

/// 生産計画の状態を段階横断で保持するアキュムレータ。
struct AllocationContext<'a> {
    scan: &'a BoardScan,
    island_map: &'a IslandMap,
    player_id: PlayerId,
    rem_funds: u32,
    used_facs: HashSet<GridPosition>,
    commands: Vec<PlannedProduction>,
    plan_trace: ProductionPlanTrace,
    my_direct_cnt: usize,
    my_indirect_cnt: usize,
    existing_air_transports: usize,
    enemy_rem_hp: Vec<u32>,
    assigned_enemy_counts: Vec<usize>,
}

struct PurchaseOrder<'a> {
    fac: GridPosition,
    stats: &'a UnitStats,
    role: SlotKind,
    purpose: PurchasePurpose<PlanStepRef>,
    op_anchor: GridPosition,
    deployment: Option<PlannedDeployment>,
    capture_intent: Option<campaign_execution::CampaignProductionIntent>,
}

struct AntiAirCandidate<'a> {
    total_cost_gold: u32,
    cost: u32,
    total_turns: u32,
    dmg: u32,
    fac: GridPosition,
    stats: &'a UnitStats,
    enemy_idx: usize,
    entity: Option<Entity>,
}

type AirliftScoreKey = (bool, u32, u32, u32, usize, usize);

struct AirliftCandidate<'a> {
    score: AirliftScoreKey,
    fac: GridPosition,
    stats: &'a UnitStats,
    target: GridPosition,
}

struct IndirectCandidate<'a> {
    eff_score: u32,
    is_supply: bool,
    rev_delay: u32,
    fac: GridPosition,
    fpos: GridPosition,
    stats: &'a UnitStats,
    best_ei: usize,
    priority_enemies: Vec<Entity>,
}

struct DirectCandidate<'a> {
    score: f32,
    cost: u32,
    fac: GridPosition,
    stats: &'a UnitStats,
    best_ei: Option<usize>,
    eff_dmg: u32,
    anchor: GridPosition,
    priority_enemies: Vec<Entity>,
}

impl<'a> AllocationContext<'a> {
    fn can_purchase(&self, fac: GridPosition, cost: u32) -> bool {
        cost > 0 && cost <= self.rem_funds && !self.used_facs.contains(&fac)
    }

    fn record_purchase(&mut self, order: PurchaseOrder<'_>, profiles: &WeaponProfileTable) {
        let PurchaseOrder {
            fac,
            stats,
            role,
            purpose,
            op_anchor,
            deployment,
            capture_intent,
        } = order;
        let rem_before = self.rem_funds;
        self.rem_funds = self.rem_funds.saturating_sub(stats.cost);
        self.used_facs.insert(fac);
        if profiles.is_indirect(stats.unit_type) {
            self.my_indirect_cnt += 1;
        } else {
            self.my_direct_cnt += 1;
        }
        if stats.movement_type == MovementType::Air && stats.max_cargo > 0 {
            self.existing_air_transports += 1;
        }

        let cmd = ProduceUnitCommand {
            player_id: self.player_id,
            target_x: fac.x,
            target_y: fac.y,
            unit_type: stats.unit_type,
        };
        self.plan_trace.steps.push(ProductionStepTrace {
            operation_kind: OperationKind::Capture,
            operation_anchor: op_anchor,
            slot_kind: role,
            deficit_before: 1.0,
            deficit_after: 0.0,
            remaining_funds_before: rem_before,
            decision: ProductionDecision::Produced {
                unit_type: stats.unit_type,
                cost: stats.cost,
                facility: fac,
            },
        });
        self.commands.push(PlannedProduction {
            command: cmd,
            role,
            cost: stats.cost,
            purpose,
            deployment,
            capture_intent,
        });
    }
}

/// 5段階の役割別・制約ベース配分で当ターンの生産リストとトレースを生成する。
pub(in crate::ai::v4) fn plan_unified_production(
    scan: &BoardScan,
    extras: &UnifiedBoardExtras,
    player_id: PlayerId,
    turn: u32,
    allow_structural_slots: bool,
    _recent_enemy: &[(u32, UnitType)],
) -> (Vec<PlannedProduction>, ProductionPlanTrace) {
    let start_instant = std::time::Instant::now();
    let plan_trace = ProductionPlanTrace::new(player_id, scan.funds, scan.free_facilities.len());
    if scan.free_facilities.is_empty() || scan.funds == 0 {
        return (Vec::new(), plan_trace);
    }

    // 決定的な順序で施設と生産可能兵種を並べる
    let mut free_facilities = scan.free_facilities.clone();
    free_facilities.sort_unstable_by_key(|(pos, _)| (pos.y, pos.x));
    let mut available_types: Vec<(UnitType, &UnitStats)> = scan
        .available_types
        .iter()
        .map(|(ut, s)| (*ut, s))
        .collect();
    available_types.sort_unstable_by_key(|(ut, _)| ut.as_str());

    let profiles = WeaponProfileTable::from_scan(scan);
    let threat_ctx = ThreatContext::build(scan, extras, &profiles);
    let island_map = IslandMap::analyze(&scan.map);
    let mut dist_cache = TurnDistCache::default();

    let my_direct_cnt = threat_ctx.f_direct_units.len();
    let my_indirect_cnt = scan
        .my_units
        .iter()
        .filter(|u| profiles.is_indirect(u.stats.unit_type))
        .count();
    let existing_air_transports = scan
        .my_units
        .iter()
        .filter(|u| u.stats.movement_type == MovementType::Air && u.stats.max_cargo > 0)
        .count();
    let enemy_rem_hp: Vec<u32> = threat_ctx.enemies.iter().map(|e| e.hp).collect();
    let assigned_enemy_counts = vec![0_usize; threat_ctx.enemies.len()];

    let mut alloc = AllocationContext {
        scan,
        island_map: &island_map,
        player_id,
        rem_funds: scan.funds,
        used_facs: HashSet::default(),
        commands: Vec::new(),
        plan_trace,
        my_direct_cnt,
        my_indirect_cnt,
        existing_air_transports,
        enemy_rem_hp,
        assigned_enemy_counts,
    };

    // Phase 1: 占領・空輸枠（獲れる未所有物件の到着短縮分だけ：初動の占領工場を対空枠で塞がない）
    if allow_structural_slots {
        allocate_capture_and_airlift_phase(
            &mut alloc,
            extras,
            &free_facilities,
            &available_types,
            &profiles,
            &threat_ctx,
            &mut dist_cache,
        );
    }

    // Phase 2: 対空・制空枠（敵戦闘航空脅威があり既存対空で不足している分だけ）
    allocate_anti_air_phase(
        &mut alloc,
        &free_facilities,
        &available_types,
        &profiles,
        &threat_ctx,
        &mut dist_cache,
    );

    // Phase 3 & Phase 4: 前衛安全チェックと間接火網枠（安全な射点・有効目標があるとき）
    allocate_indirect_phase(
        &mut alloc,
        &free_facilities,
        &available_types,
        &profiles,
        &threat_ctx,
    );

    // Phase 5: 直接戦闘・対地航空枠（残りの空き施設と予算の配分）
    allocate_direct_combat_phase(
        &mut alloc,
        extras,
        &free_facilities,
        &available_types,
        &profiles,
        &threat_ctx,
        &mut dist_cache,
    );

    // 同一ターン内の施設割り当てねじれをコスト0のペア入替で解消し、展開到着ターンを最小化する
    optimize_unified_facility_assignments(
        scan,
        &profiles,
        &mut alloc.commands,
        &mut alloc.plan_trace,
        &mut dist_cache,
    );

    alloc.plan_trace.leftover_funds = alloc.rem_funds;
    alloc.plan_trace.uncommitted_funds = alloc.rem_funds;

    if std::env::var_os("OPENWARS_UNIFIED_PRODUCTION_TRACE").is_some() {
        let elapsed_ms = start_instant.elapsed().as_secs_f64() * 1000.0;
        let summary: Vec<String> = alloc
            .commands
            .iter()
            .map(|p| {
                format!(
                    "{:?}({:?})@({},{})",
                    p.command.unit_type, p.role, p.command.target_x, p.command.target_y
                )
            })
            .collect();
        eprintln!(
            "UNIFIED_PROD_V5 P{} T{} funds={} spent={} elapsed_ms={:.3} picked={:?}",
            player_id.0,
            turn,
            scan.funds,
            scan.funds.saturating_sub(alloc.rem_funds),
            elapsed_ms,
            summary,
        );
    }

    (alloc.commands, alloc.plan_trace)
}

/// Phase 1: 攻撃力を持つ敵航空ユニットに対して、既存対空戦力で不足している枠のみ対空／迎撃兵を割り当てる。
fn allocate_anti_air_phase(
    alloc: &mut AllocationContext<'_>,
    free_facilities: &[(GridPosition, Terrain)],
    available_types: &[(UnitType, &UnitStats)],
    profiles: &WeaponProfileTable,
    threat_ctx: &ThreatContext,
    dist_cache: &mut TurnDistCache,
) {
    let scan = alloc.scan;
    let map = &scan.map;
    let chart = &scan.damage_chart;

    // 輸送専用機を除いた戦闘航空敵を抽出
    let combat_air_indices: Vec<usize> = threat_ctx
        .enemies
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e.is_air
                && e.stats.max_cargo == 0
                && profiles.has_any_weapon(e.stats.unit_type)
                && e.hp > 0
        })
        .map(|(idx, _)| idx)
        .collect();
    if combat_air_indices.is_empty() {
        return;
    }

    // 既存味方のうち、敵航空機へ MAX_AA_STRIKES_TO_KILL 発以内相当の有効対空打撃を持ち展開可能な機数を数える
    // 2発以内（50%以上）の強対空ユニットは2機、3発以内の補助対空は1機をカバー可能とみなす
    let min_aa_dmg_threshold = FULL_UNIT_HP.div_ceil(MAX_AA_STRIKES_TO_KILL);
    let strong_aa_dmg_threshold = FULL_UNIT_HP / 2;
    let mut covered_air = 0_usize;
    for u in &scan.my_units {
        if u.hp < FULL_UNIT_HP / MAX_AA_STRIKES_TO_KILL
            || !profiles.has_any_weapon(u.stats.unit_type)
        {
            continue;
        }
        let ammo = (u.stats.max_ammo1, u.stats.max_ammo2);
        let is_ind = profiles.is_indirect(u.stats.unit_type);
        let best_covered_dmg = combat_air_indices
            .iter()
            .filter_map(|&ei| {
                let et = &threat_ctx.enemies[ei];
                let dmg_vs_full = expected_damage(
                    chart,
                    profiles,
                    u.stats.unit_type,
                    u.hp,
                    ammo,
                    et.stats.unit_type,
                    et.def_proj,
                    u.stats.min_range.max(1),
                    false,
                );
                if dmg_vs_full < min_aa_dmg_threshold {
                    return None;
                }
                let reachable = if is_ind {
                    let d = map.distance(u.pos.x, u.pos.y, et.pos_proj.x, et.pos_proj.y);
                    d <= u.stats.max_range.saturating_add(u.stats.max_movement * 2)
                } else {
                    map.get_adjacent(et.pos_proj.x, et.pos_proj.y)
                        .into_iter()
                        .any(|(ax, ay)| {
                            dist_cache
                                .turns_between(
                                    scan,
                                    u.pos,
                                    GridPosition { x: ax, y: ay },
                                    u.stats.movement_type,
                                    u.stats.max_movement,
                                    u.stats.max_fuel,
                                )
                                .is_some()
                        })
                };
                reachable.then_some(dmg_vs_full)
            })
            .max()
            .unwrap_or(0);
        if best_covered_dmg >= min_aa_dmg_threshold {
            covered_air += if best_covered_dmg >= strong_aa_dmg_threshold {
                2
            } else {
                1
            };
        }
    }

    let mut deficit = combat_air_indices.len().saturating_sub(covered_air);
    let plains_def = scan.master_data.get_terrain_defense_bonus(Terrain::Plains);
    let mut existing_aa_specialized_cnt = scan
        .my_units
        .iter()
        .filter(|u| is_aa_specialized(chart, u.stats.unit_type))
        .count();
    let max_aa_specialized = combat_air_indices.len().div_ceil(2).min(2);
    // 通常盤面では1ターンの対空連打による地上戦線崩壊を防ぐため追加は最大1体に制限する（全敵が航空機の盤面を除く）
    let max_purchases_this_turn = if threat_ctx.enemies.iter().all(|e| e.is_air) {
        usize::MAX
    } else {
        1
    };
    let mut purchased_this_turn = 0_usize;

    // 各敵戦闘航空機が 1 手番あたり自軍へ与え得る最大期待損害額（単位: Gold）を事前計算する
    let enemy_air_dmg_gold_per_turn: Vec<u32> = threat_ctx
        .enemies
        .iter()
        .map(|et| {
            if !et.is_air || et.stats.max_cargo > 0 || et.hp == 0 {
                return 0;
            }
            let dist = et.stats.min_range.max(1);
            if scan.my_units.is_empty() {
                available_types
                    .iter()
                    .filter(|(_, s)| {
                        !matches!(s.movement_type, MovementType::Air | MovementType::Ship)
                    })
                    .map(|&(ut, s)| {
                        let d_pct = expected_damage(
                            chart,
                            profiles,
                            et.stats.unit_type,
                            et.hp,
                            et.ammo,
                            ut,
                            plains_def,
                            dist,
                            false,
                        );
                        s.cost.saturating_mul(d_pct) / FULL_UNIT_HP
                    })
                    .max()
                    .unwrap_or(0)
            } else {
                scan.my_units
                    .iter()
                    .map(|u| {
                        let u_def = terrain_def_at(map, &scan.master_data, u.pos);
                        let d_pct = expected_damage(
                            chart,
                            profiles,
                            et.stats.unit_type,
                            et.hp,
                            et.ammo,
                            u.stats.unit_type,
                            u_def,
                            dist,
                            false,
                        );
                        u.stats.cost.saturating_mul(d_pct) / FULL_UNIT_HP
                    })
                    .max()
                    .unwrap_or(0)
            }
        })
        .collect();

    while deficit > 0 && purchased_this_turn < max_purchases_this_turn {
        let mut best_choice: Option<AntiAirCandidate<'_>> = None;

        for &(fac, terrain) in free_facilities {
            if alloc.used_facs.contains(&fac) {
                continue;
            }
            for &(ut, stats) in available_types {
                if !alloc.can_purchase(fac, stats.cost)
                    || !scan.can_produce(terrain, ut)
                    || !profiles.has_any_weapon(ut)
                    || is_non_combat_transport(chart, stats)
                {
                    continue;
                }
                let is_aa_spec = is_aa_specialized(chart, ut);
                if is_aa_spec
                    && existing_aa_specialized_cnt >= max_aa_specialized
                    && !threat_ctx.enemies.iter().all(|e| e.is_air)
                {
                    continue;
                }
                let is_ind = profiles.is_indirect(ut);
                if is_ind
                    && threat_ctx
                        .firing_tile_threat(scan, profiles, fac, stats)
                        .direct_1t_dmg
                        > 0
                {
                    continue;
                }
                let ammo = (stats.max_ammo1, stats.max_ammo2);

                for &ei in &combat_air_indices {
                    let rem_hp = alloc.enemy_rem_hp[ei];
                    if rem_hp == 0 {
                        continue;
                    }
                    let et = &threat_ctx.enemies[ei];
                    let dmg = expected_damage(
                        chart,
                        profiles,
                        ut,
                        FULL_UNIT_HP,
                        ammo,
                        et.stats.unit_type,
                        et.def_proj,
                        stats.min_range.max(1),
                        false,
                    );
                    if dmg < min_aa_dmg_threshold {
                        continue;
                    }
                    // 生産後の実行動は次ターン以降のため、実到達・射撃開始までのターン数と安全性を評価する
                    let ready_turns = if is_ind {
                        let d_fac = map.distance(fac.x, fac.y, et.pos_proj.x, et.pos_proj.y);
                        if d_fac >= stats.min_range && d_fac <= stats.max_range {
                            1
                        } else if d_fac <= stats.max_range.saturating_add(stats.max_movement) {
                            2
                        } else {
                            continue;
                        }
                    } else {
                        let best_adj = map
                            .get_adjacent(et.pos_proj.x, et.pos_proj.y)
                            .into_iter()
                            .filter_map(|(ax, ay)| {
                                let adj = GridPosition { x: ax, y: ay };
                                dist_cache
                                    .turns_between(
                                        scan,
                                        fac,
                                        adj,
                                        stats.movement_type,
                                        stats.max_movement,
                                        stats.max_fuel,
                                    )
                                    .map(|t| (t, terrain_def_at(map, &scan.master_data, adj), adj))
                            })
                            .min_by_key(|&(t, tdef, adj)| {
                                (t, std::cmp::Reverse(tdef), adj.y, adj.x)
                            });
                        let Some((mt, atk_def, _)) = best_adj else {
                            continue;
                        };
                        // 撃ち漏らし時の敵反撃で自軍対空ユニットが即死する候補は除外
                        let post_hp = et.hp.saturating_sub(dmg);
                        if post_hp > 0 {
                            let c100 = expected_damage(
                                chart,
                                profiles,
                                et.stats.unit_type,
                                FULL_UNIT_HP,
                                et.ammo,
                                ut,
                                atk_def,
                                1,
                                true,
                            );
                            let counter_dmg = c100.saturating_mul(post_hp) / FULL_UNIT_HP;
                            if counter_dmg >= FULL_UNIT_HP {
                                continue;
                            }
                        }
                        mt.max(1)
                    };
                    let strikes_to_kill = rem_hp.div_ceil(dmg.max(1));
                    let total_turns = ready_turns.saturating_add(strikes_to_kill);
                    // 撃墜までに敵航空機が与える期待損害額 ＋ 購入額（同一 Gold 単位）を最小化する
                    let expected_loss_until_kill_gold =
                        enemy_air_dmg_gold_per_turn[ei].saturating_mul(total_turns);
                    let total_cost_gold = expected_loss_until_kill_gold.saturating_add(stats.cost);
                    let cand_key = (
                        total_cost_gold,
                        stats.cost,
                        total_turns,
                        u32::MAX - dmg,
                        fac.y,
                        fac.x,
                        ut.as_str(),
                    );
                    let replace = best_choice.as_ref().is_none_or(|b| {
                        let b_key = (
                            b.total_cost_gold,
                            b.cost,
                            b.total_turns,
                            u32::MAX - b.dmg,
                            b.fac.y,
                            b.fac.x,
                            b.stats.unit_type.as_str(),
                        );
                        cand_key < b_key
                    });
                    if replace {
                        best_choice = Some(AntiAirCandidate {
                            total_cost_gold,
                            cost: stats.cost,
                            total_turns,
                            dmg,
                            fac,
                            stats,
                            enemy_idx: ei,
                            entity: et.entity,
                        });
                    }
                }
            }
        }

        let Some(chosen) = best_choice else {
            break;
        };
        let best_ei = chosen.enemy_idx;
        let target_pos = threat_ctx.enemies[best_ei].pos_proj;
        let mut priority_enemies = Vec::new();
        if let Some(ent) = chosen.entity {
            priority_enemies.push(ent);
        }
        for &ei in &combat_air_indices {
            if ei != best_ei
                && let Some(ent) = threat_ctx.enemies[ei].entity
            {
                priority_enemies.push(ent);
            }
        }
        let dep = PlannedDeployment {
            anchor: target_pos,
            staging_anchor: target_pos,
            posture: deployment::DeploymentPosture::Execute,
            slot_kind: SlotKind::Combat,
            priority_enemies,
            threat_horizon: 4,
            forecast: deployment::DeploymentForecast::default(),
            plan_step: None,
            operation_binding: None,
        };
        alloc.record_purchase(
            PurchaseOrder {
                fac: chosen.fac,
                stats: chosen.stats,
                role: SlotKind::Combat,
                purpose: PurchasePurpose::ImmediateReinforcement,
                op_anchor: target_pos,
                deployment: Some(dep),
                capture_intent: None,
            },
            profiles,
        );
        if is_aa_specialized(chart, chosen.stats.unit_type) {
            existing_aa_specialized_cnt += 1;
        }
        alloc.enemy_rem_hp[best_ei] = alloc.enemy_rem_hp[best_ei].saturating_sub(chosen.dmg);
        alloc.assigned_enemy_counts[best_ei] += 1;
        purchased_this_turn += 1;
        covered_air += if chosen.dmg >= strong_aa_dmg_threshold {
            2
        } else {
            1
        };
        deficit = combat_air_indices.len().saturating_sub(covered_air);
    }
}

/// Phase 2: 獲れる未所有物件に対して既存占領役より到着を早められる分だけ占領兵・空輸輸送機を割り当てる。
fn allocate_capture_and_airlift_phase(
    alloc: &mut AllocationContext<'_>,
    extras: &UnifiedBoardExtras,
    free_facilities: &[(GridPosition, Terrain)],
    available_types: &[(UnitType, &UnitStats)],
    profiles: &WeaponProfileTable,
    threat_ctx: &ThreatContext,
    dist_cache: &mut TurnDistCache,
) {
    let scan = alloc.scan;
    if scan.open_properties.is_empty() {
        return;
    }
    let map = &scan.map;
    let master = &scan.master_data;
    let chart = &scan.damage_chart;

    let existing_capturers: Vec<&UnitSnapshot> = scan
        .my_units
        .iter()
        .filter(|u| u.stats.can_capture)
        .collect();
    let capturer_pos_set: HashSet<GridPosition> =
        existing_capturers.iter().map(|u| u.pos).collect();
    let enemy_prop_set: HashSet<GridPosition> =
        extras.enemy_properties.iter().map(|(p, _)| *p).collect();
    let ref_cap = scan.reference_capture_unit().cloned().or_else(|| {
        available_types
            .iter()
            .find(|(_, s)| s.can_capture)
            .map(|(_, s)| (*s).clone())
    });
    let Some(ref_cap_stats) = ref_cap else {
        return;
    };
    let cap_power = capture_power_from_hp(100);
    let default_city_dur = master.landscape_durability(Terrain::City.as_str()).max(1);
    let default_city_income = master.landscape_income(Terrain::City.as_str()).max(1);

    let capture_unit_stats: Vec<(UnitType, &UnitStats)> = available_types
        .iter()
        .filter(|(_, s)| s.can_capture)
        .copied()
        .collect();
    let capture_unit_types: Vec<UnitType> = capture_unit_stats.iter().map(|(ut, _)| *ut).collect();

    let mut props: Vec<CapturePropInfo> = Vec::new();
    for &p in &scan.open_properties {
        if capturer_pos_set.contains(&p) || threat_ctx.occ.at(p) == OCC_ENEMY {
            // 自軍占領兵が既に立っている、または敵が居座って塞いでいる物件は占領単独枠に入れない
            continue;
        }
        let terrain = map.get_terrain(p.x, p.y).unwrap_or(Terrain::City);
        let landscape = master.get_landscape_by_name(terrain.as_str());
        // 単体テスト等の合成盤面で平地上に open_properties が置かれた場合は都市の耐久・収入で補う
        let durability = landscape
            .map(|r| r.durability)
            .filter(|&d| d > 0)
            .unwrap_or(default_city_dur);
        let income = landscape
            .and_then(|r| r.income)
            .filter(|&v| v > 0)
            .unwrap_or(default_city_income);
        let cap_turns = durability.div_ceil(cap_power).max(1);
        let is_enemy_prop = enemy_prop_set.contains(&p);
        let is_prod_mouth = scan.capital_position.is_some_and(|cap| {
            map.distance(p.x, p.y, cap.x, cap.y) <= PRODUCTION_RANGE as u32
                && landscape.and_then(|r| r.supply_type.as_deref()) == Some(supply_types::GROUND)
        });
        let p_def = terrain_def_at(map, master, p);

        // 1T以内に敵直接部隊から1撃で致死ダメージを受ける無防備物件へ裸の占領兵を出さない
        let adj = map.get_adjacent(p.x, p.y);
        let max_1t_dmg = threat_ctx
            .live_reaches
            .iter()
            .filter(|lr| !profiles.is_indirect(lr.unit.stats.unit_type))
            .filter(|lr| {
                adj.iter()
                    .any(|&(nx, ny)| lr.reach_1t.contains(GridPosition { x: nx, y: ny }))
            })
            .map(|lr| {
                expected_damage(
                    chart,
                    profiles,
                    lr.unit.stats.unit_type,
                    lr.unit.hp,
                    lr.ammo,
                    ref_cap_stats.unit_type,
                    p_def,
                    1,
                    false,
                )
            })
            .max()
            .unwrap_or(0);
        if max_1t_dmg >= 100 {
            continue;
        }

        // 敵工場至近の敵所有拠点で複数の敵が守備している場合は単独占領役を送らない
        if is_enemy_prop && scan.enemy_units.len() >= 2 {
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
            if min_enemy_fac_dist <= PRODUCTION_RANGE as u32 && min_enemy_fac_dist < min_my_fac_dist
            {
                continue;
            }
        }

        // 工場生産可能な最速徒歩到着ターンを見積もる
        let mut best_foot_fac_arr = u32::MAX;
        for &(pfac, pterrain) in &scan.production_facilities {
            for &(cut, cst) in &capture_unit_stats {
                if !scan.can_produce(pterrain, cut) {
                    continue;
                }
                if let Some(t) = dist_cache.turns_between(
                    scan,
                    pfac,
                    p,
                    cst.movement_type,
                    cst.max_movement,
                    cst.max_fuel,
                ) {
                    best_foot_fac_arr = best_foot_fac_arr.min(t.max(1).saturating_add(cap_turns));
                }
            }
        }

        // 中立物件で敵占領役の方が自軍最短より先に占領完了でき距離も近い場合は先取不能として除外
        if !is_enemy_prop {
            let enemy_cap_fastest = scan
                .enemy_units
                .iter()
                .filter(|e| e.stats.can_capture)
                .filter_map(|e| {
                    dist_cache
                        .turns_between(
                            scan,
                            e.pos,
                            p,
                            e.stats.movement_type,
                            e.stats.max_movement,
                            e.stats.max_fuel,
                        )
                        .map(|eta| {
                            (
                                eta.saturating_add(cap_turns),
                                map.distance(e.pos.x, e.pos.y, p.x, p.y),
                            )
                        })
                })
                .min();
            let existing_my_arr = existing_capturers
                .iter()
                .filter_map(|u| {
                    dist_cache
                        .turns_between(
                            scan,
                            u.pos,
                            p,
                            u.stats.movement_type,
                            u.stats.max_movement,
                            u.stats.max_fuel,
                        )
                        .map(|eta| (eta.max(1), map.distance(u.pos.x, u.pos.y, p.x, p.y)))
                })
                .min();
            let fac_my_arr = free_facilities
                .iter()
                .filter_map(|&(fp, ft)| {
                    capture_unit_stats
                        .iter()
                        .filter(|&&(cut, _)| scan.can_produce(ft, cut))
                        .filter_map(|&(_, cst)| {
                            dist_cache
                                .turns_between(
                                    scan,
                                    fp,
                                    p,
                                    cst.movement_type,
                                    cst.max_movement,
                                    cst.max_fuel,
                                )
                                .map(|eta| (eta.max(1), map.distance(fp.x, fp.y, p.x, p.y)))
                        })
                        .min()
                })
                .min();
            let my_fastest_arr = match (existing_my_arr, fac_my_arr) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            if let (Some((e_comp, e_dist)), Some((my_arr, my_dist))) =
                (enemy_cap_fastest, my_fastest_arr)
                && e_comp <= my_arr
                && e_dist < my_dist
            {
                continue;
            }
        }

        props.push(CapturePropInfo {
            pos: p,
            income,
            cap_turns,
            is_prod_mouth,
            is_enemy_prop,
            base_arrival: u32::MAX,
            best_foot_fac_arrival: best_foot_fac_arr,
        });
    }

    // 首都圏生産口・高収入・近接順に並べる
    props.sort_by_key(|cp| {
        (
            !cp.is_prod_mouth,
            std::cmp::Reverse(cp.income),
            !cp.is_enemy_prop,
            cp.best_foot_fac_arrival,
            cp.pos.y,
            cp.pos.x,
        )
    });

    // 既存占領役を最短完了順に 1 対 1 マッチしてベースライン到着ターンを決める
    let mut existing_pairs: Vec<(u32, usize, usize)> = Vec::new();
    for (ui, cap_u) in existing_capturers.iter().enumerate() {
        for (pi, cp) in props.iter().enumerate() {
            if let Some(eta) = dist_cache.turns_between(
                scan,
                cap_u.pos,
                cp.pos,
                cap_u.stats.movement_type,
                cap_u.stats.max_movement,
                cap_u.stats.max_fuel,
            ) {
                existing_pairs.push((eta.max(1).saturating_add(cp.cap_turns), pi, ui));
            }
        }
    }
    existing_pairs.sort_unstable();
    let mut used_existing = vec![false; existing_capturers.len()];
    for (arr, pi, ui) in existing_pairs {
        if used_existing[ui] || props[pi].base_arrival != u32::MAX {
            continue;
        }
        used_existing[ui] = true;
        props[pi].base_arrival = arr;
    }

    // 空輸輸送機：空輸で解放または 2 ターン以上短縮できる未所有物件数が既存輸送機数を上回る場合だけ 1 機追加
    let mut airlift_needed_props: HashSet<usize> = HashSet::default();
    let mut best_airlift: Option<AirliftCandidate<'_>> = None;
    for &(fac, terrain) in free_facilities {
        if alloc.used_facs.contains(&fac) {
            continue;
        }
        for &(ut, stats) in available_types {
            if !alloc.can_purchase(fac, stats.cost)
                || !scan.can_produce(terrain, ut)
                || stats.movement_type != MovementType::Air
                || stats.max_cargo == 0
                || !stats
                    .loadable_unit_types
                    .iter()
                    .any(|lt| capture_unit_types.contains(lt))
            {
                continue;
            }
            for (pidx, cp) in props.iter().enumerate() {
                let Some(air_eta) = dist_cache.turns_between(
                    scan,
                    fac,
                    cp.pos,
                    MovementType::Air,
                    stats.max_movement,
                    stats.max_fuel,
                ) else {
                    continue;
                };
                // 積載・降下オーバーヘッド 1T を含めた空輸完了ターン
                let airlift_arr = air_eta.saturating_add(1).saturating_add(cp.cap_turns);
                let eff_foot = cp.base_arrival.min(cp.best_foot_fac_arrival);
                let accelerates = eff_foot == u32::MAX || airlift_arr.saturating_add(2) <= eff_foot;
                if !accelerates {
                    continue;
                }
                airlift_needed_props.insert(pidx);
                let gain_rank = eff_foot.saturating_sub(airlift_arr);
                // 到達不能解放を最優先、次いで短縮ターン数・収入・到着早さ・座標順
                let score_key = (
                    eff_foot == u32::MAX,
                    gain_rank,
                    cp.income,
                    u32::MAX.saturating_sub(airlift_arr),
                    usize::MAX - fac.y,
                    usize::MAX - fac.x,
                );
                if best_airlift.as_ref().is_none_or(|b| score_key > b.score) {
                    best_airlift = Some(AirliftCandidate {
                        score: score_key,
                        fac,
                        stats,
                        target: cp.pos,
                    });
                }
            }
        }
    }
    if airlift_needed_props.len() > alloc.existing_air_transports
        && let Some(chosen) = best_airlift
    {
        let fac = chosen.fac;
        let stats = chosen.stats;
        let target = chosen.target;
        let island_id = alloc
            .island_map
            .get_island_at(&target)
            .or_else(|| alloc.island_map.get_island_at(&fac))
            .map_or(crate::ai::islands::IslandId(0), |island| island.id);
        let intent = campaign_execution::CampaignProductionIntent {
            command: ProduceUnitCommand {
                player_id: alloc.player_id,
                target_x: fac.x,
                target_y: fac.y,
                unit_type: stats.unit_type,
            },
            island_id,
            role: campaign_execution::CampaignProductionRole::Transport,
            mission_target: Some(target),
        };
        alloc.record_purchase(
            PurchaseOrder {
                fac,
                stats,
                role: SlotKind::Transport,
                purpose: PurchasePurpose::OperationSlot,
                op_anchor: target,
                deployment: None,
                capture_intent: Some(intent),
            },
            profiles,
        );
    }

    // 地上占領割当：敵が存在し空き地上工場が 2 口以上ある場合は戦闘用に最低 1 口と予算を残す
    let free_ground_fac_cnt = free_facilities
        .iter()
        .filter(|&&(fac, terrain)| {
            !alloc.used_facs.contains(&fac) && is_ground_fac(terrain, &scan.master_data)
        })
        .count();
    let reserve_combat_fac = usize::from(!scan.enemy_units.is_empty() && free_ground_fac_cnt >= 2);
    let max_capture_slots = free_ground_fac_cnt.saturating_sub(reserve_combat_fac);
    let reserve_combat_funds = if reserve_combat_fac > 0 {
        available_types
            .iter()
            .filter(|&&(ut, s)| {
                is_mobile_ground_combat(chart, profiles, s)
                    && free_facilities.iter().any(|&(fac, terrain)| {
                        !alloc.used_facs.contains(&fac) && scan.can_produce(terrain, ut)
                    })
            })
            .map(|(_, s)| s.cost)
            .min()
            .unwrap_or(0)
    } else {
        0
    };

    let mut assigned_props = vec![false; props.len()];
    let mut issued_caps = 0_usize;
    while issued_caps < max_capture_slots {
        // MRV（最小残余値）: 未割当物件ごとに、今買える範囲で達成可能な最短到着ターンと達成工場数を求める
        let mut prop_best: Vec<(u32, usize)> = vec![(u32::MAX, 0); props.len()];
        for (pidx, cp) in props.iter().enumerate() {
            if assigned_props[pidx] {
                continue;
            }
            let mut best_arr = u32::MAX;
            let mut best_facs = 0_usize;
            for &(fac, terrain) in free_facilities {
                if alloc.used_facs.contains(&fac) {
                    continue;
                }
                let mut fac_min_arr = u32::MAX;
                for &(ut, stats) in &capture_unit_stats {
                    if !alloc.can_purchase(fac, stats.cost)
                        || !scan.can_produce(terrain, ut)
                        || (issued_caps >= 1
                            && alloc.rem_funds.saturating_sub(stats.cost) < reserve_combat_funds)
                    {
                        continue;
                    }
                    let Some(eta) = dist_cache.turns_between(
                        scan,
                        fac,
                        cp.pos,
                        stats.movement_type,
                        stats.max_movement,
                        stats.max_fuel,
                    ) else {
                        continue;
                    };
                    let new_arr = eta.max(1).saturating_add(cp.cap_turns);
                    if new_arr < cp.base_arrival {
                        fac_min_arr = fac_min_arr.min(new_arr);
                    }
                }
                if fac_min_arr < best_arr {
                    best_arr = fac_min_arr;
                    best_facs = 1;
                } else if fac_min_arr == best_arr && fac_min_arr < u32::MAX {
                    best_facs += 1;
                }
            }
            prop_best[pidx] = (best_arr, best_facs);
        }

        type CaptureKey<'k> = (
            bool,
            u32,
            usize,
            std::cmp::Reverse<u32>,
            u32,
            std::cmp::Reverse<u32>,
            usize,
            usize,
            &'k str,
        );
        let mut best_cap: Option<(CaptureKey<'_>, GridPosition, &UnitStats, usize)> = None;

        for (pidx, cp) in props.iter().enumerate() {
            let (min_arr, min_arr_fac_count) = prop_best[pidx];
            if assigned_props[pidx] || min_arr == u32::MAX {
                continue;
            }
            for &(fac, terrain) in free_facilities {
                if alloc.used_facs.contains(&fac) {
                    continue;
                }
                let dist = map.distance(fac.x, fac.y, cp.pos.x, cp.pos.y);
                for &(ut, stats) in &capture_unit_stats {
                    if !alloc.can_purchase(fac, stats.cost)
                        || !scan.can_produce(terrain, ut)
                        || (issued_caps >= 1
                            && alloc.rem_funds.saturating_sub(stats.cost) < reserve_combat_funds)
                    {
                        continue;
                    }
                    let Some(eta) = dist_cache.turns_between(
                        scan,
                        fac,
                        cp.pos,
                        stats.movement_type,
                        stats.max_movement,
                        stats.max_fuel,
                    ) else {
                        continue;
                    };
                    let new_arr = eta.max(1).saturating_add(cp.cap_turns);
                    if new_arr >= cp.base_arrival {
                        continue;
                    }
                    let key: CaptureKey<'_> = (
                        !cp.is_prod_mouth,
                        new_arr,
                        min_arr_fac_count,
                        std::cmp::Reverse(cp.income),
                        stats.cost,
                        std::cmp::Reverse(dist),
                        fac.y,
                        fac.x,
                        ut.as_str(),
                    );
                    if best_cap.as_ref().is_none_or(|b| key < b.0) {
                        best_cap = Some((key, fac, stats, pidx));
                    }
                }
            }
        }

        let Some((_, fac, stats, pidx)) = best_cap else {
            break;
        };
        assigned_props[pidx] = true;
        let target = props[pidx].pos;
        let island_id = alloc
            .island_map
            .get_island_at(&target)
            .or_else(|| alloc.island_map.get_island_at(&fac))
            .map_or(crate::ai::islands::IslandId(0), |island| island.id);
        let intent = campaign_execution::CampaignProductionIntent {
            command: ProduceUnitCommand {
                player_id: alloc.player_id,
                target_x: fac.x,
                target_y: fac.y,
                unit_type: stats.unit_type,
            },
            island_id,
            role: campaign_execution::CampaignProductionRole::Capture,
            mission_target: Some(target),
        };
        let dep = PlannedDeployment {
            anchor: target,
            staging_anchor: target,
            posture: deployment::DeploymentPosture::Execute,
            slot_kind: SlotKind::Capture,
            priority_enemies: Vec::new(),
            threat_horizon: 4,
            forecast: deployment::DeploymentForecast::default(),
            plan_step: None,
            operation_binding: None,
        };
        alloc.record_purchase(
            PurchaseOrder {
                fac,
                stats,
                role: SlotKind::Capture,
                purpose: PurchasePurpose::OperationSlot,
                op_anchor: target,
                deployment: Some(dep),
                capture_intent: Some(intent),
            },
            profiles,
        );
        issued_caps += 1;
    }
}

/// ヘックス盤面の標準的な前線アプローチ（幅2マス以上の進路）において、
/// 間接砲1門の後方射点を敵地上部隊の回り込みから ZOC・占有で塞ぐために必要な前衛部隊数の下限（砲1門あたり前衛2体）。
const MIN_SCREEN_UNITS_PER_INDIRECT: usize = 2;

/// 前線突破力を損なわずに間接火網を維持できる総門数上限。
const MAX_INDIRECT_UNITS: usize = 4;

/// Phase 3 & Phase 4: 前衛安全条件を満たし安全な射点と有効目標がある場合のみ間接火網を 1 門割り当てる。
fn allocate_indirect_phase(
    alloc: &mut AllocationContext<'_>,
    free_facilities: &[(GridPosition, Terrain)],
    available_types: &[(UnitType, &UnitStats)],
    profiles: &WeaponProfileTable,
    threat_ctx: &ThreatContext,
) {
    let scan = alloc.scan;
    if threat_ctx.enemies.is_empty() || alloc.my_indirect_cnt >= MAX_INDIRECT_UNITS {
        return;
    }
    // 既存間接が前衛比上限に達している（砲1門あたり前衛2体未満になる）場合は新規追加しない
    if alloc.my_direct_cnt < (alloc.my_indirect_cnt + 1) * MIN_SCREEN_UNITS_PER_INDIRECT {
        return;
    }

    let map = &scan.map;
    let chart = &scan.damage_chart;
    let total_ground_facs = threat_ctx.ground_facility_set.len();
    let free_ground_fac_cnt = free_facilities
        .iter()
        .filter(|&&(fac, terrain)| {
            !alloc.used_facs.contains(&fac) && is_ground_fac(terrain, &scan.master_data)
        })
        .count();
    if free_ground_fac_cnt == 0 {
        return;
    }

    // 退避可能な既存自軍前衛座標（補給拠点上の味方は位置を譲れる）
    let vacatable_direct: HashSet<GridPosition> = threat_ctx
        .f_direct_units
        .iter()
        .map(|u| u.pos)
        .filter(|pos| threat_ctx.is_ground_supply(*pos))
        .collect();

    // 数的劣勢なら残余工場に最低1体の直接部隊を残すため予備資金を確保する
    let min_ground_cost = available_types
        .iter()
        .filter(|&&(ut, s)| {
            !profiles.is_indirect(ut)
                && can_hit_ground(chart, ut)
                && !matches!(s.movement_type, MovementType::Air | MovementType::Ship)
                && free_facilities.iter().any(|&(fac, terrain)| {
                    !alloc.used_facs.contains(&fac) && scan.can_produce(terrain, ut)
                })
        })
        .map(|(_, s)| s.cost)
        .min()
        .unwrap_or(0);
    let outnumbered = alloc.my_direct_cnt < threat_ctx.e_direct_cnt;

    let mut best_indirect: Option<IndirectCandidate<'_>> = None;

    for &(ut, stats) in available_types {
        if !profiles.is_indirect(ut) || !can_hit_ground(chart, ut) || stats.cost == 0 {
            continue;
        }
        // Phase 3 前衛安全チェック：数的劣勢時は残資金の過半を使う高価間接を買わない
        if outnumbered
            && (stats.cost.saturating_mul(2) > alloc.rem_funds
                || stats.cost.saturating_add(min_ground_cost) > alloc.rem_funds)
        {
            continue;
        }
        // 数が同等以下の場合も、高価な間接1門で全予算を使い切って残りの空き地上工場をすべて遊ばせないよう、
        // 空き地上工場が2つ以上ある場合は少なくとも残りの空き地上工場数（上限3）分の最低地上兵生産費を残す。
        // また、敵に直接戦闘車両（戦車以上）が1両もおらず歩兵・間接中心の段階では、残資金の過半を1門の間接砲に投じない。
        let has_enemy_heavy_armor = threat_ctx
            .enemies
            .iter()
            .any(|e| !e.is_indirect && !e.is_air && e.stats.cost >= 10_000);
        if !has_enemy_heavy_armor && stats.cost.saturating_mul(2) > alloc.rem_funds {
            continue;
        }
        let min_remaining_ground_fills = if outnumbered || free_ground_fac_cnt >= 3 {
            (free_ground_fac_cnt.saturating_sub(1).min(3)) as u32
        } else {
            0
        };
        if stats
            .cost
            .saturating_add(min_ground_cost.saturating_mul(min_remaining_ground_fills))
            > alloc.rem_funds
        {
            continue;
        }
        let immobile = is_immobile_unit(stats);
        if immobile && free_ground_fac_cnt < 2 {
            continue;
        }

        // この間接ユニットが使える (射点, 施設, 遅延) の組合せを集める
        let mut candidate_tiles: Vec<(GridPosition, GridPosition, u32)> = Vec::new();
        for &(fac, terrain) in free_facilities {
            if alloc.used_facs.contains(&fac) || !scan.can_produce(terrain, ut) {
                continue;
            }
            if !immobile || total_ground_facs >= 3 {
                candidate_tiles.push((fac, fac, 0));
            }
            if !immobile {
                let r1 = reachable_tiles(
                    map,
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
                        candidate_tiles.push((p, fac, 1));
                    }
                }
            }
        }

        for (fpos, fac, delay) in candidate_tiles {
            if is_immobile_unit(stats) && threat_ctx.ground_facility_set.contains(&fpos) {
                continue;
            }
            let tile_threat = threat_ctx.firing_tile_threat(scan, profiles, fpos, stats);
            // 1T 敵直接被弾があるタイル、または対砲兵で即死圏のタイルは拒否
            if tile_threat.direct_1t_dmg > 0 || tile_threat.counter_battery_dmg >= 60 {
                continue;
            }
            // 数的優劣に関わらず、壁の隙間等を抜けて 2T 以内に射点隣接へ到達され被弾するタイル、
            // または遮蔽無視 2T 脅威があるのに進路上の護衛が 1 体もいない射点は拒否する
            if tile_threat.breach_2t_blk_dmg > 0
                || (tile_threat.breach_2t_prc_dmg > 0 && tile_threat.positional_screen_units == 0)
            {
                continue;
            }

            // 射程内ターゲットへの有効期待ダメージ集計（隘路 enemy 2x・直接支援価値）
            let (rmin, rmax) = (stats.min_range, stats.max_range);
            let mut weighted_dmg = 0_u32;
            let mut max_single_dmg = 0_u32;
            let mut target_pairs: Vec<(u32, usize, Option<Entity>)> = Vec::new();

            for (ei, et) in threat_ctx.enemies.iter().enumerate() {
                let rem_hp = alloc.enemy_rem_hp[ei];
                if rem_hp == 0 {
                    continue;
                }
                // 移動を伴う間接配置 (delay > 0) では移動後攻撃不可ルールのために初撃が 2T 後となり、
                // 既に占領完了が迫っている開けた平地上の敵占領兵を完了前に妨害できないため直接機動枠へ譲る
                if delay > 0 && et.threatens_property && et.stats.can_capture && et.firing_slots > 2
                {
                    continue;
                }
                let dp = map.distance(fpos.x, fpos.y, et.pos_proj.x, et.pos_proj.y);
                let dmg_p = if dp >= rmin && dp <= rmax {
                    expected_damage(
                        chart,
                        profiles,
                        ut,
                        100,
                        (stats.max_ammo1, stats.max_ammo2),
                        et.stats.unit_type,
                        et.def_proj,
                        dp,
                        false,
                    )
                } else {
                    0
                };
                let d0 = map.distance(fpos.x, fpos.y, et.pos0.x, et.pos0.y);
                let dmg_0 = if delay == 0 && d0 >= rmin && d0 <= rmax {
                    expected_damage(
                        chart,
                        profiles,
                        ut,
                        100,
                        (stats.max_ammo1, stats.max_ammo2),
                        et.stats.unit_type,
                        et.def0,
                        d0,
                        false,
                    )
                } else {
                    0
                };
                let dmg = dmg_p.max(dmg_0);
                if dmg == 0 {
                    continue;
                }
                let eff = dmg.min(rem_hp);
                max_single_dmg = max_single_dmg.max(eff);
                // 橋・山隘路など隣接攻撃枠が少ない敵ほど間接火網の価値が高い
                let choke_mult = if et.firing_slots <= 2 { 2 } else { 1 };
                let score_e = eff * choke_mult;
                weighted_dmg = weighted_dmg.saturating_add(score_e);
                target_pairs.push((score_e, ei, et.entity));
            }

            if max_single_dmg < 25 && weighted_dmg < 45 {
                continue;
            }

            target_pairs.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let priority_enemies: Vec<Entity> = target_pairs.iter().filter_map(|t| t.2).collect();
            let best_ei = target_pairs.first().map(|t| t.1).unwrap_or(0);
            let is_supply = threat_ctx.is_ground_supply(fpos);
            let off_facility = !threat_ctx.ground_facility_set.contains(&fpos);
            // 生産施設ではない補給都市（補給砲座）から射撃できる配置は毎ターン弾薬・HP補給と地形防御30%を得られるため遅延ペナルティを軽減する
            let delay_weight = if is_supply && off_facility { 5 } else { 15 };
            let eff_score = weighted_dmg * 100 / (100 + delay * delay_weight);
            let cand_key = (eff_score, is_supply, off_facility, u32::MAX - delay);
            let replace = best_indirect.as_ref().is_none_or(|b| {
                let b_key = (
                    b.eff_score,
                    b.is_supply,
                    !threat_ctx.ground_facility_set.contains(&b.fpos),
                    b.rev_delay,
                );
                cand_key > b_key
                    || (cand_key == b_key
                        && (stats.cost, fpos.y, fpos.x, fac.y, fac.x)
                            < (b.stats.cost, b.fpos.y, b.fpos.x, b.fac.y, b.fac.x))
            });
            if replace {
                best_indirect = Some(IndirectCandidate {
                    eff_score,
                    is_supply,
                    rev_delay: u32::MAX - delay,
                    fac,
                    fpos,
                    stats,
                    best_ei,
                    priority_enemies,
                });
            }
        }
    }

    if let Some(chosen) = best_indirect {
        let fac = chosen.fac;
        let anchor = chosen.fpos;
        let stats = chosen.stats;
        let best_ei = chosen.best_ei;
        let dep = PlannedDeployment {
            anchor,
            staging_anchor: anchor,
            posture: deployment::DeploymentPosture::Execute,
            slot_kind: SlotKind::Combat,
            priority_enemies: chosen.priority_enemies,
            threat_horizon: 4,
            forecast: deployment::DeploymentForecast::default(),
            plan_step: None,
            operation_binding: None,
        };
        alloc.record_purchase(
            PurchaseOrder {
                fac,
                stats,
                role: SlotKind::Combat,
                purpose: PurchasePurpose::ImmediateReinforcement,
                op_anchor: anchor,
                deployment: Some(dep),
                capture_intent: None,
            },
            profiles,
        );
        if best_ei < alloc.enemy_rem_hp.len() {
            let best_dmg = expected_damage(
                chart,
                profiles,
                stats.unit_type,
                100,
                (stats.max_ammo1, stats.max_ammo2),
                threat_ctx.enemies[best_ei].stats.unit_type,
                threat_ctx.enemies[best_ei].def_proj,
                map.distance(
                    anchor.x,
                    anchor.y,
                    threat_ctx.enemies[best_ei].pos_proj.x,
                    threat_ctx.enemies[best_ei].pos_proj.y,
                ),
                false,
            );
            alloc.enemy_rem_hp[best_ei] = alloc.enemy_rem_hp[best_ei].saturating_sub(best_dmg);
            alloc.assigned_enemy_counts[best_ei] += 1;
        }
    }
}

/// Phase 5: 残りの空き空港・空き地上工場へ予算と頭数に応じた直接戦闘／対地航空部隊を割り当てる。
fn allocate_direct_combat_phase(
    alloc: &mut AllocationContext<'_>,
    extras: &UnifiedBoardExtras,
    free_facilities: &[(GridPosition, Terrain)],
    available_types: &[(UnitType, &UnitStats)],
    profiles: &WeaponProfileTable,
    threat_ctx: &ThreatContext,
    dist_cache: &mut TurnDistCache,
) {
    let scan = alloc.scan;
    if scan.enemy_units.is_empty() && extras.enemy_properties.is_empty() {
        return;
    }

    let map = &scan.map;
    let chart = &scan.damage_chart;

    // 空港を先に処理（地上工場に資金を吸い尽くされて空き空港が遊ばないようにする）、続いて前線に近い地上工場
    let mut rem_facs: Vec<(GridPosition, Terrain, bool)> = free_facilities
        .iter()
        .copied()
        .filter(|(fac, _)| !alloc.used_facs.contains(fac))
        .map(|(fac, terrain)| {
            let is_air_fac = !is_ground_fac(terrain, &scan.master_data);
            (fac, terrain, is_air_fac)
        })
        .collect();
    let dist_to_front = |fac: GridPosition| -> u32 {
        threat_ctx
            .enemies
            .iter()
            .map(|e| map.distance(fac.x, fac.y, e.pos_proj.x, e.pos_proj.y))
            .min()
            .or_else(|| {
                scan.open_properties
                    .iter()
                    .map(|p| map.distance(fac.x, fac.y, p.x, p.y))
                    .min()
            })
            .unwrap_or(0)
    };
    rem_facs.sort_by_key(|&(fac, _, is_air_fac)| (!is_air_fac, dist_to_front(fac), fac.y, fac.x));

    // 最安価な対地地上戦闘兵コスト（多工場時・頭数不足時の残スロット確保基準）
    let min_ground_cost = available_types
        .iter()
        .filter(|&&(ut, s)| {
            !profiles.is_indirect(ut)
                && can_hit_ground(chart, ut)
                && !matches!(s.movement_type, MovementType::Air | MovementType::Ship)
        })
        .map(|(_, s)| s.cost)
        .min()
        .unwrap_or(1000);
    // 地上機動戦闘車両が未配備のときは予約予算によって最初の1両（装甲車等）が買えなくなるのを防ぐ
    let min_mobile_ground_cost = available_types
        .iter()
        .filter(|&&(_, s)| {
            is_mobile_ground_combat(chart, profiles, s)
                && rem_facs.iter().any(|&(_, terrain, is_air_fac)| {
                    !is_air_fac && scan.can_produce(terrain, s.unit_type)
                })
        })
        .map(|(_, s)| s.cost)
        .min()
        .unwrap_or(0);
    let mut has_mobile_ground_combat = scan
        .my_units
        .iter()
        .any(|u| is_mobile_ground_combat(chart, profiles, &u.stats))
        || alloc.commands.iter().any(|p| {
            available_types
                .iter()
                .find(|(ut, _)| *ut == p.command.unit_type)
                .is_some_and(|(_, s)| is_mobile_ground_combat(chart, profiles, s))
        });

    for idx in 0..rem_facs.len() {
        if alloc.rem_funds == 0 {
            break;
        }
        let (fac, terrain, is_air_fac) = rem_facs[idx];
        let rem_ground_after = rem_facs[idx + 1..]
            .iter()
            .filter(|&&(_, _, is_air)| !is_air)
            .count();

        // 残る地上工場でも頭数を出せるよう、必要数分の最低資金だけは残して上限を決める。
        // 空港が先に処理される際も、頭数劣勢または前線地上枠が空いているなら残り地上工場の最低歩兵生産費を確保する。
        let mut reserve = if alloc.rem_funds < min_ground_cost {
            0
        } else {
            let needed_after = if alloc.my_direct_cnt < threat_ctx.e_direct_cnt {
                rem_ground_after.min(threat_ctx.e_direct_cnt - alloc.my_direct_cnt)
            } else {
                rem_ground_after
            };
            let max_reserve = min_ground_cost.saturating_mul(needed_after as u32);
            max_reserve.min(alloc.rem_funds.saturating_sub(min_ground_cost))
        };
        if !is_air_fac
            && !has_mobile_ground_combat
            && rem_ground_after <= 1
            && min_mobile_ground_cost > 0
            && alloc.rem_funds >= min_mobile_ground_cost
            && alloc.rem_funds.saturating_sub(reserve) < min_mobile_ground_cost
        {
            reserve = 0;
        }
        if is_air_fac
            && rem_ground_after >= 1
            && !has_mobile_ground_combat
            && min_mobile_ground_cost > min_ground_cost
        {
            let mobile_ground_reserve = min_mobile_ground_cost
                .saturating_add(min_ground_cost.saturating_mul((rem_ground_after - 1) as u32));
            reserve = reserve
                .max(mobile_ground_reserve.min(alloc.rem_funds.saturating_sub(min_ground_cost)));
        }
        let max_spend = alloc.rem_funds.saturating_sub(reserve);

        // 残余敵がいるのに全て割り当て済みになったら元の HP を再参照して飽和工場からも出撃させる
        let any_enemy_left = alloc.enemy_rem_hp.iter().any(|&h| h > 0);

        let mut best_cand: Option<DirectCandidate<'_>> = None;

        for &(ut, stats) in available_types {
            if stats.cost == 0
                || stats.cost > max_spend
                || !scan.can_produce(terrain, ut)
                || profiles.is_indirect(ut)
                || !profiles.has_any_weapon(ut)
                || is_non_combat_transport(chart, stats)
                || !can_hit_ground(chart, ut)
            {
                continue;
            }
            let is_air = stats.movement_type == MovementType::Air;
            let is_ship = stats.movement_type == MovementType::Ship;
            if !is_air
                && !is_ship
                && !threat_ctx.can_reach_enemy_via_land
                && !scan.enemy_units.iter().any(|e| {
                    dist_cache
                        .turns_between(
                            scan,
                            fac,
                            e.pos,
                            stats.movement_type,
                            stats.max_movement,
                            stats.max_fuel,
                        )
                        .is_some()
                })
            {
                continue;
            }

            // 敵ユニットがいない（敵物件のみ残る）盤面では最速の占領可能／対地ユニットを物件へ向かわせる
            if threat_ctx.enemies.is_empty() {
                if let Some(target) = scan
                    .open_properties
                    .iter()
                    .filter_map(|&p| {
                        dist_cache
                            .turns_between(
                                scan,
                                fac,
                                p,
                                stats.movement_type,
                                stats.max_movement,
                                stats.max_fuel,
                            )
                            .map(|t| (t, p))
                    })
                    .min_by_key(|&(t, p)| (t, p.y, p.x))
                {
                    let score = 100.0 / (target.0 as f32 + 1.0);
                    let replace = best_cand.as_ref().is_none_or(|b| {
                        score > b.score || (score == b.score && stats.cost < b.cost)
                    });
                    if replace {
                        best_cand = Some(DirectCandidate {
                            score,
                            cost: stats.cost,
                            fac,
                            stats,
                            best_ei: None,
                            eff_dmg: 0,
                            anchor: target.1,
                            priority_enemies: Vec::new(),
                        });
                    }
                }
                continue;
            }

            // 各敵ターゲットに対する純打撃性能（到着の早さ・占領阻止・反撃交換比）を評価する
            let mut best_target_score = 0.0_f32;
            let mut best_ei_opt: Option<usize> = None;
            let mut best_eff_dmg = 0_u32;
            let mut target_rankings: Vec<(f32, GridPosition, Option<Entity>)> = Vec::new();

            for (ei, et) in threat_ctx.enemies.iter().enumerate() {
                let deal = expected_damage(
                    chart,
                    profiles,
                    ut,
                    100,
                    (stats.max_ammo1, stats.max_ammo2),
                    et.stats.unit_type,
                    et.def_proj,
                    1,
                    false,
                );
                if deal == 0 {
                    continue;
                }
                let best_adj = map
                    .get_adjacent(et.pos_proj.x, et.pos_proj.y)
                    .into_iter()
                    .filter_map(|(ax, ay)| {
                        let adj = GridPosition { x: ax, y: ay };
                        dist_cache
                            .turns_between(
                                scan,
                                fac,
                                adj,
                                stats.movement_type,
                                stats.max_movement,
                                stats.max_fuel,
                            )
                            .map(|t| (t, terrain_def_at(map, &scan.master_data, adj), adj))
                    })
                    .min_by_key(|&(t, tdef, adj)| (t, std::cmp::Reverse(tdef), adj.y, adj.x));
                let Some((move_turns, atk_def, atk_tile)) = best_adj else {
                    continue;
                };

                // 航空ユニットは攻撃対象（`et`）を今回の攻撃（`deal`）で削った・撃破した後の盤面において、
                // 攻撃地点周囲で 1T 以内に残存敵から受ける対空／迎撃ダメージ合計が重大（>= 80%）なら特攻出撃させない。
                // 対空戦車自身を爆撃機で直接叩き潰す場合（92%ダメージ）は、その対空戦車からの反撃が激減するため候補に残る。
                if is_air {
                    let lethal_aa_sum: u32 = threat_ctx
                        .live_reaches
                        .iter()
                        .filter_map(|lr| {
                            let eu = lr.unit.stats.unit_type;
                            let rem_enemy_hp =
                                if lr.unit.pos == et.pos0 && lr.unit.entity == et.entity {
                                    lr.unit.hp.saturating_sub(deal)
                                } else {
                                    lr.unit.hp
                                };
                            if rem_enemy_hp == 0 {
                                return None;
                            }
                            let eval_dist = if profiles.is_indirect(eu) {
                                let d = map.distance(
                                    lr.unit.pos.x,
                                    lr.unit.pos.y,
                                    atk_tile.x,
                                    atk_tile.y,
                                );
                                (d >= lr.unit.stats.min_range && d <= lr.unit.stats.max_range)
                                    .then_some(d)
                            } else if lr.unit.stats.movement_type == MovementType::Air {
                                // 敵ターンは他敵が退避して射線を開けられるため航空迎撃は移動圏+1で判定
                                (map.distance(lr.unit.pos.x, lr.unit.pos.y, atk_tile.x, atk_tile.y)
                                    <= lr.unit.stats.max_movement.saturating_add(1))
                                .then_some(1)
                            } else {
                                map.get_adjacent(atk_tile.x, atk_tile.y)
                                    .into_iter()
                                    .any(|(ax, ay)| {
                                        lr.reach_1t.contains(GridPosition { x: ax, y: ay })
                                    })
                                    .then_some(1)
                            }?;
                            let dmg = expected_damage(
                                chart,
                                profiles,
                                eu,
                                rem_enemy_hp,
                                lr.ammo,
                                ut,
                                atk_def,
                                eval_dist,
                                false,
                            );
                            (dmg > 0).then_some(dmg)
                        })
                        .sum();
                    if lethal_aa_sum >= 80 {
                        continue;
                    }
                }

                let rem_hp = if any_enemy_left {
                    alloc.enemy_rem_hp[ei]
                } else {
                    et.hp / (1 + alloc.assigned_enemy_counts[ei] as u32)
                };
                if rem_hp == 0 {
                    continue;
                }
                let eff_deal = deal.min(rem_hp);
                let post_hp = et.hp.saturating_sub(deal.min(et.hp));
                let counter_dmg = if !et.is_indirect && post_hp > 0 {
                    let c100 = expected_damage(
                        chart,
                        profiles,
                        et.stats.unit_type,
                        100,
                        et.ammo,
                        ut,
                        atk_def,
                        1,
                        true,
                    );
                    (c100 * post_hp) / 100
                } else {
                    0
                };

                // 物理的交換成果：与えた有効ダメージが半減された反撃損耗を上回ること（歩兵の対車両特攻等を抑止）
                let exchange_score = deal as f32 - 0.75 * counter_dmg as f32;
                if exchange_score <= 0.0 && max_spend > min_ground_cost {
                    continue;
                }
                let base_exchange =
                    exchange_score.max(1.0) * (eff_deal as f32 / deal.max(1) as f32);
                // 占領中の敵占領役の占領力を 1 撃で半減させる高速打撃（例: Recon 66% vs Infantry）を優先
                let anti_cap_mult = if et.threatens_property && deal >= et.hp / 2 {
                    2.0
                } else {
                    1.0
                };
                // 航空機は地形や地上戦線の渋滞を受けずに直行できるため到着遅延を低く保つ
                let eta_denom = move_turns.max(1) as f32;
                let t_score = (base_exchange * anti_cap_mult) / eta_denom;

                target_rankings.push((t_score, et.pos_proj, et.entity));
                if t_score > best_target_score {
                    best_target_score = t_score;
                    best_ei_opt = Some(ei);
                    best_eff_dmg = eff_deal;
                }
            }

            if best_target_score <= 0.0 {
                // 既存の有利目標がない場合でも頭数劣勢または多工場時は廉価前衛で埋める
                if alloc.my_direct_cnt >= threat_ctx.e_direct_cnt && stats.cost > min_ground_cost {
                    continue;
                }
                let anchor = threat_ctx
                    .enemies
                    .first()
                    .map(|e| e.pos_proj)
                    .unwrap_or_else(|| nearest_open_property_or(scan, fac));
                let fallback_score = 1.0 / (stats.cost as f32 / 1000.0).max(1.0);
                let replace = best_cand.as_ref().is_none_or(|b| fallback_score > b.score);
                if replace {
                    best_cand = Some(DirectCandidate {
                        score: fallback_score,
                        cost: stats.cost,
                        fac,
                        stats,
                        best_ei: None,
                        eff_dmg: 0,
                        anchor,
                        priority_enemies: Vec::new(),
                    });
                }
                continue;
            }

            target_rankings.sort_by(|a, b| b.0.total_cmp(&a.0));
            // 1体の柔らかい目標（例: 歩兵）に対する瞬間スコアだけでなく、上位複数目標への汎用打撃力も評価し、
            // 敵の戦車・車両に対処できない偵察車ばかりの偏重を防ぐ。
            let secondary_versatility: f32 = target_rankings
                .iter()
                .skip(1)
                .take(2)
                .map(|(s, _, _)| *s * 0.25)
                .sum();
            let candidate_score = best_target_score + secondary_versatility;
            let anchor = target_rankings.first().map_or(fac, |t| t.1);
            let mut seen = HashSet::default();
            let priority_enemies: Vec<Entity> = target_rankings
                .into_iter()
                .filter_map(|(_, _, ent)| ent.filter(|e| seen.insert(*e)))
                .collect();

            let replace = best_cand.as_ref().is_none_or(|b| {
                candidate_score > b.score + 1.0e-3
                    || ((candidate_score - b.score).abs() <= 1.0e-3 && stats.cost < b.cost)
            });
            if replace {
                best_cand = Some(DirectCandidate {
                    score: candidate_score,
                    cost: stats.cost,
                    fac,
                    stats,
                    best_ei: best_ei_opt,
                    eff_dmg: best_eff_dmg,
                    anchor,
                    priority_enemies,
                });
            }
        }

        if let Some(chosen) = best_cand {
            let fac = chosen.fac;
            let anchor = chosen.anchor;
            let stats = chosen.stats;
            let dep = PlannedDeployment {
                anchor,
                staging_anchor: anchor,
                posture: deployment::DeploymentPosture::Execute,
                slot_kind: SlotKind::Combat,
                priority_enemies: chosen.priority_enemies,
                threat_horizon: 4,
                forecast: deployment::DeploymentForecast::default(),
                plan_step: None,
                operation_binding: None,
            };
            alloc.record_purchase(
                PurchaseOrder {
                    fac,
                    stats,
                    role: SlotKind::Combat,
                    purpose: PurchasePurpose::ImmediateReinforcement,
                    op_anchor: anchor,
                    deployment: Some(dep),
                    capture_intent: None,
                },
                profiles,
            );
            if is_mobile_ground_combat(chart, profiles, stats) {
                has_mobile_ground_combat = true;
            }
            if let Some(best_ei) = chosen.best_ei {
                alloc.enemy_rem_hp[best_ei] =
                    alloc.enemy_rem_hp[best_ei].saturating_sub(chosen.eff_dmg);
                alloc.assigned_enemy_counts[best_ei] += 1;
            }
        }
    }
}

/// 同一ターン内の複数生産コマンドについて、各ユニットの展開／占領目標への到達ターン数合計が
/// 短縮されるペアがある場合に生産施設の割り当てを相互に入れ替え、工場と目標の前後ねじれを解消する。
fn optimize_unified_facility_assignments(
    scan: &BoardScan,
    profiles: &WeaponProfileTable,
    commands: &mut [PlannedProduction],
    plan_trace: &mut ProductionPlanTrace,
    dist_cache: &mut TurnDistCache,
) {
    if commands.len() <= 1 {
        return;
    }
    let stats_by_type: HashMap<UnitType, &UnitStats> =
        scan.available_types.iter().map(|(t, s)| (*t, s)).collect();

    /// 施設から目標位置までの実移動到達ターン数とヘックス直線距離を返す。
    /// 通常は `TurnDistCache` の実地形ターン数を優先し、両比較対象とも到達不能な地形上目標のみ理論下限で比較する。
    fn assignment_eta_and_dist(
        scan: &BoardScan,
        dist_cache: &mut TurnDistCache,
        from: GridPosition,
        target: GridPosition,
        stats: &UnitStats,
    ) -> (Option<u32>, u32) {
        let dist = scan.map.distance(from.x, from.y, target.x, target.y);
        let turns = dist_cache
            .turns_between(
                scan,
                from,
                target,
                stats.movement_type,
                stats.max_movement,
                stats.max_fuel,
            )
            .map(|t| t.max(1));
        (turns, dist)
    }

    /// 生産施設の地形を、自軍の生産施設一覧（盤面の正本）から引く。一覧にない座標のみ地図の地形を使う。
    fn facility_terrain(scan: &BoardScan, p: GridPosition) -> Option<Terrain> {
        scan.production_facilities
            .iter()
            .find(|&&(fp, _)| fp == p)
            .map(|&(_, t)| t)
            .or_else(|| scan.map.get_terrain(p.x, p.y))
    }

    let mut improved = true;
    let mut iterations = 0_usize;
    while improved && iterations < 32 {
        improved = false;
        iterations += 1;
        for i in 0..commands.len() {
            for j in (i + 1)..commands.len() {
                let unit_i = commands[i].command.unit_type;
                let unit_j = commands[j].command.unit_type;
                // 間接砲は安全性検査済みの射点と施設をセットで選んでいるため位置入替の対象外とする
                if profiles.is_indirect(unit_i) || profiles.is_indirect(unit_j) {
                    continue;
                }
                let Some(&stats_i) = stats_by_type.get(&unit_i) else {
                    continue;
                };
                let Some(&stats_j) = stats_by_type.get(&unit_j) else {
                    continue;
                };

                let target_i = commands[i]
                    .capture_intent
                    .as_ref()
                    .and_then(|intent| intent.mission_target)
                    .or_else(|| commands[i].deployment.as_ref().map(|d| d.staging_anchor));
                let target_j = commands[j]
                    .capture_intent
                    .as_ref()
                    .and_then(|intent| intent.mission_target)
                    .or_else(|| commands[j].deployment.as_ref().map(|d| d.staging_anchor));
                let (Some(target_i), Some(target_j)) = (target_i, target_j) else {
                    continue;
                };

                let pos_i = GridPosition {
                    x: commands[i].command.target_x,
                    y: commands[i].command.target_y,
                };
                let pos_j = GridPosition {
                    x: commands[j].command.target_x,
                    y: commands[j].command.target_y,
                };
                if pos_i == pos_j {
                    continue;
                }

                let terrain_i = facility_terrain(scan, pos_i);
                let terrain_j = facility_terrain(scan, pos_j);
                let (Some(terrain_i), Some(terrain_j)) = (terrain_i, terrain_j) else {
                    continue;
                };
                if !scan.can_produce(terrain_j, unit_i) || !scan.can_produce(terrain_i, unit_j) {
                    continue;
                }

                let (t_i_curr, d_i_curr) =
                    assignment_eta_and_dist(scan, dist_cache, pos_i, target_i, stats_i);
                let (t_j_curr, d_j_curr) =
                    assignment_eta_and_dist(scan, dist_cache, pos_j, target_j, stats_j);
                let (t_i_swap, d_i_swap) =
                    assignment_eta_and_dist(scan, dist_cache, pos_j, target_i, stats_i);
                let (t_j_swap, d_j_swap) =
                    assignment_eta_and_dist(scan, dist_cache, pos_i, target_j, stats_j);

                // 元は到達可能だったユニットを未到達施設へ飛ばす入替は絶対に許可しない
                if (t_i_curr.is_some() && t_i_swap.is_none())
                    || (t_j_curr.is_some() && t_j_swap.is_none())
                {
                    continue;
                }

                let eta_i_curr =
                    t_i_curr.unwrap_or_else(|| d_i_curr.div_ceil(stats_i.max_movement.max(1)));
                let eta_j_curr =
                    t_j_curr.unwrap_or_else(|| d_j_curr.div_ceil(stats_j.max_movement.max(1)));
                let eta_i_swap =
                    t_i_swap.unwrap_or_else(|| d_i_swap.div_ceil(stats_i.max_movement.max(1)));
                let eta_j_swap =
                    t_j_swap.unwrap_or_else(|| d_j_swap.div_ceil(stats_j.max_movement.max(1)));

                let curr_key = (
                    eta_i_curr.saturating_add(eta_j_curr),
                    d_i_curr.saturating_add(d_j_curr),
                );
                let swap_key = (
                    eta_i_swap.saturating_add(eta_j_swap),
                    d_i_swap.saturating_add(d_j_swap),
                );

                if swap_key < curr_key {
                    commands[i].command.target_x = pos_j.x;
                    commands[i].command.target_y = pos_j.y;
                    if let Some(intent) = commands[i].capture_intent.as_mut() {
                        intent.command.target_x = pos_j.x;
                        intent.command.target_y = pos_j.y;
                    }

                    commands[j].command.target_x = pos_i.x;
                    commands[j].command.target_y = pos_i.y;
                    if let Some(intent) = commands[j].capture_intent.as_mut() {
                        intent.command.target_x = pos_i.x;
                        intent.command.target_y = pos_i.y;
                    }

                    if commands.len() == plan_trace.steps.len() {
                        if let ProductionDecision::Produced { facility, .. } =
                            &mut plan_trace.steps[i].decision
                        {
                            *facility = pos_j;
                        }
                        if let ProductionDecision::Produced { facility, .. } =
                            &mut plan_trace.steps[j].decision
                        {
                            *facility = pos_i;
                        }
                    }
                    improved = true;
                }
            }
        }
    }
}

#[inline]
fn is_ground_fac(
    terrain: Terrain,
    master: &crate::resources::master_data::MasterDataRegistry,
) -> bool {
    master
        .get_landscape_by_name(terrain.as_str())
        .and_then(|r| r.supply_type.as_deref())
        == Some(supply_types::GROUND)
}

#[inline]
fn can_hit_ground(chart: &crate::resources::DamageChart, ut: UnitType) -> bool {
    max_base_damage(chart, ut, UnitType::Infantry) > 0
        || max_base_damage(chart, ut, UnitType::Tank) > 0
}

/// 敵装甲車両（軽戦車級）に対して有効な打撃力（基礎ダメージ20以上）を持つか判定する。
#[inline]
fn can_hit_armor(chart: &crate::resources::DamageChart, ut: UnitType) -> bool {
    max_base_damage(chart, ut, UnitType::Tank) >= 20
}

/// 対装甲能力を持たず、もっぱら対空迎撃を担う対空特化ユニット（対空戦車・ミサイル・戦闘機等）か判定する。
/// 歩兵のマシンガン等（対ヘリ15%以下）を誤って対空特化として数えないよう、有効な対空打撃力の有無も確認する。
#[inline]
fn is_aa_specialized(chart: &crate::resources::DamageChart, ut: UnitType) -> bool {
    !can_hit_armor(chart, ut)
        && (max_base_damage(chart, ut, UnitType::Bcopters) >= 34
            || max_base_damage(chart, ut, UnitType::Bomber) >= 34
            || max_base_damage(chart, ut, UnitType::Fighter) >= 34)
}

/// 占領兵ではなく対地直接攻撃を担う地上機動戦闘車両（装甲車・戦車等）か判定する。
#[inline]
fn is_mobile_ground_combat(
    chart: &crate::resources::DamageChart,
    profiles: &WeaponProfileTable,
    stats: &UnitStats,
) -> bool {
    let ut = stats.unit_type;
    !stats.can_capture
        && !profiles.is_indirect(ut)
        && can_hit_ground(chart, ut)
        && !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
        && !is_non_combat_transport(chart, stats)
}

#[inline]
fn is_non_combat_transport(chart: &crate::resources::DamageChart, stats: &UnitStats) -> bool {
    stats.max_cargo > 0
        && (matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
            || !can_hit_ground(chart, stats.unit_type))
}

fn nearest_open_property_or(scan: &BoardScan, fallback_origin: GridPosition) -> GridPosition {
    scan.open_properties
        .iter()
        .min_by_key(|p| {
            scan.map
                .distance(fallback_origin.x, fallback_origin.y, p.x, p.y)
        })
        .copied()
        .unwrap_or(fallback_origin)
}
