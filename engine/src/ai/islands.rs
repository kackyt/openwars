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

#[derive(Debug, Clone, bevy_ecs::prelude::Resource)]
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
                        if !visited[n_idx]
                            && map.get_terrain(nx, ny).is_some_and(|t| t != Terrain::Sea)
                        {
                            visited[n_idx] = true;
                            queue.push_back((nx, ny));
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
        self.islands
            .iter()
            .find(|island| island.tiles.contains(pos))
    }

    /// 各島を「自軍の拠点島（Base）」と「目標島（Target）」に分類する
    pub fn classify_islands(
        &self,
        player_id: crate::components::PlayerId,
        properties: &std::collections::HashMap<GridPosition, Option<crate::components::PlayerId>>,
    ) -> (Vec<IslandId>, Vec<IslandId>) {
        let mut base_islands = Vec::new();
        let mut target_islands = Vec::new();

        for island in &self.islands {
            let mut has_own_property = false;
            let mut has_other_property = false;

            for tile in &island.tiles {
                if let Some(owner_id) = properties.get(tile) {
                    if *owner_id == Some(player_id) {
                        has_own_property = true;
                    } else {
                        has_other_property = true;
                    }
                }
            }

            // 自軍の拠点が1つでもあればBase Islandとみなす
            if has_own_property {
                base_islands.push(island.id);
            }

            // 他勢力や中立の未占領拠点があるならTarget Islandとしても扱う
            // （本島であっても、非常に遠い拠点への輸送は有効であるため、一律排除せず距離コストで判定する）
            if has_other_property {
                target_islands.push(island.id);
            }
        }

        (base_islands, target_islands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::master_data::MasterDataRegistry;
    use crate::resources::{Map, Terrain};

    #[test]
    #[ignore]
    fn test_print_map_3_islands() {
        let registry = MasterDataRegistry::load().unwrap();
        let map_data = registry.get_map("map_3").unwrap();

        let mut ecs_map = Map::new(
            map_data.width,
            map_data.height,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        for y in 0..map_data.height {
            for x in 0..map_data.width {
                if let Some(cell) = map_data.get_cell(x, y) {
                    let terrain = registry.terrain_from_id(cell.terrain_id).unwrap();
                    let _ = ecs_map.set_terrain(x, y, terrain);
                }
            }
        }

        let island_map = IslandMap::analyze(&ecs_map);
        println!("=== map_3 ISLANDS ANALYSIS ===");
        println!("Total islands: {}", island_map.islands.len());
        for island in &island_map.islands {
            println!("Island {:?}: {} tiles", island.id, island.tiles.len());
            let mut tiles: Vec<_> = island.tiles.iter().collect();
            tiles.sort_by_key(|p| (p.y, p.x));
            for tile in tiles.iter().take(15) {
                let cell = map_data.get_cell(tile.x, tile.y).unwrap();
                let terrain = registry.terrain_from_id(cell.terrain_id).unwrap();
                println!(
                    "  ({}, {}) = {:?} [player {}]",
                    tile.x, tile.y, terrain, cell.player_id
                );
            }
            if tiles.len() > 15 {
                println!("  ... and {} more tiles", tiles.len() - 15);
            }
        }
        panic!("Show stdout");
    }
}
