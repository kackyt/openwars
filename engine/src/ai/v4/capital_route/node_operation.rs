//! DAG Nodeの作戦構築・盤面観測と出口条件。

use crate::ai::v4::capital_route::model::{
    CAPITAL_ROUTE_REGION_RADIUS, CapitalRouteExecutionPhase, CapitalRouteNodeId,
    CapitalRouteNodeObjective, CapitalRouteNodeOperation, CapitalRouteNodeOperationId,
    CapitalRouteNodeOperationRegistry, CapitalRouteNodeScope, CapitalRoutePathRegistry,
    CapitalRouteTopology, CapitalRouteTopologyRegistry,
};
use crate::ai::v4::capital_route::topology::{capital_route_topology_for, progress_order};
use crate::ai::v4::{
    Faction, GridPosition, HashMap, HashSet, Health, Map, MovementType, PlayerId, Property,
    Terrain, UnitStats, VecDeque, World, operation, victory_roadmap,
};

/// 地域を塗り分けるときの1Nodeの起点。
pub(in crate::ai::v4) struct RegionSeed<'a> {
    pub(in crate::ai::v4) anchor: GridPosition,
    pub(in crate::ai::v4) scope: CapitalRouteNodeScope,
    pub(in crate::ai::v4) capture_targets: &'a [GridPosition],
}

/// 同じ陸塊のNode地域を、代表点から同時に塗り広げて重なりなく分割する。
///
/// 各セルは、陸路（隣接マス）で最も近い代表点の地域にだけ属する。代表点・所属拠点から
/// `CAPITAL_ROUTE_REGION_RADIUS` マスを超えるセルはどの地域にも属さない。距離が同じなら
/// `seeds` の先に並ぶNodeが取る。これにより、ある地点の敵・味方は必ず一つの地域だけで
/// 数えられ、隣の地域と同じ敵を二重に「争奪中」と数えたり、橋の向こうの地域が手前の
/// 岸まで広がったりしない。
///
/// - Area: 代表点と所属拠点を起点に、同じ陸塊の陸地を塗る。橋のマスは塗らず、
///   川のマスは岸の地域に含めるがそこから先へは広げない。したがって川を挟む対岸へは
///   広がらない（橋の通過はPoint Nodeが表す）。
/// - Point（橋）: 代表点の1マスだけを持つ。向こう岸の出口セルは対岸の地域に属し、
///   通過したかどうかは `crossing_exit` で別に観測する。
///
/// 戻り値は `seeds` と同じ順の地域セル列（y・x順）。
pub(in crate::ai::v4) fn paint_capital_route_regions(
    map: &Map,
    island_map: &crate::ai::islands::IslandMap,
    island_id: crate::ai::islands::IslandId,
    seeds: &[RegionSeed<'_>],
) -> Vec<Vec<GridPosition>> {
    let mut owner: HashMap<GridPosition, usize> = HashMap::default();
    let mut queue = VecDeque::new();
    // 起点（代表点と所属拠点）を先に確定する。拠点は所属Nodeの地域から外さない。
    for (index, seed) in seeds.iter().enumerate() {
        match seed.scope {
            CapitalRouteNodeScope::Point => {
                owner.entry(seed.anchor).or_insert(index);
            }
            CapitalRouteNodeScope::Area => {
                for cell in std::iter::once(&seed.anchor).chain(seed.capture_targets) {
                    if !owner.contains_key(cell) {
                        owner.insert(*cell, index);
                        queue.push_back((*cell, 0_u32));
                    }
                }
            }
        }
    }
    // 全起点から1マスずつ同時に広げる（幅優先）。先に届いた地域がセルを取る。
    while let Some((cell, distance)) = queue.pop_front() {
        // 競合する代表点が無い端の地域が後背地全体へ膨らむと、遠方の敵まで抱えて
        // 争奪中のまま出口条件を満たせなくなる。起点から一定距離で塗るのを止める。
        if distance >= CAPITAL_ROUTE_REGION_RADIUS {
            continue;
        }
        let index = owner[&cell];
        for (x, y) in map.get_adjacent(cell.x, cell.y) {
            let next = GridPosition { x, y };
            if owner.contains_key(&next)
                || map.get_terrain(x, y) == Some(Terrain::Bridge)
                || island_map
                    .get_island_at(&next)
                    .is_none_or(|island| island.id != island_id)
            {
                continue;
            }
            owner.insert(next, index);
            // 川のマスは岸の地域に含めるが、そこから対岸へは塗り広げない。
            // 川を挟む地域は橋（Point）の通過で分ける。
            if map.get_terrain(x, y) != Some(Terrain::River) {
                queue.push_back((next, distance + 1));
            }
        }
    }
    let mut regions = vec![Vec::new(); seeds.len()];
    for (cell, index) in owner {
        regions[index].push(cell);
    }
    for region in &mut regions {
        region.sort_unstable_by_key(|position| (position.y, position.x));
    }
    regions
}

/// 拠点を、最も近い回廊セルへ占領目標として結びつける。
///
/// 回廊から `CAPITAL_ROUTE_REGION_RADIUS` を超えて離れた拠点は取り込まない。島全域の
/// 占領を1セルに背負わせず、その地域で意味を持つ周辺拠点だけを出口条件にする。
/// 等距離のセルが複数あれば自首都側（`corridor` の先頭側）を選ぶ。戻り値は目標を持つ
/// セルだけを `corridor` の順に並べ、各セルの目標は y・x 順に整列・重複除去する。
pub(in crate::ai::v4) fn attach_capture_targets_to_corridor(
    map: &Map,
    corridor: &[GridPosition],
    properties: impl IntoIterator<Item = GridPosition>,
) -> Vec<(GridPosition, Vec<GridPosition>)> {
    let mut targets_by_cell = vec![Vec::new(); corridor.len()];
    for position in properties {
        let Some((cell, distance)) = corridor
            .iter()
            .enumerate()
            .map(|(index, cell)| (index, map.distance(position.x, position.y, cell.x, cell.y)))
            .min_by_key(|(index, distance)| (*distance, *index))
        else {
            continue;
        };
        if distance <= CAPITAL_ROUTE_REGION_RADIUS {
            targets_by_cell[cell].push(position);
        }
    }
    corridor
        .iter()
        .zip(targets_by_cell)
        .filter(|(_, targets)| !targets.is_empty())
        .map(|(cell, mut targets)| {
            targets.sort_unstable_by_key(|position| (position.y, position.x));
            targets.dedup();
            (*cell, targets)
        })
        .collect()
}

/// 近接する二次元のCapture群を、一つのArea Operationへ統合する。
///
/// 同じ対象集合を複数Nodeへ複製すると、一拠点の奪回で全Nodeが同時に未完了へ戻る。
/// 先頭Nodeだけへ地域の全対象を持たせ、後続Nodeからは対象を除くことで、複数の
/// Capture Squadによる並行占領と、単一の完了判定を両立する。一直線の回廊は統合せず、
/// map_25のように前方へ順番に確保する。`capture_regions` は（代表セル, 占領目標）を
/// 自首都からの進行順に並べたもので、統合で空になった目標も要素として残す。
#[cfg(test)]
pub(in crate::ai::v4) fn consolidate_nearby_capture_target_regions(
    map: &Map,
    capture_regions: &mut [(GridPosition, Vec<GridPosition>)],
) {
    let mut start = 0;
    while start < capture_regions.len() {
        let first_anchor = capture_regions[start].0;
        let mut end = start + 1;
        while end < capture_regions.len()
            && map.distance(
                first_anchor.x,
                first_anchor.y,
                capture_regions[end].0.x,
                capture_regions[end].0.y,
            ) <= CAPITAL_ROUTE_REGION_RADIUS
        {
            end += 1;
        }
        let mut regional_targets = capture_regions[start..end]
            .iter()
            .flat_map(|(_, targets)| targets.iter().copied())
            .collect::<Vec<_>>();
        regional_targets.sort_unstable_by_key(|position| (position.y, position.x));
        regional_targets.dedup();
        let spans_multiple_columns = regional_targets
            .iter()
            .map(|position| position.x)
            .collect::<HashSet<_>>()
            .len()
            > 1;
        let spans_multiple_rows = regional_targets
            .iter()
            .map(|position| position.y)
            .collect::<HashSet<_>>()
            .len()
            > 1;
        if regional_targets.len() > 1 && spans_multiple_columns && spans_multiple_rows {
            capture_regions[start].1 = regional_targets;
            for (_, targets) in &mut capture_regions[start + 1..end] {
                targets.clear();
            }
        }
        start = end;
    }
}

/// 静的topologyからセルNode Operationの形だけを初期化する。
///
/// 実際の状態は毎手番の盤面観測で上書きするため、ここでは地形由来の前後関係だけを
/// 構築する。既存Operationを保持し、対戦中の再初期化で担当情報を失わない。
pub(in crate::ai::v4) fn ensure_capital_route_node_operations(
    world: &mut World,
    player_id: PlayerId,
) {
    let topologies = world
        .get_resource::<CapitalRouteTopologyRegistry>()
        .map(|registry| registry.topologies.clone())
        .unwrap_or_default();
    if topologies.is_empty() {
        return;
    }
    let Some(map) = world.get_resource::<Map>().cloned() else {
        return;
    };
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
        .unwrap_or_else(|| crate::ai::islands::IslandMap::analyze(&map));
    let properties = world
        .iter_entities()
        .filter_map(|entity| {
            Some((
                *entity.get::<GridPosition>()?,
                entity.get::<Property>()?.owner_id,
            ))
        })
        .collect::<Vec<_>>();
    let mut operations = world
        .remove_resource::<CapitalRouteNodeOperationRegistry>()
        .unwrap_or_default();
    for (key, topology) in topologies {
        if key.player_id != player_id {
            continue;
        }
        // 占領目標の結びつけはDAGのノード番号ではなく、回廊セルの集合で決める。
        let home = topology.nodes[topology.start_node.0].anchor;
        let mut capture_regions = attach_capture_targets_to_corridor(
            &map,
            &topology.corridor,
            properties
                .iter()
                .map(|(position, _)| *position)
                .filter(|position| {
                    *position != home
                        && island_map
                            .get_island_at(position)
                            .is_some_and(|island| island.id == key.island_id)
                }),
        );
        // 地域の代表点を先に選ぶ。終端の首都地域を優先し、地域内の施設は
        // 同じ作戦の並行目標とする。施設セルごとの前提Nodeは生成しない。
        let goal = topology.nodes[topology.goal_node.0].anchor;
        let mut representatives = vec![goal];
        let mut regional_targets: HashMap<GridPosition, Vec<GridPosition>> = HashMap::default();
        let belongs_to_region = |anchor: GridPosition, position: GridPosition| {
            let mut queue = VecDeque::from([(anchor, 0_u32)]);
            let mut visited = HashSet::from_iter([anchor]);
            while let Some((cell, distance)) = queue.pop_front() {
                if cell == position {
                    return true;
                }
                if distance >= CAPITAL_ROUTE_REGION_RADIUS {
                    continue;
                }
                for (x, y) in map.get_adjacent(cell.x, cell.y) {
                    let next = GridPosition { x, y };
                    if map.get_terrain(x, y) != Some(Terrain::Bridge)
                        && island_map
                            .get_island_at(&next)
                            .is_some_and(|island| island.id == key.island_id)
                        && visited.insert(next)
                    {
                        queue.push_back((next, distance + 1));
                    }
                }
            }
            false
        };
        for (anchor, targets) in &capture_regions {
            let representative = representatives
                .iter()
                .copied()
                .find(|region| belongs_to_region(*region, *anchor))
                .unwrap_or_else(|| {
                    representatives.push(*anchor);
                    *anchor
                });
            regional_targets
                .entry(representative)
                .or_default()
                .extend(targets.iter().copied());
        }
        capture_regions = representatives
            .iter()
            .filter_map(|anchor| {
                regional_targets.remove(anchor).map(|mut targets| {
                    targets.sort_unstable_by_key(|position| (position.y, position.x));
                    targets.dedup();
                    (*anchor, targets)
                })
            })
            .collect();
        let capture_targets = capture_regions
            .into_iter()
            .filter(|(_, targets)| !targets.is_empty())
            .collect::<HashMap<_, _>>();
        let no_targets = Vec::new();
        let capture_targets_at = |index: usize| {
            capture_targets
                .get(&topology.nodes[index].anchor)
                .unwrap_or(&no_targets)
        };
        // 移動用のセルDAGを、そのまま作戦Nodeにはしない。全セルをNodeにすると、1マス
        // ごとに次手番の状態更新を待ち、開けた地形の代替経路まで全て制圧する誤った
        // 前提DAGになる。地形・拠点スキャンで意味のあるMilestoneだけを選ぶ。
        let mut operation_indices =
            HashSet::from_iter([topology.start_node.0, topology.goal_node.0]);
        operation_indices.extend(
            capture_targets
                .keys()
                .filter_map(|anchor| topology.node_at(*anchor))
                .map(|node| node.0),
        );
        operation_indices.extend(
            topology
                .routes
                .iter()
                .filter(|route| {
                    !representatives
                        .iter()
                        .any(|region| belongs_to_region(*region, route.front))
                })
                .filter_map(|route| topology.node_at(route.front))
                .map(|node| node.0),
        );
        for index in 0..topology.nodes.len() {
            if let Some(exit) =
                capital_route_crossing_exit(&map, &topology, CapitalRouteNodeId(index))
            {
                operation_indices.insert(index);
                if let Some(exit_node) = topology.node_at(exit) {
                    operation_indices.insert(exit_node.0);
                }
            }
        }
        let mut operation_indices = operation_indices.into_iter().collect::<Vec<_>>();
        operation_indices.sort_unstable();
        let operation_index_set = operation_indices.iter().copied().collect::<HashSet<_>>();
        let operation_anchor_set = operation_indices
            .iter()
            .map(|&index| topology.nodes[index].anchor)
            .collect::<HashSet<_>>();

        // 圧縮後のedgeは、あるMilestoneから次のMilestoneへ到達可能という地理的な
        // 選択肢を表す。途中セルは移動経路へ残すが、作戦の状態遷移を増やさない。
        let mut operation_successors = vec![Vec::new(); topology.nodes.len()];
        for &origin in &operation_indices {
            let mut pending = VecDeque::from(topology.nodes[origin].successor_nodes.clone());
            let mut visited = HashSet::default();
            while let Some(candidate) = pending.pop_front() {
                if !visited.insert(candidate) {
                    continue;
                }
                if operation_index_set.contains(&candidate.0) {
                    operation_successors[origin].push(candidate);
                    continue;
                }
                pending.extend(topology.nodes[candidate.0].successor_nodes.iter().copied());
            }
            operation_successors[origin].sort_unstable_by_key(|node| node.0);
            operation_successors[origin].dedup();
        }
        let mut operation_predecessors = vec![Vec::new(); topology.nodes.len()];
        for &origin in &operation_indices {
            for successor in &operation_successors[origin] {
                operation_predecessors[successor.0].push(CapitalRouteNodeId(origin));
            }
        }
        for predecessors in &mut operation_predecessors {
            predecessors.sort_unstable_by_key(|node| node.0);
            predecessors.dedup();
        }

        // topologyの作り直しや開発中のHot reloadで旧セルNodeが残らないよう、同じ
        // topologyの作戦台帳を現在のMilestone集合へそろえる。既存Milestoneのcrossedは
        // `entry`で保持するため、橋の通過実績を失わない。
        operations
            .operations
            .retain(|id, _| id.topology != key || operation_anchor_set.contains(&id.anchor));
        // 施設数や分岐数で地域を一マスへ縮めない。通常Nodeは周囲の地域を表し、
        // 橋の通過地点だけを地形上のPointとして扱う。
        let crossing_exits = operation_indices
            .iter()
            .map(|&index| capital_route_crossing_exit(&map, &topology, CapitalRouteNodeId(index)))
            .collect::<Vec<_>>();
        let seeds = operation_indices
            .iter()
            .zip(&crossing_exits)
            .map(|(&index, crossing_exit)| RegionSeed {
                anchor: topology.nodes[index].anchor,
                scope: if crossing_exit.is_some() {
                    CapitalRouteNodeScope::Point
                } else {
                    CapitalRouteNodeScope::Area
                },
                capture_targets: capture_targets_at(index),
            })
            .collect::<Vec<_>>();
        // 全Nodeの地域を同時に塗り分け、各セルを一つの地域だけに属させる。
        let regions = paint_capital_route_regions(&map, &island_map, key.island_id, &seeds);
        for ((&index, crossing_exit), (seed, control_area)) in operation_indices
            .iter()
            .zip(crossing_exits)
            .zip(seeds.iter().zip(regions))
        {
            let node = &topology.nodes[index];
            let id = CapitalRouteNodeOperationId {
                topology: key,
                anchor: node.anchor,
            };
            let scope = seed.scope;
            let objective = if capture_targets_at(index).is_empty() {
                CapitalRouteNodeObjective::Control
            } else {
                CapitalRouteNodeObjective::Capture
            };
            operations
                .operations
                .entry(id)
                .or_insert_with(|| CapitalRouteNodeOperation {
                    id,
                    anchor: node.anchor,
                    predecessors: operation_predecessors[index]
                        .iter()
                        .map(|node| CapitalRouteNodeOperationId {
                            topology: key,
                            anchor: topology.nodes[node.0].anchor,
                        })
                        .collect(),
                    successors: operation_successors[index]
                        .iter()
                        .map(|node| CapitalRouteNodeOperationId {
                            topology: key,
                            anchor: topology.nodes[node.0].anchor,
                        })
                        .collect(),
                    objective,
                    scope,
                    control_area,
                    capture_targets: capture_targets_at(index).clone(),
                    crossing_exit,
                    crossed: false,
                    local_control: false,
                    state: victory_roadmap::RoadmapNodeState::Locked,
                    assigned_squads: HashSet::default(),
                });
        }
    }
    world.insert_resource(operations);
}

/// 橋セルからDAGの進行方向へたどり、最初に到達する非橋セルを返す。
///
/// 橋の上に到着した事実と、障害物を越えて敵側へ出た事実を混同しないために使う。
pub(in crate::ai::v4) fn capital_route_crossing_exit(
    map: &Map,
    topology: &CapitalRouteTopology,
    node: CapitalRouteNodeId,
) -> Option<GridPosition> {
    (map.get_terrain(
        topology.nodes[node.0].anchor.x,
        topology.nodes[node.0].anchor.y,
    )? == Terrain::Bridge)
        .then_some(())?;
    let mut pending = VecDeque::from(topology.nodes[node.0].successor_nodes.clone());
    let mut visited = HashSet::default();
    while let Some(candidate) = pending.pop_front() {
        if !visited.insert(candidate) {
            continue;
        }
        let position = topology.nodes[candidate.0].anchor;
        if map.get_terrain(position.x, position.y) != Some(Terrain::Bridge) {
            return Some(position);
        }
        pending.extend(topology.nodes[candidate.0].successor_nodes.iter().copied());
    }
    None
}

/// Nodeの出口条件が満たされ、後続Nodeを解放してよいかを返す。
///
/// 地域Captureは局地優勢でも前衛を次地域へ送れる。施設の占領完了は別工程として
/// 残し、護衛・占領担当と施設予約はSecuredまで維持する。Pointは従来の出口を使う。
pub(in crate::ai::v4) fn capital_route_node_exit_satisfied(
    operation: &CapitalRouteNodeOperation,
) -> bool {
    match operation.objective {
        CapitalRouteNodeObjective::Control => matches!(
            operation.state,
            victory_roadmap::RoadmapNodeState::Dominant
                | victory_roadmap::RoadmapNodeState::Secured
        ),
        CapitalRouteNodeObjective::Capture => {
            operation.state == victory_roadmap::RoadmapNodeState::Secured
                || (operation.scope == CapitalRouteNodeScope::Area && operation.local_control)
        }
    }
}

/// 圧縮した地理DAGで、Nodeへ到達する代替経路のいずれかが開いたかを返す。
///
/// 同じ地点へ北回り・南回りのedgeが合流する場合、それらは「両方を制圧する」依存では
/// ない。一方の回廊で出口条件を満たせば、後段の同一Milestoneへ進める。
pub(in crate::ai::v4) fn capital_route_node_predecessors_released(
    operations: &HashMap<CapitalRouteNodeOperationId, CapitalRouteNodeOperation>,
    operation: &CapitalRouteNodeOperation,
) -> bool {
    operation.predecessors.is_empty()
        || operation.predecessors.iter().any(|predecessor| {
            operations
                .get(predecessor)
                .is_some_and(capital_route_node_exit_satisfied)
        })
}

/// Squadの到着ではなく、地域の敵味方・占領対象・橋通過実績からNode状態を更新する。
///
/// `Contested` は敵が実際に地域へ存在する場合だけを表す。Squadがまだanchorへ着いて
/// いないという移動中の事実はSquad phaseが保持し、Nodeの戦況状態へ混ぜない。
pub(in crate::ai::v4) fn refresh_capital_route_node_operations(
    world: &mut World,
    player_id: PlayerId,
    _manager: &crate::ai::squad::SquadManager,
) {
    ensure_capital_route_node_operations(world, player_id);
    let Some(topologies) = world
        .get_resource::<CapitalRouteTopologyRegistry>()
        .map(|registry| registry.topologies.clone())
    else {
        return;
    };
    let commitments = world
        .get_resource::<CapitalRoutePathRegistry>()
        .map(|registry| registry.commitments.clone())
        .unwrap_or_default();

    let properties = world
        .iter_entities()
        .filter_map(|entity| {
            Some((
                *entity.get::<GridPosition>()?,
                entity.get::<Property>()?.owner_id,
            ))
        })
        .collect::<HashMap<_, _>>();
    let unit_observations = world
        .iter_entities()
        .filter_map(|entity| {
            let faction = entity.get::<Faction>()?;
            let position = *entity.get::<GridPosition>()?;
            let stats = entity.get::<UnitStats>()?;
            let health = entity.get::<Health>()?;
            if entity.get::<crate::components::Transporting>().is_some() {
                return None;
            }
            // 非武装車両の価格は局地優勢や橋の通過戦力に加算しない。
            if stats.max_range == 0 && !stats.can_capture {
                return None;
            }
            Some((
                faction.0,
                position,
                u64::from(stats.cost)
                    .saturating_mul(u64::from(health.current))
                    .saturating_div(u64::from(health.max.max(1))),
                stats.can_capture,
                !matches!(stats.movement_type, MovementType::Air | MovementType::Ship),
            ))
        })
        .collect::<Vec<_>>();
    let mut operations = world
        .remove_resource::<CapitalRouteNodeOperationRegistry>()
        .unwrap_or_default();

    for (key, topology) in topologies {
        if key.player_id != player_id {
            continue;
        }
        let home = topology.nodes[topology.start_node.0].anchor;
        // 前提作戦を先に評価できるよう、自首都側から進行順に並べる。
        let mut operation_ids = operations
            .operations
            .keys()
            .copied()
            .filter(|id| id.topology == key)
            .collect::<Vec<_>>();
        operation_ids.sort_unstable_by_key(|id| topology.node_at(id.anchor).map(|node| node.0));
        // まず担当情報だけを毎手番リセットする。`crossed`は橋を越えた盤面事実なので
        // 消さず、Node状態は下の観測・dependency計算で再構築する。
        for id in &operation_ids {
            let Some(operation) = operations.operations.get_mut(id) else {
                continue;
            };
            operation.assigned_squads.clear();
            operation.state = if id.anchor == home {
                victory_roadmap::RoadmapNodeState::Secured
            } else {
                victory_roadmap::RoadmapNodeState::Locked
            };
        }
        // DAGのNode番号は依存順なので、前方への線形走査で前提Nodeの出口条件を
        // 評価できる。地形DAGの合流edgeは「複数経路のいずれから来てもよい」という
        // 代替関係であり、全経路の同時制圧を要求する依存辺ではない。
        for &id in &operation_ids {
            if id.anchor == home {
                continue;
            }
            let Some(operation) = operations.operations.get(&id).cloned() else {
                continue;
            };
            let predecessors_ready =
                capital_route_node_predecessors_released(&operations.operations, &operation);
            let mut friendly_power = 0_u64;
            let mut enemy_power = 0_u64;
            let mut friendly_ground_on_exit = false;
            let mut capturer_on_target = false;
            let mut friendly_ground_in_area = false;
            for (owner, position, power, can_capture, is_ground) in &unit_observations {
                if operation.control_area.contains(position) {
                    if *owner == player_id {
                        friendly_power = friendly_power.saturating_add(*power);
                        friendly_ground_in_area |= *is_ground;
                    } else {
                        enemy_power = enemy_power.saturating_add(*power);
                    }
                }
                if *owner == player_id && *is_ground && operation.crossing_exit == Some(*position) {
                    friendly_ground_on_exit = true;
                }
                if *owner == player_id
                    && *can_capture
                    && operation.capture_targets.contains(position)
                    && properties.get(position) != Some(&Some(player_id))
                {
                    capturer_on_target = true;
                }
            }

            // 依存辺は未到着の後続の進行条件。実際に別経路で地域へ入った
            // 前衛まで未観測のLockedへ戻さず、現地の争奪・占領状態を記録する。
            if !predecessors_ready && !friendly_ground_in_area && !friendly_ground_on_exit {
                if let Some(op) = operations.operations.get_mut(&id) {
                    op.local_control = false;
                }
                continue;
            }

            let owned_capture_targets = operation
                .capture_targets
                .iter()
                .filter(|target| properties.get(target) == Some(&Some(player_id)))
                .count();
            let capture_complete = operation.objective == CapitalRouteNodeObjective::Capture
                && owned_capture_targets == operation.capture_targets.len();
            let regional_capture_dominant = operation.objective
                == CapitalRouteNodeObjective::Capture
                && operation.scope == CapitalRouteNodeScope::Area
                && !operation.capture_targets.is_empty()
                && owned_capture_targets.saturating_mul(3)
                    >= operation.capture_targets.len().saturating_mul(2);
            let crossed = operation.crossed || friendly_ground_on_exit;
            let state = if capture_complete {
                victory_roadmap::RoadmapNodeState::Secured
            } else if operation.crossing_exit.is_some() && !crossed {
                // 橋の手前で局地優勢でも、向こう岸へ実際に出るまでは出口条件未達。
                if enemy_power > 0 {
                    victory_roadmap::RoadmapNodeState::Contested
                } else {
                    victory_roadmap::RoadmapNodeState::Ready
                }
            } else if operation.crossing_exit.is_some() && enemy_power == 0 {
                // 橋は一度出口へ到達した後、部隊がさらに前進しても通過済みである。
                // 敵が橋頭堡へ再侵入していない限りSecuredを保ち、後段Nodeを再ロック
                // しない。敵が戻れば下の戦力比較でContestedへ戻して再対応する。
                victory_roadmap::RoadmapNodeState::Secured
            } else if enemy_power > 0
                && friendly_power.saturating_mul(4) < enemy_power.saturating_mul(5)
            {
                // 地域内の敵戦力が同等以上なら、実際の競合としてContestedを維持する。
                victory_roadmap::RoadmapNodeState::Contested
            } else if regional_capture_dominant && friendly_power > 0 {
                // 地域の大半を確保して局地優勢なら前衛を次へ送る。未確保・奪回拠点が
                // 残るためSecuredとはせず、後続SquadがこのNodeを継続できる状態にする。
                victory_roadmap::RoadmapNodeState::Dominant
            } else if capturer_on_target {
                victory_roadmap::RoadmapNodeState::Capturing
            } else if friendly_power > 0 {
                victory_roadmap::RoadmapNodeState::Dominant
            } else {
                victory_roadmap::RoadmapNodeState::Ready
            };
            // 地域の通過許可は局地戦力から独立に観測する。占領中という状態名や
            // 未所有施設の割合で、優勢な前衛の前進を止めない。
            let dominant = friendly_power > 0
                && (enemy_power == 0
                    || friendly_power.saturating_mul(4) >= enemy_power.saturating_mul(5));
            if let Some(operation) = operations.operations.get_mut(&id) {
                operation.crossed |= friendly_ground_on_exit;
                operation.local_control = dominant;
                operation.state = state;
            }
        }
        // 各Squadは現在の地域Nodeだけを担当する。割当は診断と指令の対応付けに使うが、
        // Squadが未到着という事実でNode状態を上書きしてはならない。
        for (squad_id, commitment) in commitments.iter().filter(|(_, commitment)| {
            commitment.player_id == player_id
                && commitment.island_id == key.island_id
                // 待機線のSquadは同じ作戦に所属するが、前衛Nodeの担当数には含めない。
                // これによりNode診断のassigned_squadsが「実際に地域へ入る波」になる。
                && commitment.phase == CapitalRouteExecutionPhase::Advance
        }) {
            let id = CapitalRouteNodeOperationId {
                topology: key,
                anchor: commitment.target_anchor,
            };
            let Some(operation) = operations.operations.get_mut(&id) else {
                continue;
            };
            operation.assigned_squads.insert(*squad_id);
        }
        if topology.beachhead && std::env::var("OPENWARS_ASSAULT_TRACE").is_ok() {
            let turn = world
                .get_resource::<crate::resources::MatchState>()
                .map_or(0, |state| state.current_turn_number.0);
            let mut nodes = operations
                .operations
                .values()
                .filter(|operation| operation.id.topology == key)
                .collect::<Vec<_>>();
            nodes.sort_unstable_by_key(|operation| {
                progress_order(&topology.from_home, operation.anchor)
            });
            let text = nodes
                .iter()
                .map(|operation| {
                    let owned = operation
                        .capture_targets
                        .iter()
                        .filter(|target| properties.get(target) == Some(&Some(player_id)))
                        .count();
                    format!(
                        "({},{}){:?}/{:?}:{:?} own{}/{} sq{}",
                        operation.anchor.x,
                        operation.anchor.y,
                        operation.objective,
                        operation.scope,
                        operation.state,
                        owned,
                        operation.capture_targets.len(),
                        operation.assigned_squads.len()
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            eprintln!("BEACHHEAD_NODES p{} T{turn} {text}", player_id.0);
        }
    }
    world.insert_resource(operations);
}

/// 構築済みDAGを現在の拠点所有へ投影し、到達済みの区間から次に止まる未確保拠点を返す。
///
/// 分岐では複数の候補が同時に現れ、合流後は一つのノードに戻る。施設の所属地域が
/// 前衛の通過を許可していれば、施設の占領完了を待たず次地域の目標を探索する。
pub(in crate::ai::v4) fn capital_route_dag_frontier_properties(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Option<Vec<Vec<GridPosition>>> {
    let topology = capital_route_topology_for(world, player_id, island_id)?;
    let mut property_owners = HashMap::default();
    for entity in world.iter_entities() {
        let (Some(position), Some(property)) =
            (entity.get::<GridPosition>(), entity.get::<Property>())
        else {
            continue;
        };
        property_owners.insert(*position, property.owner_id);
    }
    let mut reachable = vec![false; topology.nodes.len()];
    reachable[topology.start_node.0] = true;
    let mut route_properties = vec![Vec::new(); topology.routes.len()];
    debug_assert!(
        topology.nodes[topology.goal_node.0]
            .successor_nodes
            .is_empty()
    );
    // DAGはトポロジカル順に構築済みであり、successorのNode番号は必ず大きい。
    for (index, node) in topology.nodes.iter().enumerate() {
        if !reachable[index] {
            continue;
        }
        if property_owners.get(&node.anchor).copied() != Some(Some(player_id))
            && let Some(route) = topology.property_routes.get(&node.anchor)
        {
            let owner_anchor = super::commitment::route_target_node_anchor(
                world,
                player_id,
                island_id,
                node.anchor,
            );
            let region_released = world
                .get_resource::<CapitalRouteNodeOperationRegistry>()
                .and_then(|registry| {
                    registry.operations.get(&CapitalRouteNodeOperationId {
                        topology: super::model::CapitalRouteTopologyKey {
                            player_id,
                            island_id,
                        },
                        anchor: owner_anchor,
                    })
                })
                .is_some_and(capital_route_node_exit_satisfied);
            // 地域の局地制圧と施設の占領完了は別工程。制圧済み地域で前衛を止めない。
            if !region_released {
                route_properties[route.0].push(node.anchor);
                continue;
            }
        }
        for successor in &node.successor_nodes {
            reachable[successor.0] = true;
        }
    }
    Some(route_properties)
}

/// 同一陸塊の首都戦役において、各進軍軸に属する全未所有物件を返す。
///
/// 最前線の1物件だけを見るフロンティア判定とは異なり、手前の自陣物件から
/// 前線の前衛物件までを各routeへ残し、自首都に近い順に攻略できるようにする。
pub(in crate::ai::v4) fn capital_route_dag_all_properties(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Option<Vec<Vec<GridPosition>>> {
    let topology = capital_route_topology_for(world, player_id, island_id)?;
    let mut route_properties = vec![Vec::new(); topology.routes.len()];
    for entity in world.iter_entities() {
        let (Some(position), Some(property)) =
            (entity.get::<GridPosition>(), entity.get::<Property>())
        else {
            continue;
        };
        if property.owner_id == Some(player_id) {
            continue;
        }
        if let Some(route) = topology.property_routes.get(position)
            && route.0 < route_properties.len()
        {
            route_properties[route.0].push(*position);
        }
    }
    Some(route_properties)
}

/// 首都作戦を実行へ移してよいかを、DAG上の前方区間から判定する。
///
/// 単に「自軍所有のどれかの都市が中間地点を越えたか」では、側方の都市やDAG外の
/// 施設がGo条件に混ざる。各routeで自首都から連続して確保できた先の、最初の未確保
/// 拠点を使うことで、進軍・生産・Go判定を同じ首都戦役の状態から導く。
pub(in crate::ai::v4) fn capital_route_assault_authorized_from_dag(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Option<bool> {
    let topology = capital_route_topology_for(world, player_id, island_id)?;
    let goal = topology.nodes[topology.goal_node.0].anchor;
    let total_progress = topology.from_home.get(&goal).copied()?.max(1);
    let frontiers = capital_route_dag_frontier_properties(world, player_id, island_id)?;
    Some(frontiers.iter().flatten().any(|frontier| {
        topology
            .from_home
            .get(frontier)
            .is_some_and(|progress| progress.saturating_mul(2) >= total_progress)
    }))
}

/// 各進軍路の現在の前線を返す。DAGが無い島では `None`。
///
/// 進軍路の識別点（`CapitalRoute::front`）は地形から一度だけ決める固定点で、戦線が
/// 進んでも動かない。余った戦闘役をそこへ送ると、戦線が敵首都の周りへ移った後も
/// 中盤の識別点へ向かい続ける（map_6 では R31-40 に約47体が北の識別点へ向かっていた）。
/// 代わりに、自首都から確保済みの拠点を連続してたどった先の未確保拠点（DAGの前線拠点）
/// のうち、最も前進した拠点を使う。その進軍路に未確保拠点が残っていなければ敵首都とする。
pub(crate) fn capital_route_current_fronts(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Option<Vec<GridPosition>> {
    Some(
        capital_route_front_properties(world, player_id, island_id)?
            .into_iter()
            .map(|properties| properties[0])
            .collect(),
    )
}

/// 各進軍路の前線拠点を、最も前進した順に返す。前線拠点が無い進軍路は敵首都だけを持つ。
pub(crate) fn capital_route_front_properties(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Option<Vec<Vec<GridPosition>>> {
    let topology = capital_route_topology_for(world, player_id, island_id)?;
    let frontiers = capital_route_dag_frontier_properties(world, player_id, island_id)?;
    let goal = topology.nodes[topology.goal_node.0].anchor;
    Some(
        frontiers
            .into_iter()
            .map(|mut properties| {
                properties.sort_unstable_by_key(|position| {
                    std::cmp::Reverse(progress_order(&topology.from_home, *position))
                });
                if properties.is_empty() {
                    properties.push(goal);
                }
                properties
            })
            .collect(),
    )
}

/// 敵首都の攻略で、首都の敵に有効打を持たない部隊が待つ位置の、首都からの手前ステップ数。
///
/// map_6 では首都の袋地の入口の外に、入口の敵を射程に収める広場がある。待機をその広場より
/// 手前に置き、射撃位置を有効打を持つ部隊へ空ける（7ステップでは広場の中になった）。
const CAPITAL_ASSAULT_STAGING_STEPS: u32 = 11;

/// 終盤の敵首都攻略の地理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CapitalAssaultGeometry {
    /// 敵首都。
    pub(crate) capital: GridPosition,
    /// 首都の敵に有効打を持たない部隊が、射撃位置を空けて待つ回廊セル。
    pub(crate) staging: GridPosition,
}

/// 敵首都の地域とみなす、首都からの距離。
pub(crate) fn capital_assault_region_radius() -> u32 {
    CAPITAL_ROUTE_REGION_RADIUS.saturating_add(1)
}

/// 全進軍路の全前線拠点が敵首都の地域に入ったときだけ、首都攻略の地理を返す。
///
/// 最も前進した前線拠点だけを見ると、拠点を通らない経路がある進軍路では中盤から首都の
/// 地域に達してしまうため、前線拠点をすべて見る。
pub(crate) fn capital_assault_geometry(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Option<CapitalAssaultGeometry> {
    let topology = capital_route_topology_for(world, player_id, island_id)?;
    let map = world.get_resource::<Map>()?;
    let capital = topology.nodes[topology.goal_node.0].anchor;
    let front_properties = capital_route_front_properties(world, player_id, island_id)?;
    if !front_properties.iter().any(|properties| {
        properties.iter().all(|front| {
            map.distance(front.x, front.y, capital.x, capital.y) <= CAPITAL_ROUTE_REGION_RADIUS
        })
    }) {
        return None;
    }
    let staging_progress = topology
        .from_home
        .get(&capital)?
        .saturating_sub(CAPITAL_ASSAULT_STAGING_STEPS);
    let staging = topology
        .corridor
        .iter()
        .copied()
        .filter(|cell| topology.from_home.get(cell) == Some(&staging_progress))
        .min_by_key(|cell| {
            (
                map.distance(cell.x, cell.y, capital.x, capital.y),
                cell.y,
                cell.x,
            )
        })?;
    Some(CapitalAssaultGeometry { capital, staging })
}
