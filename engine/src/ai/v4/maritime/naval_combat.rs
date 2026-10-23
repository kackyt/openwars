//! 戦艦の射撃・射撃位置取り・補給帰還。
//!
//! 戦艦の主兵装（対艦ミサイル・艦対空ミサイル）は射程2〜6の間接射撃で、移動した手番には
//! 撃てない。汎用の戦術評価は毎手番どこかへ移動させるため、戦艦は一度も撃てずに
//! 海上を漂っていた。ここでは次の順で1手を決める。
//!
//! 1. 移動せずに撃てる敵が居れば、期待損害価値が最大の敵を撃つ。
//! 2. 撃てなければ、次の手番に撃てる位置（射撃位置）へ移る。射撃位置へ届くなら、
//!    射撃位置の中で被弾の少ないセルを選ぶ。届かなければ海路で最も近い射撃位置へ寄せる。
//!    撃つには射程へ入るしかないので、被弾の見積もりで接近そのものは止めない。
//! 3. 主兵装の弾切れ・燃料不足・大破のときは自軍の港へ戻って補給と修理を受ける。

use super::theater::{NAVAL_GUN_MAX_RANGE, cells_within_ring, is_navigable, sea_distances};
use super::threat::ThreatMap;
use crate::ai::command::AiCommand;
use crate::ai::context::board::{BoardUnit, PlayerBoard};
use crate::collections::{HashMap, HashSet};
use crate::components::{ActionCompleted, GridPosition, HasMoved, PlayerId, UnitStats};
use crate::resources::{DamageChart, MovementType, Terrain, UnitType};
use crate::systems::combat::get_expected_damage;
use crate::systems::movement::calculate_reachable_tile_costs;
use bevy_ecs::prelude::*;

/// 撃破した場合に、与えた損害へ上乗せする割合（%）。残存戦力の反撃を断つ価値。
const KILL_BONUS_PERCENT: u32 = 30;
/// 港へ戻って修理を受けるHP（100満点）。
const REPAIR_HP_THRESHOLD: u32 = 30;
/// 撃てない手番に港へ届くなら補給へ回る、主兵装の残弾。
const LOW_AMMO: u32 = 2;
/// 港の近くで補給を待っているとみなす距離。
const PORT_QUEUE_RADIUS: u32 = 2;
/// 港までの海路に加えて残しておく燃料。
const FUEL_MARGIN: u32 = 6;

/// 自軍の戦艦のうち、まだ行動していない1隻の次の1手を返す。
///
/// 撃てる戦艦があれば射撃を優先し、どの戦艦も撃てない場合だけ位置取りを返す。
pub(crate) fn decide_naval_action(
    world: &mut World,
    player_id: PlayerId,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let ready = ready_warships(world, player_id, skip);
    if ready.is_empty() {
        return None;
    }
    let board = PlayerBoard::observe(world, player_id);
    let chart = world
        .get_resource::<DamageChart>()
        .cloned()
        .unwrap_or_default();
    // 搭載中の部隊は地図外の座標に置かれるため、輸送部隊ごとの積荷の価値を別に集める。
    let mut cargo_costs: HashMap<Entity, u32> = HashMap::default();
    let mut loaded = world.query::<(&crate::components::Transporting, &UnitStats)>();
    for (transporting, stats) in loaded.iter(world) {
        *cargo_costs.entry(transporting.0).or_default() += stats.cost;
    }
    let occupancy = board.occupancy();
    // 揚陸作戦の浜。浜の守りを優先して撃ち、上陸を助ける。
    let beach = world
        .get_resource::<super::sealift::AssaultPlans>()
        .and_then(|plans| plans.0.get(&player_id))
        .map(|plan| plan.stops.clone())
        .unwrap_or_default();
    // 揚陸後は、橋頭堡を起点にした首都攻略の支援地点（橋頭堡・前線拠点・敵優勢のNode）も
    // 砲撃で支援する。上陸部隊が戦っている・前進する先の守りを先に削る。
    let operation_anchors =
        crate::ai::v4::capital_route::beachhead_support_anchors(world, player_id);
    // 揚陸作戦（浜の守りの排除）か上陸後の作戦があれば、撃てない戦艦をその支援位置へ集める。
    let supports_landing = !operation_anchors.is_empty() || !beach.is_empty();
    let mut beach = beach;
    beach.extend(
        operation_anchors
            .into_iter()
            .map(|anchor| (anchor.x, anchor.y)),
    );
    beach.sort_unstable();
    beach.dedup();
    let mut naval = NavalBoard::new(&board, &chart, &occupancy, player_id, cargo_costs);
    naval.beach = beach;
    naval.supports_landing = supports_landing;

    let mut best_fire: Option<(u32, Entity, AiCommand)> = None;
    for unit in board
        .units
        .iter()
        .filter(|unit| ready.contains(&unit.entity))
    {
        if naval.should_vacate_port(unit) {
            continue;
        }
        if let Some((value, command)) = naval.best_fire(unit)
            && best_fire.as_ref().is_none_or(|(best, _, _)| value > *best)
        {
            best_fire = Some((value, unit.entity, command));
        }
    }
    if let Some((_, entity, command)) = best_fire {
        return Some((entity, command));
    }
    // 撃てる戦艦が無い場合だけ移動する。先に撃つ戦艦が居る間は位置を変えない。
    board
        .units
        .iter()
        .filter(|unit| ready.contains(&unit.entity))
        .find_map(|unit| naval.reposition(unit).map(|command| (unit.entity, command)))
}

/// 行動可能な自軍の戦艦（搭載能力を持たない艦船）。
fn ready_warships(
    world: &mut World,
    player_id: PlayerId,
    skip: &HashSet<Entity>,
) -> HashSet<Entity> {
    let mut query = world.query::<(
        Entity,
        &crate::components::Faction,
        &UnitStats,
        &HasMoved,
        &ActionCompleted,
    )>();
    query
        .iter(world)
        .filter(|(entity, faction, stats, moved, done)| {
            faction.0 == player_id
                && is_warship(stats)
                && !moved.0
                && !done.0
                && !skip.contains(entity)
        })
        .map(|(entity, ..)| entity)
        .collect()
}

/// 自前の射撃で戦う艦船か（戦艦）。輸送船・空母は対象外。
pub(crate) fn is_warship(stats: &UnitStats) -> bool {
    stats.movement_type == MovementType::Ship
        && stats.max_cargo == 0
        && stats.unit_type == UnitType::Battleship
}

/// 1回の判断で共有する海上戦の盤面。
struct NavalBoard<'a> {
    board: &'a PlayerBoard,
    chart: &'a DamageChart,
    player_id: PlayerId,
    enemies: Vec<&'a BoardUnit>,
    occupancy: &'a HashMap<(usize, usize), crate::systems::movement::OccupantInfo>,
    threat: ThreatMap<'a>,
    own_ports: HashSet<(usize, usize)>,
    /// 輸送部隊ごとの積荷の生産費の合計。
    cargo_costs: HashMap<Entity, u32>,
    /// 揚陸作戦の浜の停泊セル。
    beach: Vec<(usize, usize)>,
    /// 上陸後の作戦（橋頭堡を起点にした首都攻略）を支援中か。撃てない戦艦を支援位置へ集める。
    supports_landing: bool,
}

impl<'a> NavalBoard<'a> {
    fn new(
        board: &'a PlayerBoard,
        chart: &'a DamageChart,
        occupancy: &'a HashMap<(usize, usize), crate::systems::movement::OccupantInfo>,
        player_id: PlayerId,
        cargo_costs: HashMap<Entity, u32>,
    ) -> Self {
        let enemies = board
            .units
            .iter()
            .filter(|unit| unit.owner != player_id && unit.is_deployed())
            .collect();
        let own_ports = board
            .properties
            .iter()
            .filter(|property| {
                property.owner == Some(player_id) && property.terrain == Terrain::Port
            })
            .map(|property| (property.position.x, property.position.y))
            .collect();
        Self {
            board,
            chart,
            player_id,
            enemies,
            occupancy,
            threat: ThreatMap::new(board, chart, occupancy, player_id),
            own_ports,
            cargo_costs,
            beach: Vec::new(),
            supports_landing: false,
        }
    }

    fn distance(&self, a: GridPosition, b: GridPosition) -> u32 {
        self.board.map.distance(a.x, a.y, b.x, b.y)
    }

    /// 攻撃側が距離 `distance` から防御側へ与える期待損害（HP、100満点）。
    fn expected_damage(
        &self,
        attacker: &BoardUnit,
        defender: &BoardUnit,
        defender_position: GridPosition,
        distance: u32,
    ) -> u32 {
        let defense = self
            .board
            .map
            .get_terrain(defender_position.x, defender_position.y)
            .map(|terrain| self.board.registry.get_terrain_defense_bonus(terrain))
            .unwrap_or(0);
        get_expected_damage(
            &attacker.stats,
            attacker.health.unwrap_or(100),
            attacker
                .ammo
                .unwrap_or((attacker.stats.max_ammo1, attacker.stats.max_ammo2)),
            &defender.stats,
            defense,
            distance,
            &self.board.registry,
            self.chart,
            false,
        )
    }

    /// 防御側へ与える損害の資金換算。撃破なら上乗せし、搭載中の部隊も失わせる。
    fn damage_value(&self, defender: &BoardUnit, damage: u32) -> u32 {
        let hp = defender.health.unwrap_or(100).max(1);
        let dealt = damage.min(hp);
        let mut value = defender.stats.cost.saturating_mul(dealt) / 100;
        if damage >= hp {
            value =
                value.saturating_add(defender.stats.cost.saturating_mul(KILL_BONUS_PERCENT) / 100);
            value = value.saturating_add(self.cargo_value(defender));
        }
        value
    }

    /// 射撃の価値。揚陸作戦の浜を守る地上部隊は上乗せする。
    fn target_value(&self, defender: &BoardUnit, damage: u32) -> u32 {
        let value = self.damage_value(defender, damage);
        let tuning = super::sealift::assault_tuning();
        let guards_beach = super::landmass::is_ground(defender)
            && self.beach.iter().any(|stop| {
                self.board
                    .map
                    .distance(stop.0, stop.1, defender.position.x, defender.position.y)
                    <= tuning.defense_radius
            });
        if guards_beach {
            value.saturating_mul(100 + tuning.beach_fire_bonus_percent) / 100
        } else {
            value
        }
    }

    /// 輸送部隊に搭載中の部隊の価値（撃沈で同時に失われる）。
    fn cargo_value(&self, transport: &BoardUnit) -> u32 {
        self.cargo_costs
            .get(&transport.entity)
            .copied()
            .unwrap_or(0)
    }

    /// 現在位置から移動せずに撃てる最良の標的。
    fn best_fire(&self, unit: &BoardUnit) -> Option<(u32, AiCommand)> {
        self.enemies
            .iter()
            .filter_map(|enemy| {
                let distance = self.distance(unit.position, enemy.position);
                let damage = self.expected_damage(unit, enemy, enemy.position, distance);
                (damage > 0).then(|| (self.target_value(enemy, damage), enemy))
            })
            .max_by_key(|(value, enemy)| {
                (
                    *value,
                    std::cmp::Reverse((enemy.position.x, enemy.position.y)),
                )
            })
            .map(|(value, enemy)| {
                (
                    value,
                    AiCommand::Attack {
                        target_pos: unit.position,
                        target_entity: enemy.entity,
                    },
                )
            })
    }

    /// 位置 `position` に止まった場合、次の手番に移動せずに撃てる最大の損害価値。
    fn fire_value_from(&self, unit: &BoardUnit, position: GridPosition) -> u32 {
        self.enemies
            .iter()
            .filter_map(|enemy| {
                let distance = self.distance(position, enemy.position);
                let damage = self.expected_damage(unit, enemy, enemy.position, distance);
                (damage > 0).then(|| self.target_value(enemy, damage))
            })
            .max()
            .unwrap_or(0)
    }

    /// 補給・修理のため港へ戻るべきか。
    ///
    /// `port_in_reach` は補給先の港へ今手番に入れるか。撃てない手番（この関数は射撃を
    /// 選ばなかった戦艦だけに呼ばれる）に港へ届くなら、主兵装が尽きる前に補給して、
    /// 射撃の機会がある手番を補給の往復で失わないようにする。
    fn needs_resupply(
        &self,
        unit: &BoardUnit,
        port_distance: Option<u32>,
        port_in_reach: bool,
    ) -> bool {
        // 主兵装（ammo1: 対艦ミサイル）が地上部隊・艦船を撃てる唯一の兵装である。
        // 副兵装（艦対空ミサイル）が残っていても、主兵装が尽きれば砲撃の役目を果たせない。
        let ammo1 = unit.ammo.map_or(u32::MAX, |(ammo1, _)| ammo1);
        let out_of_ammo = ammo1 == 0 || (ammo1 <= LOW_AMMO && port_in_reach);
        let low_fuel = match (unit.fuel, port_distance) {
            (Some(fuel), Some(distance)) => fuel <= distance.saturating_add(FUEL_MARGIN),
            _ => false,
        };
        let crippled = unit.health.is_some_and(|hp| hp <= REPAIR_HP_THRESHOLD);
        out_of_ammo || low_fuel || crippled
    }

    /// 撃てない戦艦の移動先を決める。
    fn reposition(&self, unit: &BoardUnit) -> Option<AiCommand> {
        let reachable = calculate_reachable_tile_costs(
            &self.board.map,
            self.occupancy,
            (unit.position.x, unit.position.y),
            unit.stats.movement_type,
            unit.stats.max_movement,
            unit.fuel.unwrap_or(unit.stats.max_fuel),
            self.player_id,
            unit.stats.unit_type,
            &self.board.registry,
        );
        let current = (unit.position.x, unit.position.y);
        // 持ち場: 揚陸作戦があれば浜の守り（居なければ浜の周り）を射程に捉える支援位置、
        // なければ次の手番に敵を射程へ捉えられる射撃位置。補給後に戻る先でもあるため、
        // 弾薬を満載した状態で撃てるかで決める。
        let mut rearmed = unit.clone();
        rearmed.ammo = Some((unit.stats.max_ammo1, unit.stats.max_ammo2));
        let support =
            if self.supports_landing || super::sealift::assault_tuning().battleship_support {
                self.beach_support_cells(&rearmed)
            } else {
                HashSet::default()
            };
        let (station, focus) = if support.is_empty() {
            (self.firing_cells(&rearmed), None)
        } else {
            (support.clone(), Some(&support))
        };
        let station_field = sea_distances(self.board, station.iter().copied());
        // 補給先の港は「今いる位置から港まで」と「港から持ち場まで」の海路の合計が最短の港。
        // 前線から遠い港へ戻ると、補給後に持ち場へ戻る手番が増える。港そのものが持ち場なら
        // 補給した次の手番から撃てる。
        let from_here = sea_distances(self.board, std::iter::once(current));
        // 補給は1港で1手番1隻しか受けられない。補給を待つ味方の戦艦が多い港は、その分の
        // 待ち時間（1隻あたり1手番の移動量）を加えて比べ、補給先を港へ分散させる。
        let movement = unit.stats.max_movement.max(1);
        let port = self
            .own_ports
            .iter()
            .copied()
            .filter(|port| {
                *port == current
                    || self.occupancy.get(port).is_none_or(|occupant| {
                        occupant.player_id == self.player_id
                            && occupant.unit_type == UnitType::Battleship
                    })
            })
            .filter_map(|port| {
                let there = *from_here.get(&port)?;
                let back = station_field.get(&port).copied().unwrap_or(0);
                let queue = self.battleships_waiting_at(unit, port);
                Some((there + back + queue * movement, port))
            })
            .min_by_key(|(round_trip, port)| (*round_trip, *port))
            .map(|(_, port)| port);
        let port_distance = port.and_then(|port| from_here.get(&port).copied());
        let port_in_reach = port.is_some_and(|port| reachable.contains_key(&port));
        if let Some(port) = port
            && self.needs_resupply(unit, port_distance, port_in_reach)
        {
            let to_port = sea_distances(self.board, std::iter::once(port));
            return self.move_toward(&reachable, &to_port, unit, true, None);
        }
        if station.is_empty() {
            return Some(AiCommand::Wait {
                target_pos: unit.position,
            });
        }
        self.move_toward(&reachable, &station_field, unit, false, focus)
    }

    /// 港 `port` の上に居るか、近くで補給を待っている（主兵装が少ない）、他の自軍の戦艦の数。
    fn battleships_waiting_at(&self, unit: &BoardUnit, port: (usize, usize)) -> u32 {
        self.board
            .units
            .iter()
            .filter(|other| {
                other.owner == self.player_id
                    && other.entity != unit.entity
                    && other.is_deployed()
                    && is_warship(&other.stats)
                    && {
                        let distance = self.board.map.distance(
                            port.0,
                            port.1,
                            other.position.x,
                            other.position.y,
                        );
                        distance == 0
                            || (distance <= PORT_QUEUE_RADIUS
                                && other.ammo.is_some_and(|(ammo1, _)| ammo1 <= LOW_AMMO))
                    }
            })
            .count() as u32
    }

    /// 港の上の戦艦が、補給を待つ味方のために港を空けるべきか。
    ///
    /// 補給を終えた戦艦が港の上から撃ち続けると、後続の戦艦が補給できず、待つ間ずっと
    /// 撃てない。主兵装の尽きた味方が近くに来ていれば、港からは撃たずに持ち場へ移る。
    fn should_vacate_port(&self, unit: &BoardUnit) -> bool {
        let here = (unit.position.x, unit.position.y);
        self.own_ports.contains(&here)
            && self.board.units.iter().any(|other| {
                other.owner == self.player_id
                    && other.entity != unit.entity
                    && other.is_deployed()
                    && is_warship(&other.stats)
                    && other.ammo.is_some_and(|(ammo1, _)| ammo1 == 0)
                    && self
                        .board
                        .map
                        .distance(here.0, here.1, other.position.x, other.position.y)
                        <= other.stats.max_movement.saturating_mul(2)
            })
    }

    /// 揚陸作戦の浜を支援する射撃位置。浜の守り（停泊セルの近くの敵地上部隊）を
    /// 撃てる海上セル、守りが居なければ浜の停泊セルを射程に捉える海上セル。
    fn beach_support_cells(&self, unit: &BoardUnit) -> HashSet<(usize, usize)> {
        let mut cells = HashSet::default();
        if self.beach.is_empty() {
            return cells;
        }
        let radius = super::sealift::assault_tuning().defense_radius;
        let map = &self.board.map;
        let guards: Vec<&&BoardUnit> = self
            .enemies
            .iter()
            .filter(|enemy| {
                super::landmass::is_ground(enemy)
                    && self.beach.iter().any(|stop| {
                        map.distance(stop.0, stop.1, enemy.position.x, enemy.position.y) <= radius
                    })
            })
            .collect();
        for guard in &guards {
            for cell in cells_within_ring(map, guard.position, 2, NAVAL_GUN_MAX_RANGE) {
                if !cells.contains(&cell)
                    && is_navigable(self.board, cell.0, cell.1)
                    && self.expected_damage(
                        unit,
                        guard,
                        guard.position,
                        map.distance(cell.0, cell.1, guard.position.x, guard.position.y),
                    ) > 0
                {
                    cells.insert(cell);
                }
            }
        }
        if guards.is_empty() {
            for stop in &self.beach {
                cells.extend(
                    cells_within_ring(
                        map,
                        GridPosition {
                            x: stop.0,
                            y: stop.1,
                        },
                        2,
                        NAVAL_GUN_MAX_RANGE,
                    )
                    .filter(|cell| is_navigable(self.board, cell.0, cell.1)),
                );
            }
        }
        cells
    }

    /// 戦艦の主兵装で敵（部隊、居なければ敵拠点）を捉えられる海上セル。
    fn firing_cells(&self, unit: &BoardUnit) -> HashSet<(usize, usize)> {
        let mut cells = HashSet::default();
        for enemy in &self.enemies {
            for cell in cells_within_ring(&self.board.map, enemy.position, 2, NAVAL_GUN_MAX_RANGE) {
                if !cells.contains(&cell)
                    && is_navigable(self.board, cell.0, cell.1)
                    && self.expected_damage(
                        unit,
                        enemy,
                        enemy.position,
                        self.board
                            .map
                            .distance(cell.0, cell.1, enemy.position.x, enemy.position.y),
                    ) > 0
                {
                    cells.insert(cell);
                }
            }
        }
        if cells.is_empty() {
            // 撃てる敵部隊が見えない間は、次の敵部隊が現れる敵の生産施設を射程に置く。
            // 生産施設が無ければ（都市だけの敵領など）、敵拠点全体を対象にする。
            let enemy_properties: Vec<_> = self
                .board
                .properties
                .iter()
                .filter(|property| property.owner.is_some_and(|owner| owner != self.player_id))
                .collect();
            let production_sites: Vec<_> = enemy_properties
                .iter()
                .copied()
                .filter(|property| {
                    matches!(
                        property.terrain,
                        Terrain::Capital | Terrain::Factory | Terrain::Airport | Terrain::Port
                    )
                })
                .collect();
            let anchors = if production_sites.is_empty() {
                enemy_properties
            } else {
                production_sites
            };
            for property in anchors {
                cells.extend(
                    cells_within_ring(&self.board.map, property.position, 2, NAVAL_GUN_MAX_RANGE)
                        .filter(|cell| is_navigable(self.board, cell.0, cell.1)),
                );
            }
        }
        cells
    }

    /// 到達可能セルから移動先を選ぶ。
    ///
    /// - 射撃位置へ向かう場合、次の手番に撃てて撃沈されないセルがあれば、その中で
    ///   「生き残るHPの割合で割り引いた射撃価値 − 受ける損害の資金換算」が最大のセル。
    ///   敵の手番の損害は次の手番の射撃より先に起きるため、射撃価値を割り引く。
    /// - 撃てるセルが無ければ（港へ戻る場合も）、目標までの海路距離が最小のセル。
    ///   撃沈されるセルは避け、同じ距離なら被弾の少ないセルを選ぶ。
    ///
    /// 被弾は同じ前進量の中での選び分けにだけ使い、射程へ近づくこと自体は止めない。
    /// 自軍の港は艦船の生産を塞ぐので、他に候補がある限り止まらない。
    fn move_toward(
        &self,
        reachable: &std::collections::BTreeMap<(usize, usize), u32>,
        goal_distance: &HashMap<(usize, usize), u32>,
        unit: &BoardUnit,
        returning_to_port: bool,
        focus: Option<&HashSet<(usize, usize)>>,
    ) -> Option<AiCommand> {
        let current = (unit.position.x, unit.position.y);
        let hp = unit.health.unwrap_or(100).max(1);
        let candidates: Vec<NavalStop> = reachable
            .keys()
            .copied()
            .filter(|cell| *cell == current || !self.occupancy.contains_key(cell))
            .filter_map(|cell| {
                let distance = *goal_distance.get(&cell)?;
                let position = GridPosition {
                    x: cell.0,
                    y: cell.1,
                };
                let threat_hp = self.threat.damage_hp(unit, cell);
                let fire = if returning_to_port {
                    0
                } else {
                    self.fire_value_from(unit, position)
                };
                Some(NavalStop {
                    cell,
                    distance,
                    threat_hp,
                    fire,
                    lethal: threat_hp >= hp,
                    blocks_port: !returning_to_port && self.own_ports.contains(&cell),
                    stays: cell == current,
                })
            })
            .collect();
        let firing = candidates
            .iter()
            .filter(|stop| stop.fire > 0 && !stop.lethal)
            .filter(|stop| focus.is_none_or(|focus| focus.contains(&stop.cell)))
            .max_by_key(|stop| {
                let survived = i64::from(hp - stop.threat_hp);
                let fire = i64::from(stop.fire) * survived / i64::from(hp);
                let loss = i64::from(unit.stats.cost) * i64::from(stop.threat_hp) / 100;
                (
                    !stop.blocks_port,
                    fire - loss,
                    stop.stays,
                    std::cmp::Reverse(stop.cell),
                )
            });
        let stop = firing.or_else(|| {
            candidates.iter().min_by_key(|stop| {
                (
                    stop.blocks_port,
                    stop.lethal,
                    stop.distance,
                    stop.threat_hp,
                    !stop.stays,
                    stop.cell,
                )
            })
        })?;
        Some(AiCommand::Wait {
            target_pos: GridPosition {
                x: stop.cell.0,
                y: stop.cell.1,
            },
        })
    }
}

/// 戦艦の移動先の候補。
struct NavalStop {
    cell: (usize, usize),
    /// 目標（射撃位置・港）までの海路距離。
    distance: u32,
    /// 次の敵手番に受ける期待損害（HP）。
    threat_hp: u32,
    /// 次の手番に移動せずに撃てる最大の損害価値。
    fire: u32,
    /// 次の敵手番に撃沈されるか。
    lethal: bool,
    /// 自軍の港（艦船の生産）を塞ぐか。
    blocks_port: bool,
    /// 現在地に留まるか。
    stays: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::v4::maritime::test_support::{naval_world, property, spawn};

    #[test]
    fn battleship_fires_without_moving_when_enemy_is_in_range() {
        // 戦艦(1,1)から距離4の陸上に敵戦車。
        let mut world = naval_world(&["~~~~~.", "~~~~~.", "~~~~~."]);
        let battleship = spawn(&mut world, 1, UnitType::Battleship, 1, 1);
        let tank = spawn(&mut world, 2, UnitType::Tank, 5, 1);

        let (entity, command) =
            decide_naval_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        assert_eq!(entity, battleship);
        match command {
            AiCommand::Attack {
                target_pos,
                target_entity,
            } => {
                assert_eq!(target_pos, GridPosition { x: 1, y: 1 });
                assert_eq!(target_entity, tank);
            }
            other => panic!("移動せずに射撃するはず: {other:?}"),
        }
    }

    #[test]
    fn battleship_moves_into_firing_range_instead_of_drifting() {
        // 敵は射程外（距離9）。次手番に撃てる位置へ寄せる。
        let mut world = naval_world(&["~~~~~~~~~~.", "~~~~~~~~~~.", "~~~~~~~~~~."]);
        let battleship = spawn(&mut world, 1, UnitType::Battleship, 1, 1);
        spawn(&mut world, 2, UnitType::Infantry, 10, 1);

        let (entity, command) =
            decide_naval_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        assert_eq!(entity, battleship);
        let AiCommand::Wait { target_pos } = command else {
            panic!("射撃位置へ移動するはず: {command:?}");
        };
        let distance =
            world
                .resource::<crate::resources::Map>()
                .distance(target_pos.x, target_pos.y, 10, 1);
        assert!(
            (2..=NAVAL_GUN_MAX_RANGE).contains(&distance),
            "移動後は次手番の射程内に入る: {target_pos:?} distance={distance}"
        );
    }

    #[test]
    fn battleship_prefers_sinking_loaded_transport() {
        // 同じ射程内に歩兵と、歩兵を積んだ輸送船。輸送船を優先する。
        let mut world = naval_world(&["~~~~~~.", "~~~~~~.", "~~~~~~."]);
        spawn(&mut world, 1, UnitType::Battleship, 0, 1);
        spawn(&mut world, 2, UnitType::Infantry, 6, 1);
        let lander = spawn(&mut world, 2, UnitType::Lander, 4, 0);
        crate::ai::v4::maritime::test_support::load(&mut world, lander, UnitType::Tank, 2);

        let (_, command) =
            decide_naval_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        let AiCommand::Attack { target_entity, .. } = command else {
            panic!("射撃するはず: {command:?}");
        };
        assert_eq!(target_entity, lander);
    }

    #[test]
    fn battleship_without_main_gun_ammo_returns_to_port() {
        // 主兵装（対艦ミサイル）が尽き、艦対空ミサイルだけが残る。射程内の戦車は撃てないので
        // 港へ戻って補給を受ける。
        let mut world = naval_world(&["P~~~~~~~~."]);
        property(&mut world, 1, 0, 0);
        let battleship = spawn(&mut world, 1, UnitType::Battleship, 4, 0);
        world
            .get_mut::<crate::components::Ammo>(battleship)
            .unwrap()
            .ammo1 = 0;
        spawn(&mut world, 2, UnitType::Tank, 9, 0);

        let (entity, command) =
            decide_naval_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        assert_eq!(entity, battleship);
        let AiCommand::Wait { target_pos } = command else {
            panic!("港へ戻るはず: {command:?}");
        };
        assert_eq!(target_pos, GridPosition { x: 0, y: 0 });
    }

    #[test]
    fn battleship_tops_up_ammo_when_it_cannot_fire_and_a_port_is_in_reach() {
        // 敵は射程外（撃てない手番）。残弾2で港に今手番に届くなら、尽きる前に補給する。
        let mut world = naval_world(&["P~~~~~~~~~~~~~."]);
        property(&mut world, 1, 0, 0);
        let battleship = spawn(&mut world, 1, UnitType::Battleship, 3, 0);
        world
            .get_mut::<crate::components::Ammo>(battleship)
            .unwrap()
            .ammo1 = 2;
        spawn(&mut world, 2, UnitType::Tank, 14, 0);

        let (_, command) =
            decide_naval_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        assert!(
            matches!(command, AiCommand::Wait { target_pos } if target_pos == GridPosition { x: 0, y: 0 }),
            "港で補給する: {command:?}"
        );

        // 残弾が十分なら敵へ寄せる。
        world
            .get_mut::<crate::components::Ammo>(battleship)
            .unwrap()
            .ammo1 = 3;
        let (_, command) =
            decide_naval_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        assert!(
            matches!(command, AiCommand::Wait { target_pos } if target_pos.x > 3),
            "敵へ寄せる: {command:?}"
        );
    }

    #[test]
    fn battleship_resupplies_at_the_port_nearest_to_its_firing_station() {
        // 西の港(0,0)は近いが前線から遠い。東の港(10,0)は少し遠いが、それ自体が戦車を
        // 射程に捉える射撃位置。往復の短い東の港へ向かう。
        let mut world = naval_world(&["P~~~~~~~~~P~~~~~."]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 1, 10, 0);
        let battleship = spawn(&mut world, 1, UnitType::Battleship, 4, 0);
        world
            .get_mut::<crate::components::Ammo>(battleship)
            .unwrap()
            .ammo1 = 0;
        spawn(&mut world, 2, UnitType::Tank, 16, 0);

        let (_, command) =
            decide_naval_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        let AiCommand::Wait { target_pos } = command else {
            panic!("補給へ向かうはず: {command:?}");
        };
        assert!(target_pos.x > 4, "東の港へ向かう: {target_pos:?}");
    }

    #[test]
    fn battleship_approaches_coast_guarded_by_land_units() {
        // 東の島に敵の戦車が並ぶ。戦車は海へ入れないので、沖合の射撃位置は脅かされない。
        let mut world = naval_world(&[
            "~~~~~~~~~~~~~~....",
            "~~~~~~~~~~~~~~....",
            "~~~~~~~~~~~~~~....",
        ]);
        let battleship = spawn(&mut world, 1, UnitType::Battleship, 4, 1);
        for (x, y) in [(15, 0), (15, 1), (15, 2), (16, 1), (17, 1)] {
            spawn(&mut world, 2, UnitType::Tank, x, y);
        }

        let (entity, command) =
            decide_naval_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        assert_eq!(entity, battleship);
        let AiCommand::Wait { target_pos } = command else {
            panic!("射撃位置へ移動するはず: {command:?}");
        };
        let distance =
            world
                .resource::<crate::resources::Map>()
                .distance(target_pos.x, target_pos.y, 15, 1);
        assert!(
            distance <= NAVAL_GUN_MAX_RANGE,
            "次手番に戦車を撃てる位置へ入る: {target_pos:?} distance={distance}"
        );
    }

    #[test]
    fn battleship_closes_in_on_out_of_range_enemy_battleship() {
        // 敵戦艦は今手番に届く射撃位置より遠い。被弾を恐れて留まらず、海路で最も近づく。
        let mut world = naval_world(&["~~~~~~~~~~~~~~~~", "~~~~~~~~~~~~~~~~", "~~~~~~~~~~~~~~~~"]);
        let own = spawn(&mut world, 1, UnitType::Battleship, 0, 1);
        spawn(&mut world, 2, UnitType::Battleship, 15, 1);

        let (entity, command) =
            decide_naval_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        assert_eq!(entity, own);
        let AiCommand::Wait { target_pos } = command else {
            panic!("位置取りするはず: {command:?}");
        };
        let distance =
            world
                .resource::<crate::resources::Map>()
                .distance(target_pos.x, target_pos.y, 15, 1);
        assert_eq!(distance, 10, "最大限に近づく: {target_pos:?}");
    }

    #[test]
    fn battleship_without_visible_enemies_heads_for_enemy_production_site() {
        // 敵部隊は見えない。西端に敵の都市、東端に敵の工場。戦艦は工場側へ寄せる。
        let mut world = naval_world(&["C~~~~~~~~~~~~~~~~~~F"]);
        property(&mut world, 2, 0, 0);
        property(&mut world, 2, 19, 0);
        let battleship = spawn(&mut world, 1, UnitType::Battleship, 9, 0);

        let (entity, command) =
            decide_naval_action(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        assert_eq!(entity, battleship);
        let AiCommand::Wait { target_pos } = command else {
            panic!("位置取りするはず: {command:?}");
        };
        assert!(target_pos.x > 9, "敵の工場側へ寄せる: {target_pos:?}");
    }

    #[test]
    fn transports_and_moved_battleships_are_not_handled() {
        let mut world = naval_world(&["~~~~~.", "~~~~~.", "~~~~~."]);
        spawn(&mut world, 1, UnitType::Lander, 1, 1);
        let battleship = spawn(&mut world, 1, UnitType::Battleship, 2, 1);
        spawn(&mut world, 2, UnitType::Tank, 5, 1);
        let skip = HashSet::from_iter([battleship]);
        assert!(decide_naval_action(&mut world, PlayerId(1), &skip).is_none());
    }
}
