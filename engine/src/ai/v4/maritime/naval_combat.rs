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
    let naval = NavalBoard::new(&board, &chart, &occupancy, player_id, cargo_costs);

    let mut best_fire: Option<(u32, Entity, AiCommand)> = None;
    for unit in board
        .units
        .iter()
        .filter(|unit| ready.contains(&unit.entity))
    {
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
                (damage > 0).then(|| (self.damage_value(enemy, damage), enemy))
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
                (damage > 0).then(|| self.damage_value(enemy, damage))
            })
            .max()
            .unwrap_or(0)
    }

    /// 補給・修理のため港へ戻るべきか。
    fn needs_resupply(&self, unit: &BoardUnit, port_distance: Option<u32>) -> bool {
        // 主兵装（ammo1: 対艦ミサイル）が地上部隊・艦船を撃てる唯一の兵装である。
        // 副兵装（艦対空ミサイル）が残っていても、主兵装が尽きれば砲撃の役目を果たせない。
        let out_of_ammo = unit.ammo.is_some_and(|(ammo1, _)| ammo1 == 0);
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
        let free_ports: Vec<_> = self
            .own_ports
            .iter()
            .copied()
            .filter(|port| {
                *port == (unit.position.x, unit.position.y) || !self.occupancy.contains_key(port)
            })
            .collect();
        let port_distances = sea_distances(self.board, free_ports.iter().copied());
        let current = (unit.position.x, unit.position.y);
        if self.needs_resupply(unit, port_distances.get(&current).copied()) {
            return self.move_toward(&reachable, &port_distances, unit, true);
        }

        // 次の手番に敵を射程へ捉えられる海上セルを射撃位置とする。
        let firing_cells = self.firing_cells(unit);
        if firing_cells.is_empty() {
            return Some(AiCommand::Wait {
                target_pos: unit.position,
            });
        }
        let approach = sea_distances(self.board, firing_cells.iter().copied());
        self.move_toward(&reachable, &approach, unit, false)
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
