use crate::ai::strategy::{ProductionPlan, ProductionStrategy, analyze_strategy};
use crate::components::{Faction, GridPosition, PlayerId, Property, UnitStats};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Players, Terrain, UnitRegistry, UnitType};
use bevy_ecs::prelude::*;

use super::strategy::GamePhase;

/// 生産AI。
/// 以下のロジックで生産計画を立てます。
/// - 歩兵・重歩兵は占領等のため10体を目安に高く評価
/// - その他のユニットは戦略（フェーズ）、アンチ性能、到達ターン数（ETA）に基づき多角的に評価
/// - 予算（貯金を差し引いた仮想予算）内で最も評価が高くなるよう動的計画法（ナップサック問題）で生産を決定
pub fn decide_production(world: &mut World, player_id: PlayerId) -> Vec<ProduceUnitCommand> {
    let mut commands = Vec::new();

    let strategy = analyze_strategy(world, player_id);

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
                if let Some(cargo) = cargo_opt
                    && cargo.loaded.is_empty()
                    && (stats.unit_type == UnitType::SupplyTruck
                        || stats.unit_type == UnitType::TransportHelicopter)
                {
                    my_empty_transports.push(*pos);
                }
            } else {
                enemy_units.push((*pos, stats.clone()));
            }
        }
    }

    let mut capital_pos = None;
    let mut my_facilities = Vec::new();
    let mut producible_types = std::collections::HashSet::new();

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
                if crate::systems::production::is_within_production_range(capital_pos, pos.x, pos.y)
                {
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
    if let Some(mut plan) = world.get_resource_mut::<ProductionPlan>() {
        if strategy.phase == GamePhase::Defense {
            plan.reserves.insert(player_id.0, 0);
        } else {
            reserves = *plan.reserves.get(&player_id.0).unwrap_or(&0);

            // 欲しいユニット（一番スコアが高いもの）が買えない場合、貯金を検討
            // ただし、現在持っている施設で生産可能なものに限定する
            let mut best_unit = None;
            let mut max_score = 0;
            let score_ref_pos = capital_pos.unwrap_or(GridPosition { x: 0, y: 0 });

            for (ut, stats) in &unit_registry.0 {
                if !producible_types.contains(ut) {
                    continue;
                }

                let score = calculate_unit_score_at(
                    *ut,
                    stats,
                    score_ref_pos,
                    &strategy,
                    &enemy_units,
                    &my_empty_transports,
                    &damage_chart,
                    &master_data,
                );
                if score > max_score {
                    max_score = score;
                    best_unit = Some((*ut, stats.cost));
                }
            }

            if let Some((ut, cost)) = best_unit
                && cost > current_funds
                && cost > reserves
            {
                plan.reserves.insert(player_id.0, cost);
                plan.reservations.entry(player_id.0).or_default().push(ut);
                reserves = cost;
            } else if let Some((_, cost)) = best_unit
                && cost <= current_funds
            {
                // 買えるユニットがベストなら、貯金目標をリセット（または達成済みとする）
                if reserves > 0 && current_funds >= reserves {
                    plan.reserves.insert(player_id.0, 0);
                    reserves = 0;
                }
            }
        }
    } else {
        world.insert_resource(ProductionPlan::default());
    }

    // --- 2. 実行予算の算出 ---
    const MIN_BUFFER: u32 = 1000;
    let available_funds = if strategy.phase == GamePhase::Defense {
        current_funds.saturating_sub(MIN_BUFFER)
    } else {
        // 貯金目標がある場合、その半分程度は今ターン使わずに残す
        // ただし、歩兵(1000G)すら買えなくなるのは避けるため、下限を設ける
        let reserve_cut = reserves / 2;
        let mut budget = current_funds
            .saturating_sub(MIN_BUFFER)
            .saturating_sub(reserve_cut);

        // もし資金があり、かつ歩兵すら買えないほど予算が削られているなら、
        // 貯金目標を少し妥協して歩兵1体分(1000G)は確保する
        if current_funds >= 2000 && budget < 1000 {
            budget = 1000;
        }
        budget
    };

    // --- 3. 動的計画法による生産決定 ---
    let mut available_types = Vec::new();
    for (unit_type, stats) in &unit_registry.0 {
        available_types.push((*unit_type, stats.clone()));
    }

    let max_items = my_facilities.len();
    let budget_idx = (available_funds / 100) as usize;
    let mut dp = vec![vec![0; budget_idx + 1]; max_items + 1];
    let mut choice = vec![vec![None; budget_idx + 1]; max_items + 1];

    for i in 1..=max_items {
        let (facility_pos, terrain) = my_facilities[i - 1];
        let terrain_name = terrain.as_str();

        for w in 0..=budget_idx {
            dp[i][w] = dp[i - 1][w];
            choice[i][w] = None;

            for (ut, stats) in &available_types {
                if !master_data.can_produce_unit(terrain_name, *ut) {
                    continue;
                }

                let scaled_cost = (stats.cost / 100) as usize;
                if scaled_cost <= w {
                    let score = calculate_unit_score_at(
                        *ut,
                        stats,
                        facility_pos,
                        &strategy,
                        &enemy_units,
                        &my_empty_transports,
                        &damage_chart,
                        &master_data,
                    );

                    let new_score = dp[i - 1][w - scaled_cost] + score;
                    if new_score > dp[i][w] {
                        dp[i][w] = new_score;
                        choice[i][w] = Some((*ut, scaled_cost));
                    }
                }
            }
        }
    }

    // 結果の復元
    let mut curr_w = budget_idx;
    for i in (1..=max_items).rev() {
        if let Some((ut, cost)) = choice[i][curr_w] {
            let (facility_pos, _) = my_facilities[i - 1];
            commands.push(ProduceUnitCommand {
                player_id,
                target_x: facility_pos.x,
                target_y: facility_pos.y,
                unit_type: ut,
            });
            curr_w -= cost;
        }
    }

    commands
}

/// 指定した地点で特定のユニットを生産した場合の期待スコアを算出します。
#[allow(clippy::too_many_arguments)]
pub fn calculate_unit_score_at(
    unit_type: UnitType,
    stats: &UnitStats,
    pos: GridPosition,
    strategy: &ProductionStrategy,
    enemy_units: &[(GridPosition, UnitStats)],
    my_empty_transports: &[GridPosition],
    damage_chart: &DamageChart,
    master_data: &MasterDataRegistry,
) -> u32 {
    // 1. 基本スコア（敵との距離、脅威度）
    let mut score: u32 = if !strategy.priority_targets.is_empty() {
        let mut min_eta = 99;
        let base_val: i32 = 1000;

        for target in &strategy.priority_targets {
            let dist = (pos.x as isize - target.x as isize).unsigned_abs()
                + (pos.y as isize - target.y as isize).unsigned_abs();

            // 地形コストを考慮したETAの簡易見積もり
            let move_cost = master_data
                .get_movement_cost(stats.movement_type, Terrain::Plains.as_str())
                .unwrap_or(1);
            let eta =
                (dist as u32 * move_cost + stats.max_movement - 1) / stats.max_movement.max(1);
            if eta < min_eta {
                min_eta = eta;
            }
        }

        // 1ターン遅れるごとに100点のペナルティ（最大1000点）
        let eta_penalty = std::cmp::min(1000, min_eta * 100);
        base_val.saturating_sub(eta_penalty as i32).max(1) as u32
    } else {
        // 敵がいない場合は均一
        100
    };

    // 2. 特殊役割ボーナス
    if stats.can_capture && strategy.phase == GamePhase::Expansion {
        score += 2000;
    }
    // 輸送ユニットの評価（空の輸送ユニットがある場合は抑制、ない場合は高評価）
    if stats.max_cargo > 0 {
        if my_empty_transports.is_empty() {
            score += 800;
        } else {
            score = score.saturating_sub(500);
        }
    }

    // 3. アンチ性能ボーナス
    // 敵の主力ユニットに対して有利なユニットを高く評価
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

    score
}

#[cfg(test)]
mod additional_tests {
    use super::*;

    #[test]
    fn test_ai_production_map_2_repro() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_2").unwrap();

        // 資金をセット
        let p2 = PlayerId(2);
        if let Some(mut players) = world.get_resource_mut::<Players>() {
            for p in &mut players.0 {
                p.funds = 7000;
            }
        }

        // 生産実行
        let commands = decide_production(&mut world, p2);

        println!("AI2 Production Commands: {:?}", commands);

        // 何らかの生産が行われるべき
        assert!(
            !commands.is_empty(),
            "AI2 should produce something on map_2"
        );
    }

    #[test]
    fn test_ai_production_respects_facility_types() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let p1 = PlayerId(1);
        if let Some(mut players) = world.get_resource_mut::<Players>() {
            for p in &mut players.0 {
                if p.id == p1 {
                    p.funds = 5000;
                }
            }
        }

        // 全エンティティを一度削除して、特定の状況を再現
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }

        // 工場しかない場合
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 100),
        ));

        // 敵を遠くに配置して「何か買いたい」状態にする
        world.spawn((
            GridPosition { x: 10, y: 10 },
            Faction(PlayerId(2)),
            UnitStats {
                unit_type: UnitType::Tank,
                ..UnitStats::mock()
            },
        ));

        // 実行
        let _ = decide_production(&mut world, p1);

        // 貯金目標を確認
        let plan = world.get_resource::<ProductionPlan>().unwrap();
        let reserve = *plan.reserves.get(&p1.0).unwrap_or(&0);

        // 工場しかないので、戦艦(30000G)などを目標にしてはいけない。
        assert!(
            reserve < 20000,
            "Should not reserve expensive naval units without a port. Reserve was: {}",
            reserve
        );
    }
}
