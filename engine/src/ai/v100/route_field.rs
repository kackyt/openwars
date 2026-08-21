//! GB版の個別目標までの地形経路場。
//!
//! ROMの近傍展開（bank 0 `1E55`）は、OpenWarsのodd-r六近傍と一致する。
//! 一方、地形コストを含む目標距離場はV100/V200専用なので通常AIとは分ける。

use crate::components::GridPosition;
use crate::resources::{Map, MasterDataRegistry, MovementType};
use crate::systems::movement::get_valid_movement_cost;
use std::collections::{BinaryHeap, HashMap};

#[derive(Clone, Copy, Eq, PartialEq)]
struct RouteState {
    cost: u32,
    position: (usize, usize),
}

impl Ord for RouteState {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .cost
            .cmp(&self.cost)
            .then_with(|| self.position.cmp(&other.position))
    }
}

impl PartialOrd for RouteState {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// 目標から各マスまでのGB疑似hex地形コストをDijkstraで作る。
///
/// ROMは候補から目標へ逆算せず、目標を始点として候補へ進入するコストを加える。
/// そのため候補自身が山なら、その山コストも比較値へ含まれる。
pub(crate) fn build_route_field(
    map: &Map,
    master_data: &MasterDataRegistry,
    target: GridPosition,
    movement_type: MovementType,
) -> HashMap<GridPosition, u32> {
    let mut distances = HashMap::new();
    let mut heap = BinaryHeap::new();
    if target.x >= map.width || target.y >= map.height {
        return distances;
    }
    distances.insert(target, 0_u32);
    heap.push(RouteState {
        cost: 0,
        position: (target.x, target.y),
    });

    while let Some(RouteState { cost, position }) = heap.pop() {
        let grid_position = GridPosition {
            x: position.0,
            y: position.1,
        };
        if cost > *distances.get(&grid_position).unwrap_or(&u32::MAX) {
            continue;
        }
        for next in map.get_adjacent(position.0, position.1) {
            let Some(next_terrain) = map.get_terrain(next.0, next.1) else {
                continue;
            };
            let Some(enter_cost) =
                get_valid_movement_cost(master_data, movement_type, next_terrain)
            else {
                continue;
            };
            let next_position = GridPosition {
                x: next.0,
                y: next.1,
            };
            let next_cost = cost.saturating_add(enter_cost);
            if next_cost < *distances.get(&next_position).unwrap_or(&u32::MAX) {
                distances.insert(next_position, next_cost);
                heap.push(RouteState {
                    cost: next_cost,
                    position: next,
                });
            }
        }
    }
    distances
}
