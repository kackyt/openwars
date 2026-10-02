//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

/// 実ゲームの移動力繰越禁止・燃料消費・移動後間接攻撃禁止を考慮し、
/// 指定射程帯から攻撃可能になるまでの最短行動ターンを返します。
#[allow(clippy::too_many_arguments)]
pub fn calculate_action_distance_to_range(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    start: (usize, usize),
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    max_fuel: u32,
    interaction_min_range: u32,
    interaction_max_range: u32,
    player_id: PlayerId,
    cache: &mut ActionTurnDistanceCache,
) -> Option<ActionTurnDistance> {
    calculate_action_distance_to_range_with_origin_policy(
        map,
        registry,
        unit_positions,
        start,
        target,
        movement_type,
        max_mp,
        max_fuel,
        interaction_min_range,
        interaction_max_range,
        player_id,
        false,
        cache,
    )
}

/// 生産施設から生まれた間接攻撃unitが、次手番の生産口を譲るために一度退避してから
/// 射撃可能になるまでの最短行動ターンを返す。
#[allow(clippy::too_many_arguments)]
pub fn calculate_action_distance_to_range_after_leaving_start(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    start: (usize, usize),
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    max_fuel: u32,
    interaction_min_range: u32,
    interaction_max_range: u32,
    player_id: PlayerId,
    cache: &mut ActionTurnDistanceCache,
) -> Option<ActionTurnDistance> {
    calculate_action_distance_to_range_with_origin_policy(
        map,
        registry,
        unit_positions,
        start,
        target,
        movement_type,
        max_mp,
        max_fuel,
        interaction_min_range,
        interaction_max_range,
        player_id,
        true,
        cache,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn calculate_action_distance_to_range_with_origin_policy(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    start: (usize, usize),
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    max_fuel: u32,
    interaction_min_range: u32,
    interaction_max_range: u32,
    player_id: PlayerId,
    must_leave_start: bool,
    cache: &mut ActionTurnDistanceCache,
) -> Option<ActionTurnDistance> {
    // 配置を正規化し、空盤面・敵の封鎖・味方の停止位置を別の探索として保持する。
    // この探索が参照する占有情報は座標と所有者のみ。
    let mut occupancy: Vec<_> = unit_positions
        .iter()
        .map(|(&(x, y), occupant)| (x, y, occupant.player_id))
        .collect();
    occupancy.sort_unstable_by_key(|&(x, y, player)| (x, y, player.0));
    let occupancy: Arc<[_]> = occupancy.into();
    let cache_key = ActionRangeCacheKey {
        occupancy: Arc::clone(&occupancy),
        start,
        target,
        movement_type,
        max_mp,
        max_fuel,
        min_range: interaction_min_range,
        max_range: interaction_max_range,
        player_id,
        must_leave_start,
    };
    if let Some(cached) = cache.cache.get(&cache_key) {
        return *cached;
    }

    if start.0 >= map.width
        || start.1 >= map.height
        || target.0 >= map.width
        || target.1 >= map.height
        || interaction_min_range > interaction_max_range
    {
        cache.cache.insert(cache_key, None);
        return None;
    }

    let mut firing_positions = HashSet::new();
    for dx in -(interaction_max_range as i32)..=(interaction_max_range as i32) {
        for dy in -(interaction_max_range as i32)..=(interaction_max_range as i32) {
            let nx = target.0 as i32 + dx;
            let ny = target.1 as i32 + dy;
            if nx < 0 || nx >= map.width as i32 || ny < 0 || ny >= map.height as i32 {
                continue;
            }
            let position = (nx as usize, ny as usize);
            let range = map.distance(position.0, position.1, target.0, target.1);
            if range < interaction_min_range || range > interaction_max_range {
                continue;
            }
            let Some(terrain_cost) = map
                .get_terrain(position.0, position.1)
                .and_then(|terrain| get_valid_movement_cost(registry, movement_type, terrain))
            else {
                continue;
            };
            if terrain_cost > max_mp && position != start {
                continue;
            }
            // 射撃位置では合流・搭載を行わないため、自分の開始位置以外の占有マスは除外する。
            if position != start && unit_positions.contains_key(&position) {
                continue;
            }
            firing_positions.insert(position);
        }
    }

    if firing_positions.contains(&start) && !must_leave_start {
        let result = Some(ActionTurnDistance {
            turns: 0,
            used_mp: 0,
            used_fuel: 0,
            requires_movement: false,
            firing_position: GridPosition {
                x: start.0,
                y: start.1,
            },
        });
        cache.cache.insert(cache_key, result);
        return result;
    }
    if firing_positions.is_empty() || max_mp == 0 {
        cache.cache.insert(cache_key, None);
        return None;
    }

    let source_key = ActionSourceCacheKey {
        occupancy,
        start,
        movement_type,
        max_mp,
        max_fuel,
        player_id,
    };
    let best_by_position = if let Some(cached) = cache.source_cache.get(&source_key) {
        Arc::clone(cached)
    } else {
        let mut heap = BinaryHeap::new();
        let mut best_by_position = HashMap::new();
        let start_state = ActionState {
            turns: 0,
            mp_in_turn: 0,
            total_mp: 0,
            used_fuel: 0,
            position: start,
        };
        heap.push(start_state);
        best_by_position.insert(start, (0, 0, 0, 0));

        while let Some(state) = heap.pop() {
            let state_cost = (
                state.turns,
                state.mp_in_turn,
                state.total_mp,
                state.used_fuel,
            );
            if best_by_position
                .get(&state.position)
                .is_some_and(|best| state_cost > *best)
            {
                continue;
            }

            // 実移動と同様、敵ZOCへ進入した手番はそのマスで移動を終了する。
            // 次の隣接マスへは次手番の移動力を使って進めるため、到達不能にはしない。
            let must_start_next_turn = state.position != start
                && is_enemy_zoc(
                    map,
                    unit_positions,
                    player_id,
                    state.position.0,
                    state.position.1,
                );

            for next_position in map.get_adjacent(state.position.0, state.position.1) {
                if unit_positions
                    .get(&next_position)
                    .is_some_and(|occupant| occupant.player_id != player_id)
                {
                    continue;
                }
                let Some(move_cost) = map
                    .get_terrain(next_position.0, next_position.1)
                    .and_then(|terrain| get_valid_movement_cost(registry, movement_type, terrain))
                else {
                    continue;
                };
                if move_cost > max_mp || state.used_fuel >= max_fuel {
                    continue;
                }

                let (next_turns, next_mp_in_turn) = if state.turns == 0 {
                    (1, move_cost)
                } else if must_start_next_turn {
                    (state.turns.saturating_add(1), move_cost)
                } else if state.mp_in_turn.saturating_add(move_cost) <= max_mp {
                    (state.turns, state.mp_in_turn + move_cost)
                } else {
                    (state.turns.saturating_add(1), move_cost)
                };
                let next_state = ActionState {
                    turns: next_turns,
                    mp_in_turn: next_mp_in_turn,
                    total_mp: state.total_mp.saturating_add(move_cost),
                    used_fuel: state.used_fuel + 1,
                    position: next_position,
                };
                let next_cost = (
                    next_state.turns,
                    next_state.mp_in_turn,
                    next_state.total_mp,
                    next_state.used_fuel,
                );
                if best_by_position
                    .get(&next_position)
                    .is_none_or(|current| next_cost < *current)
                {
                    best_by_position.insert(next_position, next_cost);
                    heap.push(next_state);
                }
            }
        }
        let calculated = Arc::new(best_by_position);
        cache
            .source_cache
            .insert(source_key, Arc::clone(&calculated));
        calculated
    };
    let mut best_result: Option<ActionTurnDistance> = None;
    for firing_position in firing_positions {
        if must_leave_start && firing_position == start {
            continue;
        }
        if let Some((turns, _, total_mp, used_fuel)) = best_by_position.get(&firing_position) {
            let firing_distance =
                map.distance(firing_position.0, firing_position.1, target.0, target.1);
            let indirect_setup = u32::from(*turns > 0 && firing_distance > 1);
            let candidate = ActionTurnDistance {
                turns: turns.saturating_add(indirect_setup),
                used_mp: *total_mp,
                used_fuel: *used_fuel,
                requires_movement: true,
                firing_position: GridPosition {
                    x: firing_position.0,
                    y: firing_position.1,
                },
            };
            let replace = best_result.is_none_or(|current| {
                (
                    candidate.turns,
                    candidate.used_fuel,
                    candidate.used_mp,
                    candidate.firing_position.x,
                    candidate.firing_position.y,
                ) < (
                    current.turns,
                    current.used_fuel,
                    current.used_mp,
                    current.firing_position.x,
                    current.firing_position.y,
                )
            });
            if replace {
                best_result = Some(candidate);
            }
        }
    }

    cache.cache.insert(cache_key, best_result);
    best_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::Terrain;

    #[test]
    fn action_distance_respects_turn_movement_boundaries() {
        let map = Map::new(
            6,
            1,
            Terrain::Forest,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = ActionTurnDistanceCache::default();

        let distance = calculate_action_distance_to_range(
            &map,
            &registry,
            &HashMap::new(),
            (0, 0),
            (5, 0),
            MovementType::Tank,
            5,
            99,
            0,
            0,
            PlayerId(1),
            &mut cache,
        )
        .unwrap();

        assert_eq!(distance.turns, 3);
        assert_eq!(distance.used_mp, 10);
        assert_eq!(distance.used_fuel, 5);
        assert_eq!(distance.firing_position, GridPosition { x: 5, y: 0 });
    }

    #[test]
    fn action_distance_reports_selected_firing_position() {
        let map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = ActionTurnDistanceCache::default();

        let distance = calculate_action_distance_to_range(
            &map,
            &registry,
            &HashMap::new(),
            (0, 0),
            (4, 0),
            MovementType::Tank,
            3,
            99,
            1,
            1,
            PlayerId(1),
            &mut cache,
        )
        .unwrap();

        assert_eq!(distance.turns, 1);
        assert_eq!(distance.firing_position, GridPosition { x: 3, y: 0 });
        assert!(distance.requires_movement);
    }

    #[test]
    fn action_distance_enemy_zoc_ends_only_current_turn() {
        let map = Map::new(
            5,
            2,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = ActionTurnDistanceCache::default();
        let enemy = OccupantInfo {
            player_id: PlayerId(2),
            unit_type: crate::resources::UnitType::Infantry,
            is_transport: false,
            free_slots: 0,
            loadable_types: Vec::new(),
        };
        let unit_positions = HashMap::from([((1, 1), enemy)]);

        let distance = calculate_action_distance_to_range(
            &map,
            &registry,
            &unit_positions,
            (0, 0),
            (4, 0),
            MovementType::Tank,
            4,
            99,
            0,
            0,
            PlayerId(1),
            &mut cache,
        )
        .unwrap();

        // (1, 0) は敵 (1, 1) のZOC。T1はそこで停止し、T2に残り3マスを進む。
        assert_eq!(distance.turns, 2);
        assert_eq!(distance.used_mp, 4);
        assert_eq!(distance.used_fuel, 4);
        assert_eq!(distance.firing_position, GridPosition { x: 4, y: 0 });
    }

    #[test]
    fn action_distance_after_leaving_start_counts_factory_vacation_setup_turn() {
        let map = Map::new(
            5,
            2,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = ActionTurnDistanceCache::default();

        let distance = calculate_action_distance_to_range_after_leaving_start(
            &map,
            &registry,
            &HashMap::new(),
            (0, 0),
            (3, 0),
            MovementType::Tank,
            1,
            1,
            1,
            3,
            PlayerId(1),
            &mut cache,
        )
        .expect("生産地点の外にも合法な射撃地点がある");

        assert!(distance.requires_movement);
        assert_ne!(distance.firing_position, GridPosition { x: 0, y: 0 });
        assert_eq!(distance.turns, 2);
    }

    #[test]
    fn action_distance_cache_distinguishes_occupancy() {
        let map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let empty = HashMap::new();
        let blocked = HashMap::from([(
            (2, 0),
            OccupantInfo {
                player_id: PlayerId(2),
                unit_type: crate::resources::UnitType::Infantry,
                is_transport: false,
                free_slots: 0,
                loadable_types: Vec::new(),
            },
        )]);
        // 同一目標のキャッシュと、異なる目標へ共有する始点キャッシュの両方を検証する。
        for targets in [[4, 4], [3, 4]] {
            for blocked_first in [false, true] {
                let mut cache = ActionTurnDistanceCache::default();
                for (index, is_blocked) in [blocked_first, !blocked_first].into_iter().enumerate() {
                    let result = calculate_action_distance_to_range(
                        &map,
                        &registry,
                        if is_blocked { &blocked } else { &empty },
                        (0, 0),
                        (targets[index], 0),
                        MovementType::Infantry,
                        3,
                        99,
                        1,
                        1,
                        PlayerId(1),
                        &mut cache,
                    );
                    assert_eq!(result.is_none(), is_blocked);
                }
            }
        }
    }
}
