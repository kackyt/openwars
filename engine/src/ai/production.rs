use crate::ai::demand::compute_unit_affinity;
use crate::ai::strategy::{ProductionPlan, ProductionStrategy, analyze_strategy};
use crate::components::{Faction, GridPosition, PlayerId, Property, UnitStats};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, MovementType, Players, Terrain, UnitRegistry, UnitType};
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
    let map = world.resource::<crate::resources::Map>().clone();

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
                    &map,
                    &unit_registry,
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

        // ユニット数が極端に少ない(5体未満)場合は、予算制限を緩和して生産を優先する
        if my_units.len() < 5 {
            budget = current_funds.saturating_sub(500);
        }

        // もし資金があり、かつ歩兵すら買えないほど予算が削られているなら、
        // 貯金目標を少し妥協して歩兵1体分(1000G)は確保する
        if current_funds >= 2000 && budget < 1000 {
            budget = 1000;
        }
        budget
    };

    // --- 3. 逐次的な生産決定 (施設単位の貪欲法) ---
    // 輸送需要(transport_demand)を動的に更新しながら生産を決定するため、
    // DPではなく施設ごとにベストな選択を行う方式を採用します。
    let mut available_types = Vec::new();
    for (unit_type, stats) in &unit_registry.0 {
        available_types.push((*unit_type, stats.clone()));
    }

    let mut remaining_funds = available_funds;
    let mut current_strategy = strategy.clone();

    for (facility_pos, terrain) in my_facilities {
        let terrain_name = terrain.as_str();
        let mut best_unit = None;
        let mut max_score = 0;

        for (ut, stats) in &available_types {
            if !master_data.can_produce_unit(terrain_name, *ut) {
                continue;
            }
            if stats.cost > remaining_funds {
                continue;
            }

            let score = calculate_unit_score_at(
                *ut,
                stats,
                facility_pos,
                &current_strategy,
                &enemy_units,
                &my_empty_transports,
                &damage_chart,
                &master_data,
                &map,
                &unit_registry,
            );

            if score > max_score {
                max_score = score;
                best_unit = Some((*ut, stats.cost, stats.max_cargo, stats.can_capture));
            }
        }

        if let Some((ut, cost, cargo, can_capture)) = best_unit {
            commands.push(ProduceUnitCommand {
                player_id,
                target_x: facility_pos.x,
                target_y: facility_pos.y,
                unit_type: ut,
            });
            remaining_funds = remaining_funds.saturating_sub(cost);
            // 輸送需要を更新
            if cargo > 0 {
                current_strategy.transport_demand =
                    current_strategy.transport_demand.saturating_sub(cargo);
            }
            // 占領需要を更新
            if can_capture {
                current_strategy.capture_demand = current_strategy.capture_demand.saturating_sub(1);
            }
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
    _my_empty_transports: &[GridPosition],
    damage_chart: &DamageChart,
    master_data: &MasterDataRegistry,
    map: &crate::resources::Map,
    unit_registry: &UnitRegistry,
) -> u32 {
    // 1. 基本スコア（敵との距離、脅威度）
    let mut min_eta = 0;
    let mut score: u32 = if !strategy.priority_targets.is_empty() {
        let mut local_min_eta = 99;
        let mut base_val: i32 = 2000; // ベースを引き上げ

        for target in &strategy.priority_targets {
            let mut dist = (pos.x as isize - target.x as isize).unsigned_abs()
                + (pos.y as isize - target.y as isize).unsigned_abs();

            // 海軍ユニットの対地評価補正
            if stats.movement_type == MovementType::Ship {
                let mut reachable_target = false;
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
                        }
                    } else {
                        // 間接攻撃ユニットは多少マシにする
                        dist += 10;
                    }
                }
            }

            // 地形コストを考慮したETAの簡易見積もり
            let move_cost = master_data
                .get_movement_cost(stats.movement_type, Terrain::Plains.as_str())
                .unwrap_or(1);
            let eta =
                (dist as u32 * move_cost + stats.max_movement - 1) / stats.max_movement.max(1);
            if eta < local_min_eta {
                local_min_eta = eta;
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
        if strategy.capture_demand > 0 {
            score += 2000; // 不足している場合は最優先
        } else if strategy.phase == GamePhase::Expansion {
            score += 1000; // 拡張期なら充足していても加点
        }
    }
    // 輸送ユニットの評価（キャパシティ需要に基づく）
    if stats.max_cargo > 0 {
        if strategy.transport_demand > 0 {
            score += 2000; // 不足している場合は大幅加点

            // 長距離補給ボーナス: 前線が遠いほど輸送機の価値を高める
            if min_eta > 8 {
                score += 1000;
            }
        } else {
            // 需要が満たされている場合
            if stats.max_ammo1 == 0 {
                // 武器がない純粋な輸送機は強く抑制
                score = score.saturating_sub(1000);
            } else {
                // 装甲車などの戦闘能力がある場合は抑制を小さくする
                score = score.saturating_sub(200);
            }
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

    // 5. 包括的需要ドット積ボーナス（Potential Impact Model）
    // DemandMatrix（自軍の能力の欠け）とユニット適性のドット積で加点する。
    // 既存のフェーズボーナス・アンチボーナスと共存させ、相乗効果で航空等の専門需要を反映する。
    {
        // DEMAND_WEIGHT: 需要が最大値(1.0)のとき、フェーズボーナス(最大1500)を上回る値に設定
        const DEMAND_WEIGHT: f32 = 3000.0;
        let normalization_scale =
            crate::ai::demand::average_attack_expectation(damage_chart, unit_registry);
        let affinity =
            compute_unit_affinity(unit_type, damage_chart, unit_registry, normalization_scale);
        let demand_score = strategy.demand.dot(&affinity) * DEMAND_WEIGHT;
        score += demand_score as u32;
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
