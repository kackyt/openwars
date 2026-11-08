//! 陸上輸送: 装甲車に重歩兵を載せ、前線の奥の敵を突ける位置まで運んで降ろす（V4・標準経路）。
//!
//! 重歩兵は移動力2で前線まで遠い。装甲車（移動力6、歩兵類1体）で運べば、反撃しない
//! 重自走砲などの間接砲や、数で削れる中戦車・軽戦車を、高価な重戦車を買わずに突ける。
//! 装甲車自身は装甲目標にほとんど損害を与えられず、中戦車の主砲で落ちるので、
//! 運んだら降ろして戻る。
//!
//! 積み込むと装甲車はその手番に動けなくなり、載せた手番には降ろせない。そのため
//! 1往復は次の順に進む。
//!
//! 1. 空の装甲車が、後方の重歩兵が移動して届くセルまで迎えに行く（[`GroundLift::rendezvous`]）。
//! 2. 重歩兵がその装甲車へ乗る（[`GroundLift::board`]）。
//! 3. 次の手番、装甲車は降ろした重歩兵が次の手番に攻撃できる敵の近くまで運んで降ろす。
//!    届かなければ近づくだけにする（[`GroundLift::deliver`]）。
//!
//! 両首都が同じ陸塊にある盤面だけで動く。海で隔てられた盤面の装甲車は、輸送船へ
//! 積む揚陸の判断（`maritime::sealift` / `maritime::carrier`）が扱う。

use crate::ai::command::AiCommand;
use crate::ai::context::board::{BoardUnit, PlayerBoard};
use crate::ai::v4::maritime::threat::ThreatMap;
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, HasMoved, PlayerId, Transporting};
use crate::resources::{DamageChart, MovementType, Terrain, UnitType};
use crate::systems::movement::{
    OccupantInfo, calculate_reachable_tile_costs, get_valid_movement_cost,
};
use bevy_ecs::prelude::*;
use std::cmp::Reverse;

/// 盤面のセル座標。
type Cell = (usize, usize);

/// 未所有の拠点の近くへ降ろす価値。敵を突けない場合の降車先として、占領で得る
/// 収入の目安（都市1つ分）を使う。
const PROPERTY_DROP_VALUE: i64 = 1000;

/// 陸上輸送の判断を使うか。実験中は環境変数で切り替える。
pub(crate) fn ground_lift_enabled() -> bool {
    std::env::var("OPENWARS_GROUND_LIFT").is_ok()
}

/// 自軍の装甲車・重歩兵の、陸上輸送としての次の1手。担当外ならNone。
pub(crate) fn decide_ground_lift(
    world: &mut World,
    player_id: PlayerId,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    // 一度「やることが無い」と判断した手番では再計算しない（盤面の観測が重い）。
    if world
        .get_resource::<GroundLiftIdle>()
        .is_some_and(|idle| idle.player_id == player_id && idle.turn == turn)
        || !has_idle_carrier(world, player_id, skip)
    {
        return None;
    }
    let board = PlayerBoard::observe(world, player_id);
    let island = shared_capital_island(&board, player_id)?;
    let chart = world
        .get_resource::<DamageChart>()
        .cloned()
        .unwrap_or_default();
    let moved = board
        .units
        .iter()
        .filter(|unit| {
            world
                .get::<HasMoved>(unit.entity)
                .is_some_and(|moved| moved.0)
        })
        .map(|unit| unit.entity)
        .collect::<HashSet<_>>();
    let cargo_of = board
        .units
        .iter()
        .filter_map(|unit| {
            world
                .get::<Transporting>(unit.entity)
                .map(|transporting| (transporting.0, unit.entity))
        })
        .collect::<HashMap<_, _>>();
    let occupancy = board.occupancy();
    let threat = ThreatMap::new(&board, &chart, &occupancy, player_id);
    let lift = GroundLift {
        board: &board,
        chart: &chart,
        occupancy: &occupancy,
        threat: &threat,
        island,
        moved: &moved,
        cargo_of: &cargo_of,
        skip,
    };
    let decision = lift
        .deliver()
        .or_else(|| lift.rendezvous())
        .or_else(|| lift.board());
    if decision.is_none() {
        world.insert_resource(GroundLiftIdle { player_id, turn });
    }
    decision
}

/// 判断の対象が居るか。盤面の観測は重いので先に調べる。自軍の装甲車が居て、
/// 装甲車か重歩兵のどちらかがまだ行動できる場合に限る。
fn has_idle_carrier(world: &mut World, player_id: PlayerId, skip: &HashSet<Entity>) -> bool {
    let mut query = world.query::<(
        Entity,
        &crate::components::Faction,
        &crate::components::UnitStats,
        &crate::components::ActionCompleted,
    )>();
    let mut has_carrier = false;
    let mut has_idle = false;
    for (entity, faction, stats, done) in query.iter(world) {
        if faction.0 != player_id {
            continue;
        }
        let carrier = is_carrier_stats(stats);
        has_carrier |= carrier;
        has_idle |=
            !done.0 && !skip.contains(&entity) && (carrier || stats.unit_type == UnitType::Mech);
    }
    has_carrier && has_idle
}

/// その手番に陸上輸送の判断が尽きたことの記録。
#[derive(Resource, Debug, Clone, Copy)]
struct GroundLiftIdle {
    player_id: PlayerId,
    turn: u32,
}

/// 歩兵類を載せて陸上を走る輸送部隊（装甲車）の能力か。
fn is_carrier_stats(stats: &crate::components::UnitStats) -> bool {
    stats.max_cargo > 0 && stats.movement_type == MovementType::ArmoredCar
}

/// 自軍と敵の首都が同じ陸塊にあれば、その陸塊。
fn shared_capital_island(board: &PlayerBoard, player_id: PlayerId) -> Option<usize> {
    let island_of = |owner_is_me: bool| {
        board
            .properties
            .iter()
            .filter(|property| {
                property.terrain == Terrain::Capital
                    && property.owner.is_some()
                    && (property.owner == Some(player_id)) == owner_is_me
            })
            .find_map(|property| board.island_map.get_island_at(&property.position))
            .map(|island| island.id.0)
    };
    let home = island_of(true)?;
    (island_of(false) == Some(home)).then_some(home)
}

/// 陸上輸送の判断で共有する盤面。
struct GroundLift<'a> {
    board: &'a PlayerBoard,
    chart: &'a DamageChart,
    occupancy: &'a HashMap<Cell, OccupantInfo>,
    threat: &'a ThreatMap<'a>,
    island: usize,
    /// この手番に移動済みの部隊。
    moved: &'a HashSet<Entity>,
    /// 輸送部隊から、載せている部隊へ。
    cargo_of: &'a HashMap<Entity, Entity>,
    skip: &'a HashSet<Entity>,
}

impl GroundLift<'_> {
    /// 自軍の、行動できる装甲車。
    fn carriers(&self) -> impl Iterator<Item = &BoardUnit> {
        self.board.units.iter().filter(|unit| {
            unit.owner == self.board.player
                && unit.is_deployed()
                && is_carrier_stats(&unit.stats)
                && self.on_island(unit.position)
        })
    }

    fn on_island(&self, position: GridPosition) -> bool {
        self.board
            .island_map
            .get_island_at(&position)
            .is_some_and(|island| island.id.0 == self.island)
    }

    fn unit(&self, entity: Entity) -> Option<&BoardUnit> {
        self.board.units.iter().find(|unit| unit.entity == entity)
    }

    fn can_act(&self, unit: &BoardUnit) -> bool {
        unit.action_completed == Some(false) && !self.skip.contains(&unit.entity)
    }

    /// 重歩兵が敵 `target` を突く価値。与える損害の価格から、反撃で失う重歩兵の価格を引く。
    /// 間接砲は隣から突かれても反撃できない。
    fn strike_value(&self, rider: &crate::components::UnitStats, target: &BoardUnit) -> i64 {
        let dealt = i64::from(best_damage(
            self.chart,
            rider.unit_type,
            target.stats.unit_type,
        )) * i64::from(target.stats.cost)
            * i64::from(target.health.unwrap_or(100))
            / 10_000;
        let counter = if target.stats.min_range > 1 {
            0
        } else {
            i64::from(best_damage(
                self.chart,
                target.stats.unit_type,
                rider.unit_type,
            )) * i64::from(rider.cost)
                / 100
        };
        dealt - counter
    }

    /// 重歩兵が突いて得になる、同じ陸塊の敵。
    fn strike_targets(&self, rider: &crate::components::UnitStats) -> Vec<(GridPosition, i64)> {
        self.board
            .units
            .iter()
            .filter(|unit| {
                unit.owner != self.board.player
                    && unit.is_deployed()
                    && self.on_island(unit.position)
            })
            .filter_map(|unit| {
                let value = self.strike_value(rider, unit);
                (value > 0).then_some((unit.position, value))
            })
            .collect()
    }

    /// 重歩兵が占領できる、同じ陸塊の未所有の拠点。
    fn open_properties(&self) -> Vec<GridPosition> {
        self.board
            .properties
            .iter()
            .filter(|property| {
                property.owner != Some(self.board.player) && self.on_island(property.position)
            })
            .map(|property| property.position)
            .collect()
    }

    /// 自軍の生産施設（装甲車が止まると生産を塞ぐ）。
    fn own_production_sites(&self) -> HashSet<Cell> {
        self.board
            .properties
            .iter()
            .filter(|property| {
                property.owner == Some(self.board.player)
                    && matches!(
                        property.terrain,
                        Terrain::Factory | Terrain::Airport | Terrain::Port
                    )
            })
            .map(|property| (property.position.x, property.position.y))
            .collect()
    }

    fn reachable(&self, unit: &BoardUnit) -> Vec<Cell> {
        calculate_reachable_tile_costs(
            &self.board.map,
            self.occupancy,
            (unit.position.x, unit.position.y),
            unit.stats.movement_type,
            unit.stats.max_movement,
            unit.fuel.unwrap_or(unit.stats.max_fuel),
            unit.owner,
            unit.stats.unit_type,
            &self.board.registry,
        )
        .into_keys()
        .collect()
    }

    /// 部隊が止まれるセル（現在地か空きセル）。
    fn stops(&self, unit: &BoardUnit) -> Vec<Cell> {
        let current = (unit.position.x, unit.position.y);
        self.reachable(unit)
            .into_iter()
            .filter(|cell| *cell == current || !self.occupancy.contains_key(cell))
            .collect()
    }

    fn passable(&self, rider: &crate::components::UnitStats, cell: Cell) -> bool {
        self.board
            .map
            .get_terrain(cell.0, cell.1)
            .is_some_and(|terrain| {
                get_valid_movement_cost(&self.board.registry, rider.movement_type, terrain)
                    .is_some()
            })
    }

    fn distance(&self, from: Cell, to: GridPosition) -> u32 {
        self.board.map.distance(from.0, from.1, to.x, to.y)
    }

    /// 降ろした重歩兵が次の手番に得る価値: 隣まで歩いて突ける敵の価値の最大、
    /// なければ届く拠点。敵の歩兵の壁（ZOC）の後ろの重自走砲は、距離が近くても
    /// 隣の空きセルへ入れないので数えない。
    fn drop_value(
        &self,
        rider: &BoardUnit,
        cell: Cell,
        targets: &[(GridPosition, i64)],
        properties: &[GridPosition],
    ) -> i64 {
        let reach = rider.stats.max_movement.saturating_add(1);
        let near = targets
            .iter()
            .filter(|(position, _)| self.distance(cell, *position) <= reach)
            .collect::<Vec<_>>();
        if !near.is_empty() {
            let stops = calculate_reachable_tile_costs(
                &self.board.map,
                self.occupancy,
                cell,
                rider.stats.movement_type,
                rider.stats.max_movement,
                rider.fuel.unwrap_or(rider.stats.max_fuel),
                rider.owner,
                rider.stats.unit_type,
                &self.board.registry,
            )
            .into_keys()
            .filter(|stop| *stop == cell || !self.occupancy.contains_key(stop))
            .collect::<Vec<_>>();
            let strike = near
                .iter()
                .filter(|(position, _)| {
                    stops
                        .iter()
                        .any(|stop| self.distance(*stop, *position) == 1)
                })
                .map(|(_, value)| *value)
                .max()
                .unwrap_or(0);
            if strike > 0 {
                return strike;
            }
        }
        if properties
            .iter()
            .any(|position| self.distance(cell, *position) <= rider.stats.max_movement)
        {
            PROPERTY_DROP_VALUE
        } else {
            0
        }
    }

    /// 前線までの距離（突ける敵、なければ未所有拠点への最短距離）。
    fn front_distance(
        &self,
        cell: Cell,
        targets: &[(GridPosition, i64)],
        properties: &[GridPosition],
    ) -> u32 {
        targets
            .iter()
            .map(|(position, _)| *position)
            .chain(properties.iter().copied())
            .map(|position| self.distance(cell, position))
            .min()
            .unwrap_or(u32::MAX)
    }

    /// 1. 重歩兵を載せた装甲車を、突ける敵の近くまで運んで降ろす。
    fn deliver(&self) -> Option<(Entity, AiCommand)> {
        for carrier in self.carriers().filter(|carrier| self.can_act(carrier)) {
            let Some(rider) = self
                .cargo_of
                .get(&carrier.entity)
                .and_then(|entity| self.unit(*entity))
            else {
                continue;
            };
            // 今手番に載せた部隊は降ろせない（装甲車も行動済みになっている）。
            if rider.action_completed != Some(false) {
                continue;
            }
            if let Some(command) = self.deliver_one(carrier, rider) {
                return Some((carrier.entity, command));
            }
        }
        None
    }

    fn deliver_one(&self, carrier: &BoardUnit, rider: &BoardUnit) -> Option<AiCommand> {
        let targets = self.strike_targets(&rider.stats);
        let properties = self.open_properties();
        let current = (carrier.position.x, carrier.position.y);
        let fresh = !self.moved.contains(&carrier.entity);
        let stops = if fresh {
            self.stops(carrier)
        } else {
            vec![current]
        };
        let hp = carrier.health.unwrap_or(100).max(1);
        // (撃破されない, 降車の価値, 前線に近い, 被弾が少ない, 座標) が最大の降車。
        type DropKey = (bool, i64, Reverse<u32>, Reverse<u32>, Reverse<(Cell, Cell)>);
        let mut best_drop: Option<DropKey> = None;
        // (撃破されない, 前線に近い, 被弾が少ない, 座標) が最大の移動先。
        type MoveKey = (bool, Reverse<u32>, Reverse<u32>, Reverse<Cell>);
        let mut best_move: Option<MoveKey> = None;
        for stop in stops {
            let threat_hp = self.threat.damage_hp(carrier, stop);
            let survives = threat_hp < hp;
            let front = self.front_distance(stop, &targets, &properties);
            let move_key = (survives, Reverse(front), Reverse(threat_hp), Reverse(stop));
            if best_move.is_none_or(|best| move_key > best) {
                best_move = Some(move_key);
            }
            for cell in self.board.map.get_adjacent(stop.0, stop.1) {
                let vacated = cell == current;
                if (!vacated && self.occupancy.contains_key(&cell))
                    || !self.on_island(GridPosition {
                        x: cell.0,
                        y: cell.1,
                    })
                    || !self.passable(&rider.stats, cell)
                {
                    continue;
                }
                let value = self.drop_value(rider, cell, &targets, &properties);
                if value <= 0 {
                    continue;
                }
                let key = (
                    survives,
                    value,
                    Reverse(self.front_distance(cell, &targets, &properties)),
                    Reverse(threat_hp),
                    Reverse((stop, cell)),
                );
                if best_drop.is_none_or(|best| key > best) {
                    best_drop = Some(key);
                }
            }
        }
        if let Some((_, _, _, _, Reverse((stop, cell)))) = best_drop {
            return Some(AiCommand::Drop {
                transport_target_pos: GridPosition {
                    x: stop.0,
                    y: stop.1,
                },
                cargo_drop_pos: GridPosition {
                    x: cell.0,
                    y: cell.1,
                },
                cargo_entity: rider.entity,
            });
        }
        let (.., Reverse(stop)) = best_move?;
        (fresh && stop != current).then_some(AiCommand::Wait {
            target_pos: GridPosition {
                x: stop.0,
                y: stop.1,
            },
        })
    }

    /// 前線から遠く、装甲車で運ぶ価値のある自軍の重歩兵（未行動・未移動）。
    fn waiting_riders(&self) -> Vec<&BoardUnit> {
        let own_open = self
            .board
            .properties
            .iter()
            .filter(|property| property.owner != Some(self.board.player))
            .map(|property| (property.position.x, property.position.y))
            .collect::<HashSet<_>>();
        self.board
            .units
            .iter()
            .filter(|unit| {
                unit.owner == self.board.player
                    && unit.is_deployed()
                    && unit.stats.unit_type == UnitType::Mech
                    && self.can_act(unit)
                    && !self.moved.contains(&unit.entity)
                    && self.on_island(unit.position)
                    // 未所有の拠点の上に居る重歩兵は占領を続ける。
                    && !own_open.contains(&(unit.position.x, unit.position.y))
            })
            .filter(|unit| {
                let targets = self.strike_targets(&unit.stats);
                let properties = self.open_properties();
                let front =
                    self.front_distance((unit.position.x, unit.position.y), &targets, &properties);
                // 歩いて2手番以内に前線へ着くなら運ばない。
                front != u32::MAX && front > unit.stats.max_movement.saturating_mul(2) + 1
            })
            .collect()
    }

    /// 空き枠のある装甲車か。
    fn has_room(&self, carrier: &BoardUnit) -> bool {
        carrier
            .cargo
            .is_some_and(|cargo| cargo.loaded < cargo.max && cargo.max > 0)
            && !self.cargo_of.contains_key(&carrier.entity)
    }

    /// 2. 空の装甲車が、後方の重歩兵の届くセルまで迎えに行く。
    fn rendezvous(&self) -> Option<(Entity, AiCommand)> {
        let riders = self.waiting_riders();
        if riders.is_empty() {
            return None;
        }
        let rider_reach = riders
            .iter()
            .map(|rider| {
                (
                    rider.entity,
                    self.reachable(rider).into_iter().collect::<HashSet<_>>(),
                )
            })
            .collect::<Vec<_>>();
        // 迎えに来て行動を終えた装甲車が既に届く重歩兵は、その装甲車へ乗る。
        let served = rider_reach
            .iter()
            .filter(|(_, reach)| {
                self.carriers().any(|carrier| {
                    !self.can_act(carrier)
                        && self.has_room(carrier)
                        && reach.contains(&(carrier.position.x, carrier.position.y))
                })
            })
            .map(|(entity, _)| *entity)
            .collect::<HashSet<_>>();
        let production_sites = self.own_production_sites();
        let targets = self.strike_targets(&riders[0].stats);
        let properties = self.open_properties();
        for carrier in self.carriers().filter(|carrier| {
            self.can_act(carrier) && self.has_room(carrier) && !self.moved.contains(&carrier.entity)
        }) {
            let hp = carrier.health.unwrap_or(100).max(1);
            let stops = self.stops(carrier);
            // (撃破されない, 重歩兵が届く, 前線に近い, 被弾が少ない, 座標) が最大の待ち合わせ先。
            type MeetKey = (bool, bool, Reverse<u32>, Reverse<u32>, Reverse<Cell>);
            let mut best: Option<MeetKey> = None;
            for stop in stops {
                if production_sites.contains(&stop) {
                    continue;
                }
                let threat_hp = self.threat.damage_hp(carrier, stop);
                let mut reached = false;
                let mut nearest = u32::MAX;
                for (rider, (entity, reach)) in riders.iter().zip(&rider_reach) {
                    if served.contains(entity) {
                        continue;
                    }
                    if reach.contains(&stop) {
                        reached = true;
                    }
                    nearest = nearest.min(self.distance(stop, rider.position));
                }
                if nearest == u32::MAX {
                    continue;
                }
                let front = if reached {
                    self.front_distance(stop, &targets, &properties)
                } else {
                    nearest
                };
                let key = (
                    threat_hp < hp,
                    reached,
                    Reverse(front),
                    Reverse(threat_hp),
                    Reverse(stop),
                );
                if best.is_none_or(|current| key > current) {
                    best = Some(key);
                }
            }
            let Some((.., Reverse(stop))) = best else {
                continue;
            };
            if stop == (carrier.position.x, carrier.position.y) {
                continue;
            }
            return Some((
                carrier.entity,
                AiCommand::Wait {
                    target_pos: GridPosition {
                        x: stop.0,
                        y: stop.1,
                    },
                },
            ));
        }
        None
    }

    /// 3. 後方の重歩兵が、移動して届く空の装甲車へ乗る。迎えを待つ装甲車は既に行動済み。
    fn board(&self) -> Option<(Entity, AiCommand)> {
        for rider in self.waiting_riders() {
            let reach = self.reachable(rider).into_iter().collect::<HashSet<_>>();
            let carrier = self
                .carriers()
                .filter(|carrier| {
                    self.has_room(carrier)
                        && (self.moved.contains(&carrier.entity) || !self.can_act(carrier))
                        && reach.contains(&(carrier.position.x, carrier.position.y))
                        && carrier
                            .stats
                            .loadable_unit_types
                            .contains(&rider.stats.unit_type)
                })
                .min_by_key(|carrier| (carrier.position.y, carrier.position.x));
            if let Some(carrier) = carrier {
                return Some((
                    rider.entity,
                    AiCommand::Load {
                        target_pos: carrier.position,
                        transport_entity: carrier.entity,
                    },
                ));
            }
        }
        None
    }
}

/// 主武器・副武器のうち有効な方の基本ダメージ。
fn best_damage(chart: &DamageChart, attacker: UnitType, defender: UnitType) -> u32 {
    chart.get_base_damage(attacker, defender).unwrap_or(0).max(
        chart
            .get_base_damage_secondary(attacker, defender)
            .unwrap_or(0),
    )
}

#[cfg(test)]
mod tests;
