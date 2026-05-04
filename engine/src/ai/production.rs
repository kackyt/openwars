use crate::ai::strategy::{ProductionPlan, ProductionStrategy, analyze_strategy};
use crate::components::{Faction, GridPosition, PlayerId, Property, UnitStats};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Players, UnitRegistry, UnitType};
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

    // 1. 資金計画の更新
    let mut reserves = 0;
    if let Some(mut plan) = world.get_resource_mut::<ProductionPlan>() {
        if strategy.phase == GamePhase::Defense {
            // 防衛期は貯金を崩して全力生産
            plan.reserves.insert(player_id.0, 0);
        } else {
            reserves = *plan.reserves.get(&player_id.0).unwrap_or(&0);

            // 欲しいユニット（一番スコアが高いもの）が買えない場合、貯金を検討
            let mut available_types = Vec::new();
            for (unit_type, stats) in &unit_registry.0 {
                available_types.push((*unit_type, stats.clone()));
            }

            let mut best_unit = None;
            let mut max_score = 0;
            // 仮の拠点を中心にスコアを計算
            let dummy_pos = GridPosition { x: 0, y: 0 };

            for (ut, stats) in &available_types {
                let score = calculate_unit_score_at(
                    *ut,
                    stats,
                    dummy_pos,
                    &strategy,
                    &[],
                    &[],
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
                // 現在の所持金で買えず、かつ貯金目標も下回っているなら貯金目標を更新
                plan.reserves.insert(player_id.0, cost);
                plan.reservations.entry(player_id.0).or_default().push(ut);
                reserves = cost;
            }
        }
    } else {
        // リソースがない場合は作成
        world.insert_resource(ProductionPlan::default());
    }

    // 2. 実行予算の算出
    // 貯金目標（reserves）がある場合、今ターンは「(所持金 - 目標額)」ではなく
    // 「今ターンの収入を貯金に回す」イメージ。
    // ここでは単純に「(所持金 - 1000G buffer) のうち、目標額に達するまでは控えめに使う」とする。
    const MIN_BUFFER: u32 = 1000;
    let available_funds = if strategy.phase == GamePhase::Defense {
        current_funds.saturating_sub(MIN_BUFFER)
    } else {
        // 貯金目標がある場合、その半分程度は今ターン使わずに残す
        current_funds
            .saturating_sub(MIN_BUFFER)
            .saturating_sub(reserves / 2)
    };

    let mut enemy_units = Vec::new();
    let mut my_facilities = Vec::new();
    let mut occupied_positions = std::collections::HashSet::new();
    let mut my_empty_transports = Vec::new();

    {
        // マップ上の全ユニットをスキャン
        let mut q_units = world.query::<(
            Entity,
            &GridPosition,
            &Faction,
            &UnitStats,
            Option<&crate::components::CargoCapacity>,
            Option<&crate::components::Transporting>,
        )>();
        for (_entity, pos, faction, stats, cargo_opt, transporting_opt) in q_units.iter(world) {
            // 輸送中のユニットはマップ上に存在しないため除外
            if transporting_opt.is_some() {
                continue;
            }

            occupied_positions.insert(*pos);
            if faction.0 == player_id {
                // 空の輸送ユニット（収容数が0）を収集
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

        let mut q_props = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in q_props.iter(world) {
            if prop.owner_id == Some(player_id)
                && master_data.is_production_facility(prop.terrain.as_str())
                && !occupied_positions.contains(pos)
            {
                my_facilities.push((*pos, prop.terrain));
            }
        }
    }

    if my_facilities.is_empty() {
        return commands;
    }

    let mut available_types = Vec::new();
    for (unit_type, stats) in &unit_registry.0 {
        available_types.push((*unit_type, stats.clone()));
    }

    let max_items = my_facilities.len();
    let budget = (available_funds / 100) as usize;
    let mut dp = vec![vec![0; budget + 1]; max_items + 1];
    let mut choice = vec![vec![None; budget + 1]; max_items + 1];

    for i in 1..=max_items {
        let (facility_pos, terrain) = my_facilities[i - 1];
        let terrain_name = terrain.as_str();

        for w in 0..=budget {
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

                    #[cfg(test)]
                    println!(
                        "AI Production: Unit={:?}, Pos={:?}, Score={}, Phase={:?}",
                        ut, facility_pos, score, strategy.phase
                    );

                    let new_score = dp[i - 1][w - scaled_cost] + score;
                    if new_score > dp[i][w] {
                        dp[i][w] = new_score;
                        choice[i][w] = Some((*ut, scaled_cost, facility_pos));
                    }
                }
            }
        }
    }

    let mut curr_w = budget;
    for i in (1..=max_items).rev() {
        if let Some((ut, _cost, pos)) = choice[i][curr_w] {
            commands.push(ProduceUnitCommand {
                target_x: pos.x,
                target_y: pos.y,
                unit_type: ut,
                player_id,
            });
            curr_w -= choice[i][curr_w].unwrap().1;
        }
    }

    commands
}

#[allow(clippy::too_many_arguments)]
fn calculate_unit_score_at(
    unit_type: UnitType,
    stats: &UnitStats,
    facility_pos: GridPosition,
    strategy: &ProductionStrategy,
    enemy_units: &[(GridPosition, UnitStats)],
    my_empty_transports: &[GridPosition],
    damage_chart: &DamageChart,
    master_data: &MasterDataRegistry,
) -> u32 {
    // 1. BaseValue: 機動力、射程、コストをベースにする
    // コストの重みを上げ、高価なユニットが選ばれやすくする
    let base_value = (stats.max_movement * 20) + (stats.max_range * 50) + (stats.cost / 10);

    // 2. StrategyBonus: 戦略フェーズと理想構成比率に基づく
    let mut strategy_bonus = 0.0;
    if let Some(&weight) = strategy.ideal_composition.get(&unit_type) {
        strategy_bonus += weight * 4000.0;
    }

    // 防衛フェーズかつ戦闘ユニットなら追加ボーナス
    if strategy.phase == GamePhase::Defense
        && (unit_type == UnitType::Tank
            || unit_type == UnitType::AntiAir
            || unit_type == UnitType::Artillery)
    {
        strategy_bonus += 2000.0;
    }

    // 3. CounterBonus: 敵軍ユニットに対する有効性
    let mut counter_bonus = 0;
    for (_, enemy_stats) in enemy_units {
        let base_dmg = damage_chart
            .get_base_damage(unit_type, enemy_stats.unit_type)
            .unwrap_or(0);
        let sec_dmg = damage_chart
            .get_base_damage_secondary(unit_type, enemy_stats.unit_type)
            .unwrap_or(0);
        let max_dmg = std::cmp::max(base_dmg, sec_dmg);
        counter_bonus += (max_dmg * enemy_stats.cost) / 500;
    }

    // 4. EtaPenalty: ターゲットへの到着ターン数によるペナルティ
    let mut eta_penalty = 0;
    if !strategy.priority_targets.is_empty() {
        let mut min_eta = 99;
        let avg_move_cost = master_data
            .get_movement_cost(stats.movement_type, "平地")
            .unwrap_or(1);

        // 周囲に空の輸送車がいるかチェック
        let has_transport_near = my_empty_transports.iter().any(|t_pos| {
            (t_pos.x as i32 - facility_pos.x as i32).abs()
                + (t_pos.y as i32 - facility_pos.y as i32).abs()
                <= 1
        });

        for target_pos in &strategy.priority_targets {
            let dist = (facility_pos.x as i32 - target_pos.x as i32).abs()
                + (facility_pos.y as i32 - target_pos.y as i32).abs();
            // 射程を加味したターゲット距離
            let target_dist = std::cmp::max(0, dist - stats.max_range as i32);

            let mut eta = (target_dist as u32 * avg_move_cost + stats.max_movement - 1)
                .checked_div(std::cmp::max(1, stats.max_movement))
                .unwrap_or(1);

            // 歩兵かつ輸送車が近くにいる場合、ETAを軽減
            if (unit_type == UnitType::Infantry || unit_type == UnitType::Mech)
                && has_transport_near
            {
                eta /= 2;
            }

            if eta < min_eta {
                min_eta = eta;
            }
        }
        // 1ターン遅れるごとに100点のペナルティ（最大1000点）
        eta_penalty = std::cmp::min(1000, min_eta * 100);
    }

    let total_score =
        (base_value as f32 + strategy_bonus) as i32 + counter_bonus as i32 - eta_penalty as i32;
    std::cmp::max(1, total_score) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Faction;
    use crate::resources::Player;
    use crate::resources::Terrain;
    use std::collections::HashMap;

    #[test]
    fn test_decide_production() {
        let mut world = World::new();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // Setup Funds
        world.insert_resource(Players(vec![
            Player {
                id: p1,
                name: "P1".to_string(),
                funds: 3500,
            }, // Can afford 2 Infantry (cost 1000) with 1000 buffer
            Player {
                id: p2,
                name: "P2".to_string(),
                funds: 1000,
            },
        ]));

        // Setup DamageChart
        world.insert_resource(crate::resources::DamageChart::new());
        // Setup Registry
        let mut registry = HashMap::new();
        registry.insert(
            UnitType::Infantry,
            crate::components::UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                ..crate::components::UnitStats::mock()
            },
        );
        world.insert_resource(UnitRegistry(registry));
        world.insert_resource(crate::resources::DamageChart::new());
        // Setup factories
        // 0. Setup Capital (needed for can_produce_at scope)
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));

        // Setup Registry
        let registry = MasterDataRegistry::load().unwrap();
        world.insert_resource(registry);

        // 1. P1 owned, empty -> Should produce (has 2500 funds)
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        // 2. P1 owned, empty -> Should produce (has 2500 funds)
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        // 3. P1 owned, empty -> Should NOT produce (funds 2500 -> 1500 -> 500, needs 1000)
        world.spawn((
            GridPosition { x: 3, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        // 4. P1 owned, occupied -> Should NOT produce
        world.spawn((
            GridPosition { x: 4, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));
        world.spawn((
            GridPosition { x: 4, y: 0 },
            Faction(p1),
            crate::components::UnitStats {
                ..crate::components::UnitStats::mock()
            },
        )); // Unit on top!

        // 5. Unowned factory -> Should NOT produce
        world.spawn((
            GridPosition { x: 5, y: 0 },
            Property::new(Terrain::Factory, None, 200),
        ));

        // 6. P2 owned factory -> Should NOT produce
        world.spawn((
            GridPosition { x: 6, y: 0 },
            Property::new(Terrain::Factory, Some(p2), 200),
        ));

        // Execute decide_production

        let commands = decide_production(&mut world, p1);

        println!("COMMANDS: {:?}", commands);
        // We expect 3 commands for (0,0) (Capital), (1,0) (Factory) and (2,0) (Factory)
        // (3,0) is skipped because of funds (needs 1000 buffer + 3*1000 = 4000, but has 3500)
        assert_eq!(commands.len(), 2);
        let mut targets: Vec<_> = commands.iter().map(|c| c.target_x).collect();
        targets.sort();
        assert!(targets.len() == 2);
    }

    #[test]
    fn test_production_strategy_integration() {
        let mut world = World::new();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        let mut registry = std::collections::HashMap::new();
        registry.insert(
            UnitType::Infantry,
            UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                max_movement: 3,
                max_range: 1,
                ..UnitStats::mock()
            },
        );
        registry.insert(
            UnitType::Tank,
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 7000,
                max_movement: 6,
                max_range: 1,
                ..UnitStats::mock()
            },
        );
        world.insert_resource(UnitRegistry(registry));
        world.insert_resource(DamageChart::new());
        world.insert_resource(MasterDataRegistry::load().unwrap());

        // 1. Expansion Phase Test (Many unowned properties)
        world.insert_resource(Players(vec![
            Player {
                id: p1,
                name: "P1".to_string(),
                funds: 50000,
            },
            Player {
                id: p2,
                name: "P2".to_string(),
                funds: 10000,
            },
        ]));

        // Capital at (0,0), Factory at (1,0)
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 100),
        ));

        // Many neutral properties to trigger Expansion phase
        for x in 2..10 {
            world.spawn((
                GridPosition { x, y: 0 },
                Property::new(Terrain::City, None, 100),
            ));
        }

        let commands = decide_production(&mut world, p1);
        // Expansion phase should prioritize Infantry
        assert!(commands.iter().any(|c| c.unit_type == UnitType::Infantry));

        // 2. Defense Phase Test (Capital threatened)
        world.insert_resource(Players(vec![Player {
            id: p1,
            name: "P1".to_string(),
            funds: 50000,
        }]));
        // Capital at (0,0)
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        // Unit at (5,5) - not on factory
        world.spawn((GridPosition { x: 5, y: 5 }, Faction(p1), UnitStats::mock()));
        // Enemy tank near capital
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Faction(p2),
            UnitStats::mock(),
            crate::components::Health {
                current: 100,
                max: 100,
            },
        ));

        let commands = decide_production(&mut world, p1);
        // Defense phase should prioritize combat units (Tank) or AntiAir
        // (Base score for Tank is much higher than Infantry when target is very close)
        assert!(commands.iter().any(|c| c.unit_type == UnitType::Tank));
    }

    #[test]
    fn test_ai_production_skips_occupied_factory() {
        let mut world = World::new();
        let p1 = PlayerId(1);

        // Setup Funds
        world.insert_resource(Players(vec![Player {
            id: p1,
            name: "P1".to_string(),
            funds: 5000,
        }]));

        // Setup DamageChart
        world.insert_resource(crate::resources::DamageChart::new());
        // Setup Registry
        let mut registry = HashMap::new();
        registry.insert(
            UnitType::Infantry,
            crate::components::UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                ..crate::components::UnitStats::mock()
            },
        );
        world.insert_resource(UnitRegistry(registry));
        world.insert_resource(crate::resources::DamageChart::new());
        // Setup MasterDataRegistry
        world.insert_resource(MasterDataRegistry::load().unwrap());

        // Setup Capital
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));

        // Case: Factory at (0,0) (Capital position) with a unit
        // Factory at (0,0)
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));
        // Factory at (0,0) occupied by a "Wait" unit
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Faction(p1),
            crate::components::UnitStats {
                ..crate::components::UnitStats::mock()
            },
            crate::components::ActionCompleted(true),
        ));

        // Factory empty at (1,0)
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        let commands = decide_production(&mut world, p1);

        println!("COMMANDS: {:?}", commands);
        // Should only produce on (1,0), not on (0,0)
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].target_x, 1);
    }
}

#[cfg(test)]
mod additional_tests {
    use super::*;
    use crate::resources::{DamageChart, Player, Players, Terrain};

    #[test]
    fn test_dp_selects_optimal_units() {
        // Since test_decide_production uses actual logic, we can verify it indirectly
        // or unit test the candidates selection. But here we test integration.
        let mut world = World::new();
        let p1 = PlayerId(1);
        world.insert_resource(Players(vec![Player {
            id: p1,
            name: "P1".to_string(),
            funds: 15000,
        }]));

        let mut registry = std::collections::HashMap::new();
        registry.insert(
            UnitType::Infantry,
            UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                ..UnitStats::mock()
            },
        );
        registry.insert(
            UnitType::Tank,
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 7000,
                ..UnitStats::mock()
            },
        );
        world.insert_resource(UnitRegistry(registry));
        world.insert_resource(DamageChart::new());
        world.insert_resource(MasterDataRegistry::load().unwrap());

        // Setup 2 factories
        world.insert_resource(Players(vec![Player {
            id: p1,
            name: "P1".to_string(),
            funds: 50000,
        }]));
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(crate::resources::Terrain::Capital, Some(p1), 200),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(crate::resources::Terrain::Factory, Some(p1), 200),
        ));
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(crate::resources::Terrain::Factory, Some(p1), 200),
        ));

        let commands = decide_production(&mut world, p1);

        println!("COMMANDS: {:?}", commands);
        assert_eq!(commands.len(), 3);
    }

    #[test]
    fn test_ai_production_air_and_navy() {
        let mut world = World::new();
        let p1 = PlayerId(1);
        let master_data = MasterDataRegistry::load().unwrap();

        world.insert_resource(Players(vec![Player {
            id: p1,
            name: "P1".to_string(),
            funds: 50000, // 高価なユニットも買える予算
        }]));

        let mut unit_registry_map = std::collections::HashMap::new();
        let mut damage_chart = crate::resources::DamageChart::new();
        for (name, unit_rec) in &master_data.units {
            let stats = master_data.create_unit_stats(name).unwrap();
            let u_type = stats.unit_type;
            unit_registry_map.insert(u_type, stats);

            // ダメージチャートの作成
            if let Some(weapon) = unit_rec.weapon1.as_ref().and_then(|w| {
                master_data
                    .weapons
                    .get(&crate::resources::master_data::UnitName(w.clone()))
            }) {
                for (def_name, dmg) in &weapon.damages {
                    if let Some(def_type) = crate::resources::UnitType::from_str(def_name) {
                        damage_chart.insert_damage(u_type, def_type, *dmg);
                    }
                }
            }
            if let Some(weapon) = unit_rec.weapon2.as_ref().and_then(|w| {
                master_data
                    .weapons
                    .get(&crate::resources::master_data::UnitName(w.clone()))
            }) {
                for (def_name, dmg) in &weapon.damages {
                    if let Some(def_type) = crate::resources::UnitType::from_str(def_name) {
                        damage_chart.insert_secondary_damage(u_type, def_type, *dmg);
                    }
                }
            }
        }
        world.insert_resource(UnitRegistry(unit_registry_map));
        world.insert_resource(damage_chart);
        world.insert_resource(master_data);

        // 首都 (地上部隊)
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 200),
        ));
        // 空港 (航空部隊)
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Airport, Some(p1), 200),
        ));
        // 港 (艦船部隊)
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(Terrain::Port, Some(p1), 200),
        ));

        // 敵ユニットを配置してスコアが出るようにする
        let p2 = PlayerId(2);
        world.spawn((
            GridPosition { x: 5, y: 5 },
            Faction(p2),
            UnitStats {
                unit_type: crate::resources::UnitType::Tank,
                cost: 7000,
                ..UnitStats::mock()
            },
        ));

        let commands = decide_production(&mut world, p1);

        println!("COMMANDS: {:?}", commands);

        let mut has_air = false;
        let mut has_navy = false;
        let mut has_ground = false;

        let registry = world.resource::<UnitRegistry>();
        for cmd in &commands {
            let stats = registry.get_stats(cmd.unit_type).unwrap();
            match stats.movement_type {
                crate::resources::MovementType::Air => has_air = true,
                crate::resources::MovementType::Ship => has_navy = true,
                _ => has_ground = true,
            }
        }

        assert!(has_air, "空港で航空部隊が生産されるはず");
        assert!(has_navy, "港で艦船部隊が生産されるはず");
        assert!(has_ground, "首都で地上部隊が生産されるはず");
    }
}
