use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;
use std::collections::{BinaryHeap, HashMap, HashSet};

#[derive(Clone)]
pub struct OccupantInfo {
    pub player_id: PlayerId,
    pub is_transport: bool,
    pub loadable_types: Vec<UnitType>,
    pub free_slots: u32,
}

pub fn is_enemy_zoc(
    map: &Map,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    player_id: PlayerId,
    x: usize,
    y: usize,
) -> bool {
    let adj = map.get_adjacent(x, y);
    for &(nx, ny) in &adj {
        if unit_positions
            .get(&(nx, ny))
            .is_some_and(|occ| occ.player_id != player_id)
        {
            return true;
        }
    }
    false
}

/// ユニットの移動タイプと地形から有効な移動コスト（99未満）を返します。
pub fn get_valid_movement_cost(
    master_data: &crate::resources::master_data::MasterDataRegistry,
    movement_type: MovementType,
    terrain: Terrain,
) -> Option<u32> {
    master_data
        .get_movement_cost(movement_type, terrain.as_str())
        .filter(|&c| c < 99)
}

/// 指定された地点から到達可能なすべてのタイルの座標を計算します。ZOCや燃料・移動コストも加味します。
#[allow(clippy::too_many_arguments)]
pub fn calculate_reachable_tiles(
    map: &Map,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    start: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    max_fuel: u32,
    player_id: PlayerId,
    moving_unit_type: UnitType,
    master_data: &crate::resources::master_data::MasterDataRegistry,
) -> HashSet<(usize, usize)> {
    #[derive(Copy, Clone, Eq, PartialEq)]
    struct State {
        cost: u32,
        fuel_used: u32,
        position: (usize, usize),
    }

    impl Ord for State {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            other
                .cost
                .cmp(&self.cost)
                .then_with(|| self.position.cmp(&other.position))
        }
    }
    impl PartialOrd for State {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    let mut reachable = HashSet::new();
    let mut heap = BinaryHeap::new();
    let mut min_cost: HashMap<(usize, usize), u32> = HashMap::new();

    heap.push(State {
        cost: 0,
        fuel_used: 0,
        position: start,
    });
    min_cost.insert(start, 0);

    while let Some(State {
        cost,
        fuel_used,
        position,
    }) = heap.pop()
    {
        if min_cost.get(&position).is_some_and(|&c| cost > c) {
            continue;
        }

        reachable.insert(position);

        if fuel_used >= max_fuel {
            continue;
        }

        // 敵ユニットによるZOC（隣接マスに敵がいる）に進入した場合は、そのマスで停止しなければならない
        if position != start && is_enemy_zoc(map, unit_positions, player_id, position.0, position.1)
        {
            continue;
        }

        // 敵ユニットが占有しているマスは通過不可
        if position != start
            && unit_positions
                .get(&position)
                .is_some_and(|occ| occ.player_id != player_id)
        {
            continue; // Enemy unit: Cannot expand through (block)
        }

        for (nx, ny) in map.get_adjacent(position.0, position.1) {
            if unit_positions
                .get(&(nx, ny))
                .is_some_and(|occ| occ.player_id != player_id)
            {
                continue; // Enemy: Can't enter/pass
            }

            if let Some(terrain_cost) = map
                .get_terrain(nx, ny)
                .and_then(|t| get_valid_movement_cost(master_data, movement_type, t))
            {
                let next_cost = cost + terrain_cost;
                let next_fuel = fuel_used + 1;

                if next_cost <= max_mp && next_fuel <= max_fuel {
                    let is_better = min_cost.get(&(nx, ny)).is_none_or(|&c| next_cost < c);
                    if is_better {
                        min_cost.insert((nx, ny), next_cost);
                        heap.push(State {
                            cost: next_cost,
                            fuel_used: next_fuel,
                            position: (nx, ny),
                        });
                    }
                }
            }
        }
    }

    reachable.retain(|&pos| {
        if pos == start {
            true
        } else if let Some(occ) = unit_positions.get(&pos) {
            occ.player_id == player_id
                && occ.is_transport
                && occ.free_slots > 0
                && occ.loadable_types.contains(&moving_unit_type)
        } else {
            true
        }
    });
    reachable
}

/// A*アルゴリズムを用いて、目的地までの最短経路を探索し、(経路, 消費コスト, 消費燃料) を返します。
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub fn find_path_a_star(
    map: &Map,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    start: (usize, usize),
    goal: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    max_fuel: u32,
    player_id: PlayerId,
    moving_unit_type: UnitType,
    master_data: &crate::resources::master_data::MasterDataRegistry,
) -> Option<(Vec<(usize, usize)>, u32, u32)> {
    let reachable = calculate_reachable_tiles(
        map,
        unit_positions,
        start,
        movement_type,
        max_mp,
        max_fuel,
        player_id,
        moving_unit_type,
        master_data,
    );
    if !reachable.contains(&goal) {
        return None;
    }

    #[derive(Copy, Clone, Eq, PartialEq)]
    struct AStarState {
        cost: u32,
        fuel_used: u32,
        position: (usize, usize),
        f_score: u32,
    }

    impl Ord for AStarState {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            other
                .f_score
                .cmp(&self.f_score)
                .then_with(|| self.position.cmp(&other.position))
        }
    }
    impl PartialOrd for AStarState {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    let heuristic = |a: (usize, usize), b: (usize, usize)| -> u32 {
        ((a.0 as isize - b.0 as isize).abs() + (a.1 as isize - b.1 as isize).abs()) as u32
    };

    let mut heap = BinaryHeap::new();
    let mut came_from: HashMap<(usize, usize), (usize, usize)> = HashMap::new();
    let mut g_score: HashMap<(usize, usize), u32> = HashMap::new();
    let mut fuel_score: HashMap<(usize, usize), u32> = HashMap::new();

    g_score.insert(start, 0);
    fuel_score.insert(start, 0);
    heap.push(AStarState {
        cost: 0,
        fuel_used: 0,
        position: start,
        f_score: heuristic(start, goal),
    });

    while let Some(AStarState {
        cost,
        fuel_used,
        position,
        ..
    }) = heap.pop()
    {
        if position == goal {
            let mut curr = goal;
            let mut path = vec![curr];
            while let Some(&prev) = came_from.get(&curr) {
                curr = prev;
                path.push(curr);
            }
            path.reverse();
            return Some((path, cost, fuel_used));
        }

        if g_score.get(&position).is_some_and(|&g| cost > g) {
            continue;
        }

        if fuel_used >= max_fuel {
            continue;
        }
        // 敵ZOC進入による強制停止
        if position != start && is_enemy_zoc(map, unit_positions, player_id, position.0, position.1)
        {
            continue;
        }

        // 敵ユニットがいるマスは通過不可
        if position != start
            && unit_positions
                .get(&position)
                .is_some_and(|occ| occ.player_id != player_id)
        {
            continue; // Enemy unit: Cannot expand through
        }

        for (nx, ny) in map.get_adjacent(position.0, position.1) {
            if let Some(occ) = unit_positions.get(&(nx, ny)) {
                if occ.player_id != player_id {
                    continue; // Enemy: Can't enter/pass
                }
                if (nx, ny) == goal {
                    let can_load = occ.is_transport
                        && occ.free_slots > 0
                        && occ.loadable_types.contains(&moving_unit_type);
                    if !can_load {
                        continue; // Destination occupied by ally, not a valid transport
                    }
                }
            }

            if let Some(terrain_cost) = map
                .get_terrain(nx, ny)
                .and_then(|t| get_valid_movement_cost(master_data, movement_type, t))
            {
                let next_cost = cost + terrain_cost;
                let next_fuel = fuel_used + 1;

                if next_cost <= max_mp && next_fuel <= max_fuel {
                    let is_better = g_score.get(&(nx, ny)).is_none_or(|&g| next_cost < g);
                    if is_better {
                        g_score.insert((nx, ny), next_cost);
                        fuel_score.insert((nx, ny), next_fuel);
                        came_from.insert((nx, ny), position);
                        heap.push(AStarState {
                            cost: next_cost,
                            fuel_used: next_fuel,
                            position: (nx, ny),
                            f_score: next_cost + heuristic((nx, ny), goal),
                        });
                    }
                }
            }
        }
    }

    None
}

/// ユニットの移動コマンド(`MoveUnitCommand`)を処理するシステム。
///
/// 【処理の流れ】
/// 1. ユニットの現在位置(`GridPosition`)、燃料(`Fuel`)、移動力(`UnitStats`)を取得します。
/// 2. A*アルゴリズムを用いて、目的地までの到達可能性と消費燃料・コストを計算します。
/// 3. 移動可能であれば、位置情報を更新し、燃料を消費します。
/// 4. ユニットの `HasMoved` フラグを true に設定します。
/// 5. 移動先に同じプレイヤーの輸送ユニットが待機しており、積載条件を満たしていれば `LoadUnitCommand` を発行して自動積載します。
/// 6. 移動結果を `UnitMovedEvent` として発行します。
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub fn move_unit_system(
    mut move_events: EventReader<MoveUnitCommand>,
    mut moved_events: EventWriter<UnitMovedEvent>,
    mut load_events: EventWriter<LoadUnitCommand>,
    mut q_units: Query<(
        Entity,
        &mut GridPosition,
        &mut Fuel,
        &mut HasMoved,
        &Faction,
        &UnitStats,
        &ActionCompleted,
        Option<&Transporting>,
        Option<&CargoCapacity>,
    )>,
    map: Res<Map>,
    players: Res<Players>,
    match_state: Res<MatchState>,
    master_data: Res<crate::resources::master_data::MasterDataRegistry>,
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Main {
        return;
    }
    let active_player = players.0[match_state.active_player_index.0].id;

    for event in move_events.read() {
        let mut unit_positions = HashMap::new();
        for (_, pos, _, _, faction, stats, _, trans, cargo_opt) in q_units.iter() {
            if trans.is_none() {
                let free_slots = cargo_opt
                    .map(|c| c.max.saturating_sub(c.loaded.len() as u32))
                    .unwrap_or(0);
                unit_positions.insert(
                    (pos.x, pos.y),
                    OccupantInfo {
                        player_id: faction.0,
                        is_transport: stats.max_cargo > 0,
                        loadable_types: stats.loadable_unit_types.clone(),
                        free_slots,
                    },
                );
            }
        }

        let mut load_action = None;

        if let Ok((
            entity,
            mut pos,
            mut fuel,
            mut has_moved,
            faction,
            stats,
            action_completed,
            _,
            _,
        )) = q_units.get_mut(event.unit_entity)
        {
            if faction.0 != active_player {
                continue;
            }
            if action_completed.0 {
                continue;
            }
            if has_moved.0 {
                continue;
            }

            if let Some((_path, _cost, fuel_used)) = find_path_a_star(
                &map,
                &unit_positions,
                (pos.x, pos.y),
                (event.target_x, event.target_y),
                stats.movement_type,
                stats.max_movement,
                fuel.current,
                faction.0,
                stats.unit_type,
                &master_data,
            ) {
                let from = *pos;
                pos.x = event.target_x;
                pos.y = event.target_y;
                fuel.current = fuel.current.saturating_sub(fuel_used);
                has_moved.0 = true;

                // Fire event
                moved_events.send(UnitMovedEvent {
                    entity,
                    from,
                    to: *pos,
                    fuel_used,
                });

                load_action = Some((
                    entity,
                    event.target_x,
                    event.target_y,
                    faction.0,
                    stats.unit_type,
                ));
            }
        }

        if let Some((unit_e, tx, ty, fac_id, u_type)) = load_action {
            let mut transport_entity = None;
            for (e, t_pos, _, _, f_faction, s_stats, _, _, _) in q_units.iter() {
                if e != unit_e
                    && t_pos.x == tx
                    && t_pos.y == ty
                    && f_faction.0 == fac_id
                    && s_stats.max_cargo > 0
                    && s_stats.loadable_unit_types.contains(&u_type)
                {
                    transport_entity = Some(e);
                    break;
                }
            }
            if let Some(te) = transport_entity {
                load_events.send(LoadUnitCommand {
                    transport_entity: te,
                    unit_entity: unit_e,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_infantry_stats() -> UnitStats {
        UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1000,
            max_movement: 3,
            movement_type: MovementType::Infantry,
            max_fuel: 99,
            max_ammo1: 0,
            max_ammo2: 0,
            min_range: 1,
            max_range: 1,
            daily_fuel_consumption: 0,
            can_capture: true,
            can_supply: false,
            max_cargo: 0,
            loadable_unit_types: vec![],
        }
    }

    fn create_bcopter_stats() -> UnitStats {
        UnitStats {
            unit_type: UnitType::Bcopters,
            cost: 9000,
            max_movement: 6,
            movement_type: MovementType::Air,
            max_fuel: 3,
            max_ammo1: 6,
            max_ammo2: 0,
            min_range: 1,
            max_range: 1,
            daily_fuel_consumption: 2,
            can_capture: false,
            can_supply: false,
            max_cargo: 0,
            loadable_unit_types: vec![],
        }
    }

    fn create_transport_heli_stats() -> UnitStats {
        UnitStats {
            unit_type: UnitType::TransportHelicopter,
            cost: 5000,
            max_movement: 6,
            movement_type: MovementType::Air,
            max_fuel: 99,
            max_ammo1: 0,
            max_ammo2: 0,
            min_range: 1,
            max_range: 1,
            daily_fuel_consumption: 2,
            can_capture: false,
            can_supply: false,
            max_cargo: 2,
            loadable_unit_types: vec![UnitType::Infantry],
        }
    }

    #[test]
    fn test_move_unit_system() {
        let mut world = World::new();
        world.insert_resource(MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        });
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Road).unwrap();
        world.insert_resource(map);
        world.insert_resource(crate::resources::master_data::MasterDataRegistry::load().unwrap());
        world.insert_resource(Events::<MoveUnitCommand>::default());
        world.insert_resource(Events::<UnitMovedEvent>::default());
        world.insert_resource(Events::<LoadUnitCommand>::default());

        let entity = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                Fuel {
                    current: 10,
                    max: 10,
                },
                HasMoved(false),
                Faction(PlayerId(1)),
                UnitStats {
                    max_fuel: 10,
                    ..create_infantry_stats()
                },
                ActionCompleted(false),
            ))
            .id();

        world.send_event(MoveUnitCommand {
            unit_entity: entity,
            target_x: 2,
            target_y: 0,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(move_unit_system);
        schedule.run(&mut world);

        let pos = world.get::<GridPosition>(entity).unwrap();
        assert_eq!(pos.x, 2);
        assert_eq!(pos.y, 0);

        let fuel = world.get::<Fuel>(entity).unwrap();
        assert_eq!(fuel.current, 8); // Moved 2 tiles plains cost 1 each
    }

    #[test]
    fn test_air_unit_fuel_and_crash() {
        let mut world = World::new();
        world.insert_resource(MatchState::default());
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Airport).unwrap();
        world.insert_resource(map);
        world.insert_resource(crate::resources::master_data::MasterDataRegistry::load().unwrap());
        world.insert_resource(Events::<crate::events::NextPhaseCommand>::default());
        world.insert_resource(Events::<crate::events::GamePhaseChangedEvent>::default());

        world.spawn((
            GridPosition { x: 0, y: 0 },
            crate::components::Property::new(Terrain::Airport, Some(PlayerId(1))),
        ));

        let heli_stats = create_bcopter_stats();

        // Heli at airport (will resupply/not crash)
        let heli1 = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel { current: 3, max: 3 },
                Ammo {
                    ammo1: 6,
                    max_ammo1: 6,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                heli_stats.clone(),
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();

        // Heli away from airport (will consume fuel and crash)
        let heli2 = world
            .spawn((
                GridPosition { x: 1, y: 1 },
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel { current: 3, max: 3 },
                Ammo {
                    ammo1: 6,
                    max_ammo1: 6,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                heli_stats.clone(),
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();

        let mut schedule = Schedule::default();
        schedule.add_systems(crate::systems::turn_management::next_phase_system);

        let advance_day = |w: &mut World, s: &mut Schedule| {
            for _ in 0..2 {
                w.send_event(crate::events::NextPhaseCommand);
                s.run(w);
            }
        };

        advance_day(&mut world, &mut schedule);
        let f2 = world.get::<Fuel>(heli2).unwrap();
        assert_eq!(f2.current, 1); // 3 - 2

        advance_day(&mut world, &mut schedule);
        let f2 = world.get::<Fuel>(heli2).unwrap();
        assert_eq!(f2.current, 0);

        advance_day(&mut world, &mut schedule);
        let h2 = world.get::<Health>(heli2).unwrap();
        assert!(h2.is_destroyed());

        let h1 = world.get::<Health>(heli1).unwrap();
        assert!(!h1.is_destroyed());
    }

    #[test]
    fn test_auto_load_on_move() {
        let mut world = World::new();
        world.insert_resource(MatchState::default());
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));
        let map = Map::new(10, 10, Terrain::Plains, GridTopology::Square);
        world.insert_resource(map);
        world.insert_resource(crate::resources::master_data::MasterDataRegistry::load().unwrap());
        world.insert_resource(Events::<MoveUnitCommand>::default());
        world.insert_resource(Events::<UnitMovedEvent>::default());
        world.insert_resource(Events::<LoadUnitCommand>::default());

        world.spawn((
            GridPosition { x: 5, y: 5 },
            Faction(PlayerId(1)),
            Health {
                current: 100,
                max: 100,
            },
            Fuel {
                current: 99,
                max: 99,
            },
            create_transport_heli_stats(),
            HasMoved(false),
            ActionCompleted(false),
            CargoCapacity {
                max: 2,
                loaded: vec![],
            },
        ));

        let inf_entity = world
            .spawn((
                GridPosition { x: 3, y: 5 },
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
                create_infantry_stats(),
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();

        world.insert_resource(MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        });

        world.send_event(MoveUnitCommand {
            unit_entity: inf_entity,
            target_x: 5,
            target_y: 5,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(super::move_unit_system);
        schedule.run(&mut world);

        let load_events = world.resource::<Events<LoadUnitCommand>>();
        let mut reader = load_events.get_cursor();
        let emitted: Vec<_> = reader.read(load_events).collect();
        assert_eq!(emitted.len(), 1);
    }

    #[test]
    fn test_pass_through_allied_unit() {
        let mut world = World::new();
        world.insert_resource(MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        });
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));
        let map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        world.insert_resource(map);
        world.insert_resource(crate::resources::master_data::MasterDataRegistry::load().unwrap());
        world.insert_resource(Events::<MoveUnitCommand>::default());
        world.insert_resource(Events::<UnitMovedEvent>::default());
        world.insert_resource(Events::<LoadUnitCommand>::default());

        let inf_stats = create_infantry_stats();

        // Subject unit (Player 1)
        let subject = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                Faction(PlayerId(1)),
                Fuel {
                    current: 99,
                    max: 99,
                },
                inf_stats.clone(),
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();

        // Allied unit at (1, 0)
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Faction(PlayerId(1)),
            inf_stats.clone(),
            Fuel {
                current: 10,
                max: 10,
            },
            HasMoved(false),
            ActionCompleted(false),
        ));

        // Move to (2, 0) through (1, 0)
        world.send_event(MoveUnitCommand {
            unit_entity: subject,
            target_x: 2,
            target_y: 0,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(move_unit_system);
        schedule.run(&mut world);

        let pos = world.get::<GridPosition>(subject).unwrap();
        assert_eq!(pos.x, 2, "Should be able to pass through allied unit");
        assert_eq!(pos.y, 0);
    }

    #[test]
    fn test_cannot_stop_on_allied_unit() {
        let mut world = World::new();
        world.insert_resource(MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        });
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));
        let map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        world.insert_resource(map);
        world.insert_resource(crate::resources::master_data::MasterDataRegistry::load().unwrap());
        world.insert_resource(Events::<MoveUnitCommand>::default());
        world.insert_resource(Events::<UnitMovedEvent>::default());
        world.insert_resource(Events::<LoadUnitCommand>::default());

        let inf_stats = create_infantry_stats();

        // Subject unit
        let subject = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                Faction(PlayerId(1)),
                Fuel {
                    current: 99,
                    max: 99,
                },
                inf_stats.clone(),
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();

        // Allied unit at (1, 0)
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Faction(PlayerId(1)),
            inf_stats.clone(),
            Fuel {
                current: 10,
                max: 10,
            },
            HasMoved(false),
            ActionCompleted(false),
        ));

        // Try to move to (1, 0)
        world.send_event(MoveUnitCommand {
            unit_entity: subject,
            target_x: 1,
            target_y: 0,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(move_unit_system);
        schedule.run(&mut world);

        let pos = world.get::<GridPosition>(subject).unwrap();
        assert_eq!(pos.x, 0, "Should NOT be able to stop on allied unit");
        assert_eq!(pos.y, 0);
    }

    #[test]
    fn test_zoc_stoppage_with_ally() {
        let mut world = World::new();
        world.insert_resource(MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        });
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));
        let map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        world.insert_resource(map);
        world.insert_resource(crate::resources::master_data::MasterDataRegistry::load().unwrap());
        world.insert_resource(Events::<MoveUnitCommand>::default());
        world.insert_resource(Events::<UnitMovedEvent>::default());
        world.insert_resource(Events::<LoadUnitCommand>::default());

        let inf_stats = create_infantry_stats();

        // Subject unit
        let subject = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                Faction(PlayerId(1)),
                Fuel {
                    current: 99,
                    max: 99,
                },
                inf_stats.clone(),
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();

        // Enemy at (1, 1). This makes (1, 0) a ZOC tile.
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Faction(PlayerId(2)),
            inf_stats.clone(),
            Fuel {
                current: 10,
                max: 10,
            },
            HasMoved(false),
            ActionCompleted(false),
        ));

        // Allied unit at (1, 0)
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Faction(PlayerId(1)),
            inf_stats.clone(),
            Fuel {
                current: 10,
                max: 10,
            },
            HasMoved(false),
            ActionCompleted(false),
        ));

        // Try to move to (2, 0) through (1, 0)
        world.send_event(MoveUnitCommand {
            unit_entity: subject,
            target_x: 2,
            target_y: 0,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(move_unit_system);
        schedule.run(&mut world);

        let pos = world.get::<GridPosition>(subject).unwrap();
        assert_eq!(
            pos.x, 0,
            "Should be stopped by ZOC and unable to stop on ally at (1,0)"
        );
    }
}
