#![allow(clippy::too_many_arguments)]

use crate::ai::demand::{
    AirDefenseAssessment, CombatCapabilitySnapshot, DemandMatrix, assess_air_defense,
    average_attack_expectation, compute_demand, compute_unit_affinity,
};
use crate::ai::island_campaign::{
    IslandCampaignDiagnostics, IslandCampaignPortfolio, IslandCampaignShortfall,
};
use crate::ai::island_campaign_analysis::{
    analyze_island_campaign, analyze_island_campaign_excluding,
};
use crate::ai::turn_distance::{TerrainConnectivity, TurnDistanceCache, calculate_turn_distance};
use crate::components::{
    ActionCompleted, Ammo, Faction, Fuel, GridPosition, Health, PlayerId, Property, UnitStats,
};
use crate::resources::{Map, MovementType, Terrain, UnitType, master_data::MasterDataRegistry};
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};

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
    /// 敵所有拠点の座標リスト (#53: 占領部隊の奪取目標として使用)
    pub enemy_properties: std::collections::HashSet<GridPosition>,
    /// 歩兵など、ヘリでも運搬可能な軽輸送需要
    pub light_transport_demand: u32,
    /// 車両など、輸送船でしか運搬できない重輸送需要
    pub heavy_transport_demand: u32,
    /// 不足している占領ユニット数。
    pub capture_demand: u32,
    /// 包括的需要マトリクス（各戦闘カテゴリの脅威ギャップと占領脅威）。
    pub demand: DemandMatrix,
    /// 航空脅威と、期限内に交戦可能な対空戦力の不足状況。
    pub air_defense: AirDefenseAssessment,
    /// 輸送を必要としている既存ユニットのリスト（位置、ステータス、基本価値）。
    pub transport_candidates: Vec<(GridPosition, UnitStats, f32)>,
    /// 現在保有している輸送ユニットの数
    pub existing_transport_count: usize,
    /// 敵地上ユニットが存在しない平和な島にあるプロパティの座標セット
    pub peaceful_properties: std::collections::HashSet<GridPosition>,
    /// V3が毎ターン盤面から再構築する島嶼キャンペーン全体。
    pub campaign_portfolio: IslandCampaignPortfolio,
    /// 完全packageごとの予約額と不足カテゴリをproductionが優先順に消費する行。
    pub campaign_shortfalls: Vec<IslandCampaignShortfall>,
}

/// 航空脅威が残る間だけ保持する緊急対空の貯金先。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmergencyAntiAirReservation {
    pub unit_type: UnitType,
    pub cost: u32,
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
    /// 通常の生産予約を上書きせず、航空脅威が消えた時だけ解除する緊急対空予約。
    pub emergency_anti_air_reservations: HashMap<u32, EmergencyAntiAirReservation>,
}

fn is_ground_movement(movement_type: MovementType) -> bool {
    !matches!(movement_type, MovementType::Air | MovementType::Ship)
}

/// V3の島嶼作戦で海を越えられる輸送枠だけを返します。
/// 地上輸送車のcargo枠は同一島内では有効でも、海上輸送需要を満たしません。
pub(crate) fn sea_transport_capacity(unit_type: UnitType, stats: &UnitStats) -> (u32, u32) {
    sea_transport_capacity_from_slots(unit_type, stats.max_cargo)
}

pub(crate) fn sea_transport_capacity_from_slots(unit_type: UnitType, max_cargo: u32) -> (u32, u32) {
    match unit_type {
        UnitType::TransportHelicopter => (max_cargo, 0),
        UnitType::Lander => (max_cargo, max_cargo),
        _ => (0, 0),
    }
}

/// 2ユニットを戦略上の近距離交戦候補として扱えるかを判定します。
/// 地上ユニット同士だけは、互いの地形移動で接続されていない場合に除外します。
fn can_form_ground_engagement(
    map: &Map,
    registry: &MasterDataRegistry,
    my_pos: GridPosition,
    my_stats: &UnitStats,
    enemy_pos: GridPosition,
    enemy_stats: &UnitStats,
    connectivity: &mut TerrainConnectivity,
) -> bool {
    if !is_ground_movement(my_stats.movement_type) || !is_ground_movement(enemy_stats.movement_type)
    {
        return true;
    }

    connectivity.is_reachable(
        map,
        registry,
        (my_pos.x, my_pos.y),
        (enemy_pos.x, enemy_pos.y),
        my_stats.movement_type,
    ) || connectivity.is_reachable(
        map,
        registry,
        (enemy_pos.x, enemy_pos.y),
        (my_pos.x, my_pos.y),
        enemy_stats.movement_type,
    )
}

/// 敵ユニットが特定の拠点を脅かしているかを判定するヘルパー関数。
fn enemy_threatens_property(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), crate::systems::movement::OccupantInfo>,
    turn_cache: &mut TurnDistanceCache,
    player_id: PlayerId,
    prop_pos: &GridPosition,
    _prop_island_id: Option<usize>,
    enemy_pos: &GridPosition,
    enemy_stats: &UnitStats,
    _enemy_island_id: Option<usize>,
) -> bool {
    let dist = calculate_turn_distance(
        map,
        registry,
        unit_positions,
        (enemy_pos.x, enemy_pos.y),
        (prop_pos.x, prop_pos.y),
        enemy_stats.movement_type,
        enemy_stats.max_movement,
        1,
        player_id,
        turn_cache,
    );

    if dist.turns == u32::MAX {
        return false;
    }

    match enemy_stats.movement_type {
        MovementType::Air => {
            let has_weapons = enemy_stats.max_ammo1 > 0 || enemy_stats.max_ammo2 > 0;
            has_weapons && dist.turns <= 2
        }
        MovementType::Ship => {
            let has_weapons = enemy_stats.max_ammo1 > 0 || enemy_stats.max_ammo2 > 0;
            has_weapons && dist.turns <= 2
        }
        _ => dist.turns <= 2,
    }
}

/// 現在のマップ状況を分析し、最適な戦略を決定します。
pub fn analyze_strategy(world: &mut World, player_id: PlayerId) -> ProductionStrategy {
    analyze_strategy_internal(world, player_id, None, &HashSet::new())
}

/// 緊急ミッションへ予約したEntityを島嶼キャンペーンから除外して戦略を分析します。
pub(crate) fn analyze_strategy_with_reserved_entities(
    world: &mut World,
    player_id: PlayerId,
    reserved_entities: &HashSet<Entity>,
) -> ProductionStrategy {
    analyze_strategy_internal(world, player_id, None, reserved_entities)
}

/// 同一ターンのSquad計画が保存したV3キャンペーンを後続の判断へ再利用する。
/// 一時cacheがない独立呼び出しでは通常分析へfallbackする。
pub(crate) fn analyze_strategy_for_turn(
    world: &mut World,
    player_id: PlayerId,
) -> ProductionStrategy {
    let cached_campaign = world
        .get_resource::<crate::ai::engine::AiTurnStrategyCache>()
        .and_then(|cache| cache.campaign_portfolio(player_id))
        .cloned();
    analyze_strategy_internal(world, player_id, cached_campaign, &HashSet::new())
}

fn analyze_strategy_internal(
    world: &mut World,
    player_id: PlayerId,
    cached_campaign: Option<IslandCampaignPortfolio>,
    reserved_entities: &HashSet<Entity>,
) -> ProductionStrategy {
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
    let is_v3 = crate::ai::resolve_player_ai_version(world, player_id).uses_v3_tactics();
    if is_v3 {
        if let Some(cached) = cached_campaign {
            strategy.campaign_portfolio = cached;
        } else {
            // 通常分析では盤面から再構築し、同じ呼び出し内と診断出力で共有する。
            strategy.campaign_portfolio = if reserved_entities.is_empty() {
                analyze_island_campaign(world, player_id)
            } else {
                analyze_island_campaign_excluding(world, player_id, reserved_entities)
            };
            // 診断Resourceは意思決定に戻さず、最後の分析結果だけをプレイヤー別に上書きする。
            if let Some(mut diagnostics) = world.get_resource_mut::<IslandCampaignDiagnostics>() {
                diagnostics
                    .by_player
                    .insert(player_id, strategy.campaign_portfolio.clone());
            } else {
                let mut diagnostics = IslandCampaignDiagnostics::default();
                diagnostics
                    .by_player
                    .insert(player_id, strategy.campaign_portfolio.clone());
                world.insert_resource(diagnostics);
            }
        }
    }
    let mut turn_cache = TurnDistanceCache::default();
    let mut unit_positions = HashMap::new();
    let mut q_all_units = world.query::<(&Faction, &GridPosition, &UnitStats)>();
    for (faction, pos, stats) in q_all_units.iter(world) {
        unit_positions.insert(
            (pos.x, pos.y),
            crate::systems::movement::OccupantInfo {
                player_id: faction.0,
                is_transport: stats.max_cargo > 0,
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots: stats.max_cargo,
            },
        );
    }
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
                // 自軍の任意の拠点がある島IDを収集
                if let Some(island_id) = get_island_id(pos) {
                    my_base_island_ids.insert(island_id);
                }
            } else if prop.owner_id.is_none() {
                unowned_properties.push(*pos);
            } else {
                enemy_properties.push(*pos);
            }
        }
    }

    // unowned_properties / enemy_properties を strategy に保存
    strategy.unowned_properties = unowned_properties.iter().cloned().collect();
    strategy.enemy_properties = enemy_properties.iter().cloned().collect();

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

    let capability_units = {
        let mut query = world.query::<(
            &GridPosition,
            &Faction,
            &UnitStats,
            &Health,
            Option<&Ammo>,
            Option<&Fuel>,
            Option<&ActionCompleted>,
            Option<&crate::components::Transporting>,
        )>();
        query
            .iter(world)
            .filter(|(position, _, _, health, _, _, _, transporting)| {
                position.x < 9999 && health.current > 0 && transporting.is_none()
            })
            .map(
                |(position, faction, stats, health, ammo, fuel, action_completed, _)| {
                    CombatCapabilitySnapshot {
                        faction: faction.0,
                        position: *position,
                        unit_type: stats.unit_type,
                        movement_type: stats.movement_type,
                        hp: health.current,
                        cost: stats.cost,
                        max_movement: stats.max_movement,
                        min_range: stats.min_range,
                        max_range: stats.max_range,
                        ammo1: ammo.map_or(99, |ammo| ammo.ammo1),
                        max_ammo1: stats.max_ammo1,
                        ammo2: ammo.map_or(99, |ammo| ammo.ammo2),
                        max_ammo2: stats.max_ammo2,
                        fuel: fuel.map_or(u32::MAX, |fuel| fuel.current),
                        action_delay: u32::from(
                            action_completed.is_some_and(|completed| completed.0),
                        ),
                    }
                },
            )
            .collect::<Vec<_>>()
    };

    // 自軍の歩兵が存在する島IDも収集対象に加える
    for (pos, stats) in &my_units {
        #[allow(clippy::collapsible_if)]
        if stats.can_capture {
            if let Some(island_id) = get_island_id(pos) {
                my_base_island_ids.insert(island_id);
            }
        }
    }

    let mut terrain_connectivity = TerrainConnectivity::default();

    // 交戦可能性の判定。地上部隊同士は、海や進入不能地形で分断された組を除外する。
    let mut min_enemy_dist = 999;
    for (m_pos, m_stats) in &my_units {
        for (e_pos, e_stats) in &enemy_units {
            if !can_form_ground_engagement(
                &map,
                &master_data,
                *m_pos,
                m_stats,
                *e_pos,
                e_stats,
                &mut terrain_connectivity,
            ) {
                continue;
            }
            let distance = map.distance(m_pos.x, m_pos.y, e_pos.x, e_pos.y) as i32;
            if distance < min_enemy_dist {
                min_enemy_dist = distance;
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
        for (enemy_pos, enemy_stats) in &enemy_units {
            let enemy_id = player_id.opposite();
            let dist = calculate_turn_distance(
                &map,
                &master_data,
                &unit_positions,
                (enemy_pos.x, enemy_pos.y),
                (cap_pos.x, cap_pos.y),
                enemy_stats.movement_type,
                enemy_stats.max_movement,
                1,
                enemy_id,
                &mut turn_cache,
            );
            if dist.turns <= 2 {
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

            // 対空の理想構成は「敵に航空戦力が実在する」ことを条件とする（V3限定）。
            // 対空砲・地対空ミサイルは地上ユニットへ一切ダメージを与えられないため、
            // 空港のないマップ（例: map_1）で対空需要を立てると死に駒を量産し、
            // 占領・機動地上戦力の生産枠と予算を奪って自軍を弱体化させる。
            // V1/V2 は評価の基準線として従来挙動のまま維持する。
            let enemy_has_air = enemy_units
                .iter()
                .any(|(_, stats)| stats.movement_type == MovementType::Air);
            if !is_v3 || enemy_has_air {
                strategy.ideal_composition.insert(UnitType::Tank, 0.3);
                strategy.ideal_composition.insert(UnitType::AntiAir, 0.2);
            } else {
                // 対空へ割り当てるはずだった需要は、防衛の主力である戦車へ振り替える
                strategy.ideal_composition.insert(UnitType::Tank, 0.5);
            }
        }
    }

    // 占領需要の計算
    // 自軍の島に未占領・敵拠点があるかどうかにかかわらず、マップ全体で占領すべき拠点があるなら歩兵を需要とする
    let total_unowned_or_enemy = unowned_properties.len() + enemy_properties.len();
    if total_unowned_or_enemy > 0 {
        // マップ全体での占領目標に対する歩兵的理想数（マップが広ければ多い）
        let ideal_capture_units_global =
            ((total_unowned_or_enemy as f32 * 0.4).ceil() as usize).clamp(3, 10);
        let total_my_capture_units = my_units.iter().filter(|(_, s)| s.can_capture).count();

        let base_demand = ideal_capture_units_global.saturating_sub(total_my_capture_units);

        // 収入不足（自軍の島の未占領拠点が残っているのに歩兵が足りない等）の場合はさらに上乗せ
        if need_more_revenue {
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
            if is_v3 {
                let critical_sites = my_props_for_demand
                    .iter()
                    .filter(|(_, terrain)| {
                        matches!(
                            terrain,
                            Terrain::Capital
                                | Terrain::Factory
                                | Terrain::Airport
                                | Terrain::Port
                                | Terrain::City
                        )
                    })
                    .map(|(position, _)| *position)
                    .collect::<Vec<_>>();
                strategy.air_defense = assess_air_defense(
                    player_id,
                    &capability_units,
                    &critical_sites,
                    &map,
                    &master_data,
                    &unit_positions,
                    &chart,
                );
                strategy.demand.anti_air = strategy.air_defense.shortage_ratio;
            }

            // 輸送が必要なユニット（停滞ユニット）的抽出。
            // V3はallocatorが確定した全攻勢を、割当優先順位どおりの輸送先として共有する。
            let map = world.resource::<crate::resources::Map>();
            let normalization_scale = average_attack_expectation(&chart, &registry);
            let transport_targets = if is_v3 {
                strategy.campaign_portfolio.offensive_target_positions()
            } else {
                strategy.priority_targets.clone()
            };
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
                    // 実在する最寄り目標がある場合だけ輸送候補を評価し、目標なしを遠距離扱いしない。
                    let nearest_target = transport_targets
                        .iter()
                        .filter(|target| {
                            // V2 は既存の侵攻許可を維持する。V3 はallocator確定済み目標を使う。
                            is_v3
                                || island_map
                                    .get_island_at(target)
                                    .map(|island| {
                                        allowed_islands.get(&island.id).copied().unwrap_or(true)
                                    })
                                    .unwrap_or(true)
                        })
                        .map(|target| {
                            let distance = (pos.x as i32 - target.x as i32).abs()
                                + (pos.y as i32 - target.y as i32).abs();
                            (distance, *target)
                        })
                        .min_by_key(|(distance, _)| *distance);
                    let Some((min_dist, nearest_target)) = nearest_target else {
                        continue;
                    };

                    // IslandMap を使って異なる島かどうかを判定（海で遮断されているとみなす）
                    let my_island_id = get_island_id(pos);
                    let target_island_id = get_island_id(&nearest_target);
                    let mut blocked_by_sea = my_island_id.is_some()
                        && target_island_id.is_some()
                        && my_island_id != target_island_id;
                    if !blocked_by_sea {
                        // 同一島IDがない場合（海の上など）や判定できない場合は簡易パスサンプリングでフォールバック
                        let steps = 4;
                        for i in 1..steps {
                            let check_x =
                                pos.x as i32 + (nearest_target.x as i32 - pos.x as i32) * i / steps;
                            let check_y =
                                pos.y as i32 + (nearest_target.y as i32 - pos.y as i32) * i / steps;
                            if let Some(Terrain::Sea | Terrain::Shoal) =
                                map.get_terrain(check_x as usize, check_y as usize)
                            {
                                blocked_by_sea = true;
                                break;
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
                        // 価値 = (需要との一致度 * 基本コスト相当) + (占領能力ボーナス)
                        let base_value = stats.cost as f32;
                        let value = strategy.demand.dot(&affinity) * base_value
                            + (if stats.can_capture { 3000.0 } else { 0.0 });

                        strategy
                            .transport_candidates
                            .push((*pos, stats.clone(), value));
                    }
                }
            }

            let (current_light_capacity, current_heavy_capacity) = if is_v3 {
                my_units
                    .iter()
                    .fold((0_u32, 0_u32), |capacity, (_, stats)| {
                        let sea_capacity = sea_transport_capacity(stats.unit_type, stats);
                        (
                            capacity.0.saturating_add(sea_capacity.0),
                            capacity.1.saturating_add(sea_capacity.1),
                        )
                    })
            } else {
                let light = my_units
                    .iter()
                    .filter(|(_, stats)| stats.loadable_unit_types.contains(&UnitType::Infantry))
                    .map(|(_, stats)| stats.max_cargo)
                    .sum();
                let heavy = my_units
                    .iter()
                    .filter(|(_, stats)| stats.loadable_unit_types.contains(&UnitType::Tank))
                    .map(|(_, stats)| stats.max_cargo)
                    .sum();
                (light, heavy)
            };

            // V3では海を越えられる輸送だけを数え、地上輸送車による需要減衰を防ぐ。
            strategy.existing_transport_count = if is_v3 {
                my_units
                    .iter()
                    .filter(|(_, stats)| {
                        let capacity = sea_transport_capacity(stats.unit_type, stats);
                        capacity.0 > 0 || capacity.1 > 0
                    })
                    .count()
            } else {
                my_units
                    .iter()
                    .filter(|(_, stats)| stats.max_cargo > 0)
                    .count()
            };

            // 軽ユニット（歩兵・バズーカ）と重ユニット（車両）の待機数をカウント
            let light_candidates = strategy
                .transport_candidates
                .iter()
                .filter(|(_, s, _)| {
                    s.unit_type == UnitType::Infantry || s.unit_type == UnitType::Mech
                })
                .count() as u32;
            let heavy_candidates = strategy
                .transport_candidates
                .iter()
                .filter(|(_, s, _)| {
                    s.unit_type != UnitType::Infantry && s.unit_type != UnitType::Mech
                })
                .count() as u32;

            strategy.light_transport_demand = light_candidates
                .saturating_sub(current_light_capacity)
                .max(if light_candidates > 0 && current_light_capacity == 0 {
                    1
                } else {
                    0
                });
            strategy.heavy_transport_demand = heavy_candidates
                .saturating_sub(current_heavy_capacity)
                .max(if heavy_candidates > 0 && current_heavy_capacity == 0 {
                    1
                } else {
                    0
                });

            // 海を越えた島への侵攻需要（Base Invasion Demand）の計算
            let has_sea_bound_target = if is_v3 {
                !strategy.campaign_portfolio.active_offensives.is_empty()
            } else {
                strategy.priority_targets.iter().any(|target| {
                    get_island_id(target).is_some_and(|target_island_id| {
                        !my_base_island_ids.contains(&target_island_id)
                    })
                })
            };

            if has_sea_bound_target {
                // 海を越えた侵攻目標がある場合、輸送需要のベースラインを保証する
                // 現存する輸送能力が0であれば、最低1を要求する
                if current_light_capacity == 0 {
                    strategy.light_transport_demand = strategy.light_transport_demand.max(1);
                }
                if current_heavy_capacity == 0 {
                    strategy.heavy_transport_demand = strategy.heavy_transport_demand.max(1);
                }

                // さらに、乗車させる部隊の需要ベースラインも保証する
                strategy.capture_demand = strategy.capture_demand.max(2);
                strategy.demand.anti_ground = strategy.demand.anti_ground.max(0.5);
            }
        }
    }

    if is_v3 {
        let campaign_owns_role_demand = !strategy.campaign_portfolio.defenses.is_empty()
            || !strategy.campaign_portfolio.active_offensives.is_empty();
        let shortfalls = strategy.campaign_portfolio.aggregate_missing_requirements();
        let mut capture_demand = 0_u32;
        let mut light_transport_demand = 0_u32;
        let mut heavy_transport_demand = 0_u32;

        // helperが保証するpriority rank・島ID順をfieldにも保持し、productionで順次消費する。
        for shortfall in &shortfalls {
            capture_demand = capture_demand.saturating_add(shortfall.capture_units);
            light_transport_demand =
                light_transport_demand.saturating_add(shortfall.light_transport_slots);
            heavy_transport_demand =
                heavy_transport_demand.saturating_add(shortfall.heavy_transport_slots);
        }

        // assignmentが存在する間はcampaign側が役割需要を所有する。完全編成済みでshortfallが
        // 0の場合もgeneric需要へ戻さず、同じ役割の重複生産を防ぐ。
        if campaign_owns_role_demand {
            strategy.capture_demand = capture_demand;
            strategy.light_transport_demand = light_transport_demand;
            strategy.heavy_transport_demand = heavy_transport_demand;
        }
        strategy.campaign_shortfalls = shortfalls;
    }

    // 平和な拠点（敵ユニットから十分に離れており、脅かされていない拠点）の分析
    let mut peaceful_properties = std::collections::HashSet::new();

    for pos in &my_properties {
        let prop_island_id = get_island_id(pos);
        let mut is_peaceful = true;

        for (e_pos, e_stats) in &enemy_units {
            let e_island_id = get_island_id(e_pos);
            let enemy_id = player_id.opposite();
            if enemy_threatens_property(
                &map,
                &master_data,
                &unit_positions,
                &mut turn_cache,
                enemy_id,
                pos,
                prop_island_id,
                e_pos,
                e_stats,
                e_island_id,
            ) {
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
    fn v3_sea_transport_capacity_excludes_ground_carriers() {
        let recon = UnitStats {
            unit_type: UnitType::Recon,
            max_cargo: 1,
            ..UnitStats::mock()
        };
        let helicopter = UnitStats {
            unit_type: UnitType::TransportHelicopter,
            max_cargo: 2,
            ..UnitStats::mock()
        };
        let lander = UnitStats {
            unit_type: UnitType::Lander,
            max_cargo: 2,
            ..UnitStats::mock()
        };

        assert_eq!(sea_transport_capacity(UnitType::Recon, &recon), (0, 0));
        assert_eq!(
            sea_transport_capacity(UnitType::TransportHelicopter, &helicopter),
            (2, 0)
        );
        assert_eq!(sea_transport_capacity(UnitType::Lander, &lander), (2, 2));
    }

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
        world.insert_resource(crate::resources::MasterDataRegistry::load().unwrap());
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
                max_movement: 6,
                movement_type: crate::resources::MovementType::Tank,
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
                max_movement: 6,
                movement_type: crate::resources::MovementType::Tank,
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

    /// Defense フェーズの理想構成テスト用ワールド。
    /// 首都のすぐ隣に敵戦車を置いて Defense フェーズを成立させ、
    /// `enemy_has_air` が真のときだけ敵の航空ユニットを追加する。
    fn setup_defense_phase_world(
        ai_version: crate::ai::ai_version::AiVersion,
        enemy_has_air: bool,
    ) -> World {
        let mut world = World::new();
        world.insert_resource(Map::new(15, 15, Terrain::Plains, GridTopology::Square));
        world.insert_resource(crate::resources::MasterDataRegistry::load().unwrap());
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        let mut settings = crate::ai::ai_version::PlayerAiSettings::default();
        settings.set_version(p1, ai_version);
        world.insert_resource(settings);

        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));

        // 首都のすぐそばに敵地上ユニット（Defense フェーズのトリガー）
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Faction(p2),
            UnitStats {
                unit_type: UnitType::Tank,
                max_movement: 6,
                movement_type: crate::resources::MovementType::Tank,
                ..UnitStats::mock()
            },
        ));

        if enemy_has_air {
            world.spawn((
                GridPosition { x: 5, y: 5 },
                Faction(p2),
                UnitStats {
                    unit_type: UnitType::Bcopters,
                    max_movement: 6,
                    movement_type: crate::resources::MovementType::Air,
                    ..UnitStats::mock()
                },
            ));
        }

        world
    }

    /// 敵に航空戦力が1機も存在しない場合、Defense フェーズでも対空を理想構成へ要求しないこと。
    /// map_1 のように空港のないマップでは、対空ユニットは地上戦力へ一切ダメージを与えられず
    /// 純粋な死に駒になるため、脅威がないのに需要を立ててはならない。
    #[test]
    fn v3_defense_composition_omits_anti_air_without_enemy_air() {
        let mut world = setup_defense_phase_world(crate::ai::ai_version::AiVersion::V3, false);
        let strategy = analyze_strategy(&mut world, PlayerId(1));

        assert_eq!(strategy.phase, GamePhase::Defense);
        assert!(
            !strategy.ideal_composition.contains_key(&UnitType::AntiAir),
            "敵に航空戦力がない場合、対空を理想構成へ含めてはならない: {:?}",
            strategy.ideal_composition
        );
    }

    /// 敵に航空戦力が存在する場合は、従来どおり Defense フェーズで対空需要を立てること。
    #[test]
    fn v3_defense_composition_demands_anti_air_against_enemy_air() {
        let mut world = setup_defense_phase_world(crate::ai::ai_version::AiVersion::V3, true);
        let strategy = analyze_strategy(&mut world, PlayerId(1));

        assert_eq!(strategy.phase, GamePhase::Defense);
        assert!(
            strategy
                .ideal_composition
                .get(&UnitType::AntiAir)
                .copied()
                .unwrap_or(0.0)
                > 0.0,
            "敵航空戦力が実在する場合は対空需要を立てるべき: {:?}",
            strategy.ideal_composition
        );
    }

    /// V1/V2 は評価の基準線として従来挙動を維持する（今回の是正は V3 限定）。
    #[test]
    fn v1_defense_composition_keeps_legacy_anti_air_demand() {
        let mut world = setup_defense_phase_world(crate::ai::ai_version::AiVersion::V1, false);
        let strategy = analyze_strategy(&mut world, PlayerId(1));

        assert_eq!(strategy.phase, GamePhase::Defense);
        assert_eq!(
            strategy.ideal_composition.get(&UnitType::AntiAir).copied(),
            Some(0.2),
            "V1 の理想構成は変更しない"
        );
    }

    /// 海を越えた島に攻略目標がある場合、自軍の地上ユニットが0であっても
    /// 輸送および地上部隊の最低需要（Base Invasion Demand）が発生することを確認するテスト
    #[test]
    fn test_analyze_strategy_sea_bound_invasion_demand() {
        let mut world = World::new();
        let master_data = crate::resources::MasterDataRegistry::load().unwrap();

        // 1. DamageChart & UnitRegistry の手動構築と登録
        let mut damage_chart = crate::resources::DamageChart::new();
        for (unit_name, unit_record) in &master_data.units {
            let att_type = master_data.unit_type_for_name(&unit_name.0).unwrap();
            if let Some(w1_name) = &unit_record.weapon1 {
                let weapon = master_data
                    .weapons
                    .get(&crate::resources::master_data::UnitName(w1_name.clone()))
                    .unwrap();
                for (def_name, dmg) in &weapon.damages {
                    let def_type = master_data.unit_type_for_name(def_name).unwrap();
                    damage_chart.insert_damage(att_type, def_type, *dmg);
                }
            }
            if let Some(w2_name) = &unit_record.weapon2 {
                let weapon = master_data
                    .weapons
                    .get(&crate::resources::master_data::UnitName(w2_name.clone()))
                    .unwrap();
                for (def_name, dmg) in &weapon.damages {
                    let def_type = master_data.unit_type_for_name(def_name).unwrap();
                    damage_chart.insert_secondary_damage(att_type, def_type, *dmg);
                }
            }
        }
        world.insert_resource(damage_chart);

        let mut unit_registry_map = std::collections::HashMap::new();
        for name in master_data.units.keys() {
            let stats = master_data.create_unit_stats(name).unwrap();
            unit_registry_map.insert(stats.unit_type, stats);
        }
        world.insert_resource(crate::resources::UnitRegistry(unit_registry_map));
        world.insert_resource(master_data.clone());

        // 2. マップと島情報の構築
        let mut map = Map::new(10, 10, Terrain::Sea, GridTopology::Square);
        // 島A (0,0)〜(2,2)
        for x in 0..=2 {
            for y in 0..=2 {
                let _ = map.set_terrain(x, y, Terrain::Plains);
            }
        }
        // 島B (7,7)〜(9,9)
        for x in 7..=9 {
            for y in 7..=9 {
                let _ = map.set_terrain(x, y, Terrain::Plains);
            }
        }
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        world.insert_resource(map);
        world.insert_resource(island_map);

        let p1 = PlayerId(1);
        // このテストはV2の汎用的な海越え需要を検証するため、バージョンを明示します。
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(p1, crate::ai::AiVersion::V2);
        world.insert_resource(settings);

        // 島Aに自軍の首都を配置
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));

        // 島Bに未占領の工場（攻略目標）を配置
        world.spawn((
            GridPosition { x: 8, y: 8 },
            Property::new(Terrain::Factory, None, 100),
        ));

        // 自軍ユニットは0、敵ユニットも0とする

        let strategy = analyze_strategy(&mut world, p1);

        // 海を隔てた未攻略の島が存在するため、最低需要が発生しているはず
        assert!(
            strategy.light_transport_demand >= 1,
            "海越え侵攻需要により light_transport_demand は1以上になるべきだが、実際は {}",
            strategy.light_transport_demand
        );
        assert!(
            strategy.heavy_transport_demand >= 1,
            "海越え侵攻需要により heavy_transport_demand は1以上になるべきだが、実際は {}",
            strategy.heavy_transport_demand
        );
        assert!(
            strategy.capture_demand >= 2,
            "海越え侵攻需要により capture_demand は2以上になるべきだが、実際は {}",
            strategy.capture_demand
        );
        assert!(
            strategy.demand.anti_ground >= 0.5,
            "海越え侵攻需要により demand.anti_ground は0.5以上になるべきだが、実際は {}",
            strategy.demand.anti_ground
        );
    }

    fn setup_separated_units(
        friendly_movement: MovementType,
        enemy_movement: MovementType,
        separator: Terrain,
    ) -> World {
        let mut world = World::new();
        let master_data = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(1, 0, separator).unwrap();
        world.insert_resource(map.clone());
        world.insert_resource(master_data);
        world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Faction(p1),
            UnitStats {
                unit_type: if friendly_movement == MovementType::Air {
                    UnitType::Fighter
                } else if friendly_movement == MovementType::Ship {
                    UnitType::Battleship
                } else {
                    UnitType::Infantry
                },
                movement_type: friendly_movement,
                max_movement: 3,
                max_range: 1,
                ..UnitStats::mock()
            },
        ));
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Faction(p2),
            UnitStats {
                unit_type: if enemy_movement == MovementType::Air {
                    UnitType::Fighter
                } else if enemy_movement == MovementType::Ship {
                    UnitType::Battleship
                } else {
                    UnitType::Infantry
                },
                movement_type: enemy_movement,
                max_movement: 3,
                max_range: 1,
                ..UnitStats::mock()
            },
        ));
        world
    }

    #[test]
    fn ground_units_across_sea_are_not_engaged() {
        let mut world =
            setup_separated_units(MovementType::Infantry, MovementType::Infantry, Terrain::Sea);
        assert_eq!(
            analyze_strategy(&mut world, PlayerId(1)).phase,
            GamePhase::Expansion
        );
    }

    #[test]
    fn air_or_ship_across_sea_remains_engaged() {
        for movement in [MovementType::Air, MovementType::Ship] {
            let mut world = setup_separated_units(movement, MovementType::Infantry, Terrain::Sea);
            assert_eq!(
                analyze_strategy(&mut world, PlayerId(1)).phase,
                GamePhase::Contested
            );
        }
    }

    #[test]
    fn shoal_does_not_connect_ground_engagement() {
        let mut world = setup_separated_units(
            MovementType::Infantry,
            MovementType::Infantry,
            Terrain::Shoal,
        );
        let island_count = world
            .resource::<crate::ai::islands::IslandMap>()
            .islands
            .len();
        assert_eq!(island_count, 1, "IslandMap 上は浅瀬で同じ島になる前提");
        assert_eq!(
            analyze_strategy(&mut world, PlayerId(1)).phase,
            GamePhase::Expansion
        );
    }

    fn setup_v3_portfolio_world(reverse_spawn: bool, funds: u32) -> World {
        let mut world = World::new();
        let master_data = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(7, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Airport).unwrap();
        map.set_terrain(3, 0, Terrain::Factory).unwrap();
        map.set_terrain(6, 0, Terrain::Capital).unwrap();
        world.insert_resource(map.clone());
        world.insert_resource(master_data);
        world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
        world.insert_resource(crate::resources::Players(vec![crate::resources::Player {
            id: PlayerId(1),
            name: "P1".to_owned(),
            funds,
        }]));
        let mut settings = crate::ai::ai_version::PlayerAiSettings::default();
        settings.set_version(PlayerId(1), crate::ai::ai_version::AiVersion::V3);
        world.insert_resource(settings);

        let properties = [
            (
                GridPosition { x: 0, y: 0 },
                Property::new(Terrain::Airport, Some(PlayerId(1)), 100),
            ),
            (
                GridPosition { x: 3, y: 0 },
                Property::new(Terrain::Factory, None, 100),
            ),
            (
                GridPosition { x: 6, y: 0 },
                Property::new(Terrain::Capital, Some(PlayerId(2)), 100),
            ),
        ];
        if reverse_spawn {
            for property in properties.into_iter().rev() {
                world.spawn(property);
            }
        } else {
            for property in properties {
                world.spawn(property);
            }
        }
        world
    }

    fn install_strategy_scoring_resources(world: &mut World) {
        let master_data = world.resource::<MasterDataRegistry>().clone();
        let mut damage_chart = crate::resources::DamageChart::new();
        let mut unit_registry = std::collections::HashMap::new();
        for (name, record) in &master_data.units {
            let unit_type = master_data.unit_type_for_name(&name.0).unwrap();
            let stats = master_data.create_unit_stats(name).unwrap();
            unit_registry.insert(unit_type, stats);
            for weapon_name in [&record.weapon1, &record.weapon2].into_iter().flatten() {
                let weapon = master_data
                    .weapons
                    .get(&crate::resources::master_data::UnitName(
                        weapon_name.clone(),
                    ))
                    .unwrap();
                for (defender_name, damage) in &weapon.damages {
                    let defender_type = master_data.unit_type_for_name(defender_name).unwrap();
                    damage_chart.insert_damage(unit_type, defender_type, *damage);
                }
            }
        }
        world.insert_resource(damage_chart);
        world.insert_resource(crate::resources::UnitRegistry(unit_registry));
    }

    #[test]
    fn v3_recon_does_not_satisfy_sea_transport_demand() {
        let mut world = setup_v3_portfolio_world(false, 6_000);
        install_strategy_scoring_resources(&mut world);
        let recon = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Recon.as_str().to_owned(),
            ))
            .unwrap();
        assert!(
            recon.max_cargo > 0,
            "fixture must reproduce the ground-carrier bug"
        );
        world.spawn((
            Faction(PlayerId(1)),
            GridPosition { x: 0, y: 0 },
            recon,
            crate::components::Health {
                current: 100,
                max: 100,
            },
        ));

        let strategy = analyze_strategy(&mut world, PlayerId(1));

        assert_eq!(strategy.existing_transport_count, 0);
        assert_eq!(strategy.light_transport_demand, 2);
    }

    #[test]
    fn v3_strategy_assesses_every_island() {
        let mut world = setup_v3_portfolio_world(false, 6_000);
        let island_count = world
            .resource::<crate::ai::islands::IslandMap>()
            .islands
            .len();

        let strategy = analyze_strategy(&mut world, PlayerId(1));

        assert_eq!(strategy.campaign_portfolio.islands.len(), island_count);
    }

    #[test]
    fn v3_strategy_updates_only_active_players_campaign_diagnostics() {
        let mut world = setup_v3_portfolio_world(false, 6_000);
        let preserved = IslandCampaignPortfolio::default();
        let mut diagnostics = IslandCampaignDiagnostics::default();
        diagnostics.by_player.insert(PlayerId(2), preserved.clone());
        world.insert_resource(diagnostics);

        let strategy = analyze_strategy(&mut world, PlayerId(1));
        let diagnostics = world.resource::<IslandCampaignDiagnostics>();

        assert_eq!(
            diagnostics.by_player.get(&PlayerId(1)),
            Some(&strategy.campaign_portfolio)
        );
        assert_eq!(diagnostics.by_player.get(&PlayerId(2)), Some(&preserved));
    }

    #[test]
    fn v3_turn_strategy_reuses_campaign_cache() {
        let mut world = setup_v3_portfolio_world(false, 6_000);
        let cached = IslandCampaignPortfolio::default();
        let mut cache = crate::ai::engine::AiTurnStrategyCache::default();
        cache.set_campaign_portfolio(PlayerId(1), cached.clone());
        world.insert_resource(cache);

        let strategy = analyze_strategy_for_turn(&mut world, PlayerId(1));

        assert_eq!(strategy.campaign_portfolio, cached);
    }

    #[test]
    fn v3_turn_strategy_preserves_generic_capture_demand_without_campaign_assignments() {
        let mut world = setup_v3_portfolio_world(false, 6_000);
        let cached = IslandCampaignPortfolio::default();
        let mut cache = crate::ai::engine::AiTurnStrategyCache::default();
        cache.set_campaign_portfolio(PlayerId(1), cached);
        world.insert_resource(cache);

        let strategy = analyze_strategy_for_turn(&mut world, PlayerId(1));

        assert!(strategy.campaign_portfolio.active_offensives.is_empty());
        assert!(strategy.campaign_portfolio.defenses.is_empty());
        assert!(strategy.campaign_shortfalls.is_empty());
        assert_eq!(strategy.capture_demand, 3);
    }

    #[test]
    fn v3_cached_empty_portfolio_does_not_create_targetless_transport_demand() {
        let mut world = setup_v3_portfolio_world(false, 6_000);
        install_strategy_scoring_resources(&mut world);
        let infantry = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        world.spawn((Faction(PlayerId(1)), GridPosition { x: 0, y: 0 }, infantry));
        let cached = IslandCampaignPortfolio::default();
        let mut cache = crate::ai::engine::AiTurnStrategyCache::default();
        cache.set_campaign_portfolio(PlayerId(1), cached);
        world.insert_resource(cache);

        let strategy = analyze_strategy_for_turn(&mut world, PlayerId(1));

        assert!(
            strategy
                .campaign_portfolio
                .offensive_target_positions()
                .is_empty()
        );
        assert!(strategy.transport_candidates.is_empty());
        assert_eq!(strategy.light_transport_demand, 0);
        assert_eq!(strategy.heavy_transport_demand, 0);
        assert_eq!(strategy.capture_demand, 2);
    }

    #[test]
    fn v3_turn_strategy_zeroes_generic_demands_for_fully_staffed_campaign() {
        let mut world = setup_v3_portfolio_world(false, 6_000);
        install_strategy_scoring_resources(&mut world);
        let infantry = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        world.spawn((Faction(PlayerId(1)), GridPosition { x: 0, y: 0 }, infantry));
        let target = GridPosition { x: 3, y: 0 };
        let island_id = world
            .resource::<crate::ai::islands::IslandMap>()
            .get_island_at(&target)
            .unwrap()
            .id;
        let empty_requirement = crate::ai::island_campaign::IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            combat_budget: 0,
            total_budget: 0,
        };
        let assignment = crate::ai::island_campaign::IslandCampaignAssignment {
            island_id,
            decision: crate::ai::island_campaign::IslandCampaignDecision::Expand,
            target_position: target,
            requirement: empty_requirement.clone(),
            purchase_shortfall: empty_requirement,
            allocated_budget: 0,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let cached = IslandCampaignPortfolio {
            islands: Vec::new(),
            active_offensives: vec![assignment],
            defenses: Vec::new(),
        };
        let mut cache = crate::ai::engine::AiTurnStrategyCache::default();
        cache.set_campaign_portfolio(PlayerId(1), cached);
        world.insert_resource(cache);

        let strategy = analyze_strategy_for_turn(&mut world, PlayerId(1));

        assert!(strategy.campaign_shortfalls.is_empty());
        assert_eq!(strategy.capture_demand, 0);
        assert_eq!(strategy.light_transport_demand, 0);
        assert_eq!(strategy.heavy_transport_demand, 0);
    }

    #[test]
    fn v3_portfolio_is_independent_of_spawn_order() {
        let mut normal = setup_v3_portfolio_world(false, 6_000);
        let mut reversed = setup_v3_portfolio_world(true, 6_000);

        let normal_portfolio = analyze_strategy(&mut normal, PlayerId(1)).campaign_portfolio;
        let reversed_portfolio = analyze_strategy(&mut reversed, PlayerId(1)).campaign_portfolio;
        let normal_assignment_ids: Vec<_> = normal_portfolio
            .active_offensives
            .iter()
            .map(|assignment| assignment.island_id)
            .collect();
        let reversed_assignment_ids: Vec<_> = reversed_portfolio
            .active_offensives
            .iter()
            .map(|assignment| assignment.island_id)
            .collect();

        assert_eq!(normal_portfolio.islands, reversed_portfolio.islands);
        assert_eq!(normal_assignment_ids, reversed_assignment_ids);
    }

    #[test]
    fn v3_funds_open_neutral_before_unaffordable_enemy_assault() {
        let mut world = setup_v3_portfolio_world(false, 6_000);
        let island_map = world.resource::<crate::ai::islands::IslandMap>().clone();
        let neutral_island = island_map
            .get_island_at(&GridPosition { x: 3, y: 0 })
            .unwrap()
            .id;
        let enemy_island = island_map
            .get_island_at(&GridPosition { x: 6, y: 0 })
            .unwrap()
            .id;

        let strategy = analyze_strategy(&mut world, PlayerId(1));
        let portfolio = &strategy.campaign_portfolio;

        assert_eq!(strategy.capture_demand, 2);
        assert_eq!(strategy.light_transport_demand, 2);
        assert_eq!(strategy.heavy_transport_demand, 0);
        assert_eq!(
            portfolio
                .assignment_for(neutral_island)
                .map(|assignment| assignment.decision),
            Some(crate::ai::island_campaign::IslandCampaignDecision::Expand)
        );
        assert_eq!(
            portfolio
                .islands
                .iter()
                .find(|assessment| assessment.island_id == enemy_island)
                .map(|assessment| assessment.decision),
            Some(crate::ai::island_campaign::IslandCampaignDecision::Observe)
        );
    }

    #[test]
    fn v3_enemy_held_below_minimum_budget_remains_observe() {
        let mut world = setup_v3_portfolio_world(false, 32_699);
        let island_map = world.resource::<crate::ai::islands::IslandMap>().clone();
        let enemy_island = island_map
            .get_island_at(&GridPosition { x: 6, y: 0 })
            .unwrap()
            .id;

        let portfolio = analyze_strategy(&mut world, PlayerId(1)).campaign_portfolio;
        let assessment = portfolio
            .islands
            .iter()
            .find(|assessment| assessment.island_id == enemy_island)
            .unwrap();

        assert_eq!(
            assessment.decision,
            crate::ai::island_campaign::IslandCampaignDecision::Observe
        );
        assert!(portfolio.assignment_for(enemy_island).is_none());
    }
}
