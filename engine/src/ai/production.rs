use crate::ai::island_campaign::{IslandCampaignShortfall, campaign_unit_type_rank};
use crate::ai::strategy::{
    ProductionPlan, ProductionStrategy, analyze_strategy_for_turn,
    sea_transport_capacity_from_slots,
};
use crate::components::{Faction, GridPosition, PlayerId, Property, UnitStats};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, MovementType, Players, Terrain, UnitRegistry, UnitType};
use bevy_ecs::prelude::*;

use super::strategy::GamePhase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CampaignProductionRequirement {
    HeavyTransport,
    LightTransport,
    Capture,
    Combat,
}

#[derive(Debug)]
struct CampaignProductionOutcome {
    commands: Vec<ProduceUnitCommand>,
    remaining_funds: u32,
    used_facilities: std::collections::HashSet<GridPosition>,
    completed_all_rows: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProductionCandidate {
    score: u32,
    facility_position: GridPosition,
    unit_type: UnitType,
    cost: u32,
    max_cargo: u32,
    can_capture: bool,
}

fn compare_production_candidates(
    left: &ProductionCandidate,
    right: &ProductionCandidate,
) -> std::cmp::Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.facility_position.y.cmp(&right.facility_position.y))
        .then_with(|| left.facility_position.x.cmp(&right.facility_position.x))
        .then_with(|| {
            campaign_unit_type_rank(left.unit_type).cmp(&campaign_unit_type_rank(right.unit_type))
        })
        .then_with(|| left.cost.cmp(&right.cost))
}

fn select_best_production_candidate(
    candidates: &[ProductionCandidate],
) -> Option<&ProductionCandidate> {
    candidates
        .iter()
        .filter(|candidate| candidate.score > 0)
        .min_by(|left, right| compare_production_candidates(left, right))
}

fn next_campaign_requirement(
    shortfall: &IslandCampaignShortfall,
) -> Option<CampaignProductionRequirement> {
    if shortfall.heavy_transport_slots > 0 {
        Some(CampaignProductionRequirement::HeavyTransport)
    } else if shortfall.light_transport_slots > 0 {
        Some(CampaignProductionRequirement::LightTransport)
    } else if shortfall.capture_units > 0 {
        Some(CampaignProductionRequirement::Capture)
    } else if shortfall.combat_budget > 0 {
        Some(CampaignProductionRequirement::Combat)
    } else {
        None
    }
}

fn campaign_candidate_matches(
    requirement: CampaignProductionRequirement,
    unit_type: UnitType,
    stats: &UnitStats,
) -> bool {
    match requirement {
        CampaignProductionRequirement::HeavyTransport => unit_type == UnitType::Lander,
        CampaignProductionRequirement::LightTransport => unit_type == UnitType::TransportHelicopter,
        CampaignProductionRequirement::Capture => stats.can_capture,
        CampaignProductionRequirement::Combat => !matches!(
            unit_type,
            UnitType::TransportHelicopter | UnitType::Lander | UnitType::SupplyTruck
        ),
    }
}

fn consume_transport_demand_after_production(
    strategy: &mut ProductionStrategy,
    unit_type: UnitType,
    max_cargo: u32,
    is_v3: bool,
) {
    if is_v3 {
        let (light_slots, heavy_slots) = sea_transport_capacity_from_slots(unit_type, max_cargo);
        match unit_type {
            UnitType::TransportHelicopter => {
                strategy.light_transport_demand =
                    strategy.light_transport_demand.saturating_sub(light_slots);
            }
            UnitType::Lander if strategy.heavy_transport_demand > 0 => {
                strategy.heavy_transport_demand =
                    strategy.heavy_transport_demand.saturating_sub(heavy_slots);
            }
            UnitType::Lander => {
                strategy.light_transport_demand =
                    strategy.light_transport_demand.saturating_sub(light_slots);
            }
            _ => {}
        }
        return;
    }

    // V1は従来どおり、Lander以外のcargo枠を軽輸送需要へ計上する。
    if max_cargo == 0 {
        return;
    }
    if unit_type == UnitType::Lander && strategy.heavy_transport_demand > 0 {
        strategy.heavy_transport_demand = strategy.heavy_transport_demand.saturating_sub(max_cargo);
    } else {
        strategy.light_transport_demand = strategy.light_transport_demand.saturating_sub(max_cargo);
    }
}

fn consume_campaign_candidate(
    shortfall: &mut IslandCampaignShortfall,
    requirement: CampaignProductionRequirement,
    stats: &UnitStats,
) {
    match requirement {
        CampaignProductionRequirement::HeavyTransport => {
            shortfall.heavy_transport_slots = shortfall
                .heavy_transport_slots
                .saturating_sub(stats.max_cargo);
        }
        CampaignProductionRequirement::LightTransport => {
            shortfall.light_transport_slots = shortfall
                .light_transport_slots
                .saturating_sub(stats.max_cargo);
        }
        CampaignProductionRequirement::Capture => {
            shortfall.capture_units = shortfall.capture_units.saturating_sub(1);
        }
        CampaignProductionRequirement::Combat => {
            shortfall.combat_budget = shortfall.combat_budget.saturating_sub(stats.cost);
        }
    }
    shortfall.reserved_budget = shortfall.reserved_budget.saturating_sub(stats.cost);
}

fn plan_campaign_shortfall_production(
    player_id: PlayerId,
    shortfalls: &[IslandCampaignShortfall],
    facilities: &[(GridPosition, Terrain)],
    available_types: &[(UnitType, UnitStats)],
    master_data: &MasterDataRegistry,
    available_funds: u32,
) -> CampaignProductionOutcome {
    let mut rows = shortfalls.to_vec();
    rows.sort_by_key(|row| (row.priority_rank, row.island_id.0));
    let mut sorted_facilities = facilities.to_vec();
    sorted_facilities.sort_by_key(|(position, _)| (position.y, position.x));
    let mut sorted_types = available_types.to_vec();
    sorted_types
        .sort_by_key(|(unit_type, stats)| (campaign_unit_type_rank(*unit_type), stats.cost));

    let mut outcome = CampaignProductionOutcome {
        commands: Vec::new(),
        remaining_funds: available_funds,
        used_facilities: std::collections::HashSet::new(),
        completed_all_rows: true,
    };

    for row in &mut rows {
        while let Some(requirement) = next_campaign_requirement(row) {
            let mut candidates = Vec::new();
            for (facility_position, terrain) in &sorted_facilities {
                if outcome.used_facilities.contains(facility_position) {
                    continue;
                }
                for (unit_type, stats) in &sorted_types {
                    if !master_data.can_produce_unit(terrain.as_str(), *unit_type)
                        || !campaign_candidate_matches(requirement, *unit_type, stats)
                        || stats.cost > outcome.remaining_funds
                        || stats.cost > row.reserved_budget
                    {
                        continue;
                    }
                    let combat_coverage = if requirement == CampaignProductionRequirement::Combat {
                        stats.cost.min(row.combat_budget)
                    } else {
                        0
                    };
                    candidates.push((
                        std::cmp::Reverse(combat_coverage),
                        stats.cost,
                        campaign_unit_type_rank(*unit_type),
                        facility_position.y,
                        facility_position.x,
                        *facility_position,
                        *unit_type,
                        stats,
                    ));
                }
            }
            candidates.sort_by_key(|candidate| {
                (
                    candidate.0,
                    candidate.1,
                    candidate.2,
                    candidate.3,
                    candidate.4,
                )
            });
            let Some((_, _, _, _, _, position, unit_type, stats)) = candidates.first() else {
                // 高優先rowを今ターン完了できない場合、予約資源を下位rowやgeneric需要へ流さない。
                outcome.completed_all_rows = false;
                return outcome;
            };

            outcome.commands.push(ProduceUnitCommand {
                player_id,
                target_x: position.x,
                target_y: position.y,
                unit_type: *unit_type,
            });
            outcome.remaining_funds = outcome.remaining_funds.saturating_sub(stats.cost);
            outcome.used_facilities.insert(*position);
            consume_campaign_candidate(row, requirement, stats);
        }
    }

    outcome
}

/// 生産AI。
/// 以下のロジックで生産計画を立てます。
/// - 歩兵・重歩兵は占領等のため10体を目安に高く評価
/// - その他のユニットは戦略（フェーズ）、アンチ性能、到達ターン数（ETA）に基づき多角的に評価
/// - 予算（貯金を差し引いた仮想予算）内で最も評価が高くなるよう動的計画法（ナップサック問題）で生産を決定
pub fn decide_production(world: &mut World, player_id: PlayerId) -> Vec<ProduceUnitCommand> {
    let mut commands = Vec::new();

    let strategy = analyze_strategy_for_turn(world, player_id);
    let map = world.resource::<crate::resources::Map>().clone();

    // V3 の生産拡張 (対編成カウンター効率) を有効にするかどうか
    let is_v3 = world
        .get_resource::<crate::ai::ai_version::PlayerAiSettings>()
        .map(|s| s.get_version(player_id).uses_v3_tactics())
        .unwrap_or(false);

    let (unit_registry, damage_chart, master_data) = {
        let ur = world.get_resource::<UnitRegistry>().cloned();
        let dc = world.get_resource::<DamageChart>().cloned();
        let md = world.get_resource::<MasterDataRegistry>().cloned();
        if ur.is_none() || dc.is_none() || md.is_none() {
            return commands;
        }
        (ur.unwrap(), dc.unwrap(), md.unwrap())
    };

    let current_funds = if let Some(players) = world.get_resource::<Players>() {
        players
            .0
            .iter()
            .find(|p| p.id == player_id)
            .map(|p| p.funds)
            .unwrap_or(0)
    } else {
        return commands;
    };

    // --- 0. 施設・ユニット・首都のスキャン ---
    let mut occupied_positions = std::collections::HashSet::new();
    let mut enemy_units = Vec::new();
    let mut my_units = Vec::new();
    let mut my_empty_transports = Vec::new();

    {
        let mut q_units = world.query::<(
            Entity,
            &GridPosition,
            &Faction,
            &UnitStats,
            Option<&crate::components::CargoCapacity>,
            Option<&crate::components::Transporting>,
        )>();
        for (_entity, pos, faction, stats, cargo_opt, transporting_opt) in q_units.iter(world) {
            if transporting_opt.is_some() {
                continue;
            }
            occupied_positions.insert(*pos);
            if faction.0 == player_id {
                my_units.push((*pos, stats.clone()));
                if let Some(cargo) = cargo_opt
                    && cargo.loaded.is_empty()
                    && stats.max_cargo > 0
                {
                    my_empty_transports.push((*pos, stats.clone()));
                }
            } else {
                enemy_units.push((*pos, stats.clone()));
            }
        }
    }

    let mut capital_pos = None;
    let mut my_facilities = Vec::new();
    let mut producible_types = std::collections::HashSet::new();

    // 生産範囲判定に使うマップのトポロジー（スクエア/ヘックス）
    let topology = world
        .get_resource::<crate::resources::Map>()
        .map(|m| m.topology)
        .unwrap_or(crate::resources::GridTopology::Square);

    {
        let mut q_props = world.query::<(&GridPosition, &Property)>();
        // まず首都を探す
        for (pos, prop) in q_props.iter(world) {
            if prop.owner_id == Some(player_id) && prop.terrain == Terrain::Capital {
                capital_pos = Some(*pos);
                break;
            }
        }

        // 生産施設を収集し、生産可能なユニットタイプを特定
        for (pos, prop) in q_props.iter(world) {
            if prop.owner_id == Some(player_id)
                && master_data.is_production_facility(prop.terrain.as_str())
                && !occupied_positions.contains(pos)
            {
                // 首都から3マス以内（PRODUCTION_RANGE）の施設のみを有効とする
                let capital_positions = capital_pos.as_slice();
                if crate::systems::production::is_within_production_range(
                    capital_positions,
                    pos.x,
                    pos.y,
                    topology,
                ) {
                    my_facilities.push((*pos, prop.terrain));
                    // この施設で生産可能なユニットタイプを記録
                    for ut in unit_registry.0.keys() {
                        if master_data.can_produce_unit(prop.terrain.as_str(), *ut) {
                            producible_types.insert(*ut);
                        }
                    }
                }
            }
        }
    }

    if my_facilities.is_empty() {
        return commands;
    }

    // --- 1. 資金計画の更新 ---
    let mut reserves = 0;

    // ProductionPlanリソースの取得または作成
    if world.get_resource::<ProductionPlan>().is_none() {
        world.insert_resource(ProductionPlan::default());
    }

    let mut plan = world.get_resource_mut::<ProductionPlan>().unwrap();
    if strategy.phase == GamePhase::Defense || (is_v3 && !strategy.campaign_shortfalls.is_empty()) {
        // campaign allocatorが完全package資金を予約済みのため、generic貯金で二重に差し引かない。
        plan.reserves.insert(player_id.0, 0);
    } else {
        reserves = *plan.reserves.get(&player_id.0).unwrap_or(&0);

        // 欲しいユニット（一番スコアが高いもの）が買えない場合、貯金を検討
        // ただし、現在持っている施設で生産可能なものに限定する
        let mut saving_candidates = Vec::new();

        for (ut, stats) in &unit_registry.0 {
            if !producible_types.contains(ut) {
                continue;
            }
            let Some((facility_position, facility_terrain)) = my_facilities
                .iter()
                .filter(|(_, terrain)| master_data.can_produce_unit(terrain.as_str(), *ut))
                .min_by_key(|(position, _)| (position.y, position.x))
            else {
                continue;
            };

            let current_ratio = if !my_units.is_empty() {
                my_units.iter().filter(|(_, s)| s.unit_type == *ut).count() as f32
                    / my_units.len() as f32
            } else {
                0.0
            };
            let ratio_diff =
                strategy.ideal_composition.get(ut).copied().unwrap_or(0.0) - current_ratio;

            let score = calculate_unit_score_at(
                *ut,
                stats,
                *facility_position,
                &strategy,
                &enemy_units,
                &my_empty_transports,
                &damage_chart,
                &master_data,
                &map,
                &unit_registry,
                *facility_terrain,
                ratio_diff,
                is_v3,
            );
            saving_candidates.push(ProductionCandidate {
                score,
                facility_position: *facility_position,
                unit_type: *ut,
                cost: stats.cost,
                max_cargo: stats.max_cargo,
                can_capture: stats.can_capture,
            });
        }

        let best_unit = select_best_production_candidate(&saving_candidates).copied();
        if let Some(candidate) = best_unit
            && candidate.cost > current_funds
            && candidate.cost > reserves
        {
            plan.reserves.insert(player_id.0, candidate.cost);
            plan.reservations
                .entry(player_id.0)
                .or_default()
                .push(candidate.unit_type);
            reserves = candidate.cost;
        } else if let Some(candidate) = best_unit
            && candidate.cost <= current_funds
        {
            // 買えるユニットがベストなら、貯金目標をリセット（または達成済みとする）
            if reserves > 0 && current_funds >= reserves {
                plan.reserves.insert(player_id.0, 0);
                reserves = 0;
            }
        }
    }

    // --- 2. 実行予算の算出 ---
    let available_funds = if strategy.phase == GamePhase::Defense {
        current_funds
    } else {
        // 貯金目標がある場合、貯金目標の達成を確実にするため、バッファを含めて予算を制限する
        let reserve_cut = if reserves > 0 { reserves / 2 + 1000 } else { 0 };
        let mut budget = current_funds.saturating_sub(reserve_cut);

        // ユニット数が極端に少ない(5体未満)場合は、即座の占領・戦力拡張を最優先するため全額を実行予算とする。
        // そうではなく、予算が歩兵コスト(1000G)を下回っているだけであれば、貯金を妥協しつつも歩兵1体分(1000G)程度に予算を抑える。
        if my_units.len() < 5 {
            budget = current_funds;
        } else if budget < 1000 {
            budget = 1000.min(current_funds);
        }
        budget
    };

    let available_types: Vec<(UnitType, UnitStats)> = unit_registry
        .0
        .iter()
        .map(|(ut, s)| (*ut, s.clone()))
        .collect();

    // --- 3. V3 campaign予約行をpriority rank・島ID順に先行消費 ---
    let mut campaign_outcome = CampaignProductionOutcome {
        commands: Vec::new(),
        remaining_funds: available_funds,
        used_facilities: std::collections::HashSet::new(),
        completed_all_rows: true,
    };
    if is_v3 && !strategy.campaign_shortfalls.is_empty() {
        let campaign_plan_exists = world
            .get_resource::<crate::ai::engine::AiTurnStrategyCache>()
            .is_some_and(|cache| cache.campaign_production_planned(player_id));
        if campaign_plan_exists {
            // 最初の完全package計画を1commandずつ消費し、同じshortfallの重複生産を防ぐ。
            let mut cache = world
                .remove_resource::<crate::ai::engine::AiTurnStrategyCache>()
                .unwrap_or_default();
            let next_command = cache.take_campaign_production_command(player_id);
            let blocks_generic = cache.campaign_production_blocks_generic(player_id);
            world.insert_resource(cache);
            if let Some(command) = next_command {
                return vec![command];
            }
            if blocks_generic {
                return commands;
            }
        } else {
            campaign_outcome = plan_campaign_shortfall_production(
                player_id,
                &strategy.campaign_shortfalls,
                &my_facilities,
                &available_types,
                &master_data,
                available_funds,
            );
            let completed_all_rows = campaign_outcome.completed_all_rows;
            let campaign_commands = std::mem::take(&mut campaign_outcome.commands);
            let mut cache = world
                .remove_resource::<crate::ai::engine::AiTurnStrategyCache>()
                .unwrap_or_default();
            cache.set_campaign_production_plan(player_id, campaign_commands, completed_all_rows);
            let next_command = cache.take_campaign_production_command(player_id);
            let blocks_generic = cache.campaign_production_blocks_generic(player_id);
            world.insert_resource(cache);
            if let Some(command) = next_command {
                return vec![command];
            }
            if blocks_generic {
                return commands;
            }
        }
    }

    // --- 4. campaign完了後だけgeneric需要を予算と施設重複込みで評価 ---
    let mut remaining_funds = campaign_outcome.remaining_funds;
    let mut current_strategy = strategy.clone();
    if is_v3 && !current_strategy.campaign_shortfalls.is_empty() {
        current_strategy.capture_demand = 0;
        current_strategy.light_transport_demand = 0;
        current_strategy.heavy_transport_demand = 0;
    }
    let mut used_facilities = campaign_outcome.used_facilities;

    loop {
        let mut production_candidates = Vec::new();

        for (facility_pos, terrain) in &my_facilities {
            if used_facilities.contains(facility_pos) {
                continue;
            }

            let terrain_name = terrain.as_str();
            for (ut, stats) in &available_types {
                if !master_data.can_produce_unit(terrain_name, *ut) {
                    continue;
                }
                if stats.cost > remaining_funds {
                    continue;
                }

                // 予算制限（remaining_funds）がすでに reserve_cut を差し引いているため、
                // この範囲内で買えるものであれば、戦闘ユニットであっても生産してよい。

                let current_ratio = if !my_units.is_empty() {
                    my_units.iter().filter(|(_, s)| s.unit_type == *ut).count() as f32
                        / my_units.len() as f32
                } else {
                    0.0
                };
                let ratio_diff = current_strategy
                    .ideal_composition
                    .get(ut)
                    .copied()
                    .unwrap_or(0.0)
                    - current_ratio;

                // 現在の戦略（減衰後）でスコアを計算
                let score = calculate_unit_score_at(
                    *ut,
                    stats,
                    *facility_pos,
                    &current_strategy,
                    &enemy_units,
                    &my_empty_transports,
                    &damage_chart,
                    &master_data,
                    &map,
                    &unit_registry,
                    *terrain,
                    ratio_diff,
                    is_v3,
                );

                production_candidates.push(ProductionCandidate {
                    score,
                    facility_position: *facility_pos,
                    unit_type: *ut,
                    cost: stats.cost,
                    max_cargo: stats.max_cargo,
                    can_capture: stats.can_capture,
                });
            }
        }

        if let Some(candidate) = select_best_production_candidate(&production_candidates).copied() {
            // 生産決定
            commands.push(ProduceUnitCommand {
                player_id,
                target_x: candidate.facility_position.x,
                target_y: candidate.facility_position.y,
                unit_type: candidate.unit_type,
            });
            remaining_funds = remaining_funds.saturating_sub(candidate.cost);
            used_facilities.insert(candidate.facility_position);

            // 需要を動的に減衰させる（次の候補評価に反映）。
            // V3では海を越えられる輸送種別だけがoffshore需要を消費する。
            consume_transport_demand_after_production(
                &mut current_strategy,
                candidate.unit_type,
                candidate.max_cargo,
                is_v3,
            );
            if candidate.can_capture {
                current_strategy.capture_demand = current_strategy.capture_demand.saturating_sub(1);
            }
        } else {
            // これ以上生産可能なものがないか、予算不足
            break;
        }
    }

    commands
}

/// #53/#55 (V3): 交戦成立率。攻撃側が防御側に対してどれだけ容易に射撃機会を
/// 得られるかを射程と機動力から近似する。
/// アウトレンジする側 (射程で上回る側) は撃ち逃げで一方的に攻撃でき、
/// アウトレンジされる側は接近中に削られて攻撃機会が減る。
fn engagement_factor(attacker: &UnitStats, defender: &UnitStats) -> f32 {
    let att_reach = attacker.max_movement + attacker.max_range;
    let def_reach = defender.max_movement + defender.max_range;
    if attacker.max_range > defender.max_range {
        // アウトレンジ可能: リーチでも上回るなら完全な撃ち逃げが成立する
        if att_reach >= def_reach { 1.0 } else { 0.8 }
    } else if attacker.max_range < defender.max_range {
        // アウトレンジされる側: 射程内に入るまでに一方的に削られる
        0.5
    } else {
        1.0
    }
}

/// #53/#55 (V3): 対編成カウンター効率スコア。
/// 候補ユニット U を1体生産した場合の、敵軍全体との「価値交換」の期待値を
/// ゴールド換算で見積もる。敵ユニット e ごとに
///   与える価値 = dmg(U→e) × cost_e × 交戦成立率(U,e)
///   受ける価値 = dmg(e→U) × cost_U × 交戦成立率(e,U)
/// の差を取り、敵軍の平均を返す。敵の主力構成に対して効率よく価値を刈り取れる
/// ユニット (例: ロケラン主体の敵にはそれをアウトレンジする自走砲) が高評価になる。
pub(crate) fn counter_efficiency_score(
    unit_stats: &UnitStats,
    enemy_units: &[(GridPosition, UnitStats)],
    damage_chart: &DamageChart,
) -> i32 {
    if enemy_units.is_empty() {
        return 0;
    }
    let mut total_net = 0i64;
    for (_, e_stats) in enemy_units {
        // 与える価値 (主武器・副武器の高い方)
        let dmg_out = damage_chart
            .get_base_damage(unit_stats.unit_type, e_stats.unit_type)
            .unwrap_or(0)
            .max(
                damage_chart
                    .get_base_damage_secondary(unit_stats.unit_type, e_stats.unit_type)
                    .unwrap_or(0),
            );
        // 受ける価値
        let dmg_in = damage_chart
            .get_base_damage(e_stats.unit_type, unit_stats.unit_type)
            .unwrap_or(0)
            .max(
                damage_chart
                    .get_base_damage_secondary(e_stats.unit_type, unit_stats.unit_type)
                    .unwrap_or(0),
            );
        let value_out =
            dmg_out as f32 * e_stats.cost as f32 / 100.0 * engagement_factor(unit_stats, e_stats);
        let value_in =
            dmg_in as f32 * unit_stats.cost as f32 / 100.0 * engagement_factor(e_stats, unit_stats);
        total_net += (value_out - value_in) as i64;
    }
    (total_net / enemy_units.len() as i64) as i32
}

/// 指定した地点で特定のユニットを生産した場合の期待スコアを算出します。
#[allow(clippy::too_many_arguments)]
pub fn calculate_unit_score_at(
    unit_type: UnitType,
    stats: &UnitStats,
    pos: GridPosition,
    strategy: &ProductionStrategy,
    enemy_units: &[(GridPosition, UnitStats)],
    my_empty_transports: &[(GridPosition, UnitStats)],
    damage_chart: &DamageChart,
    master_data: &MasterDataRegistry,
    map: &crate::resources::Map,
    _unit_registry: &UnitRegistry,
    produced_at: Terrain,
    ratio_diff: f32,
    // V3 のみ true。対編成カウンター効率スコアで生産を敵構成に適応させる
    is_v3: bool,
) -> u32 {
    // 1. 基本スコア（敵との距離、脅威度）
    let mut min_eta = 99;
    let mut score: u32 = if !strategy.priority_targets.is_empty() {
        let mut local_min_eta = 99;
        let mut base_val: i32 = 2000; // ベースを引き上げ

        for target in &strategy.priority_targets {
            // ターゲットが未占領（中立）拠点か判定
            let is_unowned_property = strategy.unowned_properties.contains(target);

            // 論理防衛評価: 占領できない戦闘ユニットは、中立拠点のETA評価を無視（スキップ）する
            if is_unowned_property && !stats.can_capture {
                continue;
            }

            let mut dist = (pos.x as isize - target.x as isize).unsigned_abs()
                + (pos.y as isize - target.y as isize).unsigned_abs();

            let mut reachable_target = false;
            // 海軍ユニットの対地評価補正
            if stats.movement_type == MovementType::Ship {
                if let Some(t_terrain) = map.get_terrain(target.x, target.y) {
                    let move_cost = master_data
                        .get_movement_cost(MovementType::Ship, t_terrain.as_str())
                        .unwrap_or(99);
                    if move_cost < 99 {
                        reachable_target = true;
                    }
                }

                // 隣接マスが海なら「沿岸」として到達可能とみなす
                if !reachable_target {
                    for adj in map.get_adjacent(target.x, target.y) {
                        if let Some(at) = map.get_terrain(adj.0, adj.1)
                            && master_data
                                .get_movement_cost(MovementType::Ship, at.as_str())
                                .unwrap_or(99)
                                < 99
                        {
                            reachable_target = true;
                            break;
                        }
                    }
                }

                if !reachable_target {
                    // 目標が直接到達不能な場合
                    if stats.max_range <= 1 {
                        // 直接攻撃ユニットは距離ペナルティ
                        dist += 20;
                        if stats.max_cargo == 0 {
                            // 輸送能力もないならベース値を大幅に下げる
                            base_val /= 4;
                        } else {
                            // 輸送能力がある場合は沿岸まで到達できれば良いのでペナルティを軽減
                            dist -= 15; // +20されたのを+5に緩和
                        }
                    } else {
                        // 間接攻撃ユニットは多少マシにする
                        dist += 10;
                    }
                }
            }

            // 地形コストを考慮したETAの簡易見積もり
            let base_terrain = if stats.movement_type == MovementType::Ship {
                Terrain::Sea.as_str()
            } else {
                Terrain::Plains.as_str()
            };
            let move_cost = master_data
                .get_movement_cost(stats.movement_type, base_terrain)
                .unwrap_or(1);
            let mut eta =
                (dist as u32 * move_cost + stats.max_movement - 1) / stats.max_movement.max(1);

            // 7.1 フォワードETA評価: 工場に空の輸送車がいる場合、輸送車を利用したETAを算出
            for (t_pos, t_stats) in my_empty_transports {
                if t_pos.x == pos.x && t_pos.y == pos.y {
                    // 輸送車がそのユニットを搭載可能かチェック
                    if t_stats.loadable_unit_types.contains(&stats.unit_type) {
                        let t_move_cost = master_data
                            .get_movement_cost(t_stats.movement_type, Terrain::Plains.as_str())
                            .unwrap_or(1);
                        let assisted_eta = (dist as u32 * t_move_cost + t_stats.max_movement - 1)
                            / t_stats.max_movement.max(1);

                        if assisted_eta < eta {
                            eta = assisted_eta;
                        }
                    }
                }
            }

            // 船の場合、ターゲットが沿岸ならETAをさらに好意的に評価（海路は速いため）
            let mut final_eta = eta;
            if stats.movement_type == MovementType::Ship && reachable_target {
                final_eta = final_eta.saturating_sub(2).max(1);
            }

            if final_eta < local_min_eta {
                local_min_eta = final_eta;
            }
        }
        min_eta = local_min_eta;

        // 1ターン遅れるごとに40点のペナルティ（緩和）
        let eta_penalty = min_eta * 40;
        base_val.saturating_sub(eta_penalty as i32).max(1) as u32
    } else {
        // 敵がいない場合は均一
        100
    };

    // 2. 特殊役割ボーナス
    if stats.can_capture {
        // 不足している占領可能ユニット数（capture_demand）に応じて線形に価値を高める
        if strategy.capture_demand > 0 {
            score += 2500 * strategy.capture_demand; // 不足数が多い（特に収入危機時）ほど超強力に歩兵を優先
        } else if strategy.phase == GamePhase::Expansion {
            score = score.saturating_sub(1000);
        } else {
            score = score.saturating_sub(2000);
        }

        // 近く（ETA=1〜2）に未占領拠点がある場合、収入確保の近接占領ボーナスを付与
        if strategy.capture_demand > 0 && min_eta <= 2 {
            score += 2000;
        }
    }
    // 輸送ユニットの評価（期待状態価値の向上分に基づく）
    if stats.max_cargo > 0 && !strategy.transport_candidates.is_empty() {
        let transport_targets = if is_v3 {
            strategy.campaign_portfolio.offensive_target_positions()
        } else {
            strategy.priority_targets.clone()
        };
        let mut transport_utility: f32 = 0.0;
        for (c_pos, c_stats, c_value) in &strategy.transport_candidates {
            // この輸送ユニットが搭載可能かチェック
            if stats.loadable_unit_types.contains(&c_stats.unit_type) {
                // 候補ユニットにとっての最寄りのターゲットを特定
                let mut min_dist_to_target = 999;
                let mut best_target = GridPosition { x: 0, y: 0 };
                for target in &transport_targets {
                    let d = (c_pos.x as i32 - target.x as i32).abs()
                        + (c_pos.y as i32 - target.y as i32).abs();
                    if d < min_dist_to_target {
                        min_dist_to_target = d;
                        best_target = *target;
                    }
                }

                // 自力ETAの見積もり（海越えなら大きなペナルティ）
                let mut is_blocked = false;
                let steps = 4;
                for i in 1..steps {
                    let cx = c_pos.x as i32 + (best_target.x as i32 - c_pos.x as i32) * i / steps;
                    let cy = c_pos.y as i32 + (best_target.y as i32 - c_pos.y as i32) * i / steps;
                    if let Some(Terrain::Sea | Terrain::Shoal) =
                        map.get_terrain(cx as usize, cy as usize)
                    {
                        is_blocked = true;
                        break;
                    }
                }

                let self_eta = if is_blocked {
                    20.0
                } else {
                    (min_dist_to_target as f32) / (c_stats.max_movement as f32).max(1.0)
                };

                // 輸送時のETA（生産地点からターゲットまでの輸送ユニットの移動時間）
                let dist_to_target = (pos.x as i32 - best_target.x as i32).abs()
                    + (pos.y as i32 - best_target.y as i32).abs();
                let transport_eta = (dist_to_target as f32) / (stats.max_movement as f32).max(1.0);

                // 短縮効果 (ETA Gain)
                let eta_gain = (self_eta - transport_eta).max(0.0);

                // ユーティリティ = ユニット価値 * 短縮ターン数
                transport_utility += c_value * eta_gain;
            }
        }

        // スコアへの統合（既存スコア体系とバランスを取るために係数 0.15 を適用）
        // 保有輸送ユニット数に応じた減衰 (1台増えるごとに評価を段階的に下げる)
        let attenuation = 1.0 / (1.0 + strategy.existing_transport_count as f32);
        score += (transport_utility * 0.15 * attenuation) as u32;

        // 2.5. Lander侵攻価値スコア (Invasion Value)
        let mut invasion_value = 0.0;
        for target in &transport_targets {
            let mut is_blocked = false;
            let steps = 4;
            for i in 1..steps {
                let cx = pos.x as i32 + (target.x as i32 - pos.x as i32) * i / steps;
                let cy = pos.y as i32 + (target.y as i32 - pos.y as i32) * i / steps;
                if let Some(Terrain::Sea | Terrain::Shoal) =
                    map.get_terrain(cx as usize, cy as usize)
                {
                    is_blocked = true;
                    break;
                }
            }

            if is_blocked {
                let property_value = if let Some(t_terrain) = map.get_terrain(target.x, target.y) {
                    match t_terrain {
                        Terrain::Capital => 5000,
                        Terrain::Factory => 3000,
                        Terrain::Port | Terrain::Airport => 2000,
                        _ => 1000,
                    }
                } else {
                    1000
                };

                let cargo_value = strategy
                    .transport_candidates
                    .iter()
                    .filter(|(_, c_stats, _)| {
                        stats.loadable_unit_types.contains(&c_stats.unit_type)
                    })
                    .map(|(_, _, val)| *val)
                    .fold(f32::MIN, |a, b| a.max(b));

                if cargo_value > f32::MIN {
                    let dist_to_target = (pos.x as i32 - target.x as i32).abs()
                        + (pos.y as i32 - target.y as i32).abs();
                    let transport_eta =
                        (dist_to_target as f32) / (stats.max_movement as f32).max(1.0);
                    invasion_value +=
                        (property_value as f32) * cargo_value / transport_eta.max(1.0);
                }
            }
        }
        let attenuation_inv = 1.0 / (1.0 + strategy.existing_transport_count as f32);
        score += (invasion_value * attenuation_inv * 0.002) as u32;

        // 輸送需要がない場合は減衰（既存ロジックの維持）
        let can_load_heavy = stats.loadable_unit_types.contains(&UnitType::Tank);
        let can_load_light = stats.loadable_unit_types.contains(&UnitType::Infantry);

        let demand = if can_load_heavy && can_load_light {
            strategy
                .heavy_transport_demand
                .max(strategy.light_transport_demand)
        } else if can_load_heavy {
            strategy.heavy_transport_demand
        } else {
            strategy.light_transport_demand
        };

        if demand == 0 {
            score = score.saturating_sub(3000);
        } else {
            // 基本的な需要ボーナス（過剰な固定加点ではなく、主役は transport_utility に任せる）
            score += demand * 1500;
        }

        // 輸送ユニットを持ちすぎている場合は強力なペナルティを課す
        if strategy.existing_transport_count >= 1 {
            score = (score as f32 * 0.5) as u32; // 2台目以降は半減
        }
        if strategy.existing_transport_count >= 2 {
            score = score.saturating_sub(2000); // 3台目以降はさらに減点
        }
    }

    // 港での艦船ボーナス
    if produced_at == Terrain::Port && stats.movement_type == MovementType::Ship {
        score += 3000; // 港なら船を作りたい（加点を倍増）
        if stats.max_range > 1 {
            score += 2000; // 戦艦などはさらに高評価
        }
    }

    // 3. アンチ性能ボーナス
    if is_v3 {
        // #53/#55 (V3): 対編成カウンター効率。敵軍の実構成に対する価値交換の
        // 期待値 (射程・機動の相性込み) で生産を適応させる。
        // 敵がロケラン主体ならそれをアウトレンジする自走砲、航空主体なら対空、
        // のように敵の主力へのカウンターが自動的に浮上する
        let counter = counter_efficiency_score(stats, enemy_units, damage_chart);
        let mut scaled = (counter * 3).clamp(-4000, 8000);
        // 拡張期 (未交戦) はカウンター生産よりも経済 (歩兵・輸送) を優先する。
        // 敵が別の島にいて届かない段階でカウンターユニットを量産しても
        // 価値を発揮できず、拡張と輸送の予算を食い潰すだけになるため
        if strategy.phase == GamePhase::Expansion {
            scaled /= 4;
        }
        score = score.saturating_add_signed(scaled);
    } else {
        // V2: 敵の主力ユニットに対して有利なユニットを頭数で加点する従来方式
        for (_, enemy_stats) in enemy_units {
            // 武器1での相性
            if let Some(damage) = damage_chart.get_base_damage(unit_type, enemy_stats.unit_type) {
                if damage >= 50 {
                    score += 500;
                }
                if damage >= 80 {
                    score += 1000;
                }
            }
            // 武器2での相性
            if damage_chart
                .get_base_damage_secondary(unit_type, enemy_stats.unit_type)
                .is_some_and(|damage| damage >= 30)
            {
                score += 300;
            }
        }
    }

    // 3.5. 拠点競争阻止ボーナス (Interception Score)
    for target in &strategy.priority_targets {
        if strategy.unowned_properties.contains(target) {
            for (e_pos, e_stats) in enemy_units {
                if !e_stats.can_capture {
                    continue;
                }
                let enemy_dist = (e_pos.x as isize - target.x as isize).unsigned_abs()
                    + (e_pos.y as isize - target.y as isize).unsigned_abs();
                let enemy_eta =
                    (enemy_dist as u32 + e_stats.max_movement - 1) / e_stats.max_movement.max(1);

                let my_dist = (pos.x as isize - target.x as isize).unsigned_abs()
                    + (pos.y as isize - target.y as isize).unsigned_abs();
                let my_eta = (my_dist as u32 + stats.max_movement - 1) / stats.max_movement.max(1);

                if enemy_eta <= my_eta {
                    let property_value =
                        if let Some(t_terrain) = map.get_terrain(target.x, target.y) {
                            match t_terrain {
                                Terrain::Capital => 5000,
                                Terrain::Factory => 3000,
                                Terrain::Port | Terrain::Airport => 2000,
                                _ => 1000,
                            }
                        } else {
                            1000
                        };

                    let damage_vs_enemy = damage_chart
                        .get_base_damage(unit_type, e_stats.unit_type)
                        .unwrap_or(0);

                    if damage_vs_enemy > 0 {
                        let interception_score =
                            (property_value * damage_vs_enemy) / (my_eta.max(1) * 10);
                        score += interception_score;
                    }
                }
            }
        }
    }

    // 4. 戦略フェーズボーナス
    match strategy.phase {
        GamePhase::Expansion => {
            if stats.max_movement >= 6 {
                score += 500;
            }
        }
        GamePhase::Assault | GamePhase::Contested => {
            if stats.unit_type == UnitType::Tank
                || stats.unit_type == UnitType::MdTank
                || stats.unit_type == UnitType::TankZ
            {
                score += 1000;
            }
        }
        GamePhase::Defense => {
            // 防衛時は間接攻撃や安価な壁ユニットを評価
            if stats.min_range > 1 {
                score += 1500;
            }
            if stats.cost <= 3000 {
                score += 500;
            }
        }
    }

    // 5. 削除済（かつてのDemandMatrixによる加算ブロックがあった場所）

    // 6. コストに応じたボーナスを追加して強力なユニットを作りやすくする
    if !stats.can_capture && stats.max_cargo == 0 && !stats.can_supply {
        score += stats.cost / 10;
    }

    // --- 6. 敵の脅威がない平和な時の戦闘ユニット生産ロックを無効化 ---
    // V2は戦略的に前線を押し上げるため、平和な時期でも戦闘ユニットを生産して前線へ送る。
    // (scoreのゼロ化は行わない)

    // 7. 理想構成（ideal_composition）の適用
    let mut final_score = score as i32;
    if ratio_diff > 0.0 {
        // 例: 30%足りないなら 0.3 * 4000 = 1200 のボーナス
        final_score += (ratio_diff * 4000.0) as i32;
    } else if ratio_diff < -0.1 {
        // 例: 10%以上過剰ならペナルティ
        // ただし、序盤(Expansion)で占領需要が高い時は歩兵などへのペナルティを無効化する
        if strategy.phase == GamePhase::Expansion && stats.can_capture {
            // ペナルティなし
        } else {
            final_score -= 1000;
        }
    }

    final_score.max(1) as u32
}

#[cfg(test)]
mod additional_tests {
    use super::*;
    use crate::ai::strategy;
    use crate::components::Health;
    use crate::resources::{Map, Terrain};

    fn campaign_test_types(master_data: &MasterDataRegistry) -> Vec<(UnitType, UnitStats)> {
        master_data
            .unit_order
            .iter()
            .filter_map(|name| master_data.create_unit_stats(name).ok())
            .map(|stats| (stats.unit_type, stats))
            .collect()
    }

    fn selection_candidate(
        score: u32,
        position: GridPosition,
        unit_type: UnitType,
        cost: u32,
    ) -> ProductionCandidate {
        ProductionCandidate {
            score,
            facility_position: position,
            unit_type,
            cost,
            max_cargo: 0,
            can_capture: unit_type == UnitType::Infantry,
        }
    }

    #[test]
    fn equal_score_selection_is_insertion_order_independent() {
        let candidates = vec![
            selection_candidate(100, GridPosition { x: 1, y: 0 }, UnitType::Infantry, 1_000),
            selection_candidate(100, GridPosition { x: 0, y: 0 }, UnitType::Mech, 3_000),
            selection_candidate(100, GridPosition { x: 0, y: 0 }, UnitType::Infantry, 1_000),
        ];
        let mut reversed = candidates.clone();
        reversed.reverse();

        let selected = select_best_production_candidate(&candidates).unwrap();
        let reversed_selected = select_best_production_candidate(&reversed).unwrap();

        assert_eq!(selected.facility_position, GridPosition { x: 0, y: 0 });
        assert_eq!(selected.unit_type, UnitType::Infantry);
        assert_eq!(
            (
                reversed_selected.facility_position,
                reversed_selected.unit_type
            ),
            (selected.facility_position, selected.unit_type)
        );
    }

    #[test]
    fn equal_score_selection_uses_cost_after_type_rank() {
        let candidates = vec![
            selection_candidate(100, GridPosition { x: 0, y: 0 }, UnitType::Infantry, 1_001),
            selection_candidate(100, GridPosition { x: 0, y: 0 }, UnitType::Infantry, 1_000),
        ];

        assert_eq!(
            select_best_production_candidate(&candidates).unwrap().cost,
            1_000
        );
    }

    #[test]
    fn v3_transport_demand_consumption_uses_offshore_types_only() {
        let master_data = MasterDataRegistry::load().unwrap();
        let stats = |unit_type: UnitType| {
            master_data
                .create_unit_stats(&crate::resources::master_data::UnitName(
                    unit_type.as_str().to_owned(),
                ))
                .unwrap()
        };

        let mut recon_strategy = ProductionStrategy {
            light_transport_demand: 2,
            heavy_transport_demand: 1,
            ..ProductionStrategy::default()
        };
        let recon = stats(UnitType::Recon);
        consume_transport_demand_after_production(
            &mut recon_strategy,
            UnitType::Recon,
            recon.max_cargo,
            true,
        );
        assert_eq!(recon_strategy.light_transport_demand, 2);
        assert_eq!(recon_strategy.heavy_transport_demand, 1);

        let mut helicopter_strategy = recon_strategy.clone();
        let helicopter = stats(UnitType::TransportHelicopter);
        consume_transport_demand_after_production(
            &mut helicopter_strategy,
            UnitType::TransportHelicopter,
            helicopter.max_cargo,
            true,
        );
        assert_eq!(helicopter_strategy.light_transport_demand, 0);
        assert_eq!(helicopter_strategy.heavy_transport_demand, 1);

        let mut heavy_lander_strategy = recon_strategy.clone();
        let lander = stats(UnitType::Lander);
        consume_transport_demand_after_production(
            &mut heavy_lander_strategy,
            UnitType::Lander,
            lander.max_cargo,
            true,
        );
        assert_eq!(heavy_lander_strategy.light_transport_demand, 2);
        assert_eq!(heavy_lander_strategy.heavy_transport_demand, 0);

        let mut light_lander_strategy = ProductionStrategy {
            light_transport_demand: 2,
            heavy_transport_demand: 0,
            ..ProductionStrategy::default()
        };
        consume_transport_demand_after_production(
            &mut light_lander_strategy,
            UnitType::Lander,
            lander.max_cargo,
            true,
        );
        assert_eq!(light_lander_strategy.light_transport_demand, 0);
    }

    #[test]
    fn v1_ground_carrier_keeps_legacy_transport_demand_consumption() {
        let master_data = MasterDataRegistry::load().unwrap();
        let recon = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Recon.as_str().to_owned(),
            ))
            .unwrap();
        let mut strategy = ProductionStrategy {
            light_transport_demand: 2,
            ..ProductionStrategy::default()
        };

        consume_transport_demand_after_production(
            &mut strategy,
            UnitType::Recon,
            recon.max_cargo,
            false,
        );

        assert_eq!(strategy.light_transport_demand, 1);
    }

    #[test]
    fn campaign_production_services_higher_priority_row_before_lower_rows() {
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let rows = vec![
            crate::ai::island_campaign::IslandCampaignShortfall {
                island_id: crate::ai::islands::IslandId(0),
                decision: crate::ai::island_campaign::IslandCampaignDecision::Defend,
                light_transport_slots: 0,
                heavy_transport_slots: 0,
                capture_units: 0,
                combat_budget: 7_000,
                reserved_budget: 7_000,
                priority_rank: 0,
            },
            crate::ai::island_campaign::IslandCampaignShortfall {
                island_id: crate::ai::islands::IslandId(1),
                decision: crate::ai::island_campaign::IslandCampaignDecision::Expand,
                light_transport_slots: 2,
                heavy_transport_slots: 0,
                capture_units: 2,
                combat_budget: 0,
                reserved_budget: 6_000,
                priority_rank: 2,
            },
            crate::ai::island_campaign::IslandCampaignShortfall {
                island_id: crate::ai::islands::IslandId(2),
                decision: crate::ai::island_campaign::IslandCampaignDecision::Assault,
                light_transport_slots: 2,
                heavy_transport_slots: 2,
                capture_units: 2,
                combat_budget: 10_200,
                reserved_budget: 32_700,
                priority_rank: 4,
            },
        ];
        let facilities = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Factory),
            (GridPosition { x: 1, y: 0 }, Terrain::Airport),
        ];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &facilities,
            &available_types,
            &master_data,
            7_000,
        );

        assert_eq!(outcome.commands.len(), 1);
        let produced_stats = available_types
            .iter()
            .find(|(unit_type, _)| *unit_type == outcome.commands[0].unit_type)
            .map(|(_, stats)| stats)
            .unwrap();
        assert!(produced_stats.cost <= 7_000);
        assert!(!produced_stats.can_capture);
        assert!(!matches!(
            outcome.commands[0].unit_type,
            UnitType::Lander | UnitType::TransportHelicopter | UnitType::SupplyTruck
        ));
        assert!(!outcome.completed_all_rows);
    }

    #[test]
    fn campaign_combat_remainder_uses_reserved_real_unit_cost() {
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let facilities = vec![(GridPosition { x: 0, y: 0 }, Terrain::Factory)];
        let row =
            |combat_budget, reserved_budget| crate::ai::island_campaign::IslandCampaignShortfall {
                island_id: crate::ai::islands::IslandId(0),
                decision: crate::ai::island_campaign::IslandCampaignDecision::Defend,
                light_transport_slots: 0,
                heavy_transport_slots: 0,
                capture_units: 0,
                combat_budget,
                reserved_budget,
                priority_rank: 0,
            };

        let small_remainder = plan_campaign_shortfall_production(
            PlayerId(1),
            &[row(80, 1_000)],
            &facilities,
            &available_types,
            &master_data,
            1_000,
        );
        let large = plan_campaign_shortfall_production(
            PlayerId(1),
            &[row(30_000, 30_000)],
            &facilities,
            &available_types,
            &master_data,
            30_000,
        );

        assert_eq!(small_remainder.commands.len(), 1);
        assert!(small_remainder.completed_all_rows);
        assert_eq!(large.commands.len(), 1);
        assert!(!large.completed_all_rows);
    }

    #[test]
    fn campaign_assault_production_requires_lander_and_helicopter() {
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let rows = vec![crate::ai::island_campaign::IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: crate::ai::island_campaign::IslandCampaignDecision::Assault,
            light_transport_slots: 2,
            heavy_transport_slots: 2,
            capture_units: 0,
            combat_budget: 0,
            reserved_budget: 20_500,
            priority_rank: 4,
        }];
        let facilities = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Port),
            (GridPosition { x: 1, y: 0 }, Terrain::Airport),
        ];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &facilities,
            &available_types,
            &master_data,
            20_500,
        );
        let produced: std::collections::HashSet<_> = outcome
            .commands
            .iter()
            .map(|command| command.unit_type)
            .collect();

        assert_eq!(
            produced,
            std::collections::HashSet::from([UnitType::Lander, UnitType::TransportHelicopter,])
        );
        assert!(outcome.completed_all_rows);
    }

    /// #53/#55 (V3): 対編成カウンター効率スコアの検証。
    /// ロケラン主体の敵編成に対して、それをアウトレンジできる重自走砲が
    /// ロケラン同型や歩兵より高評価になること
    #[test]
    fn test_counter_efficiency_vs_rocket_army() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let registry = world.get_resource::<UnitRegistry>().unwrap().clone();
        let chart = world.get_resource::<DamageChart>().unwrap().clone();

        // 敵編成: ロケットランチャー主体 (V2 の典型的なスパム構成)
        let rockets_stats = registry.0.get(&UnitType::Rockets).unwrap().clone();
        let enemy_army: Vec<(GridPosition, UnitStats)> = (0..10)
            .map(|i| (GridPosition { x: i, y: 0 }, rockets_stats.clone()))
            .collect();

        let heavy_sp_gun = registry.0.get(&UnitType::HeavySpGun).unwrap();
        let infantry = registry.0.get(&UnitType::Infantry).unwrap();

        let sp_gun_score = counter_efficiency_score(heavy_sp_gun, &enemy_army, &chart);
        let rockets_score = counter_efficiency_score(&rockets_stats, &enemy_army, &chart);
        let infantry_score = counter_efficiency_score(infantry, &enemy_army, &chart);

        // 重自走砲 (射程3-5) はロケラン (射程2-3) をアウトレンジして一方的に叩ける
        assert!(
            sp_gun_score > 0,
            "重自走砲はロケラン軍への正の交換価値を持つはず (actual: {})",
            sp_gun_score
        );
        assert!(
            sp_gun_score > rockets_score,
            "重自走砲はロケラン同型生産より高評価のはず (sp_gun: {}, rockets: {})",
            sp_gun_score,
            rockets_score
        );
        assert!(
            sp_gun_score > infantry_score,
            "重自走砲は歩兵より高評価のはず (sp_gun: {}, infantry: {})",
            sp_gun_score,
            infantry_score
        );
        // 歩兵はロケランに一方的に虐殺される (87ダメージ) ため負の交換価値
        assert!(
            infantry_score < 0,
            "歩兵はロケラン軍に対して負の交換価値のはず (actual: {})",
            infantry_score
        );
    }

    #[test]
    fn test_ai_production_saving_for_mdtank() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let p1 = PlayerId(1);
        let mut plan = ProductionPlan::default();
        plan.reserves.insert(p1.0, 16000); // MdTank目標
        world.insert_resource(plan);

        if let Some(mut players) = world.get_resource_mut::<Players>() {
            for p in &mut players.0 {
                if p.id == p1 {
                    p.funds = 10000; // MdTank(16000G)やMissiles(12000G)に足りない金額
                }
            }
        }

        // ユニット統計情報を取得
        let unit_registry = world.get_resource::<UnitRegistry>().unwrap().clone();

        // 状況設定: 敵が遠くにいて、強力なユニットが欲しい状態
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }
        // 施設をセットアップ
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 100),
        ));

        // 自軍ユニットを数体配置（ユニット数が少ないと貯金より生産を優先するため）        // 10体の歩兵を配置して、my_units.len() < 5 の緊急戦力拡張発動を確実に防ぐ
        for i in 0..10 {
            world.spawn((
                GridPosition {
                    x: i % 5,
                    y: i / 5 + 1,
                },
                Faction(p1),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ));
        }

        // 敵の「中戦車(MdTank)」を配置（十分に遠ざけてDefenseフェーズを避ける）
        world.spawn((
            GridPosition { x: 14, y: 14 },
            Faction(PlayerId(2)),
            UnitStats {
                unit_type: UnitType::MdTank,
                cost: 16000,
                max_movement: 5,
                movement_type: MovementType::Tank,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        // 実行（上で追加した ProductionPlan を活かすため、ここでリセットしない）
        let commands = decide_production(&mut world, p1);

        let plan = world.get_resource::<ProductionPlan>().unwrap();
        let reserve = *plan.reserves.get(&p1.0).unwrap_or(&0);

        // 10000Gでは買えないユニット（MissilesやMdTankなど）を目標に貯金しているはず
        assert!(
            reserve >= 12000,
            "Reserve should be at least 12000. Got: {}",
            reserve
        );
        // 資金(12000) < 貯金目標(16000) なので、高価な純戦闘ユニット（戦車等）は控えるはず
        for cmd in &commands {
            let stats = unit_registry.get_stats(cmd.unit_type).unwrap();
            assert!(
                stats.cost <= 3000 || stats.max_cargo > 0,
                "Should only produce cheap units (<= 3000) or transport units while saving. Got: {:?}",
                cmd.unit_type
            );
        }
    }

    #[test]
    fn test_ai_production_forward_eta() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let p1 = PlayerId(1);

        // 1. 全ユニットをクリア
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }

        // 2. 工場と首都を設置
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        let factory_pos = GridPosition { x: 1, y: 0 };
        world.spawn((factory_pos, Property::new(Terrain::Factory, Some(p1), 100)));

        // 3. 遠くに敵拠点を設置（距離感を作る）
        let enemy_pos = GridPosition { x: 15, y: 0 };
        world.spawn((
            enemy_pos,
            Property::new(Terrain::City, Some(PlayerId(2)), 100),
        ));

        // 敵ユニットも設置
        let enemy_stats = UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1000,
            max_movement: 3,
            movement_type: MovementType::Tank,
            ..UnitStats::mock()
        };
        world.spawn((
            enemy_pos,
            Faction(PlayerId(2)),
            enemy_stats.clone(),
            Health {
                current: 100,
                max: 100,
            },
        ));

        let registry = world.get_resource::<UnitRegistry>().unwrap().clone();
        let chart = world.get_resource::<DamageChart>().unwrap().clone();
        let map = world.get_resource::<Map>().unwrap().clone();

        // テスト用の低速タンク（speed 3）
        let tank_stats = UnitStats {
            unit_type: UnitType::Tank,
            max_movement: 3,
            movement_type: MovementType::Tank,
            ..UnitStats::mock()
        };

        let enemy_units = vec![(enemy_pos, enemy_stats)];

        // シナリオA: 輸送車なしでタンクのスコアを計測
        let score_without_transport;
        {
            let strategy = strategy::analyze_strategy(&mut world, p1);
            score_without_transport = calculate_unit_score_at(
                UnitType::Tank,
                &tank_stats,
                factory_pos,
                &strategy,
                &enemy_units,
                &[],
                &chart,
                &master_data,
                &map,
                &registry,
                Terrain::Factory,
                0.0,
                false,
            );
        }

        // シナリオB: 工場に空の輸送車(輸送ヘリ)を設置してスコアを再計算
        let score_with_transport;
        {
            // 高速な輸送車（speed 9）
            let t_stats = UnitStats {
                unit_type: UnitType::TransportHelicopter,
                max_movement: 9,
                movement_type: MovementType::Air,
                max_cargo: 1,
                loadable_unit_types: vec![UnitType::Infantry, UnitType::Tank],
                ..UnitStats::mock()
            };
            let empty_transports = vec![(factory_pos, t_stats)];

            let strategy = strategy::analyze_strategy(&mut world, p1);
            score_with_transport = calculate_unit_score_at(
                UnitType::Tank,
                &tank_stats,
                factory_pos,
                &strategy,
                &enemy_units,
                &empty_transports,
                &chart,
                &master_data,
                &map,
                &registry,
                Terrain::Factory,
                0.0,
                false,
            );
        }

        // 検証: 輸送車がある方がETAが短縮され、スコアが高くなるはず
        assert!(
            score_with_transport > score_without_transport,
            "Score with transport ({}) should be higher than without ({}) due to Forward ETA",
            score_with_transport,
            score_without_transport
        );
    }

    #[test]
    fn test_ai_production_counter_selection() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let p1 = PlayerId(1);
        if let Some(mut players) = world.get_resource_mut::<Players>() {
            for p in &mut players.0 {
                if p.id == p1 {
                    p.funds = 25000; // 十分な資金
                }
            }
        }

        // 状況設定: 敵が「戦闘ヘリ(Bcopters)」を大量に出している
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 100),
        ));

        // 敵のヘリ
        for i in 0..2 {
            world.spawn((
                GridPosition { x: 4 + i, y: 0 },
                Faction(PlayerId(2)),
                UnitStats {
                    unit_type: UnitType::Bcopters,
                    cost: 9000,
                    max_movement: 6,
                    movement_type: MovementType::Air,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ));
        }

        // 実行
        world.insert_resource(ProductionPlan::default());
        let commands = decide_production(&mut world, p1);

        let produced_types: Vec<UnitType> = commands.iter().map(|c| c.unit_type).collect();

        // ヘリへのカウンターである「対空戦車(AntiAir)」または「地対空ミサイル(Missiles)」が選ばれるべき
        assert!(
            produced_types.contains(&UnitType::AntiAir)
                || produced_types.contains(&UnitType::Missiles),
            "Should produce anti-air units against helicopters. Got: {:?}",
            produced_types
        );
    }

    #[test]
    fn test_ai_production_infantry_priority_at_start() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        // テスト用の15x15平地マップを作成して挿入（島IDが正しく認識されるように）
        let map = Map {
            width: 15,
            height: 15,
            tiles: vec![Terrain::Plains; 225],
            topology: crate::resources::GridTopology::Square,
        };
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        world.insert_resource(map);
        world.insert_resource(island_map);

        let p1 = PlayerId(1);
        if let Some(mut players) = world.get_resource_mut::<Players>() {
            for p in &mut players.0 {
                if p.id == p1 {
                    p.funds = 10000; // 十分な資金（ロケットランチャー等も買える額）
                }
            }
        }

        // 全エンティティをクリアして初期マップ状態をシミュレート
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }

        // 自軍の生産施設
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 100),
        ));

        // 中立拠点が島に点在
        world.spawn((
            GridPosition { x: 3, y: 0 },
            Property::new(Terrain::City, None, 100),
        ));
        world.spawn((
            GridPosition { x: 0, y: 3 },
            Property::new(Terrain::City, None, 100),
        ));

        // 敵歩兵が極めて少数（1体）のみ、遠くに存在し平和な状態
        world.spawn((
            GridPosition { x: 10, y: 10 },
            Faction(PlayerId(2)),
            UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                max_movement: 3,
                movement_type: MovementType::Infantry,
                can_capture: true,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let commands = decide_production(&mut world, p1);

        let produced_types: Vec<UnitType> = commands.iter().map(|c| c.unit_type).collect();

        // 資金が豊富であっても、中立拠点獲得を最優先して「歩兵（軽歩兵または重歩兵）」を生産するはず
        assert!(
            produced_types.contains(&UnitType::Infantry)
                || produced_types.contains(&UnitType::Mech),
            "Should prioritize producing capturing units (Infantry/Mech) at start. Got: {:?}",
            produced_types
        );

        // ロケットランチャーなどの高額戦闘ユニットは生産されていないはず
        assert!(
            !produced_types.contains(&UnitType::Rockets),
            "Should not produce Rocket Launchers when it is peaceful and capturing is needed. Got: {:?}",
            produced_types
        );
    }
}
