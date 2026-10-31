//! 陸続きルートの兵力需要と突破実績。

use crate::ai::v4::capital_route::model::{
    RouteBreakthroughKey, RouteBreakthroughRegistry, RouteForceDemand,
};
use crate::ai::v4::capital_route::node_operation::capital_route_dag_all_properties;
use crate::ai::v4::capital_route::topology::{
    armored_entry_bridge_gates, armored_step_distances, capital_route_topology_for,
    movement_step_distances, same_land_armored_route_gates, same_land_route_fronts,
    terrain_component_membership,
};
use crate::ai::v4::trace::campaign_scope_audit_enabled;
use crate::ai::v4::{
    CampaignPlanningObjective, Entity, Faction, GridPosition, HashMap, HashSet, Health, Map,
    MasterDataRegistry, MovementType, OperationKind, PlayerId, Property, Terrain, UnitRegistry,
    UnitSnapshot, UnitStats, UnitType, World, assignment, get_valid_movement_cost,
    property_control,
};

/// 地形・経済・現有戦力から、首都攻略の各進軍軸の需要を一括で作る。
///
/// 計算量は盤面走査と `route数 × unit数` だけで、route数は分岐数に抑えられる。
/// したがって、増援候補を一体ずつ仮配属して価値を再計算する方式にはならない。
pub(crate) fn same_land_route_force_demands(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Vec<RouteForceDemand> {
    let fronts = same_land_route_fronts(world, player_id, island_id);
    if fronts.len() < 2 {
        return vec![
            RouteForceDemand {
                opportunity_value: 0,
                force_deficit_value: 0,
                demand_value: 0,
            };
            fronts.len()
        ];
    }
    let Some(map) = world.get_resource::<Map>() else {
        return Vec::new();
    };
    let Some(master_data) = world.get_resource::<MasterDataRegistry>() else {
        return Vec::new();
    };
    let Some(unit_registry) = world.get_resource::<UnitRegistry>() else {
        return Vec::new();
    };
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
        .unwrap_or_else(|| crate::ai::islands::IslandMap::analyze(map));
    let own_capital = world.iter_entities().find_map(|entity| {
        let position = entity.get::<GridPosition>()?;
        let property = entity.get::<Property>()?;
        (property.terrain == Terrain::Capital
            && property.owner_id == Some(player_id)
            && island_map
                .get_island_at(position)
                .is_some_and(|island| island.id == island_id))
        .then_some(*position)
    });
    let enemy_capital = world.iter_entities().find_map(|entity| {
        let position = entity.get::<GridPosition>()?;
        let property = entity.get::<Property>()?;
        (property.terrain == Terrain::Capital
            && property.owner_id.is_some_and(|owner| owner != player_id)
            && island_map
                .get_island_at(position)
                .is_some_and(|island| island.id == island_id))
        .then_some(*position)
    });
    let (Some(own_capital), Some(enemy_capital)) = (own_capital, enemy_capital) else {
        return Vec::new();
    };
    let from_home = armored_step_distances(map, master_data, own_capital);
    let from_capital = armored_step_distances(map, master_data, enemy_capital);
    let capital_steps = fronts
        .iter()
        .map(|front| from_capital.get(front).copied().unwrap_or(u32::MAX / 4))
        .collect::<Vec<_>>();
    // 首都までの最長軸を戦役の残り時間幅とみなす。早く取れる施設ほど、その後の
    // 手番で収益・生産を活かせるため、地形到達性を機会価値へ自然に反映できる。
    let campaign_horizon = capital_steps
        .iter()
        .copied()
        .filter(|distance| *distance < u32::MAX / 4)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let nearest_front = |position: GridPosition| {
        fronts
            .iter()
            .enumerate()
            .min_by_key(|(index, front)| {
                (
                    map.distance(position.x, position.y, front.x, front.y),
                    *index,
                )
            })
            .map_or(0, |(index, _)| index)
    };
    let mut opportunity_values = vec![0_u64; fronts.len()];
    let mut defended_asset_values = vec![0_u64; fronts.len()];
    let mut friendly_values = vec![0_u64; fronts.len()];
    let mut enemy_values = vec![0_u64; fronts.len()];
    let mut capital_threat_values = vec![0_u64; fronts.len()];

    let topology = capital_route_topology_for(world, player_id, island_id);
    for entity in world.iter_entities() {
        let (Some(position), Some(property)) =
            (entity.get::<GridPosition>(), entity.get::<Property>())
        else {
            continue;
        };
        if property.terrain == Terrain::Capital
            || island_map
                .get_island_at(position)
                .is_none_or(|island| island.id != island_id)
        {
            continue;
        }
        let route = topology
            .and_then(|t| t.property_routes.get(position))
            .map(|r| r.0)
            .filter(|r| *r < fronts.len())
            .unwrap_or_else(|| nearest_front(*position));
        let arrival = from_home.get(position).copied().unwrap_or(u32::MAX / 4);
        if arrival >= u32::MAX / 4 {
            continue;
        }
        let income = master_data.landscape_income(property.terrain.as_str()) as u64;
        // 施設は占領後、毎手番に地上戦力を供給しうる。実際に生産可能な地上unitの
        // 最安コストを用いることで、地形名ごとの固定ボーナスを置かない。
        let production_value = unit_registry
            .0
            .iter()
            .filter(|(unit_type, stats)| {
                !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
                    && master_data.can_produce_unit(property.terrain.as_str(), **unit_type)
            })
            .map(|(_, stats)| stats.cost as u64)
            .min()
            .unwrap_or(0);
        let remaining_turns = campaign_horizon.saturating_sub(arrival).saturating_add(1) as u64;
        let property_value = income
            .saturating_add(production_value)
            .saturating_mul(remaining_turns);

        if property.owner_id == Some(player_id) {
            // 自軍所有物件の防衛資産価値: その進軍路が守るべき前線・回廊の自陣物件（都市・工場等）。
            // 自首都直近（arrival <= 2）の本拠地施設は特定の進軍路に偏らせないため除外する。
            if arrival > 2 {
                defended_asset_values[route] =
                    defended_asset_values[route].saturating_add(property_value);
            }
        } else {
            // 未所有・敵物件の攻略機会価値
            opportunity_values[route] = opportunity_values[route].saturating_add(property_value);
        }
    }

    for entity in world.iter_entities() {
        let (Some(faction), Some(position), Some(stats)) = (
            entity.get::<Faction>(),
            entity.get::<GridPosition>(),
            entity.get::<UnitStats>(),
        ) else {
            continue;
        };
        if matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
            || island_map
                .get_island_at(position)
                .is_none_or(|island| island.id != island_id)
        {
            continue;
        }
        let route = nearest_front(*position);
        let health = entity.get::<Health>().map_or(100, |health| health.current);
        let max_health = entity
            .get::<Health>()
            .map_or(100, |health| health.max.max(1));
        let current_value = (stats.cost as u64)
            .saturating_mul(health as u64)
            .saturating_div(max_health as u64);
        if faction.0 == player_id {
            friendly_values[route] = friendly_values[route].saturating_add(current_value);
        } else {
            enemy_values[route] = enemy_values[route].saturating_add(current_value);
            // 自首都への距離に応じた首都防衛需要（Capital Threat）
            let dist_to_capital = from_home.get(position).copied().unwrap_or(u32::MAX / 4);
            if dist_to_capital < campaign_horizon {
                let urgency = campaign_horizon
                    .saturating_sub(dist_to_capital)
                    .saturating_add(1) as u64;
                capital_threat_values[route] = capital_threat_values[route]
                    .saturating_add(current_value.saturating_mul(urgency));
            }
        }
    }
    opportunity_values
        .into_iter()
        .enumerate()
        .map(|(route, opportunity_value)| {
            let defended_value = defended_asset_values[route];
            let force_deficit_value = enemy_values[route].saturating_sub(friendly_values[route]);
            let capital_threat = capital_threat_values[route];
            RouteForceDemand {
                opportunity_value,
                force_deficit_value,
                demand_value: opportunity_value
                    .saturating_add(defended_value)
                    .saturating_add(force_deficit_value)
                    .saturating_add(capital_threat),
            }
        })
        .collect()
}

/// 各routeの占領vanguardが橋頭堡へ到達する予定と、新規tankの到達予定を比較する。
///
/// まだvanguardが遠い段階でtankを先買いすると、初期資金を占領・収入へ回せない。
/// 逆にvanguardが先着する見込みなら、橋手前で待たせないようtankを構造要求にする。
/// unitごとの反実仮想は行わず、移動種別ごとにgateから盤面を一度走査するだけである。
#[cfg(test)]
pub(in crate::ai::v4) fn same_land_route_entry_ready(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    gates: &[GridPosition],
) -> Vec<bool> {
    let Some(map) = world.get_resource::<Map>() else {
        return vec![false; gates.len()];
    };
    let Some(master_data) = world.get_resource::<MasterDataRegistry>() else {
        return vec![false; gates.len()];
    };
    let Some(unit_registry) = world.get_resource::<UnitRegistry>() else {
        return vec![false; gates.len()];
    };
    let Some(tank_stats) = unit_registry.0.get(&UnitType::Tank) else {
        return vec![false; gates.len()];
    };
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
        .unwrap_or_else(|| crate::ai::islands::IslandMap::analyze(map));
    let mut armor_arrival = vec![u32::MAX; gates.len()];
    // 工場ごとに探索を繰り返さず、Gate 起点の Tank 到達表を route 数ぶんだけ作る。
    let tank_paths: Vec<_> = gates
        .iter()
        .map(|gate| armored_step_distances(map, master_data, *gate))
        .collect();
    for entity in world.iter_entities() {
        let (Some(position), Some(property)) =
            (entity.get::<GridPosition>(), entity.get::<Property>())
        else {
            continue;
        };
        if property.owner_id != Some(player_id)
            || !master_data.can_produce_unit(property.terrain.as_str(), UnitType::Tank)
            || island_map
                .get_island_at(position)
                .is_none_or(|island| island.id != island_id)
        {
            continue;
        }
        for (route, distances) in tank_paths.iter().enumerate() {
            let turns = distances
                .get(position)
                .map(|steps| {
                    steps.saturating_add(tank_stats.max_movement.max(1) - 1)
                        / tank_stats.max_movement.max(1)
                })
                // 距離1は次手番の移動で到着する。生産待機と最初の移動を二重計上しない。
                .map(|turns| property_control::produced_action_offset(0, turns));
            armor_arrival[route] = armor_arrival[route].min(turns.unwrap_or(u32::MAX));
        }
    }

    let mut vanguard_arrival = vec![u32::MAX; gates.len()];
    let mut paths_by_movement: Vec<(MovementType, Vec<HashMap<GridPosition, u32>>)> = Vec::new();
    for entity in world.iter_entities() {
        let (Some(faction), Some(position), Some(stats)) = (
            entity.get::<Faction>(),
            entity.get::<GridPosition>(),
            entity.get::<UnitStats>(),
        ) else {
            continue;
        };
        if faction.0 != player_id
            || !stats.can_capture
            || matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
            || island_map
                .get_island_at(position)
                .is_none_or(|island| island.id != island_id)
        {
            continue;
        }
        let movement = stats.movement_type;
        let paths_index = if let Some(index) = paths_by_movement
            .iter()
            .position(|(cached_movement, _)| *cached_movement == movement)
        {
            index
        } else {
            paths_by_movement.push((
                movement,
                gates
                    .iter()
                    .map(|gate| movement_step_distances(map, master_data, *gate, movement))
                    .collect(),
            ));
            paths_by_movement.len().saturating_sub(1)
        };
        // 一体の占領要員を全 route の先遣として二重計上しない。最も早く着く
        // Gate だけへ帰属させることで、編成の実際の進行方向を phase 判定へ反映する。
        let arrival = paths_by_movement[paths_index]
            .1
            .iter()
            .enumerate()
            .filter_map(|(route, distances)| {
                distances.get(position).map(|steps| {
                    (
                        steps.saturating_add(stats.max_movement.max(1) - 1)
                            / stats.max_movement.max(1),
                        route,
                    )
                })
            })
            .min_by_key(|(turns, route)| (*turns, *route));
        if let Some((turns, route)) = arrival {
            vanguard_arrival[route] = vanguard_arrival[route].min(turns);
        }
    }

    vanguard_arrival
        .into_iter()
        .zip(armor_arrival)
        .map(|(vanguard, armor)| vanguard < u32::MAX && vanguard <= armor)
        .collect()
}

/// 未突破の橋を越える装甲戦力について、盤面から導く不足数を返す。
///
/// これは「各routeへ常に一体」の規則ではない。橋でのみ装甲移動が分断され、かつ
/// その向こうに需要があり、占領vanguard と新造Tankの到達予定が交差し、当該routeへ
/// 既存の装甲戦力が観測できない場合だけ不足とする。
/// 橋頭堡を確保したrouteは直ちに対象外になるため、固定した二正面編成にはならない。
#[cfg(test)]
pub(crate) fn same_land_armored_entry_shortfall(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> u32 {
    let gates = same_land_armored_route_gates(world, player_id, island_id);
    if gates.len() < 2 {
        return 0;
    }
    let pending_bridgeheads = same_land_pending_bridgeheads(world, player_id, island_id);
    let demands = same_land_route_force_demands(world, player_id, island_id);
    let entry_ready = same_land_route_entry_ready(world, player_id, island_id, &gates);
    let Some(map) = world.get_resource::<Map>() else {
        return 0;
    };
    let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() else {
        return 0;
    };
    let mut armored_by_route = vec![0_u32; gates.len()];
    for entity in world.iter_entities() {
        let (Some(faction), Some(position), Some(stats)) = (
            entity.get::<Faction>(),
            entity.get::<GridPosition>(),
            entity.get::<UnitStats>(),
        ) else {
            continue;
        };
        if faction.0 != player_id
            || stats.movement_type != MovementType::Tank
            || island_map
                .get_island_at(position)
                .is_none_or(|island| island.id != island_id)
        {
            continue;
        }
        let route = gates
            .iter()
            .enumerate()
            .min_by_key(|(route, gate)| {
                (map.distance(position.x, position.y, gate.x, gate.y), *route)
            })
            .map_or(0, |(route, _)| route);
        armored_by_route[route] = armored_by_route[route].saturating_add(1);
    }
    pending_bridgeheads
        .iter()
        .enumerate()
        .filter(|(route, pending)| {
            pending.is_some()
                && demands
                    .get(*route)
                    .is_some_and(|demand| demand.demand_value > 0)
                && entry_ready.get(*route).copied().unwrap_or(false)
                && armored_by_route.get(*route).copied().unwrap_or(0) == 0
        })
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

/// 戦役全体の需要ベクトルを、有限の戦力枠へ最大剰余方式で一括配分する。
///
/// 各unitの反実仮想評価は行わず、route数分の除算だけで済む。需要がゼロの軸へ
/// 最低1枠を強制しないため、兵力比率は盤面の状態からだけ決まる。
pub(crate) fn apportion_route_force_slots(
    slot_count: usize,
    demands: &[RouteForceDemand],
) -> Vec<usize> {
    let total_demand = demands
        .iter()
        .map(|demand| demand.demand_value)
        .sum::<u64>();
    let mut slots = vec![0_usize; demands.len()];
    if slot_count == 0 || total_demand == 0 {
        return slots;
    }
    let mut remainders = Vec::with_capacity(demands.len());
    let mut allocated = 0_usize;
    for (route, demand) in demands.iter().enumerate() {
        let numerator = (slot_count as u64).saturating_mul(demand.demand_value);
        let base = numerator / total_demand;
        slots[route] = usize::try_from(base).unwrap_or(slot_count);
        allocated = allocated.saturating_add(slots[route]);
        remainders.push((numerator % total_demand, route));
    }
    remainders.sort_unstable_by_key(|(remainder, route)| (std::cmp::Reverse(*remainder), *route));
    for (_, route) in remainders
        .into_iter()
        .take(slot_count.saturating_sub(allocated))
    {
        slots[route] = slots[route].saturating_add(1);
    }
    slots
}

/// 各橋について、橋を通過した直後に立てる敵側の非橋マスを返す。
/// Gate到着は突破ではないため、ここへ到達して初めて「渡河済み」と判断できる。
/// 各橋について、まだ味方地上部隊が橋向こうの地形連結成分へ入っていない時だけ
/// 突破地点を返す。橋への到着、または橋手前の施設の占領では完了にしない。
pub(crate) fn same_land_pending_bridgeheads(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Vec<Option<GridPosition>> {
    let Some(map) = world.get_resource::<Map>() else {
        return Vec::new();
    };
    let Some(master_data) = world.get_resource::<MasterDataRegistry>() else {
        return Vec::new();
    };
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
        .unwrap_or_else(|| crate::ai::islands::IslandMap::analyze(map));
    let mut own_capitals = Vec::new();
    let mut enemy_capitals = Vec::new();
    for entity in world.iter_entities() {
        let (Some(position), Some(property)) =
            (entity.get::<GridPosition>(), entity.get::<Property>())
        else {
            continue;
        };
        if property.terrain != Terrain::Capital
            || island_map
                .get_island_at(position)
                .is_none_or(|island| island.id != island_id)
        {
            continue;
        }
        if property.owner_id == Some(player_id) {
            own_capitals.push(*position);
        } else if property.owner_id.is_some() {
            enemy_capitals.push(*position);
        }
    }
    own_capitals.sort_unstable_by_key(|position| (position.y, position.x));
    enemy_capitals.sort_unstable_by_key(|position| (position.y, position.x));
    let (Some(home), Some(enemy_capital)) = (own_capitals.first(), enemy_capitals.first()) else {
        return Vec::new();
    };
    let gates = armored_entry_bridge_gates(map, master_data, *home, *enemy_capital);
    if gates.len() < 2 {
        return Vec::new();
    }

    let maneuver_cells = (0..map.height)
        .flat_map(|y| (0..map.width).map(move |x| GridPosition { x, y }))
        .filter(|position| {
            map.get_terrain(position.x, position.y)
                .is_some_and(|terrain| {
                    terrain != Terrain::Bridge
                        && get_valid_movement_cost(master_data, MovementType::Tank, terrain)
                            .is_some()
                })
        })
        .collect::<HashSet<_>>();
    let membership = terrain_component_membership(map, &maneuver_cells);
    let Some(source_component) = membership.get(home).copied() else {
        return Vec::new();
    };
    let from_home = armored_step_distances(map, master_data, *home);
    let from_enemy = armored_step_distances(map, master_data, *enemy_capital);

    gates
        .into_iter()
        .map(|gate| {
            let gate_distance = from_home.get(&gate).copied()?;
            let bridgehead = map
                .get_adjacent(gate.x, gate.y)
                .into_iter()
                .map(|(x, y)| GridPosition { x, y })
                .filter(|position| map.get_terrain(position.x, position.y) != Some(Terrain::Bridge))
                .filter_map(|position| {
                    let component = membership.get(&position).copied()?;
                    (component != source_component
                        && from_home
                            .get(&position)
                            .is_some_and(|distance| *distance > gate_distance))
                    .then_some(position)
                })
                .min_by_key(|position| {
                    (
                        from_enemy.get(position).copied().unwrap_or(u32::MAX),
                        position.y,
                        position.x,
                    )
                })?;
            let crossed_this_turn = world.iter_entities().any(|entity| {
                let (Some(faction), Some(position), Some(stats)) = (
                    entity.get::<Faction>(),
                    entity.get::<GridPosition>(),
                    entity.get::<UnitStats>(),
                ) else {
                    return false;
                };
                faction.0 == player_id
                    && !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
                    && *position == bridgehead
            });
            let crossed_previously = world
                .get_resource::<RouteBreakthroughRegistry>()
                .and_then(|registry| registry.crossed.get(&player_id))
                .is_some_and(|crossed| crossed.contains(&RouteBreakthroughKey { island_id, gate }));
            (!(crossed_this_turn || crossed_previously)).then_some(bridgehead)
        })
        .collect()
}

/// 手番開始時に橋頭堡上の地上部隊を観測し、通過MilestoneをGate単位で確定する。
/// 突破後に部隊が前進しても再び橋手前へ引き戻さないための永続観測である。
pub(crate) fn observe_same_land_route_bridgeheads(
    world: &mut World,
    player_id: PlayerId,
    assignments: &[crate::ai::island_campaign::IslandCampaignAssignment],
) {
    let islands = assignments
        .iter()
        .filter(|assignment| {
            assignment.decision != crate::ai::island_campaign::IslandCampaignDecision::Defend
        })
        .map(|assignment| assignment.island_id)
        .collect::<HashSet<_>>();
    let mut observed = HashSet::default();
    for island_id in islands {
        let gates = same_land_armored_route_gates(world, player_id, island_id);
        let bridgeheads = same_land_route_breakthrough_positions(world, player_id, island_id);
        for (gate, bridgehead) in gates.into_iter().zip(bridgeheads) {
            let occupied = world.iter_entities().any(|entity| {
                let (Some(faction), Some(position), Some(stats)) = (
                    entity.get::<Faction>(),
                    entity.get::<GridPosition>(),
                    entity.get::<UnitStats>(),
                ) else {
                    return false;
                };
                faction.0 == player_id
                    && !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
                    && *position == bridgehead
            });
            if occupied {
                observed.insert(RouteBreakthroughKey { island_id, gate });
            }
        }
    }
    if observed.is_empty() {
        return;
    }
    if let Some(mut registry) = world.get_resource_mut::<RouteBreakthroughRegistry>() {
        registry
            .crossed
            .entry(player_id)
            .or_default()
            .extend(observed);
    } else {
        world.insert_resource(RouteBreakthroughRegistry {
            crossed: HashMap::from_iter([(player_id, observed)]),
        });
    }
}

pub(crate) fn same_land_route_breakthrough_positions(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Vec<GridPosition> {
    let Some(map) = world.get_resource::<Map>() else {
        return Vec::new();
    };
    let Some(master_data) = world.get_resource::<MasterDataRegistry>() else {
        return Vec::new();
    };
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
        .unwrap_or_else(|| crate::ai::islands::IslandMap::analyze(map));
    let mut own_capitals = Vec::new();
    let mut enemy_capitals = Vec::new();
    for entity in world.iter_entities() {
        let (Some(position), Some(property)) =
            (entity.get::<GridPosition>(), entity.get::<Property>())
        else {
            continue;
        };
        if property.terrain != Terrain::Capital
            || island_map
                .get_island_at(position)
                .is_none_or(|island| island.id != island_id)
        {
            continue;
        }
        if property.owner_id == Some(player_id) {
            own_capitals.push(*position);
        } else if property.owner_id.is_some() {
            enemy_capitals.push(*position);
        }
    }
    own_capitals.sort_unstable_by_key(|position| (position.y, position.x));
    enemy_capitals.sort_unstable_by_key(|position| (position.y, position.x));
    let (Some(home), Some(enemy_capital)) = (own_capitals.first(), enemy_capitals.first()) else {
        return Vec::new();
    };
    let gates = armored_entry_bridge_gates(map, master_data, *home, *enemy_capital);
    if gates.len() < 2 {
        return Vec::new();
    }
    let from_home = armored_step_distances(map, master_data, *home);
    let from_enemy = armored_step_distances(map, master_data, *enemy_capital);
    gates
        .into_iter()
        .filter_map(|gate| {
            let gate_distance = from_home.get(&gate).copied()?;
            map.get_adjacent(gate.x, gate.y)
                .into_iter()
                .map(|(x, y)| GridPosition { x, y })
                .filter(|position| map.get_terrain(position.x, position.y) != Some(Terrain::Bridge))
                .filter(|position| {
                    from_home
                        .get(position)
                        .is_some_and(|distance| *distance > gate_distance)
                })
                .min_by_key(|position| {
                    (
                        from_enemy.get(position).copied().unwrap_or(u32::MAX),
                        position.y,
                        position.x,
                    )
                })
        })
        .collect()
}

/// 同一陸塊で分かれた各進軍軸について、現在位置から次に占領すべき施設を選ぶ。
/// 山回廊も橋Gateと同じCampaign軸として扱い、全軸へ最低一件を残したうえで
/// 主攻軸へ余剰の占領兵を寄せる。施設を得た次の手番には次の施設へ進むため、
/// 局地Milestoneは成功時に自然消滅して更新される。
pub(crate) fn refine_same_land_route_milestones(
    world: &World,
    player_id: PlayerId,
    portfolio: &mut crate::ai::island_campaign::IslandCampaignPortfolio,
) {
    let Some(map) = world.get_resource::<Map>() else {
        return;
    };
    let Some(master_data) = world.get_resource::<MasterDataRegistry>() else {
        return;
    };
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
        .unwrap_or_else(|| crate::ai::islands::IslandMap::analyze(map));
    let own_capital = world.iter_entities().find_map(|entity| {
        let position = entity.get::<GridPosition>()?;
        let property = entity.get::<Property>()?;
        (property.terrain == Terrain::Capital && property.owner_id == Some(player_id))
            .then_some(*position)
    });
    let Some(own_capital) = own_capital else {
        return;
    };

    for assignment in portfolio.active_offensives.iter_mut().filter(|assignment| {
        assignment.decision != crate::ai::island_campaign::IslandCampaignDecision::Defend
    }) {
        let route_fronts = same_land_route_fronts(world, player_id, assignment.island_id);
        if route_fronts.len() < 2 {
            continue;
        }
        let route_demands = same_land_route_force_demands(world, player_id, assignment.island_id);
        let from_home = armored_step_distances(map, master_data, own_capital);
        let mut route_properties =
            capital_route_dag_all_properties(world, player_id, assignment.island_id)
                .unwrap_or_else(|| {
                    // Resource未初期化の単体テストなどでは、従来の地形解析へフォールバックする。
                    let nearest_front = |position: GridPosition| {
                        route_fronts
                            .iter()
                            .enumerate()
                            .min_by_key(|(index, front)| {
                                (
                                    map.distance(position.x, position.y, front.x, front.y),
                                    *index,
                                )
                            })
                            .map_or(0, |(index, _)| index)
                    };
                    let mut properties = vec![Vec::new(); route_fronts.len()];
                    for entity in world.iter_entities() {
                        let (Some(position), Some(property)) =
                            (entity.get::<GridPosition>(), entity.get::<Property>())
                        else {
                            continue;
                        };
                        if property.owner_id == Some(player_id)
                            || island_map
                                .get_island_at(position)
                                .is_none_or(|island| island.id != assignment.island_id)
                        {
                            continue;
                        }
                        properties[nearest_front(*position)].push(*position);
                    }
                    properties
                });
        let gates = same_land_armored_route_gates(world, player_id, assignment.island_id);
        let crossed_gates = world
            .get_resource::<RouteBreakthroughRegistry>()
            .and_then(|registry| registry.crossed.get(&player_id))
            .cloned()
            .unwrap_or_default();

        let has_any_before_gate = route_properties.iter().enumerate().any(|(route, props)| {
            if let Some(gate) = gates.get(route) {
                let gate_key = RouteBreakthroughKey {
                    island_id: assignment.island_id,
                    gate: *gate,
                };
                if !crossed_gates.contains(&gate_key)
                    && let Some(gate_dist) = from_home.get(gate).copied()
                {
                    return props
                        .iter()
                        .any(|pos| from_home.get(pos).copied().unwrap_or(u32::MAX) <= gate_dist);
                }
            }
            false
        });

        // 橋Gateを未突破のルートでは、橋手前に未所有物件がある間は橋向こうの
        // 遠隔物件を先取りしない。スロット計算の前に各ルートの有効な候補物件を
        // 確定させることで、空のルートへの無駄なスロット割り当てを防ぐ。
        for (route, properties) in route_properties.iter_mut().enumerate() {
            if properties.is_empty() {
                continue;
            }
            properties.sort_unstable_by_key(|position| {
                (
                    from_home.get(position).copied().unwrap_or(u32::MAX),
                    position.y,
                    position.x,
                )
            });

            if let Some(gate) = gates.get(route) {
                let gate_key = RouteBreakthroughKey {
                    island_id: assignment.island_id,
                    gate: *gate,
                };
                if !crossed_gates.contains(&gate_key)
                    && let Some(gate_dist) = from_home.get(gate).copied()
                {
                    let has_before_gate = properties
                        .iter()
                        .any(|pos| from_home.get(pos).copied().unwrap_or(u32::MAX) <= gate_dist);
                    if has_before_gate {
                        properties.retain(|pos| {
                            from_home.get(pos).copied().unwrap_or(u32::MAX) <= gate_dist
                        });
                    } else if has_any_before_gate {
                        // 自陣側に未確保物件が残っている間は、Gate未突破のルートで
                        // Gate向こうの遠隔物件を先取りしない。
                        properties.clear();
                    }
                }
            }
        }

        let active_route_count = route_properties
            .iter()
            .filter(|properties| !properties.is_empty())
            .count();
        // Routeは戦力枠ではなく、首都戦役のロードマップである。従って有効な各routeに
        // 現在Milestoneを一つ残し、余る占領目標の先読みだけを全体需要比で並べる。
        // 兵力ゼロのrouteを作戦上閉じたり、逆に最低戦力を強制したりはしない。
        let additional_milestone_slots = usize::try_from(assignment.requirement.capture_units)
            .unwrap_or(usize::MAX)
            .saturating_sub(active_route_count);
        let effective_demands = route_properties
            .iter()
            .enumerate()
            .map(|(route, properties)| {
                if properties.is_empty() {
                    RouteForceDemand {
                        opportunity_value: 0,
                        force_deficit_value: 0,
                        demand_value: 0,
                    }
                } else {
                    route_demands
                        .get(route)
                        .cloned()
                        .unwrap_or(RouteForceDemand {
                            opportunity_value: 1,
                            force_deficit_value: 0,
                            demand_value: 1,
                        })
                }
            })
            .collect::<Vec<_>>();
        let additional_route_slots =
            apportion_route_force_slots(additional_milestone_slots, &effective_demands);
        let mut route_slots = route_properties
            .iter()
            .enumerate()
            .map(|(route, properties)| {
                usize::from(!properties.is_empty())
                    .saturating_add(additional_route_slots.get(route).copied().unwrap_or(0))
            })
            .collect::<Vec<_>>();

        // あるルートで割り当てられたスロットが物件数を超えて余った場合、物件が残っている他のルートへ回す
        let mut leftover_slots = 0_usize;
        for (route, properties) in route_properties.iter().enumerate() {
            if route_slots[route] > properties.len() {
                leftover_slots += route_slots[route] - properties.len();
                route_slots[route] = properties.len();
            }
        }
        if leftover_slots > 0 {
            for (route, properties) in route_properties.iter().enumerate() {
                if leftover_slots == 0 {
                    break;
                }
                if route_slots[route] < properties.len() {
                    let take = (properties.len() - route_slots[route]).min(leftover_slots);
                    route_slots[route] += take;
                    leftover_slots -= take;
                }
            }
        }

        let mut milestones = Vec::new();
        let mut route_milestones = Vec::new();
        for (route, properties) in route_properties.into_iter().enumerate() {
            if properties.is_empty() {
                continue;
            }

            route_milestones.push((
                route,
                properties
                    .into_iter()
                    .take(route_slots[route])
                    .collect::<Vec<_>>(),
            ));
        }
        // この順序は戦役全体の需要が高いrouteを先に観測するためのもの。座標順ではなく
        // 収益・生産基盤・戦力差を合成した需要で決めるが、各routeのMilestone自体は残す。
        route_milestones.sort_by_key(|(route, _)| {
            (
                std::cmp::Reverse(
                    route_demands
                        .get(*route)
                        .map_or(0, |demand| demand.demand_value),
                ),
                *route,
            )
        });
        for (_, targets) in route_milestones {
            milestones.extend(targets);
        }
        if let Some(target) = milestones.first().copied() {
            assignment.target_position = target;
            assignment.capture_target_positions = milestones.clone();
            let new_capture_units = u32::try_from(milestones.len()).unwrap_or(u32::MAX);
            assignment.requirement.capture_units = new_capture_units;
            assignment.purchase_shortfall.capture_units = assignment
                .purchase_shortfall
                .capture_units
                .min(new_capture_units);
        }
    }
}

pub(in crate::ai::v4) fn split_bridge_route_objectives(
    map: &Map,
    objectives: Vec<CampaignPlanningObjective>,
    gates: &[GridPosition],
    my_units: &[UnitSnapshot],
    unit_targets: &HashMap<Entity, GridPosition>,
) -> Vec<CampaignPlanningObjective> {
    if gates.len() < 2 {
        return objectives;
    }
    let unit_positions = my_units
        .iter()
        .filter_map(|unit| unit.entity.map(|entity| (entity, unit.pos)))
        .collect::<HashMap<_, _>>();
    let nearest_gate = |position: GridPosition| {
        gates
            .iter()
            .enumerate()
            .min_by_key(|(index, gate)| {
                (map.distance(position.x, position.y, gate.x, gate.y), *index)
            })
            .map_or(0, |(index, _)| index)
    };
    let mut split = Vec::new();
    for objective in objectives {
        if objective.kind != OperationKind::Capture || objective.objective_properties.len() < 2 {
            split.push(objective);
            continue;
        }
        let mut property_groups = vec![Vec::new(); gates.len()];
        for property in &objective.objective_properties {
            property_groups[nearest_gate(*property)].push(*property);
        }
        let active_groups = property_groups
            .iter()
            .enumerate()
            .filter_map(|(index, properties)| (!properties.is_empty()).then_some(index))
            .collect::<Vec<_>>();
        if active_groups.len() < 2 {
            split.push(objective);
            continue;
        }
        let total_properties = objective.objective_properties.len();
        let mut allocated_required = 0;
        for (active_order, group_index) in active_groups.iter().copied().enumerate() {
            let properties = &property_groups[group_index];
            let required_capture_survivors = if active_order + 1 == active_groups.len() {
                objective
                    .required_capture_survivors
                    .saturating_sub(allocated_required)
            } else {
                objective
                    .required_capture_survivors
                    .saturating_mul(properties.len())
                    / total_properties.max(1)
            };
            allocated_required = allocated_required.saturating_add(required_capture_survivors);
            let anchor = properties
                .iter()
                .min_by_key(|position| {
                    (
                        map.distance(
                            position.x,
                            position.y,
                            gates[group_index].x,
                            gates[group_index].y,
                        ),
                        position.y,
                        position.x,
                    )
                })
                .copied()
                .expect("空でないルート目標群");
            let protected_capture_entities = objective
                .protected_capture_entities
                .iter()
                .filter(|entity| {
                    if let Some(target) = unit_targets.get(entity) {
                        if properties.contains(target) {
                            return true;
                        }
                        return nearest_gate(*target) == group_index;
                    }
                    unit_positions
                        .get(entity)
                        .is_some_and(|position| nearest_gate(*position) == group_index)
                })
                .copied()
                .collect();
            split.push(CampaignPlanningObjective {
                island_id: objective.island_id,
                kind: objective.kind,
                anchor,
                objective_properties: properties.clone(),
                capture_eta: objective.capture_eta,
                required_capture_survivors,
                logistics_rank: objective.logistics_rank,
                forced_target_enemies: HashSet::default(),
                protected_capture_entities,
                capture_replacement_pending: objective.capture_replacement_pending,
                staging_anchor: anchor,
                execution_authorized: objective.execution_authorized,
            });
        }
    }
    split
}

/// 標準経路の同一陸塊Capture目標を、首都ルートDAGの進軍路ごとの作戦へ分ける。
///
/// 島campaignは島ごとに一つの判断しか出さないため、単一大陸では局地Capture作戦が
/// 一つだけになり、同じ陸塊の到達可能な敵が全員その一作戦へ帰属する。すると生産計画は
/// 全敵を相手に毎手番実行不能となり、配置側が進軍路ごとに配っている前線と生産が
/// 噛み合わない。進軍路ごとの前線拠点（DAGの未確保フロンティア）を各作戦の目標にし、
/// 敵も最寄りの進軍路作戦へ分かれて帰属するようにする。
///
/// `route_properties` は進軍路ごとの前線拠点（最も前進した順、末尾が最も手前）。敵首都は首都作戦が
/// 担うので含めない。進軍路に属さない元の目標物件は、最寄りの進軍路へ寄せる。
pub(in crate::ai::v4) fn split_route_front_objectives(
    map: &Map,
    objectives: Vec<CampaignPlanningObjective>,
    route_properties: &[Vec<GridPosition>],
    my_units: &[UnitSnapshot],
    unit_targets: &HashMap<Entity, GridPosition>,
) -> Vec<CampaignPlanningObjective> {
    // 複数の進軍路が同じ拠点を共有する場合は、先に現れた進軍路だけへ属させる。
    let mut claimed = HashSet::default();
    let routes = route_properties
        .iter()
        .map(|properties| {
            properties
                .iter()
                .copied()
                .filter(|property| claimed.insert(*property))
                .collect::<Vec<_>>()
        })
        .filter(|properties| !properties.is_empty())
        .collect::<Vec<_>>();
    if routes.len() < 2 {
        return objectives;
    }
    let unit_positions = my_units
        .iter()
        .filter_map(|unit| unit.entity.map(|entity| (entity, unit.pos)))
        .collect::<HashMap<_, _>>();
    // 各進軍路で次に取るべき拠点は、前線拠点のうち最も手前（自首都に近い）のもの。
    // 最も前進した拠点を作戦地点にすると、敵首都の目前を目標にして手前の敵を数え損ねる。
    let nearest_fronts = routes
        .iter()
        .map(|properties| properties[properties.len() - 1])
        .collect::<Vec<_>>();
    let nearest_route = |position: GridPosition| {
        routes
            .iter()
            .position(|properties| properties.contains(&position))
            .unwrap_or_else(|| {
                nearest_fronts
                    .iter()
                    .enumerate()
                    .min_by_key(|(index, anchor)| {
                        (
                            map.distance(position.x, position.y, anchor.x, anchor.y),
                            *index,
                        )
                    })
                    .map_or(0, |(index, _)| index)
            })
    };
    let mut split = Vec::new();
    for objective in objectives {
        if objective.kind != OperationKind::Capture {
            split.push(objective);
            continue;
        }
        let mut groups = routes.clone();
        for property in &objective.objective_properties {
            let route = nearest_route(*property);
            if !groups[route].contains(property) {
                groups[route].push(*property);
            }
        }
        let total_properties = groups.iter().map(Vec::len).sum::<usize>().max(1);
        let mut allocated_required = 0;
        for (index, properties) in groups.iter().enumerate() {
            // 島campaignが選んだ目標がこの進軍路にあればそれを作戦地点にし、
            // なければこの進軍路で最も手前の前線拠点にする。
            let anchor = if std::env::var("OPENWARS_ROUTE_ANCHOR").is_ok()
                && properties.contains(&objective.anchor)
            {
                objective.anchor
            } else {
                nearest_fronts[index]
            };
            // 占領役の必要数は物件数に比例して配り、端数は最後の進軍路へ寄せる。
            let required_capture_survivors = if index + 1 == groups.len() {
                objective
                    .required_capture_survivors
                    .saturating_sub(allocated_required)
            } else {
                objective
                    .required_capture_survivors
                    .saturating_mul(properties.len())
                    / total_properties
            };
            allocated_required = allocated_required.saturating_add(required_capture_survivors);
            // 既に向かっている占領役は、目標地点（なければ現在位置）が属する進軍路へ残す。
            let protected_capture_entities = objective
                .protected_capture_entities
                .iter()
                .filter(|entity| {
                    unit_targets
                        .get(entity)
                        .or_else(|| unit_positions.get(entity))
                        .is_some_and(|position| nearest_route(*position) == index)
                })
                .copied()
                .collect();
            split.push(CampaignPlanningObjective {
                island_id: objective.island_id,
                kind: objective.kind,
                anchor,
                objective_properties: properties.clone(),
                capture_eta: objective.capture_eta,
                required_capture_survivors,
                logistics_rank: objective.logistics_rank,
                forced_target_enemies: HashSet::default(),
                protected_capture_entities,
                capture_replacement_pending: objective.capture_replacement_pending,
                staging_anchor: anchor,
                execution_authorized: objective.execution_authorized,
            });
        }
    }
    split
}

/// 進軍路の攻守を見積もる手番数。占領役の展開と占領（2手番）に、奪回を防ぐ2手番を足した程度。
pub(in crate::ai::v4) const ROUTE_CONTEST_WINDOW: u32 = 4;

/// 目標へ見積もり期間内に届く部隊の戦力価値（価格×HP）。
fn route_contest_value(map: &Map, units: &[UnitSnapshot], target: GridPosition) -> u32 {
    units
        .iter()
        .filter(|unit| {
            map.distance(unit.pos.x, unit.pos.y, target.x, target.y)
                .div_ceil(unit.stats.max_movement.max(1))
                <= ROUTE_CONTEST_WINDOW
        })
        .map(|unit| unit.stats.cost.saturating_mul(unit.hp) / 100)
        .fold(0_u32, u32::saturating_add)
}

/// 各部隊を最も早く着ける進軍路（`anchors`のSome）一つにだけ割り当て、見積もり期間内に
/// 着ける部隊の戦力価値を進軍路ごとに合計する。首都周りの部隊を複数の進軍路へ重ねて
/// 数えないためである。
fn partitioned_route_values(
    map: &Map,
    units: &[UnitSnapshot],
    anchors: &[Option<GridPosition>],
) -> Vec<u32> {
    let mut values = vec![0_u32; anchors.len()];
    for unit in units {
        if let Some(index) = route_of_unit(map, unit, anchors) {
            values[index] =
                values[index].saturating_add(unit.stats.cost.saturating_mul(unit.hp) / 100);
        }
    }
    values
}

/// [`partitioned_route_values`] と同じ割り当てで、進軍路ごとの部隊数を数える。
fn partitioned_route_counts(
    map: &Map,
    units: &[UnitSnapshot],
    anchors: &[Option<GridPosition>],
) -> Vec<u32> {
    let mut counts = vec![0_u32; anchors.len()];
    for unit in units {
        if let Some(index) = route_of_unit(map, unit, anchors) {
            counts[index] = counts[index].saturating_add(1);
        }
    }
    counts
}

/// 部隊が最も早く着ける進軍路（見積もり期間内に着けなければNone）。
fn route_of_unit(
    map: &Map,
    unit: &UnitSnapshot,
    anchors: &[Option<GridPosition>],
) -> Option<usize> {
    let (eta, index) = anchors
        .iter()
        .enumerate()
        .filter_map(|(index, anchor)| {
            let anchor = (*anchor)?;
            let eta = map
                .distance(unit.pos.x, unit.pos.y, anchor.x, anchor.y)
                .div_ceil(unit.stats.max_movement.max(1));
            Some((eta, index))
        })
        .min()?;
    (eta <= ROUTE_CONTEST_WINDOW).then_some(index)
}

/// 守りの地点の前面で、敵が同時に戦闘へ加われる部隊数（通路の幅）。前面は守りの地点の
/// 地域のうち敵に最も近いマスで、その隣のマスのうち守りの地点から見て前面と同じか
/// 外側にあり、占領役が1手番で入れるマスの数を数える。狭い通路では少なくなる。
fn frontage_width(
    map: &Map,
    guard: GridPosition,
    enemy_units: &[UnitSnapshot],
    turns: &mut dyn FnMut(GridPosition, GridPosition) -> Option<u32>,
) -> u32 {
    let radius = crate::ai::v4::capital_route::model::CAPITAL_ROUTE_REGION_RADIUS;
    let nearest_enemy = |cell: GridPosition| {
        enemy_units
            .iter()
            .map(|unit| map.distance(unit.pos.x, unit.pos.y, cell.x, cell.y))
            .min()
            .unwrap_or(u32::MAX)
    };
    let mut region = Vec::new();
    for y in guard.y.saturating_sub(radius as usize)..=guard.y + radius as usize {
        for x in guard.x.saturating_sub(radius as usize)..=guard.x + radius as usize {
            if x < map.width && y < map.height && map.distance(x, y, guard.x, guard.y) <= radius {
                region.push(GridPosition { x, y });
            }
        }
    }
    let Some(front) = region
        .iter()
        .copied()
        .filter(|cell| *cell == guard || turns(guard, *cell).is_some())
        .min_by_key(|cell| (nearest_enemy(*cell), cell.y, cell.x))
    else {
        return 1;
    };
    let front_depth = map.distance(front.x, front.y, guard.x, guard.y);
    let mut width = 0_u32;
    for y in front.y.saturating_sub(1)..=front.y + 1 {
        for x in front.x.saturating_sub(1)..=front.x + 1 {
            let cell = GridPosition { x, y };
            if x >= map.width
                || y >= map.height
                || cell == front
                || map.distance(x, y, front.x, front.y) != 1
                || map.distance(x, y, guard.x, guard.y) < front_depth
            {
                continue;
            }
            if turns(front, cell).is_some_and(|eta| eta <= 1) {
                width += 1;
            }
        }
    }
    width.max(1)
}

/// 目標に最も近い自軍拠点。
fn nearest_own_property(
    map: &Map,
    own_properties: &[GridPosition],
    target: GridPosition,
) -> Option<GridPosition> {
    own_properties
        .iter()
        .min_by_key(|property| {
            (
                map.distance(property.x, property.y, target.x, target.y),
                property.y,
                property.x,
            )
        })
        .copied()
}

/// 目標に最も近い自軍の地上の生産施設（工場・首都）。
fn nearest_own_facility(
    map: &Map,
    own_properties: &[GridPosition],
    target: GridPosition,
) -> Option<GridPosition> {
    let facilities = own_properties
        .iter()
        .copied()
        .filter(|property| {
            matches!(
                map.get_terrain(property.x, property.y),
                Some(Terrain::Factory | Terrain::Capital)
            )
        })
        .collect::<Vec<_>>();
    nearest_own_property(map, &facilities, target)
}

/// 進軍路のCapture目標を、目標に最も近い自軍拠点を守るDefense作戦へ置き換える。
/// 自軍拠点が無ければ置き換えない。
fn guard_route(
    map: &Map,
    objective: CampaignPlanningObjective,
    own_properties: &[GridPosition],
) -> CampaignPlanningObjective {
    let Some(guard) = nearest_own_property(map, own_properties, objective.anchor) else {
        return objective;
    };
    defense_objective(objective, guard)
}

/// 進軍路のCapture目標を、`anchor` を守るDefense作戦へ置き換える。
fn defense_objective(
    objective: CampaignPlanningObjective,
    anchor: GridPosition,
) -> CampaignPlanningObjective {
    CampaignPlanningObjective {
        island_id: objective.island_id,
        kind: OperationKind::Defense,
        anchor,
        objective_properties: vec![anchor],
        capture_eta: None,
        required_capture_survivors: 0,
        logistics_rank: objective.logistics_rank,
        forced_target_enemies: HashSet::default(),
        protected_capture_entities: HashSet::default(),
        capture_replacement_pending: false,
        staging_anchor: anchor,
        execution_authorized: true,
    }
}

/// 進軍路で敵より先に押さえられる最も前の地点（守りの要衝・攻めの集結点）。候補は、この進軍路の目標に最も
/// 近い進軍路の目標を持つ自軍拠点・DAG地域の代表点と、目標そのもの。自軍の到着（部隊から、
/// または首都で生産して1手番後に出る）が敵のどの部隊より早い候補のうち、目標に最も近い
/// ものを返す。どの候補も先に押さえられなければNone。
#[allow(clippy::too_many_arguments)]
fn seizable_key_point(
    map: &Map,
    route: usize,
    route_anchors: &[GridPosition],
    candidates: &[GridPosition],
    my_units: &[UnitSnapshot],
    enemy_units: &[UnitSnapshot],
    own_capital: Option<GridPosition>,
    turns: &mut dyn FnMut(GridPosition, GridPosition) -> Option<u32>,
) -> Option<GridPosition> {
    let anchor = route_anchors[route];
    let nearest_route = |position: GridPosition| {
        route_anchors
            .iter()
            .enumerate()
            .min_by_key(|(index, other)| {
                (
                    map.distance(position.x, position.y, other.x, other.y),
                    *index,
                )
            })
            .map(|(index, _)| index)
    };
    let mut seized = Vec::new();
    for &candidate in candidates
        .iter()
        .chain(std::iter::once(&anchor))
        .filter(|candidate| nearest_route(**candidate) == Some(route))
    {
        let from_units = my_units
            .iter()
            .filter_map(|unit| turns(unit.pos, candidate))
            .min();
        let from_capital = own_capital
            .and_then(|capital| turns(capital, candidate))
            .map(|eta| eta.saturating_add(1));
        let ours = match (from_units, from_capital) {
            (Some(units), Some(capital)) => Some(units.min(capital)),
            (units, capital) => units.or(capital),
        };
        let theirs = enemy_units
            .iter()
            .filter_map(|unit| turns(unit.pos, candidate))
            .min();
        let first = match (ours, theirs) {
            (Some(ours), Some(theirs)) => ours < theirs,
            (Some(_), None) => true,
            _ => false,
        };
        if first {
            seized.push(candidate);
        }
    }
    seized.into_iter().min_by_key(|position| {
        (
            map.distance(position.x, position.y, anchor.x, anchor.y),
            position.y,
            position.x,
        )
    })
}

/// 一つの進軍路で両軍の戦力がぶつかったときの結果。ランチェスターの二次法則で、
/// 勝つ側には戦力の二乗差の平方根だけ戦力が残る。自軍が勝てば正、負ければ負の残存価値。
fn route_battle_outcome(own: f64, enemy: f64) -> f64 {
    if own >= enemy {
        (own * own - enemy * enemy).sqrt()
    } else {
        -(enemy * enemy - own * own).sqrt()
    }
}

/// 一つの進軍路の見通し。戦力は価格×HPの価値、ターン数は移動コストを加味した実ターン。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::ai::v4) struct RouteProspect {
    /// 見積もり期間内に目標へ届く自軍の戦力価値。
    pub(in crate::ai::v4) own: u32,
    /// 見積もり期間内に目標へ届く敵の戦力価値。
    pub(in crate::ai::v4) enemy: u32,
    /// この進軍路で勝てば見積もり期間内に押さえに行ける地域の拠点数の合計。
    pub(in crate::ai::v4) gains: u32,
    /// この進軍路で負ければ見積もり期間内に敵に届かれる、自軍が押さえる地域の拠点数の合計。
    pub(in crate::ai::v4) losses: u32,
    /// 目標から敵首都までの実ターン。届かなければNone。
    pub(in crate::ai::v4) capital_turns: Option<u32>,
    /// この進軍路で負けたとき、敵が守りの地点から自軍首都へ届く実ターン。
    pub(in crate::ai::v4) exposed_capital_turns: Option<u32>,
    /// 守りの地点の前面で敵が同時に戦闘へ加われる戦力の上限（敵の平均戦力価値×通路の幅）。
    /// 戦力節約で守る進軍路は、敵の全戦力ではなくこの上限と互角になればよい。
    pub(in crate::ai::v4) contact_cap: u32,
}

/// 決めた進軍路の配分案（主戦場）。主戦場を決めたら攻略・防衛の作戦を遂行し、
/// 首都ルートDAGの地域の保持者が変わるまで比べ直さない。生産とSquadの両方が読む。
#[derive(bevy_ecs::prelude::Resource, Debug, Default)]
pub(crate) struct RoutePostureMemory {
    pub(crate) by_player: HashMap<PlayerId, RoutePostureDecision>,
}

/// 配分案と、それを決めたときの地域の状況、各進軍路で押さえる要衝。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoutePostureDecision {
    /// Noneは分散案、Some(目標)はその進軍路への集中案。
    pub(crate) main: Option<GridPosition>,
    /// 決めたときの首都ルートDAGの各地域の保持者（地域の代表点順）。
    pub(crate) regions: Vec<(GridPosition, RegionHolder)>,
    /// 各進軍路で押さえる要衝と、そこへ充てる戦力。
    pub(crate) orders: Vec<RouteOrder>,
}

/// 一つの進軍路で押さえる要衝。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RouteOrder {
    /// 押さえに行く地点。攻めるなら進軍路の目標、守るなら目標に最も近い自軍拠点。
    pub(crate) key_point: GridPosition,
    /// 守る（true）か攻める（false）か。
    pub(crate) defend: bool,
    /// 攻めの作戦が進出の条件を満たすまで集まる地点。敵より先に押さえられる最も前の地点
    /// （無ければ目標に最も近い自軍拠点）。守りの作戦では要衝と同じ。
    pub(crate) staging: GridPosition,
    /// この進軍路へ充てる戦力価値（手持ちと配分案で割り振った生産の合計）。
    pub(crate) force: u32,
}

/// 首都ルートDAGのノード作戦が受け持つ地域を、どちらが押さえているか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RegionHolder {
    Own,
    Enemy,
    Contested,
}

/// 首都ルートDAGのノード作戦が受け持つ地域（拠点群）。進軍路の配分案では、拠点を都市
/// 一つずつではなく、この地域を単位に得失を数える。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ai::v4) struct RouteRegion {
    /// ノード作戦の代表点。
    pub(in crate::ai::v4) anchor: GridPosition,
    /// 地域に含まれる拠点の数。地域の価値の重みにする。
    pub(in crate::ai::v4) properties: u32,
    pub(in crate::ai::v4) holder: RegionHolder,
}

/// 地域の拠点のうち3分の2以上を押さえた側を、その地域の保持者とする。DAGの地域優勢
/// （Dominant）と同じ基準で、都市一つの取り合いでは保持者が変わらない。
pub(in crate::ai::v4) fn region_holder(
    owned: usize,
    enemy_owned: usize,
    total: usize,
) -> RegionHolder {
    if total > 0 && owned.saturating_mul(3) >= total.saturating_mul(2) {
        RegionHolder::Own
    } else if total > 0 && enemy_owned.saturating_mul(3) >= total.saturating_mul(2) {
        RegionHolder::Enemy
    } else {
        RegionHolder::Contested
    }
}

/// 拠点と首都を戦力価値と同じ尺度へ換算する値。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ai::v4) struct PostureValues {
    /// 拠点1つの価値（拠点あたりの収入を数ターン分）。
    pub(in crate::ai::v4) property: u32,
    /// 敵首都の価値（敵の総収入を数ターン分）。早く攻め込めるほど割り引かずに数える。
    pub(in crate::ai::v4) capital: u32,
    /// 自軍首都の価値（自軍の総収入を数ターン分）。早く攻め込まれるほど大きく失う。
    pub(in crate::ai::v4) own_capital: u32,
    /// 占領役1体の戦力価値。勝った側が押さえられる拠点数を残存戦力から見積もる単位。
    pub(in crate::ai::v4) capturer: u32,
}

/// 進軍路の配分案の結果。
#[derive(Debug, Default)]
pub(in crate::ai::v4) struct RouteEffortPlan {
    pub(in crate::ai::v4) objectives: Vec<CampaignPlanningObjective>,
    /// 戦力節約で守る進軍路のDefense作戦地点と、そこへ使ってよい生産額。
    pub(in crate::ai::v4) economy_anchors: Vec<(GridPosition, u32)>,
    /// 遂行する配分案。Some(None)は分散、Some(Some(目標))はその進軍路へ集中。
    /// 比べる対象が無ければNone。
    pub(in crate::ai::v4) posture: Option<Option<GridPosition>>,
    /// 各進軍路で押さえる要衝。
    pub(in crate::ai::v4) orders: Vec<RouteOrder>,
}

/// 戦力の配分案。`None`は敵のいる進軍路へ生産を等分する分散案、`Some(i)`は進軍路iへ
/// 生産を集中し、他の進軍路は手持ちの部隊だけで受け止める集中案。
type RoutePosture = Option<usize>;

/// 見積もり期間内に両軍が生産で足せる戦力の額。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ai::v4) struct PostureBudgets {
    /// 自軍の所持金と期間中の収入。
    pub(in crate::ai::v4) own: u32,
    /// 敵の期間中の収入（敵の所持金は見えない）。
    pub(in crate::ai::v4) enemy: u32,
}

/// 敵の期間中の増援を、今の敵の配置に比例して進軍路へ割り振った敵戦力。
/// 敵は主力のいる進軍路へ増援も送るとみなす。
fn reinforced_enemy(
    prospects: &[RouteProspect],
    contested: &[usize],
    enemy_budget: u32,
) -> Vec<u64> {
    let deployed = contested
        .iter()
        .map(|&route| u64::from(prospects[route].enemy))
        .sum::<u64>();
    let mut enemy = prospects
        .iter()
        .map(|prospect| u64::from(prospect.enemy))
        .collect::<Vec<_>>();
    for &route in contested {
        let share = (u64::from(enemy_budget) * u64::from(prospects[route].enemy))
            .checked_div(deployed)
            .unwrap_or_else(|| u64::from(enemy_budget) / contested.len().max(1) as u64);
        enemy[route] = enemy[route].saturating_add(share);
    }
    enemy
}

/// 配分案での各進軍路への生産の割り振り。分散案は等分する。集中案は、主戦場以外の
/// 進軍路へまず、守りの地点の前面で同時に戦える敵と互角になる額（保持できる最小限の守り）
/// を割り振り、残りを主戦場へ回す。
fn posture_allocation(
    posture: RoutePosture,
    prospects: &[RouteProspect],
    contested: &[usize],
    enemy: &[u64],
    budget: u32,
) -> Vec<u64> {
    let mut added = vec![0_u64; prospects.len()];
    match posture {
        None => {
            let share = u64::from(budget) / contested.len().max(1) as u64;
            for &route in contested {
                added[route] = share;
            }
        }
        Some(main) => {
            let mut remaining = u64::from(budget);
            for &route in contested.iter().filter(|route| **route != main) {
                let parity = enemy[route]
                    .min(u64::from(prospects[route].contact_cap))
                    .saturating_sub(u64::from(prospects[route].own));
                added[route] = parity.min(remaining);
                remaining -= added[route];
            }
            added[main] = remaining;
        }
    }
    added
}

/// 配分案の価値。各進軍路の戦闘の残存戦力に、勝てば押さえる地域・負ければ失う地域の価値を
/// 足す。押さえられる地域は勝った側の残存戦力で頭打ちにし、占領役1体分の戦力ごとに拠点
/// 1つまでとする。首都へのスピードは両方向で見て、勝った進軍路のうち最も早く敵首都へ
/// 攻め込めるものの首都の価値を足し、負けた進軍路のうち敵が最も早く自軍首都へ届くものの
/// 首都の価値を引く。首都の価値は到達までの実ターン+1で割り引く。
fn posture_score(
    posture: RoutePosture,
    prospects: &[RouteProspect],
    contested: &[usize],
    budgets: PostureBudgets,
    values: PostureValues,
) -> f64 {
    let enemy = reinforced_enemy(prospects, contested, budgets.enemy);
    let added = posture_allocation(posture, prospects, contested, &enemy, budgets.own);
    // 残存戦力で押さえられる拠点数の上限。
    let capacity = |surviving: f64| surviving / f64::from(values.capturer.max(1));
    let mut score = 0.0;
    let mut fastest_capital: Option<u32> = None;
    let mut fastest_exposure: Option<u32> = None;
    for &route in contested {
        let prospect = &prospects[route];
        let own = u64::from(prospect.own).saturating_add(added[route]);
        // 集中案で守りに回す進軍路は、守りの地点の前面で同時に戦える敵とだけ戦う。
        let engaged = if posture.is_some_and(|main| main != route) {
            enemy[route].min(u64::from(prospect.contact_cap))
        } else {
            enemy[route]
        };
        let outcome = route_battle_outcome(own as f64, engaged as f64);
        score += outcome;
        if outcome > 0.0 {
            score += f64::from(prospect.gains).min(capacity(outcome)) * f64::from(values.property);
            if let Some(turns) = prospect.capital_turns {
                fastest_capital = Some(fastest_capital.map_or(turns, |best| best.min(turns)));
            }
        } else if outcome < 0.0 {
            score -=
                f64::from(prospect.losses).min(capacity(-outcome)) * f64::from(values.property);
            if let Some(turns) = prospect.exposed_capital_turns {
                fastest_exposure = Some(fastest_exposure.map_or(turns, |best| best.min(turns)));
            }
        }
    }
    if let Some(turns) = fastest_capital {
        score += f64::from(values.capital) / f64::from(turns.saturating_add(1));
    }
    if let Some(turns) = fastest_exposure {
        score -= f64::from(values.own_capital) / f64::from(turns.saturating_add(1));
    }
    score
}

/// 配分案ごとに [`posture_score`] を見積もり、最も大きい案を選ぶ。
///
/// 集中すると主戦場では二乗則で大きく勝てるが、他の進軍路は最小限の守りで受け止め、
/// 予算が足りなければそこで地域を失い得る。分散すれば各進軍路で互角に近づく。どちらが得かは敵の配置、
/// 予算、拠点の配置、首都までの道のりで変わるので決め打ちせず、全ての案を同じ評価で
/// 比べる。同点なら分散案を保つ。
fn choose_route_posture(
    prospects: &[RouteProspect],
    contested: &[usize],
    budgets: PostureBudgets,
    values: PostureValues,
) -> RoutePosture {
    let score = |posture| posture_score(posture, prospects, contested, budgets, values);
    let mut best: RoutePosture = None;
    let mut best_score = score(None);
    for &route in contested {
        let candidate = score(Some(route));
        if candidate > best_score {
            best = Some(route);
            best_score = candidate;
        }
    }
    if std::env::var_os("OPENWARS_CONCENTRATE_DEBUG").is_some() {
        eprintln!(
            "ROUTE_POSTURE prospects={prospects:?} budgets={budgets:?} values={values:?} spread={:.0} chosen={best:?} score={best_score:.0}",
            score(None)
        );
    }
    best
}

/// 戦力を一つの進軍路へ集中する案と分散する案を比べ、集中する方が良ければ、主戦場以外の
/// 敵のいる進軍路を最小限の戦力で守る。
///
/// 敵が見積もり期間内に戦力を寄せている進軍路が二つ以上あるとき、各進軍路の見通し
/// （戦力、得る地域、失う地域、首都までの実ターン）を作り、[`choose_route_posture`]
/// で配分案を選ぶ。集中案なら、主戦場以外の敵のいる進軍路を、目標に最も近い自軍拠点を
/// 守るDefense作戦（戦力節約の守り）へ切り替え、その地点を返す。守りの作戦は保持できる
/// 最も安い編成だけを買うので、資金は主戦場へ回る。敵のいない進軍路の占領は安く取れる
/// ので、どの案でもそのまま残す。`decided` は地域の状況が変わっていないときの決定済みの案で、
/// あれば比べ直さず、主戦場・要衝・攻守もそのままに遂行する。`turns` は占領役の足で測る実ターン（届かなければNone）。
#[allow(clippy::too_many_arguments)]
pub(in crate::ai::v4) fn concentrate_route_efforts(
    map: &Map,
    objectives: Vec<CampaignPlanningObjective>,
    my_units: &[UnitSnapshot],
    enemy_units: &[UnitSnapshot],
    own_properties: &[GridPosition],
    regions: &[RouteRegion],
    capitals: (Option<GridPosition>, Option<GridPosition>),
    budgets: PostureBudgets,
    values: PostureValues,
    decided: Option<&RoutePostureDecision>,
    gun_anchors: &[GridPosition],
    turns: &mut dyn FnMut(GridPosition, GridPosition) -> Option<u32>,
) -> RouteEffortPlan {
    let (own_capital, enemy_capital) = capitals;
    let route_anchors = objectives
        .iter()
        .map(|objective| (objective.kind == OperationKind::Capture).then_some(objective.anchor))
        .collect::<Vec<_>>();
    let enemy_values = partitioned_route_values(map, enemy_units, &route_anchors);
    let contested = (0..objectives.len())
        .filter(|index| enemy_values[*index] > 0)
        .collect::<Vec<_>>();
    if std::env::var_os("OPENWARS_CONCENTRATE_DEBUG").is_some() {
        eprintln!(
            "ROUTE_POSTURE_INPUT anchors={route_anchors:?} enemy={enemy_values:?} decided={decided:?}"
        );
    }
    if contested.len() < 2 {
        return RouteEffortPlan {
            objectives,
            ..RouteEffortPlan::default()
        };
    }
    let contested_anchors = route_anchors
        .iter()
        .enumerate()
        .map(|(index, anchor)| anchor.filter(|_| contested.contains(&index)))
        .collect::<Vec<_>>();
    let own_values = partitioned_route_values(map, my_units, &contested_anchors);
    let enemy_counts = partitioned_route_counts(map, enemy_units, &route_anchors);
    let within_window =
        |turns: Option<u32>| turns.is_some_and(|turns| turns <= ROUTE_CONTEST_WINDOW);
    // 地域ごとの得失。自軍が押さえていない地域は、実ターンで最も近い進軍路の目標から
    // 見積もり期間内に届けば、その進軍路で勝ったときに押さえに行ける。自軍が押さえる地域は、
    // 進軍路の守りの地点（目標に最も近い自軍拠点）から見積もり期間内に届けば、その進軍路で
    // 負けたときに敵に届かれる。
    let guards = objectives
        .iter()
        .map(|objective| nearest_own_property(map, own_properties, objective.anchor))
        .collect::<Vec<_>>();
    let mut gains = vec![0_u32; objectives.len()];
    let mut losses = vec![0_u32; objectives.len()];
    for region in regions {
        let own = region.holder == RegionHolder::Own;
        let nearest = contested
            .iter()
            .filter_map(|&route| {
                let from = if own {
                    guards[route]?
                } else {
                    objectives[route].anchor
                };
                turns(from, region.anchor).map(|distance| (distance, route))
            })
            .min();
        let Some((distance, route)) = nearest else {
            continue;
        };
        if !within_window(Some(distance)) {
            continue;
        }
        let tally = if own { &mut losses } else { &mut gains };
        tally[route] = tally[route].saturating_add(region.properties);
    }
    let prospects = objectives
        .iter()
        .enumerate()
        .map(|(index, objective)| {
            if !contested.contains(&index) {
                return RouteProspect::default();
            }
            RouteProspect {
                own: own_values[index],
                enemy: enemy_values[index],
                gains: gains[index],
                losses: losses[index],
                capital_turns: enemy_capital.and_then(|capital| turns(objective.anchor, capital)),
                contact_cap: guards[index].map_or(u32::MAX, |guard| {
                    let mean = enemy_values[index] / enemy_counts[index].max(1);
                    mean.saturating_mul(frontage_width(map, guard, enemy_units, turns))
                }),
                exposed_capital_turns: guards[index]
                    .zip(own_capital)
                    .and_then(|(guard, capital)| turns(guard, capital)),
            }
        })
        .collect::<Vec<_>>();
    // 拠点の状況が変わらない間は、決めた配分案のまま攻略・防衛を続ける。主戦場の目標が
    // 進軍路から消えていれば決め直す。
    let decided_posture = decided.and_then(|decision| match decision.main {
        None => Some(None),
        Some(anchor) => contested
            .iter()
            .find(|&&route| objectives[route].anchor == anchor)
            .map(|&route| Some(route)),
    });
    // 攻めの砲台を割り当てた進軍路（砲台の受け持ちの要衝に最も近い進軍路のうち、砲台の多い
    // もの）。攻めてよいのはこの進軍路だけで、他の進軍路（右辺のような最低限の守り）は
    // ゴーサインを出さずに工場付近で守る。配分案どうしを比べて主戦場を選び直すことはしない
    // （見積もりの揺れで攻める進軍路が入れ替わり、部隊と砲台が戦線の間を往復するため）。
    // 攻めの砲台がまだ無い序盤だけは、最初に砲台を付ける進軍路を配分案の比較で選ぶ。
    let gun_route = {
        let mut counts = vec![0_usize; contested.len()];
        for anchor in gun_anchors {
            if let Some((nearest, _)) = contested.iter().enumerate().min_by_key(|(_, route)| {
                let candidate = objectives[**route].anchor;
                (
                    map.distance(anchor.x, anchor.y, candidate.x, candidate.y),
                    candidate.y,
                    candidate.x,
                )
            }) {
                counts[nearest] += 1;
            }
        }
        counts
            .iter()
            .enumerate()
            .filter(|(_, count)| **count > 0)
            .max_by_key(|(position, count)| (**count, std::cmp::Reverse(*position)))
            .map(|(position, _)| contested[position])
    };
    let decided_posture = decided_posture.filter(|main| gun_route.is_none() || *main == gun_route);
    // 決めた案を遂行する間は、各進軍路の要衝と攻守も決めたときのまま保つ（戦力の配分だけを
    // 今の盤面で更新する）。見積もりの揺れで攻守が手番ごとに入れ替わると、部隊が要衝へ
    // 着く前に行き先が変わる。
    let decided_orders = decided_posture
        .and(decided)
        .map(|decision| decision.orders.as_slice())
        .filter(|orders| orders.len() == contested.len());
    let posture = match gun_route {
        Some(route) => Some(route),
        None => decided_posture
            .unwrap_or_else(|| choose_route_posture(&prospects, &contested, budgets, values)),
    };
    // 配分案の戦力で各進軍路の要衝を決める。勝てる進軍路（分散案の全進軍路、集中案の
    // 主戦場）は目標を攻め、勝てない進軍路と守りの進軍路は目標に最も近い自軍拠点を守る。
    let main_anchor = posture.map(|main| objectives[main].anchor);
    let enemy = reinforced_enemy(&prospects, &contested, budgets.enemy);
    let allocation = posture_allocation(posture, &prospects, &contested, &enemy, budgets.own);
    let mut economy_anchors = Vec::new();
    let mut orders = Vec::new();
    // 各進軍路へ充てたい戦力。今いる位置ではなく、配分案の意図で決める。集中案では守りの
    // 進軍路へ敵と互角の分だけを充て、残りを全て主戦場へ充てる。
    let committed = contested
        .iter()
        .map(|&index| u64::from(own_values[index]).saturating_add(allocation[index]))
        .collect::<Vec<_>>();
    let total = committed.iter().sum::<u64>();
    let intended = |index: usize| -> u64 {
        match posture {
            None => {
                committed[contested
                    .iter()
                    .position(|&route| route == index)
                    .unwrap_or(0)]
            }
            Some(main) if main == index => total.saturating_sub(
                contested
                    .iter()
                    .filter(|&&route| route != main)
                    .map(|&route| {
                        enemy[route]
                            .min(u64::from(prospects[route].contact_cap))
                            .min(total)
                    })
                    .sum::<u64>(),
            ),
            Some(_) => enemy[index]
                .min(u64::from(prospects[index].contact_cap))
                .min(total),
        }
    };
    // 要衝の候補（自軍拠点とDAG地域の代表点）と、敵味方の到着を比べる進軍路の目標。
    let candidates = own_properties
        .iter()
        .copied()
        .chain(regions.iter().map(|region| region.anchor))
        .collect::<Vec<_>>();
    let contested_anchors = contested
        .iter()
        .map(|&index| objectives[index].anchor)
        .collect::<Vec<_>>();
    let objectives = objectives
        .into_iter()
        .enumerate()
        .map(|(index, objective)| {
            let Some(route) = contested.iter().position(|&candidate| candidate == index) else {
                return objective;
            };
            let force = u32::try_from(intended(index)).unwrap_or(u32::MAX);
            // 決めた案を遂行中なら、要衝と攻守を決めたときのまま保つ。ただし攻めの砲台の居ない
            // 守りの進軍路は、砲台を割り当てる前に決めた案でも工場付近で守る。
            if let Some(mut order) = decided_orders.map(|orders| orders[route]) {
                if order.defend
                    && gun_route.is_some_and(|gun_route| gun_route != index)
                    && let Some(facility) =
                        nearest_own_facility(map, own_properties, objective.anchor)
                {
                    order.key_point = facility;
                    order.staging = facility;
                }
                orders.push(RouteOrder { force, ..order });
                return if order.defend {
                    defense_objective(objective, order.key_point)
                } else {
                    objective
                };
            }
            // 攻めるか守るかは戦闘の見積もりで決める。敵より先に押さえられる最も前の地点は、
            // 守りの要衝、または攻めの集結点にする。
            let committed_force = u64::from(own_values[index]).saturating_add(allocation[index]);
            let wins = route_battle_outcome(committed_force as f64, enemy[index] as f64) > 0.0;
            let key_point = seizable_key_point(
                map,
                route,
                &contested_anchors,
                &candidates,
                my_units,
                enemy_units,
                own_capital,
                turns,
            );
            let attacks = wins && posture.is_none_or(|main| main == index);
            if attacks {
                let staging = key_point
                    .or_else(|| nearest_own_property(map, own_properties, objective.anchor))
                    .unwrap_or(objective.anchor);
                orders.push(RouteOrder {
                    key_point: objective.anchor,
                    defend: false,
                    staging,
                    force,
                });
                return objective;
            }
            // 守りの要衝。攻めの砲台の居る進軍路（西）は、守りに回っても首都の脇まで下がらず、
            // 目標に最も近い自軍拠点で守る。他の進軍路（右辺）は、前へ出ずに工場付近（目標に
            // 最も近い自軍の工場・首都）で守る。攻めの砲台がまだ無い序盤は、敵より先に押さえ
            // られる最も前の地点で守る。
            let guarded = match gun_route {
                Some(gun_route) if gun_route == index => {
                    guard_route(map, objective, own_properties)
                }
                Some(_) => match nearest_own_facility(map, own_properties, objective.anchor) {
                    Some(facility) => defense_objective(objective, facility),
                    None => guard_route(map, objective, own_properties),
                },
                None => match key_point {
                    Some(key_point) => defense_objective(objective, key_point),
                    None => guard_route(map, objective, own_properties),
                },
            };
            orders.push(RouteOrder {
                key_point: guarded.anchor,
                defend: true,
                staging: guarded.anchor,
                force,
            });
            // 集中案の主戦場以外は戦力節約の守りとし、割り振った互角分だけを使わせる。
            if guarded.kind == OperationKind::Defense && posture.is_some_and(|main| main != index) {
                economy_anchors.push((
                    guarded.anchor,
                    u32::try_from(allocation[index]).unwrap_or(u32::MAX),
                ));
            }
            guarded
        })
        .collect::<Vec<_>>();
    RouteEffortPlan {
        posture: Some(main_anchor),
        objectives,
        economy_anchors,
        orders,
    }
}

/// 進軍路ごとに、攻めるか守るかを戦力の見積もりで選ぶ。
///
/// 各進軍路のCapture目標について、目標へ見積もり期間内に届く敵の戦力価値（価格×HP）と、
/// 届く自軍の戦力価値に生産で足せる額（所持金と期間中の収入を進軍路で等分）を加えたものを
/// 比べる。自軍が及ばない進軍路では、取りに行っても保持できないので、その進軍路で目標に
/// 最も近い自軍拠点を守るDefense作戦へ切り替える。守る作戦も保持できるかで生産を決める
/// ため、橋や山の手前で守りに強い兵種（運搬する砲台など）が選ばれる。戦力で上回れば
/// 次の手番から再びCaptureへ戻る。
pub(in crate::ai::v4) fn defend_outmatched_routes(
    map: &Map,
    objectives: Vec<CampaignPlanningObjective>,
    my_units: &[UnitSnapshot],
    enemy_units: &[UnitSnapshot],
    own_properties: &[GridPosition],
    production_budget: u32,
    enemy_production_budget: u32,
) -> Vec<CampaignPlanningObjective> {
    let capture_count = objectives
        .iter()
        .filter(|objective| objective.kind == OperationKind::Capture)
        .count();
    if capture_count == 0 {
        return objectives;
    }
    let budget_share = production_budget / u32::try_from(capture_count).unwrap_or(u32::MAX);
    // 敵も同じ期間に収入で増援を作れるので、見込みの生産額を経路数で割って敵側にも足す。
    let enemy_budget_share =
        enemy_production_budget / u32::try_from(capture_count).unwrap_or(u32::MAX);
    objectives
        .into_iter()
        .map(|objective| {
            if objective.kind != OperationKind::Capture {
                return objective;
            }
            let target = objective.anchor;
            let enemy =
                route_contest_value(map, enemy_units, target).saturating_add(enemy_budget_share);
            let own = route_contest_value(map, my_units, target).saturating_add(budget_share);
            if own >= enemy {
                return objective;
            }
            guard_route(map, objective, own_properties)
        })
        .collect()
}

#[cfg(test)]
mod posture_tests {
    use super::*;

    const VALUES: PostureValues = PostureValues {
        property: 10_000,
        capital: 200_000,
        own_capital: 200_000,
        capturer: 1_000,
    };

    fn budgets(own: u32, enemy: u32) -> PostureBudgets {
        PostureBudgets { own, enemy }
    }

    fn route(own: u32, enemy: u32, gains: u32, losses: u32, capital: u32) -> RouteProspect {
        RouteProspect {
            own,
            enemy,
            gains,
            losses,
            capital_turns: Some(capital),
            exposed_capital_turns: None,
            contact_cap: u32::MAX,
        }
    }

    #[test]
    fn concentrates_against_the_enemy_main_body_when_the_budget_can_win_there() {
        // 左の進軍路に敵主力、右に小勢。等分では左が互角で止まるので、右は互角の最小限で
        // 守り、残りを左へ集中して主力を破る方が全体として得になる。
        let prospects = [route(0, 25_500, 3, 4, 8), route(0, 17_500, 1, 1, 8)];
        assert_eq!(
            choose_route_posture(&prospects, &[0, 1], budgets(51_000, 0), VALUES),
            Some(0)
        );
    }

    #[test]
    fn keeps_spreading_when_both_routes_are_already_held() {
        // 両方の進軍路に敵を上回る部隊がいれば、少額の増援を等分して両方で勝つ方が良い。
        let prospects = [
            route(20_000, 18_000, 1, 1, 8),
            route(20_000, 18_000, 1, 1, 8),
        ];
        assert_eq!(
            choose_route_posture(&prospects, &[0, 1], budgets(4_000, 0), VALUES),
            None
        );
    }

    #[test]
    fn enemy_reinforcements_follow_the_enemy_main_body() {
        // 期間中の敵の増援は、今の敵の配置に比例して進軍路へ割り振る。
        let prospects = [route(0, 24_500, 3, 6, 3), route(1_000, 1_000, 4, 14, 6)];
        assert_eq!(
            reinforced_enemy(&prospects, &[0, 1], 78_000),
            vec![24_500 + 74_941, 1_000 + 3_058]
        );
        // 増援を見込んでも、右を互角の最小限で守り、敵主力のいる左へ集中する。
        assert_eq!(
            choose_route_posture(&prospects, &[0, 1], budgets(104_000, 78_000), VALUES),
            Some(0)
        );
    }

    #[test]
    fn a_small_winning_force_takes_only_as_many_properties_as_it_can_capture() {
        // 自軍のいない進軍路でも、敵が占領役2体分しか残らなければ失う拠点は2つまで。
        let prospects = [route(0, 2_000, 0, 10, 8)];
        let score = posture_score(None, &prospects, &[0], budgets(0, 0), VALUES);
        assert_eq!(score, -2_000.0 - 2.0 * 10_000.0);
    }

    #[test]
    fn a_faster_road_to_the_capital_makes_the_better_main_effort() {
        // 戦力と地域が同じなら、首都へ早く攻め込める進軍路への集中の方が価値が高い。
        let prospects = [route(0, 30_000, 2, 2, 12), route(0, 30_000, 2, 2, 4)];
        let score =
            |posture| posture_score(posture, &prospects, &[0, 1], budgets(90_000, 0), VALUES);
        assert!(score(Some(1)) > score(Some(0)));
    }

    #[test]
    fn a_region_changes_hands_only_when_two_thirds_of_it_is_held() {
        assert_eq!(region_holder(2, 1, 3), RegionHolder::Own);
        assert_eq!(region_holder(1, 1, 3), RegionHolder::Contested);
        assert_eq!(region_holder(0, 2, 3), RegionHolder::Enemy);
        assert_eq!(region_holder(0, 0, 0), RegionHolder::Contested);
    }

    #[test]
    fn battle_outcome_follows_the_square_law() {
        assert_eq!(route_battle_outcome(5.0, 3.0), 4.0);
        assert_eq!(route_battle_outcome(3.0, 5.0), -4.0);
        assert_eq!(route_battle_outcome(4.0, 4.0), 0.0);
    }
}
