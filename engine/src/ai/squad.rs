#![allow(clippy::collapsible_if)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::manual_while_let_some)]
#![allow(clippy::unnecessary_map_or)]

use crate::ai::cluster::detect_enemy_clusters;
use crate::ai::strategy::analyze_strategy;
use crate::ai::turn_distance::{TurnDistanceCache, calculate_turn_distance};
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

/// 部隊（Squad）の定義
#[derive(Debug, Clone)]
pub struct Squad {
    pub id: u32,
    pub members: HashSet<Entity>,
    pub mission_type: MissionType,
    pub target: Option<GridPosition>, // 攻撃・防衛・占領の目標座標
    pub target_island: Option<crate::ai::islands::IslandId>, // 輸送ターゲットの島
    pub phase: MissionPhase,
    pub transport_cargo: Option<Entity>, // 輸送対象の歩兵
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
            id: self.next_id,
            members: HashSet::new(),
            mission_type,
            target: None,
            target_island: None,
            phase: MissionPhase::Forming,
            transport_cargo: None,
        };
        self.next_id += 1;
        self.squads.push(squad);
        self.squads.last_mut().unwrap()
    }

    pub fn remove_squad(&mut self, id: u32) {
        self.squads.retain(|s| s.id != id);
    }
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
        if let Some(cargo) = squad.transport_cargo {
            if !existing_entities.contains(&cargo) {
                squad.transport_cargo = None;
            }
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
    let mut i = 0;
    while i < manager.squads.len() {
        if manager.squads[i].mission_type == MissionType::Transport {
            let mut squad = manager.squads[i].clone();
            let should_remove = update_transport_squad_phase(world, &mut squad);
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

    // メンバーが0になった部隊の解散
    manager
        .squads
        .retain(|s| !s.members.is_empty() || s.phase == MissionPhase::Forming);

    world.insert_resource(manager);
}

/// ゲームの戦略状況に基づいて、自動的に部隊の構築と新規メンバーの割り当てを行います。
pub fn plan_squads(world: &mut World, perspective_player: PlayerId) {
    // 1. まず既存部隊のクリーンアップと SoloFallback 判定を実行
    update_squads(world, perspective_player);

    let mut manager = world.remove_resource::<SquadManager>().unwrap_or_default();
    let strategy = analyze_strategy(world, perspective_player);
    let enemy_clusters = detect_enemy_clusters(world, perspective_player);

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
        if let Some(cargo) = squad.transport_cargo {
            busy_entities.insert(cargo);
        }
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
    )>();
    for (entity, faction, pos, stats, transporting) in q_my_units.iter(world) {
        if faction.0 == perspective_player
            && !busy_entities.contains(&entity)
            && !manager.solo_fallbacks.contains(&entity)
            && transporting.is_none()
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

    for target_island_id in target_islands {
        if free_transports.is_empty() || free_infantry.is_empty() {
            break;
        }

        // すでにその島をターゲットとしている輸送部隊があるか確認
        let already_assigned = manager.squads.iter().any(|s| {
            s.mission_type == MissionType::Transport && s.target_island == Some(target_island_id)
        });

        if !already_assigned {
            let (trans_ent, _, _) = free_transports.pop().unwrap();
            let (inf_ent, _, _) = free_infantry.pop().unwrap();

            let squad = manager.create_squad(MissionType::Transport);
            squad.members.insert(trans_ent);
            squad.transport_cargo = Some(inf_ent);
            squad.target_island = Some(target_island_id);
            squad.phase = MissionPhase::Transport(TransportPhase::Pickup);
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
                perspective_player,
                &mut turn_cache,
            );

            if turn_dist <= 5 {
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
    for unowned_pos in &strategy.unowned_properties {
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

                let (inf_ent, _, _) = free_infantry.remove(assigned_inf_idx);
                squad.members.insert(inf_ent);
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
        let mut min_dist = u32::MAX;

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
            let mut nearest_cluster_dist = u32::MAX;
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
            if nearest_cluster_dist <= 15 {
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
        let mut best_dock_tile = None;
        let mut min_dist = 9999;

        for tile in &island.tiles {
            for (ax, ay) in map.get_adjacent(tile.x, tile.y) {
                if let Some(terrain) = map.get_terrain(ax, ay) {
                    if crate::systems::movement::get_valid_movement_cost(
                        registry,
                        movement_type,
                        terrain,
                    )
                    .is_some()
                    {
                        let dist =
                            (ax as i32 - t_pos.x as i32).abs() + (ay as i32 - t_pos.y as i32).abs();
                        if dist < min_dist {
                            min_dist = dist;
                            best_dock_tile = Some(GridPosition { x: ax, y: ay });
                        }
                    }
                }
            }
        }
        best_dock_tile
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

/// 輸送部隊のフェーズ更新と完了判定
pub fn update_transport_squad_phase(world: &mut World, squad: &mut Squad) -> bool {
    let transport_entity = match squad.members.iter().next() {
        Some(&e) => e,
        None => return true,
    };

    if world.get::<GridPosition>(transport_entity).is_none() {
        return true;
    }

    let cargo_entity = match squad.transport_cargo {
        Some(e) => e,
        None => return true,
    };

    let phase = match squad.phase {
        MissionPhase::Transport(p) => p,
        _ => return false,
    };

    if phase != TransportPhase::Return && world.get::<GridPosition>(cargo_entity).is_none() {
        return true;
    }

    match phase {
        TransportPhase::Pickup => {
            let loaded = if let Some(cargo) =
                world.get::<crate::components::CargoCapacity>(transport_entity)
            {
                cargo.loaded.contains(&cargo_entity)
            } else {
                false
            };
            let transporting = world
                .get::<crate::components::Transporting>(cargo_entity)
                .is_some_and(|t| t.0 == transport_entity);

            if loaded || transporting {
                squad.phase = MissionPhase::Transport(TransportPhase::Transit);
            }
        }
        TransportPhase::Transit => {
            let loaded = if let Some(cargo) =
                world.get::<crate::components::CargoCapacity>(transport_entity)
            {
                cargo.loaded.contains(&cargo_entity)
            } else {
                false
            };
            let transporting = world
                .get::<crate::components::Transporting>(cargo_entity)
                .is_some_and(|t| t.0 == transport_entity);

            if !loaded && !transporting {
                squad.phase = MissionPhase::Transport(TransportPhase::Return);
            } else if let Some(target_island_id) = squad.target_island {
                if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() {
                    if let Some(island) =
                        island_map.islands.iter().find(|i| i.id == target_island_id)
                    {
                        if let Some(t_pos) = world.get::<GridPosition>(transport_entity).cloned() {
                            let map = world.resource::<Map>();
                            let registry = world.resource::<MasterDataRegistry>();
                            let t_stats = world.get::<UnitStats>(transport_entity).unwrap();
                            if let Some(target_pos) = get_target_position_for_island(
                                map,
                                registry,
                                island,
                                t_pos,
                                t_stats.movement_type,
                            ) {
                                if (target_pos.x as i32 - t_pos.x as i32).abs()
                                    + (target_pos.y as i32 - t_pos.y as i32).abs()
                                    <= 1
                                {
                                    squad.phase = MissionPhase::Transport(TransportPhase::Drop);
                                }
                            }
                        }
                    }
                }
            }
        }
        TransportPhase::Drop => {
            let loaded = if let Some(cargo) =
                world.get::<crate::components::CargoCapacity>(transport_entity)
            {
                cargo.loaded.contains(&cargo_entity)
            } else {
                false
            };
            let transporting = world
                .get::<crate::components::Transporting>(cargo_entity)
                .is_some_and(|t| t.0 == transport_entity);

            if !loaded && !transporting {
                squad.phase = MissionPhase::Transport(TransportPhase::Return);
            }
        }
        TransportPhase::Return => {
            if let Some(t_pos) = world.get::<GridPosition>(transport_entity).cloned() {
                if let Some(t_faction) = world.get::<Faction>(transport_entity).map(|f| f.0) {
                    let mut query = world.query::<(&GridPosition, &Property)>();
                    let at_base = query.iter(world).any(|(pos, prop)| {
                        pos.x == t_pos.x && pos.y == t_pos.y && prop.owner_id == Some(t_faction)
                    });
                    if at_base {
                        return true;
                    }
                }
            }
        }
    }

    false
}

/// 輸送部隊の実行ステップ意思決定
pub fn execute_transport_squad_step(
    world: &mut World,
    squad: &Squad,
) -> Option<(Entity, crate::ai::engine::AiCommand)> {
    let transport_entity = *squad.members.iter().next()?;
    let cargo_entity = squad.transport_cargo?;

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
            let cargo_pos = world.get::<GridPosition>(cargo_entity).cloned()?;
            let dist = (t_pos.x as i32 - cargo_pos.x as i32).abs()
                + (t_pos.y as i32 - cargo_pos.y as i32).abs();

            if dist <= 1 {
                return Some((
                    cargo_entity,
                    crate::ai::engine::AiCommand::Load {
                        transport_entity,
                        target_pos: t_pos,
                    },
                ));
            }

            // 1. 輸送機がまだ行動していないなら、輸送機を歩兵へ近づける
            let transport_moved = world
                .get::<crate::components::HasMoved>(transport_entity)
                .map_or(true, |h| h.0);
            let transport_action_completed = world
                .get::<crate::components::ActionCompleted>(transport_entity)
                .map_or(true, |a| a.0);

            if !transport_moved && !transport_action_completed {
                let mut best_tile = t_pos;
                let mut min_dist = dist;

                for target_tile in &reachable {
                    let d = (target_tile.0 as i32 - cargo_pos.x as i32).abs()
                        + (target_tile.1 as i32 - cargo_pos.y as i32).abs();
                    if d < min_dist {
                        min_dist = d;
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
                .map_or(true, |h| h.0);
            let cargo_action_completed = world
                .get::<crate::components::ActionCompleted>(cargo_entity)
                .map_or(true, |a| a.0);

            if !cargo_moved && !cargo_action_completed {
                let c_stats = world.get::<UnitStats>(cargo_entity).cloned()?;
                let c_fuel = world
                    .get::<crate::components::Fuel>(cargo_entity)
                    .map(|f| f.current)?;

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
                let mut min_dist = dist;

                for target_tile in &cargo_reachable {
                    let d = (target_tile.0 as i32 - t_pos.x as i32).abs()
                        + (target_tile.1 as i32 - t_pos.y as i32).abs();
                    if d < min_dist {
                        min_dist = d;
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

                if let (Some(island_tiles), Some(target_pos)) = (island_tiles, target_pos) {
                    let mut best_drop_tile_pair = None;
                    let mut min_drop_dist = 9999;

                    for &(rx, ry) in &reachable {
                        let test_pos = GridPosition { x: rx, y: ry };
                        let drop_targets = crate::systems::transport::get_droppable_tiles_at(
                            world,
                            transport_entity,
                            cargo_entity,
                            test_pos,
                        );
                        for drop_target in drop_targets {
                            let drop_pos = GridPosition {
                                x: drop_target.0,
                                y: drop_target.1,
                            };
                            if island_tiles.contains(&drop_pos) {
                                let dist = (rx as i32 - t_pos.x as i32).abs()
                                    + (ry as i32 - t_pos.y as i32).abs();
                                if dist < min_drop_dist {
                                    min_drop_dist = dist;
                                    best_drop_tile_pair = Some((test_pos, drop_pos));
                                }
                            }
                        }
                    }

                    if let Some((trans_pos, drop_pos)) = best_drop_tile_pair {
                        return Some((
                            transport_entity,
                            crate::ai::engine::AiCommand::Drop {
                                transport_target_pos: trans_pos,
                                cargo_drop_pos: drop_pos,
                                cargo_entity,
                            },
                        ));
                    }

                    let mut best_tile = t_pos;
                    let mut min_dist = (t_pos.x as i32 - target_pos.x as i32).abs()
                        + (t_pos.y as i32 - target_pos.y as i32).abs();

                    for target_tile in &reachable {
                        let dist = (target_tile.0 as i32 - target_pos.x as i32).abs()
                            + (target_tile.1 as i32 - target_pos.y as i32).abs();
                        if dist < min_dist {
                            min_dist = dist;
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
            let drop_tiles = crate::systems::transport::get_droppable_tiles(
                world,
                transport_entity,
                cargo_entity,
            );
            let mut chosen_drop_tile = None;
            if let Some(target_island_id) = squad.target_island
                && let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>()
            {
                for &tile in &drop_tiles {
                    let pos = GridPosition {
                        x: tile.0,
                        y: tile.1,
                    };
                    if let Some(island) = island_map.get_island_at(&pos) {
                        if island.id == target_island_id {
                            chosen_drop_tile = Some(pos);
                            break;
                        }
                    }
                }
            }
            let final_drop_pos = chosen_drop_tile
                .or_else(|| drop_tiles.first().map(|t| GridPosition { x: t.0, y: t.1 }));

            if let Some(drop_pos) = final_drop_pos {
                return Some((
                    transport_entity,
                    crate::ai::engine::AiCommand::Drop {
                        transport_target_pos: t_pos,
                        cargo_drop_pos: drop_pos,
                        cargo_entity,
                    },
                ));
            } else {
                let mut best_drop_tile_pair = None;
                let mut min_drop_dist = 9999;

                for &(rx, ry) in &reachable {
                    let test_pos = GridPosition { x: rx, y: ry };
                    let drop_targets = crate::systems::transport::get_droppable_tiles_at(
                        world,
                        transport_entity,
                        cargo_entity,
                        test_pos,
                    );
                    if let Some(drop_target) = drop_targets.first() {
                        let dist =
                            (rx as i32 - t_pos.x as i32).abs() + (ry as i32 - t_pos.y as i32).abs();
                        if dist < min_drop_dist {
                            min_drop_dist = dist;
                            best_drop_tile_pair = Some((
                                test_pos,
                                GridPosition {
                                    x: drop_target.0,
                                    y: drop_target.1,
                                },
                            ));
                        }
                    }
                }

                if let Some((trans_pos, drop_pos)) = best_drop_tile_pair {
                    return Some((
                        transport_entity,
                        crate::ai::engine::AiCommand::Drop {
                            transport_target_pos: trans_pos,
                            cargo_drop_pos: drop_pos,
                            cargo_entity,
                        },
                    ));
                }

                if let Some(target_island_id) = squad.target_island {
                    if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>()
                    {
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
                                let mut min_dist = (t_pos.x as i32 - target_pos.x as i32).abs()
                                    + (t_pos.y as i32 - target_pos.y as i32).abs();

                                for target_tile in &reachable {
                                    let dist = (target_tile.0 as i32 - target_pos.x as i32).abs()
                                        + (target_tile.1 as i32 - target_pos.y as i32).abs();
                                    if dist < min_dist {
                                        min_dist = dist;
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
        }
        TransportPhase::Return => {
            let mut nearest_prop_pos = t_pos;
            let mut min_dist = 9999;
            let mut query = world.query::<(&GridPosition, &Property)>();
            for (pos, prop) in query.iter(world) {
                if prop.owner_id == Some(t_faction) {
                    let dist = (pos.x as i32 - t_pos.x as i32).abs()
                        + (pos.y as i32 - t_pos.y as i32).abs();
                    if dist < min_dist {
                        min_dist = dist;
                        nearest_prop_pos = *pos;
                    }
                }
            }

            let mut best_tile = t_pos;
            let mut best_dist = (t_pos.x as i32 - nearest_prop_pos.x as i32).abs()
                + (t_pos.y as i32 - nearest_prop_pos.y as i32).abs();

            for target_tile in &reachable {
                let dist = (target_tile.0 as i32 - nearest_prop_pos.x as i32).abs()
                    + (target_tile.1 as i32 - nearest_prop_pos.y as i32).abs();
                if dist < best_dist {
                    best_dist = dist;
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
