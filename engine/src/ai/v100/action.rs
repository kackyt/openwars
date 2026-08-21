//! Gameboy Wars Turboの部隊選択・行動判定を模擬するV100/V200専用実装。
//!
//! ROMは行動を単一の加重点へ変換せず、部隊選択後に占領、攻撃、IQ200固有の
//! 合流、通常移動を順番に試す。この段階構造とWRAM候補盤面の行優先走査を保つ。

use super::candidate_field::{
    CandidateTile, build_candidate_field, build_load_candidate_field, build_merge_candidate_field,
};
use super::route_field::build_route_field;
use crate::ai::AiVersion;
use crate::ai::engine::AiCommand;
use crate::components::{
    ActionCompleted, Ammo, Faction, Fuel, GridPosition, HasMoved, Health, PlayerId, Property,
    UnitStats,
};
use crate::resources::{DamageChart, Map, MasterDataRegistry, MovementType};
use crate::systems::combat::get_expected_damage;
use crate::systems::movement::OccupantInfo;
use bevy_ecs::prelude::*;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

/// GBの部隊レコードに保持される個別目標座標に相当するV100/V200専用状態。
///
/// AIは1手番を複数のstepに分けて実行するため、毎stepで現在位置から目標を
/// 引き直すと、先に動いた部隊の影響で後続部隊の目標が変わってしまう。
#[derive(Resource, Default)]
struct ObjectiveAssignmentState {
    by_player: HashMap<PlayerId, HashMap<Entity, GridPosition>>,
}

#[derive(Clone, Copy)]
struct AssignmentUnit {
    entity: Entity,
    position: GridPosition,
    unit_type: crate::resources::UnitType,
    record_order: u32,
    fast_ground: bool,
    air_unit: bool,
}

#[derive(Clone)]
struct UnitView {
    entity: Entity,
    record_order: u32,
    position: GridPosition,
    stats: UnitStats,
    hp: u32,
    max_hp: u32,
    fuel: u32,
    ammo: (u32, u32),
    cargo_count: usize,
}

#[derive(Clone)]
struct EnemyView {
    entity: Entity,
    position: GridPosition,
    stats: UnitStats,
    hp: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct AttackKey {
    /// ROMの攻撃相性表に対応する代替量。撃破後の残HPが小さい対象を先にする。
    target_remaining_hp: u32,
    /// 攻撃後に残る地形の防御値が高い方を先にする。
    defensive_position: Reverse<u32>,
    /// 同じ残HPなら、元から損耗している対象を先にする。
    target_hp: u32,
}

struct AttackEvaluationContext<'a> {
    map: &'a Map,
    master_data: &'a MasterDataRegistry,
    damage_chart: &'a DamageChart,
    properties: &'a [(GridPosition, Property)],
    player_id: PlayerId,
    version: AiVersion,
}

/// V100/V200の次の行動を決める。既存V1〜V4の評価器は呼び出さない。
pub(crate) fn decide_action(
    world: &mut World,
    player_id: PlayerId,
    skipped: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let map = world.get_resource::<Map>()?.clone();
    let master_data = world.get_resource::<MasterDataRegistry>()?.clone();
    let damage_chart = world.get_resource::<DamageChart>()?.clone();
    let version = crate::ai::resolve_player_ai_version(world, player_id);
    let mut units = Vec::new();
    let mut assignment_units = Vec::new();
    let mut occupants = HashMap::new();
    let mut friendly_by_position = HashMap::new();
    {
        let mut query = world.query::<(
            Entity,
            &GridPosition,
            &Faction,
            &UnitStats,
            &Health,
            &Fuel,
            Option<&Ammo>,
            &HasMoved,
            &ActionCompleted,
            Option<&crate::components::CargoCapacity>,
            Option<&crate::components::Transporting>,
        )>();
        for (
            entity,
            position,
            faction,
            stats,
            health,
            fuel,
            ammo,
            moved,
            completed,
            cargo,
            transporting,
        ) in query.iter(world)
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
                    free_slots: cargo.map_or(stats.max_cargo, |value| {
                        value.max.saturating_sub(value.loaded.len() as u32)
                    }),
                },
            );
            if faction.0 == player_id {
                friendly_by_position.insert((position.x, position.y), entity);
                assignment_units.push(AssignmentUnit {
                    entity,
                    position: *position,
                    unit_type: stats.unit_type,
                    record_order: 0,
                    fast_ground: stats.movement_type == MovementType::ArmoredCar
                        && stats.max_movement >= 6,
                    air_unit: stats.movement_type == MovementType::Air,
                });
                if !moved.0 && !completed.0 && !skipped.contains(&entity) {
                    units.push(UnitView {
                        entity,
                        record_order: 0,
                        position: *position,
                        stats: stats.clone(),
                        hp: health.current,
                        max_hp: health.max,
                        fuel: fuel.current,
                        ammo: ammo.map_or((0, 0), |value| (value.ammo1, value.ammo2)),
                        cargo_count: cargo.map_or(0, |value| value.loaded.len()),
                    });
                }
            }
        }
    }
    let observed_records: Vec<_> = assignment_units
        .iter()
        .map(|unit| (unit.entity, unit.position, unit.unit_type))
        .collect();
    let record_orders =
        super::unit_record::synchronize_unit_records(world, player_id, &observed_records);
    for unit in &mut assignment_units {
        unit.record_order = record_orders
            .get(&unit.entity)
            .copied()
            .unwrap_or_else(|| unit.entity.index());
    }
    for unit in &mut units {
        unit.record_order = record_orders
            .get(&unit.entity)
            .copied()
            .unwrap_or_else(|| unit.entity.index());
    }
    let properties: Vec<_> = {
        let mut query = world.query::<(&GridPosition, &Property)>();
        query
            .iter(world)
            .map(|(position, property)| (*position, *property))
            .collect()
    };
    let property_positions: HashSet<_> = properties
        .iter()
        .map(|(position, _)| (position.x, position.y))
        .collect();
    // 生産直後の行動済み部隊へ目標を割り当てると、同じ初期波を増援と誤認する。
    // ROMと同様、実際に行動可能な部隊が現れた時点でその波をまとめて割り当てる。
    let objective_assignments = if units.is_empty() {
        world
            .get_resource::<ObjectiveAssignmentState>()
            .and_then(|state| state.by_player.get(&player_id))
            .cloned()
            .unwrap_or_default()
    } else {
        update_objective_assignments(world, &assignment_units, &properties, &map, player_id)
    };
    let mut enemies: Vec<_> = {
        let mut query = world.query::<(Entity, &GridPosition, &Faction, &UnitStats, &Health)>();
        query
            .iter(world)
            .filter_map(|(entity, position, faction, stats, health)| {
                (faction.0 != player_id && health.current > 0).then_some(EnemyView {
                    entity,
                    position: *position,
                    stats: stats.clone(),
                    hp: health.current,
                })
            })
            .collect()
    };
    enemies.sort_by_key(|enemy| (enemy.position.y, enemy.position.x, enemy.entity.index()));

    // ROM 44BC/45B8は都市・工場上の部隊を先に選び、生産地点を空けてから一般部隊へ進む。
    let objective_route_costs: HashMap<_, _> = units
        .iter()
        .filter_map(|unit| {
            let objective = objective_assignments.get(&unit.entity).copied()?;
            let route_field =
                build_route_field(&map, &master_data, objective, unit.stats.movement_type);
            Some((
                unit.entity,
                route_field.get(&unit.position).copied().unwrap_or(u32::MAX),
            ))
        })
        .collect();
    units.sort_by_key(|unit| {
        actor_priority(
            unit,
            &property_positions,
            objective_assignments.get(&unit.entity).copied(),
            objective_route_costs.get(&unit.entity).copied(),
        )
    });

    if let Some(actor) = units.into_iter().next() {
        let candidates = build_candidate_field(
            &map,
            &occupants,
            actor.position,
            &actor.stats,
            actor.fuel,
            player_id,
            &master_data,
        );

        // 歩兵が輸送ユニットのいる候補マスへ入った場合は、通常待機ではなく搭載を確定する。
        // 輸送ユニットが存在しない盤面では候補が空になるため、通常判断へ影響しない。
        let load_candidates = build_load_candidate_field(
            &map,
            &occupants,
            actor.position,
            &actor.stats,
            actor.fuel,
            player_id,
            &master_data,
        );
        if let Some(command) =
            super::transport::choose_load(world, actor.entity, &load_candidates, player_id)
        {
            return Some((actor.entity, command));
        }
        if super::compatibility_profile::is_gbw_transport(&actor.stats)
            && let Some(command) = super::transport::choose_transport_action(
                world,
                actor.entity,
                &candidates,
                objective_assignments.get(&actor.entity).copied(),
                player_id,
                version,
            )
        {
            return Some((actor.entity, command));
        }
        // ROM 49F6の判定表と同じく、占領は通常攻撃より前に確定する。
        if let Some(command) = choose_capture(
            &actor,
            &candidates,
            &properties,
            objective_assignments.get(&actor.entity).copied(),
            player_id,
        ) {
            return Some((actor.entity, command));
        }
        if let Some(command) = choose_attack(
            &actor,
            &candidates,
            &enemies,
            &AttackEvaluationContext {
                map: &map,
                master_data: &master_data,
                damage_chart: &damage_chart,
                properties: &properties,
                player_id,
                version,
            },
        ) {
            return Some((actor.entity, command));
        }
        // ROM 5D36はIQ200だけが通る同種部隊合流分岐。
        if version == AiVersion::V200
            && let Some(command) = choose_merge(
                &actor,
                &map,
                &occupants,
                &friendly_by_position,
                world,
                player_id,
                &master_data,
            )
        {
            return Some((actor.entity, command));
        }
        return Some((
            actor.entity,
            choose_wait(
                &actor,
                &candidates,
                objective_assignments.get(&actor.entity).copied(),
                &map,
                &master_data,
            ),
        ));
    }
    None
}

/// 既知のGB初期部隊では高速地上系を先に、続いて施設を塞ぐ部隊を処理する。
fn actor_priority(
    unit: &UnitView,
    property_positions: &HashSet<(usize, usize)>,
    objective: Option<GridPosition>,
    objective_route_cost: Option<u32>,
) -> (u8, u32, u32) {
    if super::compatibility_profile::is_gbw_transport(&unit.stats) && unit.cargo_count == 0 {
        // ROM 530Cの空輸送分岐は通常の部隊走査より先に搭載候補へ接近する。
        return (0, 0, unit.record_order);
    }
    let on_property = property_positions.contains(&(unit.position.x, unit.position.y));
    let fast_ground =
        unit.stats.movement_type == MovementType::ArmoredCar && unit.stats.max_movement >= 6;
    match (
        unit.stats.min_range > 1,
        on_property,
        fast_ground,
        objective,
    ) {
        // ROM 44F4は間接攻撃部隊を施設上の通常部隊より先に走査する。
        (true, _, _, _) => (1, 0, unit.record_order),
        // ROMの施設退避走査は部隊レコード昇順。
        (false, true, true, _) => (2, 0, unit.record_order),
        (false, true, false, _) => (3, 0, unit.record_order),
        // ROM 452Cは目標値0xFFの未割当部隊をレコード順に選ぶ。
        (false, false, _, None) => (4, 0, unit.record_order),
        (false, false, _, Some(_)) => {
            // ROM 45E5は部隊レコード+15の目標経路値を最小化し、同値なら後の
            // レコードを選ぶ。値はhex直線距離ではなく地形コスト込みのDBC6相当値。
            (
                5,
                objective_route_cost.unwrap_or(u32::MAX),
                u32::MAX - unit.record_order,
            )
        }
    }
}

/// 部隊レコード順に最寄りの未割当拠点を予約し、GBの個別目標欄に相当する表を作る。
#[cfg(test)]
fn assign_objectives(
    units: &[(Entity, GridPosition)],
    properties: &[(GridPosition, Property)],
    map: &Map,
    player_id: PlayerId,
) -> HashMap<Entity, GridPosition> {
    let ordered_units: Vec<_> = units
        .iter()
        .map(|(entity, position)| (*entity, *position, entity.index(), false))
        .collect();
    assign_unreserved_objectives(
        &ordered_units,
        properties,
        map,
        player_id,
        &mut HashSet::new(),
    )
}

/// 生存部隊の既存目標を保ち、未割当部隊だけへ新しい目標を設定する。
fn update_objective_assignments(
    world: &mut World,
    units: &[AssignmentUnit],
    properties: &[(GridPosition, Property)],
    map: &Map,
    player_id: PlayerId,
) -> HashMap<Entity, GridPosition> {
    let alive: HashSet<_> = units.iter().map(|unit| unit.entity).collect();
    let reinforcement_wave = world
        .get_resource::<ObjectiveAssignmentState>()
        .is_some_and(|state| state.by_player.contains_key(&player_id));
    let mut assignments = world
        .get_resource::<ObjectiveAssignmentState>()
        .and_then(|state| state.by_player.get(&player_id))
        .cloned()
        .unwrap_or_default();
    // GBの目標座標は他の部隊が先に占領してもレコードから消えない。部隊が生存する
    // 間は保持し、現在の所有者変化だけを理由に再割当しない。
    assignments.retain(|entity, _| alive.contains(entity));

    let mut reserved: HashSet<_> = assignments.values().copied().collect();
    let unassigned: Vec<_> = units
        .iter()
        .filter(|unit| !assignments.contains_key(&unit.entity))
        .copied()
        .collect();
    if reinforcement_wave {
        assignments.extend(assign_reinforcement_objectives(
            &unassigned,
            properties,
            map,
            player_id,
            &mut reserved,
        ));
    } else {
        let unassigned_positions: Vec<_> = unassigned
            .iter()
            .map(|unit| (unit.entity, unit.position, unit.record_order, unit.air_unit))
            .collect();
        assignments.extend(assign_unreserved_objectives(
            &unassigned_positions,
            properties,
            map,
            player_id,
            &mut reserved,
        ));
    }

    let mut state = world.get_resource_or_insert_with(ObjectiveAssignmentState::default);
    state.by_player.insert(player_id, assignments.clone());
    assignments
}

/// 初期展開後の増援は、ROMで観測した通り役割別に敵本拠地群の同じ目標を共有する。
fn assign_reinforcement_objectives(
    units: &[AssignmentUnit],
    properties: &[(GridPosition, Property)],
    map: &Map,
    player_id: PlayerId,
    reserved: &mut HashSet<GridPosition>,
) -> HashMap<Entity, GridPosition> {
    let enemy_properties: Vec<_> = properties
        .iter()
        .filter(|(_, property)| {
            property.owner_id.is_some_and(|owner| owner != player_id)
                && property.max_capture_points > 0
        })
        .copied()
        .collect();
    let mut result = HashMap::new();
    let mut ordered_units = units.to_vec();
    ordered_units.sort_by_key(|unit| unit.record_order);
    for unit in &ordered_units {
        if unit.air_unit {
            // 航空増援は、既存部隊がまだ担当していない拠点へ順に割り当てる。
            // これはROMの航空部隊・輸送部隊が遠隔島へ個別目標を持つ挙動に対応する。
            let unreserved_exists = properties.iter().any(|(position, property)| {
                property.max_capture_points > 0
                    && property.owner_id != Some(player_id)
                    && !reserved.contains(position)
            });
            let selected = properties
                .iter()
                .filter(|(position, property)| {
                    property.max_capture_points > 0
                        && property.owner_id != Some(player_id)
                        && (!unreserved_exists || !reserved.contains(position))
                })
                .min_by_key(|(position, property)| {
                    (
                        mobile_objective_rank(property.terrain),
                        map.distance(unit.position.x, unit.position.y, position.x, position.y),
                        objective_scan_order(*position, player_id).0,
                        objective_scan_order(*position, player_id).1,
                    )
                })
                .map(|(position, _)| *position);
            if let Some(position) = selected {
                reserved.insert(position);
                result.insert(unit.entity, position);
            }
            continue;
        }
        let selected = enemy_properties
            .iter()
            .min_by_key(|(position, property)| {
                let role_rank = if unit.fast_ground {
                    u8::from(property.terrain != crate::resources::Terrain::Capital)
                } else {
                    u8::from(property.terrain != crate::resources::Terrain::Factory)
                };
                (
                    role_rank,
                    map.distance(unit.position.x, unit.position.y, position.x, position.y),
                    position.y,
                    Reverse(position.x),
                )
            })
            .map(|(position, _)| *position);
        if let Some(position) = selected {
            result.insert(unit.entity, position);
        }
    }
    result
}

fn assign_unreserved_objectives(
    units: &[(Entity, GridPosition, u32, bool)],
    properties: &[(GridPosition, Property)],
    map: &Map,
    player_id: PlayerId,
    reserved: &mut HashSet<GridPosition>,
) -> HashMap<Entity, GridPosition> {
    let mut ordered_units = units.to_vec();
    ordered_units.sort_by_key(|(_, _, record_order, _)| *record_order);
    let objectives: Vec<_> = properties
        .iter()
        .filter(|(_, property)| {
            property.max_capture_points > 0 && property.owner_id != Some(player_id)
        })
        .copied()
        .collect();
    let mut result = HashMap::new();

    for (entity, origin, _, air_unit) in ordered_units {
        let unreserved_exists = objectives
            .iter()
            .any(|(position, _)| !reserved.contains(position));
        let selected = objectives
            .iter()
            .filter(|(position, _)| !unreserved_exists || !reserved.contains(position))
            .min_by_key(|(position, property)| {
                let distance = map.distance(origin.x, origin.y, position.x, position.y);
                let scan_order = objective_scan_order(*position, player_id);
                if air_unit {
                    // ROMの航空部隊は近隣都市より、輸送路となる港・空港を先に割り当てる。
                    (
                        mobile_objective_rank(property.terrain),
                        distance,
                        0,
                        scan_order.0,
                        scan_order.1,
                    )
                } else {
                    (0, distance, 0, scan_order.0, scan_order.1)
                }
            })
            .map(|(position, _)| *position);
        if let Some(position) = selected {
            reserved.insert(position);
            result.insert(entity, position);
        }
    }
    result
}

fn mobile_objective_rank(terrain: crate::resources::Terrain) -> u32 {
    match terrain {
        crate::resources::Terrain::Port => 0,
        crate::resources::Terrain::Airport => 1,
        crate::resources::Terrain::Factory => 2,
        crate::resources::Terrain::Capital => 3,
        _ => 4,
    }
}

/// ROMは盤面配列を行優先で走査し、同値なら後から現れたマスへ更新する。
fn objective_scan_order(position: GridPosition, _player_id: PlayerId) -> (usize, usize) {
    (usize::MAX - position.y, usize::MAX - position.x)
}

fn choose_capture(
    actor: &UnitView,
    candidates: &[CandidateTile],
    properties: &[(GridPosition, Property)],
    assigned_objective: Option<GridPosition>,
    player_id: PlayerId,
) -> Option<AiCommand> {
    if !actor.stats.can_capture {
        return None;
    }
    let is_capturable = |position: GridPosition| {
        properties.iter().any(|(property_position, property)| {
            *property_position == position
                && property.owner_id != Some(player_id)
                && property.max_capture_points > 0
        })
    };
    // ROMの個別目標が今手番で到達できる場合は、途中の別拠点へ逸れず目標を占領する。
    if let Some(objective) = assigned_objective
        && is_capturable(objective)
        && candidates
            .iter()
            .any(|candidate| candidate.position == objective)
    {
        return Some(AiCommand::Capture {
            target_pos: objective,
        });
    }

    let mut best: Option<(u32, GridPosition)> = None;
    for candidate in candidates {
        if is_capturable(candidate.position)
            && best
                .as_ref()
                .is_none_or(|(cost, _)| candidate.movement_cost <= *cost)
        {
            // ROM 593Dは同一コストでも後から走査したマスへ更新する。
            best = Some((candidate.movement_cost, candidate.position));
        }
    }
    best.map(|(_, target_pos)| AiCommand::Capture { target_pos })
}

fn choose_attack(
    actor: &UnitView,
    candidates: &[CandidateTile],
    enemies: &[EnemyView],
    context: &AttackEvaluationContext<'_>,
) -> Option<AiCommand> {
    let mut best: Option<(AttackKey, AiCommand)> = None;
    let attack_movement = attack_movement_allowance(&actor.stats, context.version);
    for candidate in candidates {
        // ROM 56D4はIQ100の移動後攻撃盤面だけ移動力を1減らし、最低2を保証する。
        // IQ200では減算値が0なので、通常移動と同じ移動力を使う。
        if candidate.movement_cost > attack_movement {
            continue;
        }
        if candidate.position != actor.position && actor.stats.min_range > 1 {
            continue;
        }
        let terrain_defense = attack_position_value(
            candidate.position,
            context.map,
            context.master_data,
            context.properties,
            context.player_id,
            context.version,
        );
        for enemy in enemies {
            let distance = context.map.distance(
                candidate.position.x,
                candidate.position.y,
                enemy.position.x,
                enemy.position.y,
            );
            let enemy_defense = context
                .map
                .get_terrain(enemy.position.x, enemy.position.y)
                .map_or(0, |terrain| {
                    context.master_data.get_terrain_defense_bonus(terrain)
                });
            let damage = get_expected_damage(
                &actor.stats,
                actor.hp,
                actor.ammo,
                &enemy.stats,
                enemy_defense,
                distance,
                context.master_data,
                context.damage_chart,
                false,
            );
            if damage == 0 {
                continue;
            }
            let key = AttackKey {
                target_remaining_hp: enemy.hp.saturating_sub(damage),
                defensive_position: Reverse(terrain_defense),
                target_hp: enemy.hp,
            };
            if best.as_ref().is_none_or(|(current, _)| key <= *current) {
                // ROM 57E0は比較値が同じ場合も後の行優先候補へ更新する。
                best = Some((
                    key,
                    AiCommand::Attack {
                        target_pos: candidate.position,
                        target_entity: enemy.entity,
                    },
                ));
            }
        }
    }
    best.map(|(_, command)| command)
}

fn attack_movement_allowance(stats: &UnitStats, version: AiVersion) -> u32 {
    match version {
        AiVersion::V100 => stats.max_movement.saturating_sub(1).max(2),
        AiVersion::V200 => stats.max_movement,
        _ => unreachable!("V100/V200専用AI以外から攻撃移動力を参照しました"),
    }
}

fn attack_position_value(
    position: GridPosition,
    map: &Map,
    master_data: &MasterDataRegistry,
    properties: &[(GridPosition, Property)],
    player_id: PlayerId,
    version: AiVersion,
) -> u32 {
    let own_property = properties.iter().any(|(property_position, property)| {
        *property_position == position && property.owner_id == Some(player_id)
    });
    if version == AiVersion::V200 && own_property {
        // ROM 4434はIQ200の副評価で自軍施設を0扱いし、通常地形の防御値を優先する。
        return 0;
    }
    map.get_terrain(position.x, position.y)
        .map_or(0, |terrain| master_data.get_terrain_defense_bonus(terrain))
}

#[allow(clippy::too_many_arguments)]
fn choose_merge(
    actor: &UnitView,
    map: &Map,
    occupants: &HashMap<(usize, usize), OccupantInfo>,
    friendly_by_position: &HashMap<(usize, usize), Entity>,
    world: &World,
    player_id: PlayerId,
    master_data: &MasterDataRegistry,
) -> Option<AiCommand> {
    let candidates = build_merge_candidate_field(
        map,
        occupants,
        actor.position,
        &actor.stats,
        actor.fuel,
        player_id,
        master_data,
    );
    for candidate in candidates {
        let target_entity =
            *friendly_by_position.get(&(candidate.position.x, candidate.position.y))?;
        let target_health = world.get::<Health>(target_entity)?;
        // GB版は99を満タンとして合計119未満だけを合流候補にする。
        // OpenWarsの100刻みへ換算し、片方も損耗していない無意味な合流は除く。
        if (actor.hp < actor.max_hp || target_health.current < target_health.max)
            && actor.hp + target_health.current < 120
        {
            return Some(AiCommand::Merge {
                target_pos: candidate.position,
                target_entity,
            });
        }
    }
    None
}

fn choose_wait(
    actor: &UnitView,
    candidates: &[CandidateTile],
    objective: Option<GridPosition>,
    map: &Map,
    master_data: &MasterDataRegistry,
) -> AiCommand {
    let route_distances = objective
        .map(|objective| build_route_field(map, master_data, objective, actor.stats.movement_type));
    let mut best: Option<(u32, GridPosition)> = None;
    for candidate in candidates {
        let key = objective.map(|objective| {
            route_distances
                .as_ref()
                .and_then(|distances| distances.get(&candidate.position))
                .copied()
                .unwrap_or_else(|| {
                    map.distance(
                        candidate.position.x,
                        candidate.position.y,
                        objective.x,
                        objective.y,
                    )
                })
        });
        if let Some(key) = key
            && best.as_ref().is_none_or(|(current, _)| key <= *current)
        {
            // ROMの目標距離欄は地形込みの経路長である。到達候補自体にはOpenWarsの
            // ZOC制限を適用したうえで、同値なら4BF6の実測どおり後の行優先候補を残す。
            best = Some((key, candidate.position));
        }
    }
    AiCommand::Wait {
        target_pos: best.map_or(actor.position, |(_, position)| position),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::UnitType;

    fn unit(entity: u32, movement_type: MovementType, position: GridPosition) -> UnitView {
        UnitView {
            entity: Entity::from_raw(entity),
            record_order: entity,
            position,
            stats: UnitStats {
                unit_type: UnitType::Infantry,
                movement_type,
                max_movement: 6,
                ..UnitStats::mock()
            },
            hp: 100,
            max_hp: 100,
            fuel: 99,
            ammo: (9, 0),
            cargo_count: 0,
        }
    }

    fn assignment_unit(entity: Entity, position: GridPosition) -> AssignmentUnit {
        AssignmentUnit {
            entity,
            position,
            unit_type: UnitType::Infantry,
            record_order: entity.index(),
            fast_ground: false,
            air_unit: false,
        }
    }

    #[test]
    fn actor_priority_vacates_fast_ground_then_other_property_units() {
        let property_positions = HashSet::from([(2, 2), (3, 2)]);
        let fast = unit(2, MovementType::ArmoredCar, GridPosition { x: 2, y: 2 });
        let infantry = unit(1, MovementType::Infantry, GridPosition { x: 3, y: 2 });
        let field = unit(0, MovementType::Infantry, GridPosition { x: 1, y: 1 });

        assert_eq!(actor_priority(&fast, &property_positions, None, None).0, 2);
        assert_eq!(
            actor_priority(&infantry, &property_positions, None, None).0,
            3
        );
        assert_eq!(actor_priority(&field, &property_positions, None, None).0, 4);
    }

    #[test]
    fn actor_priority_uses_route_cost_and_later_record_for_ties() {
        let property_positions = HashSet::new();
        let earlier = unit(0, MovementType::Infantry, GridPosition { x: 7, y: 5 });
        let later = unit(4, MovementType::Infantry, GridPosition { x: 3, y: 5 });
        let earlier_key = actor_priority(
            &earlier,
            &property_positions,
            Some(GridPosition { x: 7, y: 7 }),
            Some(3),
        );
        let later_key = actor_priority(
            &later,
            &property_positions,
            Some(GridPosition { x: 1, y: 7 }),
            Some(3),
        );

        assert!(later_key < earlier_key);
    }

    #[test]
    fn initial_infantry_receive_distinct_nearest_objectives() {
        let map = Map::new(
            10,
            14,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Hex,
        );
        let units = [
            (Entity::from_raw(0), GridPosition { x: 6, y: 3 }),
            (Entity::from_raw(1), GridPosition { x: 7, y: 4 }),
            (Entity::from_raw(2), GridPosition { x: 6, y: 4 }),
            (Entity::from_raw(3), GridPosition { x: 7, y: 3 }),
            (Entity::from_raw(4), GridPosition { x: 5, y: 3 }),
        ];
        let mut properties: Vec<_> = [1, 3, 5, 7]
            .map(|x| {
                (
                    GridPosition { x, y: 7 },
                    Property::new(crate::resources::Terrain::City, Some(PlayerId(2)), 200),
                )
            })
            .into();
        properties.push((
            GridPosition { x: 4, y: 10 },
            Property::new(crate::resources::Terrain::Factory, Some(PlayerId(2)), 200),
        ));

        let assignments = assign_objectives(&units, &properties, &map, PlayerId(1));

        assert_eq!(
            assignments[&Entity::from_raw(0)],
            GridPosition { x: 7, y: 7 }
        );
        assert_eq!(
            assignments[&Entity::from_raw(1)],
            GridPosition { x: 5, y: 7 }
        );
        assert_eq!(
            assignments[&Entity::from_raw(2)],
            GridPosition { x: 3, y: 7 }
        );
        assert_eq!(
            assignments[&Entity::from_raw(3)],
            GridPosition { x: 4, y: 10 }
        );
        assert_eq!(
            assignments[&Entity::from_raw(4)],
            GridPosition { x: 1, y: 7 }
        );
    }

    #[test]
    fn objectives_remain_stable_while_units_act_sequentially() {
        let map = Map::new(
            10,
            14,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Hex,
        );
        let first = Entity::from_raw(0);
        let second = Entity::from_raw(1);
        let mut properties = vec![
            (
                GridPosition { x: 7, y: 7 },
                Property::new(crate::resources::Terrain::City, Some(PlayerId(2)), 200),
            ),
            (
                GridPosition { x: 5, y: 7 },
                Property::new(crate::resources::Terrain::City, Some(PlayerId(2)), 200),
            ),
        ];
        let mut world = World::new();
        let initial = update_objective_assignments(
            &mut world,
            &[
                assignment_unit(first, GridPosition { x: 6, y: 3 }),
                assignment_unit(second, GridPosition { x: 7, y: 4 }),
            ],
            &properties,
            &map,
            PlayerId(1),
        );
        let after_first_move = update_objective_assignments(
            &mut world,
            &[
                assignment_unit(first, GridPosition { x: 7, y: 5 }),
                assignment_unit(second, GridPosition { x: 7, y: 4 }),
            ],
            &properties,
            &map,
            PlayerId(1),
        );

        assert_eq!(after_first_move, initial);
        assert_eq!(after_first_move[&second], GridPosition { x: 5, y: 7 });

        properties[0].1.owner_id = Some(PlayerId(1));
        let after_ally_capture = update_objective_assignments(
            &mut world,
            &[
                assignment_unit(first, GridPosition { x: 7, y: 5 }),
                assignment_unit(second, GridPosition { x: 5, y: 7 }),
            ],
            &properties,
            &map,
            PlayerId(1),
        );
        assert_eq!(after_ally_capture, initial);
    }

    #[test]
    fn map1_second_wave_fifth_infantry_targets_observed_factory() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_1",
            crate::resources::GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>().clone();
        let properties: Vec<_> = {
            let mut query = world.query::<(&GridPosition, &Property)>();
            query
                .iter(&world)
                .map(|(position, property)| (*position, *property))
                .collect()
        };
        let first_wave = [
            assignment_unit(Entity::from_raw(0), GridPosition { x: 6, y: 3 }),
            assignment_unit(Entity::from_raw(1), GridPosition { x: 7, y: 4 }),
            assignment_unit(Entity::from_raw(2), GridPosition { x: 6, y: 4 }),
            assignment_unit(Entity::from_raw(3), GridPosition { x: 7, y: 3 }),
            assignment_unit(Entity::from_raw(4), GridPosition { x: 5, y: 3 }),
        ];
        update_objective_assignments(&mut world, &first_wave, &properties, &map, PlayerId(1));
        let mut all_units = first_wave.to_vec();
        all_units.extend([
            AssignmentUnit {
                entity: Entity::from_raw(5),
                position: GridPosition { x: 6, y: 3 },
                unit_type: UnitType::Recon,
                record_order: 5,
                fast_ground: true,
                air_unit: false,
            },
            assignment_unit(Entity::from_raw(6), GridPosition { x: 7, y: 4 }),
            assignment_unit(Entity::from_raw(7), GridPosition { x: 6, y: 4 }),
            AssignmentUnit {
                entity: Entity::from_raw(8),
                position: GridPosition { x: 7, y: 3 },
                unit_type: UnitType::Recon,
                record_order: 8,
                fast_ground: true,
                air_unit: false,
            },
            assignment_unit(Entity::from_raw(9), GridPosition { x: 5, y: 3 }),
        ]);

        let assignments =
            update_objective_assignments(&mut world, &all_units, &properties, &map, PlayerId(1));

        assert_eq!(
            assignments[&Entity::from_raw(9)],
            GridPosition { x: 4, y: 10 }
        );
        assert_eq!(
            assignments[&Entity::from_raw(5)],
            GridPosition { x: 3, y: 11 }
        );
    }

    #[test]
    fn wait_uses_later_row_major_candidate_when_distance_is_equal() {
        let map = Map::new(
            10,
            14,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Hex,
        );
        let actor = unit(0, MovementType::Infantry, GridPosition { x: 6, y: 3 });
        let candidates = vec![
            CandidateTile {
                position: GridPosition { x: 6, y: 5 },
                movement_cost: 2,
            },
            CandidateTile {
                position: GridPosition { x: 7, y: 5 },
                movement_cost: 2,
            },
        ];

        assert!(matches!(
            choose_wait(
                &actor,
                &candidates,
                Some(GridPosition { x: 7, y: 7 }),
                &map,
                &MasterDataRegistry::load().unwrap(),
            ),
            AiCommand::Wait {
                target_pos: GridPosition { x: 7, y: 5 }
            }
        ));
    }

    #[test]
    fn wait_avoids_mountain_route_like_white_fifth_infantry() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_1",
            crate::resources::GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>().clone();
        let mut actor = unit(0, MovementType::Infantry, GridPosition { x: 2, y: 11 });
        actor.stats.max_movement = 3;
        let candidates = build_candidate_field(
            &map,
            &HashMap::new(),
            actor.position,
            &actor.stats,
            actor.fuel,
            PlayerId(2),
            &master_data,
        );

        let command = choose_wait(
            &actor,
            &candidates,
            Some(GridPosition { x: 6, y: 4 }),
            &map,
            &master_data,
        );
        let route_field = build_route_field(
            &map,
            &master_data,
            GridPosition { x: 6, y: 4 },
            MovementType::Infantry,
        );
        let candidate_values: Vec<_> = candidates
            .iter()
            .map(|candidate| route_field[&candidate.position])
            .collect();
        assert_eq!(candidate_values, vec![7, 7, 9, 8, 8, 7, 10, 9, 9, 8, 8]);
        assert!(
            matches!(
                command,
                AiCommand::Wait {
                    target_pos: GridPosition { x: 5, y: 10 }
                }
            ),
            "{command:?}"
        );
    }

    #[test]
    fn attack_key_prefers_defended_tile_without_unit_price() {
        let exposed = AttackKey {
            target_remaining_hp: 40,
            defensive_position: Reverse(10),
            target_hp: 70,
        };
        let defended = AttackKey {
            target_remaining_hp: 40,
            defensive_position: Reverse(30),
            target_hp: 70,
        };

        assert!(defended < exposed);
    }

    #[test]
    fn attack_movement_reserves_one_point_like_rom_field() {
        let mut infantry = unit(0, MovementType::Infantry, GridPosition { x: 0, y: 0 });
        let recon = unit(1, MovementType::ArmoredCar, GridPosition { x: 0, y: 0 });

        infantry.stats.max_movement = 3;
        assert_eq!(
            attack_movement_allowance(&infantry.stats, AiVersion::V100),
            2
        );
        assert_eq!(
            attack_movement_allowance(&infantry.stats, AiVersion::V200),
            3
        );
        assert_eq!(attack_movement_allowance(&recon.stats, AiVersion::V100), 5);

        let mut slow = infantry.stats;
        slow.max_movement = 2;
        assert_eq!(attack_movement_allowance(&slow, AiVersion::V100), 2);
    }

    #[test]
    fn iq200_treats_own_property_as_zero_attack_position_value() {
        let master_data = MasterDataRegistry::load().unwrap();
        let position = GridPosition { x: 1, y: 1 };
        let map = Map::new(
            3,
            3,
            crate::resources::Terrain::City,
            crate::resources::GridTopology::Hex,
        );
        let properties = [(
            position,
            Property::new(crate::resources::Terrain::City, Some(PlayerId(1)), 200),
        )];

        assert_eq!(
            attack_position_value(
                position,
                &map,
                &master_data,
                &properties,
                PlayerId(1),
                AiVersion::V200,
            ),
            0
        );
        assert_eq!(
            attack_position_value(
                position,
                &map,
                &master_data,
                &properties,
                PlayerId(1),
                AiVersion::V100,
            ),
            15
        );
    }
}
