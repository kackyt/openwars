//! 移動先から目標への接近の測り方。

use super::board::TacticalBoard;
use crate::ai::tactics::distance_cache::ActionDistanceCache;
use crate::ai::turn_distance::{TurnDistance, TurnDistanceCache, calculate_turn_distance};
use crate::components::{GridPosition, UnitStats};
use bevy_ecs::prelude::*;

/// ターン距離がこれ以上なら到達不能とみなし、接近の加点をしない。
const UNREACHABLE_TURNS: u32 = 99;

/// 1回の行動選択で共有するターン距離の計算器。
pub(crate) struct ApproachDistances<'a> {
    board: &'a TacticalBoard,
    cache: &'a mut TurnDistanceCache,
}

/// 目標までのターン距離と、同点比較用のトポロジーに応じた格子距離。
pub(crate) type Approach = (TurnDistance, i32);

/// 手番内で共有するターン距離cacheを借りて `evaluate` を実行する。
///
/// 自軍の移動は距離探索上の通行可否を変えない。敵が撃破されて占有マスが変わるまで、
/// 同一手番の全unitでDijkstra結果を共有する。
pub(crate) fn with_turn_distances<R>(
    world: &mut World,
    board: &TacticalBoard,
    evaluate: impl FnOnce(&World, &mut ApproachDistances) -> R,
) -> R {
    let mut strategy_cache = world
        .remove_resource::<ActionDistanceCache>()
        .unwrap_or_default();
    let mut cache = strategy_cache
        .take_action_distance_cache(board.player_id, &board.enemy_blocker_positions());
    let result = evaluate(
        world,
        &mut ApproachDistances {
            board,
            cache: &mut cache,
        },
    );
    strategy_cache.put_action_distance_cache(cache);
    world.insert_resource(strategy_cache);
    result
}

impl ApproachDistances<'_> {
    /// Square / Hexの盤面トポロジーに従って格子距離を測る。
    pub(crate) fn distance(&self, a: GridPosition, b: GridPosition) -> i32 {
        self.board.map.distance(a.x, a.y, b.x, b.y) as i32
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
                self.distance(from, target),
            );
            if nearest.is_none_or(|current| candidate < current) {
                nearest = Some(candidate);
            }
        }
        nearest
    }
}

/// 目標へ近いほど大きい接近score。消費MPによる連続ターン距離と格子距離を加える。
/// 切り上げ境界を跨ぐ小さな迂回で、前進の加点を相殺しない。
pub(crate) fn approach_bonus(dist: TurnDistance, grid_distance: i32, max_movement: u32) -> i32 {
    if dist.turns >= UNREACHABLE_TURNS {
        return 0;
    }
    let movement = max_movement.max(1) as f32;
    let cont_turns = dist.used_mp as f32 / movement;
    let turn_score = (100.0 - cont_turns).max(0.0) * 1000.0;
    let moves = grid_distance as f32 / movement;
    let move_score = (100.0 - moves).max(0.0) * 2000.0;
    (turn_score + move_score) as i32
}

/// 到達可能な最寄り目標への接近score（到達不能・目標なしは0）。
pub(crate) fn nearest_approach_bonus(nearest: Option<Approach>, max_movement: u32) -> i32 {
    match nearest {
        Some((turns, grid_distance)) if turns.turns < UNREACHABLE_TURNS => {
            approach_bonus(turns, grid_distance, max_movement)
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_approach_bonus_continuous() {
        // 48→49MPで切り上げターンが増えても、2マスの前進は森の防御200点を上回る。
        let before = approach_bonus(
            TurnDistance {
                turns: 12,
                used_mp: 48,
            },
            20,
            4,
        );
        let after = approach_bonus(
            TurnDistance {
                turns: 13,
                used_mp: 49,
            },
            18,
            4,
        );
        assert_eq!(after - before, 750);
        assert!(after > before + 200);
        assert!(
            approach_bonus(
                TurnDistance {
                    turns: 13,
                    used_mp: 50
                },
                18,
                4
            ) < after
        );
        assert_eq!(
            approach_bonus(
                TurnDistance {
                    turns: 0,
                    used_mp: 0
                },
                0,
                4
            ),
            300_000
        );
        assert_eq!(
            approach_bonus(
                TurnDistance {
                    turns: 0,
                    used_mp: 0
                },
                0,
                0
            ),
            300_000
        );
        assert_eq!(nearest_approach_bonus(None, 4), 0);
        assert_eq!(
            approach_bonus(
                TurnDistance {
                    turns: 99,
                    used_mp: 0
                },
                0,
                4
            ),
            0
        );
    }
}
