//! 装甲車に載せた占領役（歩兵）を、揚陸先の陸塊で拠点の近くへ降ろす。
//!
//! 輸送船の積載枠は2つしかないため、歩兵を載せた装甲車を積むと1枠で2体を運べる
//! （[`super::sealift`]）。揚陸後は装甲車の移動力（6）で拠点の近くまで運び、歩兵を
//! 降ろして占領させる。歩兵は装甲車の中に居る間は撃たれない。
//!
//! 降車セルは次の優先順で選ぶ: 装甲車が次の敵手番に撃破されない → 歩兵が降りる
//! セルから最寄りの未所有拠点が近い → 装甲車の被弾が少ない → 座標。最寄りの拠点が
//! 歩兵の移動力より遠ければ、降ろさずに拠点へ近づくだけにする。
//!
//! 取り残された陸塊（未所有拠点も敵も無い本拠島）の装甲車は、輸送船へ乗るために
//! 歩兵を載せたまま待つので扱わない。

use super::landmass::{Landmasses, is_ground};
use super::sealift::is_ground_carrier;
use super::threat::ThreatMap;
use crate::ai::command::AiCommand;
use crate::ai::context::board::{BoardUnit, PlayerBoard};
use crate::collections::HashSet;
use crate::components::{
    ActionCompleted, Faction, GridPosition, HasMoved, PlayerId, Transporting, UnitStats,
};
use crate::resources::{DamageChart, MovementType};
use crate::systems::movement::{calculate_reachable_tile_costs, get_valid_movement_cost};
use bevy_ecs::prelude::*;
use std::cmp::Reverse;

/// 盤面のセル座標。
type Cell = (usize, usize);
/// 降車の優先順のキー（大きいほど良い）: 装甲車が撃破されない、歩兵の降車セルが拠点に
/// 近い、装甲車の被弾が少ない、座標（停止セル, 降車セル）。
type DropKey = (bool, Reverse<u32>, Reverse<u32>, Reverse<(Cell, Cell)>);
/// 移動先の優先順のキー（大きいほど良い）: 撃破されない、拠点に近い、被弾が少ない、座標。
type MoveKey = (bool, Reverse<u32>, Reverse<u32>, Reverse<Cell>);

/// 自軍の、占領役を載せた装甲車の次の1手。担当外ならNone。
pub(crate) fn decide_carrier_drop(
    world: &mut World,
    player_id: PlayerId,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    // 降ろせる（今手番に載せたのではない）占領役と、それを載せた装甲車。
    let mut riders: Vec<(Entity, Entity)> = Vec::new();
    let mut loaded = world.query::<(Entity, &Transporting, &ActionCompleted)>();
    for (entity, transporting, done) in loaded.iter(world) {
        if !done.0 {
            riders.push((transporting.0, entity));
        }
    }
    // 盤面の観測は重いので、行動できる自軍の地上の輸送部隊（装甲車）に載っている
    // 場合だけ観測する（輸送船・輸送ヘリの積荷は輸送の判断が扱う）。
    riders.retain(|(carrier, _)| {
        let owned = world
            .get::<Faction>(*carrier)
            .is_some_and(|faction| faction.0 == player_id);
        let ground_carrier = world.get::<UnitStats>(*carrier).is_some_and(|stats| {
            stats.max_cargo > 0
                && !super::sealift::is_sealift_transport(stats)
                && !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
        });
        let ready = world
            .get::<ActionCompleted>(*carrier)
            .is_some_and(|done| !done.0)
            && world.get::<Transporting>(*carrier).is_none();
        owned && ground_carrier && ready && !skip.contains(carrier)
    });
    if riders.is_empty() {
        return None;
    }
    riders.sort_by_key(|(carrier, rider)| (*carrier, *rider));
    let board = PlayerBoard::observe(world, player_id);
    let candidates: Vec<(&BoardUnit, &BoardUnit, bool)> = riders
        .iter()
        .filter_map(|(carrier, rider)| {
            let carrier = board.units.iter().find(|unit| unit.entity == *carrier)?;
            let rider = board.units.iter().find(|unit| unit.entity == *rider)?;
            let active = carrier.owner == player_id
                && carrier.is_deployed()
                && is_ground_carrier(carrier)
                && rider.stats.can_capture
                && carrier.action_completed == Some(false)
                && !skip.contains(&carrier.entity);
            let moved = world
                .get::<HasMoved>(carrier.entity)
                .is_some_and(|moved| moved.0);
            active.then_some((carrier, rider, !moved))
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let landmasses = Landmasses::observe(&board, player_id);
    let chart = world
        .get_resource::<DamageChart>()
        .cloned()
        .unwrap_or_default();
    let occupancy = board.occupancy();
    let threat = ThreatMap::new(&board, &chart, &occupancy, player_id);
    let planner = DropPlanner {
        board: &board,
        landmasses: &landmasses,
        occupancy: &occupancy,
        threat: &threat,
    };
    candidates.into_iter().find_map(|(carrier, rider, fresh)| {
        planner
            .decide(carrier, rider, fresh)
            .map(|command| (carrier.entity, command))
    })
}

/// 装甲車の降車判断で共有する盤面。
struct DropPlanner<'a> {
    board: &'a PlayerBoard,
    landmasses: &'a Landmasses,
    occupancy: &'a crate::collections::HashMap<Cell, crate::systems::movement::OccupantInfo>,
    threat: &'a ThreatMap<'a>,
}

impl DropPlanner<'_> {
    fn decide(&self, carrier: &BoardUnit, rider: &BoardUnit, fresh: bool) -> Option<AiCommand> {
        let (landmass, info) = self.landmasses.at(carrier.position)?;
        // 本拠島で輸送船を待つ装甲車は、歩兵を載せたまま待つ。
        if info.unowned.is_empty() && info.enemy_ground == 0 {
            return None;
        }
        let map = &self.board.map;
        let targets = &info.unowned;
        let current = (carrier.position.x, carrier.position.y);
        let stops: Vec<Cell> = if fresh {
            self.reachable(carrier)
                .into_iter()
                .filter(|cell| *cell == current || !self.occupancy.contains_key(cell))
                .collect()
        } else {
            vec![current]
        };
        let hp = carrier.health.unwrap_or(100).max(1);
        let distance_to_target = |cell: Cell| {
            targets
                .iter()
                .map(|target| map.distance(cell.0, cell.1, target.x, target.y))
                .min()
                .unwrap_or(0)
        };
        // (撃破されない, 拠点に近い, 被弾が少ない, 座標) が最大の降車。
        let mut best_drop: Option<DropKey> = None;
        // (撃破されない, 拠点に近い, 被弾が少ない, 座標) が最大の移動先。
        let mut best_move: Option<MoveKey> = None;
        for stop in stops {
            let threat_hp = self.threat.damage_hp(carrier, stop);
            let survives = threat_hp < hp;
            let move_key = (
                survives,
                Reverse(distance_to_target(stop)),
                Reverse(threat_hp),
                Reverse(stop),
            );
            if best_move.is_none_or(|best| move_key > best) {
                best_move = Some(move_key);
            }
            for cell in map.get_adjacent(stop.0, stop.1) {
                let vacated = cell == current;
                if (!vacated && self.occupancy.contains_key(&cell))
                    || self.landmasses.land.at(GridPosition {
                        x: cell.0,
                        y: cell.1,
                    }) != Some(landmass)
                    || !self.passable(rider, cell)
                {
                    continue;
                }
                let key = (
                    survives,
                    Reverse(distance_to_target(cell)),
                    Reverse(threat_hp),
                    Reverse((stop, cell)),
                );
                if best_drop.is_none_or(|best| key > best) {
                    best_drop = Some(key);
                }
            }
        }
        // 歩兵が次の手番に拠点へ届くなら降ろす。移動済み（移動前に決めた降車を続ける）
        // なら現在地から降ろす。
        if let Some((_, Reverse(distance), _, Reverse((stop, cell)))) = best_drop
            && (distance <= rider.stats.max_movement || !fresh || targets.is_empty())
        {
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

    /// 歩兵がセルへ進入できるか。
    fn passable(&self, rider: &BoardUnit, cell: Cell) -> bool {
        is_ground(rider)
            && self
                .board
                .map
                .get_terrain(cell.0, cell.1)
                .is_some_and(|terrain| {
                    get_valid_movement_cost(
                        &self.board.registry,
                        rider.stats.movement_type,
                        terrain,
                    )
                    .is_some()
                })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::v4::maritime::test_support::{load, naval_world, property, spawn};
    use crate::resources::UnitType;

    fn decide(world: &mut World) -> Option<(Entity, AiCommand)> {
        decide_carrier_drop(world, PlayerId(1), &HashSet::default())
    }

    #[test]
    fn landed_recon_drops_its_infantry_onto_a_neutral_city() {
        let mut world = naval_world(&["H........C"]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 0, 9, 0);
        let recon = spawn(&mut world, 1, UnitType::Recon, 3, 0);
        let infantry = load(&mut world, recon, UnitType::Infantry, 1);

        let (entity, command) = decide(&mut world).unwrap();
        assert_eq!(entity, recon);
        match command {
            AiCommand::Drop {
                cargo_drop_pos,
                cargo_entity,
                ..
            } => {
                assert_eq!(cargo_entity, infantry);
                assert_eq!(cargo_drop_pos, GridPosition { x: 9, y: 0 });
            }
            other => panic!("都市へ降ろすはず: {other:?}"),
        }
    }

    #[test]
    fn recon_carries_its_infantry_toward_a_distant_city() {
        // 都市は歩兵の移動力より遠い。降ろさずに装甲車で近づく。
        let mut world = naval_world(&["H..................C"]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 0, 19, 0);
        let recon = spawn(&mut world, 1, UnitType::Recon, 1, 0);
        load(&mut world, recon, UnitType::Infantry, 1);

        let (entity, command) = decide(&mut world).unwrap();
        assert_eq!(entity, recon);
        let AiCommand::Wait { target_pos } = command else {
            panic!("都市へ近づくはず: {command:?}");
        };
        assert_eq!(target_pos, GridPosition { x: 7, y: 0 });
    }

    #[test]
    fn recon_on_a_secured_island_keeps_its_infantry_for_the_lander() {
        let mut world = naval_world(&["H..,~~,C"]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 0, 7, 0);
        let recon = spawn(&mut world, 1, UnitType::Recon, 1, 0);
        load(&mut world, recon, UnitType::Infantry, 1);
        assert!(decide(&mut world).is_none());
    }
}
