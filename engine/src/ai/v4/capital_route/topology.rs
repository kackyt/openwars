//! 首都ルートの静的地形トポロジーと準備。

use crate::ai::v4::capital_route::model::{
    CapitalRoute, CapitalRouteId, CapitalRouteNode, CapitalRouteNodeId, CapitalRouteTopology,
    CapitalRouteTopologyKey, CapitalRouteTopologyRegistry,
};
use crate::ai::v4::{
    GridPosition, HashMap, HashSet, Map, MasterDataRegistry, MovementType, PlayerId, Property,
    Terrain, VecDeque, World, get_valid_movement_cost,
};

/// 同一陸塊の首都攻略本隊を解放できるだけ、勝利ルート上の前線が進んだかを判定する。
/// 後方・側方の補助目標の所有数は、首都へ向かう主線のGo条件には含めない。
pub(in crate::ai::v4) fn same_land_capital_front_reached(
    route_distance: u32,
    forward_distance: u32,
) -> bool {
    forward_distance.saturating_mul(2) <= route_distance.max(1)
}

/// 指定した通行可能マスを連結成分へ分け、座標から成分IDを引けるようにする。
pub(in crate::ai::v4) fn terrain_component_membership(
    map: &Map,
    cells: &HashSet<GridPosition>,
) -> HashMap<GridPosition, usize> {
    let mut unseen = cells.clone();
    let mut membership = HashMap::default();
    let mut component_id = 0;
    while let Some(start) = unseen
        .iter()
        .min_by_key(|position| (position.y, position.x))
        .copied()
    {
        unseen.remove(&start);
        let mut queue = VecDeque::from([start]);
        while let Some(position) = queue.pop_front() {
            membership.insert(position, component_id);
            for (x, y) in map.get_adjacent(position.x, position.y) {
                let adjacent = GridPosition { x, y };
                if unseen.remove(&adjacent) {
                    queue.push_back(adjacent);
                }
            }
        }
        component_id += 1;
    }
    membership
}

/// 装甲部隊が首都側の地域から最初に通る橋Gateを抽出する。
/// 同じ川に橋が3本以上あっても、最も離れた2本を独立した作戦軸として採用する。
pub(in crate::ai::v4) fn armored_entry_bridge_gates(
    map: &Map,
    master_data: &MasterDataRegistry,
    home: GridPosition,
    enemy_capital: GridPosition,
) -> Vec<GridPosition> {
    let armored_cells = (0..map.height)
        .flat_map(|y| (0..map.width).map(move |x| GridPosition { x, y }))
        .filter(|position| {
            map.get_terrain(position.x, position.y)
                .is_some_and(|terrain| {
                    get_valid_movement_cost(master_data, MovementType::Tank, terrain).is_some()
                })
        })
        .collect::<HashSet<_>>();
    if !armored_cells.contains(&home) || !armored_cells.contains(&enemy_capital) {
        return Vec::new();
    }
    let maneuver_cells = armored_cells
        .iter()
        .filter(|position| map.get_terrain(position.x, position.y) != Some(Terrain::Bridge))
        .copied()
        .collect::<HashSet<_>>();
    let maneuver_membership = terrain_component_membership(map, &maneuver_cells);
    let Some(source_component) = maneuver_membership.get(&home).copied() else {
        return Vec::new();
    };
    let bridge_cells = armored_cells
        .iter()
        .filter(|position| map.get_terrain(position.x, position.y) == Some(Terrain::Bridge))
        .copied()
        .collect::<HashSet<_>>();
    let bridge_membership = terrain_component_membership(map, &bridge_cells);
    let mut bridge_groups: HashMap<usize, Vec<GridPosition>> = HashMap::default();
    for (position, component) in bridge_membership {
        bridge_groups.entry(component).or_default().push(position);
    }
    let mut gates = bridge_groups
        .into_values()
        .filter_map(|group| {
            let adjacent_components = group
                .iter()
                .flat_map(|position| map.get_adjacent(position.x, position.y))
                .filter_map(|(x, y)| maneuver_membership.get(&GridPosition { x, y }).copied())
                .collect::<HashSet<_>>();
            (adjacent_components.contains(&source_component) && adjacent_components.len() >= 2)
                .then(|| {
                    group
                        .into_iter()
                        .min_by_key(|position| (position.y, position.x))
                        .expect("空でない橋連結成分")
                })
        })
        .collect::<Vec<_>>();
    gates.sort_unstable_by_key(|position| (position.y, position.x));
    if gates.len() <= 2 {
        return gates;
    }
    let mut best_pair = (gates[0], gates[1]);
    let mut best_distance =
        map.distance(best_pair.0.x, best_pair.0.y, best_pair.1.x, best_pair.1.y);
    for (index, first) in gates.iter().enumerate() {
        for second in gates.iter().skip(index + 1) {
            let distance = map.distance(first.x, first.y, second.x, second.y);
            if distance > best_distance {
                best_distance = distance;
                best_pair = (*first, *second);
            }
        }
    }
    let mut selected = vec![best_pair.0, best_pair.1];
    selected.sort_unstable_by_key(|position| (position.y, position.x));
    selected
}

/// 戦車が通行できるマスだけを使い、始点からの最短ステップ距離を求める。
/// 地形コストの厳密な消費量ではなく、回廊が何本に分かれるかを判定するための探索である。
pub(in crate::ai::v4) fn movement_step_distances(
    map: &Map,
    master_data: &MasterDataRegistry,
    start: GridPosition,
    movement_type: MovementType,
) -> HashMap<GridPosition, u32> {
    let mut distances = HashMap::default();
    if !map.get_terrain(start.x, start.y).is_some_and(|terrain| {
        get_valid_movement_cost(master_data, movement_type, terrain).is_some()
    }) {
        return distances;
    }
    distances.insert(start, 0);
    let mut queue = VecDeque::from([start]);
    while let Some(position) = queue.pop_front() {
        let distance = distances[&position];
        for (x, y) in map.get_adjacent(position.x, position.y) {
            let adjacent = GridPosition { x, y };
            if distances.contains_key(&adjacent)
                || !map.get_terrain(x, y).is_some_and(|terrain| {
                    get_valid_movement_cost(master_data, movement_type, terrain).is_some()
                })
            {
                continue;
            }
            distances.insert(adjacent, distance.saturating_add(1));
            queue.push_back(adjacent);
        }
    }
    distances
}

pub(in crate::ai::v4) fn armored_step_distances(
    map: &Map,
    master_data: &MasterDataRegistry,
    start: GridPosition,
) -> HashMap<GridPosition, u32> {
    movement_step_distances(map, master_data, start, MovementType::Tank)
}

/// 橋のない山岳地形でも、両首都の間で独立して続く装甲回廊を最大2本返す。
/// 回廊中央の薄い帯が別連結成分になることを利用し、細かなセル違いは別ルートに数えない。
pub(in crate::ai::v4) fn armored_parallel_route_fronts(
    map: &Map,
    master_data: &MasterDataRegistry,
    home: GridPosition,
    enemy_capital: GridPosition,
) -> Vec<GridPosition> {
    let from_home = armored_step_distances(map, master_data, home);
    let Some(total_distance) = from_home.get(&enemy_capital).copied() else {
        return Vec::new();
    };
    // 短距離のセル違いを複数Campaignへ分けない。map_6級の長い山岳回廊だけを対象にする。
    if total_distance < 18 {
        return Vec::new();
    }
    let from_enemy = armored_step_distances(map, master_data, enemy_capital);
    let detour = (total_distance / 4).clamp(4, 10);
    let corridor = from_home
        .iter()
        .filter_map(|(position, from_start)| {
            let from_goal = from_enemy.get(position)?;
            (from_start.saturating_add(*from_goal) <= total_distance.saturating_add(detour))
                .then_some(*position)
        })
        .collect::<HashSet<_>>();
    // 中間地点ではなく敵寄り2/3地点で回廊を切る。占領済みの中継拠点に張り付かず、
    // 首都へ向かう次段の目標としても使える。
    // 中盤の固定地点ではなく、複数の進行帯を走査して最も分かれている地点を使う。
    // map_6 のように山岳の分岐が中盤より手前にある地図で、敵側 2/3 だけを見ると
    // 合流後の 1 本を誤って「唯一のルート」と数えるためである。
    let band_width = (total_distance / 6).clamp(3, 6);
    let mut best_center = 0;
    let mut fronts = Vec::new();
    let mut center = band_width;
    while center < total_distance {
        let lower = center.saturating_sub(band_width / 2);
        let upper = center.saturating_add(band_width / 2).min(total_distance);
        let band = corridor
            .iter()
            .filter(|position| {
                from_home
                    .get(position)
                    .is_some_and(|distance| (lower..=upper).contains(distance))
            })
            .copied()
            .collect::<HashSet<_>>();
        let membership = terrain_component_membership(map, &band);
        let mut groups: HashMap<usize, Vec<GridPosition>> = HashMap::default();
        for position in band {
            if let Some(component) = membership.get(&position) {
                groups.entry(*component).or_default().push(position);
            }
        }
        let mut candidates = groups
            .into_values()
            .filter(|group| {
                let progress = group
                    .iter()
                    .filter_map(|position| from_home.get(position))
                    .copied()
                    .collect::<Vec<_>>();
                progress
                    .iter()
                    .min()
                    .is_some_and(|minimum| *minimum <= lower.saturating_add(1))
                    && progress
                        .iter()
                        .max()
                        .is_some_and(|maximum| *maximum >= upper.saturating_sub(1))
            })
            .filter_map(|group| {
                group.into_iter().min_by_key(|position| {
                    (
                        from_home
                            .get(position)
                            .copied()
                            .unwrap_or(u32::MAX)
                            .abs_diff(center),
                        from_enemy.get(position).copied().unwrap_or(u32::MAX),
                        position.y,
                        position.x,
                    )
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|position| (position.y, position.x));
        if candidates.len() > fronts.len()
            || (candidates.len() == fronts.len() && center > best_center)
        {
            fronts = candidates;
            best_center = center;
        }
        center = center.saturating_add(band_width);
    }
    fronts.sort_unstable_by_key(|position| (position.y, position.x));
    if fronts.len() <= 2 {
        return fronts;
    }
    let mut best_pair = (fronts[0], fronts[1]);
    let mut best_distance =
        map.distance(best_pair.0.x, best_pair.0.y, best_pair.1.x, best_pair.1.y);
    for (index, first) in fronts.iter().enumerate() {
        for second in fronts.iter().skip(index + 1) {
            let distance = map.distance(first.x, first.y, second.x, second.y);
            if distance > best_distance {
                best_distance = distance;
                best_pair = (*first, *second);
            }
        }
    }
    let mut selected = vec![best_pair.0, best_pair.1];
    selected.sort_unstable_by_key(|position| (position.y, position.x));
    selected
}

/// 首都間の装甲進軍軸を、橋のGate優先で最大2本に正規化する。
///
/// `same_land_route_fronts` と島Campaign候補の生成が別々の定義を持つと、
/// 生産した増援がSquad側の軸を知らないままになる。両者はこの純粋関数を共有する。
pub(crate) fn armored_capital_route_fronts(
    map: &Map,
    master_data: &MasterDataRegistry,
    home: GridPosition,
    enemy_capital: GridPosition,
) -> Vec<GridPosition> {
    let gates = armored_entry_bridge_gates(map, master_data, home, enemy_capital);
    if gates.len() >= 2 {
        gates
    } else {
        armored_parallel_route_fronts(map, master_data, home, enemy_capital)
    }
}

/// 同一陸塊の首都間にある、現在プレイヤー側から見た装甲進出Gateを返す。
/// 島IDだけでは川・山で分かれた実際の進軍軸を区別できないため、Squad計画とも共有する。
pub(crate) fn same_land_armored_route_gates(
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
    armored_entry_bridge_gates(map, master_data, *home, *enemy_capital)
}

/// 構築済みTopologyから、指定島のrouteを返す。
pub(in crate::ai::v4) fn capital_route_topology_for(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Option<&CapitalRouteTopology> {
    let key = CapitalRouteTopologyKey {
        player_id,
        island_id,
    };
    world
        .get_resource::<CapitalRouteTopologyRegistry>()
        .and_then(|registry| registry.topologies.get(&key))
}

/// 橋と山岳回廊を同じ「進軍軸の前方地点」として返す。
///
/// 実戦では開始時に作ったTopologyを再利用する。unit testなど、まだResourceを
/// 初期化していない呼び出しだけは純粋な地形解析へフォールバックする。
pub(crate) fn same_land_route_fronts(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Vec<GridPosition> {
    if let Some(topology) = capital_route_topology_for(world, player_id, island_id) {
        return topology.routes.iter().map(|route| route.front).collect();
    }
    derive_same_land_route_fronts(world, player_id, island_id)
}

/// Topology初期化時だけ使う、首都間の純粋な地形解析。
pub(in crate::ai::v4) fn derive_same_land_route_fronts(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Vec<GridPosition> {
    let gates = same_land_armored_route_gates(world, player_id, island_id);
    if gates.len() >= 2 {
        return gates;
    }
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
    armored_capital_route_fronts(map, master_data, *home, *enemy_capital)
}

/// 指定勢力から見た同一陸塊の首都対を返す。
pub(in crate::ai::v4) fn same_land_capitals(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Option<(GridPosition, GridPosition)> {
    let island_map = world.get_resource::<crate::ai::islands::IslandMap>()?;
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
        } else if property.owner_id.is_some_and(|owner| owner != player_id) {
            enemy_capitals.push(*position);
        }
    }
    own_capitals.sort_unstable_by_key(|position| (position.y, position.x));
    enemy_capitals.sort_unstable_by_key(|position| (position.y, position.x));
    Some((*own_capitals.first()?, *enemy_capitals.first()?))
}

/// 自首都からの進行距離・y・xの順で、回廊上の座標を決定的に並べるキー。
///
/// 回廊外の座標は最後に並べる。中継点作戦の前提関係は自首都側から評価するため、
/// 作戦の走査順と回廊セルの並びの両方でこのキーを使う。
pub(in crate::ai::v4) fn progress_order(
    from_home: &HashMap<GridPosition, u32>,
    position: GridPosition,
) -> (u32, usize, usize) {
    (
        from_home.get(&position).copied().unwrap_or(u32::MAX),
        position.y,
        position.x,
    )
}

/// 静的な地形・拠点配置から、勢力ごとの前向き首都攻略DAGを一度だけ構築する。
///
/// ノードは両首都を結ぶ最短装甲回廊のセル、辺は自首都からの距離が一つ増える移動だけで
/// 構成する。従って回廊の分岐と合流は表せる一方、後退辺による循環は入り込まない。
pub(in crate::ai::v4) fn build_capital_route_topology(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Option<CapitalRouteTopology> {
    let map = world.get_resource::<Map>()?;
    let master_data = world.get_resource::<MasterDataRegistry>()?;
    let island_map = world.get_resource::<crate::ai::islands::IslandMap>()?;
    // 自軍首都の無い敵首都島では、揚陸作戦の橋頭堡を起点にして島内の首都攻略を進める。
    let (home, enemy_capital, beachhead) = match same_land_capitals(world, player_id, island_id) {
        Some((home, enemy_capital)) => (home, enemy_capital, false),
        None => {
            // 調整中の作戦（TODO: 評価で有効性を確認したら環境変数の切替を外す）。
            if std::env::var("OPENWARS_BEACHHEAD_DAG").is_err() {
                return None;
            }
            let (home, enemy_capital) = beachhead_capitals(world, player_id, island_id)?;
            (home, enemy_capital, true)
        }
    };
    let from_home = armored_step_distances(map, master_data, home);
    let from_enemy_capital = armored_step_distances(map, master_data, enemy_capital);
    let total_distance = from_home.get(&enemy_capital).copied()?;
    let mut fronts = if beachhead {
        armored_capital_route_fronts(map, master_data, home, enemy_capital)
    } else {
        derive_same_land_route_fronts(world, player_id, island_id)
    };
    // 分岐がない地図も「routeが無い」のではなく、一本の首都進軍路である。
    // ここをTopologyなしへ落とすと、map_1/map_2ではDAG・Squad・生産が再び別系統に
    // なってしまう。最短装甲回廊の中間セルを一枝の識別点にし、単一路と分岐路を
    // 同じ親戦役として扱う。
    if fronts.is_empty() {
        let halfway = total_distance / 2;
        let front = from_home
            .iter()
            .filter_map(|(position, from_start)| {
                let from_goal = from_enemy_capital.get(position)?;
                (from_start.saturating_add(*from_goal) == total_distance)
                    .then_some((*position, *from_start))
            })
            .min_by_key(|(position, progress)| (progress.abs_diff(halfway), position.y, position.x))
            .map(|(position, _)| position)?;
        fronts.push(front);
    }
    let nearest_route = |position: GridPosition| {
        fronts
            .iter()
            .enumerate()
            .min_by_key(|(route, front)| {
                (
                    map.distance(position.x, position.y, front.x, front.y),
                    *route,
                )
            })
            .map_or(0, |(route, _)| route)
    };
    let mut property_routes = HashMap::default();
    for entity in world.iter_entities() {
        let (Some(position), Some(_property)) =
            (entity.get::<GridPosition>(), entity.get::<Property>())
        else {
            continue;
        };
        if *position == home
            || island_map
                .get_island_at(position)
                .is_none_or(|island| island.id != island_id)
        {
            continue;
        }
        let route = nearest_route(*position);
        property_routes.insert(*position, CapitalRouteId(route));
    }
    let detour = (total_distance / 4).clamp(4, 10);
    let mut corridor_positions = from_home
        .iter()
        .filter_map(|(position, from_start)| {
            let from_goal = from_enemy_capital.get(position)?;
            (from_start.saturating_add(*from_goal) <= total_distance.saturating_add(detour))
                .then_some(*position)
        })
        .collect::<Vec<_>>();
    corridor_positions.sort_unstable_by_key(|position| progress_order(&from_home, *position));
    let node_ids = corridor_positions
        .iter()
        .enumerate()
        .map(|(index, position)| (*position, CapitalRouteNodeId(index)))
        .collect::<HashMap<_, _>>();
    let start_node = *node_ids.get(&home)?;
    let goal_node = *node_ids.get(&enemy_capital)?;
    let mut nodes = corridor_positions
        .iter()
        .map(|position| CapitalRouteNode {
            anchor: *position,
            successor_nodes: Vec::new(),
        })
        .collect::<Vec<_>>();
    for (position, node_id) in &node_ids {
        if *node_id == goal_node {
            continue;
        }
        // 回廊セルは必ず距離表に載っている。辺は距離が一つ増える隣接セルへだけ張る。
        let progress = from_home[position];
        nodes[node_id.0].successor_nodes = map
            .get_adjacent(position.x, position.y)
            .into_iter()
            .map(|(x, y)| GridPosition { x, y })
            .filter(|adjacent| from_home.get(adjacent) == Some(&progress.saturating_add(1)))
            .filter_map(|adjacent| node_ids.get(&adjacent).copied())
            .collect();
    }
    let routes = fronts
        .into_iter()
        .map(|front| CapitalRoute { front })
        .collect();
    Some(CapitalRouteTopology {
        start_node,
        goal_node,
        nodes,
        corridor: corridor_positions,
        routes,
        property_routes,
        from_home,
        beachhead,
    })
}

/// 自軍首都の無い敵首都島での、首都攻略DAGの起点（橋頭堡）と敵首都。
///
/// 起点は揚陸作戦の浜（停泊セル）に接する島内のセルのうち、敵首都へ装甲移動で
/// 最も近いセルとする。揚陸作戦がこの島を目標にしていなければNone。
pub(in crate::ai::v4) fn beachhead_capitals(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Option<(GridPosition, GridPosition)> {
    let map = world.get_resource::<Map>()?;
    let master_data = world.get_resource::<MasterDataRegistry>()?;
    let island_map = world.get_resource::<crate::ai::islands::IslandMap>()?;
    let on_island = |position: &GridPosition| {
        island_map
            .get_island_at(position)
            .is_some_and(|island| island.id == island_id)
    };
    let mut enemy_capitals = Vec::new();
    for entity in world.iter_entities() {
        let (Some(position), Some(property)) =
            (entity.get::<GridPosition>(), entity.get::<Property>())
        else {
            continue;
        };
        if property.terrain != Terrain::Capital || !on_island(position) {
            continue;
        }
        if property.owner_id == Some(player_id) {
            // 自軍首都がある島は同一陸塊の首都戦役として扱う。
            return None;
        }
        if property.owner_id.is_some() {
            enemy_capitals.push(*position);
        }
    }
    enemy_capitals.sort_unstable_by_key(|position| (position.y, position.x));
    let enemy_capital = *enemy_capitals.first()?;
    let plan = world
        .get_resource::<crate::ai::v4::maritime::sealift::AssaultPlans>()?
        .0
        .get(&player_id)?;
    let from_capital = armored_step_distances(map, master_data, enemy_capital);
    let origin = plan
        .stops
        .iter()
        .flat_map(|stop| map.get_adjacent(stop.0, stop.1))
        .map(|(x, y)| GridPosition { x, y })
        .filter(|cell| on_island(cell))
        .filter_map(|cell| Some((*from_capital.get(&cell)?, cell)))
        .min_by_key(|(distance, cell)| (*distance, cell.y, cell.x))
        .map(|(_, cell)| cell)?;
    Some((origin, enemy_capital))
}

/// 島に自軍の地上部隊（搭載中を除く）が居るか。
fn has_own_ground_on_island(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> bool {
    let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() else {
        return false;
    };
    world.iter_entities().any(|entity| {
        entity
            .get::<crate::components::Faction>()
            .is_some_and(|faction| faction.0 == player_id)
            && entity.get::<crate::components::Transporting>().is_none()
            && entity
                .get::<crate::components::UnitStats>()
                .is_some_and(|stats| {
                    !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
                })
            && entity
                .get::<GridPosition>()
                .and_then(|position| island_map.get_island_at(position))
                .is_some_and(|island| island.id == island_id)
    })
}

/// V4の手番開始時に、同一陸塊の首都戦役を一度だけ解析してResourceへ保存する。
pub(super) fn cache_capital_route_topologies(world: &mut World, player_id: PlayerId) {
    let island_ids = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .map(|island_map| {
            island_map
                .islands
                .iter()
                .map(|island| island.id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut newly_built = Vec::new();
    for island_id in island_ids {
        let key = CapitalRouteTopologyKey {
            player_id,
            island_id,
        };
        let existing_origin = world
            .get_resource::<CapitalRouteTopologyRegistry>()
            .and_then(|registry| registry.topologies.get(&key))
            .map(|topology| {
                (
                    topology.beachhead,
                    topology.nodes[topology.start_node.0].anchor,
                )
            });
        let rebuild = match existing_origin {
            None => true,
            // 橋頭堡のDAGは、まだ誰も上陸していない間だけ揚陸作戦の浜の変更に合わせて
            // 作り直す。上陸後は起点を固定し、上陸部隊の前進順序を揺らさない。
            Some((true, origin)) => {
                !has_own_ground_on_island(world, player_id, island_id)
                    && beachhead_capitals(world, player_id, island_id)
                        .is_some_and(|(new_origin, _)| new_origin != origin)
            }
            Some((false, _)) => false,
        };
        if rebuild && let Some(topology) = build_capital_route_topology(world, player_id, island_id)
        {
            if topology.beachhead && std::env::var("OPENWARS_ASSAULT_TRACE").is_ok() {
                eprintln!(
                    "BEACHHEAD_DAG p{} island{:?} origin{:?} goal{:?} routes{}",
                    player_id.0,
                    island_id,
                    topology.nodes[topology.start_node.0].anchor,
                    topology.nodes[topology.goal_node.0].anchor,
                    topology.routes.len()
                );
            }
            newly_built.push((key, topology));
        }
    }
    if !newly_built.is_empty() {
        let mut registry = world
            .remove_resource::<CapitalRouteTopologyRegistry>()
            .unwrap_or_default();
        // 作り直したDAGの旧Milestoneの作戦状態を残さない。
        if let Some(mut operations) = world
            .get_resource_mut::<crate::ai::v4::capital_route::model::CapitalRouteNodeOperationRegistry>()
        {
            operations.operations.retain(|id, _| {
                !newly_built
                    .iter()
                    .any(|(key, _)| registry.topologies.contains_key(key) && id.topology == *key)
            });
        }
        registry.topologies.extend(newly_built);
        world.insert_resource(registry);
    }
}

/// 指定島が自首都と敵首都をともに含む、陸路主体の首都戦役かを返す。
/// 海洋作戦では上陸隊を一点へ集める価値があるため、陸上の分散規則を混ぜない。
pub(crate) fn is_same_land_capital_island(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> bool {
    let Some(map) = world.get_resource::<Map>() else {
        return false;
    };
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
        .unwrap_or_else(|| crate::ai::islands::IslandMap::analyze(map));
    let mut has_own_capital = false;
    let mut has_enemy_capital = false;
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
        has_own_capital |= property.owner_id == Some(player_id);
        has_enemy_capital |= property
            .owner_id
            .is_some_and(|owner_id| owner_id != player_id);
    }
    has_own_capital && has_enemy_capital
}
