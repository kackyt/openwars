//! 自軍の生産施設を塞いでいる編成中（Forming）・待機中ユニットの隣接タイルへの退避判断。
use crate::ai::command::AiCommand;
use crate::ai::context::tactical::has_capturable_property_at;
use crate::ai::squad::{MissionPhase, MissionType, SquadManager};
use crate::collections::{HashMap, HashSet};
use crate::components::{
    ActionCompleted, Faction, GridPosition, HasMoved, PlayerId, Property, UnitStats,
};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Map, Terrain};
use crate::systems::movement::{OccupantInfo, calculate_reachable_tiles};
use bevy_ecs::prelude::*;

/// Forming中または割当待ちの空輸送役が自軍生産施設を塞いだ場合、隣へ退避する。
pub(crate) fn decide_forming_campaign_site_relief(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &HashSet<Entity>,
    include_active_missions: bool,
) -> Option<(Entity, AiCommand)> {
    // 拡張経路では、首都攻略DAGの指令先・セル列と、退避先での占領開始を加える。
    // 退避対象の収集・候補順・通常の退避先選択は両経路で共通。
    let is_extended_path =
        crate::ai::code_path::is_extended_at("engine.rs::decide_forming_campaign_site_relief");

    let map = world.get_resource::<Map>()?.clone();
    let registry = world.get_resource::<MasterDataRegistry>()?.clone();
    let production_positions = owned_production_positions(world, player_id, &map, &registry);
    if production_positions.is_empty() {
        return None;
    }

    let forming_groups = collect_relief_groups(
        world,
        player_id,
        &production_positions,
        include_active_missions,
        is_extended_path,
    )?;
    let (occupied, unit_positions) = collect_unit_occupancy(world);

    for (group, mission_target) in forming_groups {
        let group_positions: Vec<_> = group
            .iter()
            .filter_map(|entity| world.get::<GridPosition>(*entity).copied())
            .collect();
        for entity in &group {
            if let Some(action) = select_relief_action_for_unit(
                world,
                player_id,
                *entity,
                mission_target,
                &group_positions,
                skip_entities,
                &production_positions,
                &occupied,
                &unit_positions,
                &map,
                &registry,
                is_extended_path,
            ) {
                return Some(action);
            }
        }
    }
    None
}

/// 自軍首都の生産圏内にある自軍生産施設の座標一覧を返す。
fn owned_production_positions(
    world: &mut World,
    player_id: PlayerId,
    map: &Map,
    registry: &MasterDataRegistry,
) -> HashSet<(usize, usize)> {
    let capital_positions: Vec<_> = world
        .query::<(&GridPosition, &Property)>()
        .iter(world)
        .filter_map(|(position, property)| {
            (property.owner_id == Some(player_id) && property.terrain == Terrain::Capital)
                .then_some(*position)
        })
        .collect();
    world
        .query::<(&GridPosition, &Property)>()
        .iter(world)
        .filter_map(|(position, property)| {
            (property.owner_id == Some(player_id)
                && registry.is_production_facility(property.terrain.as_str())
                && crate::systems::production::is_within_production_range(
                    &capital_positions,
                    position.x,
                    position.y,
                    map.topology,
                ))
            .then_some((position.x, position.y))
        })
        .collect()
}

/// 生産施設から退避させる対象グループ（構成Entity群と目標座標）を優先順に収集する。
fn collect_relief_groups(
    world: &mut World,
    player_id: PlayerId,
    production_positions: &HashSet<(usize, usize)>,
    include_active_missions: bool,
    is_extended_path: bool,
) -> Option<Vec<(Vec<Entity>, Option<GridPosition>)>> {
    let manager = world.get_resource::<SquadManager>()?;
    let mut forming_groups: Vec<(Vec<Entity>, Option<GridPosition>)> = manager
        .squads
        .iter()
        .filter(|squad| {
            squad.plan.owner_id == Some(player_id)
                && squad.plan.mission_type == MissionType::Transport
                && squad.plan.phase == MissionPhase::Forming
        })
        .map(|squad| {
            let mut entities: Vec<_> = squad
                .transport_entity
                .iter()
                .chain(squad.cargo_entities.iter())
                .copied()
                .collect();
            entities.sort_by_key(|entity| entity.to_bits());
            entities.dedup();
            (entities, squad.plan.target)
        })
        .collect();
    // 局地護衛待ちの占領兵も、生産施設上では待たせない。任務目標へは進めず、
    // 隣接する非生産マスへ1歩退避して、次手番の護衛生産枠を開ける。
    forming_groups.extend(
        manager
            .squads
            .iter()
            .filter(|squad| {
                squad.plan.owner_id == Some(player_id)
                    && squad.plan.mission_type == MissionType::Capture
                    && !squad.plan.departure_authorized
            })
            .map(|squad| {
                (
                    squad.members.iter().copied().collect::<Vec<_>>(),
                    squad.plan.target,
                )
            }),
    );
    if include_active_missions {
        forming_groups.extend(
            manager
                .squads
                .iter()
                .filter(|squad| {
                    squad.plan.owner_id == Some(player_id)
                        && squad.plan.mission_type != MissionType::Transport
                        && (squad.plan.mission_type != MissionType::Capture
                            || squad.plan.departure_authorized)
                })
                .map(|squad| {
                    // 通常executorより先に工場を空ける場合も、DAG orderがあれば
                    // 古いSquad.targetではなく現在Nodeの指令先を使う。
                    let route_target = is_extended_path
                        .then(|| {
                            squad.members.iter().find_map(|entity| {
                                crate::ai::v4::capital_route_tactical_target(
                                    world, player_id, *entity,
                                )
                            })
                        })
                        .flatten();
                    (
                        squad.members.iter().copied().collect::<Vec<_>>(),
                        route_target.unwrap_or(squad.plan.target),
                    )
                }),
        );
    }
    let squad_entities = manager
        .squads
        .iter()
        .flat_map(|squad| {
            squad
                .members
                .iter()
                .chain(squad.cargo_entities.iter())
                .chain(squad.delivered_cargo.iter())
                .copied()
        })
        .collect::<HashSet<_>>();
    // portfolioから外れた直後など、owner解放とSquad再構築の境界にいる空輸送役も
    // 空港上で永久停止させない。退避だけなら別前線への任務変更にはならない。
    let mut unassigned_blockers = world
        .query::<(
            Entity,
            &Faction,
            &GridPosition,
            &UnitStats,
            Option<&crate::components::CargoCapacity>,
            Option<&crate::components::Transporting>,
        )>()
        .iter(world)
        .filter_map(|(entity, faction, position, stats, cargo, transporting)| {
            (faction.0 == player_id
                && stats.max_cargo > 0
                && transporting.is_none()
                && cargo.is_some_and(|capacity| capacity.loaded.is_empty())
                && production_positions.contains(&(position.x, position.y))
                && !squad_entities.contains(&entity))
            .then_some(entity)
        })
        .collect::<Vec<_>>();
    unassigned_blockers.sort_by_key(|entity| entity.to_bits());
    forming_groups.extend(
        unassigned_blockers
            .into_iter()
            .map(|entity| (vec![entity], None)),
    );
    forming_groups
        .sort_by_key(|(entities, _)| entities.first().map_or(u64::MAX, |entity| entity.to_bits()));
    Some(forming_groups)
}

/// 移動計算用の占有マップ（座標 → 占有ユニット情報）。
type OccupantMap = HashMap<(usize, usize), OccupantInfo>;

/// 盤面上の非搭載ユニットの占有座標集合と移動計算用の占有マップを構築する。
fn collect_unit_occupancy(world: &World) -> (HashSet<(usize, usize)>, OccupantMap) {
    let mut occupied = HashSet::default();
    let mut unit_positions = HashMap::default();
    for entity_ref in world.iter_entities() {
        if entity_ref
            .get::<crate::components::Transporting>()
            .is_some()
        {
            continue;
        }
        let (Some(position), Some(faction), Some(stats)) = (
            entity_ref.get::<GridPosition>(),
            entity_ref.get::<Faction>(),
            entity_ref.get::<UnitStats>(),
        ) else {
            continue;
        };
        let free_slots = entity_ref
            .get::<crate::components::CargoCapacity>()
            .map(|capacity| {
                capacity
                    .max
                    .saturating_sub(u32::try_from(capacity.loaded.len()).unwrap_or(u32::MAX))
            })
            .unwrap_or(0);
        occupied.insert((position.x, position.y));
        unit_positions.insert(
            (position.x, position.y),
            OccupantInfo {
                player_id: faction.0,
                is_transport: stats.max_cargo > 0,
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots,
            },
        );
    }
    (occupied, unit_positions)
}

/// 1体のユニットが生産施設を塞いでいる場合、退避先タイルとコマンドを選択する。
#[allow(clippy::too_many_arguments)]
fn select_relief_action_for_unit(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    mission_target: Option<GridPosition>,
    group_positions: &[GridPosition],
    skip_entities: &HashSet<Entity>,
    production_positions: &HashSet<(usize, usize)>,
    occupied: &HashSet<(usize, usize)>,
    unit_positions: &OccupantMap,
    map: &Map,
    registry: &MasterDataRegistry,
    is_extended_path: bool,
) -> Option<(Entity, AiCommand)> {
    if skip_entities.contains(&entity)
        || world
            .get::<Faction>(entity)
            .is_none_or(|faction| faction.0 != player_id)
        || world.get::<HasMoved>(entity).is_none_or(|moved| moved.0)
        || world
            .get::<ActionCompleted>(entity)
            .is_none_or(|action| action.0)
        || world
            .get::<crate::components::Transporting>(entity)
            .is_some()
    {
        return None;
    }
    let position = *world.get::<GridPosition>(entity)?;
    if !production_positions.contains(&(position.x, position.y)) {
        return None;
    }
    let stats = world.get::<UnitStats>(entity)?;
    let fuel = world
        .get::<crate::components::Fuel>(entity)
        .map_or(u32::MAX, |fuel| fuel.current);
    let reachable = calculate_reachable_tiles(
        map,
        unit_positions,
        (position.x, position.y),
        stats.movement_type,
        stats.max_movement,
        fuel,
        player_id,
        stats.unit_type,
        registry,
    );
    let is_valid_destination = |tile: &(usize, usize)| {
        *tile != (position.x, position.y)
            && !occupied.contains(tile)
            && !production_positions.contains(tile)
    };
    // V4のDAG所属unitは、工場退避でも同じセル列を進む。単なる目標距離で
    // 選ぶと、map_25で敵側Nodeとは逆の旧Squad.targetへ初手だけ逸脱する。
    let route_destination = is_extended_path
        .then(|| {
            crate::ai::v4::capital_route_advance_destination(world, player_id, entity, &reachable)
        })
        .flatten()
        .map(|target| (target.x, target.y))
        .filter(is_valid_destination);
    let destination = route_destination.or_else(|| {
        reachable
            .iter()
            .copied()
            .filter(is_valid_destination)
            .min_by_key(|(x, y)| {
                let target_distance =
                    mission_target.map_or(0, |target| map.distance(*x, *y, target.x, target.y));
                let group_distance = group_positions.iter().fold(0_u32, |total, member| {
                    total.saturating_add(map.distance(*x, *y, member.x, member.y))
                });
                (target_distance, group_distance, *y, *x)
            })
    })?;
    let target_pos = GridPosition {
        x: destination.0,
        y: destination.1,
    };
    // 拡張経路では、退避先が占領可能な物件ならその場で占領を始める。
    // 首都攻略DAGの占領許可（capital_route_allows_capture_at）は旧拡張実装も
    // 確認していない。退避は生産口を空ける行動であり、追加すると拡張経路の
    // 挙動が変わるため、要否は別途の比較で判断する。
    let starts_capture = is_extended_path
        && stats.can_capture
        && has_capturable_property_at(world, target_pos, player_id);
    let command = if starts_capture {
        AiCommand::Capture { target_pos }
    } else {
        AiCommand::Wait { target_pos }
    };
    Some((entity, command))
}
