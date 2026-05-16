use crate::components::GridPosition;
use crate::resources::{Map, Terrain};
use std::collections::{HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IslandId(pub usize);

#[derive(Debug, Clone)]
pub struct Island {
    pub id: IslandId,
    pub tiles: HashSet<GridPosition>,
}

#[derive(Debug, Clone)]
pub struct IslandMap {
    pub islands: Vec<Island>,
}

impl IslandMap {
    /// マップ全体を走査し、海（Sea）以外の連続するマスを1つの「島」として認識する
    pub fn analyze(map: &Map) -> Self {
        let mut visited = vec![false; map.width * map.height];
        let mut islands = Vec::new();
        let mut next_id = 0;

        for y in 0..map.height {
            for x in 0..map.width {
                let idx = y * map.width + x;
                if visited[idx] {
                    continue;
                }

                let terrain = map.get_terrain(x, y).unwrap();
                // 今回はシンプルに、海(Sea)以外を陸地（または浅瀬など）として島に含める
                if terrain == Terrain::Sea {
                    visited[idx] = true;
                    continue;
                }

                // フラッドフィルによる島の検出
                let mut island_tiles = HashSet::new();
                let mut queue = VecDeque::new();
                queue.push_back((x, y));
                visited[idx] = true;

                while let Some((cx, cy)) = queue.pop_front() {
                    island_tiles.insert(GridPosition { x: cx, y: cy });

                    for (nx, ny) in map.get_adjacent(cx, cy) {
                        let n_idx = ny * map.width + nx;
                        if !visited[n_idx] {
                            if let Some(n_terrain) = map.get_terrain(nx, ny) {
                                if n_terrain != Terrain::Sea {
                                    visited[n_idx] = true;
                                    queue.push_back((nx, ny));
                                }
                            }
                        }
                    }
                }

                islands.push(Island {
                    id: IslandId(next_id),
                    tiles: island_tiles,
                });
                next_id += 1;
            }
        }

        Self { islands }
    }

    /// 指定した座標が属する島を返す
    pub fn get_island_at(&self, pos: &GridPosition) -> Option<&Island> {
        self.islands.iter().find(|island| island.tiles.contains(pos))
    }
}
