//! 移動先から目標への接近の測り方。

use super::board::TacticalBoard;
use crate::ai::turn_distance::{TurnDistance, TurnDistanceCache, calculate_turn_distance};
use crate::components::{GridPosition, UnitStats};

/// ターン距離がこれ以上なら到達不能とみなし、接近の加点をしない。
const UNREACHABLE_TURNS: u32 = 99;

/// 1回の行動選択で共有するターン距離の計算器。
pub(crate) struct ApproachDistances<'a> {
    board: &'a TacticalBoard,
    cache: &'a mut TurnDistanceCache,
}

/// 目標までのターン距離と、同点比較用の格子（マンハッタン）距離。
pub(crate) type Approach = (TurnDistance, i32);

impl<'a> ApproachDistances<'a> {
    pub(crate) fn new(board: &'a TacticalBoard, cache: &'a mut TurnDistanceCache) -> Self {
        Self { board, cache }
    }

    /// `from` から `target` を `range` 以内に捉えるまでのターン距離。
    pub(crate) fn turns(
        &mut self,
        stats: &UnitStats,
        from: GridPosition,
        target: GridPosition,
        range: u32,
    ) -> TurnDistance {
        calculate_turn_distance(
            &self.board.map,
            &self.board.registry,
            &self.board.occupancy,
            (from.x, from.y),
            (target.x, target.y),
            stats.movement_type,
            stats.max_movement,
            range,
            self.board.player_id,
            self.cache,
        )
    }

    /// 目標群のうち、（ターン距離, 格子距離）が最小のもの。同点なら先に現れた目標。
    pub(crate) fn nearest(
        &mut self,
        stats: &UnitStats,
        from: GridPosition,
        targets: impl IntoIterator<Item = GridPosition>,
        range: u32,
    ) -> Option<Approach> {
        let mut nearest: Option<Approach> = None;
        for target in targets {
            let candidate = (
                self.turns(stats, from, target, range),
                manhattan(from, target),
            );
            if nearest.is_none_or(|current| candidate < current) {
                nearest = Some(candidate);
            }
        }
        nearest
    }
}

pub(crate) fn manhattan(from: GridPosition, to: GridPosition) -> i32 {
    (from.x as i32 - to.x as i32).abs() + (from.y as i32 - to.y as i32).abs()
}

/// 目標へ近いほど大きい接近score。ターン距離（1ターン1000点）と、移動力で割った
/// 格子距離（1移動分2000点）の両方を加える。後者は同じターン距離の中での前進を区別する。
pub(crate) fn approach_bonus(turns: u32, grid_distance: i32, max_movement: u32) -> i32 {
    let moves = grid_distance as f32 / max_movement as f32;
    (100 - turns as i32).max(0) * 1000 + ((100.0 - moves).max(0.0) * 2000.0) as i32
}

/// 到達可能な最寄り目標への接近score（到達不能・目標なしは0）。
pub(crate) fn nearest_approach_bonus(nearest: Option<Approach>, max_movement: u32) -> i32 {
    match nearest {
        Some((turns, grid_distance)) if turns.turns < UNREACHABLE_TURNS => {
            approach_bonus(turns.turns, grid_distance, max_movement)
        }
        _ => 0,
    }
}
