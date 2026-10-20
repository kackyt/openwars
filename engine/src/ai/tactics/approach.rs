//! 移動先から目標への接近の測り方。

use super::board::TacticalBoard;
use crate::ai::tactics::distance_cache::ActionDistanceCache;
use crate::ai::turn_distance::{TurnDistance, TurnDistanceCache, calculate_turn_distance};
use crate::collections::HashMap;
use crate::components::{GridPosition, UnitStats};
use crate::systems::movement::OccupantInfo;
use bevy_ecs::prelude::*;

/// ターン距離がこれ以上なら到達不能とみなし、接近の加点をしない。
const UNREACHABLE_TURNS: u32 = 99;

/// 1回の行動選択で共有するターン距離の計算器。
pub(crate) struct ApproachDistances<'a> {
    board: &'a TacticalBoard,
    cache: &'a mut TurnDistanceCache,
    /// 占有を無視した地形だけの距離（`terrain_cache` 用の空の占有表と組で使う）。
    terrain_cache: &'a mut TurnDistanceCache,
    no_occupancy: HashMap<(usize, usize), OccupantInfo>,
}

/// 目標までのターン距離と、同点比較用の格子（マンハッタン）距離。
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
    let mut terrain_cache = strategy_cache.take_terrain_distance_cache();
    let result = evaluate(
        world,
        &mut ApproachDistances {
            board,
            cache: &mut cache,
            terrain_cache: &mut terrain_cache,
            no_occupancy: HashMap::default(),
        },
    );
    strategy_cache.put_action_distance_cache(cache);
    strategy_cache.put_terrain_distance_cache(terrain_cache);
    world.insert_resource(strategy_cache);
    result
}

impl ApproachDistances<'_> {
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

    /// 現在位置と移動先それぞれから、`target` を `range` 以内に捉えるまでの距離を比べる。
    /// 経路長は地形だけで測る。敵ユニットを壁として扱うと、前線の敵を大回りする経路が
    /// 最短になり、後退を前進と取り違えるため。
    pub(crate) fn target_progress(
        &mut self,
        stats: &UnitStats,
        current: GridPosition,
        candidate: GridPosition,
        target: GridPosition,
        range: u32,
    ) -> TargetProgress {
        let map = &self.board.map;
        let current_grid = map.distance(current.x, current.y, target.x, target.y);
        let candidate_grid = map.distance(candidate.x, candidate.y, target.x, target.y);
        let current_path_mp = path_mp(self.terrain_turns(stats, current, target, range));
        let candidate_path_mp = path_mp(self.terrain_turns(stats, candidate, target, range));
        TargetProgress {
            current_grid,
            candidate_grid,
            current_path_mp,
            candidate_path_mp,
            range,
        }
    }

    /// 占有を無視し、地形の移動costだけで測ったターン距離。
    fn terrain_turns(
        &mut self,
        stats: &UnitStats,
        from: GridPosition,
        target: GridPosition,
        range: u32,
    ) -> TurnDistance {
        calculate_turn_distance(
            &self.board.map,
            &self.board.registry,
            &self.no_occupancy,
            (from.x, from.y),
            (target.x, target.y),
            stats.movement_type,
            stats.max_movement,
            range,
            self.board.player_id,
            self.terrain_cache,
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

/// `calculate_turn_distance_to_range` が到達不能（近似値）を表す消費移動力の番兵値。
const UNREACHABLE_MP: u32 = u32::MAX;

/// 到達可能なら目標までの消費移動力、到達不能ならNone。
fn path_mp(distance: TurnDistance) -> Option<u32> {
    (distance.used_mp != UNREACHABLE_MP).then_some(distance.used_mp)
}

/// 1手で目標へ近づくか遠ざかるかの比較。距離はすべて「目標を `range` 以内に捉えるまで」。
/// 経路長は占有を無視した地形だけの値。
#[derive(Debug, Clone, Copy)]
pub(crate) struct TargetProgress {
    pub(crate) current_grid: u32,
    pub(crate) candidate_grid: u32,
    /// 経路上の消費移動力。到達不能はNone。
    pub(crate) current_path_mp: Option<u32>,
    pub(crate) candidate_path_mp: Option<u32>,
    range: u32,
}

impl TargetProgress {
    /// 目標から遠ざかる移動か。経路長で比べるため、山・川の迂回で格子距離が一時的に
    /// 増える前進は遠ざかる扱いにしない。`range` 以内は経路長0で同点になる。
    /// 現在位置から到達不能なら経路長で比べられないので、格子距離で比べる
    /// （`range` 以内は従来どおり同点）。移動先だけが到達不能なら遠ざかる扱い。
    pub(crate) fn moves_away(&self) -> bool {
        match (self.current_path_mp, self.candidate_path_mp) {
            (Some(current), Some(candidate)) => candidate > current,
            (Some(_), None) => true,
            (None, _) => self.candidate_grid > self.current_grid.max(self.range),
        }
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

#[cfg(test)]
mod tests {
    use super::TargetProgress;

    fn progress(
        current_grid: u32,
        candidate_grid: u32,
        current_path_mp: Option<u32>,
        candidate_path_mp: Option<u32>,
        range: u32,
    ) -> TargetProgress {
        TargetProgress {
            current_grid,
            candidate_grid,
            current_path_mp,
            candidate_path_mp,
            range,
        }
    }

    #[test]
    fn path_length_decides_when_both_reachable() {
        // 格子距離が増えても経路長が縮めば遠ざからない（迂回）。
        assert!(!progress(3, 9, Some(26), Some(20), 1).moves_away());
        // 格子距離が縮んでも経路長が伸びれば遠ざかる（袋小路）。
        assert!(progress(5, 4, Some(10), Some(12), 0).moves_away());
        // 射程内（経路長0）同士の位置替えは同点。
        assert!(!progress(0, 1, Some(0), Some(0), 1).moves_away());
    }

    #[test]
    fn unreachable_candidate_moves_away() {
        assert!(progress(5, 2, Some(10), None, 0).moves_away());
    }

    #[test]
    fn unreachable_current_falls_back_to_grid_distance() {
        assert!(progress(5, 6, None, Some(3), 0).moves_away());
        assert!(!progress(5, 5, None, None, 0).moves_away());
        // 範囲内は格子距離でも同点。
        assert!(!progress(0, 1, None, None, 1).moves_away());
    }
}
