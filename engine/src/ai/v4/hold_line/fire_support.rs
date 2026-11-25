//! 火力支援計画: 陣地線を間接火力で支えるための砲座候補と購入期限を、地形と射程だけから求める。
//!
//! 計画書 `docs/07-project-management/plans/v4-demand-based-production.md` の 3.1 節に対応する。
//! 座標やマップ固有の定数は持たず、陣地線 `L`・敵側の列 `E`・自陣側 `home`・射程・距離関数だけを使う。
//!
//! Step 1 では関数の形と単体テスト（`fire_support/tests.rs`）だけを先に用意している。
//! 本体は未実装（`todo!()`）で、テストは `#[ignore]` にしてある。実装したら
//! 各テストの `#[ignore]` と、このファイル先頭の `allow` を外すこと。

// Step 3 で生産へ接続するまでは呼び出し元がなく、本体も未実装のため一時的に許可する。
#![allow(dead_code, unused_variables)]

use crate::collections::HashSet;
use crate::components::GridPosition;

#[cfg(test)]
mod tests;

/// 射程（最小・最大。どちらもマス単位で両端を含む）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FireRange {
    pub(crate) min: u32,
    pub(crate) max: u32,
}

impl FireRange {
    /// 距離 `distance` のマスを撃てるか。
    pub(crate) fn contains(self, distance: u32) -> bool {
        self.min <= distance && distance <= self.max
    }
}

/// 火力支援計画の入力。
pub(crate) struct FireSupportInput<'a, D>
where
    D: Fn(GridPosition, GridPosition) -> u32,
{
    /// 陣地線 `L`（自軍が保持するマス）。
    pub(crate) line: &'a [GridPosition],
    /// 陣地線の敵側に接する1列 `E`（敵が陣地線を攻撃するときに立つマス）。
    pub(crate) enemy_side: &'a [GridPosition],
    /// 陣地線の自陣側のマス。`line` と重なっていてもよい。
    pub(crate) home: &'a HashSet<GridPosition>,
    /// 盤面上の距離（射程判定と隣接判定に使う。隣接は距離 1）。
    pub(crate) distance: D,
}

/// 砲座候補1つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatterySite {
    /// 砲を置くマス。
    pub(crate) position: GridPosition,
    /// このマスから射程で覆える `L ∪ E` のマス。`(y, x)` の昇順。
    pub(crate) covered: Vec<GridPosition>,
}

/// 指定した射程の間接部隊が使える砲座候補を返す。
///
/// 条件:
/// - `home` に含まれる。
/// - `E` のどのマスにも隣接しない（陣地線を保持していれば敵の直接攻撃を受けない）。
/// - `L ∪ E` のうち射程内のマスが1つ以上ある（最小射程より近いマスは覆えない）。
///
/// 生産口かどうかでは除外しない。生産口を塞ぐ損は需要価値（`demand_value`）の占有コストで比べる。
/// 戻り値は砲座の `(y, x)` の昇順。
pub(crate) fn battery_sites<D>(
    input: &FireSupportInput<'_, D>,
    range: FireRange,
) -> Vec<BatterySite>
where
    D: Fn(GridPosition, GridPosition) -> u32,
{
    todo!("Step 1: 砲座候補の列挙")
}

/// 砲座へ着いた次の手番から撃つ間接部隊について、購入期限（この手番までに買えば間に合う）を返す。
///
/// - `enemy_arrival_turn`: 敵がその砲座の射程内のマスに着く手番。
/// - `travel_turns`: 生産施設から砲座までの移動手番数（生産施設そのものが砲座なら 0）。
///
/// 買った手番には動けず、移動した手番には撃てないため、撃ち始めは `購入手番 + 移動手番数 + 1`。
/// これが敵の到着手番以下になる最後の購入手番を返す。手番 0 より前になる（もう間に合わない）なら `None`。
pub(crate) fn purchase_deadline(enemy_arrival_turn: u32, travel_turns: u32) -> Option<u32> {
    todo!("Step 1: 購入期限")
}
