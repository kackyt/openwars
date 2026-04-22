use crate::components::{Faction, GridPosition, PlayerId, Property, Transporting, UnitStats};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Players, UnitRegistry, UnitType};
use bevy_ecs::prelude::*;

/// 生産AI。
/// 以下のロジックで生産計画を立てます。
/// - 歩兵・重歩兵は占領等のため10体を目安に高く評価
/// - その他のユニットは敵軍のユニットとの相性やコストから評価値を計算
/// - 予算内で最も評価が高くなるよう動的計画法（ナップサック問題）で生産数・ユニットを決定
/// - 配置場所は、敵や未占領拠点からの距離で最も評価が高くなる位置を選ぶ
pub fn decide_production(world: &mut World, player_id: PlayerId) -> Vec<ProduceUnitCommand> {
    use crate::systems::production::can_produce_at;
    let mut commands = Vec::new();

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

    // 補充や修理のための予備資金として1000Gを残す
    const RESERVE_FUNDS: u32 = 1000;
    let available_funds = current_funds.saturating_sub(RESERVE_FUNDS);

    let (unit_registry, damage_chart, master_data) = {
        let ur = world.get_resource::<UnitRegistry>().cloned();
        let dc = world.get_resource::<DamageChart>().cloned();
        let md = world.get_resource::<MasterDataRegistry>().cloned();
        if ur.is_none() || dc.is_none() || md.is_none() {
            return commands;
        }
        (ur.unwrap(), dc.unwrap(), md.unwrap())
    };

    let mut my_infantry_count = 0;
    let mut enemy_units = Vec::new();
    let mut unowned_properties = Vec::new();
    let mut my_facilities = Vec::new();
    let mut occupied_positions = std::collections::HashSet::new();

    {
        let mut q_units =
            world.query_filtered::<(&GridPosition, &Faction, &UnitStats), Without<Transporting>>();
        for (pos, faction, stats) in q_units.iter(world) {
            occupied_positions.insert(*pos);
            if faction.0 == player_id {
                if stats.unit_type == UnitType::Infantry || stats.unit_type == UnitType::Mech {
                    my_infantry_count += 1;
                }
            } else {
                enemy_units.push((*pos, stats.clone()));
            }
        }

        let mut q_props = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in q_props.iter(world) {
            if prop.owner_id == Some(player_id)
                && master_data.is_production_facility(prop.terrain.as_str())
            {
                if !occupied_positions.contains(pos) {
                    my_facilities.push((*pos, prop.terrain));
                }
            } else if prop.owner_id != Some(player_id) {
                unowned_properties.push(*pos);
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

    let mut production_candidates = Vec::new();

    for (ut, stats) in &available_types {
        let mut score = 0;

        if *ut == UnitType::Infantry || *ut == UnitType::Mech {
            if my_infantry_count < 10 {
                // 歩兵が足りない場合、1体につき1000点（歩兵の標準コスト相当）のボーナスを与えて生産を促進する
                let infantry_cost = unit_registry
                    .get_stats(UnitType::Infantry)
                    .map(|s| s.cost)
                    .unwrap_or(1000);
                // 歩兵が足りない場合、1体につき歩兵の標準コスト相当のボーナスを与えて生産を促進する
                let infantry_shortage_bonus = infantry_cost;
                score += (10 - my_infantry_count) * infantry_shortage_bonus;
            }
            // 軽歩兵と重歩兵の基礎スコアを、ユニットの機動力（移動力）と潜在的な戦闘力（コスト）のバランスから動的に算出する
            // 軽歩兵は移動力が高いこと、重歩兵はコストが高いこと（戦闘力に比例）が評価に反映される
            score += stats.max_movement * 100 + (stats.cost / 10);
        }

        let mut combat_score = 0;
        for (_, enemy_stats) in &enemy_units {
            let base_dmg = damage_chart
                .get_base_damage(*ut, enemy_stats.unit_type)
                .unwrap_or(0);
            let sec_dmg = damage_chart
                .get_base_damage_secondary(*ut, enemy_stats.unit_type)
                .unwrap_or(0);
            let max_dmg = std::cmp::max(base_dmg, sec_dmg);

            // ダメージはパーセンテージ（0-100）であるため、敵のコストに掛けて100で割ることで、実質的なダメージ金額価値を算出する
            combat_score += (max_dmg * enemy_stats.cost) / 100;
        }
        score += combat_score;

        production_candidates.push((stats.cost, score, *ut));
    }

    let max_items = my_facilities.len();
    // 計算量を削減するため、予算とコストを100G単位にスケールダウンしてDPテーブルを構築する
    let budget = (available_funds / 100) as usize;
    let mut dp = vec![vec![0; budget + 1]; max_items + 1];
    let mut choice = vec![vec![None; budget + 1]; max_items + 1];

    for i in 1..=max_items {
        for w in 0..=budget {
            dp[i][w] = dp[i - 1][w];
            choice[i][w] = None;

            for (cost, score, ut) in &production_candidates {
                let scaled_cost = (*cost / 100) as usize;
                if scaled_cost <= w {
                    let new_score = dp[i - 1][w - scaled_cost] + score;
                    if new_score > dp[i][w] {
                        dp[i][w] = new_score;
                        choice[i][w] = Some((*ut, scaled_cost));
                    }
                }
            }
        }
    }

    let mut selected_units = Vec::new();
    let mut curr_w = budget;
    for i in (1..=max_items).rev() {
        if let Some((ut, cost)) = choice[i][curr_w] {
            selected_units.push(ut);
            curr_w -= cost;
        }
    }

    for ut in selected_units {
        let mut best_facility_idx = None;
        let mut best_place_score: isize = -1;

        for (idx, (pos, _terrain)) in my_facilities.iter().enumerate() {
            if can_produce_at(world, player_id, pos.x, pos.y, ut, &master_data).is_err() {
                continue;
            }

            let mut place_score: isize = 0;

            let stats = unit_registry.get_stats(ut).unwrap();
            let max_movement = std::cmp::max(1, stats.max_movement) as isize;
            let max_range = stats.max_range as isize;

            // ユニットの機動力と移動コストを加味して到達ターン数を近似する
            // 本格的な経路探索は重いため、対象までの直線距離(マンハッタン)に対し、平地(Plains)の移動コストを掛けて概算する
            let plains_cost = master_data
                .get_movement_cost(stats.movement_type, "平地")
                .unwrap_or(1) as isize;
            let avg_move_cost = plains_cost;

            if ut == UnitType::Infantry || ut == UnitType::Mech {
                let mut min_turns = isize::MAX;
                for unowned_pos in &unowned_properties {
                    let dist = (pos.x as isize - unowned_pos.x as isize).abs()
                        + (pos.y as isize - unowned_pos.y as isize).abs();
                    // 拠点に到達するまでのターン数を計算 (切り上げ)
                    let turns = std::cmp::max(1, ((dist * avg_move_cost) + max_movement - 1) / max_movement);
                    if turns < min_turns {
                        min_turns = turns;
                    }
                }
                if min_turns != isize::MAX {
                    let infantry_cost = stats.cost as isize;
                    // 到達ターン数で評価値を割り引く（1ターンの場合は期待値/1, 2ターンの場合は期待値/2）
                    place_score += infantry_cost / min_turns;
                }
            } else {
                let mut combat_place_score = 0;
                for (enemy_pos, enemy_stats) in &enemy_units {
                    let dist = (pos.x as isize - enemy_pos.x as isize).abs()
                        + (pos.y as isize - enemy_pos.y as isize).abs();
                    // 遠距離ユニットの場合は射程に入るまでの距離で計算する
                    let target_dist = std::cmp::max(0, dist - max_range);
                    // 射程に入るまでのターン数を計算 (切り上げ)
                    let turns =
                        std::cmp::max(1, ((target_dist * avg_move_cost) + max_movement - 1) / max_movement);

                    let base_dmg = damage_chart
                        .get_base_damage(ut, enemy_stats.unit_type)
                        .unwrap_or(0);
                    let sec_dmg = damage_chart
                        .get_base_damage_secondary(ut, enemy_stats.unit_type)
                        .unwrap_or(0);
                    let max_dmg = std::cmp::max(base_dmg, sec_dmg);

                    if max_dmg > 0 {
                        // 基礎期待値を到達ターン数で割り引いて評価する
                        let base_expected_value =
                            (max_dmg as isize * enemy_stats.cost as isize) / 100;
                        combat_place_score += base_expected_value / turns;
                    }
                }
                place_score += combat_place_score;
            }

            if place_score > best_place_score {
                best_place_score = place_score;
                best_facility_idx = Some(idx);
            }
        }

        if let Some(idx) = best_facility_idx {
            let (pos, _) = my_facilities.remove(idx);
            commands.push(ProduceUnitCommand {
                target_x: pos.x,
                target_y: pos.y,
                unit_type: ut,
                player_id,
            });
        }
    }

    commands
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
    use crate::resources::{DamageChart, Player, Players};

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
}
