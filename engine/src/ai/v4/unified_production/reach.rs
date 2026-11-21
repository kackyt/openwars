//! 高速・決定的な盤面到達性・最短移動コスト場ユーティリティ。
//!
//! 小マップ向けの生産評価で反復的に呼ばれるため、盤面サイズ `width * height` の
//! フラット配列を用いてハッシュ計算と動的確保を抑え、完全決定的な順序で探索する。

use crate::collections::HashSet;
use crate::components::GridPosition;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Map, MovementType};
use crate::systems::movement::get_valid_movement_cost;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// 到達不能を表す番兵コスト。
pub(super) const UNREACHABLE: u32 = u32::MAX;

/// 自軍ユニットの占有コード。
pub(super) const OCC_FRIENDLY: u8 = 1;
/// 敵軍ユニットの占有コード。
pub(super) const OCC_ENEMY: u8 = 2;

/// 盤面各マスのユニット占有状態（0: 空き, 1: 自軍, 2: 敵軍）。
#[derive(Debug, Clone)]
pub(super) struct OccupancyGrid {
    pub(super) width: usize,
    cells: Vec<u8>,
    /// 各マスが敵（code = 2）に隣接しているか（ZOC 判定用）。
    friendly_zoc_locked: Vec<bool>,
    /// 各マスが自軍（code = 1）に隣接しているか（敵の ZOC 判定用）。
    enemy_zoc_locked: Vec<bool>,
}

impl OccupancyGrid {
    /// 盤面の自軍・敵ユニット位置から占有・ZOC グリッドを構築する。
    pub(super) fn from_units(
        map: &Map,
        friendly_positions: impl IntoIterator<Item = GridPosition>,
        enemy_positions: impl IntoIterator<Item = GridPosition>,
    ) -> Self {
        let len = map.width.saturating_mul(map.height);
        let mut cells = vec![0_u8; len];
        let mut friendly_zoc_locked = vec![false; len];
        let mut enemy_zoc_locked = vec![false; len];

        for pos in friendly_positions {
            if pos.x < map.width && pos.y < map.height {
                cells[pos.y * map.width + pos.x] = OCC_FRIENDLY;
                for (nx, ny) in map.get_adjacent(pos.x, pos.y) {
                    enemy_zoc_locked[ny * map.width + nx] = true;
                }
            }
        }
        for pos in enemy_positions {
            if pos.x < map.width && pos.y < map.height {
                cells[pos.y * map.width + pos.x] = OCC_ENEMY;
                for (nx, ny) in map.get_adjacent(pos.x, pos.y) {
                    friendly_zoc_locked[ny * map.width + nx] = true;
                }
            }
        }

        Self {
            width: map.width,
            cells,
            friendly_zoc_locked,
            enemy_zoc_locked,
        }
    }

    #[inline]
    pub(super) fn idx(&self, pos: GridPosition) -> usize {
        pos.y * self.width + pos.x
    }

    #[inline]
    pub(super) fn at(&self, pos: GridPosition) -> u8 {
        self.cells[self.idx(pos)]
    }

    #[inline]
    fn is_zoc_for(&self, mover_code: u8, pos: GridPosition) -> bool {
        let idx = self.idx(pos);
        if mover_code == OCC_FRIENDLY {
            self.friendly_zoc_locked[idx]
        } else {
            self.enemy_zoc_locked[idx]
        }
    }
}

/// 盤面全体に対する移動コスト・到達性マップ。
#[derive(Debug, Clone)]
pub(super) struct ReachMap {
    pub(super) width: usize,
    pub(super) height: usize,
    costs: Vec<u32>,
}

impl ReachMap {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            costs: vec![UNREACHABLE; width.saturating_mul(height)],
        }
    }

    #[inline]
    pub(super) fn get(&self, pos: GridPosition) -> Option<u32> {
        if pos.x >= self.width || pos.y >= self.height {
            return None;
        }
        let cost = self.costs[pos.y * self.width + pos.x];
        (cost != UNREACHABLE).then_some(cost)
    }

    #[inline]
    pub(super) fn contains(&self, pos: GridPosition) -> bool {
        self.get(pos).is_some()
    }

    /// 到達可能な全 `(座標, 移動コスト)` を座標昇順で返す。
    pub(super) fn iter(&self) -> impl Iterator<Item = (GridPosition, u32)> + '_ {
        self.costs
            .iter()
            .enumerate()
            .filter(|&(_, &cost)| cost != UNREACHABLE)
            .map(|(idx, &cost)| {
                let x = idx % self.width;
                let y = idx / self.width;
                (GridPosition { x, y }, cost)
            })
    }
}

/// ユニット遮蔽と敵 ZOC 停止を含む到達可能マス集合を返す。
#[allow(clippy::too_many_arguments)]
pub(super) fn reachable_tiles(
    map: &Map,
    master_data: &MasterDataRegistry,
    occ: &OccupancyGrid,
    start: GridPosition,
    movement_type: MovementType,
    max_movement: u32,
    max_fuel: u32,
    mover_code: u8,
    mp_mult: u32,
    allow_own_mobile: Option<&HashSet<GridPosition>>,
) -> ReachMap {
    let mut dist = ReachMap::new(map.width, map.height);
    if start.x >= map.width || start.y >= map.height {
        return dist;
    }
    let max_mp = max_movement.saturating_mul(mp_mult);
    let is_air = movement_type == MovementType::Air;
    let start_idx = occ.idx(start);
    dist.costs[start_idx] = 0;

    // (累計移動コスト, 累計移動歩数=燃料消費, y, x) の最小ヒープ
    let mut heap = BinaryHeap::new();
    heap.push(Reverse((0_u32, 0_u32, start.y, start.x)));

    while let Some(Reverse((cost, steps, y, x))) = heap.pop() {
        let pos = GridPosition { x, y };
        let idx = occ.idx(pos);
        if cost > dist.costs[idx] || steps >= max_fuel {
            continue;
        }
        // 航空ユニット以外は敵 ZOC に入ったマスで移動が止まる
        if pos != start && !is_air && occ.is_zoc_for(mover_code, pos) {
            continue;
        }
        for (nx, ny) in map.get_adjacent(x, y) {
            let npos = GridPosition { x: nx, y: ny };
            let cell_occ = occ.at(npos);
            if cell_occ != 0 && cell_occ != mover_code {
                continue;
            }
            let Some(terrain) = map.get_terrain(nx, ny) else {
                continue;
            };
            let Some(step_cost) = get_valid_movement_cost(master_data, movement_type, terrain)
            else {
                continue;
            };
            let next_cost = cost.saturating_add(step_cost);
            let next_steps = steps.saturating_add(1);
            let nidx = occ.idx(npos);
            if next_cost <= max_mp && next_steps <= max_fuel && next_cost < dist.costs[nidx] {
                dist.costs[nidx] = next_cost;
                heap.push(Reverse((next_cost, next_steps, ny, nx)));
            }
        }
    }

    // 他味方が占有しているマスへの停止可否を確定する（補給都市退避許可分だけ残す）
    for y in 0..map.height {
        for x in 0..map.width {
            let pos = GridPosition { x, y };
            let idx = occ.idx(pos);
            if dist.costs[idx] == UNREACHABLE || pos == start {
                continue;
            }
            if occ.cells[idx] != 0 {
                let allowed = allow_own_mobile.is_some_and(|set| set.contains(&pos));
                if !allowed {
                    dist.costs[idx] = UNREACHABLE;
                }
            }
        }
    }

    dist
}

/// ユニット遮蔽・ZOC を無視した純地形到達範囲を返す（2ターン突破リスク計算用）。
pub(super) fn reachable_pierce(
    map: &Map,
    master_data: &MasterDataRegistry,
    start: GridPosition,
    movement_type: MovementType,
    max_movement: u32,
    max_fuel: u32,
    mp_mult: u32,
) -> ReachMap {
    let mut dist = ReachMap::new(map.width, map.height);
    if start.x >= map.width || start.y >= map.height {
        return dist;
    }
    let max_mp = max_movement.saturating_mul(mp_mult);
    let start_idx = start.y * map.width + start.x;
    dist.costs[start_idx] = 0;

    let mut heap = BinaryHeap::new();
    heap.push(Reverse((0_u32, 0_u32, start.y, start.x)));

    while let Some(Reverse((cost, steps, y, x))) = heap.pop() {
        let idx = y * map.width + x;
        if cost > dist.costs[idx] || steps >= max_fuel {
            continue;
        }
        for (nx, ny) in map.get_adjacent(x, y) {
            let Some(terrain) = map.get_terrain(nx, ny) else {
                continue;
            };
            let Some(step_cost) = get_valid_movement_cost(master_data, movement_type, terrain)
            else {
                continue;
            };
            let next_cost = cost.saturating_add(step_cost);
            let next_steps = steps.saturating_add(1);
            let nidx = ny * map.width + nx;
            if next_cost <= max_mp && next_steps <= max_fuel && next_cost < dist.costs[nidx] {
                dist.costs[nidx] = next_cost;
                heap.push(Reverse((next_cost, next_steps, ny, nx)));
            }
        }
    }

    dist
}

/// 多始点 Dijkstra による目標集合からの最短移動コスト場（敵前進投影・距離参照用）。
pub(super) fn shortest_dist_field(
    map: &Map,
    master_data: &MasterDataRegistry,
    occ: &OccupancyGrid,
    goals: &[GridPosition],
    movement_type: MovementType,
    mover_code: u8,
) -> ReachMap {
    let mut dist = ReachMap::new(map.width, map.height);
    let mut heap = BinaryHeap::new();

    for &goal in goals {
        if goal.x < map.width && goal.y < map.height {
            let idx = occ.idx(goal);
            if dist.costs[idx] != 0 {
                dist.costs[idx] = 0;
                heap.push(Reverse((0_u32, goal.y, goal.x)));
            }
        }
    }

    while let Some(Reverse((cost, y, x))) = heap.pop() {
        let idx = y * map.width + x;
        if cost > dist.costs[idx] {
            continue;
        }
        for (nx, ny) in map.get_adjacent(x, y) {
            let npos = GridPosition { x: nx, y: ny };
            let cell_occ = occ.at(npos);
            if cell_occ != 0 && cell_occ != mover_code {
                continue;
            }
            let Some(terrain) = map.get_terrain(nx, ny) else {
                continue;
            };
            let Some(step_cost) = get_valid_movement_cost(master_data, movement_type, terrain)
            else {
                continue;
            };
            let next_cost = cost.saturating_add(step_cost);
            let nidx = occ.idx(npos);
            if next_cost < dist.costs[nidx] {
                dist.costs[nidx] = next_cost;
                heap.push(Reverse((next_cost, ny, nx)));
            }
        }
    }

    dist
}

/// 単一始点からマップ全域への純地形最短移動コスト場を返す。
pub(super) fn unit_distance_field(
    map: &Map,
    master_data: &MasterDataRegistry,
    start: GridPosition,
    movement_type: MovementType,
) -> ReachMap {
    let mut dist = ReachMap::new(map.width, map.height);
    if start.x >= map.width || start.y >= map.height {
        return dist;
    }
    let start_idx = start.y * map.width + start.x;
    dist.costs[start_idx] = 0;

    let mut heap = BinaryHeap::new();
    heap.push(Reverse((0_u32, start.y, start.x)));

    while let Some(Reverse((cost, y, x))) = heap.pop() {
        let idx = y * map.width + x;
        if cost > dist.costs[idx] {
            continue;
        }
        for (nx, ny) in map.get_adjacent(x, y) {
            let Some(terrain) = map.get_terrain(nx, ny) else {
                continue;
            };
            let Some(step_cost) = get_valid_movement_cost(master_data, movement_type, terrain)
            else {
                continue;
            };
            let next_cost = cost.saturating_add(step_cost);
            let nidx = ny * map.width + nx;
            if next_cost < dist.costs[nidx] {
                dist.costs[nidx] = next_cost;
                heap.push(Reverse((next_cost, ny, nx)));
            }
        }
    }

    dist
}
