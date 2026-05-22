use crate::ai::demand::{
    DemandMatrix, average_attack_expectation, compute_demand, compute_unit_affinity,
};
use crate::components::{Faction, GridPosition, PlayerId, Property, UnitStats};
use crate::resources::{MovementType, Terrain, UnitType};
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
    /// 未占領（中立）拠点の座標リスト
    pub unowned_properties: std::collections::HashSet<GridPosition>,
    /// 不足している輸送キャパシティ数。
    pub transport_demand: u32,
    /// 不足している占領ユニット数。
    pub capture_demand: u32,
    /// 包括的需要マトリクス（各戦闘カテゴリの脅威ギャップと占領脅威）。
    pub demand: DemandMatrix,
    /// 輸送を必要としている既存ユニットのリスト（位置、ステータス、基本価値）。
    pub transport_candidates: Vec<(GridPosition, UnitStats, f32)>,
    /// 現在保有している輸送ユニットの数
    pub existing_transport_count: usize,
    /// 敵地上ユニットが存在しない平和な島にあるプロパティの座標セット
    pub peaceful_properties: std::collections::HashSet<GridPosition>,
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

/// 敵ユニットが特定の拠点を脅かしているかを判定するヘルパー関数。
fn enemy_threatens_property(
    prop_pos: &GridPosition,
    prop_island_id: Option<usize>,
    enemy_pos: &GridPosition,
    enemy_stats: &UnitStats,
    enemy_island_id: Option<usize>,
) -> bool {
    let dist = (prop_pos.x as i32 - enemy_pos.x as i32).abs()
        + (prop_pos.y as i32 - enemy_pos.y as i32).abs();

    match enemy_stats.movement_type {
        MovementType::Air => {
            // 航空ユニットは島をまたいで長距離を移動できるため、
            // 攻撃力を持つユニットであり、かつ「移動力 + 射程 + 猶予(3マス)」以内にいる場合に脅威と判定する
            let has_weapons = enemy_stats.max_ammo1 > 0 || enemy_stats.max_ammo2 > 0;
            has_weapons && dist <= (enemy_stats.max_movement + enemy_stats.max_range + 3) as i32
        }
        MovementType::Ship => {
            // 海上ユニットは沿岸に近づいて攻撃するため、
            // 攻撃力を持つユニットであり、かつ「最大射程 + 2マス」または5マス以内にいる場合に脅威と判定する
            let has_weapons = enemy_stats.max_ammo1 > 0 || enemy_stats.max_ammo2 > 0;
            has_weapons && dist <= (enemy_stats.max_range + 2).max(5) as i32
        }
        _ => {
            // 地上ユニットの場合、同じ島にいるか、または島を問わず距離が極めて近い（8マス未満）場合に脅威と判定する
            let same_island = prop_island_id.is_some() && prop_island_id == enemy_island_id;
            same_island || dist < 8
        }
    }
}

/// 現在のマップ状況を分析し、最適な戦略を決定します。
pub fn analyze_strategy(world: &mut World, player_id: PlayerId) -> ProductionStrategy {
    let mut strategy = ProductionStrategy::default();

    let mut unowned_properties = Vec::new();
    let mut my_properties = Vec::new();
    let mut enemy_properties = Vec::new();
    let mut my_capital_pos = None;

    let master_data = world
        .get_resource::<crate::resources::master_data::MasterDataRegistry>()
        .cloned()
        .unwrap_or_else(|| {
            crate::resources::master_data::MasterDataRegistry::load().unwrap_or_default()
        });
    let map = world.resource::<crate::resources::Map>().clone();
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
        .unwrap_or_else(|| crate::ai::islands::IslandMap::analyze(&map));

    let mut allowed_islands = std::collections::HashMap::new();
    for island in &island_map.islands {
        let allowed = crate::ai::planner::is_invasion_allowed(world, player_id, island.id, island);
        allowed_islands.insert(island.id, allowed);
    }

    let get_island_id =
        |pos: &GridPosition| -> Option<usize> { island_map.get_island_at(pos).map(|i| i.id.0) };

    let mut my_base_island_ids = std::collections::HashSet::new();

    // 1. 拠点の分析
    {
        let mut q_props = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in q_props.iter(world) {
            if prop.owner_id == Some(player_id) {
                my_properties.push(*pos);
                if prop.terrain == crate::resources::Terrain::Capital {
                    my_capital_pos = Some(*pos);
                }
                // 自軍の生産拠点がある島IDを収集
                if master_data.is_production_facility(prop.terrain.as_str())
                    && let Some(island_id) = get_island_id(pos)
                {
                    my_base_island_ids.insert(island_id);
                }
            } else if prop.owner_id.is_none() {
                unowned_properties.push(*pos);
            } else {
                enemy_properties.push(*pos);
            }
        }
    }

    // unowned_properties を strategy に保存
    strategy.unowned_properties = unowned_properties.iter().cloned().collect();

    let mut my_units = Vec::new();
    let mut enemy_units = Vec::new();

    // 2. ユニットの分析
    {
        let mut q_units = world.query::<(
            &GridPosition,
            &Faction,
            &UnitStats,
            Option<&crate::components::Transporting>,
        )>();
        for (pos, faction, stats, transporting) in q_units.iter(world) {
            // マップ外（輸送機内など）のユニットは距離計算などの分析から除外
            if pos.x >= 9999 || transporting.is_some() {
                continue;
            }
            if faction.0 == player_id {
                my_units.push((*pos, stats.clone()));
            } else {
                enemy_units.push((*pos, stats.clone()));
            }
        }
    }

    // 交戦可能性の判定
    let mut min_enemy_dist = 999;
    for (m_pos, _) in &my_units {
        for (e_pos, _) in &enemy_units {
            let dist =
                (m_pos.x as i32 - e_pos.x as i32).abs() + (m_pos.y as i32 - e_pos.y as i32).abs();
            if dist < min_enemy_dist {
                min_enemy_dist = dist;
            }
        }
    }

    // 自軍平均移動力 + 射程 を閾値とする
    let avg_engagement_range = if !my_units.is_empty() {
        let total_reach: u32 = my_units
            .iter()
            .map(|(_, s)| s.max_movement + s.max_range)
            .sum();
        total_reach / my_units.len() as u32
    } else {
        5
    };

    let is_engaged = min_enemy_dist <= (avg_engagement_range + 1) as i32;

    // 3. フェーズの判定
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

    // --- 3.1 島嶼（IslandMap）と拠点獲得劣勢（収入不足）の分析 ---
    let mut island_unowned_properties = 0;
    for pos in &unowned_properties {
        if let Some(island_id) = get_island_id(pos)
            && my_base_island_ids.contains(&island_id)
        {
            island_unowned_properties += 1;
        }
    }

    let mut island_enemy_properties = 0;
    for pos in &enemy_properties {
        if let Some(island_id) = get_island_id(pos)
            && my_base_island_ids.contains(&island_id)
        {
            island_enemy_properties += 1;
        }
    }

    let mut island_my_capture_units = 0;
    for (pos, stats) in &my_units {
        if stats.can_capture
            && let Some(island_id) = get_island_id(pos)
            && my_base_island_ids.contains(&island_id)
        {
            island_my_capture_units += 1;
        }
    }

    // 島内の拠点数に基づく目標占領ユニット数
    let total_island_properties = island_unowned_properties + island_enemy_properties;
    let ideal_capture_units = ((total_island_properties as f32 * 0.5).ceil() as usize).clamp(3, 10);

    // 収入（拠点確保）優先判定: 島内に中立拠点が残っており、歩兵が目標未満である場合
    let need_more_revenue =
        island_unowned_properties > 0 && island_my_capture_units < ideal_capture_units;

    // フェーズの判定
    if capital_threatened {
        strategy.phase = GamePhase::Defense;
    } else if need_more_revenue {
        strategy.phase = GamePhase::Expansion;
    } else if is_engaged {
        if enemy_units.len() >= my_units.len() {
            strategy.phase = GamePhase::Contested;
        } else {
            strategy.phase = GamePhase::Assault;
        }
    } else {
        strategy.phase = GamePhase::Expansion;
    }

    // ターゲットの統合: フェーズに関わらず、中立拠点と敵拠点の両方を考慮する
    // ただしフェーズによって重みを変えるために、ここではリストの順序や内容を調整
    strategy.priority_targets = match strategy.phase {
        GamePhase::Expansion => {
            let mut targets = unowned_properties.clone();
            targets.extend(enemy_properties.iter().cloned());
            targets
        }
        GamePhase::Contested | GamePhase::Assault => {
            let mut targets = enemy_properties.clone();
            // 中立拠点も近いものはターゲットに含める
            targets.extend(unowned_properties.iter().cloned());
            targets
        }
        GamePhase::Defense => {
            if let Some(cap_pos) = my_capital_pos {
                vec![cap_pos]
            } else {
                enemy_properties.clone()
            }
        }
    };

    // 理想構成の適用
    match strategy.phase {
        GamePhase::Expansion => {
            strategy.ideal_composition.insert(UnitType::Infantry, 0.7);
            strategy.ideal_composition.insert(UnitType::Tank, 0.2);
            strategy.ideal_composition.insert(UnitType::Recon, 0.1);
        }
        GamePhase::Contested => {
            strategy.ideal_composition.insert(UnitType::Infantry, 0.4);
            strategy.ideal_composition.insert(UnitType::Tank, 0.4);
            strategy.ideal_composition.insert(UnitType::Artillery, 0.2);
        }
        GamePhase::Assault => {
            strategy.ideal_composition.insert(UnitType::Infantry, 0.2);
            strategy.ideal_composition.insert(UnitType::Tank, 0.6);
            strategy.ideal_composition.insert(UnitType::Artillery, 0.2);
        }
        GamePhase::Defense => {
            strategy.ideal_composition.insert(UnitType::Infantry, 0.5);
            strategy.ideal_composition.insert(UnitType::Tank, 0.3);
            strategy.ideal_composition.insert(UnitType::AntiAir, 0.2);
        }
    }

    // 占領需要の計算
    if total_island_properties > 0 {
        let base_demand = ideal_capture_units.saturating_sub(island_my_capture_units);
        if need_more_revenue {
            // 収入確保優先（歩兵不足）時は需要を上乗せし、歩兵生産が確実に最優先になるようにする
            strategy.capture_demand = (base_demand + 4).max(1) as u32;
        } else {
            strategy.capture_demand = base_demand as u32;
        }
    } else {
        strategy.capture_demand = 0;
    }

    // 包括的需要マトリクスの計算
    // 自軍・敵軍の状況から、占領脅威・消耗ギャップを数値化した需要ベクトル。
    {
        let damage_chart = world
            .get_resource::<crate::resources::DamageChart>()
            .cloned();
        let unit_registry = world
            .get_resource::<crate::resources::UnitRegistry>()
            .cloned();

        if let (Some(chart), Some(registry)) = (damage_chart, unit_registry) {
            // 自軍屠性を制限する：拠点の terrain を取得
            let my_props_for_demand: Vec<(GridPosition, Terrain)> = {
                let mut q = world.query::<(&GridPosition, &Property)>();
                q.iter(world)
                    .filter(|(_, p)| p.owner_id == Some(player_id))
                    .map(|(pos, p)| (*pos, p.terrain))
                    .collect()
            };
            strategy.demand = compute_demand(
                &my_units,
                &enemy_units,
                &my_props_for_demand,
                &chart,
                &registry,
            );

            // 輸送が必要なユニット（停滞ユニット）の抽出
            let map = world.resource::<crate::resources::Map>();
            let normalization_scale = average_attack_expectation(&chart, &registry);
            for (pos, stats) in &my_units {
                // 陸上ユニットかつ、輸送能力を持たない戦闘/占領用ユニットのみ
                if matches!(
                    stats.movement_type,
                    MovementType::Infantry
                        | MovementType::Tank
                        | MovementType::ArmoredCar
                        | MovementType::Artillery
                ) && stats.max_cargo == 0
                {
                    // 最寄りのターゲットへの距離と「海による遮断」を判定
                    let mut min_dist = 999;
                    let mut blocked_by_sea = false;

                    for target in &strategy.priority_targets {
                        // ターゲットがある島を取得し、侵攻が許可されているかチェック
                        if let Some(target_island_id) =
                            island_map.get_island_at(target).map(|i| i.id)
                        {
                            let allowed = allowed_islands
                                .get(&target_island_id)
                                .cloned()
                                .unwrap_or(true);
                            // 侵攻が許可されていない島は、海を越える輸送のターゲットとして考慮しない
                            if !allowed {
                                continue;
                            }
                        }

                        let dist = (pos.x as i32 - target.x as i32).abs()
                            + (pos.y as i32 - target.y as i32).abs();
                        if dist < min_dist {
                            min_dist = dist;

                            // 簡易パスサンプリングで海があるかチェック
                            blocked_by_sea = false;
                            let steps = 4;
                            for i in 1..steps {
                                let check_x =
                                    pos.x as i32 + (target.x as i32 - pos.x as i32) * i / steps;
                                let check_y =
                                    pos.y as i32 + (target.y as i32 - pos.y as i32) * i / steps;
                                if let Some(Terrain::Sea | Terrain::Shoal) =
                                    map.get_terrain(check_x as usize, check_y as usize)
                                {
                                    blocked_by_sea = true;
                                    break;
                                }
                            }
                        }
                    }

                    // 輸送を検討すべき条件: 海で遮断されている、または距離が極端に遠い
                    if blocked_by_sea || min_dist > 15 {
                        let affinity = compute_unit_affinity(
                            stats.unit_type,
                            &chart,
                            &registry,
                            normalization_scale,
                        );
                        // 価値 = (需要との一致度 * 係数) + (占領能力ボーナス)
                        let value = strategy.demand.dot(&affinity) * 2000.0
                            + (if stats.can_capture { 3000.0 } else { 0.0 });

                        strategy
                            .transport_candidates
                            .push((*pos, stats.clone(), value));
                    }
                }
            }

            let current_capacity: u32 = my_units.iter().map(|(_, s)| s.max_cargo).sum();
            // 海上輸送需要については、海軍ユニットのみをカウントする
            strategy.existing_transport_count = my_units
                .iter()
                .filter(|(_, s)| s.max_cargo > 0 && s.movement_type == MovementType::Ship)
                .count();

            // 輸送需要の計算: transport_candidates（実際に停滞しているユニット）の数に基づく
            strategy.transport_demand = (strategy.transport_candidates.len() as u32)
                .saturating_sub(current_capacity)
                .max(
                    if !strategy.transport_candidates.is_empty() && current_capacity == 0 {
                        1
                    } else {
                        0
                    },
                );
        }
    }

    // 平和な拠点（敵ユニットから十分に離れており、脅かされていない拠点）の分析
    let mut peaceful_properties = std::collections::HashSet::new();

    for pos in &my_properties {
        let prop_island_id = get_island_id(pos);
        let mut is_peaceful = true;

        for (e_pos, e_stats) in &enemy_units {
            let e_island_id = get_island_id(e_pos);
            if enemy_threatens_property(pos, prop_island_id, e_pos, e_stats, e_island_id) {
                is_peaceful = false;
                break;
            }
        }

        if is_peaceful {
            peaceful_properties.insert(*pos);
        }
    }
    strategy.peaceful_properties = peaceful_properties;

    strategy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{GridTopology, Map, Terrain};

    #[test]
    fn test_analyze_strategy_expansion() {
        let mut world = World::new();
        world.insert_resource(Map::new(15, 15, Terrain::Plains, GridTopology::Square));
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
        world.insert_resource(Map::new(15, 15, Terrain::Plains, GridTopology::Square));
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

    /// 首都が脅威にさらされている場合、未占領拠点が存在しても Expansion ではなく Defense フェーズが優先されることを確認する回帰テスト
    #[test]
    fn test_analyze_strategy_defense_prioritized_over_expansion() {
        let mut world = World::new();
        world.insert_resource(Map::new(15, 15, Terrain::Plains, GridTopology::Square));
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        world.insert_resource(crate::resources::MasterDataRegistry::load().unwrap());

        // 1. 首都 (p1) を配置
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));

        // 2. 多数の未占領拠点を配置 (本来なら Expansion フェーズになる条件)
        world.spawn((
            GridPosition { x: 5, y: 5 },
            Property::new(Terrain::Factory, None, 100),
        ));
        world.spawn((
            GridPosition { x: 10, y: 10 },
            Property::new(Terrain::Factory, None, 100),
        ));

        // 3. 首都のすぐそばに敵戦闘ユニットを配置 (Defense フェーズになるべき)
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Faction(p2),
            UnitStats {
                unit_type: UnitType::Tank,
                max_ammo1: 9,
                max_ammo2: 0,
                ..UnitStats::mock()
            },
        ));

        let strategy = analyze_strategy(&mut world, p1);
        assert_eq!(
            strategy.phase,
            GamePhase::Defense,
            "首都が脅かされている場合は、中立拠点があっても Defense フェーズが優先されるべき"
        );
    }
}
