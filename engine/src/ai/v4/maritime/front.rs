//! 細い陸路（隘路）だけで結ばれた前線。
//!
//! 首都同士が戦車の通れる1〜2本の細い陸路だけで結ばれている盤面（例: map_49 の地峡）では、
//! 全軍で陸路を進むと隘路に部隊が詰まり、本拠の戦力が前線へ届かない。両陣営とも港を持ち、
//! 海路で相手側の海岸へ届き、隘路が拠点の少ない通り道なら、戦力を2つに分けて同時に攻める。
//!
//! - 陸路の部隊: 隘路を押し進む部隊。隘路で戦える数（経路ごとの先頭と後詰め）まで、
//!   間接射撃の部隊と隘路に近い部隊を割り当てる（[`super::landmass`]）。
//! - 海路の部隊: 残りの部隊。揚陸作戦で相手側の海岸へ送る（[`super::sealift`]）。
//!
//! 隘路は陸塊から外し、両側を別の陸塊とみなす。隘路に入った部隊はどの陸塊にも属さず、
//! 通常の陸上作戦のまま進む。戦艦は隘路の出口の敵を砲撃して陸路の部隊を支援する
//! （[`super::naval_combat`]）。

use super::theater::{Components, is_navigable, is_passable, sea_distances};
use crate::ai::context::board::PlayerBoard;
use crate::collections::HashSet;
use crate::components::{GridPosition, PlayerId};
use crate::resources::{MovementType, Terrain};
use std::collections::VecDeque;

/// 首都同士を結ぶ戦車の陸路が、この本数（互いにセルを共有しない経路の数）以下なら隘路とみなす。
const NARROW_FRONT_PATHS: usize = 2;

/// 隘路とみなす、隘路のセルに対する拠点の数の上限（%）。
///
/// 地峡や橋のような通り道には拠点が少なく、陸路は部隊の通り抜けで頭打ちになるため、
/// 陸路と海路へ戦力を分けると効く。拠点が密集した区間（セル3つに1つを超える）は、
/// 拠点を取り合う主戦場なので全軍の陸上作戦に任せる。
const MAX_CORRIDOR_PROPERTY_PERCENT: usize = 30;

/// 盤面のセル座標。
type Cell = (usize, usize);

/// 自軍の首都と敵の首都を結ぶ隘路。
#[derive(Debug, Clone, Default)]
pub(crate) struct NarrowFront {
    /// 隘路のセル（両側の最小の切断セルと、その間のセル）。
    pub(crate) corridor: HashSet<Cell>,
    /// 隘路より自軍の首都側の、戦車が通れるセル。
    pub(crate) home_side: HashSet<Cell>,
    /// 首都同士を結ぶ、互いにセルを共有しない戦車の陸路の数。
    pub(crate) paths: usize,
}

impl NarrowFront {
    /// 自軍の首都と敵の首都が隘路だけで結ばれ、海路でも行き来できるなら、その隘路。
    pub(crate) fn observe(board: &PlayerBoard, player_id: PlayerId) -> Option<Self> {
        let capital = |own: bool| {
            board.properties.iter().find(|property| {
                property.terrain == Terrain::Capital
                    && property
                        .owner
                        .is_some_and(|owner| (owner == player_id) == own)
            })
        };
        let home = capital(true)?.position;
        let enemy = capital(false)?.position;
        Self::between(board, home, enemy)
    }

    /// [`Self::observe`] の隘路のうち、陸塊の塗り分け `land` が隘路で両側を分けたもの
    /// （隘路のセルがどの陸塊にも属さない）。歩兵の山道などで迂回できる盤面では分けない。
    pub(crate) fn observe_split(
        board: &PlayerBoard,
        player_id: PlayerId,
        land: &Components,
    ) -> Option<Self> {
        Self::observe(board, player_id).filter(|front| {
            front.corridor.iter().all(|cell| {
                land.at(GridPosition {
                    x: cell.0,
                    y: cell.1,
                })
                .is_none()
            })
        })
    }

    /// `a` と `b` を結ぶ戦車の陸路が隘路で、両側の港から海路で相手側の海岸へ届くなら、
    /// `a` 側から見た隘路。
    fn between(board: &PlayerBoard, a: GridPosition, b: GridPosition) -> Option<Self> {
        let vehicle = |x: usize, y: usize| is_passable(board, MovementType::Tank, x, y);
        let (home_side, paths) = near_side(board, &vehicle, a, b)?;
        let (enemy_side, _) = near_side(board, &vehicle, b, a)?;
        if !sea_links(board, &home_side, &enemy_side) {
            return None;
        }
        // 両側の手前の領域に入らずに、戦車の陸路でつながるセルが隘路。
        let mut corridor: HashSet<Cell> = HashSet::default();
        let mut queue: VecDeque<Cell> = home_side
            .iter()
            .flat_map(|cell| board.map.get_adjacent(cell.0, cell.1))
            .filter(|cell| {
                vehicle(cell.0, cell.1) && !home_side.contains(cell) && !enemy_side.contains(cell)
            })
            .collect();
        while let Some(cell) = queue.pop_front() {
            if !corridor.insert(cell) {
                continue;
            }
            for next in board.map.get_adjacent(cell.0, cell.1) {
                if vehicle(next.0, next.1)
                    && !home_side.contains(&next)
                    && !enemy_side.contains(&next)
                    && !corridor.contains(&next)
                {
                    queue.push_back(next);
                }
            }
        }
        // 拠点の密集した隘路は、通り道ではなく全軍で取り合う主戦場として扱う。
        let properties = board
            .properties
            .iter()
            .filter(|property| corridor.contains(&(property.position.x, property.position.y)))
            .count();
        if properties * 100 > corridor.len() * MAX_CORRIDOR_PROPERTY_PERCENT {
            return None;
        }
        Some(Self {
            corridor,
            home_side,
            paths,
        })
    }

    /// 陸路の部隊として隘路へ割り当てる数。
    ///
    /// 細い陸路で敵と撃ち合えるのは先頭の数部隊だけで、それより多く送っても隘路に並んで
    /// 待つだけになる。経路1本あたり [`LAND_FORCE_PER_PATH`] 部隊（先頭と後詰め、後ろから
    /// 撃つ間接射撃）までを陸路に割り当てる。
    pub(crate) fn land_force_quota(&self) -> usize {
        self.paths * LAND_FORCE_PER_PATH
    }
}

/// 陸路の部隊の数（隘路の経路1本あたり）。
const LAND_FORCE_PER_PATH: usize = 6;

/// 首都の組 `pairs` のうち、隘路だけで結ばれ海路でも行き来できる組の隘路を `land` から外す。
pub(crate) fn split_narrow_fronts(
    board: &PlayerBoard,
    mut land: Components,
    pairs: &[(GridPosition, GridPosition)],
) -> Components {
    for (a, b) in pairs {
        if land.at(*a).is_none() || land.at(*a) != land.at(*b) {
            continue;
        }
        let Some(front) = NarrowFront::between(board, *a, *b) else {
            continue;
        };
        let split = Components::label_where(board, |x, y| {
            land.at(GridPosition { x, y }).is_some() && !front.corridor.contains(&(x, y))
        });
        // 歩兵の山道などで隘路を迂回できるなら分けない。
        if split.at(*a).is_some() && split.at(*b).is_some() && split.at(*a) != split.at(*b) {
            land = split;
        }
    }
    land
}

/// `from` から `to` への戦車の陸路が [`NARROW_FRONT_PATHS`] 本以下なら、`from` に最も近い
/// 最小の切断セルの手前（`from` 側）の領域と陸路の本数。陸路が無い・隘路でないならNone。
///
/// セルを入口・出口の2節点に分け、セル自体の容量を1とした最大流で求める。
fn near_side(
    board: &PlayerBoard,
    vehicle: &impl Fn(usize, usize) -> bool,
    from: GridPosition,
    to: GridPosition,
) -> Option<(HashSet<Cell>, usize)> {
    let map = &board.map;
    let width = map.width;
    let index = |x: usize, y: usize| y * width + x;
    let mut graph = FlowGraph::new(2 * width * map.height);
    for y in 0..map.height {
        for x in 0..width {
            if !vehicle(x, y) {
                continue;
            }
            let cell = index(x, y);
            graph.add_edge(2 * cell, 2 * cell + 1, 1);
            for (nx, ny) in map.get_adjacent(x, y) {
                if vehicle(nx, ny) {
                    graph.add_edge(2 * cell + 1, 2 * index(nx, ny), NARROW_FRONT_PATHS + 1);
                }
            }
        }
    }
    let source = 2 * index(from.x, from.y) + 1;
    let sink = 2 * index(to.x, to.y);
    let mut flow = 0;
    while flow <= NARROW_FRONT_PATHS && graph.augment(source, sink) {
        flow += 1;
    }
    if flow == 0 || flow > NARROW_FRONT_PATHS {
        return None;
    }
    // 残余グラフで出口へ届くセルが、最小の切断セルより手前の領域。
    let reached = graph.reachable(source);
    let side = (0..map.height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .filter(|(x, y)| reached[2 * index(*x, *y) + 1])
        .collect();
    Some((side, flow))
}

/// 両側に港があり、`a_side` 側の港から海路で `b_side` の海岸へ届くか。
fn sea_links(board: &PlayerBoard, a_side: &HashSet<Cell>, b_side: &HashSet<Cell>) -> bool {
    let ports_in = |side: &HashSet<Cell>| -> Vec<Cell> {
        board
            .properties
            .iter()
            .filter(|property| property.terrain == Terrain::Port)
            .map(|property| (property.position.x, property.position.y))
            .filter(|cell| side.contains(cell))
            .collect()
    };
    let a_ports = ports_in(a_side);
    if a_ports.is_empty() || ports_in(b_side).is_empty() {
        return false;
    }
    let sea = sea_distances(board, a_ports);
    b_side.iter().any(|cell| {
        board
            .map
            .get_adjacent(cell.0, cell.1)
            .into_iter()
            .any(|next| is_navigable(board, next.0, next.1) && sea.contains_key(&next))
    })
}

/// 容量付き有向グラフの最大流（増加路を幅優先で探す）。
struct FlowGraph {
    edges: Vec<Vec<usize>>,
    to: Vec<usize>,
    capacity: Vec<usize>,
}

impl FlowGraph {
    fn new(nodes: usize) -> Self {
        Self {
            edges: vec![Vec::new(); nodes],
            to: Vec::new(),
            capacity: Vec::new(),
        }
    }

    /// 辺と、その逆辺（容量0）を加える。辺の番号は対で、`e ^ 1` が逆辺。
    fn add_edge(&mut self, from: usize, to: usize, capacity: usize) {
        self.edges[from].push(self.to.len());
        self.to.push(to);
        self.capacity.push(capacity);
        self.edges[to].push(self.to.len());
        self.to.push(from);
        self.capacity.push(0);
    }

    /// `source` から `sink` へ1単位流せたらtrue。
    fn augment(&mut self, source: usize, sink: usize) -> bool {
        let mut via = vec![None; self.edges.len()];
        let mut seen = vec![false; self.edges.len()];
        seen[source] = true;
        let mut queue = VecDeque::from([source]);
        while let Some(node) = queue.pop_front() {
            if node == sink {
                break;
            }
            for &edge in &self.edges[node] {
                let next = self.to[edge];
                if self.capacity[edge] > 0 && !seen[next] {
                    seen[next] = true;
                    via[next] = Some(edge);
                    queue.push_back(next);
                }
            }
        }
        if !seen[sink] {
            return false;
        }
        let mut node = sink;
        while let Some(edge) = via[node] {
            self.capacity[edge] -= 1;
            self.capacity[edge ^ 1] += 1;
            node = self.to[edge ^ 1];
        }
        true
    }

    /// 残余グラフで `source` から届く節点。
    fn reachable(&self, source: usize) -> Vec<bool> {
        let mut seen = vec![false; self.edges.len()];
        seen[source] = true;
        let mut queue = VecDeque::from([source]);
        while let Some(node) = queue.pop_front() {
            for &edge in &self.edges[node] {
                let next = self.to[edge];
                if self.capacity[edge] > 0 && !seen[next] {
                    seen[next] = true;
                    queue.push_back(next);
                }
            }
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::super::landmass::label_landmasses;
    use crate::ai::v4::maritime::test_support::board_from_rows;
    use crate::components::GridPosition;

    fn at(x: usize, y: usize) -> GridPosition {
        GridPosition { x, y }
    }

    /// 両陣営の首都・港の位置（首都 (1,1)・(1,6)、港 (2,1)・(2,6)）。
    const PROPERTIES: [(usize, usize, u32); 4] = [(1, 1, 1), (2, 1, 1), (1, 6, 2), (2, 6, 2)];

    #[test]
    fn narrow_isthmus_with_ports_on_both_sides_splits_the_landmass() {
        // 首都同士は1マス幅の地峡でしかつながらない。左端の海で港同士が行き来できる。
        let rows = [
            "~~~~~~~", "~HP...~", "~.....~", "~~~.~~~", "~~~.~~~", "~.....~", "~HP...~", "~~~~~~~",
        ];
        let board = board_from_rows(&rows, &PROPERTIES);
        let land = label_landmasses(&board);
        assert!(land.at(at(1, 1)).is_some());
        assert!(land.at(at(1, 6)).is_some());
        assert_ne!(land.at(at(1, 1)), land.at(at(1, 6)));
        // 地峡のセルはどちらの陸塊にも属さない。
        assert_eq!(land.at(at(3, 3)), None);
        assert_eq!(land.at(at(3, 4)), None);
    }

    #[test]
    fn wide_land_front_keeps_one_landmass() {
        // 3マス幅の陸路は隘路ではない。
        let rows = [
            "~~~~~~~", "~HP...~", "~.....~", "~~...~~", "~~...~~", "~.....~", "~HP...~", "~~~~~~~",
        ];
        let board = board_from_rows(&rows, &PROPERTIES);
        let land = label_landmasses(&board);
        assert_eq!(land.at(at(1, 1)), land.at(at(1, 6)));
    }

    #[test]
    fn narrow_isthmus_without_ports_keeps_one_landmass() {
        // 港が無ければ海路で回り込めないので、地峡を通るしかない。
        let rows = [
            "~~~~~~~", "~H....~", "~.....~", "~~~.~~~", "~~~.~~~", "~.....~", "~H....~", "~~~~~~~",
        ];
        let board = board_from_rows(&rows, &[(1, 1, 1), (1, 6, 2)]);
        let land = label_landmasses(&board);
        assert_eq!(land.at(at(1, 1)), land.at(at(1, 6)));
    }

    #[test]
    fn isthmus_crowded_with_cities_is_a_battlefield_not_a_corridor() {
        // 地峡の2マスとも都市。拠点の密集した区間は全軍で取り合う主戦場として、分けない。
        let rows = [
            "~~~~~~~", "~HP...~", "~.....~", "~~~C~~~", "~~~C~~~", "~.....~", "~HP...~", "~~~~~~~",
        ];
        let mut properties = PROPERTIES.to_vec();
        properties.extend([(3, 3, 0), (3, 4, 0)]);
        let board = board_from_rows(&rows, &properties);
        let land = label_landmasses(&board);
        assert_eq!(land.at(at(1, 1)), land.at(at(1, 6)));
    }
}
