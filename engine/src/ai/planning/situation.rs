//! 状況判断: 盤面snapshotから戦略フェーズ・物件の区分・首都脅威・収入要否を求める。
//!
//! 部隊再編と生産判断の両方がこの結果を使う。生産需要（構成比・輸送・対空など）は
//! ここでは計算せず、生産側（[`crate::ai::strategy`]）が状況判断の結果から求める。
//! Worldは読み書きしない。

use crate::ai::context::board::PlayerBoard;
use crate::ai::strategy::GamePhase;
use crate::ai::turn_distance::{TerrainConnectivity, TurnDistanceCache, calculate_turn_distance};
use crate::collections::HashSet;
use crate::components::{GridPosition, UnitStats};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Map, MovementType, Terrain};

/// 状況判断の結果。
#[derive(Debug, Clone, Default)]
pub(crate) struct StrategicSituation {
    pub phase: GamePhase,
    /// 未占領（中立）物件。
    pub unowned_properties: HashSet<GridPosition>,
    /// 敵所有物件。
    pub enemy_properties: HashSet<GridPosition>,
    /// フェーズに応じて並べた戦略目標（未占領・敵物件・自首都）。
    pub priority_targets: Vec<GridPosition>,
    /// 自軍物件の座標と物件地形（列挙順）。
    pub my_properties: Vec<(GridPosition, Terrain)>,
    /// 自軍物件か占領可能unitのある島。
    pub my_base_island_ids: HashSet<usize>,
    /// 自島の中立物件が残り、占領要員が目標未満か。
    pub need_more_revenue: bool,
    /// 地図上の自軍・敵軍unit（搭載中・マップ外を除く。列挙順）。
    pub my_units: Vec<(GridPosition, UnitStats)>,
    pub enemy_units: Vec<(GridPosition, UnitStats)>,
}

/// 盤面から状況を判断する。
pub(crate) fn assess(board: &PlayerBoard) -> StrategicSituation {
    let player_id = board.player;
    let map = &board.map;
    let registry = &board.registry;
    let island_map = &board.island_map;
    let get_island_id =
        |pos: &GridPosition| -> Option<usize> { island_map.get_island_at(pos).map(|i| i.id.0) };

    // 1. 拠点の分析
    let mut unowned_properties = Vec::new();
    let mut enemy_properties = Vec::new();
    let mut my_properties = Vec::new();
    // 自首都は列挙順で最後に見つかったもの。
    let mut my_capital_pos = None;
    let mut my_base_island_ids = HashSet::default();
    for property in &board.properties {
        let pos = &property.position;
        if property.owner == Some(player_id) {
            my_properties.push((*pos, property.terrain));
            if property.terrain == Terrain::Capital {
                my_capital_pos = Some(*pos);
            }
            // 自軍の任意の拠点がある島IDを収集
            if let Some(island_id) = get_island_id(pos) {
                my_base_island_ids.insert(island_id);
            }
        } else if property.owner.is_none() {
            unowned_properties.push(*pos);
        } else {
            enemy_properties.push(*pos);
        }
    }

    // 2. ユニットの分析（マップ外・輸送機内のユニットは距離計算などの分析から除外）
    let mut my_units = Vec::new();
    let mut enemy_units = Vec::new();
    for unit in board.units.iter().filter(|unit| unit.is_deployed()) {
        if unit.owner == player_id {
            my_units.push((unit.position, unit.stats.clone()));
        } else {
            enemy_units.push((unit.position, unit.stats.clone()));
        }
    }

    // 自軍の歩兵が存在する島IDも収集対象に加える
    for (pos, stats) in &my_units {
        if stats.can_capture
            && let Some(island_id) = get_island_id(pos)
        {
            my_base_island_ids.insert(island_id);
        }
    }

    let is_engaged = is_engaged(map, registry, &my_units, &enemy_units);
    let capital_threatened =
        my_capital_pos.is_some_and(|capital| capital_is_threatened(board, capital, &enemy_units));

    // 3. 島嶼（IslandMap）と拠点獲得劣勢（収入不足）の分析
    let on_base_island = |pos: &GridPosition| {
        get_island_id(pos).is_some_and(|island_id| my_base_island_ids.contains(&island_id))
    };
    let island_unowned_properties = unowned_properties
        .iter()
        .filter(|pos| on_base_island(pos))
        .count();
    let island_enemy_properties = enemy_properties
        .iter()
        .filter(|pos| on_base_island(pos))
        .count();
    let island_my_capture_units = my_units
        .iter()
        .filter(|(pos, stats)| stats.can_capture && on_base_island(pos))
        .count();

    // 島内の拠点数に基づく目標占領ユニット数
    let total_island_properties = island_unowned_properties + island_enemy_properties;
    let ideal_capture_units = ((total_island_properties as f32 * 0.5).ceil() as usize).clamp(3, 10);

    // 収入（拠点確保）優先判定: 島内に中立拠点が残っており、歩兵が目標未満である場合
    let need_more_revenue =
        island_unowned_properties > 0 && island_my_capture_units < ideal_capture_units;

    // 4. フェーズの判定
    let phase = if capital_threatened {
        GamePhase::Defense
    } else if need_more_revenue {
        GamePhase::Expansion
    } else if is_engaged {
        if enemy_units.len() >= my_units.len() {
            GamePhase::Contested
        } else {
            GamePhase::Assault
        }
    } else {
        GamePhase::Expansion
    };

    // ターゲットの統合: フェーズに関わらず、中立拠点と敵拠点の両方を考慮する
    // ただしフェーズによって重みを変えるために、ここではリストの順序や内容を調整
    let priority_targets = match phase {
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

    StrategicSituation {
        phase,
        unowned_properties: unowned_properties.into_iter().collect(),
        enemy_properties: enemy_properties.into_iter().collect(),
        priority_targets,
        my_properties,
        my_base_island_ids,
        need_more_revenue,
        my_units,
        enemy_units,
    }
}

fn is_ground_movement(movement_type: MovementType) -> bool {
    !matches!(movement_type, MovementType::Air | MovementType::Ship)
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

/// 交戦中か: 交戦しうる最寄りの敵が、自軍の平均到達距離（移動力＋射程）＋1以内に居る。
/// 地上部隊同士は、海や進入不能地形で分断された組を除外する。
fn is_engaged(
    map: &Map,
    registry: &MasterDataRegistry,
    my_units: &[(GridPosition, UnitStats)],
    enemy_units: &[(GridPosition, UnitStats)],
) -> bool {
    let mut terrain_connectivity = TerrainConnectivity::default();
    let mut min_enemy_dist = 999;
    for (m_pos, m_stats) in my_units {
        for (e_pos, e_stats) in enemy_units {
            if !can_form_ground_engagement(
                map,
                registry,
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

    min_enemy_dist <= (avg_engagement_range + 1) as i32
}

/// 首都が脅かされているか: 2ターン以内に首都へ届く敵unitが居る。
fn capital_is_threatened(
    board: &PlayerBoard,
    capital: GridPosition,
    enemy_units: &[(GridPosition, UnitStats)],
) -> bool {
    // 戦略分析の占有モデル（搭載中を含み、空き枠は最大積載数）。
    let unit_positions = board.strategic_occupancy();
    let mut turn_cache = TurnDistanceCache::default();
    let enemy_id = board.player.opposite();
    enemy_units.iter().any(|(enemy_pos, enemy_stats)| {
        calculate_turn_distance(
            &board.map,
            &board.registry,
            &unit_positions,
            (enemy_pos.x, enemy_pos.y),
            (capital.x, capital.y),
            enemy_stats.movement_type,
            enemy_stats.max_movement,
            1,
            enemy_id,
            &mut turn_cache,
        )
        .turns
            <= 2
    })
}
