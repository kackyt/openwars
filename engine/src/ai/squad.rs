#![allow(clippy::collapsible_if)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::manual_while_let_some)]
#![allow(clippy::unnecessary_map_or)]

use crate::ai::cluster::detect_enemy_clusters;
use crate::ai::strategy::analyze_strategy;
use crate::ai::turn_distance::{
    TerrainConnectivity, TurnDistanceCache, calculate_all_turn_distances, calculate_turn_distance,
    is_terrain_reachable,
};
use crate::components::{Ammo, Faction, GridPosition, Health, PlayerId, Property, UnitStats};
use crate::resources::{Map, Terrain, UnitType, master_data::MasterDataRegistry};
use crate::systems::movement::calculate_reachable_tiles;
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};

/// ミッションの種別
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionType {
    Attack,
    Capture,
    Defense,
    Transport,
}

/// 輸送ミッションの各フェーズ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportPhase {
    Pickup,
    Transit,
    Drop,
    Return,
}

/// 部隊が実行中のミッションの状態
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionPhase {
    Forming,
    MovingToTarget,
    Executing,
    Completed,
    Transport(TransportPhase),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SquadId(pub u32);

/// 部隊（Squad）の定義
#[derive(Debug, Clone)]
pub struct Squad {
    pub id: SquadId,
    pub members: HashSet<Entity>,
    pub mission_type: MissionType,
    pub target: Option<GridPosition>, // 攻撃・防衛・占領の目標座標
    pub target_island: Option<crate::ai::islands::IslandId>, // 輸送ターゲットの島
    pub phase: MissionPhase,
    /// 輸送部隊の輸送役。HashSet の列挙順ではなく明示的に保持する。
    pub transport_entity: Option<Entity>,
    /// この輸送部隊へ割り当てたカーゴ。占領要員を先頭にした決定的な順序で保持する。
    pub cargo_entities: Vec<Entity>,
    /// Pickup 時の合流位置。
    pub pickup_position: Option<GridPosition>,
    /// Transit/Drop で最後に選択した降車位置。
    pub drop_position: Option<GridPosition>,
    /// 降車済みで通常部隊への引き継ぎを待つカーゴ。
    pub delivered_cargo: Vec<Entity>,
}

/// 全ての部隊を管理するリソース
#[derive(Resource, Default, Debug, Clone)]
pub struct SquadManager {
    pub squads: Vec<Squad>,
    pub solo_fallbacks: HashSet<Entity>,
    next_id: u32,
}

impl SquadManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_squad(&mut self, mission_type: MissionType) -> &mut Squad {
        let squad = Squad {
            id: SquadId(self.next_id),
            members: HashSet::new(),
            mission_type,
            target: None,
            target_island: None,
            phase: MissionPhase::Forming,
            transport_entity: None,
            cargo_entities: Vec::new(),
            pickup_position: None,
            drop_position: None,
            delivered_cargo: Vec::new(),
        };
        self.next_id += 1;
        self.squads.push(squad);
        self.squads.last_mut().unwrap()
    }

    pub fn remove_squad(&mut self, id: SquadId) {
        self.squads.retain(|s| s.id != id);
    }
}

/// #53 (V3): 敵生産施設への奪取部隊の護衛として適格かを判定する。
/// 「歩兵に随伴できる機動力があり、損傷しておらず、弾薬がある」戦闘ユニットのみ。
/// 鈍足ユニット (砲台等)・損傷ユニット・弾切れユニットを護衛に組み込むと、
/// 部隊の足が揃わない/戦力にならず奪取が成立しないため除外する。
#[allow(clippy::too_many_arguments)]
fn select_nearest_compatible_cargo(
    candidates: &[(Entity, GridPosition, UnitStats)],
    transport_position: GridPosition,
    transport_stats: &UnitStats,
    target_island: crate::ai::islands::IslandId,
    target_position: Option<GridPosition>,
    island_map: &crate::ai::islands::IslandMap,
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), crate::systems::movement::OccupantInfo>,
    player_id: PlayerId,
    turn_cache: &mut TurnDistanceCache,
    connectivity: &mut TerrainConnectivity,
    enemy_types: &[UnitType],
    damage_chart: Option<&crate::resources::DamageChart>,
    require_effective_combat: bool,
) -> Option<usize> {
    let mut best = None;
    for (index, (entity, position, stats)) in candidates.iter().enumerate() {
        if !transport_stats
            .loadable_unit_types
            .contains(&stats.unit_type)
        {
            continue;
        }
        let already_reachable = target_position.is_some_and(|target| {
            connectivity.is_reachable(
                map,
                registry,
                (position.x, position.y),
                (target.x, target.y),
                stats.movement_type,
            )
        });
        if already_reachable
            || target_position.is_none()
                && island_map
                    .get_island_at(position)
                    .is_some_and(|island| island.id == target_island)
        {
            continue;
        }
        let max_damage = damage_chart.map_or(0, |chart| {
            enemy_types
                .iter()
                .map(|enemy_type| {
                    chart
                        .get_base_damage(stats.unit_type, *enemy_type)
                        .or_else(|| chart.get_base_damage_secondary(stats.unit_type, *enemy_type))
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0)
        });
        if require_effective_combat
            && !enemy_types.is_empty()
            && damage_chart.is_some()
            && max_damage == 0
        {
            continue;
        }
        let distance = calculate_turn_distance(
            map,
            registry,
            unit_positions,
            (position.x, position.y),
            (transport_position.x, transport_position.y),
            stats.movement_type,
            stats.max_movement,
            0,
            player_id,
            turn_cache,
        );
        let score = (
            std::cmp::Reverse(max_damage),
            distance,
            position.y,
            position.x,
            entity.to_bits(),
        );
        if best.is_none_or(|(_, best_score)| score < best_score) {
            best = Some((index, score));
        }
    }
    best.map(|(index, _)| index)
}

fn select_pickup_position(
    world: &World,
    transport_position: GridPosition,
    transport_stats: &UnitStats,
    cargo_entities: &[Entity],
    connectivity: &mut TerrainConnectivity,
) -> Option<GridPosition> {
    let map = world.resource::<Map>();
    let registry = world.resource::<MasterDataRegistry>();
    let cargo_data: Vec<_> = cargo_entities
        .iter()
        .filter_map(|cargo| {
            Some((
                *world.get::<GridPosition>(*cargo)?,
                world.get::<UnitStats>(*cargo)?.clone(),
            ))
        })
        .collect();
    if cargo_data.len() != cargo_entities.len() {
        return None;
    }

    let mut best = None;
    for y in 0..map.height {
        for x in 0..map.width {
            let Some(terrain) = map.get_terrain(x, y) else {
                continue;
            };
            if crate::systems::movement::get_valid_movement_cost(
                registry,
                transport_stats.movement_type,
                terrain,
            )
            .is_none()
                || transport_stats.movement_type == crate::resources::MovementType::Ship
                    && !matches!(terrain, Terrain::Port | Terrain::Shoal)
                || !connectivity.is_reachable(
                    map,
                    registry,
                    (transport_position.x, transport_position.y),
                    (x, y),
                    transport_stats.movement_type,
                )
            {
                continue;
            }
            // 輸送役がいない合流点をカーゴが先に占有すると輸送役が入れないため除外する。
            if (x, y) != (transport_position.x, transport_position.y)
                && cargo_data
                    .iter()
                    .any(|(position, _)| (position.x, position.y) == (x, y))
            {
                continue;
            }

            let mut max_distance = map.distance(transport_position.x, transport_position.y, x, y);
            let mut total_distance = max_distance;
            let all_cargo_can_board = cargo_data.iter().all(|(position, stats)| {
                let boarding_distance = if terrain == Terrain::Shoal
                    && transport_stats.movement_type == crate::resources::MovementType::Ship
                {
                    map.get_adjacent(x, y)
                        .into_iter()
                        .filter(|adjacent| {
                            map.get_terrain(adjacent.0, adjacent.1)
                                .and_then(|adjacent_terrain| {
                                    crate::systems::movement::get_valid_movement_cost(
                                        registry,
                                        stats.movement_type,
                                        adjacent_terrain,
                                    )
                                })
                                .is_some()
                                && connectivity.is_reachable(
                                    map,
                                    registry,
                                    (position.x, position.y),
                                    *adjacent,
                                    stats.movement_type,
                                )
                        })
                        .map(|adjacent| {
                            map.distance(position.x, position.y, adjacent.0, adjacent.1) + 1
                        })
                        .min()
                } else if connectivity.is_reachable(
                    map,
                    registry,
                    (position.x, position.y),
                    (x, y),
                    stats.movement_type,
                ) {
                    Some(map.distance(position.x, position.y, x, y))
                } else {
                    None
                };
                if let Some(distance) = boarding_distance {
                    max_distance = max_distance.max(distance);
                    total_distance += distance;
                    true
                } else {
                    false
                }
            });
            if !all_cargo_can_board {
                continue;
            }
            let current_rank = if (x, y) == (transport_position.x, transport_position.y) {
                0u8
            } else {
                1u8
            };
            let score = (current_rank, max_distance, total_distance, y, x);
            if best.is_none_or(|(_, best_score)| score < best_score) {
                best = Some((GridPosition { x, y }, score));
            }
        }
    }
    best.map(|(position, _)| position)
}

fn escort_is_eligible(
    stats: &UnitStats,
    infantry_movement: u32,
    hp: u32,
    ammo1: u32,
    ammo2: u32,
) -> bool {
    // 機動力: 歩兵と同等以上 (鈍足ユニットを弾く)
    if stats.max_movement < infantry_movement {
        return false;
    }
    // 損傷: HP < 70 のユニットは前線から抜くべきでない
    if hp < 70 {
        return false;
    }
    // 弾薬: 主武器が弾切れ (副武器も無し) なら戦力にならない
    if stats.max_ammo1 > 0 && ammo1 == 0 && !(stats.max_ammo2 > 0 && ammo2 > 0) {
        return false;
    }
    true
}

/// 毎ターンの部隊の再編成と SoloFallback の判定を行います。
pub fn update_squads(world: &mut World, perspective_player: PlayerId) {
    let mut manager = world.remove_resource::<SquadManager>().unwrap_or_default();

    // 存在しなくなったエンティティの削除
    let mut existing_entities = HashSet::new();
    let mut units_needing_fallback = Vec::new();
    let mut units_recovered = Vec::new();

    let mut query = world.query::<(Entity, &Faction, &Health, Option<&Ammo>)>();
    for (entity, faction, health, ammo_opt) in query.iter(world) {
        if faction.0 == perspective_player {
            existing_entities.insert(entity);

            // SoloFallback の判定 (HP < 60 または 弾薬切れ)
            let mut no_ammo = false;
            if let Some(ammo) = ammo_opt {
                no_ammo = (ammo.max_ammo1 > 0 && ammo.ammo1 == 0)
                    && (ammo.max_ammo2 > 0 && ammo.ammo2 == 0);
            }

            if health.current < 60 || no_ammo {
                units_needing_fallback.push(entity);
            } else if health.current >= 70 && !no_ammo {
                // 回復条件を満たした
                units_recovered.push(entity);
            }
        }
    }

    // 生存エンティティのみ残す
    for squad in &mut manager.squads {
        squad.members.retain(|e| existing_entities.contains(e));
        squad
            .cargo_entities
            .retain(|cargo| existing_entities.contains(cargo));
        squad
            .delivered_cargo
            .retain(|cargo| existing_entities.contains(cargo));
        if squad
            .transport_entity
            .is_some_and(|transport| !existing_entities.contains(&transport))
        {
            squad.transport_entity = None;
        }
    }

    // SoloFallback の更新
    manager
        .solo_fallbacks
        .retain(|e| existing_entities.contains(e));

    for e in units_needing_fallback {
        manager.solo_fallbacks.insert(e);
        // Squad から外す
        for squad in &mut manager.squads {
            squad.members.remove(&e);
        }
    }

    for e in units_recovered {
        manager.solo_fallbacks.remove(&e);
    }

    // 輸送部隊のフェーズ更新と完了判定
    let mut delivered_units = Vec::new();
    let mut i = 0;
    while i < manager.squads.len() {
        if manager.squads[i].mission_type == MissionType::Transport {
            let mut squad = manager.squads[i].clone();
            let should_remove = update_transport_squad_phase(world, &mut squad);
            delivered_units.extend(
                squad
                    .delivered_cargo
                    .drain(..)
                    .map(|cargo| (cargo, squad.target_island, squad.target)),
            );
            if should_remove {
                manager.squads.remove(i);
            } else {
                manager.squads[i] = squad;
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    // 降車したユニットを輸送部隊から解放し、通常の占領・攻撃部隊へ引き渡す。
    for (cargo, target_island, preferred_target) in delivered_units {
        handoff_delivered_cargo(
            world,
            &mut manager,
            perspective_player,
            cargo,
            target_island,
            preferred_target,
        );
    }

    // 占領完了した Capture 部隊の解散
    let mut properties_ownership = HashMap::new();
    {
        let mut q_props = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in q_props.iter(world) {
            properties_ownership.insert(*pos, prop.owner_id);
        }
    }

    manager.squads.retain(|squad| {
        if squad.mission_type == MissionType::Capture {
            if let Some(target_pos) = squad.target {
                if properties_ownership.get(&target_pos) == Some(&Some(perspective_player)) {
                    // 自軍の所有になったので解散
                    return false;
                }
            }
        }
        true
    });

    // 侵攻島の敵・未占領拠点が無くなった通常部隊は島拘束を解除する。
    if let Some(island_map) = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
    {
        let mut active_islands = HashSet::new();
        for (position, owner) in &properties_ownership {
            if *owner != Some(perspective_player)
                && let Some(island) = island_map.get_island_at(position)
            {
                active_islands.insert(island.id);
            }
        }
        let mut enemy_query = world.query::<(&GridPosition, &Faction)>();
        for (position, faction) in enemy_query.iter(world) {
            if faction.0 != perspective_player
                && let Some(island) = island_map.get_island_at(position)
            {
                active_islands.insert(island.id);
            }
        }
        for squad in &mut manager.squads {
            if matches!(
                squad.mission_type,
                MissionType::Attack | MissionType::Defense
            ) && squad
                .target_island
                .is_some_and(|island| !active_islands.contains(&island))
            {
                squad.target_island = None;
            }
        }
    }

    // メンバーが0になった部隊の解散
    manager.squads.retain(|s| {
        !s.members.is_empty()
            || s.phase == MissionPhase::Completed
            || s.phase == MissionPhase::Forming
    });

    world.insert_resource(manager);
}

/// ゲームの戦略状況に基づいて、自動的に部隊の構築と新規メンバーの割り当てを行います。
pub fn plan_squads(world: &mut World, perspective_player: PlayerId) {
    // 1. まず既存部隊のクリーンアップと SoloFallback 判定を実行
    update_squads(world, perspective_player);

    let strategy = analyze_strategy(world, perspective_player);
    let mut manager = world.remove_resource::<SquadManager>().unwrap_or_default();
    let enemy_clusters = detect_enemy_clusters(world, perspective_player);

    // V3 の戦略拡張 (#53: 敵拠点の奪取目標化) を有効にするかどうか
    let is_v3 = world
        .get_resource::<crate::ai::ai_version::PlayerAiSettings>()
        .map(|s| s.get_version(perspective_player).uses_v3_tactics())
        .unwrap_or(false);

    // #53 (V3): メンバーが全滅した占領部隊を解散する。
    // 残しておくと「そのターゲットには部隊が存在する」と誤判定され続け
    // (dedupe に引っかかる)、失敗した奪取目標へ二度と部隊が送られなくなる
    if is_v3 {
        manager
            .squads
            .retain(|s| s.mission_type != MissionType::Capture || !s.members.is_empty());
    }

    let map = world.resource::<Map>().clone();
    let registry = world
        .get_resource::<MasterDataRegistry>()
        .cloned()
        .unwrap_or_default();
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
        .unwrap_or_else(|| crate::ai::islands::IslandMap::analyze(&map));

    // 他の部隊にすでに割り当て済みの全メンバーを特定
    let mut busy_entities = HashSet::new();
    for squad in &manager.squads {
        for &member in &squad.members {
            busy_entities.insert(member);
        }
        busy_entities.extend(squad.cargo_entities.iter().copied());
    }

    // 占有情報（経路探索用）
    let mut unit_positions = HashMap::new();
    let mut q_all_units = world.query::<(
        &Faction,
        &GridPosition,
        &UnitStats,
        Option<&crate::components::Transporting>,
    )>();
    for (faction, pos, stats, transporting) in q_all_units.iter(world) {
        if transporting.is_some() {
            continue;
        }
        unit_positions.insert(
            (pos.x, pos.y),
            crate::systems::movement::OccupantInfo {
                player_id: faction.0,
                is_transport: stats.max_cargo > 0,
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots: stats.max_cargo,
            },
        );
    }

    let mut turn_cache = TurnDistanceCache::default();
    // マップ全体のフラッドフィルを毎回やり直さないよう、地形連結判定を使い回す。
    let mut connectivity = TerrainConnectivity::default();

    // フリーの自軍ユニットを収集
    let mut free_combat_units = Vec::new();
    let mut free_infantry = Vec::new();
    let mut free_transports = Vec::new();

    let mut q_my_units = world.query::<(
        Entity,
        &Faction,
        &GridPosition,
        &UnitStats,
        Option<&crate::components::Transporting>,
        Option<&crate::components::Fuel>,
    )>();
    for (entity, faction, pos, stats, transporting, fuel) in q_my_units.iter(world) {
        if faction.0 == perspective_player
            && !busy_entities.contains(&entity)
            && !manager.solo_fallbacks.contains(&entity)
            && transporting.is_none()
            && fuel.is_none_or(|fuel| fuel.current > 0)
        {
            let is_transport = stats.unit_type == UnitType::TransportHelicopter
                || stats.unit_type == UnitType::Lander;
            let is_infantry =
                stats.unit_type == UnitType::Infantry || stats.unit_type == UnitType::Mech;

            if is_transport {
                free_transports.push((entity, *pos, stats.clone()));
            } else if is_infantry {
                free_infantry.push((entity, *pos, stats.clone()));
            } else {
                free_combat_units.push((entity, *pos, stats.clone()));
            }
        }
    }

    // A. 輸送部隊の割り当て（planner.rs からの統合）
    let mut base_islands = HashSet::new();
    let mut target_islands = Vec::new();
    let mut properties_ownership = HashMap::new();
    {
        let mut q_props = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in q_props.iter(world) {
            properties_ownership.insert(*pos, prop.owner_id);
        }
    }
    let (b_islands, t_islands) =
        island_map.classify_islands(perspective_player, &properties_ownership);
    base_islands.extend(b_islands.iter().copied());
    target_islands.extend(t_islands.iter().copied());

    // ---------------------------------------------------------
    // V1からの統合: 島ごとの期待値（IslandScore）の算出
    // ---------------------------------------------------------
    let mut own_production_bases = Vec::new();
    let mut enemy_production_count_map = std::collections::HashMap::new();
    let mut enemy_owned_islands = HashSet::new();
    let mut island_props_map = std::collections::HashMap::new();

    for (pos, owner) in &properties_ownership {
        if let Some(island) = island_map.get_island_at(pos) {
            island_props_map
                .entry(island.id)
                .or_insert_with(Vec::new)
                .push(*pos);

            let terrain = map
                .get_terrain(pos.x, pos.y)
                .unwrap_or(crate::resources::Terrain::City);
            if *owner == Some(perspective_player) {
                if terrain == crate::resources::Terrain::Factory
                    || terrain == crate::resources::Terrain::Capital
                {
                    own_production_bases.push(*pos);
                }
            } else if owner.is_some() {
                enemy_owned_islands.insert(island.id);
                if terrain == crate::resources::Terrain::Factory
                    || terrain == crate::resources::Terrain::Capital
                {
                    *enemy_production_count_map.entry(island.id).or_insert(0) += 1;
                }
            }
        }
    }

    let mut objectives = Vec::new();
    for &target_id in &target_islands {
        let mut min_distance = i32::MAX;
        let mut props_with_terrain = Vec::new();

        if let Some(positions) = island_props_map.get(&target_id) {
            for tile in positions {
                if properties_ownership.get(tile) == Some(&Some(perspective_player)) {
                    continue; // 自分の拠点はスキップ
                }
                props_with_terrain.push((
                    *tile,
                    map.get_terrain(tile.x, tile.y)
                        .unwrap_or(crate::resources::Terrain::City),
                ));

                let mut local_min_dist = i32::MAX;
                let mut nearest_base_pos = None;
                for base_pos in &own_production_bases {
                    let dist = (tile.x as i32 - base_pos.x as i32).abs()
                        + (tile.y as i32 - base_pos.y as i32).abs();
                    if dist < local_min_dist {
                        local_min_dist = dist;
                        nearest_base_pos = Some(*base_pos);
                    }
                }

                // 徒歩圏内（6マス以下かつ同じ島）は除外
                if nearest_base_pos
                    .and_then(|p| island_map.get_island_at(&p))
                    .filter(|base_island| base_island.id == target_id && local_min_dist <= 6)
                    .is_some()
                {
                    continue;
                }

                if local_min_dist < min_distance {
                    min_distance = local_min_dist;
                }
            }
        }

        let distance_to_nearest_base = if min_distance == i32::MAX {
            10
        } else {
            min_distance
        };
        let enemy_prod = *enemy_production_count_map.get(&target_id).unwrap_or(&0);

        let objective = crate::ai::objectives::Objective::evaluate(
            target_id,
            &props_with_terrain,
            distance_to_nearest_base,
            enemy_prod,
            &registry,
        );
        objectives.push(objective);
    }

    // V3 の敵島侵攻は戦略レイヤーで選択した1島だけを対象とする。
    if is_v3 {
        if let Some(invasion_target) = strategy.invasion_target {
            objectives.retain(|objective| objective.target_island == invasion_target.target_island);
        } else {
            // 敵島侵攻が無い間は、中立島への通常拡張を従来どおり継続する。
            objectives.retain(|objective| !enemy_owned_islands.contains(&objective.target_island));
        }
    }

    // スコア降順ソート（海を渡る必要がある別島を最優先）。
    // 同点時は島IDで決定し、HashSet や ECS の列挙順へ依存しない。
    objectives.sort_by_key(|objective| {
        let is_same_island = base_islands.contains(&objective.target_island);
        let group = if is_same_island { 1u8 } else { 0u8 }; // 別島=0(高優先), 同島=1(低優先)
        (
            group,
            std::cmp::Reverse(objective.priority_score),
            objective.target_island.0,
        )
    });

    // #54 (V3): 自軍の歩兵 (占領要員) が存在する島の集合。
    // 輸送カーゴの選定で「占領要員が未着の島へはまず歩兵を送る」判定に使う
    let mut my_infantry_islands = HashSet::new();
    if is_v3 {
        for ((x, y), occ) in &unit_positions {
            if occ.player_id == perspective_player
                && matches!(occ.unit_type, UnitType::Infantry | UnitType::Mech)
            {
                let pos = GridPosition { x: *x, y: *y };
                if let Some(island) = island_map.get_island_at(&pos) {
                    my_infantry_islands.insert(island.id);
                }
            }
        }
    }

    // 輸送役の選択順を座標と Entity ID で固定する。
    // 逆順にソートし、後続のループで pop() (O(1)) から取り出せるようにする。
    free_transports
        .sort_by_key(|(entity, pos, _)| std::cmp::Reverse((pos.y, pos.x, entity.to_bits())));

    // 優先順位の高い島から輸送機を割り当てる
    for objective in objectives.iter() {
        if free_transports.is_empty() {
            break;
        }

        let mut to_assign = objective.needed_infantry.0;
        let target_position = strategy
            .invasion_target
            .filter(|target| target.target_island == objective.target_island)
            .map(|target| target.target_position)
            .or_else(|| {
                island_props_map
                    .get(&objective.target_island)
                    .and_then(|positions| {
                        let mut positions = positions.clone();
                        positions.sort_by_key(|position| (position.y, position.x));
                        positions.into_iter().find(|position| {
                            properties_ownership.get(position) != Some(&Some(perspective_player))
                        })
                    })
            });
        let mut enemy_types = Vec::new();
        let mut enemy_query = world.query::<(&GridPosition, &Faction, &UnitStats)>();
        for (position, faction, stats) in enemy_query.iter(world) {
            if faction.0 != perspective_player
                && island_map
                    .get_island_at(position)
                    .is_some_and(|island| island.id == objective.target_island)
            {
                enemy_types.push(stats.unit_type);
            }
        }
        enemy_types.sort_by_key(|unit_type| *unit_type as u8);
        enemy_types.dedup();
        let damage_chart = world
            .get_resource::<crate::resources::DamageChart>()
            .cloned();

        while to_assign > 0 && !free_transports.is_empty() {
            let (transport_entity, transport_position, transport_stats) =
                free_transports.pop().unwrap();
            let capacity = world
                .get::<crate::components::CargoCapacity>(transport_entity)
                .map(|cargo| cargo.max as usize)
                .unwrap_or(transport_stats.max_cargo as usize);
            if capacity == 0 {
                continue;
            }

            let mut cargo_entities = world
                .get::<crate::components::CargoCapacity>(transport_entity)
                .map(|cargo| cargo.loaded.clone())
                .unwrap_or_default();
            cargo_entities.sort_by_key(|entity| entity.to_bits());
            cargo_entities.dedup();
            cargo_entities.truncate(capacity);

            let initially_loaded_count = cargo_entities.len();
            let mut assigned_capture_count = cargo_entities
                .iter()
                .filter(|entity| {
                    world
                        .get::<UnitStats>(**entity)
                        .is_some_and(|stats| stats.can_capture)
                })
                .count();
            let mut selected_entries = Vec::new();

            if cargo_entities.len() < capacity {
                // V3 は不足している占領要員を先に補い、残り容量へ有効な戦闘要員を積む。
                // V2 は従来どおり戦闘要員を先に検討する。
                let search_order = if is_v3 && assigned_capture_count == 0 {
                    [false, true]
                } else {
                    [true, false]
                };

                for search_combat in search_order {
                    if cargo_entities.len() >= capacity {
                        break;
                    }
                    let candidates = if search_combat {
                        &free_combat_units
                    } else {
                        &free_infantry
                    };
                    let Some(index) = select_nearest_compatible_cargo(
                        candidates,
                        transport_position,
                        &transport_stats,
                        objective.target_island,
                        target_position,
                        &island_map,
                        &map,
                        &registry,
                        &unit_positions,
                        perspective_player,
                        &mut turn_cache,
                        &mut connectivity,
                        &enemy_types,
                        damage_chart.as_ref(),
                        search_combat,
                    ) else {
                        continue;
                    };
                    let entry = if search_combat {
                        free_combat_units.swap_remove(index)
                    } else {
                        free_infantry.swap_remove(index)
                    };
                    if entry.2.can_capture {
                        assigned_capture_count += 1;
                    }
                    cargo_entities.push(entry.0);
                    selected_entries.push((search_combat, entry));

                    // V2 は単一カーゴ運用を維持し、V3 のみ残り容量へ護衛を追加する。
                    if !is_v3 {
                        break;
                    }
                }

                while is_v3 && cargo_entities.len() < capacity {
                    let Some(index) = select_nearest_compatible_cargo(
                        &free_infantry,
                        transport_position,
                        &transport_stats,
                        objective.target_island,
                        target_position,
                        &island_map,
                        &map,
                        &registry,
                        &unit_positions,
                        perspective_player,
                        &mut turn_cache,
                        &mut connectivity,
                        &enemy_types,
                        damage_chart.as_ref(),
                        false,
                    ) else {
                        break;
                    };
                    let entry = free_infantry.swap_remove(index);
                    if entry.2.can_capture {
                        assigned_capture_count += 1;
                    }
                    cargo_entities.push(entry.0);
                    selected_entries.push((false, entry));
                }
            }

            if cargo_entities.is_empty() {
                continue;
            }
            let requires_pickup = cargo_entities.len() > initially_loaded_count;
            let mut pickup_position = if requires_pickup {
                select_pickup_position(
                    world,
                    transport_position,
                    &transport_stats,
                    &cargo_entities[initially_loaded_count..],
                    &mut connectivity,
                )
            } else {
                Some(transport_position)
            };
            if pickup_position.is_none() {
                // 不成立の組み合わせは free pool へ戻し、搭載済みカーゴだけで継続する。
                for (is_combat, entry) in selected_entries.drain(..) {
                    if entry.2.can_capture {
                        assigned_capture_count = assigned_capture_count.saturating_sub(1);
                    }
                    if is_combat {
                        free_combat_units.push(entry);
                    } else {
                        free_infantry.push(entry);
                    }
                }
                cargo_entities.truncate(initially_loaded_count);
                if cargo_entities.is_empty() {
                    continue;
                }
                pickup_position = Some(transport_position);
            }

            let squad = manager.create_squad(MissionType::Transport);
            squad.members.insert(transport_entity);
            squad.transport_entity = Some(transport_entity);
            squad.cargo_entities = cargo_entities;
            squad.target_island = Some(objective.target_island);
            squad.target = target_position;
            squad.pickup_position = pickup_position;
            squad.phase =
                MissionPhase::Transport(if requires_pickup && !selected_entries.is_empty() {
                    TransportPhase::Pickup
                } else {
                    TransportPhase::Transit
                });

            to_assign = to_assign.saturating_sub(assigned_capture_count);
        }
    }

    // pop() で取り出した分だけ逆順になっているため、元の昇順（座標と Entity ID 順）へ戻す。
    free_transports.reverse();

    // 割り当てられなかった搭載済み輸送機は、全カーゴを追跡して安全に降ろす。
    let mut i = 0;
    while i < free_transports.len() {
        let (transport_entity, transport_position, _) = free_transports[i].clone();
        let mut cargo_entities = world
            .get::<crate::components::CargoCapacity>(transport_entity)
            .map(|cargo| cargo.loaded.clone())
            .unwrap_or_default();
        cargo_entities.sort_by_key(|entity| entity.to_bits());
        cargo_entities.dedup();

        if !cargo_entities.is_empty() {
            let squad = manager.create_squad(MissionType::Transport);
            squad.members.insert(transport_entity);
            squad.transport_entity = Some(transport_entity);
            squad.cargo_entities = cargo_entities;
            squad.pickup_position = Some(transport_position);
            squad.target_island = strategy.invasion_target.map(|target| target.target_island);
            squad.target = strategy
                .invasion_target
                .map(|target| target.target_position);
            squad.phase = MissionPhase::Transport(if squad.target_island.is_some() {
                TransportPhase::Transit
            } else {
                TransportPhase::Drop
            });
            free_transports.remove(i);
        } else {
            i += 1;
        }
    }

    // B. 防衛部隊の立ち上げ（Defenseフェーズ）
    let mut my_capital_pos = None;
    let mut q_props = world.query::<(&GridPosition, &Property)>();
    for (pos, prop) in q_props.iter(world) {
        if prop.terrain == Terrain::Capital && prop.owner_id == Some(perspective_player) {
            my_capital_pos = Some(*pos);
            break;
        }
    }

    if let Some(capital) = my_capital_pos {
        for cluster in &enemy_clusters {
            let turn_dist = calculate_turn_distance(
                &map,
                &registry,
                &unit_positions,
                (capital.x, capital.y),
                (cluster.center.x, cluster.center.y),
                crate::resources::MovementType::Infantry,
                3,
                1,
                perspective_player,
                &mut turn_cache,
            );

            if turn_dist.turns <= 5 {
                // すでにこのクラスターを防衛目標としている部隊があるか
                let exists = manager.squads.iter().any(|s| {
                    s.mission_type == MissionType::Defense && s.target == Some(cluster.center)
                });

                if !exists {
                    let squad = manager.create_squad(MissionType::Defense);
                    squad.target = Some(cluster.center);
                    squad.phase = MissionPhase::Forming;

                    // 最寄りの戦闘ユニットを最大2基割り当てる
                    free_combat_units.sort_by_key(|(_, pos, stats)| {
                        calculate_turn_distance(
                            &map,
                            &registry,
                            &unit_positions,
                            (pos.x, pos.y),
                            (cluster.center.x, cluster.center.y),
                            stats.movement_type,
                            stats.max_movement,
                            1,
                            perspective_player,
                            &mut turn_cache,
                        )
                    });

                    let assign_count = free_combat_units.len().min(3);
                    for _ in 0..assign_count {
                        let (ent, _, _) = free_combat_units.remove(0);
                        squad.members.insert(ent);
                    }
                }
            }
        }
    }

    // C. 占領部隊の立ち上げ（Expansionフェーズ）
    // #53 (V3): 中立拠点に加えて敵所有拠点も奪取目標に含める。
    // 従来は中立拠点のみが対象だったため、中立を取り切ると占領部隊が
    // 編成されなくなり、敵領土の奪取が発生しなかった。
    let mut capture_targets: Vec<GridPosition> =
        strategy.unowned_properties.iter().cloned().collect();
    if is_v3 {
        capture_targets.extend(strategy.enemy_properties.iter().cloned());
    }
    // 最寄りのフリー歩兵から近い順に割り当てる (HashSet 順の非決定性も排除し、
    // 限られた歩兵を近い目標へ優先的に振り分ける)。
    // #53 (V3): 工場・首都などの生産施設は敵の増援源であり、奪取すれば
    // 前線の消耗戦を根本から崩せるため、多少遠くても優先する
    // (前線都市の取り返し合いに全歩兵が吸われる膠着の解消)
    const PRODUCTION_FACILITY_DIST_BONUS: usize = 6;
    // #53 (V3): 敵生産施設への奪取部隊は同時に1個まで (集中スピアヘッド)。
    // 複数同時に編成すると前線から兵力が抜かれすぎて防衛線が崩壊し、
    // 逐次投入の各個撃破で全部隊が溶けるため
    const MAX_CONCURRENT_FACILITY_CAPTURES: usize = 1;
    let is_enemy_facility = |t: &GridPosition| -> bool {
        properties_ownership
            .get(t)
            .is_some_and(|o| o.is_some() && *o != Some(perspective_player))
            && map
                .get_terrain(t.x, t.y)
                .is_some_and(|tr| registry.is_production_facility(tr.as_str()))
    };
    let mut active_facility_captures = manager
        .squads
        .iter()
        .filter(|s| {
            s.mission_type == MissionType::Capture
                && !s.members.is_empty()
                && s.target.as_ref().is_some_and(&is_enemy_facility)
        })
        .count();
    capture_targets.sort_by_key(|t| {
        let d = free_infantry
            .iter()
            .map(|(_, pos, _)| pos.x.abs_diff(t.x) + pos.y.abs_diff(t.y))
            .min()
            .unwrap_or(usize::MAX);
        let facility_bonus = if is_v3
            && map
                .get_terrain(t.x, t.y)
                .is_some_and(|tr| registry.is_production_facility(tr.as_str()))
        {
            PRODUCTION_FACILITY_DIST_BONUS
        } else {
            0
        };
        (d.saturating_sub(facility_bonus), t.x, t.y)
    });

    // #53 (V3): 敵生産施設への突入 (スピアヘッド) は戦力優勢 (Assault フェーズ)
    // のときのみ許可する。拮抗・劣勢時に前線から兵力を抜くと防衛線が崩壊する
    let allow_facility_capture = strategy.phase == crate::ai::strategy::GamePhase::Assault;

    for unowned_pos in &capture_targets {
        // #53 (V3): 敵生産施設への奪取部隊は同時 MAX_CONCURRENT_FACILITY_CAPTURES 個まで
        let target_is_enemy_facility = is_enemy_facility(unowned_pos);
        if target_is_enemy_facility
            && (!allow_facility_capture
                || active_facility_captures >= MAX_CONCURRENT_FACILITY_CAPTURES)
        {
            continue;
        }

        let target_island_opt = island_map.get_island_at(unowned_pos);
        if target_island_opt.is_none() {
            continue;
        }
        let target_island = target_island_opt.unwrap();
        let is_on_base_island = base_islands.contains(&target_island.id);

        // この未占領拠点と同じ島にいるフリーの歩兵を探す
        let inf_on_same_island_idx = free_infantry.iter().position(|(_, pos, _)| {
            island_map
                .get_island_at(pos)
                .map_or(false, |i| i.id == target_island.id)
        });

        // 占領部隊を立ち上げられる条件：
        // 1. その島にフリーの歩兵がすでにいる
        // 2. または、自軍の初期島（Base Island）であり、フリーの歩兵が（どの島でもいいから）存在する
        let can_capture =
            inf_on_same_island_idx.is_some() || (is_on_base_island && !free_infantry.is_empty());

        if can_capture {
            let exists = manager
                .squads
                .iter()
                .any(|s| s.mission_type == MissionType::Capture && s.target == Some(*unowned_pos));

            if !exists {
                let squad = manager.create_squad(MissionType::Capture);
                squad.target = Some(*unowned_pos);
                squad.phase = MissionPhase::Forming;

                // 割り当てる歩兵の選択：
                // 同じ島にいる歩兵がいればそれを優先、いなければ free_infantry の最後の歩兵を割り当てる
                let assigned_inf_idx = if let Some(idx) = inf_on_same_island_idx {
                    idx
                } else {
                    free_infantry.len() - 1
                };

                let (inf_ent, _, inf_stats) = free_infantry.remove(assigned_inf_idx);
                let inf_movement = inf_stats.max_movement;
                squad.members.insert(inf_ent);

                // #53 (V3): 敵生産施設 (工場・首都等) への奪取部隊には護衛の
                // 戦闘ユニットを最大2両随伴させる。敵の生産圏は防御が厚く、
                // 単独の歩兵では到達前に撃破されて奪取が成立しないため。
                // 護衛は「歩兵に随伴できる機動力があり戦闘可能」なユニットに限定する
                // (鈍足の砲台や弾切れ・損傷ユニットを組み込むと部隊が機能しないため)
                if is_v3 && target_is_enemy_facility {
                    // 対象拠点から「遠い順」に並べ替え、末尾 (最も近いユニット) から pop() で
                    // 取り出すことで Vec 先頭削除による O(N) シフトを避ける (#57 レビュー対応)。
                    free_combat_units.sort_by_key(|(_, pos, _)| {
                        std::cmp::Reverse(
                            pos.x.abs_diff(unowned_pos.x) + pos.y.abs_diff(unowned_pos.y),
                        )
                    });
                    let mut assigned = 0;
                    // 護衛条件を満たさず不採用としたユニットは後で free pool に戻す
                    let mut rejected = Vec::new();
                    while assigned < 2 {
                        let Some((ent, pos, stats)) = free_combat_units.pop() else {
                            break;
                        };
                        let hp = world
                            .get::<crate::components::Health>(ent)
                            .map(|h| h.current)
                            .unwrap_or(100);
                        let (ammo1, ammo2) = world
                            .get::<crate::components::Ammo>(ent)
                            .map(|a| (a.ammo1, a.ammo2))
                            .unwrap_or((u32::MAX, u32::MAX));
                        if escort_is_eligible(&stats, inf_movement, hp, ammo1, ammo2) {
                            squad.members.insert(ent);
                            assigned += 1;
                        } else {
                            rejected.push((ent, pos, stats));
                        }
                    }
                    // 護衛に採用しなかった候補は他部隊が利用できるよう free pool へ戻す
                    free_combat_units.append(&mut rejected);
                    active_facility_captures += 1;
                }
            }
        }
    }

    // D. 攻撃部隊の立ち上げ
    for cluster in &enemy_clusters {
        if free_combat_units.is_empty() {
            break;
        }

        let exists = manager
            .squads
            .iter()
            .any(|s| s.mission_type == MissionType::Attack && s.target == Some(cluster.center));

        if !exists {
            let squad = manager.create_squad(MissionType::Attack);
            squad.target = Some(cluster.center);
            squad.phase = MissionPhase::Forming;

            // 最寄りの戦闘ユニットを最大2基割り当てる
            free_combat_units.sort_by_key(|(_, pos, stats)| {
                calculate_turn_distance(
                    &map,
                    &registry,
                    &unit_positions,
                    (pos.x, pos.y),
                    (cluster.center.x, cluster.center.y),
                    stats.movement_type,
                    stats.max_movement,
                    1,
                    perspective_player,
                    &mut turn_cache,
                )
            });

            let assign_count = free_combat_units.len().min(3);
            for _ in 0..assign_count {
                let (ent, _, _) = free_combat_units.remove(0);
                squad.members.insert(ent);
            }
        }
    }

    // ---------------------------------------------------------------
    // 定員管理付きの余剰ユニット割り当てロジック（交通渋滞解消）
    // 1部隊の最大人数。これを超えると後続ユニットが前線で渋滞を起こす
    const MAX_SQUAD_SIZE: usize = 3;

    while !free_combat_units.is_empty() {
        let (ent, pos, stats) = free_combat_units.pop().unwrap();

        // ステップ1: 定員未満の既存 Attack/Defense 部隊のうち、最も近いものを探す
        let mut best_squad_idx = None;
        let mut min_dist = crate::ai::turn_distance::TurnDistance {
            turns: u32::MAX,
            used_mp: u32::MAX,
        };

        for (idx, squad) in manager.squads.iter().enumerate() {
            if (squad.mission_type == MissionType::Attack
                || squad.mission_type == MissionType::Defense)
                && squad.members.len() < MAX_SQUAD_SIZE
            {
                if let Some(target) = squad.target {
                    let dist = calculate_turn_distance(
                        &map,
                        &registry,
                        &unit_positions,
                        (pos.x, pos.y),
                        (target.x, target.y),
                        stats.movement_type,
                        stats.max_movement,
                        1,
                        perspective_player,
                        &mut turn_cache,
                    );
                    if dist < min_dist {
                        min_dist = dist;
                        best_squad_idx = Some(idx);
                    }
                }
            }
        }

        if let Some(idx) = best_squad_idx {
            // 定員未満の部隊に吸収
            manager.squads[idx].members.insert(ent);
        } else {
            // ステップ2: 既存部隊がすべて定員に達している場合、
            // 最も近い敵クラスターを目標とする新規 Attack 部隊（第2波）を新設する
            let mut nearest_cluster_dist = crate::ai::turn_distance::TurnDistance {
                turns: u32::MAX,
                used_mp: u32::MAX,
            };
            let mut nearest_cluster_center = None;

            for cluster in &enemy_clusters {
                let dist = calculate_turn_distance(
                    &map,
                    &registry,
                    &unit_positions,
                    (pos.x, pos.y),
                    (cluster.center.x, cluster.center.y),
                    stats.movement_type,
                    stats.max_movement,
                    1,
                    perspective_player,
                    &mut turn_cache,
                );
                if dist < nearest_cluster_dist {
                    nearest_cluster_dist = dist;
                    nearest_cluster_center = Some(cluster.center);
                }
            }

            let final_target;
            // 敵が15ターン以上遠い場合や存在しない場合は、最寄りの拠点（未占領または敵所有）を目標にする
            if nearest_cluster_dist.turns <= 15 {
                final_target = nearest_cluster_center;
            } else {
                let mut nearest_prop_dist = u32::MAX;
                let mut nearest_prop_pos = None;
                for (p_pos, p_owner) in &properties_ownership {
                    if *p_owner != Some(perspective_player) {
                        // 処理時間を削減するためマンハッタン距離による概算を使用
                        let dist = (pos.x as i32 - p_pos.x as i32).unsigned_abs()
                            + (pos.y as i32 - p_pos.y as i32).unsigned_abs();
                        if dist < nearest_prop_dist {
                            nearest_prop_dist = dist;
                            nearest_prop_pos = Some(*p_pos);
                        }
                    }
                }
                final_target = nearest_prop_pos.or(nearest_cluster_center);
            }

            if let Some(target) = final_target {
                // 新規部隊を立ち上げて1体目のメンバーとして割り当てる
                let new_squad = manager.create_squad(MissionType::Attack);
                new_squad.target = Some(target);
                new_squad.phase = MissionPhase::Forming;
                new_squad.members.insert(ent);
            }
            // 目標がまったく存在しない場合は放置（SoloFallback として機能）
        }
    }

    world.insert_resource(manager);
}

/// 降車済みカーゴを通常の占領・攻撃部隊へ引き渡します。
fn handoff_delivered_cargo(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    cargo: Entity,
    target_island: Option<crate::ai::islands::IslandId>,
    preferred_target: Option<GridPosition>,
) {
    if world
        .get::<crate::components::Transporting>(cargo)
        .is_some()
        || manager
            .squads
            .iter()
            .any(|squad| squad.members.contains(&cargo))
    {
        return;
    }
    let (Some(position), Some(stats)) = (
        world.get::<GridPosition>(cargo).copied(),
        world.get::<UnitStats>(cargo).cloned(),
    ) else {
        return;
    };
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned();

    if stats.can_capture {
        let mut targets = Vec::new();
        let mut query = world.query::<(&GridPosition, &Property)>();
        for (target, property) in query.iter(world) {
            if property.owner_id == Some(player_id) {
                continue;
            }
            if target_island.is_some_and(|island_id| {
                island_map
                    .as_ref()
                    .and_then(|map| map.get_island_at(target))
                    .is_none_or(|island| island.id != island_id)
            }) {
                continue;
            }
            let preferred_rank = if Some(*target) == preferred_target {
                0u8
            } else {
                1u8
            };
            targets.push((
                preferred_rank,
                position.x.abs_diff(target.x) + position.y.abs_diff(target.y),
                target.y,
                target.x,
                *target,
            ));
        }
        targets.sort_by_key(|target| (target.0, target.1, target.2, target.3));
        if let Some((_, _, _, _, target)) = targets.first().copied() {
            let squad = manager.create_squad(MissionType::Capture);
            squad.members.insert(cargo);
            squad.target = Some(target);
            squad.target_island = target_island;
            squad.phase = MissionPhase::MovingToTarget;
            return;
        }
    }

    let mut enemy_targets = Vec::new();
    let mut query = world.query::<(&GridPosition, &Faction)>();
    for (target, faction) in query.iter(world) {
        if faction.0 == player_id {
            continue;
        }
        if target_island.is_some_and(|island_id| {
            island_map
                .as_ref()
                .and_then(|map| map.get_island_at(target))
                .is_none_or(|island| island.id != island_id)
        }) {
            continue;
        }
        enemy_targets.push((
            position.x.abs_diff(target.x) + position.y.abs_diff(target.y),
            target.y,
            target.x,
            *target,
        ));
    }
    enemy_targets.sort_by_key(|target| (target.0, target.1, target.2));
    if let Some(target) = enemy_targets
        .first()
        .map(|target| target.3)
        .or(preferred_target)
    {
        let squad = manager.create_squad(MissionType::Attack);
        squad.members.insert(cargo);
        squad.target = Some(target);
        squad.target_island = target_island;
        squad.phase = MissionPhase::MovingToTarget;
    }
}

// ==========================================
// 輸送部隊用のフェーズ更新 & 実行ロジック
// ==========================================

fn get_target_position_for_island(
    map: &Map,
    registry: &MasterDataRegistry,
    island: &crate::ai::islands::Island,
    t_pos: GridPosition,
    movement_type: crate::resources::MovementType,
) -> Option<GridPosition> {
    if movement_type == crate::resources::MovementType::Ship {
        island
            .tiles
            .iter()
            .filter(|tile| {
                matches!(
                    map.get_terrain(tile.x, tile.y),
                    Some(Terrain::Port | Terrain::Shoal)
                ) && crate::systems::movement::get_valid_movement_cost(
                    registry,
                    movement_type,
                    map.get_terrain(tile.x, tile.y).unwrap(),
                )
                .is_some()
            })
            .min_by_key(|tile| {
                (
                    t_pos.x.abs_diff(tile.x) + t_pos.y.abs_diff(tile.y),
                    tile.y,
                    tile.x,
                )
            })
            .copied()
    } else {
        island
            .tiles
            .iter()
            .min_by_key(|p| {
                (
                    (p.x as i32 - t_pos.x as i32).abs() + (p.y as i32 - t_pos.y as i32).abs(),
                    p.x,
                    p.y,
                )
            })
            .cloned()
    }
}

/// 現在ターンに到達可能な輸送位置から、合法・到達可能・低脅威な降車位置を選びます。
fn select_landing_candidate(
    world: &mut World,
    transport_entity: Entity,
    cargo_entity: Entity,
    transport_position: GridPosition,
    reachable: &HashSet<(usize, usize)>,
    target_island: Option<crate::ai::islands::IslandId>,
    target_position: Option<GridPosition>,
) -> Option<(GridPosition, GridPosition)> {
    let cargo_stats = world.get::<UnitStats>(cargo_entity).cloned()?;
    let cargo_health = world
        .get::<Health>(cargo_entity)
        .map(|health| health.current)
        .unwrap_or(100);
    let cargo_faction = world.get::<Faction>(cargo_entity)?.0;
    let map = world.resource::<Map>().clone();
    let registry = world.resource::<MasterDataRegistry>().clone();
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned();
    let damage_chart = world
        .get_resource::<crate::resources::DamageChart>()
        .cloned();

    let mut enemy_threats = Vec::new();
    let mut enemy_query = world.query::<(&GridPosition, &Faction, &UnitStats, Option<&Health>)>();
    for (position, faction, stats, health) in enemy_query.iter(world) {
        if faction.0 == cargo_faction {
            continue;
        }
        enemy_threats.push((
            *position,
            stats.unit_type,
            stats.cost,
            health.map(|value| value.current).unwrap_or(100),
            stats.min_range,
            stats.max_range,
            stats.max_movement,
        ));
    }

    let mut reachable_positions: Vec<_> = reachable.iter().copied().collect();
    reachable_positions.sort_by_key(|position| (position.1, position.0));
    let mut best = None;
    let empty_occupants = HashMap::new();
    let target_distances = target_position.map(|target| {
        calculate_all_turn_distances(
            &map,
            &registry,
            &empty_occupants,
            (target.x, target.y),
            cargo_stats.movement_type,
            cargo_stats.max_movement,
            0,
            cargo_faction,
        )
    });

    for (transport_x, transport_y) in reachable_positions {
        let candidate_transport = GridPosition {
            x: transport_x,
            y: transport_y,
        };
        let mut drop_tiles = crate::systems::transport::get_droppable_tiles_at(
            world,
            transport_entity,
            cargo_entity,
            candidate_transport,
        );
        drop_tiles.sort_by_key(|position| (position.1, position.0));

        for (drop_x, drop_y) in drop_tiles {
            let drop_position = GridPosition {
                x: drop_x,
                y: drop_y,
            };
            if target_island.is_some_and(|island_id| {
                island_map
                    .as_ref()
                    .and_then(|map| map.get_island_at(&drop_position))
                    .is_none_or(|island| island.id != island_id)
            }) {
                continue;
            }
            let turns = if let Some(distances) = &target_distances {
                let Some(distance) = distances.get(&drop_position) else {
                    continue;
                };
                distance.turns
            } else {
                0
            };

            let terrain_defense = map
                .get_terrain(drop_position.x, drop_position.y)
                .map(|terrain| registry.get_terrain_defense_bonus(terrain))
                .unwrap_or(0);
            let danger = damage_chart.as_ref().map_or(0, |chart| {
                crate::ai::threat::indirect_fire_expected_loss(
                    &map,
                    (drop_position.x, drop_position.y),
                    cargo_stats.unit_type,
                    cargo_stats.cost,
                    cargo_health,
                    terrain_defense,
                    &enemy_threats,
                    chart,
                )
            });
            let transport_distance = map.distance(
                transport_position.x,
                transport_position.y,
                transport_x,
                transport_y,
            ) as usize;
            let score = (
                danger,
                turns,
                transport_distance,
                drop_y,
                drop_x,
                transport_y,
                transport_x,
            );
            if best.is_none_or(|(_, _, best_score)| score < best_score) {
                best = Some((candidate_transport, drop_position, score));
            }
        }
    }

    best.map(|(transport, drop, _)| (transport, drop))
}

/// 輸送部隊のフェーズ更新と完了判定
pub fn update_transport_squad_phase(world: &mut World, squad: &mut Squad) -> bool {
    let Some(transport_entity) = squad.transport_entity else {
        return true;
    };
    if world.get::<GridPosition>(transport_entity).is_none() {
        return true;
    }
    let phase = match squad.phase {
        MissionPhase::Transport(phase) => phase,
        _ => return false,
    };

    // 実際の CargoCapacity を正とし、計画から漏れた搭載済みカーゴも追跡へ戻す。
    let mut loaded = world
        .get::<crate::components::CargoCapacity>(transport_entity)
        .map(|capacity| capacity.loaded.clone())
        .unwrap_or_default();
    loaded.sort_by_key(|entity| entity.to_bits());
    loaded.dedup();
    for cargo in &loaded {
        if !squad.cargo_entities.contains(cargo) {
            squad.cargo_entities.push(*cargo);
        }
    }

    let mut remaining = Vec::new();
    for cargo in squad.cargo_entities.drain(..) {
        let is_loaded = loaded.contains(&cargo)
            || world
                .get::<crate::components::Transporting>(cargo)
                .is_some_and(|transporting| transporting.0 == transport_entity);
        if is_loaded
            || phase == TransportPhase::Pickup && world.get::<GridPosition>(cargo).is_some()
        {
            remaining.push(cargo);
        } else if matches!(phase, TransportPhase::Transit | TransportPhase::Drop)
            && world.get::<GridPosition>(cargo).is_some()
        {
            squad.delivered_cargo.push(cargo);
        }
    }
    squad.cargo_entities = remaining;

    match phase {
        TransportPhase::Pickup => {
            if squad.cargo_entities.is_empty() {
                return true;
            }
            let all_loaded = squad.cargo_entities.iter().all(|cargo| {
                loaded.contains(cargo)
                    && world
                        .get::<crate::components::Transporting>(*cargo)
                        .is_some_and(|transporting| transporting.0 == transport_entity)
            });
            if all_loaded {
                squad.phase = MissionPhase::Transport(TransportPhase::Transit);
            }
        }
        TransportPhase::Transit => {
            if loaded.is_empty() {
                squad.phase = MissionPhase::Transport(TransportPhase::Return);
            } else if let Some(cargo) = loaded.first().copied()
                && !crate::systems::transport::get_droppable_tiles(world, transport_entity, cargo)
                    .is_empty()
            {
                squad.phase = MissionPhase::Transport(TransportPhase::Drop);
            }
        }
        TransportPhase::Drop => {
            if loaded.is_empty() {
                squad.phase = MissionPhase::Transport(TransportPhase::Return);
            }
        }
        TransportPhase::Return => {
            if !loaded.is_empty() {
                squad.phase = MissionPhase::Transport(TransportPhase::Drop);
                return false;
            }
            if let Some(t_pos) = world.get::<GridPosition>(transport_entity).copied()
                && let Some(t_faction) = world
                    .get::<Faction>(transport_entity)
                    .map(|faction| faction.0)
            {
                let mut query = world.query::<(&GridPosition, &Property)>();
                let at_base = query.iter(world).any(|(position, property)| {
                    *position == t_pos && property.owner_id == Some(t_faction)
                });
                if at_base {
                    return true;
                }
            }
        }
    }

    false
}

/// 輸送部隊の実行ステップ意思決定
pub fn execute_transport_squad_step(
    world: &mut World,
    squad: &mut Squad,
    skip_entities: &std::collections::HashSet<Entity>,
) -> Option<(Entity, crate::ai::engine::AiCommand)> {
    let transport_entity = squad.transport_entity?;

    let (t_pos, t_stats, t_fuel, t_faction) = {
        let t_pos = world.get::<GridPosition>(transport_entity).cloned()?;
        let t_stats = world.get::<UnitStats>(transport_entity).cloned()?;
        let t_fuel = world
            .get::<crate::components::Fuel>(transport_entity)
            .map(|f| f.current)?;
        let t_faction = world.get::<Faction>(transport_entity).cloned()?;
        (t_pos, t_stats, t_fuel, t_faction.0)
    };

    let phase = match squad.phase {
        MissionPhase::Transport(p) => p,
        _ => return None,
    };
    let loaded_cargo = world
        .get::<crate::components::CargoCapacity>(transport_entity)
        .map(|capacity| capacity.loaded.clone())
        .unwrap_or_default();
    let cargo_entity = match phase {
        TransportPhase::Pickup => squad.cargo_entities.iter().copied().find(|cargo| {
            !loaded_cargo.contains(cargo)
                && world
                    .get::<crate::components::Transporting>(*cargo)
                    .is_none()
        }),
        TransportPhase::Transit | TransportPhase::Drop => squad
            .cargo_entities
            .iter()
            .copied()
            .find(|cargo| loaded_cargo.contains(cargo)),
        TransportPhase::Return => None,
    };

    let mut unit_positions = HashMap::new();
    let mut query = world.query::<(
        Entity,
        &GridPosition,
        &Faction,
        &UnitStats,
        Option<&crate::components::CargoCapacity>,
        Option<&crate::components::Transporting>,
    )>();
    for (_e, pos, faction, stats, cargo_opt, transporting_opt) in query.iter(world) {
        if transporting_opt.is_some() {
            continue;
        }
        let free_slots = cargo_opt
            .map(|c| c.max.saturating_sub(c.loaded.len() as u32))
            .unwrap_or(0);
        unit_positions.insert(
            (pos.x, pos.y),
            crate::systems::movement::OccupantInfo {
                player_id: faction.0,
                is_transport: stats.max_cargo > 0,
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots,
            },
        );
    }

    let reachable = {
        let map = world.resource::<Map>();
        let registry = world.resource::<MasterDataRegistry>();
        calculate_reachable_tiles(
            map,
            &unit_positions,
            (t_pos.x, t_pos.y),
            t_stats.movement_type,
            t_stats.max_movement,
            t_fuel,
            t_faction,
            t_stats.unit_type,
            registry,
        )
    };

    match phase {
        TransportPhase::Pickup => {
            let cargo_entity = cargo_entity?;
            let cargo_pos = world.get::<GridPosition>(cargo_entity).cloned()?;
            let pickup_position = squad.pickup_position.unwrap_or(t_pos);
            let dist = (t_pos.x as i32 - cargo_pos.x as i32).abs()
                + (t_pos.y as i32 - cargo_pos.y as i32).abs();

            let transport_moved = world
                .get::<crate::components::HasMoved>(transport_entity)
                .map_or(true, |moved| moved.0);
            let transport_action_completed = world
                .get::<crate::components::ActionCompleted>(transport_entity)
                .map_or(true, |action| action.0);
            let cargo_action_completed = world
                .get::<crate::components::ActionCompleted>(cargo_entity)
                .map_or(true, |action| action.0);
            if dist == 0 && !transport_action_completed && !cargo_action_completed {
                return Some((
                    cargo_entity,
                    crate::ai::engine::AiCommand::Load {
                        transport_entity,
                        target_pos: t_pos,
                    },
                ));
            }

            // 1. 輸送機がまだ行動していないなら、輸送機を歩兵へ近づける

            if !transport_moved
                && !transport_action_completed
                && !skip_entities.contains(&transport_entity)
            {
                let mut best_tile = t_pos;
                let mut min_score = None;

                let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
                let map = world.resource::<Map>();
                let registry = world.resource::<MasterDataRegistry>();

                for target_tile in &reachable {
                    let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                        map,
                        registry,
                        &unit_positions,
                        (target_tile.0, target_tile.1),
                        (pickup_position.x, pickup_position.y),
                        t_stats.movement_type,
                        t_stats.max_movement,
                        0, // 指定した合流地点を目指す
                        t_faction,
                        &mut cache,
                    );

                    let dx = target_tile.0 as i32 - pickup_position.x as i32;
                    let dy = target_tile.1 as i32 - pickup_position.y as i32;
                    let m_dist = dx.abs() + dy.abs();
                    let score = (t_dist, m_dist, target_tile.0, target_tile.1);

                    if min_score.map_or(true, |m| score < m) {
                        min_score = Some(score);
                        best_tile = GridPosition {
                            x: target_tile.0,
                            y: target_tile.1,
                        };
                    }
                }
                return Some((
                    transport_entity,
                    crate::ai::engine::AiCommand::Wait {
                        target_pos: best_tile,
                    },
                ));
            }

            // 2. 輸送機がすでに行動済みなら、歩兵を輸送機へ近づける
            let cargo_moved = world
                .get::<crate::components::HasMoved>(cargo_entity)
                .map_or(true, |moved| moved.0);

            if !cargo_moved && !cargo_action_completed && !skip_entities.contains(&cargo_entity) {
                let c_stats = world.get::<UnitStats>(cargo_entity).cloned()?;
                let c_fuel = world
                    .get::<crate::components::Fuel>(cargo_entity)
                    .map(|f| f.current)
                    .unwrap_or(99);

                let cargo_reachable = {
                    let map = world.resource::<Map>();
                    let registry = world.resource::<MasterDataRegistry>();
                    crate::systems::movement::calculate_reachable_tiles(
                        map,
                        &unit_positions,
                        (cargo_pos.x, cargo_pos.y),
                        c_stats.movement_type,
                        c_stats.max_movement,
                        c_fuel,
                        t_faction,
                        c_stats.unit_type,
                        registry,
                    )
                };

                let mut best_tile = cargo_pos;
                let mut min_score = None;

                let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
                let map = world.resource::<Map>();
                let registry = world.resource::<MasterDataRegistry>();
                let cargo_goal = if t_pos == pickup_position {
                    pickup_position
                } else {
                    map.get_adjacent(pickup_position.x, pickup_position.y)
                        .into_iter()
                        .filter(|position| {
                            map.get_terrain(position.0, position.1)
                                .and_then(|terrain| {
                                    crate::systems::movement::get_valid_movement_cost(
                                        registry,
                                        c_stats.movement_type,
                                        terrain,
                                    )
                                })
                                .is_some()
                                && is_terrain_reachable(
                                    map,
                                    registry,
                                    (cargo_pos.x, cargo_pos.y),
                                    *position,
                                    c_stats.movement_type,
                                )
                        })
                        .min_by_key(|position| {
                            (
                                map.distance(cargo_pos.x, cargo_pos.y, position.0, position.1),
                                position.1,
                                position.0,
                            )
                        })
                        .map(|position| GridPosition {
                            x: position.0,
                            y: position.1,
                        })
                        .unwrap_or(cargo_pos)
                };

                for target_tile in &cargo_reachable {
                    let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                        map,
                        registry,
                        &unit_positions,
                        (target_tile.0, target_tile.1),
                        (cargo_goal.x, cargo_goal.y),
                        c_stats.movement_type,
                        c_stats.max_movement,
                        0, // 輸送役到着前は合流点の隣接マスで待機する
                        t_faction,
                        &mut cache,
                    );

                    let dx = target_tile.0 as i32 - cargo_goal.x as i32;
                    let dy = target_tile.1 as i32 - cargo_goal.y as i32;
                    let m_dist = dx.abs() + dy.abs();
                    let score = (t_dist, m_dist, target_tile.0, target_tile.1);

                    if min_score.map_or(true, |m| score < m) {
                        min_score = Some(score);
                        best_tile = GridPosition {
                            x: target_tile.0,
                            y: target_tile.1,
                        };
                    }
                }

                return Some((
                    cargo_entity,
                    crate::ai::engine::AiCommand::Wait {
                        target_pos: best_tile,
                    },
                ));
            }
        }
        TransportPhase::Transit => {
            let cargo_entity = cargo_entity?;
            if skip_entities.contains(&transport_entity)
                || world
                    .get::<crate::components::ActionCompleted>(transport_entity)
                    .is_some_and(|action| action.0)
            {
                return None;
            }
            if let Some(target_island_id) = squad.target_island {
                let (island_tiles, target_pos) = {
                    if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>()
                    {
                        if let Some(island) =
                            island_map.islands.iter().find(|i| i.id == target_island_id)
                        {
                            let map = world.resource::<Map>();
                            let registry = world.resource::<MasterDataRegistry>();
                            let target_pos = get_target_position_for_island(
                                map,
                                registry,
                                island,
                                t_pos,
                                t_stats.movement_type,
                            );
                            (Some(island.tiles.clone()), target_pos)
                        } else {
                            (None, None)
                        }
                    } else {
                        (None, None)
                    }
                };

                if let (Some(_), Some(target_pos)) = (island_tiles, target_pos) {
                    if let Some((transport_target, drop_target)) = select_landing_candidate(
                        world,
                        transport_entity,
                        cargo_entity,
                        t_pos,
                        &reachable,
                        squad.target_island,
                        squad.target,
                    ) {
                        squad.drop_position = Some(drop_target);
                        squad.phase = MissionPhase::Transport(TransportPhase::Drop);
                        return Some((
                            transport_entity,
                            crate::ai::engine::AiCommand::Drop {
                                transport_target_pos: transport_target,
                                cargo_drop_pos: drop_target,
                                cargo_entity,
                            },
                        ));
                    }

                    let mut best_tile = t_pos;
                    let mut min_score = None;

                    let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
                    let map = world.resource::<Map>();
                    let registry = world.resource::<MasterDataRegistry>();

                    for target_tile in &reachable {
                        let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                            map,
                            registry,
                            &unit_positions,
                            (target_tile.0, target_tile.1),
                            (target_pos.x, target_pos.y),
                            t_stats.movement_type,
                            t_stats.max_movement,
                            1, // 目標に隣接するマスを目指す
                            t_faction,
                            &mut cache,
                        );

                        let dx = target_tile.0 as i32 - target_pos.x as i32;
                        let dy = target_tile.1 as i32 - target_pos.y as i32;
                        let m_dist = dx.abs() + dy.abs();
                        let score = (t_dist, m_dist, target_tile.0, target_tile.1);

                        if min_score.map_or(true, |m| score < m) {
                            min_score = Some(score);
                            best_tile = GridPosition {
                                x: target_tile.0,
                                y: target_tile.1,
                            };
                        }
                    }

                    return Some((
                        transport_entity,
                        crate::ai::engine::AiCommand::Wait {
                            target_pos: best_tile,
                        },
                    ));
                }
            }
        }
        TransportPhase::Drop => {
            let cargo_entity = cargo_entity?;
            if skip_entities.contains(&transport_entity)
                || world
                    .get::<crate::components::ActionCompleted>(transport_entity)
                    .is_some_and(|action| action.0)
            {
                return None;
            }
            if let Some((transport_target, drop_target)) = select_landing_candidate(
                world,
                transport_entity,
                cargo_entity,
                t_pos,
                &reachable,
                squad.target_island,
                squad.target,
            ) {
                squad.drop_position = Some(drop_target);
                return Some((
                    transport_entity,
                    crate::ai::engine::AiCommand::Drop {
                        transport_target_pos: transport_target,
                        cargo_drop_pos: drop_target,
                        cargo_entity,
                    },
                ));
            }

            if let Some(target_island_id) = squad.target_island {
                if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() {
                    if let Some(island) =
                        island_map.islands.iter().find(|i| i.id == target_island_id)
                    {
                        let map = world.resource::<Map>();
                        let registry = world.resource::<MasterDataRegistry>();
                        if let Some(target_pos) = get_target_position_for_island(
                            map,
                            registry,
                            island,
                            t_pos,
                            t_stats.movement_type,
                        ) {
                            let mut best_tile = t_pos;
                            let mut min_score = None;
                            let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
                            let map = world.resource::<Map>();
                            let registry = world.resource::<MasterDataRegistry>();

                            for target_tile in &reachable {
                                let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                                    map,
                                    registry,
                                    &unit_positions,
                                    (target_tile.0, target_tile.1),
                                    (target_pos.x, target_pos.y),
                                    t_stats.movement_type,
                                    t_stats.max_movement,
                                    1, // 目標に隣接するマスを目指す
                                    t_faction,
                                    &mut cache,
                                );
                                let m_dist = (target_tile.0 as i32 - target_pos.x as i32).abs()
                                    + (target_tile.1 as i32 - target_pos.y as i32).abs();
                                let score = (t_dist, m_dist, target_tile.0, target_tile.1);

                                if min_score.map_or(true, |m| score < m) {
                                    min_score = Some(score);
                                    best_tile = GridPosition {
                                        x: target_tile.0,
                                        y: target_tile.1,
                                    };
                                }
                            }
                            return Some((
                                transport_entity,
                                crate::ai::engine::AiCommand::Wait {
                                    target_pos: best_tile,
                                },
                            ));
                        }
                    }
                }
            }
            return Some((
                transport_entity,
                crate::ai::engine::AiCommand::Wait { target_pos: t_pos },
            ));
        }
        TransportPhase::Return => {
            let map = world.resource::<Map>().clone();
            let registry = world.resource::<MasterDataRegistry>().clone();
            let mut return_targets = Vec::new();
            let mut query = world.query::<(&GridPosition, &Property)>();
            for (position, property) in query.iter(world) {
                if property.owner_id != Some(t_faction)
                    || crate::systems::movement::get_valid_movement_cost(
                        &registry,
                        t_stats.movement_type,
                        property.terrain,
                    )
                    .is_none()
                    || t_stats.movement_type == crate::resources::MovementType::Ship
                        && property.terrain != Terrain::Port
                    || !is_terrain_reachable(
                        &map,
                        &registry,
                        (t_pos.x, t_pos.y),
                        (position.x, position.y),
                        t_stats.movement_type,
                    )
                {
                    continue;
                }
                return_targets.push((
                    t_pos.x.abs_diff(position.x) + t_pos.y.abs_diff(position.y),
                    position.y,
                    position.x,
                    *position,
                ));
            }
            return_targets.sort_by_key(|target| (target.0, target.1, target.2));
            let Some(nearest_prop_pos) = return_targets.first().map(|target| target.3) else {
                return Some((
                    transport_entity,
                    crate::ai::engine::AiCommand::Wait { target_pos: t_pos },
                ));
            };

            let mut best_tile = t_pos;
            let mut min_turn_dist = 999.0;
            let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
            let map = world.resource::<Map>();
            let registry = world.resource::<MasterDataRegistry>();

            for target_tile in &reachable {
                let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                    map,
                    registry,
                    &unit_positions,
                    (target_tile.0, target_tile.1),
                    (nearest_prop_pos.x, nearest_prop_pos.y),
                    t_stats.movement_type,
                    t_stats.max_movement,
                    1,
                    t_faction,
                    &mut cache,
                );
                let dx = target_tile.0 as i32 - nearest_prop_pos.x as i32;
                let dy = target_tile.1 as i32 - nearest_prop_pos.y as i32;
                let m_dist = dx.abs() + dy.abs();
                let e_dist_sq = dx * dx + dy * dy;
                let score = t_dist.turns as f32
                    + (m_dist as f32 / 1_000.0)
                    + (e_dist_sq as f32 / 10_000_000.0)
                    + (target_tile.0 as f32 / 100_000_000.0)
                    + (target_tile.1 as f32 / 1_000_000_000.0);

                if score < min_turn_dist {
                    min_turn_dist = score;
                    best_tile = GridPosition {
                        x: target_tile.0,
                        y: target_tile.1,
                    };
                }
            }
            return Some((
                transport_entity,
                crate::ai::engine::AiCommand::Wait {
                    target_pos: best_tile,
                },
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::*;
    use crate::resources::master_data::*;
    use crate::resources::*;

    fn setup_test_world() -> World {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }

        let map = Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: GridTopology::Square,
        };

        // Setup a small map
        world.insert_resource(map.clone());
        world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
        world.insert_resource(SquadManager::new());
        world
    }

    #[test]
    fn test_update_squads_solo_fallback() {
        let mut world = setup_test_world();
        let p1 = PlayerId(1);

        // Spawn a unit with low HP
        let unit1 = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 5, y: 5 },
                Health {
                    current: 50,
                    max: 100,
                },
            ))
            .id();

        // Spawn a healthy unit
        let unit2 = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 6, y: 5 },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        update_squads(&mut world, p1);

        let manager = world.get_resource::<SquadManager>().unwrap();
        assert!(
            manager.solo_fallbacks.contains(&unit1),
            "Unit with 50 HP should be in fallback"
        );
        assert!(
            !manager.solo_fallbacks.contains(&unit2),
            "Healthy unit should not be in fallback"
        );

        // Now heal the unit
        if let Some(mut h) = world.get_mut::<Health>(unit1) {
            h.current = 100;
        }

        update_squads(&mut world, p1);

        let manager = world.get_resource::<SquadManager>().unwrap();
        assert!(
            !manager.solo_fallbacks.contains(&unit1),
            "Unit should have recovered and left fallback"
        );
    }

    #[test]
    fn test_plan_squads_attack_creation() {
        let mut world = setup_test_world();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // Enemy cluster at (8,8)
        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 8, y: 8 },
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                max_movement: 6,
                cost: 7000,
                ..UnitStats::mock()
            },
        ));

        // Friendly units to form an attack squad
        let u1 = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 2, y: 2 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    cost: 7000,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        let u2 = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 2, y: 3 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    cost: 7000,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        plan_squads(&mut world, p1);

        let manager = world.get_resource::<SquadManager>().unwrap();

        // Verify Attack squad created
        let attack_squads: Vec<_> = manager
            .squads
            .iter()
            .filter(|s| s.mission_type == MissionType::Attack)
            .collect();
        assert_eq!(
            attack_squads.len(),
            1,
            "Should create exactly 1 attack squad"
        );
        assert!(attack_squads[0].members.contains(&u1));
        assert!(attack_squads[0].members.contains(&u2));
        assert_eq!(attack_squads[0].target, Some(GridPosition { x: 8, y: 8 }));
    }

    #[test]
    fn test_plan_squads_capture() {
        let mut world = setup_test_world();
        let p1 = PlayerId(1);

        // Friendly base so it doesn't think it's off-island
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::Factory, Some(p1), 100),
        ));

        // Unowned city to capture
        world.spawn((
            GridPosition { x: 5, y: 5 },
            Property::new(Terrain::City, None, 100),
        ));

        // Friendly infantry
        let inf = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 4, y: 4 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        plan_squads(&mut world, p1);

        let manager = world.get_resource::<SquadManager>().unwrap();

        // Verify Capture squad created
        let capture_squads: Vec<_> = manager
            .squads
            .iter()
            .filter(|s| s.mission_type == MissionType::Capture)
            .collect();
        assert_eq!(capture_squads.len(), 1, "Should create 1 capture squad");
        assert!(capture_squads[0].members.contains(&inf));
        assert_eq!(capture_squads[0].target, Some(GridPosition { x: 5, y: 5 }));
    }

    /// Issue #53: V3 では中立拠点が無くても敵所有拠点を目標とする占領部隊が
    /// 編成されること、V2 では従来通り編成されないことを検証する
    #[test]
    fn test_v3_capture_squad_targets_enemy_property() {
        let run = |version: crate::ai::ai_version::AiVersion| -> Option<GridPosition> {
            let mut world = setup_test_world();
            let p1 = PlayerId(1);
            let p2 = PlayerId(2);

            let mut settings = crate::ai::ai_version::PlayerAiSettings::new();
            settings.set_version(p1, version);
            settings.set_version(p2, version);
            world.insert_resource(settings);

            // 自軍の工場 (base island 判定用)
            world.spawn((
                GridPosition { x: 1, y: 1 },
                Property::new(Terrain::Factory, Some(p1), 100),
            ));

            // 敵所有の都市 (中立拠点は存在しない)
            world.spawn((
                GridPosition { x: 5, y: 5 },
                Property::new(Terrain::City, Some(p2), 100),
            ));

            // フリーの自軍歩兵
            world.spawn((
                p1,
                Faction(p1),
                GridPosition { x: 4, y: 4 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ));

            plan_squads(&mut world, p1);

            let manager = world.get_resource::<SquadManager>().unwrap();
            manager
                .squads
                .iter()
                .find(|s| s.mission_type == MissionType::Capture)
                .and_then(|s| s.target)
        };

        // V2: 敵拠点は占領部隊の対象にならない (従来挙動)
        assert_eq!(
            run(crate::ai::ai_version::AiVersion::V2),
            None,
            "V2 は敵拠点への占領部隊を編成しないはず"
        );

        // V3: 敵拠点を目標とする占領部隊が編成される
        assert_eq!(
            run(crate::ai::ai_version::AiVersion::V3),
            Some(GridPosition { x: 5, y: 5 }),
            "V3 は敵拠点 (5,5) への占領部隊を編成するはず"
        );
    }

    /// Issue #53 (Gemini #4): 敵生産施設への護衛の適格判定。
    /// 鈍足・損傷・弾切れのユニットは護衛から除外され、
    /// 機動力があり健全で弾薬のあるユニットのみが選ばれることを検証する。
    #[test]
    fn test_escort_eligibility_filter() {
        let infantry_movement = 3;

        // 機動力十分・健全・弾薬あり → 適格 (中戦車 移動6)
        let fast_tank = UnitStats {
            unit_type: UnitType::MdTank,
            max_movement: 6,
            max_ammo1: 6,
            max_ammo2: 9,
            ..UnitStats::mock()
        };
        assert!(
            escort_is_eligible(&fast_tank, infantry_movement, 100, 6, 9),
            "健全で機動力・弾薬のある戦車は護衛適格のはず"
        );

        // 鈍足 (移動1 < 歩兵3) → 不適格 (砲台のような据置き砲)
        let slow_gun = UnitStats {
            unit_type: UnitType::Artillery,
            max_movement: 1,
            max_ammo1: 5,
            ..UnitStats::mock()
        };
        assert!(
            !escort_is_eligible(&slow_gun, infantry_movement, 100, 5, 0),
            "歩兵より鈍足のユニットは護衛不適格のはず"
        );

        // 損傷 (HP < 70) → 不適格
        assert!(
            !escort_is_eligible(&fast_tank, infantry_movement, 50, 6, 9),
            "HP50 の損傷ユニットは護衛不適格のはず"
        );

        // 弾切れ (主武器0・副武器0) → 不適格
        assert!(
            !escort_is_eligible(&fast_tank, infantry_movement, 100, 0, 0),
            "主武器・副武器とも弾切れのユニットは護衛不適格のはず"
        );

        // 主武器弾切れでも副武器が残っていれば適格
        assert!(
            escort_is_eligible(&fast_tank, infantry_movement, 100, 0, 5),
            "副武器が残っていれば護衛適格のはず"
        );
    }

    /// Issue #53: 中立拠点と敵拠点が混在する場合、歩兵から近い目標が
    /// 優先的に割り当てられる (決定的な順序) ことを検証する
    #[test]
    fn test_v3_capture_targets_sorted_by_distance() {
        let mut world = setup_test_world();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        let mut settings = crate::ai::ai_version::PlayerAiSettings::new();
        settings.set_version(p1, crate::ai::ai_version::AiVersion::V3);
        world.insert_resource(settings);

        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::Factory, Some(p1), 100),
        ));
        // 近い敵拠点 (歩兵から距離2) と遠い中立拠点 (距離8)
        world.spawn((
            GridPosition { x: 5, y: 3 },
            Property::new(Terrain::City, Some(p2), 100),
        ));
        world.spawn((
            GridPosition { x: 9, y: 5 },
            Property::new(Terrain::City, None, 100),
        ));

        // フリー歩兵は1体のみ → 近い方 (敵拠点) に割り当てられるはず
        world.spawn((
            p1,
            Faction(p1),
            GridPosition { x: 3, y: 3 },
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                can_capture: true,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        plan_squads(&mut world, p1);

        let manager = world.get_resource::<SquadManager>().unwrap();
        let capture_targets: Vec<_> = manager
            .squads
            .iter()
            .filter(|s| s.mission_type == MissionType::Capture && !s.members.is_empty())
            .filter_map(|s| s.target)
            .collect();
        assert_eq!(
            capture_targets,
            vec![GridPosition { x: 5, y: 3 }],
            "1体しかいない歩兵は最も近い目標 (敵拠点) に割り当てられるはず"
        );
    }

    fn setup_transport_phase_world() -> (World, Entity, Entity, Entity, crate::ai::islands::IslandId)
    {
        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Shoal).unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let target_island = island_map
            .get_island_at(&GridPosition { x: 2, y: 0 })
            .unwrap()
            .id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);

        let player = PlayerId(1);
        let capture = world
            .spawn((
                Faction(player),
                GridPosition { x: 9999, y: 9999 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let combat = world
            .spawn((
                Faction(player),
                GridPosition { x: 9999, y: 9999 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    can_capture: false,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Lander,
                    movement_type: MovementType::Ship,
                    max_movement: 6,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry, UnitType::Tank],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: vec![capture, combat],
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        world.entity_mut(capture).insert(Transporting(transport));
        world.entity_mut(combat).insert(Transporting(transport));
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(Terrain::City, Some(PlayerId(2)), 100),
        ));
        (world, transport, capture, combat, target_island)
    }

    #[test]
    fn transport_pickup_waits_until_all_cargo_are_loaded() {
        let (mut world, transport, capture, combat, target_island) = setup_transport_phase_world();
        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .retain(|entity| *entity == capture);
        world.entity_mut(combat).remove::<Transporting>();
        *world.get_mut::<GridPosition>(combat).unwrap() = GridPosition { x: 0, y: 0 };

        let mut manager = SquadManager::new();
        let mut squad = manager.create_squad(MissionType::Transport).clone();
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities = vec![capture, combat];
        squad.target_island = Some(target_island);
        squad.phase = MissionPhase::Transport(TransportPhase::Pickup);

        assert!(!update_transport_squad_phase(&mut world, &mut squad));
        assert_eq!(squad.phase, MissionPhase::Transport(TransportPhase::Pickup));

        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .push(combat);
        world.entity_mut(combat).insert(Transporting(transport));
        *world.get_mut::<GridPosition>(combat).unwrap() = GridPosition { x: 9999, y: 9999 };
        assert!(!update_transport_squad_phase(&mut world, &mut squad));
        assert_eq!(
            squad.phase,
            MissionPhase::Transport(TransportPhase::Transit)
        );
    }

    #[test]
    fn transport_drop_waits_for_all_cargo_and_hands_them_off() {
        let (mut world, transport, capture, combat, target_island) = setup_transport_phase_world();
        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities = vec![capture, combat];
        squad.target_island = Some(target_island);
        squad.target = Some(GridPosition { x: 2, y: 0 });
        squad.phase = MissionPhase::Transport(TransportPhase::Drop);
        world.insert_resource(manager);

        // 1体目を降ろしても Drop を維持し、占領部隊へ引き渡す。
        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .retain(|entity| *entity == combat);
        world.entity_mut(capture).remove::<Transporting>();
        *world.get_mut::<GridPosition>(capture).unwrap() = GridPosition { x: 1, y: 0 };
        update_squads(&mut world, PlayerId(1));
        {
            let manager = world.resource::<SquadManager>();
            let transport_squad = manager
                .squads
                .iter()
                .find(|squad| squad.mission_type == MissionType::Transport)
                .unwrap();
            assert_eq!(
                transport_squad.phase,
                MissionPhase::Transport(TransportPhase::Drop)
            );
            assert_eq!(transport_squad.cargo_entities, vec![combat]);
            assert!(manager.squads.iter().any(|squad| {
                squad.mission_type == MissionType::Capture && squad.members.contains(&capture)
            }));
        }

        // 最後のカーゴを降ろした後だけ Return へ進み、戦闘部隊へ引き渡す。
        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .clear();
        world.entity_mut(combat).remove::<Transporting>();
        *world.get_mut::<GridPosition>(combat).unwrap() = GridPosition { x: 2, y: 0 };
        update_squads(&mut world, PlayerId(1));
        let manager = world.resource::<SquadManager>();
        let transport_squad = manager
            .squads
            .iter()
            .find(|squad| squad.mission_type == MissionType::Transport)
            .unwrap();
        assert_eq!(
            transport_squad.phase,
            MissionPhase::Transport(TransportPhase::Return)
        );
        assert!(transport_squad.cargo_entities.is_empty());
        assert!(manager.squads.iter().any(|squad| {
            squad.mission_type == MissionType::Attack && squad.members.contains(&combat)
        }));
    }

    #[test]
    fn landing_prefers_tile_outside_indirect_fire_range() {
        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
        for x in 1..=4 {
            map.set_terrain(x, 0, Terrain::Plains).unwrap();
        }
        map.set_terrain(1, 1, Terrain::Shoal).unwrap();
        map.set_terrain(3, 1, Terrain::Shoal).unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let target_island = island_map
            .get_island_at(&GridPosition { x: 4, y: 0 })
            .unwrap()
            .id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);
        let mut damage_chart = crate::resources::DamageChart::new();
        damage_chart.insert_damage(UnitType::Artillery, UnitType::Infantry, 50);
        world.insert_resource(damage_chart);

        let player = PlayerId(1);
        let cargo = world
            .spawn((
                Faction(player),
                GridPosition { x: 9999, y: 9999 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    cost: 1000,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::Lander,
                    movement_type: MovementType::Ship,
                    max_movement: 6,
                    max_cargo: 1,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 1,
                    loaded: vec![cargo],
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        world.entity_mut(cargo).insert(Transporting(transport));
        world.spawn((
            Faction(PlayerId(2)),
            GridPosition { x: 1, y: 2 },
            UnitStats {
                unit_type: UnitType::Artillery,
                movement_type: MovementType::Artillery,
                min_range: 2,
                max_range: 3,
                max_movement: 5,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let reachable = HashSet::from([(1, 1), (3, 1)]);
        let selected = select_landing_candidate(
            &mut world,
            transport,
            cargo,
            GridPosition { x: 1, y: 1 },
            &reachable,
            Some(target_island),
            Some(GridPosition { x: 4, y: 0 }),
        )
        .unwrap();
        assert_eq!(selected.0, GridPosition { x: 3, y: 1 });
        assert_eq!(selected.1, GridPosition { x: 3, y: 0 });
    }

    #[test]
    fn shoal_separated_unit_remains_transport_candidate() {
        let registry = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(1, 0, Terrain::Shoal).unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let target = GridPosition { x: 2, y: 0 };
        let target_island = island_map.get_island_at(&target).unwrap().id;
        let candidate_entity = Entity::from_raw(1);
        let candidates = vec![(
            candidate_entity,
            GridPosition { x: 0, y: 0 },
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                can_capture: true,
                ..UnitStats::mock()
            },
        )];
        let transport_stats = UnitStats {
            unit_type: UnitType::Lander,
            movement_type: MovementType::Ship,
            max_cargo: 2,
            loadable_unit_types: vec![UnitType::Infantry],
            ..UnitStats::mock()
        };
        let mut cache = TurnDistanceCache::default();
        let mut connectivity = TerrainConnectivity::default();

        assert_eq!(
            select_nearest_compatible_cargo(
                &candidates,
                GridPosition { x: 1, y: 0 },
                &transport_stats,
                target_island,
                Some(target),
                &island_map,
                &map,
                &registry,
                &HashMap::new(),
                PlayerId(1),
                &mut cache,
                &mut connectivity,
                &[],
                None,
                false,
            ),
            Some(0)
        );
    }

    #[test]
    fn shoal_is_legal_pickup_for_adjacent_ground_cargo() {
        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(2, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(1, 0, Terrain::Shoal).unwrap();
        world.insert_resource(map);
        world.insert_resource(registry);
        let cargo = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let transport_stats = UnitStats {
            unit_type: UnitType::Lander,
            movement_type: MovementType::Ship,
            max_movement: 6,
            loadable_unit_types: vec![UnitType::Infantry],
            ..UnitStats::mock()
        };

        assert_eq!(
            select_pickup_position(
                &world,
                GridPosition { x: 1, y: 0 },
                &transport_stats,
                &[cargo],
                &mut TerrainConnectivity::default(),
            ),
            Some(GridPosition { x: 1, y: 0 })
        );
    }
}
