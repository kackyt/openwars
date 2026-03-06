#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terrain {
    Road,     // 道路
    Bridge,   // 橋
    Plains,   // 平地
    River,    // 川
    Forest,   // 森
    Mountain, // 山
    Sea,      // 海
    Shoal,    // 浅瀬
    City,     // 都市
    Factory,  // 工場
    Airport,  // 空港
    Port,     // 港
    Capital,  // 首都
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridTopology {
    Square,    // 4方向または8方向（今回は主に4方向を想定）
    OffsetHex, // 奇数・偶数行で半マスずれた6方向接点
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Map {
    pub width: usize,
    pub height: usize,
    pub topology: GridTopology,
    tiles: Vec<Terrain>,
}

impl Map {
    pub fn new(
        width: usize,
        height: usize,
        default_terrain: Terrain,
        topology: GridTopology,
    ) -> Self {
        Self {
            width,
            height,
            topology,
            tiles: vec![default_terrain; width * height],
        }
    }

    pub fn set_terrain(
        &mut self,
        x: usize,
        y: usize,
        terrain: Terrain,
    ) -> Result<(), &'static str> {
        if self.in_bounds(x, y) {
            let index = self.index(x, y);
            self.tiles[index] = terrain;
            Ok(())
        } else {
            Err("Out of bounds")
        }
    }

    pub fn get_terrain(&self, x: usize, y: usize) -> Option<Terrain> {
        if self.in_bounds(x, y) {
            Some(self.tiles[self.index(x, y)])
        } else {
            None
        }
    }

    pub fn get_adjacent(&self, x: usize, y: usize) -> Vec<(usize, usize)> {
        let mut adj = Vec::new();
        if !self.in_bounds(x, y) {
            return adj;
        }

        let ix = x as isize;
        let iy = y as isize;

        let offsets: &[(isize, isize)] = match self.topology {
            GridTopology::Square => &[(0, -1), (0, 1), (-1, 0), (1, 0)],
            GridTopology::OffsetHex => {
                // If odd rows are shifted half a step right:
                // even row y:
                //   (-1,-1)  (0,-1)
                // (-1, 0)  x  (1, 0)
                //   (-1, 1)  (0, 1)
                //
                // odd row y:
                //   (0,-1)  (1,-1)
                // (-1, 0)  x  (1, 0)
                //   (0, 1)  (1, 1)
                if y % 2 == 0 {
                    &[(-1, -1), (0, -1), (-1, 0), (1, 0), (-1, 1), (0, 1)]
                } else {
                    &[(0, -1), (1, -1), (-1, 0), (1, 0), (0, 1), (1, 1)]
                }
            }
        };

        for &(dx, dy) in offsets {
            let nx = ix + dx;
            let ny = iy + dy;
            if nx >= 0 && nx < self.width as isize && ny >= 0 && ny < self.height as isize {
                adj.push((nx as usize, ny as usize));
            }
        }

        adj
    }

    fn index(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }

    fn in_bounds(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_creation_and_query() {
        let mut map = Map::new(10, 10, Terrain::Plains, GridTopology::Square);

        assert_eq!(map.get_terrain(0, 0), Some(Terrain::Plains));

        // Add specific terrains
        assert!(map.set_terrain(1, 1, Terrain::Forest).is_ok());
        assert!(map.set_terrain(2, 2, Terrain::Mountain).is_ok());
        assert!(map.set_terrain(3, 3, Terrain::Capital).is_ok());

        assert_eq!(map.get_terrain(1, 1), Some(Terrain::Forest));
        assert_eq!(map.get_terrain(2, 2), Some(Terrain::Mountain));
        assert_eq!(map.get_terrain(3, 3), Some(Terrain::Capital));

        // Out of bounds
        assert_eq!(map.get_terrain(10, 10), None);
        assert!(map.set_terrain(10, 10, Terrain::Sea).is_err());
    }

    #[test]
    fn test_get_adjacent_square() {
        let map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        let mut adj = map.get_adjacent(2, 2);
        adj.sort();
        assert_eq!(adj, vec![(1, 2), (2, 1), (2, 3), (3, 2)]);
    }

    #[test]
    fn test_get_adjacent_offset_hex() {
        let map = Map::new(5, 5, Terrain::Plains, GridTopology::OffsetHex);

        // Even row (y=2)
        let mut adj_even = map.get_adjacent(2, 2);
        adj_even.sort();
        assert_eq!(
            adj_even,
            vec![(1, 1), (1, 2), (1, 3), (2, 1), (2, 3), (3, 2)]
        );

        // Odd row (y=1)
        let mut adj_odd = map.get_adjacent(2, 1);
        adj_odd.sort();
        assert_eq!(
            adj_odd,
            vec![(1, 1), (2, 0), (2, 2), (3, 0), (3, 1), (3, 2)]
        );
    }
}
