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

    // ProductionPlanリソースの取得または作成
    if world.get_resource::<ProductionPlan>().is_none() {
        world.insert_resource(ProductionPlan::default());
    }

    let mut plan = world.get_resource_mut::<ProductionPlan>().unwrap();
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
                Terrain::Capital, // 貯金目標計算時は仮でCapital
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

    // --- 3. 実行可能な生産候補を全施設から収集 ---
    let mut candidates = Vec::new();
    let available_types: Vec<(UnitType, UnitStats)> = unit_registry
        .0
        .iter()
        .map(|(ut, s)| (*ut, s.clone()))
        .collect();

    for (facility_pos, terrain) in &my_facilities {
        let terrain_name = terrain.as_str();
        for (ut, stats) in &available_types {
            if !master_data.can_produce_unit(terrain_name, *ut) {
                continue;
            }
            // スコア計算
            let score = calculate_unit_score_at(
                *ut,
                stats,
                *facility_pos,
                &strategy,
                &enemy_units,
                &my_empty_transports,
                &damage_chart,
                &master_data,
                &map,
                &unit_registry,
                *terrain,
            );
            candidates.push((
                score,
                *facility_pos,
                *ut,
                stats.cost,
                stats.max_cargo,
                stats.can_capture,
            ));
        }
    }

    // スコア降順にソート
    candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // --- 4. 予算と施設重複を考慮して生産決定 ---
    let mut remaining_funds = available_funds;
    let mut current_strategy = strategy.clone();
    let mut used_facilities = std::collections::HashSet::new();

    for (score, pos, ut, cost, cargo, can_capture) in candidates {
        if used_facilities.contains(&pos) {
            continue;
        }
        if cost > remaining_funds {
            continue;
        }
        if score == 0 {
            continue;
        }

        // 決定
        commands.push(ProduceUnitCommand {
            player_id,
            target_x: pos.x,
            target_y: pos.y,
            unit_type: ut,
        });
        remaining_funds = remaining_funds.saturating_sub(cost);
        used_facilities.insert(pos);

        // 需要を動的に減衰させる
        if cargo > 0 {
            current_strategy.transport_demand =
                current_strategy.transport_demand.saturating_sub(cargo);
        }
        if can_capture {
            current_strategy.capture_demand = current_strategy.capture_demand.saturating_sub(1);
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
    my_empty_transports: &[(GridPosition, UnitStats)],
    damage_chart: &DamageChart,
    master_data: &MasterDataRegistry,
    map: &crate::resources::Map,
    unit_registry: &UnitRegistry,
    produced_at: Terrain,
) -> u32 {
    // 1. 基本スコア（敵との距離、脅威度）
    let mut score: u32 = if !strategy.priority_targets.is_empty() {
        let mut local_min_eta = 99;
        let mut base_val: i32 = 2000; // ベースを引き上げ

        for target in &strategy.priority_targets {
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
        let min_eta = local_min_eta;

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
            // 歩兵が既に十分な場合は大幅に評価を下げる
            score = score.saturating_sub(500);
        } else {
            // 戦闘フェーズで歩兵が足りているなら、他のユニットを優先
            score = score.saturating_sub(1500);
        }
    }
    // 輸送ユニットの評価（期待状態価値の向上分に基づく）
    if stats.max_cargo > 0 && !strategy.transport_candidates.is_empty() {
        let mut transport_utility: f32 = 0.0;
        for (c_pos, c_stats, c_value) in &strategy.transport_candidates {
            // この輸送ユニットが搭載可能かチェック
            if stats.loadable_unit_types.contains(&c_stats.unit_type) {
                // 候補ユニットにとっての最寄りのターゲットを特定
                let mut min_dist_to_target = 999;
                let mut best_target = GridPosition { x: 0, y: 0 };
                for target in &strategy.priority_targets {
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

        // 輸送需要がない場合は減衰（既存ロジックの維持）
        if strategy.transport_demand == 0 {
            score = score.saturating_sub(3000);
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
        const DEMAND_WEIGHT: f32 = 4000.0; // 若干強化
        let normalization_scale =
            crate::ai::demand::average_attack_expectation(damage_chart, unit_registry);
        let affinity =
            compute_unit_affinity(unit_type, damage_chart, unit_registry, normalization_scale);
        let demand_score = strategy.demand.dot(&affinity) * DEMAND_WEIGHT;
        score += demand_score as u32;
    }

    // 6. コスト正規化（ナップサックバイアス緩和）
    // スコアが同じなら安価な方が効率が良いが、高コストユニットが全く選ばれないのを防ぐため、
    // 「生存性と突破力」の対価として、高コストユニットに比例的なボーナスを与える。
    let cost_factor = (stats.cost as f32 / 5000.0).sqrt(); // 5000Gを基準に緩やかに加算
    score = (score as f32 * (1.0 + cost_factor * 0.5)) as u32; // 補正係数を 0.2 -> 0.5 に強化

    score
}

#[cfg(test)]
mod additional_tests {
    use super::*;
    use crate::ai::strategy;
    use crate::components::Health;
    use crate::resources::{Map, Terrain};

    #[test]
    fn test_ai_production_saving_for_mdtank() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let p1 = PlayerId(1);
        if let Some(mut players) = world.get_resource_mut::<Players>() {
            for p in &mut players.0 {
                if p.id == p1 {
                    p.funds = 12000; // MdTank(16000G)には足りないが、Tank(7000G)は買える金額
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

        // 自軍ユニットを数体配置（ユニット数が少ないと貯金より生産を優先するため）
        for i in 0..5 {
            world.spawn((
                GridPosition { x: 0, y: i + 1 },
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

        // 敵の「中戦車(MdTank)」を配置（距離6以上にしてDefenseフェーズを避ける）
        world.spawn((
            GridPosition { x: 6, y: 0 },
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

        // 実行
        world.insert_resource(ProductionPlan::default());
        let commands = decide_production(&mut world, p1);

        let plan = world.get_resource::<ProductionPlan>().unwrap();
        let reserve = *plan.reserves.get(&p1.0).unwrap_or(&0);

        // 中戦車(16000)以上のユニットを目標に貯金しているはず
        assert!(
            reserve >= 16000,
            "Reserve should be at least 16000 (MdTank). Got: {}",
            reserve
        );
        // 資金(12000) < 貯金目標(16000) なので、高価なユニット（戦車等）は控えるはず
        for cmd in &commands {
            let stats = unit_registry.get_stats(cmd.unit_type).unwrap();
            assert!(
                stats.cost <= 1000,
                "Should only produce very cheap units while saving. Got: {:?}",
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
        world.spawn((
            enemy_pos,
            Faction(PlayerId(2)),
            UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                max_movement: 3,
                movement_type: MovementType::Tank,
                ..UnitStats::mock()
            },
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

        // シナリオA: 輸送車なしでタンクのスコアを計測
        let score_without_transport;
        {
            let strategy = strategy::analyze_strategy(&mut world, p1);
            score_without_transport = calculate_unit_score_at(
                UnitType::Tank,
                &tank_stats,
                factory_pos,
                &strategy,
                &[],
                &[],
                &chart,
                &master_data,
                &map,
                &registry,
                Terrain::Factory,
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
                &[],
                &empty_transports,
                &chart,
                &master_data,
                &map,
                &registry,
                Terrain::Factory,
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
}
