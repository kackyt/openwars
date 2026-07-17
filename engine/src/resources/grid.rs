//! グリッド座標系（スクエア / ヘックス）の距離・隣接計算ロジック。
//!
//! ヘックスは「odd-r オフセット座標」（奇数行が右に半マスずれる pointy-top 配置）を採用する。
//! 距離計算はオフセット座標をキューブ座標に変換してから行う。

/// マップのグリッド形状（トポロジー）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GridTopology {
    Square,
    Hex,
}

impl GridTopology {
    /// このトポロジーに対応するジオメトリ実装を返す
    pub fn geometry(&self) -> &'static dyn GridGeometry {
        match self {
            GridTopology::Square => &SquareGrid,
            GridTopology::Hex => &HexGrid,
        }
    }

    /// 2点間のグリッド距離（最短ステップ数）を返す
    pub fn distance(&self, a: (usize, usize), b: (usize, usize)) -> u32 {
        match self {
            GridTopology::Square => SquareGrid.distance(a, b),
            GridTopology::Hex => HexGrid.distance(a, b),
        }
    }

    /// マップ境界内の隣接セルを列挙する
    pub fn neighbors(
        &self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) -> Vec<(usize, usize)> {
        match self {
            GridTopology::Square => SquareGrid.neighbors(x, y, width, height),
            GridTopology::Hex => HexGrid.neighbors(x, y, width, height),
        }
    }
}

/// グリッド形状ごとの距離・隣接計算を抽象化するトレイト。
/// スクエア／ヘックスを切り替え可能にしつつ、既存ロジックには
/// `Map::distance` / `Map::get_adjacent` 経由で透過的に提供する。
pub trait GridGeometry: Send + Sync {
    /// 2点間のグリッド距離（最短ステップ数）
    fn distance(&self, a: (usize, usize), b: (usize, usize)) -> u32;

    /// マップ境界内 (width x height) に収まる隣接セルの一覧
    fn neighbors(&self, x: usize, y: usize, width: usize, height: usize) -> Vec<(usize, usize)>;
}

/// 四角形グリッド（4近傍・マンハッタン距離）
pub struct SquareGrid;

impl GridGeometry for SquareGrid {
    fn distance(&self, a: (usize, usize), b: (usize, usize)) -> u32 {
        let dx = (a.0 as i64 - b.0 as i64).unsigned_abs();
        let dy = (a.1 as i64 - b.1 as i64).unsigned_abs();
        (dx + dy) as u32
    }

    fn neighbors(&self, x: usize, y: usize, width: usize, height: usize) -> Vec<(usize, usize)> {
        let mut adj = Vec::with_capacity(4);
        if x > 0 {
            adj.push((x - 1, y));
        }
        if x + 1 < width {
            adj.push((x + 1, y));
        }
        if y > 0 {
            adj.push((x, y - 1));
        }
        if y + 1 < height {
            adj.push((x, y + 1));
        }
        adj
    }
}

/// ヘックスグリッド（odd-r オフセット座標・6近傍・キューブ距離）
pub struct HexGrid;

/// odd-r オフセット座標 (col, row) をキューブ座標 (q, r, s) に変換する
fn offset_to_cube(x: i64, y: i64) -> (i64, i64, i64) {
    // 奇数行が右にずれる odd-r レイアウト。y が負になることはない前提だが、
    // 差分計算の安全のため Euclidean ではなく切り捨て除算で統一する
    let q = x - (y - (y & 1)) / 2;
    let r = y;
    (q, r, -q - r)
}

impl GridGeometry for HexGrid {
    fn distance(&self, a: (usize, usize), b: (usize, usize)) -> u32 {
        let (aq, ar, as_) = offset_to_cube(a.0 as i64, a.1 as i64);
        let (bq, br, bs) = offset_to_cube(b.0 as i64, b.1 as i64);
        // キューブ座標での距離 = 各成分差の絶対値の合計 / 2
        let d = (aq - bq).unsigned_abs() + (ar - br).unsigned_abs() + (as_ - bs).unsigned_abs();
        (d / 2) as u32
    }

    fn neighbors(&self, x: usize, y: usize, width: usize, height: usize) -> Vec<(usize, usize)> {
        // odd-r レイアウトの近傍オフセット（行の偶奇で斜め方向の列が変わる）
        const EVEN_ROW: [(i64, i64); 6] = [(1, 0), (-1, 0), (0, -1), (-1, -1), (0, 1), (-1, 1)];
        const ODD_ROW: [(i64, i64); 6] = [(1, 0), (-1, 0), (1, -1), (0, -1), (1, 1), (0, 1)];

        #[allow(clippy::manual_is_multiple_of)]
        let offsets = if y % 2 == 0 { &EVEN_ROW } else { &ODD_ROW };
        let mut adj = Vec::with_capacity(6);
        for (dx, dy) in offsets {
            let nx = x as i64 + dx;
            let ny = y as i64 + dy;
            if nx >= 0 && ny >= 0 && (nx as usize) < width && (ny as usize) < height {
                adj.push((nx as usize, ny as usize));
            }
        }
        adj
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};

    #[test]
    fn test_square_distance_is_manhattan() {
        let g = GridTopology::Square;
        assert_eq!(g.distance((0, 0), (0, 0)), 0);
        assert_eq!(g.distance((0, 0), (3, 4)), 7);
        assert_eq!(g.distance((5, 2), (1, 2)), 4);
    }

    #[test]
    fn test_square_neighbors_clipped_at_border() {
        let g = GridTopology::Square;
        let mut corner = g.neighbors(0, 0, 5, 5);
        corner.sort();
        assert_eq!(corner, vec![(0, 1), (1, 0)]);
        assert_eq!(g.neighbors(2, 2, 5, 5).len(), 4);
    }

    #[test]
    fn test_hex_distance_basic() {
        let g = GridTopology::Hex;
        assert_eq!(g.distance((0, 0), (0, 0)), 0);
        // 同一行は列差そのまま
        assert_eq!(g.distance((0, 0), (4, 0)), 4);
        // 隣接セル（奇数行の斜め）は距離1
        assert_eq!(g.distance((0, 0), (0, 1)), 1);
        assert_eq!(g.distance((1, 1), (1, 2)), 1);
        // 斜めに大きく移動するケース：マンハッタンより短くなる
        assert_eq!(g.distance((0, 0), (2, 4)), 4);
        assert_eq!(g.distance((0, 0), (1, 1)), 2);
    }

    #[test]
    fn test_hex_distance_is_symmetric() {
        let g = GridTopology::Hex;
        for a in [(0usize, 0usize), (3, 2), (1, 5), (4, 4)] {
            for b in [(2usize, 3usize), (0, 4), (5, 1)] {
                assert_eq!(g.distance(a, b), g.distance(b, a), "a={:?} b={:?}", a, b);
            }
        }
    }

    #[test]
    fn test_hex_neighbors_even_row() {
        let g = GridTopology::Hex;
        // 偶数行 (y=2): 斜め方向は自分より左の列
        let mut adj = g.neighbors(2, 2, 10, 10);
        adj.sort();
        assert_eq!(adj, vec![(1, 1), (1, 2), (1, 3), (2, 1), (2, 3), (3, 2)]);
    }

    #[test]
    fn test_hex_neighbors_odd_row() {
        let g = GridTopology::Hex;
        // 奇数行 (y=3): 斜め方向は自分より右の列
        let mut adj = g.neighbors(2, 3, 10, 10);
        adj.sort();
        assert_eq!(adj, vec![(1, 3), (2, 2), (2, 4), (3, 2), (3, 3), (3, 4)]);
    }

    #[test]
    fn test_hex_neighbors_clipped_at_border() {
        let g = GridTopology::Hex;
        // 左上隅 (0,0)：境界外は除外される
        let mut adj = g.neighbors(0, 0, 5, 5);
        adj.sort();
        assert_eq!(adj, vec![(0, 1), (1, 0)]);
        // 右下隅 (4,4)：偶数行なので斜めは左列のみ、下方向は境界外
        let mut adj = g.neighbors(4, 4, 5, 5);
        adj.sort();
        assert_eq!(adj, vec![(3, 3), (3, 4), (4, 3)]);
    }

    /// Map がトポロジーに応じて grid モジュールへ委譲していることの確認
    #[test]
    fn test_map_delegates_to_topology() {
        use crate::resources::{Map, Terrain};
        // ヘックスマップが panic せずに生成できること（既存ロジック非破壊の確認）
        let hex_map = Map::new(5, 5, Terrain::Plains, GridTopology::Hex);
        assert_eq!(hex_map.distance(0, 0, 2, 4), 4);
        assert_eq!(hex_map.get_adjacent(2, 3).len(), 6);

        let square_map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        assert_eq!(square_map.distance(0, 0, 2, 4), 6);
        assert_eq!(square_map.get_adjacent(2, 3).len(), 4);
    }

    /// 距離関数と隣接関数の整合性チェック：
    /// BFS（隣接関数によるステップ数）と distance の結果が一致すること
    #[test]
    fn test_hex_distance_matches_bfs() {
        let g = GridTopology::Hex;
        let (w, h) = (8usize, 8usize);
        let start = (3usize, 3usize);

        // BFS で全セルへの最短ステップ数を計算
        let mut steps: HashMap<(usize, usize), u32> = HashMap::new();
        let mut queue = VecDeque::new();
        steps.insert(start, 0);
        queue.push_back(start);
        while let Some(pos) = queue.pop_front() {
            let d = steps[&pos];
            for n in g.neighbors(pos.0, pos.1, w, h) {
                steps.entry(n).or_insert_with(|| {
                    queue.push_back(n);
                    d + 1
                });
            }
        }

        for y in 0..h {
            for x in 0..w {
                assert_eq!(
                    g.distance(start, (x, y)),
                    steps[&(x, y)],
                    "distance mismatch at ({}, {})",
                    x,
                    y
                );
            }
        }
    }
}
