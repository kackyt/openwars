//! 揚陸先（浜）の候補と、戦況による評価。
//!
//! 輸送船が積荷を降ろせるのは港・浅瀬（停泊セル）からだけである。陸塊へ降ろせる
//! 停泊セルを近いものどうしでまとめた「浜」が揚陸先の候補になり、浜ごとに島内の
//! 首都攻略DAGの起点（降車セルのうち敵首都へ最も近いセル）が決まる。
//!
//! 浜は次の観点で評価する。
//!
//! 1. 上陸できるか: 敵に塞がれていない停泊セルと降車セルがあるか（無ければ候補外）。
//! 2. 反撃の危険: 次の敵手番に降車セルを攻撃できる敵地上戦力（実際の移動範囲と射程で
//!    数える）と、近くの敵生産拠点から出てくる増援の見込み。首都に近い浜ほど大きい。
//! 3. 得られるもの: 起点から敵首都までの進軍路沿いで取れる拠点と、首都までの距離。
//!
//! 評価は「得られるもの ÷ 反撃の危険」とする。

use super::{Cell, SealiftContext};
use crate::ai::context::board::BoardUnit;
use crate::ai::v4::capital_route::topology::armored_step_distances;
use crate::ai::v4::maritime::landmass::is_ground;
use crate::collections::{HashMap, HashSet};
use crate::components::GridPosition;
use crate::resources::{MovementType, Terrain, UnitType};
use crate::systems::movement::{calculate_reachable_tile_costs, get_valid_movement_cost};
use bevy_ecs::prelude::*;

/// 停泊セルを同じ浜にまとめる距離。
const BEACH_LINK_DISTANCE: u32 = 3;
/// 1つの浜とみなす停泊セルの広がり（端から端までの距離）の上限。
const MAX_BEACH_SPAN: u32 = 8;
/// 進軍路沿いの拠点1つの価値（資金換算。収入1,000を約10手番）。
const ROUTE_PROPERTY_VALUE: i64 = 10_000;
/// 敵首都の価値。起点から首都までの装甲移動ステップが増えるほど割り引く。
const CAPITAL_VALUE: i64 = 60_000;
/// 首都の価値を半分にする装甲移動ステップ数（中戦車の1手番分）。
const CAPITAL_DISCOUNT_STEPS: i64 = 6;
/// 敵生産拠点が降車セルからこの距離以内なら、次の手番に増援が届くとみなす。
const REINFORCEMENT_REACH: u32 = 6;
/// 届く範囲の敵生産拠点1つから出てくる増援の見込み（生産費×HP）。
const REINFORCEMENT_VALUE: i64 = 7_000;
/// 反撃の危険の下限（無防備な浜どうしを得られるもので比べる）。
const RISK_FLOOR: i64 = 10_000;
/// 上陸済みの味方を数える、降車セルからの距離。
const FRIENDLY_RADIUS: u32 = 3;
/// 戦艦1隻の1射撃で削れる、浜の守り1体の戦力の割合（%）。
const NAVAL_SHOT_PERCENT: i64 = 65;

/// 1つの揚陸先（浜）の評価。
#[derive(Debug, Clone)]
pub(crate) struct LandingSite {
    /// 輸送船が止まって降ろす停泊セル（港・浅瀬）。
    pub(crate) stops: Vec<Cell>,
    /// 停泊セルから降ろせる陸塊上のセル。
    pub(crate) drop_cells: Vec<GridPosition>,
    /// 首都攻略DAGの起点（降車セルのうち敵首都へ最も近いセル）。
    pub(crate) origin: Option<GridPosition>,
    /// 敵に塞がれていない停泊セルの数。
    pub(crate) free_stops: usize,
    /// 敵に塞がれていない降車セルの数。
    pub(crate) free_drop_cells: usize,
    /// 次の敵手番に降車セルを攻撃できる敵地上戦力（生産費×HP）。
    pub(crate) counterattack: i64,
    /// 近くの敵生産拠点から次の手番に出てくる増援の見込み。
    pub(crate) reinforcement: i64,
    /// 起点から敵首都までの装甲移動ステップ（首都が無ければNone）。
    pub(crate) route_steps: Option<u32>,
    /// 起点から敵首都までの進軍路沿いにある、自軍が所有していない拠点の数。
    pub(crate) route_properties: u32,
    /// 降車セルの近くに上陸済みの味方地上戦力。
    pub(crate) friendly: i64,
    /// 降車セルを射程に収める自軍の戦艦が、1手番に削れる浜の守りの見込み。
    pub(crate) naval_support: i64,
    /// 次の敵手番に降車セルを攻撃できる敵地上部隊（浜の守り）。
    pub(crate) defenders: Vec<Entity>,
}

impl LandingSite {
    /// 上陸できるか（敵に塞がれていない停泊セルと降車セルがある）。
    pub(crate) fn feasible(&self) -> bool {
        self.free_stops > 0 && self.free_drop_cells > 0
    }

    /// 上陸後に降ろした部隊が受ける、反撃と増援の見込み。
    pub(crate) fn threat(&self) -> i64 {
        self.counterattack + self.reinforcement
    }

    /// 得られるもの（資金換算）。
    pub(crate) fn value(&self) -> i64 {
        let capital = self.route_steps.map_or(0, |steps| {
            CAPITAL_VALUE * CAPITAL_DISCOUNT_STEPS / (i64::from(steps) + CAPITAL_DISCOUNT_STEPS)
        });
        i64::from(self.route_properties) * ROUTE_PROPERTY_VALUE + capital
    }

    /// 得られるもの ÷ 反撃の危険（上陸済みの味方の分だけ危険を下げる）。
    pub(crate) fn score(&self) -> i64 {
        let risk = (self.threat() - self.friendly).max(0) + RISK_FLOOR;
        self.value() * 1_000 / risk
    }
}

/// 部隊の戦力（生産費×HP）。
pub(crate) fn strength(unit: &BoardUnit) -> i64 {
    i64::from(unit.stats.cost) * i64::from(unit.health.unwrap_or(100)) / 100
}

impl SealiftContext {
    /// 陸塊 `landmass` への揚陸先の候補と、その評価。座標順に並べる。
    pub(super) fn landing_sites(&self, landmass: u32) -> Vec<LandingSite> {
        let map = &self.board.map;
        let registry = &self.board.registry;
        let on_landmass = |cell: GridPosition| self.landmasses.land.at(cell) == Some(landmass);
        let mut stops: Vec<Cell> = Vec::new();
        for y in 0..map.height {
            for x in 0..map.width {
                let Some(terrain) = map.get_terrain(x, y) else {
                    continue;
                };
                if !matches!(terrain, Terrain::Shoal | Terrain::Port)
                    || get_valid_movement_cost(registry, MovementType::Ship, terrain).is_none()
                    || !map
                        .get_adjacent(x, y)
                        .into_iter()
                        .any(|(nx, ny)| on_landmass(GridPosition { x: nx, y: ny }))
                {
                    continue;
                }
                stops.push((x, y));
            }
        }
        // 近く（BEACH_LINK_DISTANCE以内）で連鎖する停泊セルを1つの浜にまとめる。
        let mut groups: Vec<Vec<Cell>> = Vec::new();
        for stop in stops {
            let linked: Vec<usize> = groups
                .iter()
                .enumerate()
                .filter(|(_, group)| {
                    group.iter().any(|other| {
                        map.distance(stop.0, stop.1, other.0, other.1) <= BEACH_LINK_DISTANCE
                    })
                })
                .map(|(index, _)| index)
                .collect();
            let mut merged = vec![stop];
            for index in linked.into_iter().rev() {
                merged.extend(groups.swap_remove(index));
            }
            merged.sort_unstable();
            groups.push(merged);
        }
        // 海岸線に沿って長く連鎖した浜（端から端までMAX_BEACH_SPANを超える）は、停泊セルごとに
        // 近くの停泊セルだけの浜へ分ける。長い浜を1つにまとめると、海岸線に沿った全ての守りを
        // 1つの浜の反撃として数え、手薄な区間へ上陸できなくなる。
        let mut split: Vec<Vec<Cell>> = Vec::new();
        for group in groups {
            let span = group
                .iter()
                .flat_map(|a| group.iter().map(move |b| map.distance(a.0, a.1, b.0, b.1)))
                .max()
                .unwrap_or(0);
            if span <= MAX_BEACH_SPAN {
                split.push(group);
                continue;
            }
            for center in &group {
                split.push(
                    group
                        .iter()
                        .copied()
                        .filter(|stop| {
                            map.distance(center.0, center.1, stop.0, stop.1) <= BEACH_LINK_DISTANCE
                        })
                        .collect(),
                );
            }
        }
        let mut groups = split;
        groups.sort_unstable();
        groups.dedup();

        let enemy_capital = self
            .board
            .properties
            .iter()
            .filter(|property| {
                property.terrain == Terrain::Capital
                    && property.owner.is_some_and(|owner| owner != self.player_id)
                    && on_landmass(property.position)
            })
            .map(|property| property.position)
            .min_by_key(|position| (position.y, position.x));
        let from_capital =
            enemy_capital.map(|capital| armored_step_distances(map, registry, capital));
        let enemy_ground: Vec<&BoardUnit> = self
            .board
            .units
            .iter()
            .filter(|unit| {
                unit.owner != self.player_id
                    && unit.is_deployed()
                    && is_ground(unit)
                    && on_landmass(unit.position)
            })
            .collect();
        let strike_cells = self.enemy_strike_cells(&enemy_ground);
        let enemy_production: Vec<GridPosition> = self
            .board
            .properties
            .iter()
            .filter(|property| {
                matches!(property.terrain, Terrain::Factory | Terrain::Capital)
                    && property.owner.is_some_and(|owner| owner != self.player_id)
                    && on_landmass(property.position)
            })
            .map(|property| property.position)
            .collect();
        let enemy_occupied: HashSet<Cell> = self
            .board
            .units
            .iter()
            .filter(|unit| unit.owner != self.player_id && unit.is_deployed())
            .map(|unit| (unit.position.x, unit.position.y))
            .collect();

        groups
            .into_iter()
            .map(|stops| {
                let mut drop_cells: Vec<GridPosition> = stops
                    .iter()
                    .flat_map(|stop| map.get_adjacent(stop.0, stop.1))
                    .map(|(x, y)| GridPosition { x, y })
                    .filter(|cell| {
                        on_landmass(*cell)
                            && map.get_terrain(cell.x, cell.y).is_some_and(|terrain| {
                                get_valid_movement_cost(registry, MovementType::Infantry, terrain)
                                    .is_some()
                            })
                    })
                    .collect();
                drop_cells.sort_unstable_by_key(|cell| (cell.y, cell.x));
                drop_cells.dedup();
                let free_stops = stops
                    .iter()
                    .filter(|stop| !enemy_occupied.contains(stop))
                    .count();
                let free_drop_cells = drop_cells
                    .iter()
                    .filter(|cell| !enemy_occupied.contains(&(cell.x, cell.y)))
                    .count();
                if std::env::var("OPENWARS_ASSAULT_TRACE").is_ok() {
                    let cells: Vec<String> = drop_cells
                        .iter()
                        .map(|cell| {
                            let who = self
                                .board
                                .units
                                .iter()
                                .find(|unit| unit.is_deployed() && unit.position == *cell)
                                .map(|unit| format!("{:?}p{}", unit.stats.unit_type, unit.owner.0))
                                .unwrap_or_default();
                            format!("({},{}){who}", cell.x, cell.y)
                        })
                        .collect();
                    eprintln!("DROPCELLS {:?} {}", stops.first(), cells.join(" "));
                }
                // 降車セルへ攻撃できる敵（同じ敵を浜ごとに1回だけ数える）。
                let defender_units: Vec<&BoardUnit> = enemy_ground
                    .iter()
                    .zip(&strike_cells)
                    .filter(|(_, cells)| drop_cells.iter().any(|cell| cells.contains(cell)))
                    .map(|(unit, _)| *unit)
                    .collect();
                let defenders: Vec<i64> =
                    defender_units.iter().map(|unit| strength(unit)).collect();
                let counterattack: i64 = defenders.iter().sum();
                // 降車セルを今の位置から撃てる、主兵装の残る自軍の戦艦。1隻が1手番に守り1体の
                // 一部を削るとみなし、守りの数で頭打ちにする。
                let supporting = self
                    .board
                    .units
                    .iter()
                    .filter(|unit| {
                        unit.owner == self.player_id
                            && unit.is_deployed()
                            && unit.stats.unit_type == UnitType::Battleship
                            && unit.ammo.is_some_and(|(main, _)| main > 0)
                            && drop_cells.iter().any(|cell| {
                                let distance =
                                    map.distance(cell.x, cell.y, unit.position.x, unit.position.y);
                                (unit.stats.min_range..=unit.stats.max_range).contains(&distance)
                            })
                    })
                    .count()
                    .min(defenders.len()) as i64;
                let naval_support = match defenders.len() as i64 {
                    0 => 0,
                    count => supporting * counterattack / count * NAVAL_SHOT_PERCENT / 100,
                };
                let reinforcement = enemy_production
                    .iter()
                    .filter(|site| {
                        let from_site =
                            |cell: &GridPosition| map.distance(site.x, site.y, cell.x, cell.y);
                        drop_cells
                            .iter()
                            .any(|cell| from_site(cell) <= REINFORCEMENT_REACH)
                    })
                    .count() as i64
                    * REINFORCEMENT_VALUE;
                let origin = from_capital.as_ref().and_then(|from_capital| {
                    drop_cells
                        .iter()
                        .filter_map(|cell| Some((*from_capital.get(cell)?, *cell)))
                        .min_by_key(|(steps, cell)| (*steps, cell.y, cell.x))
                        .map(|(_, cell)| cell)
                });
                let route_steps = origin
                    .zip(from_capital.as_ref())
                    .and_then(|(origin, from_capital)| from_capital.get(&origin).copied());
                let route_properties = match (origin, route_steps, from_capital.as_ref()) {
                    (Some(origin), Some(steps), Some(from_capital)) => {
                        self.route_property_count(landmass, origin, steps, from_capital)
                    }
                    _ => 0,
                };
                let friendly = self
                    .board
                    .units
                    .iter()
                    .filter(|unit| {
                        unit.owner == self.player_id
                            && unit.is_deployed()
                            && is_ground(unit)
                            && drop_cells.iter().any(|cell| {
                                map.distance(cell.x, cell.y, unit.position.x, unit.position.y)
                                    <= FRIENDLY_RADIUS
                            })
                    })
                    .map(strength)
                    .sum();
                LandingSite {
                    stops,
                    drop_cells,
                    origin,
                    free_stops,
                    free_drop_cells,
                    counterattack,
                    reinforcement,
                    route_steps,
                    route_properties,
                    friendly,
                    naval_support,
                    defenders: defender_units.iter().map(|unit| unit.entity).collect(),
                }
            })
            .collect()
    }

    /// 起点から敵首都までの進軍路（首都攻略DAGの回廊と同じ迂回幅）沿いにある、
    /// 自軍が所有していない拠点の数。
    fn route_property_count(
        &self,
        landmass: u32,
        origin: GridPosition,
        route_steps: u32,
        from_capital: &HashMap<GridPosition, u32>,
    ) -> u32 {
        let from_origin = armored_step_distances(&self.board.map, &self.board.registry, origin);
        let detour = (route_steps / 4).clamp(4, 10);
        self.board
            .properties
            .iter()
            .filter(|property| {
                property.owner != Some(self.player_id)
                    && self.landmasses.land.at(property.position) == Some(landmass)
                    && from_origin
                        .get(&property.position)
                        .zip(from_capital.get(&property.position))
                        .is_some_and(|(to, from)| to + from <= route_steps + detour)
            })
            .count() as u32
    }

    /// 敵地上部隊ごとに、次の敵手番に攻撃できるセル。直接攻撃は移動して隣接できる
    /// セル、間接攻撃は移動せずに射程へ捉えるセル。
    fn enemy_strike_cells(&self, enemies: &[&BoardUnit]) -> Vec<HashSet<GridPosition>> {
        let map = &self.board.map;
        enemies
            .iter()
            .map(|enemy| {
                let start = (enemy.position.x, enemy.position.y);
                let mut cells = HashSet::default();
                if enemy.stats.min_range > 1 {
                    for cell in super::super::theater::cells_within_ring(
                        map,
                        enemy.position,
                        enemy.stats.min_range,
                        enemy.stats.max_range,
                    ) {
                        cells.insert(GridPosition {
                            x: cell.0,
                            y: cell.1,
                        });
                    }
                    return cells;
                }
                let reachable = calculate_reachable_tile_costs(
                    map,
                    &self.occupancy,
                    start,
                    enemy.stats.movement_type,
                    enemy.stats.max_movement,
                    enemy.fuel.unwrap_or(enemy.stats.max_fuel),
                    enemy.owner,
                    enemy.stats.unit_type,
                    &self.board.registry,
                );
                for stop in reachable
                    .keys()
                    .filter(|stop| **stop == start || !self.occupancy.contains_key(*stop))
                {
                    cells.extend(
                        map.get_adjacent(stop.0, stop.1)
                            .into_iter()
                            .map(|(x, y)| GridPosition { x, y }),
                    );
                }
                cells
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::v4::maritime::test_support::{naval_world, property, spawn};
    use crate::collections::HashSet;
    use crate::components::PlayerId;
    use crate::resources::UnitType;

    /// 西の浜(1,0)は敵首都(11,0)から遠く、東の浜(14,0)は首都・工場の隣にある。
    fn sites(world: &mut bevy_ecs::prelude::World) -> Vec<LandingSite> {
        let context = SealiftContext::observe(world, PlayerId(1), &HashSet::default());
        let landmass = context
            .landmasses
            .land
            .at(GridPosition { x: 5, y: 0 })
            .unwrap();
        context.landing_sites(landmass)
    }

    #[test]
    fn landing_far_from_the_capital_beats_landing_into_its_counterattack() {
        let mut world = naval_world(&["~,.........HF.,~"]);
        property(&mut world, 2, 11, 0);
        property(&mut world, 2, 12, 0);
        spawn(&mut world, 2, UnitType::Tank, 10, 0);

        let sites = sites(&mut world);
        let west = sites
            .iter()
            .find(|site| site.stops == vec![(1, 0)])
            .unwrap();
        let east = sites
            .iter()
            .find(|site| site.stops == vec![(14, 0)])
            .unwrap();
        // 首都の隣の浜は、戦車の反撃と首都・工場からの増援を受ける。
        assert!(east.counterattack > 0 && east.reinforcement > 0);
        assert_eq!((west.counterattack, west.reinforcement), (0, 0));
        // 首都までの道のりは長いが、反撃を受けない西の浜を選ぶ。
        assert!(west.route_steps > east.route_steps);
        assert!(west.score() > east.score(), "west={west:?} east={east:?}");
        assert_eq!(west.origin, Some(GridPosition { x: 2, y: 0 }));
    }

    #[test]
    fn beach_blocked_by_enemy_cannot_be_landed() {
        let mut world = naval_world(&["~,.........HF.,~"]);
        property(&mut world, 2, 11, 0);
        // 西の浜の浅瀬を敵が塞いでいる。
        spawn(&mut world, 2, UnitType::Infantry, 1, 0);

        let sites = sites(&mut world);
        let west = sites
            .iter()
            .find(|site| site.stops == vec![(1, 0)])
            .unwrap();
        let east = sites
            .iter()
            .find(|site| site.stops == vec![(14, 0)])
            .unwrap();
        assert!(!west.feasible());
        assert!(east.feasible());
    }
}
