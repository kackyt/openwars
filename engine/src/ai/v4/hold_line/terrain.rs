use crate::collections::{HashMap, HashSet};
use crate::components::GridPosition;
use crate::resources::MovementType;
use crate::resources::{Map, master_data::MasterDataRegistry};
use crate::systems::movement::get_valid_movement_cost;
use std::collections::VecDeque;

pub(crate) struct TerrainLine {
    pub(crate) cells: Vec<GridPosition>,
    pub(crate) home: HashSet<GridPosition>,
}

// 初期地形のみを使い、複数端点からの最短地上距離を求める。
fn distances(
    map: &Map,
    ground: &HashMap<GridPosition, u32>,
    starts: &[GridPosition],
    blocked: &HashSet<GridPosition>,
) -> HashMap<GridPosition, usize> {
    let mut result = HashMap::default();
    let mut queue = VecDeque::new();
    for &pos in starts {
        if ground.contains_key(&pos) && !blocked.contains(&pos) && !result.contains_key(&pos) {
            result.insert(pos, 0);
            queue.push_back(pos);
        }
    }
    while let Some(pos) = queue.pop_front() {
        let next_distance = result[&pos] + 1;
        for (x, y) in map.get_adjacent(pos.x, pos.y) {
            let next = GridPosition { x, y };
            if ground.contains_key(&next) && !blocked.contains(&next) && !result.contains_key(&next)
            {
                result.insert(next, next_distance);
                queue.push_back(next);
            }
        }
    }
    result
}

pub(crate) fn select_terrain_line(
    map: &Map,
    master: &MasterDataRegistry,
    ours: &[GridPosition],
    theirs: &[GridPosition],
) -> Option<TerrainLine> {
    let types = [
        MovementType::Infantry,
        MovementType::Tank,
        MovementType::Artillery,
        MovementType::ArmoredCar,
    ];
    let mut ground = HashMap::default();
    for y in 0..map.height {
        for x in 0..map.width {
            let terrain = map.get_terrain(x, y)?;
            if types
                .iter()
                .any(|&kind| get_valid_movement_cost(master, kind, terrain).is_some())
            {
                ground.insert(
                    GridPosition { x, y },
                    master.get_terrain_defense_bonus(terrain),
                );
            }
        }
    }
    let empty = HashSet::default();
    let our_dist = distances(map, &ground, ours, &empty);
    let enemy_dist = distances(map, &ground, theirs, &empty);
    if !theirs.iter().any(|p| our_dist.contains_key(p)) {
        return None;
    }
    let allowed: HashSet<_> = ground
        .keys()
        .copied()
        .filter(|p| {
            our_dist.contains_key(p)
                && enemy_dist.contains_key(p)
                && !ours.contains(p)
                && !theirs.contains(p)
        })
        .collect();
    let mut levels: Vec<_> = allowed.iter().map(|p| ground[p]).collect();
    levels.sort_unstable_by(|a, b| b.cmp(a));
    levels.dedup();
    let separates = |blocked: &HashSet<GridPosition>| {
        let reached = distances(map, &ground, ours, blocked);
        !theirs.iter().any(|p| reached.contains_key(p))
    };
    let mut candidates: Vec<Vec<GridPosition>> = Vec::new();
    for level in levels {
        let blocked: HashSet<_> = allowed
            .iter()
            .copied()
            .filter(|p| ground[p] >= level)
            .collect();
        if !separates(&blocked) {
            continue;
        }
        for starts in [ours, theirs] {
            let area = distances(map, &ground, starts, &blocked);
            let mut frontier = HashSet::default();
            for pos in area.keys() {
                for (x, y) in map.get_adjacent(pos.x, pos.y) {
                    let neighbor = GridPosition { x, y };
                    if blocked.contains(&neighbor) {
                        frontier.insert(neighbor);
                    }
                }
            }
            // 低防御、敵から遠い順に戻し、分離を壊さない余分なマスを削る。
            let mut order: Vec<_> = frontier.iter().copied().collect();
            order.sort_unstable_by_key(|p| (ground[p], std::cmp::Reverse(enemy_dist[p]), p.y, p.x));
            for pos in order {
                frontier.remove(&pos);
                if !separates(&frontier) {
                    frontier.insert(pos);
                }
            }
            if frontier.is_empty() || !separates(&frontier) {
                continue;
            }
            let mut cells: Vec<_> = frontier.into_iter().collect();
            cells.sort_unstable_by_key(|p| (p.y, p.x));
            if !candidates.contains(&cells) {
                candidates.push(cells);
            }
        }
    }
    // 平均は整数の交差積で比較し、端数による同点を避ける。
    candidates.sort_unstable_by(|a, b| {
        let defense = |v: &[GridPosition]| v.iter().map(|p| u128::from(ground[p])).sum::<u128>();
        let distance = |v: &[GridPosition]| v.iter().map(|p| enemy_dist[p] as u128).sum::<u128>();
        let (al, bl) = (a.len() as u128, b.len() as u128);
        (defense(b) * al)
            .cmp(&(defense(a) * bl))
            .then_with(|| (distance(a) * bl).cmp(&(distance(b) * al)))
            .then_with(|| a.len().cmp(&b.len()))
            .then_with(|| {
                a.iter()
                    .map(|p| (p.y, p.x))
                    .cmp(b.iter().map(|p| (p.y, p.x)))
            })
    });
    let cells = candidates.into_iter().next()?;
    let blocked = cells.iter().copied().collect();
    let home = distances(map, &ground, ours, &blocked)
        .into_keys()
        .collect();
    Some(TerrainLine { cells, home })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{GridTopology, Terrain};
    use crate::setup::initialize_world_from_master_data_with_topology;

    fn p(x: usize, y: usize) -> GridPosition {
        GridPosition { x, y }
    }

    #[test]
    fn terrain_line_selects_all_nine_forest_cells() {
        let master = MasterDataRegistry::load().unwrap();
        let (world, _) =
            initialize_world_from_master_data_with_topology(&master, "map_42", GridTopology::Hex)
                .unwrap();
        let map = world.resource::<Map>();
        let ours: Vec<_> = (3..=7).map(|x| p(x, 3)).collect();
        let theirs: Vec<_> = (3..=7).map(|x| p(x, 11)).collect();
        let line = select_terrain_line(map, &master, &ours, &theirs).unwrap();
        assert_eq!(line.cells, (1..=9).map(|x| p(x, 7)).collect::<Vec<_>>());
        assert!(
            ours.iter()
                .all(|v| line.home.contains(v) && !line.cells.contains(v))
        );
        assert!(
            theirs
                .iter()
                .all(|v| !line.home.contains(v) && !line.cells.contains(v))
        );
    }

    #[test]
    fn terrain_line_requires_both_routes() {
        let master = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(5, 5, Terrain::Sea, GridTopology::Square);
        for y in 0..5 {
            for x in [0, 4] {
                map.set_terrain(x, y, Terrain::Plains).unwrap();
            }
        }
        for y in [1, 3] {
            for x in 1..4 {
                map.set_terrain(x, y, Terrain::Bridge).unwrap();
            }
        }
        let line = select_terrain_line(&map, &master, &[p(0, 2)], &[p(4, 2)]).unwrap();
        assert_eq!(line.cells.len(), 2);
        assert!(line.cells.iter().any(|v| v.y == 1));
        assert!(line.cells.iter().any(|v| v.y == 3));
    }

    #[test]
    fn terrain_line_skips_disconnected_sides() {
        let master = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Plains).unwrap();
        map.set_terrain(2, 0, Terrain::Plains).unwrap();
        assert!(select_terrain_line(&map, &master, &[p(0, 0)], &[p(2, 0)]).is_none());
    }
}
