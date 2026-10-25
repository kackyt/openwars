//! 陸続きルートの兵力需要と突破実績。

use crate::ai::v4::capital_route::model::{
    RouteBreakthroughKey, RouteBreakthroughRegistry, RouteForceDemand,
};
use crate::ai::v4::capital_route::node_operation::capital_route_dag_all_properties;
use crate::ai::v4::capital_route::topology::{
    RouteFrontDistances, armored_entry_bridge_gates, armored_step_distances,
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
    // 拠点・unitは山や海を迂回する装甲距離で最寄りの軸へ帰属させる。直線距離では、
    // 山の向こうの敵首都周辺が遠回りの別軸ではなく近く見える軸の需要に数えられる。
    let front_distances = RouteFrontDistances::new(map, master_data, &fronts);
    let nearest_front = |position: GridPosition| front_distances.nearest_route(map, position);
    let mut opportunity_values = vec![0_u64; fronts.len()];
    let mut friendly_values = vec![0_u64; fronts.len()];
    let mut enemy_values = vec![0_u64; fronts.len()];
    let mut capital_threat_values = vec![0_u64; fronts.len()];

    for entity in world.iter_entities() {
        let (Some(position), Some(property)) =
            (entity.get::<GridPosition>(), entity.get::<Property>())
        else {
            continue;
        };
        if property.owner_id == Some(player_id)
            || property.terrain == Terrain::Capital
            || island_map
                .get_island_at(position)
                .is_none_or(|island| island.id != island_id)
        {
            continue;
        }
        let route = nearest_front(*position);
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
        opportunity_values[route] = opportunity_values[route].saturating_add(
            income
                .saturating_add(production_value)
                .saturating_mul(remaining_turns),
        );
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
            let force_deficit_value = enemy_values[route].saturating_sub(friendly_values[route]);
            let capital_threat = capital_threat_values[route];
            RouteForceDemand {
                opportunity_value,
                force_deficit_value,
                demand_value: opportunity_value
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
                    let front_distances = RouteFrontDistances::new(map, master_data, &route_fronts);
                    let nearest_front =
                        |position: GridPosition| front_distances.nearest_route(map, position);
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
