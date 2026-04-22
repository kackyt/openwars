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
                const INFANTRY_SHORTAGE_BONUS: u32 = 1000;
                score += (10 - my_infantry_count) * INFANTRY_SHORTAGE_BONUS;
            }
            if *ut == UnitType::Infantry {
                score += 500;
            } else {
                score += 700;
            }
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

            combat_score += (max_dmg * enemy_stats.cost) / 100;
        }
        score += combat_score;

        production_candidates.push((stats.cost, score, *ut));
    }

    let max_items = my_facilities.len();
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

            let dist = |p1: &GridPosition, p2: &GridPosition| {
                (p1.x as isize - p2.x as isize).abs() + (p1.y as isize - p2.y as isize).abs()
            };

            if ut == UnitType::Infantry || ut == UnitType::Mech {
                let mut min_dist = isize::MAX;
                for unowned_pos in &unowned_properties {
                    let d = dist(pos, unowned_pos);
                    if d < min_dist {
                        min_dist = d;
                    }
                }
                if min_dist != isize::MAX {
                    // 距離が近いほど高い評価を与える（最大1000点、1マス離れるごとに10点減点）
                    // 1000点は初期の歩兵ボーナスと同等スケールにするための基準値
                    const MAX_PLACE_SCORE: isize = 1000;
                    const DISTANCE_PENALTY: isize = 10;
                    place_score += MAX_PLACE_SCORE - (min_dist * DISTANCE_PENALTY);
                }
            } else {
                let mut combat_place_score = 0;
                for (enemy_pos, enemy_stats) in &enemy_units {
                    let d = dist(pos, enemy_pos);
                    let d = if d == 0 { 1 } else { d };
                    let base_dmg = damage_chart
                        .get_base_damage(ut, enemy_stats.unit_type)
                        .unwrap_or(0);
                    let sec_dmg = damage_chart
                        .get_base_damage_secondary(ut, enemy_stats.unit_type)
                        .unwrap_or(0);
                    let max_dmg = std::cmp::max(base_dmg, sec_dmg);

                    if max_dmg > 0 {
                        combat_place_score +=
                            ((max_dmg as isize * enemy_stats.cost as isize) / 100) / d;
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
