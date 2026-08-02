#![allow(clippy::too_many_arguments)]
#![allow(clippy::manual_div_ceil)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::manual_checked_ops)]

use crate::components::{GridPosition, PlayerId};
use crate::resources::{Map, MovementType, master_data::MasterDataRegistry};
use crate::systems::movement::{OccupantInfo, get_valid_movement_cost};
use bevy_ecs::prelude::*;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// 目的地までのターン数と消費移動力（MP）を保持する構造体。
/// ターン数が同じ場合、消費移動力が小さい経路を優先するために使用します。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnDistance {
    /// 到達にかかるターン数
    pub turns: u32,
    /// 累計で消費した移動力（MP）
    pub used_mp: u32,
}

impl PartialOrd for TurnDistance {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TurnDistance {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.turns
            .cmp(&other.turns)
            .then_with(|| self.used_mp.cmp(&other.used_mp))
    }
}

pub type TurnCacheKey = (
    usize,
    usize,
    usize,
    usize,
    MovementType,
    u32,
    u32,
    u32,
    PlayerId,
);

/// 移動タイプごとの地形連結成分を計算し、同一分析中の到達判定で再利用します。
#[derive(Default)]
pub struct TerrainConnectivity {
    components: HashMap<MovementType, Vec<Option<usize>>>,
}

impl TerrainConnectivity {
    pub fn is_reachable(
        &mut self,
        map: &Map,
        registry: &MasterDataRegistry,
        start: (usize, usize),
        target: (usize, usize),
        movement_type: MovementType,
    ) -> bool {
        if start.0 >= map.width
            || start.1 >= map.height
            || target.0 >= map.width
            || target.1 >= map.height
        {
            return false;
        }
        let components = self.components.entry(movement_type).or_insert_with(|| {
            let mut result = vec![None; map.width * map.height];
            let mut next_component = 0;
            for y in 0..map.height {
                for x in 0..map.width {
                    let index = y * map.width + x;
                    if result[index].is_some()
                        || map
                            .get_terrain(x, y)
                            .and_then(|terrain| {
                                get_valid_movement_cost(registry, movement_type, terrain)
                            })
                            .is_none()
                    {
                        continue;
                    }
                    let mut queue = VecDeque::from([(x, y)]);
                    result[index] = Some(next_component);
                    while let Some(position) = queue.pop_front() {
                        for adjacent in map.get_adjacent(position.0, position.1) {
                            let adjacent_index = adjacent.1 * map.width + adjacent.0;
                            if result[adjacent_index].is_some()
                                || map
                                    .get_terrain(adjacent.0, adjacent.1)
                                    .and_then(|terrain| {
                                        get_valid_movement_cost(registry, movement_type, terrain)
                                    })
                                    .is_none()
                            {
                                continue;
                            }
                            result[adjacent_index] = Some(next_component);
                            queue.push_back(adjacent);
                        }
                    }
                    next_component += 1;
                }
            }
            result
        });
        let start_component = components[start.1 * map.width + start.0];
        let target_component = components[target.1 * map.width + target.0];
        start_component.is_some() && start_component == target_component
    }
}

/// ユニット配置や ZOC を無視し、地形だけを使って2地点が接続されているか判定します。
pub fn is_terrain_reachable(
    map: &Map,
    registry: &MasterDataRegistry,
    start: (usize, usize),
    target: (usize, usize),
    movement_type: MovementType,
) -> bool {
    TerrainConnectivity::default().is_reachable(map, registry, start, target, movement_type)
}

/// ターン数ベースの距離計算をキャッシュするためのリソース
#[derive(Resource, Default)]
pub struct TurnDistanceCache {
    /// キー: (出発地x, 出発地y, 目標地x, 目標地y, 移動タイプ, 移動力, 最小射程, 最大射程, 勢力ID)
    /// 値: 到達ターン数と消費移動力を保持する TurnDistance 構造体
    pub cache: HashMap<TurnCacheKey, TurnDistance>,
}

impl TurnDistanceCache {
    pub fn clear(&mut self) {
        self.cache.clear();
    }
}

/// 実際の移動ルールに従って射程帯へ入り、攻撃可能になるまでの距離。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionTurnDistance {
    /// 攻撃可能になる自軍行動ターン。現在位置から即射撃可能なら0。
    pub turns: u32,
    /// 経路上で消費する地形移動コストの合計。
    pub used_mp: u32,
    /// 経路上で消費する燃料。実移動と同じく通過タイル数で数える。
    pub used_fuel: u32,
    /// 射撃位置へ移動する必要があるか。
    pub requires_movement: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ActionRangeCacheKey {
    start: (usize, usize),
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    max_fuel: u32,
    min_range: u32,
    max_range: u32,
    player_id: PlayerId,
}

/// 射程帯への正確な行動ターン距離を同一分析中に再利用するキャッシュ。
#[derive(Default)]
pub struct ActionTurnDistanceCache {
    cache: HashMap<ActionRangeCacheKey, Option<ActionTurnDistance>>,
}

/// 実ターン境界を考慮する Dijkstra の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActionState {
    turns: u32,
    mp_in_turn: u32,
    total_mp: u32,
    used_fuel: u32,
    position: (usize, usize),
}

impl Ord for ActionState {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .turns
            .cmp(&self.turns)
            .then_with(|| other.mp_in_turn.cmp(&self.mp_in_turn))
            .then_with(|| other.total_mp.cmp(&self.total_mp))
            .then_with(|| other.used_fuel.cmp(&self.used_fuel))
            .then_with(|| self.position.cmp(&other.position))
    }
}

impl PartialOrd for ActionState {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A* / Dijkstra の状態
#[derive(Copy, Clone, Eq, PartialEq)]
struct State {
    cost: u32,
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

/// 指定した2地点間のターン数距離を計算します。
/// ターゲットが移動タイプ的に進入不可の場合（船→陸地など）、
/// ターゲットに隣接する進入可能なマスまでの最短ターン数を返します。
/// 到達不可能な場合は u32::MAX を返します。
pub fn calculate_turn_distance(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    start: (usize, usize),
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    interaction_max_range: u32,
    player_id: PlayerId,
    cache: &mut TurnDistanceCache,
) -> TurnDistance {
    calculate_turn_distance_to_range(
        map,
        registry,
        unit_positions,
        start,
        target,
        movement_type,
        max_mp,
        0,
        interaction_max_range,
        player_id,
        cache,
    )
}

/// ターゲットから最小〜最大射程内にある合法な行動位置までのターン数を計算します。
/// 間接攻撃の死角を含めて評価する必要がある対空カバレッジ計算で使用します。
#[allow(clippy::too_many_arguments)]
pub fn calculate_turn_distance_to_range(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    start: (usize, usize),
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    interaction_min_range: u32,
    interaction_max_range: u32,
    player_id: PlayerId,
    cache: &mut TurnDistanceCache,
) -> TurnDistance {
    if start == target && interaction_min_range == 0 {
        return TurnDistance {
            turns: 0,
            used_mp: 0,
        };
    }

    let cache_key = (
        start.0,
        start.1,
        target.0,
        target.1,
        movement_type,
        max_mp,
        interaction_min_range,
        interaction_max_range,
        player_id,
    );
    if let Some(&dist) = cache.cache.get(&cache_key) {
        return dist;
    }

    // ターゲットから指定射程帯に入る、進入可能な全マスを目標地点とする。
    let mut effective_targets = Vec::new();
    for dx in -(interaction_max_range as i32)..=(interaction_max_range as i32) {
        for dy in -(interaction_max_range as i32)..=(interaction_max_range as i32) {
            let nx = target.0 as i32 + dx;
            let ny = target.1 as i32 + dy;
            if nx >= 0 && nx < map.width as i32 && ny >= 0 && ny < map.height as i32 {
                let pos = (nx as usize, ny as usize);
                let range = map.distance(pos.0, pos.1, target.0, target.1);
                if range < interaction_min_range || range > interaction_max_range {
                    continue;
                }
                if let Some(terrain) = map.get_terrain(pos.0, pos.1) {
                    if get_valid_movement_cost(registry, movement_type, terrain).is_some() {
                        let mut can_enter = true;
                        // 目標地点そのもの以外で、敵がいる場合は到達不可
                        if pos != target {
                            if let Some(occupant) = unit_positions.get(&pos) {
                                if occupant.player_id != player_id {
                                    can_enter = false;
                                }
                            }
                        }
                        if can_enter {
                            effective_targets.push(pos);
                        }
                    }
                }
            }
        }
    }

    if effective_targets.is_empty() {
        // ターゲットに隣接する進入可能タイルが存在しない → 到達不可
        let dx = (start.0 as i32 - target.0 as i32).abs();
        let dy = (start.1 as i32 - target.1 as i32).abs();
        let approx = 50 + ((dx + dy) as u32 / 4);
        let approx_encoded = TurnDistance {
            turns: approx,
            used_mp: 0xFFFFFFFF,
        };
        cache.cache.insert(cache_key, approx_encoded);
        return approx_encoded;
    }

    // スタート地点がいずれかの到達目標に一致する場合
    for &et in &effective_targets {
        if start == et {
            let zero = TurnDistance {
                turns: 0,
                used_mp: 0,
            };
            cache.cache.insert(cache_key, zero);
            return zero;
        }
    }

    // ダイクストラ法でスタートから各タイルへの最短移動コストを計算
    let mut dist = HashMap::new();
    let mut heap = BinaryHeap::new();

    dist.insert(start, 0u32);
    heap.push(State {
        cost: 0,
        position: start,
    });

    while let Some(State { cost, position }) = heap.pop() {
        // いずれかの到達目標に到達した
        if effective_targets.contains(&position) {
            let turns = if max_mp == 0 {
                TurnDistance {
                    turns: u32::MAX,
                    used_mp: u32::MAX,
                }
            } else {
                let base_turns = (cost + max_mp - 1) / max_mp; // ceil(cost / max_mp)
                TurnDistance {
                    turns: base_turns,
                    used_mp: cost,
                }
            };
            cache.cache.insert(cache_key, turns);
            return turns;
        }

        if cost > *dist.get(&position).unwrap_or(&u32::MAX) {
            continue;
        }

        for next_pos in map.get_adjacent(position.0, position.1) {
            let Some(next_terrain) = map.get_terrain(next_pos.0, next_pos.1) else {
                continue;
            };

            let Some(move_cost) = get_valid_movement_cost(registry, movement_type, next_terrain)
            else {
                continue;
            };

            // 敵ユニットによるゾック（通行不可）判定
            // ただし目標地点に敵がいる場合は攻撃可能なので通行可能とみなす
            if !effective_targets.contains(&next_pos) {
                if let Some(occupant) = unit_positions.get(&next_pos) {
                    if occupant.player_id != player_id {
                        continue; // 敵がいるマスは通過不可
                    }
                }
            }

            let next_cost = cost + move_cost;
            let current_best = *dist.get(&next_pos).unwrap_or(&u32::MAX);

            if next_cost < current_best {
                dist.insert(next_pos, next_cost);
                heap.push(State {
                    cost: next_cost,
                    position: next_pos,
                });
            }
        }
    }

    // 完全に到達不可な場合でも、マンハッタン距離ベースの近似値を返す
    // 遠すぎる値にするとスコア計算で差分が出なくなるため、50 + 距離/4 程度に留める
    let dx = (start.0 as i32 - target.0 as i32).abs();
    let dy = (start.1 as i32 - target.1 as i32).abs();
    let approx = 50 + ((dx + dy) as u32 / 4);
    let approx_encoded = TurnDistance {
        turns: approx,
        used_mp: 0xFFFFFFFF,
    };

    cache.cache.insert(cache_key, approx_encoded);
    approx_encoded
}

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
    let cache_key = ActionRangeCacheKey {
        start,
        target,
        movement_type,
        max_mp,
        max_fuel,
        min_range: interaction_min_range,
        max_range: interaction_max_range,
        player_id,
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

    if firing_positions.contains(&start) {
        let result = Some(ActionTurnDistance {
            turns: 0,
            used_mp: 0,
            used_fuel: 0,
            requires_movement: false,
        });
        cache.cache.insert(cache_key, result);
        return result;
    }
    if firing_positions.is_empty() || max_mp == 0 {
        cache.cache.insert(cache_key, None);
        return None;
    }

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
    let mut best_result: Option<ActionTurnDistance> = None;

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

        if firing_positions.contains(&state.position) {
            let firing_distance =
                map.distance(state.position.0, state.position.1, target.0, target.1);
            let indirect_setup = u32::from(state.turns > 0 && firing_distance > 1);
            let candidate = ActionTurnDistance {
                turns: state.turns.saturating_add(indirect_setup),
                used_mp: state.total_mp,
                used_fuel: state.used_fuel,
                requires_movement: true,
            };
            let replace = best_result.is_none_or(|current| {
                (candidate.turns, candidate.used_fuel, candidate.used_mp)
                    < (current.turns, current.used_fuel, current.used_mp)
            });
            if replace {
                best_result = Some(candidate);
            }
        }

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

    cache.cache.insert(cache_key, best_result);
    best_result
}

/// 始点（start）からマップ上のすべての到達可能な座標への最短到達ターン数をダイクストラ法で一括計算し、
/// HashMap として返します。
pub fn calculate_all_turn_distances(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    interaction_max_range: u32,
    player_id: PlayerId,
) -> HashMap<GridPosition, TurnDistance> {
    let mut dist = HashMap::new();
    let mut heap = BinaryHeap::new();
    let mut turns_map = HashMap::new();

    if max_mp == 0 {
        return turns_map;
    }

    // ターゲットからインタラクション可能な（射程内の）全ての進入可能タイルを始点として登録
    for dx in -(interaction_max_range as i32)..=(interaction_max_range as i32) {
        for dy in -(interaction_max_range as i32)..=(interaction_max_range as i32) {
            let m_dist = dx.abs() + dy.abs();
            if m_dist > interaction_max_range as i32 {
                continue;
            }

            let nx = target.0 as i32 + dx;
            let ny = target.1 as i32 + dy;
            if nx >= 0 && nx < map.width as i32 && ny >= 0 && ny < map.height as i32 {
                let pos = (nx as usize, ny as usize);
                if let Some(terrain) = map.get_terrain(pos.0, pos.1) {
                    if get_valid_movement_cost(registry, movement_type, terrain).is_some() {
                        let mut can_enter = true;
                        // 目標地点そのもの以外で、敵がいる場合は始点にできない
                        if pos != target {
                            if let Some(occupant) = unit_positions.get(&pos) {
                                if occupant.player_id != player_id {
                                    can_enter = false;
                                }
                            }
                        }
                        if can_enter {
                            dist.insert(pos, 0);
                            heap.push(State {
                                cost: 0,
                                position: pos,
                            });
                        }
                    }
                }
            }
        }
    }

    while let Some(State { cost, position }) = heap.pop() {
        if cost > *dist.get(&position).unwrap_or(&u32::MAX) {
            continue;
        }

        // 逆方向探索: 順方向では next_pos から position へ移動するため、position の進入コストを加算する
        let Some(pos_terrain) = map.get_terrain(position.0, position.1) else {
            continue;
        };
        let Some(move_cost) = get_valid_movement_cost(registry, movement_type, pos_terrain) else {
            continue;
        };

        for next_pos in map.get_adjacent(position.0, position.1) {
            let Some(next_terrain) = map.get_terrain(next_pos.0, next_pos.1) else {
                continue;
            };

            // 順方向の移動元(next_pos)自体が進入可能かどうかのチェック
            if get_valid_movement_cost(registry, movement_type, next_terrain).is_none() {
                continue;
            }

            // 敵ユニットによるゾック（通行不可）判定
            // 逆方向探索では、next_pos が味方にとって通行可能かを見る（実際には next_pos から出発するため）
            if let Some(occupant) = unit_positions.get(&next_pos) {
                if occupant.player_id != player_id {
                    continue; // 敵がいるマスは通過不可
                }
            }

            let next_cost = cost + move_cost;
            let current_best = *dist.get(&next_pos).unwrap_or(&u32::MAX);

            if next_cost < current_best {
                dist.insert(next_pos, next_cost);
                heap.push(State {
                    cost: next_cost,
                    position: next_pos,
                });
            }
        }
    }

    // コスト (move_cost の合計) を TurnDistance 構造体に変換
    for (pos, cost) in dist {
        let base_turns = (cost + max_mp - 1) / max_mp;
        let encoded_turns = TurnDistance {
            turns: base_turns,
            used_mp: cost,
        };
        turns_map.insert(GridPosition { x: pos.0, y: pos.1 }, encoded_turns);
    }

    turns_map
}

/// AIの意思決定・ビーム探索プロセスにおいて、個別に確保して使い回すためのキャッシュ領域。
/// メモリー肥大化を防ぎ、かつ同一ターン内の探索全体でキャッシュを安全に共有します。
pub struct AiTurnCache {
    /// キー: (始点x, 始点y, 移動タイプ, 移動力, 射程, 勢力ID)
    /// 値: その始点からマップ上の全到達座標への最短到達ターン数テーブル (Arc)
    #[allow(clippy::type_complexity)]
    pub sssp_cache: HashMap<
        (usize, usize, MovementType, u32, u32, PlayerId),
        Arc<HashMap<crate::components::GridPosition, TurnDistance>>,
    >,
}

impl AiTurnCache {
    pub fn new() -> Self {
        Self {
            sssp_cache: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.sssp_cache.clear();
    }
}

impl Default for AiTurnCache {
    fn default() -> Self {
        Self::new()
    }
}

/// キャッシュを利用して、始点から全座標への最短ターン数テーブルを一括取得します。
pub fn calculate_all_turn_distances_cached(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    interaction_max_range: u32,
    player_id: PlayerId,
    cache: &mut AiTurnCache,
) -> Arc<HashMap<crate::components::GridPosition, TurnDistance>> {
    let key = (
        target.0,
        target.1,
        movement_type,
        max_mp,
        interaction_max_range,
        player_id,
    );
    if let Some(cached_map) = cache.sssp_cache.get(&key) {
        return cached_map.clone(); // Arc の clone なので極めて高速
    }

    let result = calculate_all_turn_distances(
        map,
        registry,
        unit_positions,
        target,
        movement_type,
        max_mp,
        interaction_max_range,
        player_id,
    );
    let shared_result = Arc::new(result);
    cache.sssp_cache.insert(key, shared_result.clone());
    shared_result
}

/// エンコードされたターン数（上位16bit: 実ターン数、下位16bit: 消費コスト）から
/// 実ターン数を取り出します。
pub fn decode_turn_distance(encoded: u32) -> u32 {
    if encoded == u32::MAX {
        u32::MAX
    } else {
        encoded >> 16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::Terrain;

    #[test]
    fn test_turn_distance() {
        let map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = TurnDistanceCache::default();
        let unit_positions = HashMap::new();

        // 平地はコスト1。歩兵（移動力3）。
        // 距離4のマスへは、コスト4。ターン数は ceil(4/3) = 2
        let dist = calculate_turn_distance(
            &map,
            &registry,
            &unit_positions,
            (0, 0),
            (4, 0),
            MovementType::Infantry,
            3,
            0,
            PlayerId(1),
            &mut cache,
        );
        assert_eq!(dist.turns, 2);

        // キャッシュヒットの確認
        let dist2 = calculate_turn_distance(
            &map,
            &registry,
            &unit_positions,
            (0, 0),
            (4, 0),
            MovementType::Infantry,
            3,
            0,
            PlayerId(1),
            &mut cache,
        );
        assert_eq!(dist2.turns, 2);
    }

    #[test]
    fn turn_distance_to_range_excludes_minimum_range_dead_zone() {
        let map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = TurnDistanceCache::default();

        let distance = calculate_turn_distance_to_range(
            &map,
            &registry,
            &HashMap::new(),
            (3, 0),
            (4, 0),
            MovementType::Infantry,
            3,
            2,
            3,
            PlayerId(1),
            &mut cache,
        );

        assert_eq!(distance.turns, 1);
        assert_eq!(distance.used_mp, 1);
    }

    #[test]
    fn turn_distance_to_range_accepts_existing_firing_position() {
        let map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = TurnDistanceCache::default();

        let distance = calculate_turn_distance_to_range(
            &map,
            &registry,
            &HashMap::new(),
            (2, 0),
            (4, 0),
            MovementType::Infantry,
            3,
            2,
            3,
            PlayerId(1),
            &mut cache,
        );

        assert_eq!(distance.turns, 0);
        assert_eq!(distance.used_mp, 0);
    }

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
    }

    #[test]
    fn action_distance_counts_fuel_per_traversed_tile() {
        let map = Map::new(
            3,
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
            (2, 0),
            MovementType::Tank,
            5,
            2,
            0,
            0,
            PlayerId(1),
            &mut cache,
        )
        .unwrap();

        assert_eq!(distance.used_mp, 4);
        assert_eq!(distance.used_fuel, 2);
    }

    #[test]
    fn action_distance_adds_setup_turn_for_moved_indirect_attack() {
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
            1,
            99,
            1,
            3,
            PlayerId(1),
            &mut cache,
        )
        .unwrap();

        assert_eq!(distance.turns, 2);
        assert_eq!(distance.used_fuel, 1);
    }

    #[test]
    fn action_distance_rejects_allied_occupied_firing_position() {
        let map = Map::new(
            3,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = ActionTurnDistanceCache::default();
        let unit_positions = HashMap::from([(
            (1, 0),
            OccupantInfo {
                player_id: PlayerId(1),
                unit_type: crate::resources::UnitType::Infantry,
                is_transport: false,
                free_slots: 0,
                loadable_types: Vec::new(),
            },
        )]);

        assert!(
            calculate_action_distance_to_range(
                &map,
                &registry,
                &unit_positions,
                (0, 0),
                (2, 0),
                MovementType::Tank,
                5,
                99,
                1,
                1,
                PlayerId(1),
                &mut cache,
            )
            .is_none()
        );
    }

    #[test]
    fn test_turn_distance_unreachable() {
        let mut map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let _ = map.set_terrain(2, 0, Terrain::Sea); // 海は歩兵進入不可
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = TurnDistanceCache::default();
        let unit_positions = HashMap::new();

        let dist = calculate_turn_distance(
            &map,
            &registry,
            &unit_positions,
            (0, 0),
            (4, 0),
            MovementType::Infantry,
            3,
            0,
            PlayerId(1),
            &mut cache,
        );
        assert_eq!(dist.turns, 51);
    }

    #[test]
    fn terrain_reachability_does_not_use_unreachable_approximation() {
        let mut map = Map::new(
            3,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        map.set_terrain(1, 0, Terrain::Sea).unwrap();
        let registry = MasterDataRegistry::load().unwrap_or_default();

        assert!(!is_terrain_reachable(
            &map,
            &registry,
            (0, 0),
            (2, 0),
            MovementType::Infantry,
        ));
        assert!(is_terrain_reachable(
            &map,
            &registry,
            (0, 0),
            (2, 0),
            MovementType::Air,
        ));
    }
}
