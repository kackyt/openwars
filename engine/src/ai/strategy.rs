use crate::components::{Faction, GridPosition, PlayerId, Property, UnitStats};
use crate::resources::UnitType;
use bevy_ecs::prelude::*;
use std::collections::HashMap;

/// ゲームの戦略的フェーズ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GamePhase {
    /// 拡張期: 未占領の拠点を確保することを最優先するフェーズ。
    #[default]
    Expansion,
    /// 対峙期: 前線が形成され、敵軍と睨み合っているフェーズ。
    Contested,
    /// 決戦期: 敵の拠点を奪い、敵軍を壊滅させるフェーズ。
    Assault,
    /// 防衛期: 自軍の首都や拠点が脅かされている緊急フェーズ。
    Defense,
}

/// 生産戦略。
/// マップの状態分析から導き出された、現在のプレイヤーが取るべき生産方針。
#[derive(Debug, Clone, Default)]
pub struct ProductionStrategy {
    /// 現在の戦略フェーズ。
    pub phase: GamePhase,
    /// 目標とするユニット構成比率（UnitTypeごとの重み）。
    pub ideal_composition: HashMap<UnitType, f32>,
    /// 戦略的に優先すべきターゲット位置（未占領拠点や敵の群れ）。
    pub priority_targets: Vec<GridPosition>,
}

/// 複数ターンにまたがる生産計画。
/// 貯金や次ターンの生産予約を管理するリソース。
#[derive(Resource, Debug, Clone, Default)]
pub struct ProductionPlan {
    /// 勢力ごとの貯金状況。
    /// キーはプレイヤーID(Factionの値を流用)、値は予約されている資金額。
    pub reserves: HashMap<u32, u32>,
    /// 勢力ごとの次ターン生産予約ユニット。
    pub reservations: HashMap<u32, Vec<UnitType>>,
}

/// 現在のマップ状況を分析し、最適な戦略を決定します。
pub fn analyze_strategy(world: &mut World, player_id: PlayerId) -> ProductionStrategy {
    let mut strategy = ProductionStrategy::default();

    let mut total_properties = 0;
    let mut unowned_properties = Vec::new();
    let mut my_properties = Vec::new();
    let mut enemy_properties = Vec::new();
    let mut my_capital_pos = None;

    // 1. 拠点の分析
    {
        let mut q_props = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in q_props.iter(world) {
            total_properties += 1;
            if prop.owner_id == Some(player_id) {
                my_properties.push(*pos);
                if prop.terrain == crate::resources::Terrain::Capital {
                    my_capital_pos = Some(*pos);
                }
            } else if prop.owner_id.is_none() {
                unowned_properties.push(*pos);
            } else {
                enemy_properties.push(*pos);
            }
        }
    }

    let mut my_units = Vec::new();
    let mut enemy_units = Vec::new();

    // 2. ユニットの分析
    {
        let mut q_units = world.query::<(&GridPosition, &Faction, &UnitStats)>();
        for (pos, faction, stats) in q_units.iter(world) {
            if faction.0 == player_id {
                my_units.push((*pos, stats.unit_type));
            } else {
                enemy_units.push((*pos, stats.unit_type));
            }
        }
    }

    // 3. フェーズの判定
    let unowned_ratio = if total_properties > 0 {
        unowned_properties.len() as f32 / total_properties as f32
    } else {
        0.0
    };

    // 輸送需要の計算
    let infantry_count = my_units
        .iter()
        .filter(|(_, ut)| *ut == UnitType::Infantry || *ut == UnitType::Mech)
        .count();
    let transport_capacity = my_units
        .iter()
        .filter(|(_, ut)| {
            *ut == UnitType::SupplyTruck
                || *ut == UnitType::TransportHelicopter
                || *ut == UnitType::Lander
        })
        .count(); // 簡易的に1ユニットにつき1人収容と仮定

    let transport_demand = infantry_count > transport_capacity + 2;

    // 首都付近に敵がいるかチェック
    let mut capital_threatened = false;
    if let Some(cap_pos) = my_capital_pos {
        for (enemy_pos, _) in &enemy_units {
            let dist = (cap_pos.x as i32 - enemy_pos.x as i32).abs()
                + (cap_pos.y as i32 - enemy_pos.y as i32).abs();
            if dist <= 5 {
                capital_threatened = true;
                break;
            }
        }
    }

    let is_capital_threatened = capital_threatened;

    if is_capital_threatened {
        strategy.phase = GamePhase::Defense;
    } else if unowned_ratio > 0.2 {
        strategy.phase = GamePhase::Expansion;
    } else if enemy_units.len() > my_units.len() + 2 {
        strategy.phase = GamePhase::Contested;
    } else {
        strategy.phase = GamePhase::Assault;
    }

    // 4. 理想構成比率の決定
    match strategy.phase {
        GamePhase::Expansion => {
            strategy.ideal_composition.insert(UnitType::Infantry, 0.7);
            strategy.ideal_composition.insert(UnitType::Tank, 0.2);
            strategy.ideal_composition.insert(UnitType::Recon, 0.1);
            strategy.priority_targets = unowned_properties;
        }
        GamePhase::Contested => {
            strategy.ideal_composition.insert(UnitType::Infantry, 0.4);
            strategy.ideal_composition.insert(UnitType::Tank, 0.4);
            strategy.ideal_composition.insert(UnitType::Artillery, 0.2);
            strategy.priority_targets = enemy_properties;
        }
        GamePhase::Assault => {
            strategy.ideal_composition.insert(UnitType::Infantry, 0.2);
            strategy.ideal_composition.insert(UnitType::Tank, 0.6);
            strategy.ideal_composition.insert(UnitType::Artillery, 0.2);
            strategy.priority_targets = enemy_properties;
        }
        GamePhase::Defense => {
            strategy.ideal_composition.insert(UnitType::Infantry, 0.5);
            strategy.ideal_composition.insert(UnitType::Tank, 0.3);
            strategy.ideal_composition.insert(UnitType::AntiAir, 0.2);
            if let Some(cap_pos) = my_capital_pos {
                strategy.priority_targets = vec![cap_pos];
            }
        }
    }

    // 輸送需要が高い場合、輸送ユニットの比率を底上げする
    if transport_demand {
        *strategy
            .ideal_composition
            .entry(UnitType::SupplyTruck)
            .or_default() += 0.2;
    }

    strategy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::Terrain;

    #[test]
    fn test_analyze_strategy_expansion() {
        let mut world = World::new();
        let p1 = PlayerId(1);

        // 拠点を配置 (未占領が多い)
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            GridPosition { x: 5, y: 5 },
            Property::new(Terrain::Factory, None, 100),
        ));
        world.spawn((
            GridPosition { x: 10, y: 10 },
            Property::new(Terrain::Factory, None, 100),
        ));

        let strategy = analyze_strategy(&mut world, p1);
        assert_eq!(strategy.phase, GamePhase::Expansion);
        assert!(strategy.ideal_composition.get(&UnitType::Infantry).unwrap() > &0.6);
    }

    #[test]
    fn test_analyze_strategy_defense() {
        let mut world = World::new();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // 首都
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));

        // 首都のすぐそばに敵ユニット
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Faction(p2),
            UnitStats {
                unit_type: UnitType::Tank,
                ..UnitStats::mock()
            },
        ));

        let strategy = analyze_strategy(&mut world, p1);
        assert_eq!(strategy.phase, GamePhase::Defense);
    }
}
