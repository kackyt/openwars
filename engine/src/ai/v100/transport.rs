//! Gameboy Warsの輸送分岐を能力ベースで模擬するV100/V200共通処理。
//!
//! ROM 530Cは空の輸送部隊を搭載可能部隊へ寄せ、搭載後は個別目標へ通常移動し、
//! 5675で降車可能地点を走査する。この順序をマップ名や絶対座標へ依存せず再現する。

use super::candidate_field::CandidateTile;
use super::route_field::build_route_field_to_any;
use crate::ai::AiVersion;
use crate::ai::engine::AiCommand;
use crate::ai::islands::IslandMap;
use crate::components::{
    ActionCompleted, CargoCapacity, Faction, Fuel, GridPosition, HasMoved, Health, PlayerId,
    Property, Transporting, UnitStats,
};
use crate::resources::{Map, MasterDataRegistry, MovementType, Terrain};
use crate::systems::movement::{
    OccupantInfo, calculate_reachable_tile_costs, get_valid_movement_cost,
};
use bevy_ecs::prelude::*;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
struct PassengerView {
    entity: Entity,
    position: GridPosition,
    stats: UnitStats,
    fuel: u32,
    available: bool,
    production_priority: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PickupScore {
    production_priority_count: usize,
    loadable_count: usize,
    total_cost: Reverse<u32>,
}

/// 降車候補の比較値と、輸送ユニット・搭載ユニットそれぞれの到達位置。
type DropScore = (u32, Reverse<usize>, Reverse<usize>, usize, usize);
type DropChoice = (DropScore, GridPosition, GridPosition);

/// 輸送能力を持つ部隊について、空なら搭載地点、積載済みなら前進・降車を決める。
pub(crate) fn choose_transport_action(
    world: &mut World,
    actor: Entity,
    candidates: &[CandidateTile],
    assigned_objective: Option<GridPosition>,
    player_id: PlayerId,
    version: AiVersion,
) -> Option<AiCommand> {
    let cargo = world.get::<CargoCapacity>(actor)?.loaded.clone();
    if cargo.is_empty() {
        choose_pickup_position(world, actor, candidates, player_id, version)
    } else {
        choose_loaded_action(
            world,
            actor,
            &cargo,
            candidates,
            assigned_objective,
            player_id,
        )
    }
}

/// 通常の部隊判断で輸送部隊座標へ到達できる場合、待機ではなく搭載命令へ変換する。
pub(crate) fn choose_load(
    world: &mut World,
    actor: Entity,
    candidates: &[CandidateTile],
    player_id: PlayerId,
) -> Option<AiCommand> {
    let actor_type = world.get::<UnitStats>(actor)?.unit_type;
    let candidate_positions: HashSet<_> = candidates
        .iter()
        .map(|candidate| (candidate.position.x, candidate.position.y))
        .collect();
    let mut targets = Vec::new();
    let mut query = world.query::<(
        Entity,
        &GridPosition,
        &Faction,
        &UnitStats,
        &CargoCapacity,
        Option<&Transporting>,
    )>();
    for (entity, position, faction, stats, cargo, transporting) in query.iter(world) {
        if faction.0 == player_id
            && transporting.is_none()
            && super::compatibility_profile::is_gbw_transport(stats)
            && cargo.loaded.len() < cargo.max as usize
            && stats.loadable_unit_types.contains(&actor_type)
            && candidate_positions.contains(&(position.x, position.y))
        {
            targets.push((position.y, position.x, entity.index(), entity, *position));
        }
    }
    targets.sort_by_key(|target| (target.0, target.1, target.2));
    targets
        .into_iter()
        .next()
        .map(|(_, _, _, transport_entity, target_pos)| AiCommand::Load {
            target_pos,
            transport_entity,
        })
}

fn choose_pickup_position(
    world: &mut World,
    actor: Entity,
    candidates: &[CandidateTile],
    player_id: PlayerId,
    version: AiVersion,
) -> Option<AiCommand> {
    let map = world.get_resource::<Map>()?.clone();
    let master_data = world.get_resource::<MasterDataRegistry>()?.clone();
    let actor_position = *world.get::<GridPosition>(actor)?;
    let actor_stats = world.get::<UnitStats>(actor)?.clone();
    let capacity = world.get::<CargoCapacity>(actor)?.max as usize;
    let (passengers, occupants) =
        collect_passengers_and_occupants(world, actor, player_id, &actor_stats.loadable_unit_types);
    if passengers.is_empty() {
        return None;
    }

    let mut best: Option<(PickupScore, GridPosition)> = None;
    for candidate in candidates {
        let mut candidate_occupants = occupants.clone();
        candidate_occupants.remove(&(actor_position.x, actor_position.y));
        candidate_occupants.insert(
            (candidate.position.x, candidate.position.y),
            OccupantInfo {
                player_id,
                is_transport: true,
                unit_type: actor_stats.unit_type,
                loadable_types: actor_stats.loadable_unit_types.clone(),
                free_slots: capacity as u32,
            },
        );

        let mut reachable: Vec<_> = passengers
            .iter()
            .filter(|passenger| passenger.available)
            .filter_map(|passenger| {
                calculate_reachable_tile_costs(
                    &map,
                    &candidate_occupants,
                    (passenger.position.x, passenger.position.y),
                    passenger.stats.movement_type,
                    passenger.stats.max_movement,
                    passenger.fuel,
                    player_id,
                    passenger.stats.unit_type,
                    &master_data,
                )
                .get(&(candidate.position.x, candidate.position.y))
                .copied()
                .map(|cost| (passenger.production_priority, cost))
            })
            .collect();
        reachable.sort_by_key(|(priority, cost)| (Reverse(*priority), *cost));
        reachable.truncate(capacity);
        let score = PickupScore {
            production_priority_count: reachable.iter().filter(|(priority, _)| *priority).count(),
            loadable_count: reachable.len(),
            total_cost: Reverse(reachable.iter().map(|(_, cost)| *cost).sum()),
        };
        if score.loadable_count == 0 {
            continue;
        }
        if pickup_candidate_is_better(best, score, candidate.position, version) {
            best = Some((score, candidate.position));
        }
    }

    let target_pos = best.map(|(_, position)| position).unwrap_or_else(|| {
        // 今手番に搭載できない場合も、ROM 530Cと同様に最寄りの搭載可能部隊へ接近する。
        let passenger = passengers
            .iter()
            .min_by_key(|passenger| {
                (
                    u8::from(!passenger.available),
                    map.distance(
                        actor_position.x,
                        actor_position.y,
                        passenger.position.x,
                        passenger.position.y,
                    ),
                    super::unit_record::record_order(world, passenger.entity),
                )
            })
            .expect("passengers is not empty");
        select_progress_candidate(candidates, passenger.position, &map).unwrap_or(actor_position)
    });
    Some(AiCommand::Wait { target_pos })
}

fn pickup_candidate_is_better(
    current: Option<(PickupScore, GridPosition)>,
    score: PickupScore,
    position: GridPosition,
    version: AiVersion,
) -> bool {
    let Some((current_score, current_position)) = current else {
        return true;
    };
    if score != current_score {
        return score > current_score;
    }
    // 4C19の同値更新差を反映する。IQ100は後の行優先、IQ200は先の行を保持する。
    match version {
        AiVersion::V100 => (position.y, position.x) >= (current_position.y, current_position.x),
        AiVersion::V200 => (position.y, position.x) < (current_position.y, current_position.x),
        _ => unreachable!("V100/V200専用AI以外から輸送判断を参照しました"),
    }
}

fn choose_loaded_action(
    world: &mut World,
    actor: Entity,
    cargo: &[Entity],
    candidates: &[CandidateTile],
    assigned_objective: Option<GridPosition>,
    player_id: PlayerId,
) -> Option<AiCommand> {
    let map = world.get_resource::<Map>()?.clone();
    let master_data = world.get_resource::<MasterDataRegistry>()?.clone();
    let island_map = world
        .get_resource::<IslandMap>()
        .cloned()
        .unwrap_or_else(|| IslandMap::analyze(&map));
    let actor_position = *world.get::<GridPosition>(actor)?;
    let actor_movement_type = world.get::<UnitStats>(actor)?.movement_type;
    let cargo_views: Vec<_> = cargo
        .iter()
        .filter_map(|entity| {
            world
                .get::<UnitStats>(*entity)
                .map(|stats| (*entity, stats.movement_type))
        })
        .collect();
    if cargo_views.is_empty() {
        return None;
    }
    let objective = select_transport_objective(
        world,
        actor_position,
        assigned_objective,
        player_id,
        &map,
        &island_map,
    )?;
    let target_island = island_map.get_island_at(&objective).map(|island| island.id);

    // GB版は搭載順に降車可能性を調べる。先頭が降ろせない場合にも、後続の
    // 搭載ユニットまで走査を続け、合法な候補があれば同じ手番で降車させる。
    for (cargo_entity, _) in &cargo_views {
        let mut best_drop: Option<DropChoice> = None;
        for candidate in candidates {
            for (drop_x, drop_y) in crate::systems::transport::get_droppable_tiles_at(
                world,
                actor,
                *cargo_entity,
                candidate.position,
            ) {
                let drop_position = GridPosition {
                    x: drop_x,
                    y: drop_y,
                };
                if target_island.is_some()
                    && island_map
                        .get_island_at(&drop_position)
                        .map(|island| island.id)
                        != target_island
                {
                    continue;
                }
                let key = (
                    map.distance(drop_x, drop_y, objective.x, objective.y),
                    Reverse(candidate.position.y),
                    Reverse(candidate.position.x),
                    drop_y,
                    drop_x,
                );
                if best_drop
                    .as_ref()
                    .is_none_or(|(current, _, _)| key < *current)
                {
                    best_drop = Some((key, candidate.position, drop_position));
                }
            }
        }
        if let Some((_, transport_target_pos, cargo_drop_pos)) = best_drop {
            return Some(AiCommand::Drop {
                transport_target_pos,
                cargo_drop_pos,
                cargo_entity: *cargo_entity,
            });
        }
    }

    let occupied_positions = collect_visible_occupied_positions(world, actor);
    let landing_origins = collect_landing_origins(
        &map,
        &master_data,
        &island_map,
        target_island,
        actor_position,
        actor_movement_type,
        &cargo_views,
        &occupied_positions,
    );
    let route_field =
        build_route_field_to_any(&map, &master_data, &landing_origins, actor_movement_type);
    Some(AiCommand::Wait {
        target_pos: select_route_progress_candidate(candidates, &route_field)
            .or_else(|| select_progress_candidate(candidates, objective, &map))
            .unwrap_or(actor_position),
    })
}

fn collect_visible_occupied_positions(world: &mut World, actor: Entity) -> HashSet<(usize, usize)> {
    let mut occupied = HashSet::new();
    let mut query =
        world.query_filtered::<(Entity, &GridPosition), (With<Faction>, Without<Transporting>)>();
    for (entity, position) in query.iter(world) {
        if entity != actor {
            occupied.insert((position.x, position.y));
        }
    }
    occupied
}

#[allow(clippy::too_many_arguments)]
fn collect_landing_origins(
    map: &Map,
    master_data: &MasterDataRegistry,
    island_map: &IslandMap,
    target_island: Option<crate::ai::islands::IslandId>,
    actor_position: GridPosition,
    actor_movement_type: MovementType,
    cargo: &[(Entity, MovementType)],
    occupied_positions: &HashSet<(usize, usize)>,
) -> Vec<GridPosition> {
    let mut origins = Vec::new();
    for y in 0..map.height {
        for x in 0..map.width {
            let origin = GridPosition { x, y };
            if origin != actor_position && occupied_positions.contains(&(x, y)) {
                continue;
            }
            let Some(terrain) = map.get_terrain(x, y) else {
                continue;
            };
            if get_valid_movement_cost(master_data, actor_movement_type, terrain).is_none()
                || !crate::systems::transport::can_unload_from_terrain(
                    Some(actor_movement_type),
                    Some(terrain),
                )
            {
                continue;
            }

            let has_drop = map.get_adjacent(x, y).into_iter().any(|(drop_x, drop_y)| {
                let drop_position = GridPosition {
                    x: drop_x,
                    y: drop_y,
                };
                if target_island.is_some()
                    && island_map
                        .get_island_at(&drop_position)
                        .map(|island| island.id)
                        != target_island
                {
                    return false;
                }
                if occupied_positions.contains(&(drop_x, drop_y)) {
                    return false;
                }
                let Some(drop_terrain) = map.get_terrain(drop_x, drop_y) else {
                    return false;
                };
                cargo.iter().any(|(_, movement_type)| {
                    get_valid_movement_cost(master_data, *movement_type, drop_terrain).is_some()
                })
            });
            if has_drop {
                origins.push(origin);
            }
        }
    }
    origins
}

fn select_route_progress_candidate(
    candidates: &[CandidateTile],
    route_field: &HashMap<GridPosition, u32>,
) -> Option<GridPosition> {
    candidates
        .iter()
        .filter_map(|candidate| {
            route_field.get(&candidate.position).map(|cost| {
                (
                    (
                        *cost,
                        Reverse(candidate.position.y),
                        Reverse(candidate.position.x),
                    ),
                    candidate.position,
                )
            })
        })
        .min_by_key(|(key, _)| *key)
        .map(|(_, position)| position)
}

fn select_transport_objective(
    world: &mut World,
    origin: GridPosition,
    assigned: Option<GridPosition>,
    player_id: PlayerId,
    map: &Map,
    island_map: &IslandMap,
) -> Option<GridPosition> {
    let properties: Vec<_> = {
        let mut query = world.query::<(&GridPosition, &Property)>();
        query
            .iter(world)
            .map(|(position, property)| (*position, *property))
            .collect()
    };
    let base_island = properties
        .iter()
        .find(|(_, property)| {
            property.owner_id == Some(player_id) && property.terrain == Terrain::Capital
        })
        .and_then(|(position, _)| island_map.get_island_at(position))
        .map(|island| island.id);
    let is_remote = |position: GridPosition| {
        base_island.is_none()
            || island_map
                .get_island_at(&position)
                .is_some_and(|island| Some(island.id) != base_island)
    };
    let assigned_is_owned = assigned.is_some_and(|assigned_position| {
        properties.iter().any(|(position, property)| {
            *position == assigned_position && property.owner_id == Some(player_id)
        })
    });
    if assigned.is_some_and(is_remote) && !assigned_is_owned {
        return assigned;
    }
    properties
        .into_iter()
        .filter(|(position, property)| {
            property.owner_id != Some(player_id)
                && property.max_capture_points > 0
                && is_remote(*position)
        })
        .min_by_key(|(position, property)| {
            (
                map.distance(origin.x, origin.y, position.x, position.y),
                transport_objective_rank(property.terrain),
                position.y,
                Reverse(position.x),
            )
        })
        .map(|(position, _)| position)
        .or(assigned)
}

fn transport_objective_rank(terrain: Terrain) -> u8 {
    match terrain {
        Terrain::Airport | Terrain::Port | Terrain::Factory => 0,
        Terrain::Capital => 1,
        _ => 2,
    }
}

fn select_progress_candidate(
    candidates: &[CandidateTile],
    objective: GridPosition,
    map: &Map,
) -> Option<GridPosition> {
    candidates
        .iter()
        .min_by_key(|candidate| {
            (
                map.distance(
                    candidate.position.x,
                    candidate.position.y,
                    objective.x,
                    objective.y,
                ),
                Reverse(candidate.position.y),
                Reverse(candidate.position.x),
            )
        })
        .map(|candidate| candidate.position)
}

fn collect_passengers_and_occupants(
    world: &mut World,
    actor: Entity,
    player_id: PlayerId,
    loadable_types: &[crate::resources::UnitType],
) -> (Vec<PassengerView>, HashMap<(usize, usize), OccupantInfo>) {
    let mut passengers = Vec::new();
    let mut occupants = HashMap::new();
    let owned_production: HashSet<_> = {
        let mut query = world.query::<(&GridPosition, &Property)>();
        query
            .iter(world)
            .filter_map(|(position, property)| {
                (property.owner_id == Some(player_id)
                    && matches!(
                        property.terrain,
                        Terrain::Capital | Terrain::Factory | Terrain::Airport | Terrain::Port
                    ))
                .then_some((position.x, position.y))
            })
            .collect()
    };
    let mut query = world.query::<(
        Entity,
        &GridPosition,
        &Faction,
        &UnitStats,
        &Fuel,
        &Health,
        &HasMoved,
        &ActionCompleted,
        Option<&CargoCapacity>,
        Option<&Transporting>,
    )>();
    for (entity, position, faction, stats, fuel, health, moved, completed, cargo, transporting) in
        query.iter(world)
    {
        if transporting.is_some() || health.current == 0 {
            continue;
        }
        occupants.insert(
            (position.x, position.y),
            OccupantInfo {
                player_id: faction.0,
                is_transport: super::compatibility_profile::is_gbw_transport(stats),
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots: cargo.map_or(stats.max_cargo, |capacity| {
                    capacity.max.saturating_sub(capacity.loaded.len() as u32)
                }),
            },
        );
        if entity != actor && faction.0 == player_id && loadable_types.contains(&stats.unit_type) {
            passengers.push(PassengerView {
                entity,
                position: *position,
                stats: stats.clone(),
                fuel: fuel.current,
                available: !moved.0 && !completed.0,
                production_priority: owned_production.contains(&(position.x, position.y)),
            });
        }
    }
    passengers.sort_by_key(|passenger| super::unit_record::record_order(world, passenger.entity));
    (passengers, occupants)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{GridTopology, UnitType};

    #[test]
    fn pickup_tie_uses_rom_iq_scan_direction() {
        let current = Some((
            PickupScore {
                production_priority_count: 2,
                loadable_count: 2,
                total_cost: Reverse(3),
            },
            GridPosition { x: 3, y: 3 },
        ));
        let score = PickupScore {
            production_priority_count: 2,
            loadable_count: 2,
            total_cost: Reverse(3),
        };

        assert!(pickup_candidate_is_better(
            current,
            score,
            GridPosition { x: 4, y: 3 },
            AiVersion::V100,
        ));
        assert!(!pickup_candidate_is_better(
            current,
            score,
            GridPosition { x: 4, y: 3 },
            AiVersion::V200,
        ));
    }

    #[test]
    fn transport_objectives_prioritize_production_facilities() {
        assert!(
            transport_objective_rank(Terrain::Airport) < transport_objective_rank(Terrain::City)
        );
        assert!(
            transport_objective_rank(Terrain::Port) < transport_objective_rank(Terrain::Capital)
        );
    }

    #[test]
    fn loaded_transport_checks_later_cargo_when_first_cannot_drop() {
        let mut world = World::new();
        let mut map = Map::new(3, 3, Terrain::Sea, GridTopology::Hex);
        map.set_terrain(2, 1, Terrain::Mountain).unwrap();
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());

        let first_cargo = world
            .spawn(UnitStats {
                unit_type: UnitType::MdTank,
                movement_type: MovementType::Tank,
                ..UnitStats::mock()
            })
            .id();
        let second_cargo = world
            .spawn(UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                ..UnitStats::mock()
            })
            .id();
        let actor_position = GridPosition { x: 1, y: 1 };
        let actor = world
            .spawn((
                actor_position,
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    movement_type: MovementType::Air,
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: vec![first_cargo, second_cargo],
                },
            ))
            .id();
        let candidates = [CandidateTile {
            position: actor_position,
            movement_cost: 0,
        }];

        let command = choose_loaded_action(
            &mut world,
            actor,
            &[first_cargo, second_cargo],
            &candidates,
            Some(GridPosition { x: 2, y: 1 }),
            PlayerId(1),
        )
        .unwrap();

        assert!(matches!(
            command,
            AiCommand::Drop {
                cargo_entity,
                cargo_drop_pos: GridPosition { x: 2, y: 1 },
                ..
            } if cargo_entity == second_cargo
        ));
    }

    #[test]
    fn loaded_lander_routes_to_legal_shoal_instead_of_waiting_near_objective() {
        let mut world = World::new();
        let mut map = Map::new(9, 7, Terrain::Sea, GridTopology::Hex);
        // 施設へ直進すると陸壁の手前で止まるが、南端の浅瀬からは降車できる形にする。
        for y in 0..6 {
            map.set_terrain(5, y, Terrain::Plains).unwrap();
        }
        map.set_terrain(5, 6, Terrain::Shoal).unwrap();
        map.set_terrain(6, 1, Terrain::City).unwrap();
        let master_data = MasterDataRegistry::load().unwrap();
        world.insert_resource(map.clone());
        world.insert_resource(master_data);

        let cargo = world
            .spawn(UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                ..UnitStats::mock()
            })
            .id();
        let actor_position = GridPosition { x: 4, y: 1 };
        let actor = world
            .spawn((
                actor_position,
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Lander,
                    movement_type: MovementType::Ship,
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: vec![cargo],
                },
            ))
            .id();
        let candidates = [
            CandidateTile {
                position: actor_position,
                movement_cost: 0,
            },
            CandidateTile {
                position: GridPosition { x: 4, y: 2 },
                movement_cost: 1,
            },
        ];
        let objective = GridPosition { x: 6, y: 1 };
        assert_eq!(
            select_progress_candidate(&candidates, objective, &map),
            Some(actor_position),
            "施設への直線距離だけなら陸壁手前で停止する再現条件"
        );

        let command = choose_loaded_action(
            &mut world,
            actor,
            &[cargo],
            &candidates,
            Some(objective),
            PlayerId(1),
        )
        .unwrap();

        assert!(matches!(
            command,
            AiCommand::Wait {
                target_pos: GridPosition { x: 4, y: 2 }
            }
        ));
    }
}
