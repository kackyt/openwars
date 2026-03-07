use crate::domain::map_grid::{Map, Terrain};
use crate::domain::unit_roster::MovementType;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

/// Returns the movement cost for a given MovementType on a given Terrain.
/// Returns None if the terrain is impassable.
pub fn get_movement_cost(movement_type: MovementType, terrain: Terrain) -> Option<u32> {
    match movement_type {
        MovementType::Foot => match terrain {
            Terrain::Road
            | Terrain::Bridge
            | Terrain::Plains
            | Terrain::Shoal
            | Terrain::City
            | Terrain::Factory
            | Terrain::Airport
            | Terrain::Port
            | Terrain::Capital
            | Terrain::Forest => Some(1),
            Terrain::River | Terrain::Mountain => Some(2),
            Terrain::Sea => None,
        },
        MovementType::Vehicle => match terrain {
            Terrain::Road
            | Terrain::Bridge
            | Terrain::City
            | Terrain::Factory
            | Terrain::Airport
            | Terrain::Port
            | Terrain::Capital => Some(1),
            Terrain::Plains => Some(2),
            Terrain::Forest
            | Terrain::River
            | Terrain::Mountain
            | Terrain::Sea
            | Terrain::Shoal => None,
        },
        MovementType::Tracked => match terrain {
            Terrain::Road
            | Terrain::Bridge
            | Terrain::Plains
            | Terrain::City
            | Terrain::Factory
            | Terrain::Airport
            | Terrain::Port
            | Terrain::Capital => Some(1),
            Terrain::Forest => Some(2),
            Terrain::River | Terrain::Mountain | Terrain::Sea | Terrain::Shoal => None,
        },
        MovementType::Tires => match terrain {
            Terrain::Road
            | Terrain::Bridge
            | Terrain::City
            | Terrain::Factory
            | Terrain::Airport
            | Terrain::Port
            | Terrain::Capital => Some(1),
            Terrain::Plains => Some(2),
            Terrain::Forest => Some(3),
            Terrain::River | Terrain::Mountain | Terrain::Sea | Terrain::Shoal => None,
        },
        MovementType::LowAltitude | MovementType::HighAltitude => Some(1),
        MovementType::Ship => match terrain {
            Terrain::Sea | Terrain::Shoal | Terrain::Port => Some(1),
            _ => None,
        },
    }
}

pub struct MovementContext<'a> {
    pub map: &'a Map,
    pub unit_positions: HashMap<(usize, usize), u32>,
}

impl<'a> MovementContext<'a> {
    pub fn is_enemy_zoc(&self, player_id: u32, x: usize, y: usize) -> bool {
        let adj = self.map.get_adjacent(x, y);
        for &(nx, ny) in &adj {
            if let Some(&owner) = self.unit_positions.get(&(nx, ny)) {
                if owner != player_id {
                    return true;
                }
            }
        }
        false
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
struct State {
    cost: u32,
    fuel_used: u32,
    position: (usize, usize),
}

impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .cmp(&self.cost)
            .then_with(|| self.position.cmp(&other.position))
    }
}

impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn calculate_reachable_tiles(
    context: &MovementContext,
    start: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    max_fuel: u32,
    player_id: u32,
) -> HashSet<(usize, usize)> {
    let mut reachable = HashSet::new();
    let mut heap = BinaryHeap::new();
    let mut min_cost: HashMap<(usize, usize), u32> = HashMap::new();

    heap.push(State {
        cost: 0,
        fuel_used: 0,
        position: start,
    });
    min_cost.insert(start, 0);

    while let Some(State {
        cost,
        fuel_used,
        position,
    }) = heap.pop()
    {
        if let Some(&c) = min_cost.get(&position) {
            if cost > c {
                continue;
            }
        }

        reachable.insert(position);

        if fuel_used >= max_fuel {
            continue;
        }

        if position != start && context.is_enemy_zoc(player_id, position.0, position.1) {
            continue;
        }

        for (nx, ny) in context.map.get_adjacent(position.0, position.1) {
            if let Some(&owner) = context.unit_positions.get(&(nx, ny)) {
                if owner != player_id {
                    continue; // Enemy units are impassable
                }
            }

            if let Some(terrain) = context.map.get_terrain(nx, ny) {
                if let Some(terrain_cost) = get_movement_cost(movement_type, terrain) {
                    let next_cost = cost + terrain_cost;
                    let next_fuel = fuel_used + 1;

                    if next_cost <= max_mp && next_fuel <= max_fuel {
                        let is_better = min_cost.get(&(nx, ny)).map_or(true, |&c| next_cost < c);
                        if is_better {
                            min_cost.insert((nx, ny), next_cost);
                            heap.push(State {
                                cost: next_cost,
                                fuel_used: next_fuel,
                                position: (nx, ny),
                            });
                        }
                    }
                }
            }
        }
    }

    // Friendly units can be passed but cannot stop on them
    reachable.retain(|&pos| pos == start || context.unit_positions.get(&pos) != Some(&player_id));

    reachable
}

/// A-star algorithm to find shortest path within reach. Returns (Path, Cost, Fuel consumed)
pub fn find_path_a_star(
    context: &MovementContext,
    start: (usize, usize),
    goal: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    max_fuel: u32,
    player_id: u32,
) -> Option<(Vec<(usize, usize)>, u32, u32)> {
    let reachable =
        calculate_reachable_tiles(context, start, movement_type, max_mp, max_fuel, player_id);
    if !reachable.contains(&goal) {
        return None;
    }

    #[derive(Copy, Clone, Eq, PartialEq)]
    struct AStarState {
        cost: u32,
        fuel_used: u32,
        position: (usize, usize),
        f_score: u32,
    }

    impl Ord for AStarState {
        fn cmp(&self, other: &Self) -> Ordering {
            other
                .f_score
                .cmp(&self.f_score)
                .then_with(|| self.position.cmp(&other.position))
        }
    }
    impl PartialOrd for AStarState {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }

    let heuristic = |a: (usize, usize), b: (usize, usize)| -> u32 {
        ((a.0 as isize - b.0 as isize).abs() + (a.1 as isize - b.1 as isize).abs()) as u32
    };

    let mut heap = BinaryHeap::new();
    let mut came_from: HashMap<(usize, usize), (usize, usize)> = HashMap::new();
    let mut g_score: HashMap<(usize, usize), u32> = HashMap::new();
    let mut fuel_score: HashMap<(usize, usize), u32> = HashMap::new();

    g_score.insert(start, 0);
    fuel_score.insert(start, 0);
    heap.push(AStarState {
        cost: 0,
        fuel_used: 0,
        position: start,
        f_score: heuristic(start, goal),
    });

    while let Some(AStarState {
        cost,
        fuel_used,
        position,
        ..
    }) = heap.pop()
    {
        if position == goal {
            let mut curr = goal;
            let mut path = vec![curr];
            while let Some(&prev) = came_from.get(&curr) {
                curr = prev;
                path.push(curr);
            }
            path.reverse();
            return Some((path, cost, fuel_used));
        }

        if let Some(&g) = g_score.get(&position) {
            if cost > g {
                continue;
            }
        }

        if fuel_used >= max_fuel {
            continue;
        }
        if position != start && context.is_enemy_zoc(player_id, position.0, position.1) {
            continue;
        }

        for (nx, ny) in context.map.get_adjacent(position.0, position.1) {
            if let Some(&owner) = context.unit_positions.get(&(nx, ny)) {
                if owner != player_id && (nx, ny) != goal {
                    continue; // Enemy, can't pass
                }
            }

            if let Some(terrain) = context.map.get_terrain(nx, ny) {
                if let Some(terrain_cost) = get_movement_cost(movement_type, terrain) {
                    let next_cost = cost + terrain_cost;
                    let next_fuel = fuel_used + 1;

                    if next_cost <= max_mp && next_fuel <= max_fuel {
                        let is_better = g_score.get(&(nx, ny)).map_or(true, |&g| next_cost < g);
                        if is_better {
                            g_score.insert((nx, ny), next_cost);
                            fuel_score.insert((nx, ny), next_fuel);
                            came_from.insert((nx, ny), position);
                            heap.push(AStarState {
                                cost: next_cost,
                                fuel_used: next_fuel,
                                position: (nx, ny),
                                f_score: next_cost + heuristic((nx, ny), goal),
                            });
                        }
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_foot_movement_cost() {
        assert_eq!(
            get_movement_cost(MovementType::Foot, Terrain::Plains),
            Some(1)
        );
        assert_eq!(
            get_movement_cost(MovementType::Foot, Terrain::Mountain),
            Some(2)
        );
        assert_eq!(get_movement_cost(MovementType::Foot, Terrain::Sea), None);
    }

    #[test]
    fn test_tracked_movement_cost() {
        assert_eq!(
            get_movement_cost(MovementType::Tracked, Terrain::Road),
            Some(1)
        );
        assert_eq!(
            get_movement_cost(MovementType::Tracked, Terrain::Forest),
            Some(2)
        );
        assert_eq!(
            get_movement_cost(MovementType::Tracked, Terrain::Mountain),
            None
        );
    }

    #[test]
    fn test_reachable_tiles_zoc_and_fuel() {
        let mut map = Map::new(
            5,
            5,
            Terrain::Plains,
            crate::domain::map_grid::GridTopology::Square,
        );
        map.set_terrain(2, 2, Terrain::Mountain).unwrap();
        map.set_terrain(1, 1, Terrain::Road).unwrap();
        map.set_terrain(1, 2, Terrain::Road).unwrap();
        map.set_terrain(1, 3, Terrain::Road).unwrap();

        let mut unit_positions = std::collections::HashMap::new();
        unit_positions.insert((3, 3), 2);

        let context = MovementContext {
            map: &map,
            unit_positions,
        };

        let reachable = calculate_reachable_tiles(&context, (1, 1), MovementType::Vehicle, 4, 5, 1);

        // Can reach (1,2) and (1,3)
        assert!(reachable.contains(&(1, 2)));
        assert!(reachable.contains(&(1, 3)));

        // Cannot reach (2,2) Mountain
        assert!(!reachable.contains(&(2, 2)));

        // ZoC Check: Player 2 at (3,3)
        // ZoC is (2,3), (3,2), (4,3), (3,4)
        // Path to (2,4) through (2,3) should be blocked because we must stop at (2,3)
        // Even if we had enough MP, (2,4) should not be reachable via ZoC.
        assert!(!reachable.contains(&(2, 4)));
    }

    #[test]
    fn test_fuel_limit() {
        let map = Map::new(
            5,
            5,
            Terrain::Road,
            crate::domain::map_grid::GridTopology::Square,
        );
        let context = MovementContext {
            map: &map,
            unit_positions: std::collections::HashMap::new(),
        };

        let reachable = calculate_reachable_tiles(&context, (2, 2), MovementType::Foot, 10, 2, 1);

        assert!(reachable.contains(&(2, 0)));
        assert!(reachable.contains(&(2, 4)));
        assert!(reachable.contains(&(0, 2)));

        assert!(!reachable.contains(&(4, 4)));
        assert!(!reachable.contains(&(1, 0)));
    }
}
