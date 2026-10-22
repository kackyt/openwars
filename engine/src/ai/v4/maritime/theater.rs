//! 海上・航空戦域の観測。
//!
//! 地上部隊が陸路で届かない拠点（島）が残っているか、自軍の港から出た艦船が敵の
//! 部隊・拠点を射程へ捉えられるか、敵が艦船を持っているかを盤面から求める。
//! 「港がある」だけでは艦船を使う理由にならない。陸続きの盤面で敵が艦船を持たない
//! 場合（例: 首都同士が同じ陸塊にある小島マップ）は、艦船が戦闘に寄与しないと判定する。

use crate::ai::context::board::PlayerBoard;
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, PlayerId};
use crate::resources::{Map, MovementType, Terrain, UnitType};
use std::collections::VecDeque;

/// 戦艦の主砲（対艦ミサイル）の最大射程。射撃位置の探索範囲に使う。
pub(crate) const NAVAL_GUN_MAX_RANGE: u32 = 6;

/// 移動タイプごとの通行可否で塗り分けた連結成分。
#[derive(Debug, Clone)]
pub(crate) struct Components {
    width: usize,
    labels: Vec<Option<u32>>,
}

impl Components {
    /// 指定移動タイプで通行できるセルを、隣接で連結した成分へ塗り分ける。
    pub(crate) fn label(board: &PlayerBoard, movement: MovementType) -> Self {
        let map = &board.map;
        let passable = |x: usize, y: usize| {
            map.get_terrain(x, y).is_some_and(|terrain| {
                board
                    .registry
                    .get_movement_cost(movement, terrain.as_str())
                    .is_some_and(|cost| cost < 99)
            })
        };
        let mut labels = vec![None; map.width * map.height];
        let mut next = 0_u32;
        for y in 0..map.height {
            for x in 0..map.width {
                if labels[y * map.width + x].is_some() || !passable(x, y) {
                    continue;
                }
                // 幅優先で同じ成分のセルへ同じ番号を付ける。
                let mut queue = VecDeque::from([(x, y)]);
                labels[y * map.width + x] = Some(next);
                while let Some((cx, cy)) = queue.pop_front() {
                    for (nx, ny) in map.get_adjacent(cx, cy) {
                        let index = ny * map.width + nx;
                        if labels[index].is_none() && passable(nx, ny) {
                            labels[index] = Some(next);
                            queue.push_back((nx, ny));
                        }
                    }
                }
                next += 1;
            }
        }
        Self {
            width: map.width,
            labels,
        }
    }

    /// セルの成分番号。通行不能なセルはNone。
    pub(crate) fn at(&self, position: GridPosition) -> Option<u32> {
        self.labels
            .get(position.y * self.width + position.x)
            .copied()
            .flatten()
    }
}

/// 艦船が通行できるセルか（海・浅瀬・港）。
pub(crate) fn is_navigable(board: &PlayerBoard, x: usize, y: usize) -> bool {
    board.map.get_terrain(x, y).is_some_and(|terrain| {
        board
            .registry
            .get_movement_cost(MovementType::Ship, terrain.as_str())
            .is_some_and(|cost| cost < 99)
    })
}

/// 複数の起点から、艦船の通行可能セルだけをたどった歩数を求める（占有は無視する）。
///
/// 艦船は移動後に間接射撃できないため、射撃位置まで何手で着くかを位置取りの
/// 接近評価に使う。起点自体が通行不能な場合は含めない。
pub(crate) fn sea_distances(
    board: &PlayerBoard,
    sources: impl IntoIterator<Item = (usize, usize)>,
) -> HashMap<(usize, usize), u32> {
    let mut distances = HashMap::default();
    let mut queue = VecDeque::new();
    for source in sources {
        if is_navigable(board, source.0, source.1) && !distances.contains_key(&source) {
            distances.insert(source, 0);
            queue.push_back(source);
        }
    }
    while let Some(current) = queue.pop_front() {
        let distance = distances[&current];
        for next in board.map.get_adjacent(current.0, current.1) {
            if !distances.contains_key(&next) && is_navigable(board, next.0, next.1) {
                distances.insert(next, distance + 1);
                queue.push_back(next);
            }
        }
    }
    distances
}

/// 指定座標から射程内（`min..=max`）にある全セル。
pub(crate) fn cells_within_ring(
    map: &Map,
    center: GridPosition,
    min: u32,
    max: u32,
) -> impl Iterator<Item = (usize, usize)> + '_ {
    let radius = max as usize;
    let x_range = center.x.saturating_sub(radius)..=(center.x + radius).min(map.width - 1);
    x_range.flat_map(move |x| {
        let y_range = center.y.saturating_sub(radius)..=(center.y + radius).min(map.height - 1);
        y_range.filter_map(move |y| {
            let distance = map.distance(center.x, center.y, x, y);
            (distance >= min && distance <= max).then_some((x, y))
        })
    })
}

/// 1プレイヤー視点の海上・航空戦域の事実。
#[derive(Debug, Clone, Default)]
pub(crate) struct MaritimeTheater {
    /// 敵首都が自軍首都と別の陸塊にあるか。
    pub(crate) capitals_separated_by_sea: bool,
    /// 自軍所有の港の座標。
    pub(crate) own_ports: Vec<GridPosition>,
    /// 自軍所有の空港の座標。
    pub(crate) own_airports: Vec<GridPosition>,
    /// 自軍の港から出た艦船が射程へ捉えられる敵の部隊・拠点があるか。
    pub(crate) coastal_targets_in_reach: bool,
    /// 敵の艦船の数（輸送船を含む）。
    pub(crate) enemy_ships: u32,
    /// 敵の戦艦の数。
    pub(crate) enemy_battleships: u32,
    /// 自軍の戦艦の数。
    pub(crate) own_battleships: u32,
    /// 敵の航空機の数。
    pub(crate) enemy_aircraft: u32,
}

impl MaritimeTheater {
    /// 盤面から戦域の事実を集める。Worldは書き換えない。
    pub(crate) fn observe(board: &PlayerBoard, player_id: PlayerId) -> Self {
        let land = Components::label(board, MovementType::Infantry);
        let own_capitals: Vec<_> = board
            .properties
            .iter()
            .filter(|property| {
                property.owner == Some(player_id) && property.terrain == Terrain::Capital
            })
            .map(|property| property.position)
            .collect();
        let home_landmasses: HashSet<u32> = own_capitals
            .iter()
            .filter_map(|position| land.at(*position))
            .collect();
        let enemy_capital_landmasses: HashSet<u32> = board
            .properties
            .iter()
            .filter(|property| {
                property.terrain == Terrain::Capital
                    && property.owner.is_some_and(|owner| owner != player_id)
            })
            .filter_map(|property| land.at(property.position))
            .collect();
        let capitals_separated_by_sea = !enemy_capital_landmasses.is_empty()
            && enemy_capital_landmasses.is_disjoint(&home_landmasses);

        let owned = |terrain: Terrain| -> Vec<GridPosition> {
            board
                .properties
                .iter()
                .filter(|property| property.owner == Some(player_id) && property.terrain == terrain)
                .map(|property| property.position)
                .collect()
        };
        let own_ports = owned(Terrain::Port);
        let own_airports = owned(Terrain::Airport);

        let mut theater = Self {
            capitals_separated_by_sea,
            own_ports,
            own_airports,
            ..Self::default()
        };
        for unit in board.units.iter().filter(|unit| unit.is_deployed()) {
            let is_enemy = unit.owner != player_id;
            match (unit.stats.movement_type, is_enemy) {
                (MovementType::Ship, true) => {
                    theater.enemy_ships += 1;
                    if unit.stats.unit_type == UnitType::Battleship {
                        theater.enemy_battleships += 1;
                    }
                }
                (MovementType::Ship, false) if unit.stats.unit_type == UnitType::Battleship => {
                    theater.own_battleships += 1;
                }
                (MovementType::Air, true) => theater.enemy_aircraft += 1,
                _ => {}
            }
        }
        theater.coastal_targets_in_reach = coastal_targets_in_reach(board, player_id, &theater);
        theater
    }

    /// 艦船（戦艦）が戦闘に寄与できる戦域か。
    ///
    /// 港から射程へ捉えられる敵が居ることを前提に、海を渡らないと敵首都へ届かない
    /// 盤面か、敵が艦船を出している盤面だけを対象にする。陸続きで敵も艦船を
    /// 持たない盤面では、港があっても艦船へ資金を割かない。
    pub(crate) fn naval_combat_is_useful(&self) -> bool {
        !self.own_ports.is_empty()
            && self.coastal_targets_in_reach
            && (self.capitals_separated_by_sea || self.enemy_ships > 0)
    }

    /// 同時に保有する戦艦の上限。
    ///
    /// 敵戦艦を先に射程へ捉えて数で上回れる数と、海を渡る作戦の火力支援を合わせる。
    pub(crate) fn battleship_cap(&self) -> u32 {
        if !self.naval_combat_is_useful() {
            return 0;
        }
        let support = if self.capitals_separated_by_sea { 2 } else { 1 };
        self.enemy_battleships + support
    }
}

/// 自軍の港から艦船で到達できる海域のどこかから、敵の部隊または拠点が射程に入るか。
fn coastal_targets_in_reach(
    board: &PlayerBoard,
    player_id: PlayerId,
    theater: &MaritimeTheater,
) -> bool {
    if theater.own_ports.is_empty() {
        return false;
    }
    let reachable_sea = sea_distances(
        board,
        theater
            .own_ports
            .iter()
            .map(|position| (position.x, position.y)),
    );
    let enemy_positions = board
        .units
        .iter()
        .filter(|unit| unit.owner != player_id && unit.is_deployed())
        .map(|unit| unit.position)
        .chain(
            board
                .properties
                .iter()
                .filter(|property| property.owner.is_some_and(|owner| owner != player_id))
                .map(|property| property.position),
        );
    for position in enemy_positions {
        if cells_within_ring(&board.map, position, 2, NAVAL_GUN_MAX_RANGE)
            .any(|cell| reachable_sea.contains_key(&cell))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::v4::maritime::test_support::board_from_rows;

    #[test]
    fn separated_capitals_make_naval_combat_useful() {
        // 左の島に自軍首都と港、右の島に敵首都。間は海。
        let board = board_from_rows(
            &["H.P~~~~..H", "...~~~~...", "...~~~~..."],
            &[(0, 0, 1), (2, 0, 1), (9, 0, 2)],
        );
        let theater = MaritimeTheater::observe(&board, PlayerId(1));
        assert!(theater.capitals_separated_by_sea);
        assert!(theater.coastal_targets_in_reach);
        assert!(theater.naval_combat_is_useful());
        assert_eq!(theater.battleship_cap(), 2);
    }

    #[test]
    fn shared_landmass_without_enemy_ships_does_not_use_battleships() {
        // 首都同士が陸続きの島。港はあるが敵は艦船を持たない。
        let board = board_from_rows(
            &["~~~~~~~~", "~H.P..H~", "~......~", "~~~~~~~~"],
            &[(1, 1, 1), (3, 1, 1), (6, 1, 2)],
        );
        let theater = MaritimeTheater::observe(&board, PlayerId(1));
        assert!(!theater.capitals_separated_by_sea);
        assert!(!theater.naval_combat_is_useful());
        assert_eq!(theater.battleship_cap(), 0);
    }

    #[test]
    fn sea_distances_follow_navigable_cells_only() {
        let board = board_from_rows(&["P~~.~"], &[(0, 0, 1)]);
        let distances = sea_distances(&board, [(0, 0)]);
        assert_eq!(distances.get(&(2, 0)), Some(&2));
        // 陸のセルで海域が途切れるため、その先の海へは届かない。
        assert_eq!(distances.get(&(4, 0)), None);
    }
}
