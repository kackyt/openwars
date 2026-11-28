//! この手番に完了できる占領を、ほかの行動より先に最大数仕上げる（試験スイッチ）。
//!
//! 占領の進み（残り耐久）は拠点に残るので、途中まで進んだ拠点は誰が仕上げてもよい。
//! 完了した拠点は次の手番から収入になるため、仕上げられる占領を他の行動（陣地線への移動、
//! 新しい占領の開始など）で後回しにしない。
//!
//! 決め方: 「拠点の残り耐久 <= 歩兵の占領力（HP 由来）」かつ「この手番にその拠点へ入れる」組を辺とし、
//! 完了数が最大（同数なら入れ替えの少ない）対応づけをハンガリー法で求める。座標や拠点の並び順は使わない。
//! 拠点に乗っている味方が自分では仕上げられない場合は、その味方を先に退かせて入れ替える。
//!
//! 計算量: 到達マスは歩兵ごとに 1 回（入れ替え候補の拠点だけ占有者を除いて再計算）、
//! 対応づけは O(n²m)（n = 歩兵、m = 完了可能な拠点）。

use crate::ai::command::AiCommand;
use crate::collections::{HashMap, HashSet};
use crate::components::{
    ActionCompleted, CargoCapacity, Faction, Fuel, GridPosition, HasMoved, Health, PlayerId,
    Property, Transporting, UnitStats,
};
use crate::resources::Map;
use crate::resources::master_data::MasterDataRegistry;
use crate::systems::movement::{OccupantInfo, calculate_reachable_tiles};
use bevy_ecs::prelude::*;
use std::collections::BTreeSet;

/// 試験スイッチ（`OPENWARS_V4_CAPTURE_COMPLETION_FIRST=1`）。
pub(crate) fn enabled() -> bool {
    std::env::var("OPENWARS_V4_CAPTURE_COMPLETION_FIRST").as_deref() == Ok("1")
}

struct UnitView {
    entity: Entity,
    position: GridPosition,
    owner: PlayerId,
    stats: UnitStats,
    hp: u32,
    fuel: u32,
    available: bool,
}

fn units(world: &World, skip: &HashSet<Entity>) -> Vec<UnitView> {
    let mut result = world
        .iter_entities()
        .filter_map(|entity| {
            if entity.contains::<Transporting>() {
                return None;
            }
            let stats = entity.get::<UnitStats>()?.clone();
            let hp = entity.get::<Health>()?.current;
            let position = *entity.get::<GridPosition>()?;
            let owner = entity.get::<Faction>()?.0;
            if hp == 0 {
                return None;
            }
            Some(UnitView {
                entity: entity.id(),
                position,
                owner,
                fuel: entity
                    .get::<Fuel>()
                    .map_or(stats.max_fuel, |fuel| fuel.current),
                available: !skip.contains(&entity.id())
                    && !entity.get::<HasMoved>().is_some_and(|moved| moved.0)
                    && !entity
                        .get::<ActionCompleted>()
                        .is_some_and(|completed| completed.0)
                    && !entity
                        .get::<CargoCapacity>()
                        .is_some_and(|cargo| !cargo.loaded.is_empty()),
                stats,
                hp,
            })
        })
        .collect::<Vec<_>>();
    result.sort_unstable_by_key(|unit| unit.entity.to_bits());
    result
}

fn occupancy<'a>(
    units: impl Iterator<Item = &'a UnitView>,
) -> HashMap<(usize, usize), OccupantInfo> {
    units
        .map(|unit| {
            (
                (unit.position.x, unit.position.y),
                OccupantInfo {
                    player_id: unit.owner,
                    is_transport: unit.stats.max_cargo > 0,
                    unit_type: unit.stats.unit_type,
                    loadable_types: unit.stats.loadable_unit_types.clone(),
                    free_slots: 0,
                },
            )
        })
        .collect()
}

fn reachable(
    map: &Map,
    registry: &MasterDataRegistry,
    occupied: &HashMap<(usize, usize), OccupantInfo>,
    unit: &UnitView,
) -> BTreeSet<(usize, usize)> {
    calculate_reachable_tiles(
        map,
        occupied,
        (unit.position.x, unit.position.y),
        unit.stats.movement_type,
        unit.stats.max_movement,
        unit.fuel,
        unit.owner,
        unit.stats.unit_type,
        registry,
    )
}

/// 次に実行する 1 手（仕上げの占領、または仕上げのための退去）。
pub(crate) fn decide_action(
    world: &World,
    player: PlayerId,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let map = world.get_resource::<Map>()?;
    let registry = world.get_resource::<MasterDataRegistry>()?;
    let all = units(world, skip);
    let by_position = all
        .iter()
        .map(|unit| ((unit.position.x, unit.position.y), unit))
        .collect::<HashMap<_, _>>();
    // 完了しうる拠点（自軍以外が所有・中立で、残り耐久が歩兵 1 体の最大占領力以下）
    let max_power = crate::ai::v4::property_control::capture_power_from_hp(100);
    let mut targets = world
        .iter_entities()
        .filter_map(|entity| {
            let position = *entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.owner_id != Some(player)
                && property.max_capture_points > 0
                && property.capture_points > 0
                && property.capture_points <= max_power)
                .then_some((position, property.capture_points))
        })
        .filter(|(position, _)| {
            // 敵が乗っている拠点は占領できない
            by_position
                .get(&(position.x, position.y))
                .is_none_or(|unit| unit.owner == player)
        })
        .collect::<Vec<_>>();
    targets.sort_unstable_by_key(|(position, _)| (position.y, position.x));
    let capturers = all
        .iter()
        .filter(|unit| unit.owner == player && unit.available && unit.stats.can_capture)
        .collect::<Vec<_>>();
    if capturers.is_empty() {
        return None;
    }
    let occupied = occupancy(all.iter());
    if let Some(action) = completion_action(
        world,
        map,
        registry,
        player,
        &by_position,
        &capturers,
        &occupied,
        &targets,
    ) {
        return Some(action);
    }
    start_action(
        map,
        registry,
        world,
        player,
        &by_position,
        &capturers,
        &occupied,
    )
}

/// 第二段: この手番に完了できない占領の開始。届く拠点のうち、完了までに要る手番数が少ない拠点へ
/// 占領役を割り当てる（都市 2 手番を空港 3 手番より先に始める）。拠点は空いているものだけ。
fn start_action(
    map: &Map,
    registry: &MasterDataRegistry,
    world: &World,
    player: PlayerId,
    by_position: &HashMap<(usize, usize), &UnitView>,
    capturers: &[&UnitView],
    occupied: &HashMap<(usize, usize), OccupantInfo>,
) -> Option<(Entity, AiCommand)> {
    let mut targets = world
        .iter_entities()
        .filter_map(|entity| {
            let position = *entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.owner_id != Some(player)
                && property.max_capture_points > 0
                && property.capture_points > 0
                && !by_position.contains_key(&(position.x, position.y)))
            .then_some((position, property.capture_points))
        })
        .collect::<Vec<_>>();
    targets.sort_unstable_by_key(|(position, _)| (position.y, position.x));
    if targets.is_empty() {
        return None;
    }
    const NONE: u64 = u64::MAX / 4;
    let costs = capturers
        .iter()
        .map(|unit| {
            let power = crate::ai::v4::property_control::capture_power_from_hp(unit.hp);
            let reach = reachable(map, registry, occupied, unit);
            targets
                .iter()
                .map(|(position, points)| {
                    if reach.contains(&(position.x, position.y)) {
                        u64::from(crate::ai::v4::property_control::capture_actions_needed(
                            *points, power,
                        ))
                    } else {
                        NONE
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let matching = crate::ai::assignment_solver::min_cost_assignment(&costs, NONE);
    matching.iter().enumerate().find_map(|(i, j)| {
        let j = (*j)?;
        (costs[i][j] < NONE).then(|| {
            (
                capturers[i].entity,
                AiCommand::Capture {
                    target_pos: targets[j].0,
                },
            )
        })
    })
}

/// 第一段: この手番に完了できる占領（入れ替えを含む）。
#[allow(clippy::too_many_arguments)]
fn completion_action(
    world: &World,
    map: &Map,
    registry: &MasterDataRegistry,
    player: PlayerId,
    by_position: &HashMap<(usize, usize), &UnitView>,
    capturers: &[&UnitView],
    occupied: &HashMap<(usize, usize), OccupantInfo>,
    targets: &[(GridPosition, u32)],
) -> Option<(Entity, AiCommand)> {
    if targets.is_empty() {
        return None;
    }
    let occupied = occupied.clone();

    // 費用: 自分の乗っている拠点を仕上げる 1、移動して仕上げる 2（入れ替えが要るなら 3）、不可は対象外
    const NONE: u64 = u64::MAX / 4;
    let mut costs = vec![vec![NONE; targets.len()]; capturers.len()];
    for (i, unit) in capturers.iter().enumerate() {
        let power = crate::ai::v4::property_control::capture_power_from_hp(unit.hp);
        let reach = reachable(map, registry, &occupied, unit);
        for (j, (position, points)) in targets.iter().enumerate() {
            if power < *points {
                continue;
            }
            let key = (position.x, position.y);
            if unit.position == *position {
                costs[i][j] = 1;
                continue;
            }
            match by_position.get(&key) {
                None if reach.contains(&key) => costs[i][j] = 2,
                Some(occupant) if occupant.owner == player && occupant.available => {
                    // 占有している味方が退けば入れるか（占有者を除いて到達マスを求め直す）
                    let mut without = occupied.clone();
                    without.remove(&key);
                    if reachable(map, registry, &without, unit).contains(&key) {
                        costs[i][j] = 3;
                    }
                }
                _ => {}
            }
        }
    }
    let matching = crate::ai::assignment_solver::min_cost_assignment(&costs, NONE);
    let pairs = matching
        .iter()
        .enumerate()
        .filter_map(|(i, j)| j.filter(|&j| costs[i][j] < NONE).map(|j| (i, j)))
        .collect::<Vec<_>>();
    if pairs.is_empty() {
        return None;
    }

    // 1) 拠点が空いている、または自分が乗っている組から実行する
    for &(i, j) in &pairs {
        let (position, _) = targets[j];
        let key = (position.x, position.y);
        let unit = capturers[i];
        if unit.position == position || !by_position.contains_key(&key) {
            return Some((
                unit.entity,
                AiCommand::Capture {
                    target_pos: position,
                },
            ));
        }
    }
    // 2) 入れ替え: 拠点に乗っている味方を、占領に使わない空きマスへ退かせる
    let planned = pairs
        .iter()
        .map(|&(_, j)| (targets[j].0.x, targets[j].0.y))
        .collect::<HashSet<_>>();
    for &(_, j) in &pairs {
        let (position, _) = targets[j];
        // 1) を通らなかった組は、拠点に別の味方が乗っている（退去先がなければ次の組へ）
        let Some(occupant) = by_position.get(&(position.x, position.y)) else {
            continue;
        };
        let production = |x: usize, y: usize| {
            world.iter_entities().any(|entity| {
                entity.get::<GridPosition>() == Some(&GridPosition { x, y })
                    && entity.get::<Property>().is_some_and(|property| {
                        property.owner_id == Some(player)
                            && registry.is_production_facility(property.terrain.as_str())
                    })
            })
        };
        let destination = reachable(map, registry, &occupied, occupant)
            .into_iter()
            .filter(|&(x, y)| {
                (x, y) != (position.x, position.y)
                    && !occupied.contains_key(&(x, y))
                    && !planned.contains(&(x, y))
                    && !production(x, y)
            })
            .min_by_key(|&(x, y)| (map.distance(x, y, position.x, position.y), y, x));
        let Some(destination) = destination else {
            continue;
        };
        return Some((
            occupant.entity,
            AiCommand::Wait {
                target_pos: GridPosition {
                    x: destination.0,
                    y: destination.1,
                },
            },
        ));
    }
    None
}
