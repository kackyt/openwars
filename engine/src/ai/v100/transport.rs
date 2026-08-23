//! Gameboy Warsの輸送分岐を能力ベースで模擬するV100/V200共通処理。
//!
//! ROM 530Cは空の輸送部隊を搭載可能部隊へ寄せ、搭載後は個別目標へ通常移動し、
//! 5675で降車可能地点を走査する。この順序をマップ名や絶対座標へ依存せず再現する。

use super::candidate_field::CandidateTile;
use super::route_field::{build_route_field, build_route_field_to_any};
use crate::ai::AiVersion;
use crate::ai::engine::AiCommand;
use crate::components::{
    ActionCompleted, CargoCapacity, Faction, Fuel, GridPosition, HasMoved, Health, PlayerId,
    Transporting, UnitStats,
};
use crate::resources::{Map, MasterDataRegistry, Terrain};
use crate::systems::movement::{OccupantInfo, calculate_reachable_tile_costs};
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PickupScore {
    loadable_count: usize,
}

/// 輸送能力を持つ部隊について、空なら搭載地点、積載済みなら降車だけを試す。
/// 降車できない場合はROM 4A22と同じくNoneを返し、攻撃・合流・通常移動へ続ける。
pub(crate) fn choose_transport_action(
    world: &mut World,
    actor: Entity,
    candidates: &[CandidateTile],
    assigned_objective: Option<GridPosition>,
    mission: Option<u8>,
    player_id: PlayerId,
    version: AiVersion,
) -> Option<AiCommand> {
    let cargo = world.get::<CargoCapacity>(actor)?.loaded.clone();
    let actor_stats = world.get::<UnitStats>(actor)?.clone();
    if cargo.is_empty() {
        // ROM 532E〜533Aは空輸送部隊の集合移動を任務状態3だけに限定する。
        if mission.is_none_or(|value| value & 0x03 != 3) {
            return None;
        }
        // ROM 530Cで乗客へ接近するのは装甲車、レーダー輸送機、輸送ヘリ、輸送船だけ。
        // OpenWarsにレーダー輸送機はない。空母は航空機側からの直接搭載だけを行う。
        if !matches!(
            actor_stats.unit_type,
            crate::resources::UnitType::Recon
                | crate::resources::UnitType::TransportHelicopter
                | crate::resources::UnitType::Lander
        ) {
            return None;
        }
        choose_pickup_position(world, actor, candidates, player_id, version)
    } else {
        choose_loaded_action(
            world,
            actor,
            &cargo,
            candidates,
            assigned_objective,
            player_id,
            version,
        )
    }
}

/// 通常の部隊判断で輸送部隊座標へ到達できる場合、待機ではなく搭載命令へ変換する。
pub(crate) fn choose_load(
    world: &mut World,
    actor: Entity,
    candidates: &[CandidateTile],
    mission_three_transports: &HashSet<Entity>,
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
            // ROM 524C〜5250は搭載先レコードの任務状態も3に限定する。
            && mission_three_transports.contains(&entity)
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
    let (passengers, occupants) =
        collect_passengers_and_occupants(world, actor, player_id, &actor_stats.loadable_unit_types);
    if passengers.is_empty() {
        return None;
    }

    // ROM 5431は各乗客の移動可能盤面をDBC6へ加算し、盤面全域から最大値を探す。
    // 輸送側が今手番に到達できる候補だけを数える処理ではない。
    let coverage = build_pickup_coverage(
        &map,
        &master_data,
        &passengers,
        &occupants,
        actor_position,
        player_id,
        version,
    );

    let target_pos = select_pickup_field_target(&map, actor_stats.unit_type, &coverage)?;
    let actor_movement = actor_stats
        .max_movement
        .saturating_sub(super::rom_logic::movement_evaluation_penalty(version))
        .max(2);
    let legal_candidates: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.movement_cost <= actor_movement)
        .collect();
    let route_field =
        build_route_field_to_any(&map, &master_data, &[target_pos], actor_stats.movement_type);
    let target_pos = select_route_progress_candidate(&legal_candidates, &route_field)?;
    Some(AiCommand::Wait { target_pos })
}

fn build_pickup_coverage(
    map: &Map,
    master_data: &MasterDataRegistry,
    passengers: &[PassengerView],
    occupants: &HashMap<(usize, usize), OccupantInfo>,
    actor_position: GridPosition,
    player_id: PlayerId,
    version: AiVersion,
) -> HashMap<GridPosition, usize> {
    let passenger_penalty = super::rom_logic::movement_evaluation_penalty(version) * 2;
    let mut coverage = HashMap::<GridPosition, usize>::new();
    for passenger in passengers.iter().filter(|passenger| passenger.available) {
        let movement = passenger
            .stats
            .max_movement
            .saturating_sub(passenger_penalty)
            .max(2);
        for ((x, y), _) in calculate_reachable_tile_costs(
            map,
            occupants,
            (passenger.position.x, passenger.position.y),
            passenger.stats.movement_type,
            movement,
            passenger.fuel,
            player_id,
            passenger.stats.unit_type,
            master_data,
        ) {
            // ROM 5409は空きマスと現在の輸送部隊自身のマスだけを候補に戻す。
            if occupants.contains_key(&(x, y)) && (x, y) != (actor_position.x, actor_position.y) {
                continue;
            }
            *coverage.entry(GridPosition { x, y }).or_default() += 1;
        }
    }
    coverage
}

fn select_pickup_field_target(
    map: &Map,
    actor_type: crate::resources::UnitType,
    coverage: &HashMap<GridPosition, usize>,
) -> Option<GridPosition> {
    let mut best: Option<(PickupScore, GridPosition)> = None;
    for y in 0..map.height {
        for x in 0..map.width {
            let position = GridPosition { x, y };
            let loadable_count = coverage.get(&position).copied().unwrap_or_default();
            let Some(terrain) = map.get_terrain(x, y) else {
                continue;
            };
            if loadable_count == 0 || !pickup_terrain_allowed(actor_type, terrain) {
                continue;
            }
            let score = PickupScore { loadable_count };
            if pickup_candidate_is_better(best, score) {
                best = Some((score, position));
            }
        }
    }
    best.map(|(_, position)| position)
}

/// ROM 54D1の地形分類表2F5Dに対応する。
fn pickup_terrain_allowed(actor_type: crate::resources::UnitType, terrain: Terrain) -> bool {
    match actor_type {
        // 0x16/0x24は施設(分類0)、海(7)、港(8)、浅瀬(9)を除外する。
        crate::resources::UnitType::Recon | crate::resources::UnitType::TransportHelicopter => {
            !matches!(
                terrain,
                Terrain::Capital
                    | Terrain::City
                    | Terrain::Factory
                    | Terrain::Airport
                    | Terrain::Port
                    | Terrain::Sea
                    | Terrain::Shoal
            )
        }
        // 0x2Cは港(8)と浅瀬(9)だけを搭載集合地点にする。
        crate::resources::UnitType::Lander => matches!(terrain, Terrain::Port | Terrain::Shoal),
        _ => false,
    }
}

fn pickup_candidate_is_better(
    current: Option<(PickupScore, GridPosition)>,
    score: PickupScore,
) -> bool {
    let Some((current_score, _)) = current else {
        return true;
    };
    // ROM 5351〜538Bは到達可能な未行動乗客数が厳密に増えた場合だけ更新する。
    // 同数ならDBE5の先の候補、すなわち行優先走査で最初のマスを保持する。
    score > current_score
}

fn choose_loaded_action(
    world: &mut World,
    actor: Entity,
    cargo: &[Entity],
    candidates: &[CandidateTile],
    assigned_objective: Option<GridPosition>,
    _player_id: PlayerId,
    version: AiVersion,
) -> Option<AiCommand> {
    let map = world.get_resource::<Map>()?.clone();
    let master_data = world.get_resource::<MasterDataRegistry>()?.clone();
    let actor_stats = world.get::<UnitStats>(actor)?.clone();
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
    let objective = assigned_objective?;
    let movement = actor_stats
        .max_movement
        .saturating_sub(super::rom_logic::movement_evaluation_penalty(version))
        .max(2);
    let cargo_route_fields: HashMap<_, _> = cargo_views
        .iter()
        .map(|(entity, movement_type)| {
            (
                *entity,
                build_route_field(&map, &master_data, objective, *movement_type),
            )
        })
        .collect();
    let landing_origins: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.movement_cost <= movement)
        .filter(|candidate| {
            matches!(
                map.get_terrain(candidate.position.x, candidate.position.y),
                Some(
                    Terrain::Capital
                        | Terrain::City
                        | Terrain::Factory
                        | Terrain::Airport
                        | Terrain::Port
                )
            )
        })
        .filter(|candidate| {
            cargo_route_fields.values().any(|field| {
                field
                    .get(&candidate.position)
                    .is_some_and(|cost| *cost < 0x40)
            })
        })
        .map(|candidate| candidate.position)
        .collect();
    let landing_origin_set: HashSet<_> = landing_origins.iter().copied().collect();

    // GB版は搭載順に降車可能性を調べる。先頭が降ろせない場合にも、後続の
    // 搭載ユニットまで走査を続ける。ただしROM 553D〜5675と同様、通常移動で
    // 目的側の上陸地点へ到達した候補だけを降車対象にする。
    for (cargo_entity, _) in &cargo_views {
        let route_field = cargo_route_fields.get(cargo_entity)?;
        let mut best_drop: Option<(GridPosition, GridPosition)> = None;
        for candidate in candidates
            .iter()
            .filter(|candidate| landing_origin_set.contains(&candidate.position))
        {
            let selected_drop = crate::systems::transport::get_droppable_tiles_at(
                world,
                actor,
                *cargo_entity,
                candidate.position,
            )
            .into_iter()
            .map(|(drop_x, drop_y)| GridPosition {
                x: drop_x,
                y: drop_y,
            })
            .filter(|drop_position| route_field.contains_key(drop_position))
            .min_by_key(|drop_position| {
                (
                    route_field.get(drop_position).copied().unwrap_or(u32::MAX),
                    Reverse(drop_position.y),
                    Reverse(drop_position.x),
                )
            });
            if let Some(drop_position) = selected_drop {
                // ROM 55EE〜565FはD421を先頭から走査して有効候補ごとに上書きする。
                // candidate_fieldも行優先なので、最後の有効な施設マスを保持する。
                best_drop = Some((candidate.position, drop_position));
            }
        }
        if let Some((transport_target_pos, cargo_drop_pos)) = best_drop {
            return Some(AiCommand::Drop {
                transport_target_pos,
                cargo_drop_pos,
                cargo_entity: *cargo_entity,
            });
        }
    }

    None
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

fn collect_passengers_and_occupants(
    world: &mut World,
    actor: Entity,
    player_id: PlayerId,
    loadable_types: &[crate::resources::UnitType],
) -> (Vec<PassengerView>, HashMap<(usize, usize), OccupantInfo>) {
    let mut passengers = Vec::new();
    let mut occupants = HashMap::new();
    let pickup_eligible = world
        .get_resource::<super::rom_logic::RomAiState>()
        .map(|state| state.pickup_eligible_units(player_id));
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
                // ROM 5464は未行動フラグではなく、部隊レコード+12のbit 5を検査する。
                // OpenWars側の合法手制約も満たす部隊だけを集合盤面へ加える。
                available: !moved.0
                    && !completed.0
                    && pickup_eligible
                        .as_ref()
                        .is_none_or(|eligible| eligible.contains(&entity)),
            });
        }
    }
    passengers.sort_by_key(|passenger| super::unit_record::record_order(world, passenger.entity));
    (passengers, occupants)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Property;
    use crate::resources::{GridTopology, MovementType, UnitRegistry, UnitType};

    #[test]
    fn pickup_tie_keeps_first_rom_scan_candidate_for_both_iq_levels() {
        let current = Some((
            PickupScore { loadable_count: 2 },
            GridPosition { x: 3, y: 3 },
        ));
        let score = PickupScore { loadable_count: 2 };

        assert!(!pickup_candidate_is_better(current, score));
    }

    #[test]
    fn map3_first_transport_uses_rom_maximum_coverage_tile() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_3",
            GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>();
        let infantry = world.resource::<UnitRegistry>().0[&UnitType::Infantry].clone();
        let passenger_positions = [
            GridPosition { x: 6, y: 8 },
            GridPosition { x: 7, y: 3 },
            GridPosition { x: 5, y: 2 },
            GridPosition { x: 6, y: 9 },
            GridPosition { x: 6, y: 7 },
            GridPosition { x: 5, y: 5 },
            GridPosition { x: 6, y: 6 },
            GridPosition { x: 6, y: 5 },
            GridPosition { x: 5, y: 6 },
        ];
        let passengers: Vec<_> = passenger_positions
            .iter()
            .enumerate()
            .map(|(index, position)| PassengerView {
                entity: Entity::from_raw(index as u32),
                position: *position,
                stats: infantry.clone(),
                fuel: 99,
                // ROM実測では通常移動を行ったrecord 4/5だけbit 5が解除される。
                available: !matches!(index, 3 | 4),
            })
            .collect();
        let mut occupants = HashMap::new();
        for position in passenger_positions {
            occupants.insert(
                (position.x, position.y),
                OccupantInfo {
                    player_id: PlayerId(1),
                    is_transport: false,
                    unit_type: UnitType::Infantry,
                    loadable_types: Vec::new(),
                    free_slots: 0,
                },
            );
        }
        let actor_position = GridPosition { x: 5, y: 4 };
        let coverage = build_pickup_coverage(
            map,
            &master_data,
            &passengers,
            &occupants,
            actor_position,
            PlayerId(1),
            AiVersion::V100,
        );
        let selected = select_pickup_field_target(map, UnitType::TransportHelicopter, &coverage);

        assert_eq!(
            selected,
            Some(GridPosition { x: 7, y: 6 }),
            "GB上位2候補: (7,6)={:?}, (5,7)={:?}",
            coverage.get(&GridPosition { x: 7, y: 6 }),
            coverage.get(&GridPosition { x: 5, y: 7 })
        );
    }

    #[test]
    fn loading_into_lander_selects_the_reachable_transport() {
        let mut world = World::new();
        let pickup_port = GridPosition { x: 2, y: 2 };
        let passenger = world
            .spawn((
                GridPosition { x: 1, y: 2 },
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let lander = world
            .spawn((
                pickup_port,
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Lander,
                    movement_type: MovementType::Ship,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
            ))
            .id();
        let candidates = [CandidateTile {
            position: pickup_port,
            movement_cost: 1,
        }];

        assert!(
            choose_load(
                &mut world,
                passenger,
                &candidates,
                &HashSet::new(),
                PlayerId(1),
            )
            .is_none(),
            "任務3ではない輸送先をROM 524Cが拒否する"
        );
        let command = choose_load(
            &mut world,
            passenger,
            &candidates,
            &HashSet::from([lander]),
            PlayerId(1),
        )
        .unwrap();

        assert!(matches!(
            command,
            AiCommand::Load {
                target_pos,
                transport_entity,
            } if target_pos == pickup_port && transport_entity == lander
        ));
    }

    #[test]
    fn loaded_transport_checks_later_cargo_when_first_cannot_drop() {
        let mut world = World::new();
        let mut map = Map::new(3, 3, Terrain::Sea, GridTopology::Hex);
        map.set_terrain(1, 1, Terrain::City).unwrap();
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
            AiVersion::V100,
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
    fn loaded_lander_can_drop_only_from_rom_facility_origin() {
        let mut world = World::new();
        let mut map = Map::new(9, 5, Terrain::Sea, GridTopology::Hex);
        // ROM 5610は輸送側の停止マスが施設コード0x30以上の場合だけ降車を許す。
        for x in 0..9 {
            map.set_terrain(x, 2, Terrain::Plains).unwrap();
        }
        let pickup_port = GridPosition { x: 1, y: 2 };
        let destination_port = GridPosition { x: 7, y: 2 };
        let objective = GridPosition { x: 8, y: 2 };
        map.set_terrain(pickup_port.x, pickup_port.y, Terrain::Port)
            .unwrap();
        map.set_terrain(destination_port.x, destination_port.y, Terrain::Port)
            .unwrap();
        map.set_terrain(0, 2, Terrain::Capital).unwrap();
        map.set_terrain(objective.x, objective.y, Terrain::City)
            .unwrap();
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        world.spawn((
            GridPosition { x: 0, y: 2 },
            Property::new(Terrain::Capital, Some(PlayerId(1)), 400),
        ));
        world.spawn((
            objective,
            Property::new(Terrain::City, Some(PlayerId(2)), 200),
        ));

        let cargo = world
            .spawn(UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                ..UnitStats::mock()
            })
            .id();
        let actor = world
            .spawn((
                pickup_port,
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
                position: pickup_port,
                movement_cost: 0,
            },
            CandidateTile {
                position: GridPosition { x: 2, y: 1 },
                movement_cost: 1,
            },
        ];

        let command = choose_loaded_action(
            &mut world,
            actor,
            &[cargo],
            &candidates,
            Some(objective),
            PlayerId(1),
            AiVersion::V100,
        )
        .unwrap();

        assert!(matches!(
            command,
            AiCommand::Drop {
                transport_target_pos,
                ..
            } if transport_target_pos == pickup_port
        ));
    }

    #[test]
    fn loaded_lander_leaves_route_progress_to_normal_movement() {
        let mut world = World::new();
        let mut map = Map::new(9, 7, Terrain::Sea, GridTopology::Hex);
        // 施設へ直進すると陸壁の手前で止まるが、南端の浅瀬からは降車できる形にする。
        for y in 0..6 {
            map.set_terrain(5, y, Terrain::Plains).unwrap();
        }
        map.set_terrain(5, 6, Terrain::Shoal).unwrap();
        map.set_terrain(6, 1, Terrain::City).unwrap();
        let master_data = MasterDataRegistry::load().unwrap();
        world.insert_resource(map);
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

        let command = choose_loaded_action(
            &mut world,
            actor,
            &[cargo],
            &candidates,
            Some(objective),
            PlayerId(1),
            AiVersion::V100,
        );

        assert!(command.is_none());
    }
}
