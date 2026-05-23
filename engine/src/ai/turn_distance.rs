#![allow(clippy::too_many_arguments)]
#![allow(clippy::manual_div_ceil)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::manual_checked_ops)]

use crate::components::PlayerId;
use crate::resources::{Map, MovementType, master_data::MasterDataRegistry};
use crate::systems::movement::{OccupantInfo, get_valid_movement_cost};
use bevy_ecs::prelude::*;
use std::collections::{BinaryHeap, HashMap};

/// ターン数ベースの距離計算をキャッシュするためのリソース
#[derive(Resource, Default)]
pub struct TurnDistanceCache {
    /// キー: (出発地x, 出発地y, 目標地x, 目標地y, 移動タイプ, 移動力)
    /// 値: 到達ターン数 (到達不可の場合は u32::MAX)
    pub cache: HashMap<(usize, usize, usize, usize, MovementType, u32), u32>,
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

    let cache_key = (start.0, start.1, target.0, target.1, movement_type, max_mp);
    if let Some(&dist) = cache.cache.get(&cache_key) {
        return dist;
    }

    // 移動先の地形が進入可能か大まかにチェック (完全に進入不可なら即弾く)
    if let Some(target_terrain) = map.get_terrain(target.0, target.1) {
        let t_cost = get_valid_movement_cost(registry, movement_type, target_terrain);
        if t_cost.is_none() {
            cache.cache.insert(cache_key, u32::MAX);
            return u32::MAX;
        }
    }

    let mut dist = HashMap::new();
    let mut heap = BinaryHeap::new();

    dist.insert(start, 0);
    heap.push(State {
        cost: 0,
        position: start,
    });

    while let Some(State { cost, position }) = heap.pop() {
        if position == target {
            // 到達した
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
            // ただし目標地点に敵がいる場合は攻撃可能なので通行可能とみなす（隣接マスまで計算するだけにするか、ここでは通れるとする）
            // この関数は「到達可能か」なので、目標地点にいるユニットの通過は通れないとしても、その手前までは行ける。
            // しかし「そのマスに入る」コストなので、目標マス以外に敵がいたらそこは通過不可。
            if next_pos != target {
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

    cache.cache.insert(cache_key, u32::MAX);
    u32::MAX
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
        assert_eq!(dist, u32::MAX);
    }
}
