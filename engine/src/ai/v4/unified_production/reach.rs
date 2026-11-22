//! 高速・決定的な盤面到達性・最短移動ターン場ユーティリティ。
//!
//! 実ゲームエンジン（`systems/movement.rs`）と一致するよう、以下のルールを厳密に適用する：
//! - 航空ユニットを含むすべての移動タイプが、敵 ZOC マスに入った時点でそのターンの移動を停止する。
//! - 複数ターンの移動では残り MP を次ターンへ繰り越さず、ターン境界で MP 上限 `max_movement` をリセットする。

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

/// 盤面全体に対する移動コスト／到達ターンマップ。
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

    /// 到達可能な全 `(座標, コスト/ターン数)` を座標昇順で返す。
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

/// ユニット遮蔽・全兵種共通の敵 ZOC 停止・ターン境界を含む到達可能マス集合を返す。
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
    max_turns: u32,
    allow_own_mobile: Option<&HashSet<GridPosition>>,
) -> ReachMap {
    let mut result = ReachMap::new(map.width, map.height);
    if start.x >= map.width || start.y >= map.height {
        return result;
    }
    let effective_fuel = if max_fuel == 0 { u32::MAX } else { max_fuel };
    let len = map.width.saturating_mul(map.height);
    let mut best_state = vec![(UNREACHABLE, UNREACHABLE); len];
    let start_idx = occ.idx(start);
    best_state[start_idx] = (0, 0);

    // (経過ターン, ターン内消費MP, 累計歩数=燃料消費, y, x) の最小ヒープ
    let mut heap = BinaryHeap::new();
    heap.push(Reverse((0_u32, 0_u32, 0_u32, start.y, start.x)));

    while let Some(Reverse((turns, mp_in_turn, steps, y, x))) = heap.pop() {
        let pos = GridPosition { x, y };
        let idx = occ.idx(pos);
        if (turns, mp_in_turn) > best_state[idx] || turns > max_turns || steps >= effective_fuel {
            continue;
        }
        // 他味方が占有しており退避許可もないマスではターンを終了して次ターンへ繰り越せない
        let occupied_non_vacatable = pos != start
            && occ.cells[idx] != 0
            && !allow_own_mobile.is_some_and(|set| set.contains(&pos));
        // 実エンジン準拠：航空ユニットを含め全移動タイプが敵 ZOC に進入したらそのターンを終了する
        let must_start_next_turn = turns > 0 && pos != start && occ.is_zoc_for(mover_code, pos);

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
            if step_cost > max_movement {
                continue;
            }
            let (next_turns, next_mp) = if turns == 0 {
                (1, step_cost)
            } else if must_start_next_turn || mp_in_turn.saturating_add(step_cost) > max_movement {
                if occupied_non_vacatable {
                    continue;
                }
                (turns.saturating_add(1), step_cost)
            } else {
                (turns, mp_in_turn.saturating_add(step_cost))
            };
            let next_steps = steps.saturating_add(1);
            if next_turns > max_turns || next_steps > effective_fuel {
                continue;
            }
            let nidx = occ.idx(npos);
            if (next_turns, next_mp) < best_state[nidx] {
                best_state[nidx] = (next_turns, next_mp);
                heap.push(Reverse((next_turns, next_mp, next_steps, ny, nx)));
            }
        }
    }

    // 停止可能マスのみ確定する（補給都市退避許可分を除き他味方占有マスへの停止は不可）
    for y in 0..map.height {
        for x in 0..map.width {
            let pos = GridPosition { x, y };
            let idx = occ.idx(pos);
            let t = best_state[idx].0;
            if t == UNREACHABLE {
                continue;
            }
            if pos != start && occ.cells[idx] != 0 {
                let allowed = allow_own_mobile.is_some_and(|set| set.contains(&pos));
                if !allowed {
                    continue;
                }
            }
            result.costs[idx] = t;
        }
    }

    result
}

/// ユニット遮蔽・ZOC を無視し、ターン境界（残り MP 繰り越し禁止）だけを守った到達範囲を返す。
pub(super) fn reachable_pierce(
    map: &Map,
    master_data: &MasterDataRegistry,
    start: GridPosition,
    movement_type: MovementType,
    max_movement: u32,
    max_fuel: u32,
    max_turns: u32,
) -> ReachMap {
    let mut result = ReachMap::new(map.width, map.height);
    if start.x >= map.width || start.y >= map.height {
        return result;
    }
    let effective_fuel = if max_fuel == 0 { u32::MAX } else { max_fuel };
    let len = map.width.saturating_mul(map.height);
    let mut best_state = vec![(UNREACHABLE, UNREACHABLE); len];
    let start_idx = start.y * map.width + start.x;
    best_state[start_idx] = (0, 0);

    let mut heap = BinaryHeap::new();
    heap.push(Reverse((0_u32, 0_u32, 0_u32, start.y, start.x)));

    while let Some(Reverse((turns, mp_in_turn, steps, y, x))) = heap.pop() {
        let idx = y * map.width + x;
        if (turns, mp_in_turn) > best_state[idx] || turns > max_turns || steps >= effective_fuel {
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
            if step_cost > max_movement {
                continue;
            }
            let (next_turns, next_mp) = if turns == 0 {
                (1, step_cost)
            } else if mp_in_turn.saturating_add(step_cost) > max_movement {
                (turns.saturating_add(1), step_cost)
            } else {
                (turns, mp_in_turn.saturating_add(step_cost))
            };
            let next_steps = steps.saturating_add(1);
            if next_turns > max_turns || next_steps > effective_fuel {
                continue;
            }
            let nidx = ny * map.width + nx;
            if (next_turns, next_mp) < best_state[nidx] {
                best_state[nidx] = (next_turns, next_mp);
                heap.push(Reverse((next_turns, next_mp, next_steps, ny, nx)));
            }
        }
    }

    for (idx, &(t, _)) in best_state.iter().enumerate() {
        if t != UNREACHABLE {
            result.costs[idx] = t;
        }
    }

    result
}

/// 単一始点からマップ全域への、ターン境界つき最小移動ターン数場を返す（始点は 0 ターン）。
pub(super) fn unit_turn_distance_field(
    map: &Map,
    master_data: &MasterDataRegistry,
    start: GridPosition,
    movement_type: MovementType,
    max_movement: u32,
    max_fuel: u32,
) -> ReachMap {
    reachable_pierce(
        map,
        master_data,
        start,
        movement_type,
        max_movement.max(1),
        max_fuel,
        u32::MAX,
    )
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
