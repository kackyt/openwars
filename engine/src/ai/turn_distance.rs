#![allow(clippy::too_many_arguments)]
#![allow(clippy::manual_div_ceil)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::manual_checked_ops)]

use crate::components::{GridPosition, PlayerId};
use crate::resources::{Map, MovementType, master_data::MasterDataRegistry};
use crate::systems::movement::{OccupantInfo, get_valid_movement_cost};
use bevy_ecs::prelude::*;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

pub type TurnCacheKey = (usize, usize, usize, usize, MovementType, u32, PlayerId);

/// ターン数ベースの距離計算をキャッシュするためのリソース
#[derive(Resource, Default)]
pub struct TurnDistanceCache {
    /// キー: (出発地x, 出発地y, 目標地x, 目標地y, 移動タイプ, 移動力, 勢力ID)
    /// 値: 到達ターン数 (到達不可の場合は u32::MAX)
    pub cache: HashMap<TurnCacheKey, u32>,
}

impl TurnDistanceCache {
    pub fn clear(&mut self) {
        self.cache.clear();
    }
}

/// A* / Dijkstra の状態
#[derive(Copy, Clone, Eq, PartialEq)]
struct State {
    cost: u32,
    position: (usize, usize),
}

impl Ord for State {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .cost
            .cmp(&self.cost)
            .then_with(|| self.position.cmp(&other.position))
    }
}

impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// 指定した2地点間のターン数距離を計算します。
/// ターゲットが移動タイプ的に進入不可の場合（船→陸地など）、
/// ターゲットに隣接する進入可能なマスまでの最短ターン数を返します。
/// 到達不可能な場合は u32::MAX を返します。
pub fn calculate_turn_distance(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    start: (usize, usize),
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    player_id: PlayerId,
    cache: &mut TurnDistanceCache,
) -> u32 {
    if start == target {
        return 0;
    }

    let cache_key = (
        start.0,
        start.1,
        target.0,
        target.1,
        movement_type,
        max_mp,
        player_id,
    );
    if let Some(&dist) = cache.cache.get(&cache_key) {
        return dist;
    }

    // ターゲットの地形が進入不可の場合（船→陸地など）、
    // ターゲットに隣接する進入可能なマスへのルートに切り替える
    let target_passable = map
        .get_terrain(target.0, target.1)
        .and_then(|t| get_valid_movement_cost(registry, movement_type, t))
        .is_some();

    // 実際の到達目標タイル群を決定する
    // ターゲットが進入可能 → そのまま [target]
    // ターゲットが進入不可 → ターゲットに隣接する進入可能なタイル群
    let effective_targets: Vec<(usize, usize)> = if target_passable {
        vec![target]
    } else {
        let mut adj_targets = Vec::new();
        for adj in map.get_adjacent(target.0, target.1) {
            if let Some(adj_terrain) = map.get_terrain(adj.0, adj.1) {
                if get_valid_movement_cost(registry, movement_type, adj_terrain).is_some() {
                    adj_targets.push(adj);
                }
            }
        }
        adj_targets
    };

    if effective_targets.is_empty() {
        // ターゲットに隣接する進入可能タイルが存在しない → 到達不可
        let dx = (start.0 as i32 - target.0 as i32).abs();
        let dy = (start.1 as i32 - target.1 as i32).abs();
        let approx = 50 + ((dx + dy) as u32 / 4);
        cache.cache.insert(cache_key, approx);
        return approx;
    }

    // スタート地点がいずれかの到達目標に一致する場合
    for &et in &effective_targets {
        if start == et {
            cache.cache.insert(cache_key, 0);
            return 0;
        }
    }

    // ダイクストラ法でスタートから各タイルへの最短移動コストを計算
    let mut dist = HashMap::new();
    let mut heap = BinaryHeap::new();

    dist.insert(start, 0u32);
    heap.push(State {
        cost: 0,
        position: start,
    });

    while let Some(State { cost, position }) = heap.pop() {
        // いずれかの到達目標に到達した
        if effective_targets.contains(&position) {
            let turns = if max_mp == 0 {
                u32::MAX
            } else {
                (cost + max_mp - 1) / max_mp // ceil(cost / max_mp)
            };
            cache.cache.insert(cache_key, turns);
            return turns;
        }

        if cost > *dist.get(&position).unwrap_or(&u32::MAX) {
            continue;
        }

        for next_pos in map.get_adjacent(position.0, position.1) {
            let Some(next_terrain) = map.get_terrain(next_pos.0, next_pos.1) else {
                continue;
            };

            let Some(move_cost) = get_valid_movement_cost(registry, movement_type, next_terrain)
            else {
                continue;
            };

            // 敵ユニットによるゾック（通行不可）判定
            // ただし目標地点に敵がいる場合は攻撃可能なので通行可能とみなす
            if !effective_targets.contains(&next_pos) {
                if let Some(occupant) = unit_positions.get(&next_pos) {
                    if occupant.player_id != player_id {
                        continue; // 敵がいるマスは通過不可
                    }
                }
            }

            let next_cost = cost + move_cost;
            let current_best = *dist.get(&next_pos).unwrap_or(&u32::MAX);

            if next_cost < current_best {
                dist.insert(next_pos, next_cost);
                heap.push(State {
                    cost: next_cost,
                    position: next_pos,
                });
            }
        }
    }

    // 完全に到達不可な場合でも、マンハッタン距離ベースの近似値を返す
    // 遠すぎる値にするとスコア計算で差分が出なくなるため、50 + 距離/4 程度に留める
    let dx = (start.0 as i32 - target.0 as i32).abs();
    let dy = (start.1 as i32 - target.1 as i32).abs();
    let approx = 50 + ((dx + dy) as u32 / 4);

    cache.cache.insert(cache_key, approx);
    approx
}

/// 始点（start）からマップ上のすべての到達可能な座標への最短到達ターン数をダイクストラ法で一括計算し、
/// HashMap として返します。
pub fn calculate_all_turn_distances(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    interaction_max_range: u32,
    player_id: PlayerId,
) -> HashMap<GridPosition, u32> {
    let mut dist = HashMap::new();
    let mut heap = BinaryHeap::new();
    let mut turns_map = HashMap::new();

    if max_mp == 0 {
        return turns_map;
    }

    // ターゲットからインタラクション可能な（射程内の）全ての進入可能タイルを始点として登録
    for dx in -(interaction_max_range as i32)..=(interaction_max_range as i32) {
        for dy in -(interaction_max_range as i32)..=(interaction_max_range as i32) {
            let m_dist = dx.abs() + dy.abs();
            if m_dist > interaction_max_range as i32 {
                continue;
            }

            let nx = target.0 as i32 + dx;
            let ny = target.1 as i32 + dy;
            if nx >= 0 && nx < map.width as i32 && ny >= 0 && ny < map.height as i32 {
                let pos = (nx as usize, ny as usize);
                if let Some(terrain) = map.get_terrain(pos.0, pos.1) {
                    if get_valid_movement_cost(registry, movement_type, terrain).is_some() {
                        let mut can_enter = true;
                        // 目標地点そのもの以外で、敵がいる場合は始点にできない
                        if pos != target {
                            if let Some(occupant) = unit_positions.get(&pos) {
                                if occupant.player_id != player_id {
                                    can_enter = false;
                                }
                            }
                        }
                        if can_enter {
                            dist.insert(pos, 0);
                            heap.push(State {
                                cost: 0,
                                position: pos,
                            });
                        }
                    }
                }
            }
        }
    }

    while let Some(State { cost, position }) = heap.pop() {
        if cost > *dist.get(&position).unwrap_or(&u32::MAX) {
            continue;
        }

        for next_pos in map.get_adjacent(position.0, position.1) {
            let Some(next_terrain) = map.get_terrain(next_pos.0, next_pos.1) else {
                continue;
            };

            let Some(move_cost) = get_valid_movement_cost(registry, movement_type, next_terrain)
            else {
                continue;
            };

            // 敵ユニットによるゾック（通行不可）判定
            if let Some(occupant) = unit_positions.get(&next_pos) {
                if occupant.player_id != player_id {
                    continue; // 敵がいるマスは通過不可
                }
            }

            let next_cost = cost + move_cost;
            let current_best = *dist.get(&next_pos).unwrap_or(&u32::MAX);

            if next_cost < current_best {
                dist.insert(next_pos, next_cost);
                heap.push(State {
                    cost: next_cost,
                    position: next_pos,
                });
            }
        }
    }

    // コスト (move_cost の合計) をターン数 (ceil(cost / max_mp)) に変換
    for (pos, cost) in dist {
        let turns = (cost + max_mp - 1) / max_mp;
        turns_map.insert(GridPosition { x: pos.0, y: pos.1 }, turns);
    }

    turns_map
}

/// AIの意思決定・ビーム探索プロセスにおいて、個別に確保して使い回すためのキャッシュ領域。
/// メモリー肥大化を防ぎ、かつ同一ターン内の探索全体でキャッシュを安全に共有します。
pub struct AiTurnCache {
    /// キー: (始点x, 始点y, 移動タイプ, 移動力, 射程, 勢力ID)
    /// 値: その始点からマップ上の全到達座標への最短到達ターン数テーブル (Arc)
    #[allow(clippy::type_complexity)]
    pub sssp_cache: HashMap<
        (usize, usize, MovementType, u32, u32, PlayerId),
        Arc<HashMap<crate::components::GridPosition, u32>>,
    >,
}

impl AiTurnCache {
    pub fn new() -> Self {
        Self {
            sssp_cache: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.sssp_cache.clear();
    }
}

impl Default for AiTurnCache {
    fn default() -> Self {
        Self::new()
    }
}

/// キャッシュを利用して、始点から全座標への最短ターン数テーブルを一括取得します。
pub fn calculate_all_turn_distances_cached(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    interaction_max_range: u32,
    player_id: PlayerId,
    cache: &mut AiTurnCache,
) -> Arc<HashMap<crate::components::GridPosition, u32>> {
    let key = (
        target.0,
        target.1,
        movement_type,
        max_mp,
        interaction_max_range,
        player_id,
    );
    if let Some(cached_map) = cache.sssp_cache.get(&key) {
        return cached_map.clone(); // Arc の clone なので極めて高速
    }

    let result = calculate_all_turn_distances(
        map,
        registry,
        unit_positions,
        target,
        movement_type,
        max_mp,
        interaction_max_range,
        player_id,
    );
    let shared_result = Arc::new(result);
    cache.sssp_cache.insert(key, shared_result.clone());
    shared_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::Terrain;

    #[test]
    fn test_turn_distance() {
        let map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = TurnDistanceCache::default();
        let unit_positions = HashMap::new();

        // 平地はコスト1。歩兵（移動力3）。
        // 距離4のマスへは、コスト4。ターン数は ceil(4/3) = 2
        let dist = calculate_turn_distance(
            &map,
            &registry,
            &unit_positions,
            (0, 0),
            (4, 0),
            MovementType::Infantry,
            3,
            PlayerId(1),
            &mut cache,
        );
        assert_eq!(dist, 2);

        // キャッシュヒットの確認
        let dist2 = calculate_turn_distance(
            &map,
            &registry,
            &unit_positions,
            (0, 0),
            (4, 0),
            MovementType::Infantry,
            3,
            PlayerId(1),
            &mut cache,
        );
        assert_eq!(dist2, 2);
    }

    #[test]
    fn test_turn_distance_unreachable() {
        let mut map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let _ = map.set_terrain(2, 0, Terrain::Sea); // 海は歩兵進入不可
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = TurnDistanceCache::default();
        let unit_positions = HashMap::new();

        let dist = calculate_turn_distance(
            &map,
            &registry,
            &unit_positions,
            (0, 0),
            (4, 0),
            MovementType::Infantry,
            3,
            PlayerId(1),
            &mut cache,
        );
        assert_eq!(dist, 51);
    }
}
