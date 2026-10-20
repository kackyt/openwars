//! Squad単位のDAG区間割当と診断。

use crate::ai::v4::capital_route::force_demand::{
    apportion_route_force_slots, same_land_route_force_demands,
};
use crate::ai::v4::capital_route::model::{
    CapitalRouteAssignmentDiagnostics, CapitalRouteCommitment, CapitalRouteExecutionPhase,
    CapitalRouteId, CapitalRouteNodeId, CapitalRouteNodeObjective, CapitalRouteNodeOperation,
    CapitalRouteNodeOperationId, CapitalRouteNodeOperationRegistry, CapitalRouteNodeScope,
    CapitalRoutePathRegistry, CapitalRouteTopologyKey, CapitalRouteTopologyRegistry,
};
use crate::ai::v4::capital_route::model::{CapitalRouteTopology, RouteForceDemand};
use crate::ai::v4::capital_route::node_operation::{
    capital_route_dag_frontier_properties, capital_route_node_exit_satisfied,
    refresh_capital_route_node_operations,
};
use crate::ai::v4::capital_route::topology::capital_route_topology_for;
use crate::ai::v4::{
    BTreeMap, Entity, Faction, GridPosition, HashMap, HashSet, Health, Map, MasterDataRegistry,
    MissionType, MovementType, PlayerId, Property, SquadId, Transporting, UnitStats, UnitType,
    World, assignment, deployment, operation, victory_roadmap,
};

/// Squadの局地任務とDAG区間の役割を合成する。
pub(in crate::ai::v4) fn capital_route_execution_phase(
    mission: &MissionType,
) -> CapitalRouteExecutionPhase {
    match mission {
        MissionType::Defense | MissionType::Reserve => CapitalRouteExecutionPhase::Hold,
        MissionType::Transport => CapitalRouteExecutionPhase::Supply,
        _ => CapitalRouteExecutionPhase::Advance,
    }
}

/// Combat Squadが現在のDAG Nodeを制圧する役かを返す。
///
/// 生産時には首都や将来の前線を指していたDefense Squadでも、未完了Nodeより前方を
/// 目指しているなら、そのNodeを飛び越える権限はない。Roadmapが現在NodeのControl
/// 役へ再指令し、Capture Squadの出発条件にも使う。本土側を守るDefenseだけはHoldに
/// 残すため、Nodeより後方だけを指定する場合はControlにしない。
pub(in crate::ai::v4) fn capital_route_controls_active_node(
    mission: &MissionType,
    target_is_on_or_beyond_active_node: bool,
) -> bool {
    matches!(mission, MissionType::Attack)
        || (matches!(mission, MissionType::Defense) && target_is_on_or_beyond_active_node)
}

/// Capture Squadを現在のDAG Nodeへ束縛するかを返す。
///
/// 現在Nodeの拠点対象、またはそこまでのDAGセルを明示目標にするCaptureだけを
/// Roadmapの前衛へ参加させる。DAG外の外周拠点を指すSquadまで、進行度が前方という
/// 理由だけで中央の最短線へ上書きしない。map_25のように最短線の上下に拠点列がある
/// マップでは、そのSquad orderを残すことで進路上の施設を継続して占領できる。
pub(in crate::ai::v4) fn capital_route_capture_joins_active_node(
    mission: &MissionType,
    has_explicit_target: bool,
    target_matches_active_route: bool,
    target_belongs_to_other_capture_node: bool,
) -> bool {
    !matches!(mission, MissionType::Capture)
        || !has_explicit_target
        || (target_matches_active_route && !target_belongs_to_other_capture_node)
}

/// Nodeへ同時に進入させる地上member数を、地理的な収容面積から求める。
///
/// これは「最低防衛数」のような固定戦力ではない。Pointは一点を一体ずつ通すしかなく、
/// Areaは実際に部隊が立てる局地セル数だけを前衛へ送る。余剰は後方の待機線へ残し、
/// 生産施設から同じ一点へ無制限に押し込まない。
pub(in crate::ai::v4) fn capital_route_node_frontline_capacity(
    operation: &CapitalRouteNodeOperation,
) -> usize {
    match operation.scope {
        CapitalRouteNodeScope::Point => 1,
        CapitalRouteNodeScope::Area => operation
            .control_area
            .len()
            .max(operation.capture_targets.len())
            .max(1),
    }
}

/// Capture Nodeの前衛枠を、占領役とControl役へ分ける。
///
/// Captureを先に一つの共通枠へ入れるだけでは、Pointの容量1を占領兵が使い切り、
/// 敵がいるときに必要なControl Squadが必ずStageへ落ちて出発gateが開かない。
/// Areaでは総容量を概ね維持しつつ、Pointでも占領役1体と隣接護衛1体を許可する。
pub(in crate::ai::v4) fn capital_route_role_frontline_capacity(
    operation: &CapitalRouteNodeOperation,
    capture_role: bool,
) -> usize {
    let total = capital_route_node_frontline_capacity(operation);
    if operation.objective != CapitalRouteNodeObjective::Capture {
        return total;
    }
    let capture_capacity = operation.capture_targets.len().max(1);
    if capture_role {
        capture_capacity
    } else {
        total.saturating_sub(capture_capacity).max(1)
    }
}

/// Capture Nodeの専用枠を使うSquadかを返す。
///
/// 汎用plannerのmission名だけで決めると、遠方で新設されたCapture Squadが唯一の枠を
/// 占有し、すでに前線へいる歩兵・Mechが占領できない。Capture missionは専用枠で待たせ、
/// Attack等の占領可能Squadは枠が空いているときだけ占領役へ転用する。
pub(in crate::ai::v4) fn capital_route_uses_capture_lane(
    operation: &CapitalRouteNodeOperation,
    mission: &MissionType,
    capture_member_count: usize,
    assigned_capture_members: usize,
) -> bool {
    if operation.objective != CapitalRouteNodeObjective::Capture {
        return false;
    }
    matches!(mission, MissionType::Capture)
        || (capture_member_count > 0
            && assigned_capture_members < capital_route_role_frontline_capacity(operation, true))
}

/// 前衛枠へ入れるSquadの順位を返す。
///
/// Squad IDや任務名を先に見ると、後方で新造された部隊がAdvanceを保持したまま、
/// すでに接敵線へ到達した予備へStageの後退指令を出してしまう。地上DAG上の実前進度を
/// 第一基準にし、同じ位置でだけCapture任務・占領能力・安定IDを使う。
pub(in crate::ai::v4) fn capital_route_frontline_sort_key(
    forward_progress: u32,
    mission: &MissionType,
    capture_member_count: usize,
    frontline_control_value: u64,
    squad_id: SquadId,
) -> (
    std::cmp::Reverse<u32>,
    bool,
    bool,
    std::cmp::Reverse<u64>,
    u32,
) {
    (
        std::cmp::Reverse(forward_progress),
        !matches!(mission, MissionType::Capture),
        capture_member_count == 0,
        std::cmp::Reverse(frontline_control_value),
        squad_id.0,
    )
}

/// 前衛枠が埋まったSquadの待機線を、同じDAG区間の既通過セルから決める。
///
/// 生産施設セルを候補から外し、起点から前線までの道に順番に置く。これにより後続も
/// Squadとして同じ作戦へ所属し続ける一方、先頭のCapture Nodeへ全員が殺到しない。
pub(in crate::ai::v4) fn capital_route_staging_target(
    world: &World,
    player_id: PlayerId,
    path: &[GridPosition],
    execution_target: GridPosition,
    stage_index: usize,
) -> GridPosition {
    let master_data = world.get_resource::<MasterDataRegistry>();
    let production_sites = world
        .iter_entities()
        .filter_map(|entity| {
            let position = *entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.owner_id == Some(player_id)
                && master_data.is_some_and(|registry| {
                    registry.is_production_facility(property.terrain.as_str())
                }))
            .then_some(position)
        })
        .collect::<HashSet<_>>();
    let end_index = path
        .iter()
        .rposition(|cell| *cell == execution_target)
        .unwrap_or(path.len());
    let staging_cells = path
        .iter()
        .take(end_index)
        .copied()
        .filter(|cell| !production_sites.contains(cell))
        .collect::<Vec<_>>();
    staging_cells
        .get(stage_index % staging_cells.len().max(1))
        .copied()
        .or_else(|| path.first().copied())
        .unwrap_or(execution_target)
}

/// 同じ論理作戦のDAG指令だけを参照する。memberの重なりから旧命令を推測しない。
pub(in crate::ai::v4) fn inherited_capital_route_commitment(
    registry: &CapitalRoutePathRegistry,
    operation_id: crate::ai::squad::SquadOperationId,
) -> Option<&CapitalRouteCommitment> {
    registry
        .squads_by_operation
        .get(&operation_id)
        .and_then(|id| registry.commitments.get(id))
}

/// 後方Nodeが再び争奪になっても、すでに先のNodeへ投入した前衛は呼び戻さない。
///
/// Roadmapの先頭未完了Nodeは新規・後続Squadの行き先として使う。一方、同じ経路・同じ
/// 区間目標に対してAdvance/Stage済みのSquadは、担当Nodeが未完了である限り前進を継続する。
/// これにより前線の小さな戦力変動で全軍が往復することを防ぐ。
pub(in crate::ai::v4) fn capital_route_active_node_for_squad(
    operation_path_nodes: &[CapitalRouteNodeId],
    roadmap_active_node: CapitalRouteNodeId,
    previous: Option<&CapitalRouteCommitment>,
    operations: &CapitalRouteNodeOperationRegistry,
    topology_key: CapitalRouteTopologyKey,
) -> CapitalRouteNodeId {
    let Some(previous) = previous.filter(|commitment| {
        matches!(
            commitment.phase,
            CapitalRouteExecutionPhase::Advance | CapitalRouteExecutionPhase::Stage
        )
    }) else {
        return roadmap_active_node;
    };
    let Some(roadmap_index) = operation_path_nodes
        .iter()
        .position(|node| *node == roadmap_active_node)
    else {
        return roadmap_active_node;
    };
    let Some(previous_index) = operation_path_nodes
        .iter()
        .position(|node| *node == previous.target_node)
    else {
        return roadmap_active_node;
    };
    if previous_index <= roadmap_index {
        return roadmap_active_node;
    }
    let operation_id = CapitalRouteNodeOperationId {
        topology: topology_key,
        node: previous.target_node,
    };
    if operations
        .operations
        .get(&operation_id)
        .is_some_and(|operation| !capital_route_node_exit_satisfied(operation))
    {
        previous.target_node
    } else {
        roadmap_active_node
    }
}

/// 主力が先へ進んだ後も、未確保拠点が残る優勢Areaへ護衛を一個残すNodeを返す。
///
/// `Dominant` は前衛全体を一拠点の奪回で呼び戻さないための解放条件であり、占領完了を
/// 意味しない。Capture役だけを残すと歩兵が各個撃破されるため、現在Nodeより後方にある
/// 最も近い未完了AreaへControl分隊を一個だけ残し、`Secured`になるまで占領を援護する。
pub(in crate::ai::v4) fn capital_route_dominant_capture_escort_node(
    operation_path_nodes: &[CapitalRouteNodeId],
    active_node: CapitalRouteNodeId,
    operations: &CapitalRouteNodeOperationRegistry,
    topology_key: CapitalRouteTopologyKey,
) -> Option<CapitalRouteNodeId> {
    let active_index = operation_path_nodes
        .iter()
        .position(|node| *node == active_node)?;
    operation_path_nodes[..active_index]
        .iter()
        .rev()
        .copied()
        .find(|node| {
            operations
                .operations
                .get(&CapitalRouteNodeOperationId {
                    topology: topology_key,
                    node: *node,
                })
                .is_some_and(|operation| {
                    operation.objective == CapitalRouteNodeObjective::Capture
                        && operation.scope == CapitalRouteNodeScope::Area
                        && operation.state == victory_roadmap::RoadmapNodeState::Dominant
                })
        })
}

/// Entityから具体Squadを一意に解決し、そのSquadにRoadmapが与えたDAG orderを返す。
/// ここでEntity自身の位置・兵種・HPから別のrouteを選ばないことが、DAGを第二の
/// Entity割当器にしないための境界である。
pub(in crate::ai::v4) fn capital_route_commitment_for_entity(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
) -> Option<&CapitalRouteCommitment> {
    let squad_id = world
        .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
        .and_then(|registry| registry.assignment(entity))
        .and_then(|assignment| assignment.squad_id)?;
    world
        .get_resource::<CapitalRoutePathRegistry>()
        .and_then(|registry| registry.commitments.get(&squad_id))
        .filter(|commitment| commitment.player_id == player_id)
}

/// 指定目標へ到達できるDAG内の一経路を選び、分岐を通す実在ノードを返す。
///
/// 始点直後の複数枝は当手番の戦力需要が高い方を選び、合流以降は同じ後続ノード列を使う。
/// ここで選んだウェイポイントは移動側へ渡し、経路探索が別枝へ逸れるのを防ぐ。
pub(in crate::ai::v4) fn capital_route_dag_nodes_for_target(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    route: CapitalRouteId,
    target: GridPosition,
) -> Option<Vec<CapitalRouteNodeId>> {
    let topology = capital_route_topology_for(world, player_id, island_id)?;
    let target_node = topology
        .nodes
        .iter()
        .position(|node| node.anchor == target)
        .map(CapitalRouteNodeId)?;
    let mut reaches_target = vec![false; topology.nodes.len()];
    reaches_target[target_node.0] = true;
    for index in (0..topology.nodes.len()).rev() {
        reaches_target[index] |= topology.nodes[index]
            .successor_nodes
            .iter()
            .any(|successor| reaches_target[successor.0]);
    }
    let map = world.get_resource::<Map>()?;
    let first = topology.nodes[topology.start_node.0]
        .successor_nodes
        .iter()
        .copied()
        .filter(|node| reaches_target[node.0])
        .min_by_key(|node| {
            let front = topology.routes[route.0].front;
            (
                map.distance(
                    topology.nodes[node.0].anchor.x,
                    topology.nodes[node.0].anchor.y,
                    front.x,
                    front.y,
                ),
                topology.nodes[node.0].anchor.y,
                topology.nodes[node.0].anchor.x,
            )
        })?;
    let mut path = vec![topology.start_node, first];
    let mut current = first;
    while current != target_node {
        let next = topology.nodes[current.0]
            .successor_nodes
            .iter()
            .copied()
            .filter(|node| reaches_target[node.0])
            .min_by_key(|node| {
                let front = topology.routes[route.0].front;
                (
                    map.distance(
                        topology.nodes[node.0].anchor.x,
                        topology.nodes[node.0].anchor.y,
                        front.x,
                        front.y,
                    ),
                    topology.nodes[node.0].anchor.y,
                    topology.nodes[node.0].anchor.x,
                )
            })?;
        path.push(next);
        current = next;
    }
    Some(path)
}

/// 現在のcampaign Squadへ、選択済みDAG枝の入口を束縛する。
///
/// 分岐の担当はSquad単位であり、memberを別枝へ個別に振り直さない。これにより
/// DAGは地理的なorderを提供するだけになり、Entityを直接割り当てる第二のplannerに
/// ならない。
pub(crate) fn refresh_capital_route_path_commitments(
    world: &mut World,
    player_id: PlayerId,
    manager: &mut crate::ai::squad::SquadManager,
) {
    // 旧順序を維持し、前回指令の観測後に候補を抽出、配分、反映する。
    refresh_capital_route_node_operations(world, player_id, manager);
    let existing = world
        .get_resource::<CapitalRoutePathRegistry>()
        .cloned()
        .unwrap_or_default();
    let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() else {
        return;
    };
    let topology_islands = world
        .get_resource::<CapitalRouteTopologyRegistry>()
        .map(|registry| {
            registry
                .topologies
                .keys()
                .filter_map(|key| (key.player_id == player_id).then_some(key.island_id))
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let owned_properties = world
        .iter_entities()
        .filter_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.owner_id == Some(player_id)).then_some(*position)
        })
        .collect::<HashSet<_>>();
    let context = AssignmentContext {
        world,
        player_id,
        existing: &existing,
        owned_properties: &owned_properties,
    };
    let candidates = collect_route_squads(&context, manager, island_map, &topology_islands);
    let mut draft = CommitmentDraft::default();
    for (island_id, squads) in candidates {
        assign_island_routes(&context, island_id, squads, &mut draft);
    }
    authorize_capture_departures(world, player_id, manager, &draft);
    commit_route_assignments(world, player_id, manager, draft);
    refresh_capital_route_node_operations(world, player_id, manager);
}

#[derive(Debug, Clone)]
struct RouteSquad {
    id: SquadId,
    operation_id: crate::ai::squad::SquadOperationId,
    mission: MissionType,
    target: Option<GridPosition>,
    forward_progress: u32,
    ground_member_count: usize,
    capture_member_count: usize,
    frontline_control_value: u64,
}

/// 今回の割当で共有する読み取り専用の盤面と前回指令。
struct AssignmentContext<'a> {
    world: &'a World,
    player_id: PlayerId,
    existing: &'a CapitalRoutePathRegistry,
    owned_properties: &'a HashSet<GridPosition>,
}

/// 島内の枝選択と護衛の指名結果。前衛枠の消費とは分ける。
struct RouteAllocation {
    targets: Vec<Option<GridPosition>>,
    quotas: Vec<usize>,
    assigned_routes: HashMap<SquadId, usize>,
    inherited_commitments: HashMap<SquadId, CapitalRouteCommitment>,
    preferred_capture_escort_by_route: Vec<Option<SquadId>>,
}

/// Squad順に消費する前衛枠・待機線と診断値。
struct IslandAssignment {
    committed_counts: Vec<usize>,
    path_unavailable_counts: Vec<usize>,
    frontline_members: HashMap<(CapitalRouteNodeOperationId, bool), usize>,
    staged_squads: HashMap<CapitalRouteNodeOperationId, usize>,
    retained_capture_escorts: HashSet<CapitalRouteNodeOperationId>,
}

/// 確定前の指令と、同じNodeに属するCapture/Controlの対応。
#[derive(Default)]
struct CommitmentDraft {
    commitments: HashMap<SquadId, CapitalRouteCommitment>,
    assignment_diagnostics: HashMap<CapitalRouteTopologyKey, CapitalRouteAssignmentDiagnostics>,
    capture_squads_by_operation: HashMap<CapitalRouteNodeOperationId, Vec<SquadId>>,
    control_squads_by_operation: HashMap<CapitalRouteNodeOperationId, HashSet<SquadId>>,
}

/// 全セルの移動経路と、作戦が存在するMilestoneの列を区別する。
struct SquadRoutePath {
    path_nodes: Vec<CapitalRouteNodeId>,
    path: Vec<GridPosition>,
    target_node: CapitalRouteNodeId,
    operation_path_nodes: Vec<CapitalRouteNodeId>,
}

/// 任務と現在Nodeを照合した前衛役割・前進先。
struct SquadRouteRole {
    is_capture_squad: bool,
    is_control_squad: bool,
    requested_phase: CapitalRouteExecutionPhase,
    forward_target: GridPosition,
}

/// 一個のSquadの地上member・占領力・前進度を観測する。島境界を越える候補は除外する。
fn route_squad(
    context: &AssignmentContext<'_>,
    squad: &crate::ai::squad::Squad,
    island_map: &crate::ai::islands::IslandMap,
    topology_islands: &HashSet<crate::ai::islands::IslandId>,
) -> Option<(crate::ai::islands::IslandId, RouteSquad)> {
    let world = context.world;
    let player_id = context.player_id;
    let mut island_id = None;
    let mut ground_member_count = 0_usize;
    let mut capture_member_count = 0_usize;
    let mut frontline_control_value = 0_u64;
    let mut crosses_island_boundary = false;
    for entity in &squad.members {
        let Some(member) = ground_route_member(context, *entity, island_map, topology_islands)
        else {
            continue;
        };
        if island_id.is_some_and(|current| current != member.island_id) {
            crosses_island_boundary = true;
            break;
        }
        island_id = Some(member.island_id);
        ground_member_count = ground_member_count.saturating_add(1);
        if member.can_capture {
            capture_member_count = capture_member_count.saturating_add(1);
        }
        frontline_control_value = frontline_control_value.saturating_add(member.control_value);
    }
    if !crosses_island_boundary
        && let Some(island_id) = island_id
        && ground_member_count > 0
    {
        let forward_progress = squad
            .members
            .iter()
            .filter_map(|entity| world.get::<GridPosition>(*entity))
            .filter_map(|position| {
                capital_route_topology_for(world, player_id, island_id)?
                    .from_home
                    .get(position)
                    .copied()
            })
            .max()
            .unwrap_or(0);
        return Some((
            island_id,
            RouteSquad {
                id: squad.id,
                operation_id: squad.operation_id,
                mission: squad.plan.mission_type.clone(),
                target: squad.plan.target,
                forward_progress,
                ground_member_count,
                capture_member_count,
                frontline_control_value,
            },
        ));
    }
    None
}

/// 生産時の明示Deploymentを除き、候補Squadを元の一覧順のまま島へまとめる。
fn collect_route_squads(
    context: &AssignmentContext<'_>,
    manager: &crate::ai::squad::SquadManager,
    island_map: &crate::ai::islands::IslandMap,
    topology_islands: &HashSet<crate::ai::islands::IslandId>,
) -> HashMap<crate::ai::islands::IslandId, Vec<RouteSquad>> {
    let world = context.world;
    let player_id = context.player_id;
    // 発注から実Entityへ引き継いだ任務はDeploymentが正本である。汎用DAGが
    // 同じ部隊へ二つ目の移動目標を与えると、購入時に検証した初撃ETAが成立しない。
    let explicit_deployments = world
        .get_resource::<deployment::V4DeploymentRegistry>()
        .map(|registry| registry.active_entities(player_id))
        .unwrap_or_default();
    let mut squads_by_island = HashMap::<crate::ai::islands::IslandId, Vec<RouteSquad>>::default();
    for squad in manager.squads.iter().filter(|squad| {
        squad.plan.owner_id == Some(player_id)
            && squad.plan.mission_type != MissionType::Transport
            && !squad
                .members
                .iter()
                .any(|entity| explicit_deployments.contains(entity))
    }) {
        if let Some((island, candidate)) = route_squad(context, squad, island_map, topology_islands)
        {
            squads_by_island.entry(island).or_default().push(candidate);
        }
    }
    squads_by_island
}

/// 枝の先頭未確保拠点を需要へ投影し、元の規則で兵力枠を配る。
fn route_quotas(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    topology: &CapitalRouteTopology,
    ground_member_count: usize,
) -> (Vec<Option<GridPosition>>, Vec<RouteForceDemand>, Vec<usize>) {
    // 各枝の先頭未確保拠点を区間目標にする。橋を通っただけでは進行させず、
    // 拠点の確保により次の区間へ進む。
    let frontiers = capital_route_dag_frontier_properties(world, player_id, island_id)
        .unwrap_or_else(|| vec![Vec::new(); topology.routes.len()]);
    let targets = frontiers
        .iter()
        .map(|properties| {
            properties
                .first()
                .copied()
                .map(|property| route_target_node_anchor(world, player_id, island_id, property))
        })
        .collect::<Vec<_>>();
    let mut demands = same_land_route_force_demands(world, player_id, island_id);
    for (route, target) in targets.iter().enumerate() {
        if target.is_none() {
            demands[route].demand_value = 0;
        }
    }
    let mut quotas = apportion_route_force_slots(ground_member_count, &demands);
    // 需要観測がゼロでも、首都戦役に入ったSquadをDAG外へ落としてはならない。
    // この場合だけ「最も需要が大きい（同値ならroute番号が小さい）有効枝」へ残りを
    // 一括で置く。各枝への固定最低戦力ではなく、Squad orderの全量性を守る退避規則。
    let allocated = quotas.iter().sum::<usize>();
    if allocated < ground_member_count
        && let Some(route) = (0..topology.routes.len())
            .filter(|route| targets[*route].is_some())
            .max_by_key(|route| (demands[*route].demand_value, std::cmp::Reverse(*route)))
    {
        quotas[route] = quotas[route].saturating_add(ground_member_count - allocated);
    }

    (targets, demands, quotas)
}

/// 区間目標の拠点を、それを占領対象に含むNode作戦のanchorへ置き換える。
///
/// Node作戦は近接する拠点を一つのAreaにまとめて登録するため、拠点自体のセルには
/// 作戦がないことがある。拠点セルをそのまま目標にすると経路の終点に作戦がなく、
/// その枝のSquadが全員「経路なし」になってDAGから外れる。
pub(in crate::ai::v4) fn route_target_node_anchor(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    property: GridPosition,
) -> GridPosition {
    let Some(operations) = world.get_resource::<CapitalRouteNodeOperationRegistry>() else {
        return property;
    };
    let topology_key = CapitalRouteTopologyKey {
        player_id,
        island_id,
    };
    let mut candidates = operations
        .operations
        .values()
        .filter(|operation| operation.id.topology == topology_key)
        .collect::<Vec<_>>();
    // 拠点セル自身に作戦があればそれを使い、なければ占領対象に含む作戦のうち
    // 自首都側（Node番号が小さい）のものを選ぶ。
    if candidates
        .iter()
        .any(|operation| operation.anchor == property)
    {
        return property;
    }
    candidates.sort_unstable_by_key(|operation| operation.id.node.0);
    candidates
        .into_iter()
        .find(|operation| operation.capture_targets.contains(&property))
        .map_or(property, |operation| operation.anchor)
}

/// 同じ区間目標へ向かう前回の枝を先に保持し、Squad単位で既配分を数える。
fn retain_previous_routes(
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    topology: &CapitalRouteTopology,
    map: &Map,
    squads: &[RouteSquad],
    allocation: &mut RouteAllocation,
    assigned_counts: &mut [usize],
) {
    let RouteAllocation {
        targets,
        inherited_commitments,
        assigned_routes,
        ..
    } = allocation;
    // 前手番のSquad orderをまず保持する。Squadのmember数が変化しても、同じ
    // Nodeを実行する限り分岐を変えないため、回復・補充で戦略目標が揺れない。
    // ただし作戦がSquadの目標を別の枝へ移した場合は、作戦の枝を優先する。
    for squad in squads {
        let Some(commitment) = inherited_commitments.get(&squad.id) else {
            continue;
        };
        let route = commitment.route.0;
        if commitment.player_id != player_id
            || commitment.island_id != island_id
            || targets.get(route).copied().flatten() != Some(commitment.target)
            || squad_target_route(topology, map, &squad.mission, squad.target, targets)
                .is_some_and(|target_route| target_route != route)
        {
            continue;
        }
        assigned_counts[route] = assigned_counts[route].saturating_add(squad.ground_member_count);
        assigned_routes.insert(squad.id, route);
    }
}

/// 作戦がSquadに与えた目標が属するDAGの枝を返す。
///
/// 作戦は進軍路の前線・拠点・敵を目標にSquadを作る。DAGがその目標と無関係に枝を
/// 決めると、南の前線を守るSquadが北のNodeへ回され、南のCapture Nodeには制圧役が
/// いないまま占領役の出発許可が止まる。目標が前線そのものならその枝、枝の拠点なら
/// その枝、それ以外は最寄りの前線の枝とする。占領役は従来どおり枝の拠点だけで決める。
pub(in crate::ai::v4) fn squad_target_route(
    topology: &CapitalRouteTopology,
    map: &Map,
    mission: &MissionType,
    target: Option<GridPosition>,
    targets: &[Option<GridPosition>],
) -> Option<usize> {
    let target = target?;
    let route = topology
        .routes
        .iter()
        .position(|route| route.front == target)
        .or_else(|| topology.property_routes.get(&target).map(|route| route.0))
        .or_else(|| {
            (*mission != MissionType::Capture)
                .then(|| {
                    topology
                        .routes
                        .iter()
                        .enumerate()
                        .min_by_key(|(index, route)| {
                            (
                                map.distance(target.x, target.y, route.front.x, route.front.y),
                                *index,
                            )
                        })
                        .map(|(index, _)| index)
                })
                .flatten()
        })?;
    targets
        .get(route)
        .is_some_and(|target| target.is_some())
        .then_some(route)
}

/// 未配分Squadを占領目標または不足枠の順で枝へ置く。同点時の枝番号を保つ。
fn assign_remaining_routes(
    topology: &CapitalRouteTopology,
    map: &Map,
    squads: &[RouteSquad],
    demands: &[RouteForceDemand],
    allocation: &mut RouteAllocation,
    assigned_counts: &mut [usize],
) {
    let RouteAllocation {
        targets,
        quotas,
        assigned_routes,
        ..
    } = allocation;
    // 個体ごとの限界価値を再計算せず、Squad全体を一つのrouteへ置く。これにより
    // 同じSquadのCombat役とCapture役が別枝へ分離しない。
    for squad in squads {
        if assigned_routes.contains_key(&squad.id) {
            continue;
        }
        let preferred_target_route =
            squad_target_route(topology, map, &squad.mission, squad.target, targets);
        let route = preferred_target_route.or_else(|| {
            (0..topology.routes.len())
                .filter(|route| targets[*route].is_some())
                .max_by_key(|route| {
                    (
                        quotas[*route].saturating_sub(assigned_counts[*route]),
                        demands[*route].demand_value,
                        std::cmp::Reverse(*route),
                    )
                })
        });
        let Some(route) = route else {
            continue;
        };
        assigned_counts[route] = assigned_counts[route].saturating_add(squad.ground_member_count);
        assigned_routes.insert(squad.id, route);
    }
}

/// 各枝でCapture Areaへ残すControl護衛を、現在価値・前進度・安定IDで指名する。
fn preferred_capture_escorts(
    topology: &CapitalRouteTopology,
    squads: &[RouteSquad],
    assigned_routes: &HashMap<SquadId, usize>,
) -> Vec<Option<SquadId>> {
    // 各進軍枝で、未完了Capture Areaへ残す最良のControl護衛を先に指名する。
    // 単にloopで最初の候補を取ると、少し先行したReconが戦車より先に護衛枠を
    // 消費する。残存HP・価格・直接交戦能力を主基準、前進度を副基準にする。
    (0..topology.routes.len())
        .map(|route| {
            squads
                .iter()
                .filter(|squad| {
                    assigned_routes.get(&squad.id) == Some(&route)
                        && squad.capture_member_count == 0
                        && squad.mission == MissionType::Attack
                })
                .max_by_key(|squad| {
                    (
                        squad.frontline_control_value,
                        squad.forward_progress,
                        std::cmp::Reverse(squad.id.0),
                    )
                })
                .map(|squad| squad.id)
        })
        .collect::<Vec<_>>()
}

/// 候補の前進順を確定し、枝配分からNode前衛枠まで島単位で段取りする。
fn assign_island_routes(
    context: &AssignmentContext<'_>,
    island_id: crate::ai::islands::IslandId,
    mut squads: Vec<RouteSquad>,
    draft: &mut CommitmentDraft,
) {
    let world = context.world;
    let player_id = context.player_id;
    let Some(topology) = capital_route_topology_for(world, player_id, island_id) else {
        return;
    };
    squads.sort_unstable_by_key(|squad| {
        capital_route_frontline_sort_key(
            squad.forward_progress,
            &squad.mission,
            squad.capture_member_count,
            squad.frontline_control_value,
            squad.id,
        )
    });
    let ground_member_count = squads
        .iter()
        .map(|squad| squad.ground_member_count)
        .sum::<usize>();
    let (targets, demands, quotas) =
        route_quotas(world, player_id, island_id, topology, ground_member_count);
    let mut allocation = RouteAllocation {
        targets,
        quotas,
        assigned_routes: HashMap::default(),
        inherited_commitments: squads
            .iter()
            .filter_map(|squad| {
                inherited_capital_route_commitment(context.existing, squad.operation_id)
                    .cloned()
                    .map(|commitment| (squad.id, commitment))
            })
            .collect(),
        preferred_capture_escort_by_route: Vec::new(),
    };
    let mut assigned_counts = vec![0_usize; topology.routes.len()];
    let Some(map) = world.get_resource::<Map>() else {
        return;
    };
    retain_previous_routes(
        player_id,
        island_id,
        topology,
        map,
        &squads,
        &mut allocation,
        &mut assigned_counts,
    );
    assign_remaining_routes(
        topology,
        map,
        &squads,
        &demands,
        &mut allocation,
        &mut assigned_counts,
    );
    allocation.preferred_capture_escort_by_route =
        preferred_capture_escorts(topology, &squads, &allocation.assigned_routes);
    let mut state = IslandAssignment {
        committed_counts: vec![0_usize; topology.routes.len()],
        path_unavailable_counts: vec![0_usize; topology.routes.len()],
        frontline_members: HashMap::default(),
        staged_squads: HashMap::default(),
        retained_capture_escorts: HashSet::default(),
    };
    for squad in &squads {
        assign_route_squad(
            context,
            island_id,
            topology,
            &allocation,
            squad,
            &mut state,
            draft,
        );
    }
    draft.assignment_diagnostics.insert(
        CapitalRouteTopologyKey {
            player_id,
            island_id,
        },
        CapitalRouteAssignmentDiagnostics {
            eligible_ground_units: ground_member_count,
            route_quotas: allocation.quotas,
            committed_by_route: state.committed_counts,
            path_unavailable_by_route: state.path_unavailable_counts,
        },
    );
}

/// 移動用セル列と作戦Milestone列を取り出す。経路不能は呼出し元で一度だけ数える。
fn squad_route_path(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    topology: &CapitalRouteTopology,
    route: usize,
    target: GridPosition,
) -> Option<SquadRoutePath> {
    let path_nodes = capital_route_dag_nodes_for_target(
        world,
        player_id,
        island_id,
        CapitalRouteId(route),
        target,
    )?;
    let path = path_nodes
        .iter()
        .map(|node| topology.nodes[node.0].anchor)
        .collect::<Vec<_>>();
    let target_node = *path_nodes.last().expect("DAGの目標Nodeへの経路は空でない");
    // path_nodesは移動用の全セル列であり、Operationが存在するのは走査で選んだ
    // Milestoneだけである。未選択セルを未達Nodeと見なして一歩ずつ止めない。
    let operation_path_nodes = world
        .get_resource::<CapitalRouteNodeOperationRegistry>()
        .map(|registry| {
            path_nodes
                .iter()
                .copied()
                .filter(|node| {
                    registry
                        .operations
                        .contains_key(&CapitalRouteNodeOperationId {
                            topology: CapitalRouteTopologyKey {
                                player_id,
                                island_id,
                            },
                            node: *node,
                        })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if operation_path_nodes.is_empty() {
        return None;
    }

    Some(SquadRoutePath {
        path_nodes,
        path,
        target_node,
        operation_path_nodes,
    })
}

/// 前回の未完了Nodeを保持し、必要なCapture Areaには指名済み護衛だけを残す。
fn active_squad_node(
    context: &AssignmentContext<'_>,
    island_id: crate::ai::islands::IslandId,
    route: usize,
    squad: &RouteSquad,
    path: &SquadRoutePath,
    allocation: &RouteAllocation,
    retained_capture_escorts: &mut HashSet<CapitalRouteNodeOperationId>,
) -> CapitalRouteNodeId {
    let world = context.world;
    let player_id = context.player_id;
    let operation_path_nodes = &path.operation_path_nodes;
    let target_node = path.target_node;
    let inherited_commitments = &allocation.inherited_commitments;
    let preferred_capture_escort_by_route = &allocation.preferred_capture_escort_by_route;
    // Nodeの状態名ではなく出口条件を見る。Capture Nodeは`Dominant`でも未完了で、
    // 周辺拠点をSecuredにするまで同じNodeのSquad指令を継続する。
    let topology_key = CapitalRouteTopologyKey {
        player_id,
        island_id,
    };
    let mut active_node = world
        .get_resource::<CapitalRouteNodeOperationRegistry>()
        .map(|registry| {
            let roadmap_active_node = operation_path_nodes
                .iter()
                .copied()
                .find(|node| {
                    registry
                        .operations
                        .get(&CapitalRouteNodeOperationId {
                            topology: topology_key,
                            node: *node,
                        })
                        .is_none_or(|operation| !capital_route_node_exit_satisfied(operation))
                })
                .unwrap_or(target_node);
            capital_route_active_node_for_squad(
                operation_path_nodes,
                roadmap_active_node,
                inherited_commitments.get(&squad.id),
                registry,
                topology_key,
            )
        })
        .unwrap_or(target_node);
    if squad.capture_member_count == 0
        && squad.mission == MissionType::Attack
        && preferred_capture_escort_by_route
            .get(route)
            .copied()
            .flatten()
            == Some(squad.id)
        && let Some(escort_node) = world
            .get_resource::<CapitalRouteNodeOperationRegistry>()
            .and_then(|registry| {
                capital_route_dominant_capture_escort_node(
                    operation_path_nodes,
                    active_node,
                    registry,
                    topology_key,
                )
            })
    {
        let escort_operation_id = CapitalRouteNodeOperationId {
            topology: topology_key,
            node: escort_node,
        };
        if retained_capture_escorts.insert(escort_operation_id) {
            active_node = escort_node;
        }
    }

    active_node
}

/// 盤面投影済みNodeを読む。作戦未登録時は経路未準備として返す。
fn assigned_node_operation(
    world: &World,
    topology_key: CapitalRouteTopologyKey,
    active_node: CapitalRouteNodeId,
) -> Option<CapitalRouteNodeOperation> {
    let player_id = topology_key.player_id;
    let island_id = topology_key.island_id;
    world
        .get_resource::<CapitalRouteNodeOperationRegistry>()
        .and_then(|registry| {
            registry
                .operations
                .get(&CapitalRouteNodeOperationId {
                    topology: CapitalRouteTopologyKey {
                        player_id,
                        island_id,
                    },
                    node: active_node,
                })
                .cloned()
        })
}

/// 局地Nodeへ参加するCapture/Controlを判定し、外周・別枝の占領任務を保つ。
fn squad_route_role(
    context: &AssignmentContext<'_>,
    topology_key: CapitalRouteTopologyKey,
    topology: &CapitalRouteTopology,
    squad: &RouteSquad,
    node_operation: &CapitalRouteNodeOperation,
    path: &[GridPosition],
    route_target: (usize, GridPosition),
) -> Option<SquadRouteRole> {
    let world = context.world;
    let player_id = context.player_id;
    let island_id = topology_key.island_id;
    let is_capture_squad = squad.mission == MissionType::Capture;
    let active_node_progress = topology
        .from_home
        .get(&node_operation.anchor)
        .copied()
        .unwrap_or(0);
    // 既存targetが別枝の首都側を指していても、現在Nodeより前方ならControl
    // Squadである。現在Nodeを飛ばして遠方targetへ行かせず、まずAreaを制圧する。
    let target_is_on_or_beyond_active_node = squad.target.is_some_and(|target| {
        path.contains(&target)
            || topology
                .from_home
                .get(&target)
                .is_some_and(|progress| *progress >= active_node_progress)
    });
    // Captureの明示目標は、単に自首都から遠いだけでは現在Nodeへ
    // 取り込まない。現在Nodeの拠点、または選択済みDAG経路上の目標だけが
    // Roadmapに属し、外周の拠点任務はSquad独自の進路を保つ。
    let capture_target_matches_active_route = squad.target.is_some_and(|target| {
        path.contains(&target) || node_operation.capture_targets.contains(&target)
    });
    let target_belongs_to_other_capture_node = squad.target.is_some_and(|target| {
        world
            .get_resource::<CapitalRouteNodeOperationRegistry>()
            .is_some_and(|registry| {
                registry.operations.values().any(|operation| {
                    operation.id.topology.player_id == player_id
                        && operation.id.topology.island_id == island_id
                        && operation.id != node_operation.id
                        && operation.objective == CapitalRouteNodeObjective::Capture
                        && !capital_route_node_exit_satisfied(operation)
                        && operation.capture_targets.contains(&target)
                })
            })
    });
    if !capital_route_capture_joins_active_node(
        &squad.mission,
        squad.target.is_some(),
        capture_target_matches_active_route,
        target_belongs_to_other_capture_node,
    ) {
        // 後方・別枝の地域占領はSquad orderを正本とし、DAG commitmentを
        // 作らない。完了後に前方目標へ変われば、現在Nodeへ再参加する。
        return None;
    }
    let is_control_squad =
        capital_route_controls_active_node(&squad.mission, target_is_on_or_beyond_active_node);
    let requested_phase = if is_capture_squad || is_control_squad {
        CapitalRouteExecutionPhase::Advance
    } else {
        capital_route_execution_phase(&squad.mission)
    };

    let forward_target = squad_forward_target(
        context,
        topology,
        squad,
        node_operation,
        path,
        route_target,
        requested_phase,
    );
    Some(SquadRouteRole {
        is_capture_squad,
        is_control_squad,
        requested_phase,
        forward_target,
    })
}

/// Advanceは橋出口、Holdは区間の既確保拠点、Supplyは指定終点を用いる。
fn squad_forward_target(
    context: &AssignmentContext<'_>,
    topology: &CapitalRouteTopology,
    squad: &RouteSquad,
    node_operation: &CapitalRouteNodeOperation,
    path: &[GridPosition],
    route_target: (usize, GridPosition),
    requested_phase: CapitalRouteExecutionPhase,
) -> GridPosition {
    let (route, target) = route_target;
    let owned_properties = context.owned_properties;
    let directive_target = squad.target.filter(|target| path.contains(target));
    match requested_phase {
        // 橋Nodeはanchor（橋の上）ではなく、向こう岸の出口へ到達するまでを
        // 前進区間にする。これにより橋手前への接近で指令が終わらない。
        CapitalRouteExecutionPhase::Advance => node_operation
            .crossing_exit
            .filter(|_| !node_operation.crossed)
            .unwrap_or(node_operation.anchor),
        // 将来的に地上輸送SquadがDAG orderを持つ場合も、配送先が区間外なら
        // 前線拠点を使う。現行ではTransport Squadをorder対象から除外している。
        CapitalRouteExecutionPhase::Supply => directive_target.unwrap_or(target),
        // 防衛は任意の後方待機ではなく、指定された同一区間の拠点、または
        // Gate/最後に確保した拠点までを守備範囲にする。
        CapitalRouteExecutionPhase::Hold => {
            if let Some(target) = directive_target {
                target
            } else {
                let gate_index = path
                    .iter()
                    .position(|cell| *cell == topology.routes[route].front)
                    .unwrap_or(0);
                let hold_index = path
                    .iter()
                    .enumerate()
                    .filter(|(_, cell)| owned_properties.contains(cell))
                    .map(|(index, _)| index)
                    .max()
                    .unwrap_or(gate_index)
                    .max(gate_index);
                path[hold_index]
            }
        }
        // Stageは前衛枠の判定後に、同じ経路内の待機セルへ置き換える。
        CapitalRouteExecutionPhase::Stage => {
            unreachable!("StageはSquad任務から直接は作られない")
        }
    }
}

/// Capture/Control別の前衛枠を消費し、余剰Squadを同じ経路の待機線へ割り当てる。
fn allocate_frontline_phase(
    context: &AssignmentContext<'_>,
    squad: &RouteSquad,
    node_operation: &CapitalRouteNodeOperation,
    path: &[GridPosition],
    role: &SquadRouteRole,
    state: &mut IslandAssignment,
) -> (CapitalRouteExecutionPhase, GridPosition, bool) {
    let world = context.world;
    let player_id = context.player_id;
    let operation_id = node_operation.id;
    let frontline_members = &mut state.frontline_members;
    let staged_squads = &mut state.staged_squads;
    let requested_phase = role.requested_phase;
    let forward_target = role.forward_target;
    let assigned_capture_members = frontline_members
        .get(&(operation_id, true))
        .copied()
        .unwrap_or(0);
    let uses_capture_lane = capital_route_uses_capture_lane(
        node_operation,
        &squad.mission,
        squad.capture_member_count,
        assigned_capture_members,
    );
    let role_capacity = capital_route_role_frontline_capacity(node_operation, uses_capture_lane);
    let (phase, execution_target) = if requested_phase != CapitalRouteExecutionPhase::Advance {
        (requested_phase, forward_target)
    } else {
        let assigned = frontline_members
            .entry((operation_id, uses_capture_lane))
            .or_default();
        if *assigned < role_capacity {
            *assigned = assigned.saturating_add(squad.ground_member_count);
            (CapitalRouteExecutionPhase::Advance, forward_target)
        } else {
            let stage_index = staged_squads.entry(operation_id).or_default();
            let target =
                capital_route_staging_target(world, player_id, path, forward_target, *stage_index);
            *stage_index = stage_index.saturating_add(1);
            (CapitalRouteExecutionPhase::Stage, target)
        }
    };

    (phase, execution_target, uses_capture_lane)
}

/// 同じNodeへ実際にAdvanceする占領役・護衛だけを出発許可の根拠へ記録する。
fn record_frontline_role(
    squad: &RouteSquad,
    node_operation: &CapitalRouteNodeOperation,
    role: &SquadRouteRole,
    phase: CapitalRouteExecutionPhase,
    uses_capture_lane: bool,
    draft: &mut CommitmentDraft,
) {
    let operation_id = node_operation.id;
    let is_capture_squad = role.is_capture_squad;
    let is_control_squad = role.is_control_squad;
    let capture_squads_by_operation = &mut draft.capture_squads_by_operation;
    let control_squads_by_operation = &mut draft.control_squads_by_operation;
    if is_capture_squad
        && node_operation.objective == CapitalRouteNodeObjective::Capture
        && phase == CapitalRouteExecutionPhase::Advance
    {
        capture_squads_by_operation
            .entry(operation_id)
            .or_default()
            .push(squad.id);
    }
    if is_control_squad && !uses_capture_lane && phase == CapitalRouteExecutionPhase::Advance {
        control_squads_by_operation
            .entry(operation_id)
            .or_default()
            .insert(squad.id);
    }
}

/// 経路・Node・役割・前衛枠の結果を一つのSquad指令として組み立てる。
fn assign_route_squad(
    context: &AssignmentContext<'_>,
    island_id: crate::ai::islands::IslandId,
    topology: &CapitalRouteTopology,
    allocation: &RouteAllocation,
    squad: &RouteSquad,
    state: &mut IslandAssignment,
    draft: &mut CommitmentDraft,
) {
    let world = context.world;
    let player_id = context.player_id;
    let Some(route) = allocation.assigned_routes.get(&squad.id).copied() else {
        return;
    };
    let Some(target) = allocation.targets[route] else {
        return;
    };
    let Some(route_path) = squad_route_path(world, player_id, island_id, topology, route, target)
    else {
        state.path_unavailable_counts[route] =
            state.path_unavailable_counts[route].saturating_add(1);
        return;
    };
    let active_node = active_squad_node(
        context,
        island_id,
        route,
        squad,
        &route_path,
        allocation,
        &mut state.retained_capture_escorts,
    );
    let topology_key = CapitalRouteTopologyKey {
        player_id,
        island_id,
    };
    let Some(node_operation) = assigned_node_operation(world, topology_key, active_node) else {
        state.path_unavailable_counts[route] =
            state.path_unavailable_counts[route].saturating_add(1);
        return;
    };
    let Some(role) = squad_route_role(
        context,
        topology_key,
        topology,
        squad,
        &node_operation,
        &route_path.path,
        (route, target),
    ) else {
        return;
    };
    let frontline_capacity = capital_route_node_frontline_capacity(&node_operation);
    let (phase, execution_target, uses_capture_lane) = allocate_frontline_phase(
        context,
        squad,
        &node_operation,
        &route_path.path,
        &role,
        state,
    );
    record_frontline_role(
        squad,
        &node_operation,
        &role,
        phase,
        uses_capture_lane,
        draft,
    );
    let SquadRoutePath {
        path_nodes, path, ..
    } = route_path;
    let committed_counts = &mut state.committed_counts;
    let commitments = &mut draft.commitments;
    committed_counts[route] = committed_counts[route].saturating_add(squad.ground_member_count);
    commitments.insert(
        squad.id,
        CapitalRouteCommitment {
            player_id,
            island_id,
            route: CapitalRouteId(route),
            target,
            target_node: active_node,
            objective: node_operation.objective,
            scope: node_operation.scope,
            control_area: node_operation.control_area,
            capture_targets: node_operation.capture_targets,
            crossing_exit: node_operation.crossing_exit,
            crossed: node_operation.crossed,
            execution_target,
            frontline_capacity,
            path_nodes,
            path,
            phase,
        },
    );
}

/// 地域の敵と同一Nodeの護衛からCapture Squadの出発許可を更新する。
fn authorize_capture_squad(
    player_id: PlayerId,
    squad_id: SquadId,
    manager: &mut crate::ai::squad::SquadManager,
    commitments: &HashMap<SquadId, CapitalRouteCommitment>,
    enemy_positions: &[GridPosition],
    has_control_squad: bool,
) {
    let Some(commitment) = commitments.get(&squad_id) else {
        return;
    };
    let local_enemy_present = enemy_positions
        .iter()
        .any(|position| commitment.control_area.contains(position));
    // 敵がいないAreaはControl Squadの到着待ちで占領を止めない。敵がいるAreaは
    // 同じNodeへControl Squadを割り当てた場合だけCapture Squadを発進させる。
    let departure_authorized = !local_enemy_present || has_control_squad;
    if let Some(mut squad) = manager
        .edit_each()
        .find(|squad| squad.id == squad_id && squad.plan.owner_id == Some(player_id))
    {
        squad.plan_mut().departure_authorized = departure_authorized;
        if departure_authorized && squad.plan.phase == crate::ai::squad::MissionPhase::Forming {
            squad.plan_mut().phase = crate::ai::squad::MissionPhase::MovingToTarget;
        }
    }
}

/// 前衛割当後の全Nodeについて、占領役の出発許可を確定する。
fn authorize_capture_departures(
    world: &World,
    player_id: PlayerId,
    manager: &mut crate::ai::squad::SquadManager,
    draft: &CommitmentDraft,
) {
    let enemy_positions = world
        .iter_entities()
        .filter_map(|entity| {
            let faction = entity.get::<Faction>()?;
            let position = entity.get::<GridPosition>()?;
            (faction.0 != player_id).then_some(*position)
        })
        .collect::<Vec<_>>();

    for (operation_id, capture_squads) in &draft.capture_squads_by_operation {
        let has_control_squad = draft
            .control_squads_by_operation
            .get(operation_id)
            .is_some_and(|squads| !squads.is_empty());
        authorize_capture_group(
            player_id,
            capture_squads,
            manager,
            &draft.commitments,
            &enemy_positions,
            has_control_squad,
        );
    }
}

/// 同じNodeのCapture一覧を元のSquad順のまま更新する。
fn authorize_capture_group(
    player_id: PlayerId,
    capture_squads: &[SquadId],
    manager: &mut crate::ai::squad::SquadManager,
    commitments: &HashMap<SquadId, CapitalRouteCommitment>,
    enemy_positions: &[GridPosition],
    has_control_squad: bool,
) {
    for squad_id in capture_squads {
        authorize_capture_squad(
            player_id,
            *squad_id,
            manager,
            commitments,
            enemy_positions,
            has_control_squad,
        );
    }
}

/// 他playerの指令を残し、今回のSquad指令・作戦ID索引・診断を同時に反映する。
fn commit_route_assignments(
    world: &mut World,
    player_id: PlayerId,
    manager: &crate::ai::squad::SquadManager,
    draft: CommitmentDraft,
) {
    let CommitmentDraft {
        commitments,
        assignment_diagnostics,
        ..
    } = draft;
    let mut registry = world
        .remove_resource::<CapitalRoutePathRegistry>()
        .unwrap_or_default();
    registry
        .commitments
        .retain(|_, commitment| commitment.player_id != player_id);
    registry.commitments.extend(commitments);
    let commitments = &registry.commitments;
    registry.squads_by_operation.retain(|_, id| {
        commitments
            .get(id)
            .is_some_and(|order| order.player_id != player_id)
    });
    for squad in manager
        .squads
        .iter()
        .filter(|s| s.plan.owner_id == Some(player_id))
    {
        if registry.commitments.contains_key(&squad.id) {
            registry
                .squads_by_operation
                .insert(squad.operation_id, squad.id);
        }
    }
    registry
        .assignment_diagnostics
        .retain(|key, _| key.player_id != player_id);
    registry
        .assignment_diagnostics
        .extend(assignment_diagnostics);
    world.insert_resource(registry);
}

/// 首都攻略DAGの盤面投影を、対戦ログで検証できる形にして返す。
///
/// routeの形だけでは「どの戦車がどの枝へ送られ、waypointの手前で止まったのか」を
/// 判別できない。そのため、未確保の先頭施設と地上unitの束縛を同じsnapshotに載せる。
pub fn capital_route_diagnostics_for_player(
    world: &World,
    player_id: PlayerId,
) -> serde_json::Value {
    let Some(registry) = world.get_resource::<CapitalRouteTopologyRegistry>() else {
        return serde_json::json!({ "routes": [] });
    };
    let mut keys = registry
        .topologies
        .keys()
        .copied()
        .filter(|key| key.player_id == player_id)
        .collect::<Vec<_>>();
    keys.sort_by_key(|key| key.island_id.0);
    let commitments = world
        .get_resource::<CapitalRoutePathRegistry>()
        .map(|registry| registry.commitments.clone())
        .unwrap_or_default();
    let assignment_diagnostics = world
        .get_resource::<CapitalRoutePathRegistry>()
        .map(|registry| registry.assignment_diagnostics.clone())
        .unwrap_or_default();
    let node_operations = world
        .get_resource::<CapitalRouteNodeOperationRegistry>()
        .map(|registry| registry.operations.clone())
        .unwrap_or_default();
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned();
    let squad_contexts = world
        .get_resource::<crate::ai::squad::SquadManager>()
        .map(|manager| {
            manager
                .squads
                .iter()
                .filter(|squad| squad.plan.owner_id == Some(player_id))
                .flat_map(|squad| {
                    squad.members.iter().map(move |entity| {
                        (
                            *entity,
                            (
                                squad.id,
                                squad.plan.target_island,
                                squad.plan.target,
                                format!("{:?}", squad.plan.mission_type),
                                format!("{:?}", squad.plan.phase),
                                squad.plan.departure_authorized,
                            ),
                        )
                    })
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    let routes = keys
        .into_iter()
        .filter_map(|key| {
            let topology = registry.topologies.get(&key)?;
            let frontiers = capital_route_dag_frontier_properties(world, player_id, key.island_id)
                .unwrap_or_else(|| vec![Vec::new(); topology.routes.len()]);
            let route_frontiers = topology
                .routes
                .iter()
                .enumerate()
                .map(|(route_index, route)| {
                    let properties = frontiers
                        .get(route_index)
                        .into_iter()
                        .flatten()
                        .map(|position| serde_json::json!({ "x": position.x, "y": position.y }))
                        .collect::<Vec<_>>();
                    serde_json::json!({
                        "route_index": route_index,
                        "front_x": route.front.x,
                        "front_y": route.front.y,
                        "frontier_properties": properties,
                    })
                })
                .collect::<Vec<_>>();
            let mut ground_units = Vec::new();
            // 旧トレース利用者向けにTankだけの配列も残す。
            let mut tanks = Vec::new();
            let mut ground_unit_count = 0usize;
            let mut committed_ground_unit_count = 0usize;
            let mut uncommitted_ground_reasons = BTreeMap::<String, usize>::new();
            for entity in world.iter_entities() {
                let (Some(faction), Some(position), Some(stats)) = (
                    entity.get::<Faction>(),
                    entity.get::<GridPosition>(),
                    entity.get::<UnitStats>(),
                ) else {
                    continue;
                };
                if faction.0 != player_id
                    || matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
                    || island_map.as_ref().is_none_or(|islands| {
                        islands
                            .get_island_at(position)
                            .is_none_or(|island| island.id != key.island_id)
                    })
                {
                    continue;
                }
                ground_unit_count = ground_unit_count.saturating_add(1);
                let squad_context = squad_contexts.get(&entity.id());
                let commitment = squad_context
                    .and_then(|(squad_id, _, _, _, _, _)| commitments.get(squad_id))
                    .filter(|commitment| {
                        commitment.player_id == player_id && commitment.island_id == key.island_id
                    });
                if commitment.is_some() {
                    committed_ground_unit_count = committed_ground_unit_count.saturating_add(1);
                }
                let squad_id = squad_context.map(|(squad_id, _, _, _, _, _)| *squad_id);
                let squad_target =
                    squad_context.and_then(|(_, _, target, _, _, _)| *target);
                let squad_mission =
                    squad_context.map(|(_, _, _, mission, _, _)| mission.clone());
                let squad_phase = squad_context.map(|(_, _, _, _, phase, _)| phase.clone());
                let departure_authorized =
                    squad_context.map(|(_, _, _, _, _, authorized)| *authorized);
                let operation_squad_id = world
                    .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
                    .and_then(|registry| registry.assignment(entity.id()))
                    .and_then(|assignment| assignment.squad_id);
                let order_resolved =
                    capital_route_commitment_for_entity(world, player_id, entity.id()).is_some();
                let uncommitted_reason = if commitment.is_some() {
                    None
                } else {
                    Some(
                        match squad_context {
                            None => "no_squad",
                            Some((_, island_id, _, _, _, _))
                                if *island_id != Some(key.island_id) =>
                            {
                                "other_island_squad"
                            }
                            Some((_, _, None, _, _, _)) => "squad_without_target",
                            Some(_) => "squad_target_without_dag_commitment",
                        }
                        .to_owned(),
                    )
                };
                if let Some(reason) = &uncommitted_reason {
                    *uncommitted_ground_reasons
                        .entry(reason.clone())
                        .or_default() += 1;
                }
                let progress = topology.from_home.get(position).copied();
                let target_progress = commitment
                    .and_then(|commitment| topology.from_home.get(&commitment.target).copied());
                let diagnostic = serde_json::json!({
                    "entity_id": entity.id().to_bits(),
                    "unit_type": format!("{:?}", stats.unit_type),
                    "x": position.x,
                    "y": position.y,
                    "progress": progress,
                    "squad_id": squad_id.map(|squad_id| squad_id.0),
                    "squad_mission": squad_mission,
                    "squad_phase": squad_phase,
                    "squad_target_x": squad_target.map(|target| target.x),
                    "squad_target_y": squad_target.map(|target| target.y),
                    "departure_authorized": departure_authorized,
                    "operation_squad_id": operation_squad_id.map(|squad_id| squad_id.0),
                    "order_resolved": order_resolved,
                    "uncommitted_reason": uncommitted_reason,
                    "route_index": commitment.map(|commitment| commitment.route.0),
                    "phase": commitment.map(|commitment| format!("{:?}", commitment.phase)),
                    "target_x": commitment.map(|commitment| commitment.target.x),
                    "target_y": commitment.map(|commitment| commitment.target.y),
                    "target_node": commitment.map(|commitment| commitment.target_node.0),
                    "node_objective": commitment.map(|commitment| format!("{:?}", commitment.objective)),
                    "node_scope": commitment.map(|commitment| format!("{:?}", commitment.scope)),
                    "capture_targets": commitment.map(|commitment| commitment.capture_targets.iter().map(|target| {
                        serde_json::json!({ "x": target.x, "y": target.y })
                    }).collect::<Vec<_>>()),
                    "crossing_exit": commitment.and_then(|commitment| commitment.crossing_exit).map(|target| {
                        serde_json::json!({ "x": target.x, "y": target.y })
                    }),
                    "crossed": commitment.map(|commitment| commitment.crossed),
                    "execution_target_x": commitment.map(|commitment| commitment.execution_target.x),
                    "execution_target_y": commitment.map(|commitment| commitment.execution_target.y),
                    "frontline_capacity": commitment.map(|commitment| commitment.frontline_capacity),
                    "path_len": commitment.map(|commitment| commitment.path.len()),
                    "path_nodes": commitment.map(|commitment| commitment.path_nodes.iter().map(|node| node.0).collect::<Vec<_>>()),
                    // 停滞時に「割当はあるが、次にどの山迂回セルを踏むべきか」を
                    // ログだけで再現できるよう、実行対象のセル列も残す。
                    "path": commitment.map(|commitment| commitment.path.iter().map(|cell| {
                        serde_json::json!({ "x": cell.x, "y": cell.y })
                    }).collect::<Vec<_>>()),
                    "before_target": progress.zip(target_progress).map(|(at, target)| at < target),
                });
                if stats.unit_type == UnitType::Tank {
                    tanks.push(diagnostic.clone());
                }
                ground_units.push(diagnostic);
            }
            ground_units.sort_by_key(|unit| unit["entity_id"].as_u64().unwrap_or_default());
            tanks.sort_by_key(|tank| tank["entity_id"].as_u64().unwrap_or_default());
            let edge_count = topology
                .nodes
                .iter()
                .map(|node| node.successor_nodes.len())
                .sum::<usize>();
            let assignment = assignment_diagnostics
                .get(&key)
                .cloned()
                .unwrap_or_default();
            let mut node_states = node_operations
                .values()
                .filter(|operation| operation.id.topology == key)
                .map(|operation| {
                    let mut squad_ids = operation
                        .assigned_squads
                        .iter()
                        .map(|squad_id| squad_id.0)
                        .collect::<Vec<_>>();
                    squad_ids.sort_unstable();
                    serde_json::json!({
                        "node": operation.id.node.0,
                        "x": operation.anchor.x,
                        "y": operation.anchor.y,
                        "state": format!("{:?}", operation.state),
                        "objective": format!("{:?}", operation.objective),
                        "scope": format!("{:?}", operation.scope),
                        "control_area": operation.control_area.iter().map(|position| {
                            serde_json::json!({ "x": position.x, "y": position.y })
                        }).collect::<Vec<_>>(),
                        "capture_targets": operation.capture_targets.iter().map(|position| {
                            serde_json::json!({ "x": position.x, "y": position.y })
                        }).collect::<Vec<_>>(),
                        "crossing_exit": operation.crossing_exit.map(|position| {
                            serde_json::json!({ "x": position.x, "y": position.y })
                        }),
                        "crossed": operation.crossed,
                        "predecessors": operation.predecessors.iter().map(|predecessor| predecessor.node.0).collect::<Vec<_>>(),
                        "successors": operation.successors.iter().map(|successor| successor.node.0).collect::<Vec<_>>(),
                        "assigned_squad_ids": squad_ids,
                    })
                })
                .collect::<Vec<_>>();
            node_states.sort_by_key(|node| node["node"].as_u64().unwrap_or_default());
            Some(serde_json::json!({
                "island_id": key.island_id.0,
                "start": {
                    "x": topology.nodes[topology.start_node.0].anchor.x,
                    "y": topology.nodes[topology.start_node.0].anchor.y,
                },
                "goal": {
                    "x": topology.nodes[topology.goal_node.0].anchor.x,
                    "y": topology.nodes[topology.goal_node.0].anchor.y,
                },
                "node_count": topology.nodes.len(),
                "edge_count": edge_count,
                "routes": route_frontiers,
                "node_operations": node_states,
                "ground_unit_count": ground_unit_count,
                "committed_ground_unit_count": committed_ground_unit_count,
                "uncommitted_ground_reasons": uncommitted_ground_reasons,
                "assignment": {
                    "eligible_ground_units": assignment.eligible_ground_units,
                    "route_quotas": assignment.route_quotas,
                    "committed_by_route": assignment.committed_by_route,
                    "path_unavailable_by_route": assignment.path_unavailable_by_route,
                },
                "ground_units": ground_units,
                "tanks": tanks,
            }))
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "routes": routes })
}

/// Squad候補へ数えてよい一個の地上memberと、護衛選択に使う現在価値。
struct GroundRouteMember {
    island_id: crate::ai::islands::IslandId,
    can_capture: bool,
    control_value: u64,
}

/// 生存・所有・輸送・移動種別と島を照合し、護衛の現在価値を同じ式で測る。
fn ground_route_member(
    context: &AssignmentContext<'_>,
    entity: Entity,
    island_map: &crate::ai::islands::IslandMap,
    topology_islands: &HashSet<crate::ai::islands::IslandId>,
) -> Option<GroundRouteMember> {
    let world = context.world;
    let player_id = context.player_id;
    let faction = world.get::<Faction>(entity)?;
    let position = world.get::<GridPosition>(entity)?;
    let stats = world.get::<UnitStats>(entity)?;
    if faction.0 != player_id
        || world.get::<Transporting>(entity).is_some()
        || matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
    {
        return None;
    }
    let member_island = island_map.get_island_at(position).map(|island| island.id)?;
    if !topology_islands.contains(&member_island) {
        return None;
    }
    // 兵種名には依存せず、前線で損害を受け止められる現在価値を護衛選定に使う。
    // 間接兵器より直接交戦できる高価・健全な部隊を同じ前進度で先に割り当てる。
    let health_ratio = world
        .get::<Health>(entity)
        .map(|health| u64::from(health.current).saturating_mul(100) / u64::from(health.max.max(1)))
        .unwrap_or(100);
    let direct_fire_weight = if stats.min_range <= 1 { 2_u64 } else { 1_u64 };
    let frontline_control_value = u64::from(stats.cost)
        .saturating_mul(health_ratio)
        .saturating_mul(direct_fire_weight);
    Some(GroundRouteMember {
        island_id: member_island,
        can_capture: stats.can_capture,
        control_value: frontline_control_value,
    })
}
