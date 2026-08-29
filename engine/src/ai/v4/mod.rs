//! V4: 作戦駆動生産 AI。
//!
//! V1〜V3 の生産ロジック（`GamePhase` ごとの理想構成をハードコードし、
//! 構成比の差分で買うものを決める方式）とは完全に分離した独立モジュール。
//!
//! 基本方針:
//! 1. 盤面から「作戦（Operation）」＝獲る／守るべき拠点のまとまりを切り出す
//! 2. 各作戦について、観測量だけから 5 つの枠を逆算する
//!    （占領枠・撃破枠・護衛枠・輸送枠・迎撃枠）
//! 3. 空いている生産枠を、最も不足している枠から順に埋める
//!
//! 「敵を減らす」と「占領する」は別フェーズではなく同一作戦の別枠として
//! 同時に立つため、倒してから占領するのではなく並行して進む。

pub mod campaign_execution;
mod combat_profile;
pub mod deployment;
pub mod logistics_plan;
pub mod operation;
mod operation_selection;
pub mod plan_revision;
pub mod rolling_plan;
pub mod trace;
pub mod victory_roadmap;

use crate::ai::production::plan_campaign_with_expansion_denial_reserve;
use operation::{
    AcquisitionMode, OperationFacts, OperationKind, OperationSlots, RESERVATION_PATIENCE_TURNS,
    SLOT_PRIORITY, SlotKind, SlotTier, acquisition_mode, derive_slots,
};
use operation_selection::{OperationCandidate, select_operation_candidates};
use plan_revision::{
    ActivePlanObjective, DeploymentExecutionObservation, PlanDisposition, PlanStepRef,
    SelectedPlan, V4RollingPlanRegistry,
};
use rolling_plan::{
    DEFAULT_SEARCH_TURNS, EnemyPlanUnit, FriendlyPlanUnit, RollingPlanInput,
    evaluate_fixed_package, plan_force_package, production_options,
};
use trace::{
    CampaignTurnForecastTrace, EnemyProductionForecastTrace, ProductionDecision,
    ProductionOperationTrace, ProductionPlanTrace, ProductionStepTrace, ProductionTraceDiagnostics,
    ReinforcementContingencyTrace, RollingCombatPlanTrace, RollingPurchaseTrace,
    RollingTargetTrace,
};

use crate::ai::squad::{MissionType, SquadId};
use crate::ai::turn_distance::{
    ActionTurnDistanceCache, TerrainConnectivity, calculate_action_distance_to_range,
};
use crate::components::{
    CargoCapacity, Faction, GridPosition, Health, PlayerId, Property, Transporting, UnitStats,
};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Map, MovementType, Players, Terrain, UnitRegistry, UnitType};
use crate::systems::movement::{OccupantInfo, get_valid_movement_cost};
use crate::systems::transport::can_unload_from_terrain;
use bevy_ecs::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// 揚陸可否キャッシュのキー。
///
/// 移動種別と座標を生タプルの順番で取り違えないよう、用途ごとの値オブジェクトにする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DeliveryKey {
    transport_movement: MovementType,
    cargo_movement: MovementType,
    start: GridPosition,
    target: GridPosition,
}

/// 射程内への到達可否キャッシュのキー。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct EngagementKey {
    movement_type: MovementType,
    start: GridPosition,
    target: GridPosition,
    range: u32,
}

/// 到達性まわりの計算結果を、1 回の生産判断のあいだだけ再利用するためのコンテキスト。
///
/// 揚陸可否の判定はマップ全域の走査を伴うため、施設と目標の組み合わせごとに
/// 結果を憶えておかないと候補評価のたびに同じ探索を繰り返すことになる。
#[derive(Default)]
struct ReachCtx {
    terrain: TerrainConnectivity,
    delivery: HashMap<DeliveryKey, bool>,
    engagement: HashMap<EngagementKey, bool>,
    /// 生産地点から射撃可能位置までの実行ターン。余剰増援の候補表だけで使い、
    /// 移動後に攻撃できない間接unitを「今すぐ有効」と誤認しない。
    action_turns: ActionTurnDistanceCache,
}

/// 同一陸塊Campaignで、どの橋を地上部隊が実際に越えたかを保持する。
/// 橋の向こうで北南の進軍路が再合流する地図でも、片方を越えただけで両方を
/// 完了にしないため、地形連結成分ではなく橋Gate単位で記録する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RouteBreakthroughKey {
    island_id: crate::ai::islands::IslandId,
    gate: GridPosition,
}

#[derive(Resource, Debug, Default)]
pub(crate) struct RouteBreakthroughRegistry {
    crossed: HashMap<PlayerId, HashSet<RouteBreakthroughKey>>,
}

/// 首都戦役における進軍DAGを特定するキー。
///
/// 地形と拠点配置は対戦中に変わらないため、同じ勢力・島のDAGを毎手番に再構築しない。
/// 先攻・後攻はそれぞれ自首都を始点にして独立に構築する。片方のDAGを逆順には読まない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CapitalRouteTopologyKey {
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
}

/// 首都戦役DAG内のrouteを識別する値オブジェクト。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CapitalRouteId(usize);

/// 首都戦役DAG内の区間ノードを識別する値オブジェクト。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CapitalRouteNodeId(usize);

/// 静的なセルDAGのNodeを、毎手番に観測・実行できる作戦として識別するID。
///
/// `VictoryRoadmap` の島Campaign IDとは混ぜない。同一島の地上進軍を表すため、
/// topologyとその中のNode番号の組で一意にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CapitalRouteNodeOperationId {
    topology: CapitalRouteTopologyKey,
    node: CapitalRouteNodeId,
}

/// DAG NodeがSquadへ要求する達成内容。
///
/// Nodeのanchorは地理上の代表点であって、Entityがそこへ到着するだけでは達成にならない。
/// Controlは回廊地域での優勢、Captureは地域内に紐づく拠点群の確保を出口条件にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapitalRouteNodeObjective {
    Control,
    Capture,
}

/// 地形・拠点スキャンから決める、DAG Nodeの戦術的な広がり。
///
/// Pointは橋・細い通過点・単一拠点のように、到達すべき一点が明確なNodeである。
/// Areaは分岐点または複数拠点を含む局地戦であり、Squadには中心座標ではなく地域を渡す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapitalRouteNodeScope {
    Point,
    Area,
}

/// 首都間の最短装甲回廊を構成する一マス分のDAGノード。
///
/// 同距離の別ノードは並列に進め、後段の同一ノードへ合流できる。
#[derive(Debug, Clone)]
struct CapitalRouteNode {
    progress: u32,
    anchor: GridPosition,
    successor_nodes: Vec<CapitalRouteNodeId>,
}

/// 始点から分岐する進軍軸を、戦力配分の集計にだけ使う。
///
/// 実際のMilestoneの前後関係は `CapitalRouteTopology::nodes` のDAGで表す。
#[derive(Debug, Clone)]
struct CapitalRoute {
    front: GridPosition,
}

/// 一度だけ地形・拠点を走査して構築する、勢力ごとの前向き首都攻略DAG。
///
/// 所有者・unit・損耗は持たない。毎手番にはこのDAGへ盤面を投影するだけにする。
#[derive(Debug, Clone)]
struct CapitalRouteTopology {
    start_node: CapitalRouteNodeId,
    goal_node: CapitalRouteNodeId,
    nodes: Vec<CapitalRouteNode>,
    routes: Vec<CapitalRoute>,
    property_routes: HashMap<GridPosition, CapitalRouteId>,
    from_home: HashMap<GridPosition, u32>,
}

#[derive(Resource, Debug, Default)]
pub(crate) struct CapitalRouteTopologyRegistry {
    topologies: HashMap<CapitalRouteTopologyKey, CapitalRouteTopology>,
}

/// 首都攻略セルNodeを盤面へ投影した作戦状態。
///
/// topologyは地形だけを持つ静的データのままにし、所有権・前提解放・担当Squadは
/// この台帳だけに置く。これによりセルDAGもRoadmapと同じく「状態を持つNode」になる。
#[derive(Debug, Clone)]
struct CapitalRouteNodeOperation {
    id: CapitalRouteNodeOperationId,
    anchor: GridPosition,
    predecessors: Vec<CapitalRouteNodeOperationId>,
    successors: Vec<CapitalRouteNodeOperationId>,
    objective: CapitalRouteNodeObjective,
    scope: CapitalRouteNodeScope,
    /// anchorと前後のDAGセルで構成する局地作戦地域。戦術器はこの地域に入った後、
    /// anchor一点ではなく敵・地形・占領対象を見て行動する。
    control_area: Vec<GridPosition>,
    /// Capture Nodeが確保すべき、回廊周辺の拠点群。
    capture_targets: Vec<GridPosition>,
    /// 橋Nodeだけが持つ敵側の出口セル。ここへ地上部隊が出るまで、橋手前で優勢でも
    /// Nodeを完了にしない。
    crossing_exit: Option<GridPosition>,
    /// 一度でも橋向こうへ出たという盤面事実を保持する。通過した部隊が次手番にさらに
    /// 前進しても、橋Nodeを未達へ戻さない。
    crossed: bool,
    state: victory_roadmap::RoadmapNodeState,
    assigned_squads: HashSet<SquadId>,
}

/// Area Nodeを局地戦として観測する半径。
///
/// 前後のMilestone座標をそのまま同じ地域へ入れると、map_25のような長い回廊で
/// 自軍首都・中央戦線・敵首都が一つのAreaになり、後方部隊だけでNodeをDominantと
/// 誤判定する。拠点をNodeへ対応付ける半径とそろえ、実際の近傍セルだけを扱う。
const CAPITAL_ROUTE_REGION_RADIUS: u32 = 3;

/// Nodeの戦術地域を、前後Nodeではなくanchor近傍の実セルから構築する。
fn capital_route_node_control_area(
    map: &Map,
    island_map: &crate::ai::islands::IslandMap,
    island_id: crate::ai::islands::IslandId,
    anchor: GridPosition,
    scope: CapitalRouteNodeScope,
    capture_targets: &[GridPosition],
) -> Vec<GridPosition> {
    let mut control_area = match scope {
        CapitalRouteNodeScope::Point => vec![anchor],
        CapitalRouteNodeScope::Area => (0..map.height)
            .flat_map(|y| (0..map.width).map(move |x| GridPosition { x, y }))
            .filter(|position| {
                map.distance(anchor.x, anchor.y, position.x, position.y)
                    <= CAPITAL_ROUTE_REGION_RADIUS
                    && island_map
                        .get_island_at(position)
                        .is_some_and(|island| island.id == island_id)
            })
            .collect::<Vec<_>>(),
    };
    control_area.extend(capture_targets.iter().copied());
    control_area.sort_unstable_by_key(|position| (position.y, position.x));
    control_area.dedup();
    control_area
}

#[derive(Resource, Debug, Default)]
pub(crate) struct CapitalRouteNodeOperationRegistry {
    operations: HashMap<CapitalRouteNodeOperationId, CapitalRouteNodeOperation>,
}

/// 地上Squadへ束縛した、首都攻略DAGの実行区間。
///
/// `target` の座標だけでは山・海峡を迂回する経路が決まらない。そのため分岐から
/// 現在の未確保拠点までのセル列を保持する。Entityはこのorderを持つSquadの
/// memberとしてだけ経路を参照し、DAG自身がEntityへ別の目標を配らない。
#[derive(Debug, Clone)]
struct CapitalRouteCommitment {
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    route: CapitalRouteId,
    /// 区間の完了を判定する先頭未確保拠点。所有権が変わるまで進軍区間は閉じない。
    target: GridPosition,
    /// `target` が対応するセルDAG Node。SquadはこのNode Operationを実行する。
    target_node: CapitalRouteNodeId,
    /// 現在のNodeが要求する達成内容。移動先の一点ではなく、Squadの局地任務を
    /// 決めるために保持する。
    objective: CapitalRouteNodeObjective,
    /// 地形・拠点スキャンで決まったPoint/Areaの扱い。
    scope: CapitalRouteNodeScope,
    /// 現在のNodeに属する回廊地域。
    control_area: Vec<GridPosition>,
    /// Capture Nodeで占領を許可する周辺拠点群。
    capture_targets: Vec<GridPosition>,
    /// 橋Nodeの通過出口と、通過済みの観測結果。
    crossing_exit: Option<GridPosition>,
    crossed: bool,
    /// このEntityが現在のphaseで移動する区間内の終点。
    ///
    /// 防衛はGateまたは最後に確保した拠点まで、地上輸送は配送先まで移動する。
    /// したがって、両者もDAGの所属を保ったまま通常の突撃役と異なる移動をできる。
    execution_target: GridPosition,
    /// 前線Nodeへ同時に入れてよい地上member数。地形から決める収容枠であり、
    /// 敵の数だけ無制限に後続を集めないためにSquad指令へ付随させる。
    frontline_capacity: usize,
    /// 経路の座標列と同じ順序のNode列。地理制約だけでなく、どのNodeを通過するかを
    /// 監査・状態遷移へ渡す。
    path_nodes: Vec<CapitalRouteNodeId>,
    path: Vec<GridPosition>,
    phase: CapitalRouteExecutionPhase,
}

/// DAG区間でのSquadの役割。回復はmemberごとの戦術的な一時状態であり、
/// Squadの戦略orderを書き換えない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapitalRouteExecutionPhase {
    /// Nodeの実行枠を使い、占領・戦闘・突破を担当する前衛Squad。
    Advance,
    /// 前衛枠が埋まっている間、確保済みのDAGセル列へ退避して次の波を待つSquad。
    /// Squadの所属は維持し、DAGがEntityを直接操作することはない。
    Stage,
    Hold,
    Supply,
}

/// Squadの局地任務とDAG区間の役割を合成する。
fn capital_route_execution_phase(mission: &MissionType) -> CapitalRouteExecutionPhase {
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
fn capital_route_controls_active_node(
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
fn capital_route_capture_joins_active_node(
    mission: &MissionType,
    has_explicit_target: bool,
    target_matches_active_route: bool,
    target_belongs_to_other_capture_node: bool,
) -> bool {
    !matches!(mission, MissionType::Capture)
        || !has_explicit_target
        || (target_matches_active_route && !target_belongs_to_other_capture_node)
}

/// 未完了Capture Nodeの施設セルを、占領可能unitのために予約する。
///
/// Areaのanchorと実際の施設が離れている場合、Combat unitも同じ局地戦領域を自由に
/// 動くため、施設上で待機して歩兵を永久に遮断し得る。Node単位の予約にすることで、
/// 別枝から同じAreaへ入ったSquadにも共通の占領セル境界を適用する。
fn capital_route_capture_target_is_reserved(
    operations: &CapitalRouteNodeOperationRegistry,
    player_id: PlayerId,
    position: GridPosition,
) -> bool {
    operations.operations.values().any(|operation| {
        operation.id.topology.player_id == player_id
            && operation.objective == CapitalRouteNodeObjective::Capture
            && !capital_route_node_exit_satisfied(operation)
            && operation.capture_targets.contains(&position)
    })
}

/// Nodeへ同時に進入させる地上member数を、地理的な収容面積から求める。
///
/// これは「最低防衛数」のような固定戦力ではない。Pointは一点を一体ずつ通すしかなく、
/// Areaは実際に部隊が立てる局地セル数だけを前衛へ送る。余剰は後方の待機線へ残し、
/// 生産施設から同じ一点へ無制限に押し込まない。
fn capital_route_node_frontline_capacity(operation: &CapitalRouteNodeOperation) -> usize {
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
fn capital_route_role_frontline_capacity(
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
fn capital_route_uses_capture_lane(
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
fn capital_route_frontline_sort_key(
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
fn capital_route_staging_target(
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

/// 近接する二次元のCapture群を、一つのArea Operationへ統合する。
///
/// 同じ対象集合を複数Nodeへ複製すると、一拠点の奪回で全Nodeが同時に未完了へ戻る。
/// 先頭Nodeだけへ地域の全対象を持たせ、後続Nodeからは対象を除くことで、複数の
/// Capture Squadによる並行占領と、単一の完了判定を両立する。一直線の回廊は統合せず、
/// map_25のように前方へ順番に確保する。
fn consolidate_nearby_capture_target_regions(
    map: &Map,
    node_anchors: &[GridPosition],
    capture_targets: &mut [Vec<GridPosition>],
) {
    let capture_nodes = capture_targets
        .iter()
        .enumerate()
        .filter_map(|(index, targets)| (!targets.is_empty()).then_some(index))
        .collect::<Vec<_>>();
    let mut start = 0;
    while start < capture_nodes.len() {
        let first_node = capture_nodes[start];
        let mut end = start + 1;
        while end < capture_nodes.len()
            && map.distance(
                node_anchors[first_node].x,
                node_anchors[first_node].y,
                node_anchors[capture_nodes[end]].x,
                node_anchors[capture_nodes[end]].y,
            ) <= CAPITAL_ROUTE_REGION_RADIUS
        {
            end += 1;
        }
        let group = &capture_nodes[start..end];
        let mut regional_targets = group
            .iter()
            .flat_map(|index| capture_targets[*index].iter().copied())
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
            capture_targets[first_node] = regional_targets;
            for index in &group[1..] {
                capture_targets[*index].clear();
            }
        }
        start = end;
    }
}

#[derive(Resource, Debug, Default, Clone)]
pub(crate) struct CapitalRoutePathRegistry {
    /// EntityではなくSquadをキーにする。memberの追加・回復・退却があっても、
    /// Squad orderのidentityとDAG上の担当区間は変わらない。
    commitments: HashMap<SquadId, CapitalRouteCommitment>,
    /// 再編でSquad IDが変わった場合に、旧Squadのorderを新Squadへ一括移送するための
    /// 前手番member集合。Entityごとの別命令にはせず、重なる旧Squadが一つの場合だけ
    /// 新しいSquad全体へ継承する。
    commitment_members: HashMap<SquadId, BTreeSet<Entity>>,
    assignment_diagnostics: HashMap<CapitalRouteTopologyKey, CapitalRouteAssignmentDiagnostics>,
}

/// 再編後のSquadへ、前手番の首都戦役orderを継承する。
///
/// Squad IDが維持されていれば直接参照する。IDが変わった場合も、memberが重なる旧Squadが
/// 一つだけなら、そのorderを新Squad全体へ移す。複数の旧Squadを統合した場合はどちらかの
/// Entityの命令を優先せず、現在の需要からSquad単位で再計画する。
fn inherited_capital_route_commitment<'a>(
    registry: &'a CapitalRoutePathRegistry,
    squad_id: SquadId,
    members: &BTreeSet<Entity>,
) -> Option<&'a CapitalRouteCommitment> {
    if let Some(commitment) = registry.commitments.get(&squad_id) {
        return Some(commitment);
    }
    let mut inherited = None;
    for (previous_squad_id, previous_members) in &registry.commitment_members {
        if previous_members.is_disjoint(members) {
            continue;
        }
        let Some(commitment) = registry.commitments.get(previous_squad_id) else {
            continue;
        };
        if inherited.is_some() {
            return None;
        }
        inherited = Some(commitment);
    }
    inherited
}

/// 後方Nodeが再び争奪になっても、すでに先のNodeへ投入した前衛は呼び戻さない。
///
/// Roadmapの先頭未完了Nodeは新規・後続Squadの行き先として使う。一方、同じ経路・同じ
/// 区間目標に対してAdvance/Stage済みのSquadは、担当Nodeが未完了である限り前進を継続する。
/// これにより前線の小さな戦力変動で全軍が往復することを防ぐ。
fn capital_route_active_node_for_squad(
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
fn capital_route_dominant_capture_escort_node(
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

/// 首都戦役DAGの現在区間へ前進している実Entityを、島と区間目標で集計した結果。
///
/// `AssaultCapital`は勝利条件として常在する一方、前段の未確保拠点は`Capture`として
/// 実行する。両者に同じ戦力を同時に見積もらせず、現在の区間を持つ子作戦だけが
/// 既存戦力として受け取るために、目標座標別の集計も保持する。
#[derive(Debug, Default)]
struct AdvancingCapitalRouteEntities {
    by_island: HashMap<crate::ai::islands::IslandId, HashSet<Entity>>,
    by_target: HashMap<(crate::ai::islands::IslandId, GridPosition), HashSet<Entity>>,
    /// 前衛枠が埋まり待機線へ回ったmember。待機中でも作戦に帰属した実戦力であり、
    /// 生産plannerは前線へ到達するまでの時間を付けて既存戦力へ含める。
    ///
    /// 人数だけを持って追加Combatを一律に止めると、plannerは待機部隊を戦力として
    /// 数えない一方で生産だけを止める矛盾になり、資金が残ったまま前線が崩れる。
    staged_by_target: HashMap<(crate::ai::islands::IslandId, GridPosition), HashSet<Entity>>,
}

impl CapitalRoutePathRegistry {
    /// 首都戦役DAGで実際に前進中の戦力を、島と現在区間ごとに返す。
    ///
    /// DAGの経路・Squad・生産plannerが別々の所有権を持つと、盤上では前進している
    /// 戦力を首都攻略の見積だけがゼロと誤認する。回復・輸送・区間保持はここへ入れず、
    /// 次の未確保区間へ進むCombat候補だけを親作戦の既存戦力として共有する。
    fn advancing_combat_entities(
        &self,
        player_id: PlayerId,
        manager: &crate::ai::squad::SquadManager,
    ) -> AdvancingCapitalRouteEntities {
        let mut entities = AdvancingCapitalRouteEntities::default();
        for (squad_id, commitment) in &self.commitments {
            if commitment.player_id != player_id {
                continue;
            }
            let Some(squad) = manager.squads.iter().find(|squad| squad.id == *squad_id) else {
                continue;
            };
            if squad.owner_id != Some(player_id) {
                continue;
            }
            if commitment.phase == CapitalRouteExecutionPhase::Stage {
                let entry = entities
                    .staged_by_target
                    .entry((commitment.island_id, commitment.target))
                    .or_default();
                entry.extend(squad.members.iter().copied());
                continue;
            }
            if commitment.phase != CapitalRouteExecutionPhase::Advance {
                continue;
            }
            for entity in &squad.members {
                entities
                    .by_island
                    .entry(commitment.island_id)
                    .or_default()
                    .insert(*entity);
                entities
                    .by_target
                    .entry((commitment.island_id, commitment.target))
                    .or_default()
                    .insert(*entity);
            }
        }
        entities
    }
}

/// Entityから具体Squadを一意に解決し、そのSquadにRoadmapが与えたDAG orderを返す。
/// ここでEntity自身の位置・兵種・HPから別のrouteを選ばないことが、DAGを第二の
/// Entity割当器にしないための境界である。
fn capital_route_commitment_for_entity(
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

/// 経路へ入れなかった理由を、Squad外観ではなく割当処理の時点で残す。
/// これにより「DAG所属が少ない」を戦力配分不足と経路構築失敗に分けて観測できる。
#[derive(Debug, Default, Clone)]
struct CapitalRouteAssignmentDiagnostics {
    eligible_ground_units: usize,
    route_quotas: Vec<usize>,
    committed_by_route: Vec<usize>,
    path_unavailable_by_route: Vec<usize>,
}

impl ReachCtx {
    /// 地形連結による到達判定（`TerrainConnectivity` への委譲）。
    fn is_reachable(
        &mut self,
        map: &Map,
        registry: &MasterDataRegistry,
        start: (usize, usize),
        target: (usize, usize),
        movement_type: MovementType,
    ) -> bool {
        self.terrain
            .is_reachable(map, registry, start, target, movement_type)
    }

    /// 標的位置そのものではなく、射程内の合法地形へ到達できるかを判定する。
    /// 艦船の遠距離射撃を「陸上座標へ入れない」という理由で候補外にしない。
    fn can_reach_engagement_envelope(
        &mut self,
        map: &Map,
        registry: &MasterDataRegistry,
        start: (usize, usize),
        target: (usize, usize),
        movement_type: MovementType,
        max_range: u32,
    ) -> bool {
        let key = EngagementKey {
            movement_type,
            start: GridPosition {
                x: start.0,
                y: start.1,
            },
            target: GridPosition {
                x: target.0,
                y: target.1,
            },
            range: max_range.max(1),
        };
        if let Some(cached) = self.engagement.get(&key) {
            return *cached;
        }
        let range = max_range.max(1);
        let reachable = (0..map.height).any(|y| {
            (0..map.width).any(|x| {
                map.distance(x, y, target.0, target.1) <= range
                    && map.get_terrain(x, y).is_some_and(|terrain| {
                        crate::systems::movement::get_valid_movement_cost(
                            registry,
                            movement_type,
                            terrain,
                        )
                        .is_some()
                    })
                    && self.is_reachable(map, registry, start, (x, y), movement_type)
            })
        });
        self.engagement.insert(key, reachable);
        reachable
    }
}

/// 敵がこのターン数以内に到達できる自軍拠点は防衛作戦の対象とする。
const DEFENSE_THREAT_ETA: u32 = 2;

/// 占領開始後に拠点を確保し切るまでに必要な最小手番数。
const CAPTURE_COMPLETION_TURNS: u32 = 2;

/// Expectedへ入れる敵生産の波数。Expectedは毎ターン再評価する直近波であり、
/// 作戦期限全体の全生産を一括で仮想敵へ積むStressとは意図的に分ける。
const EXPECTED_REINFORCEMENT_WAVES: u32 = 1;

/// 傾向推定に残す直近の観測手番数。古い開幕編成を現在の生産傾向に混ぜない。
const ENEMY_PRODUCTION_HISTORY_TURNS: u32 = 6;

/// 実際に新規観測した敵Entity 1 体分の生産実績。
#[derive(Debug, Clone, Copy)]
struct ObservedEnemyProduction {
    turn: u32,
    unit_type: UnitType,
    cost: u32,
}

/// 予測した次回観測と、その手番で照合する集計値。
#[derive(Debug, Clone, Copy)]
struct PendingEnemyProductionForecast {
    turn: u32,
    units: u32,
    cost: u32,
}

/// 敵生産の実績・予測誤差を、自軍AIごとに保持する状態。
#[derive(Debug, Default)]
struct EnemyProductionEstimatorState {
    initialized: bool,
    known_entities: HashSet<Entity>,
    history: VecDeque<ObservedEnemyProduction>,
    pending: VecDeque<PendingEnemyProductionForecast>,
    evaluated_samples: u32,
    absolute_unit_error_sum: u32,
    absolute_cost_error_sum: u32,
}

/// 敵の実生産履歴から次手番の生産量を推定する台帳。
///
/// 初回走査で盤上にいる敵は開幕配置か既存戦力かを区別できないため、学習対象にしない。
/// 以後に新規出現したEntityだけを実生産として扱う。
#[derive(Resource, Debug, Default)]
struct EnemyProductionEstimator {
    by_observer: HashMap<PlayerId, EnemyProductionEstimatorState>,
}

/// 直近の実生産から、次の敵手番に現れる量を予測して前回予測を採点する。
///
/// 基準は履歴全体の平均で、直近2手番の平均がその前の2手番を上回るときだけ
/// 差分を加える。したがって「同じ傾向が続き、増加中なら少し増える」予測になり、
/// 施設数×期限の最悪ケースをそのまま要求量へ変換しない。
fn observe_enemy_production(
    world: &mut World,
    player_id: PlayerId,
    turn: u32,
    scan: &BoardScan,
) -> EnemyProductionForecastTrace {
    let mut estimator = world
        .remove_resource::<EnemyProductionEstimator>()
        .unwrap_or_default();
    let state = estimator.by_observer.entry(player_id).or_default();
    let visible = scan
        .enemy_units
        .iter()
        .filter_map(|unit| unit.entity.map(|entity| (entity, unit)))
        .collect::<HashMap<_, _>>();

    if !state.initialized {
        state.initialized = true;
        state.known_entities = visible.keys().copied().collect();
        world.insert_resource(estimator);
        return EnemyProductionForecastTrace::default();
    }

    let new_productions = visible
        .iter()
        .filter(|(entity, _)| !state.known_entities.contains(entity))
        .map(|(_, unit)| ObservedEnemyProduction {
            turn,
            unit_type: unit.stats.unit_type,
            cost: unit.stats.cost,
        })
        .collect::<Vec<_>>();
    state.known_entities = visible.keys().copied().collect();

    let actual_units = u32::try_from(new_productions.len()).unwrap_or(u32::MAX);
    let actual_cost = new_productions
        .iter()
        .map(|production| production.cost)
        .fold(0_u32, u32::saturating_add);
    while state
        .pending
        .front()
        .is_some_and(|forecast| forecast.turn <= turn)
    {
        let forecast = state.pending.pop_front().expect("frontで存在を確認済み");
        state.evaluated_samples = state.evaluated_samples.saturating_add(1);
        state.absolute_unit_error_sum = state
            .absolute_unit_error_sum
            .saturating_add(forecast.units.abs_diff(actual_units));
        state.absolute_cost_error_sum = state
            .absolute_cost_error_sum
            .saturating_add(forecast.cost.abs_diff(actual_cost));
    }
    state.history.extend(new_productions);
    let oldest_turn = turn.saturating_sub(ENEMY_PRODUCTION_HISTORY_TURNS.saturating_sub(1));
    while state
        .history
        .front()
        .is_some_and(|production| production.turn < oldest_turn)
    {
        state.history.pop_front();
    }

    let history_start = state
        .history
        .front()
        .map_or(turn, |production| production.turn);
    let observed_turns = turn.saturating_sub(history_start).saturating_add(1).max(1);
    let observed_count = u32::try_from(state.history.len()).unwrap_or(u32::MAX);
    let baseline_per_turn = observed_count.div_ceil(observed_turns);
    let recent_start = turn.saturating_sub(1);
    let previous_start = turn.saturating_sub(3);
    let previous_end = turn.saturating_sub(2);
    let recent_count = state
        .history
        .iter()
        .filter(|production| production.turn >= recent_start)
        .count() as u32;
    let previous_count = state
        .history
        .iter()
        .filter(|production| production.turn >= previous_start && production.turn <= previous_end)
        .count() as u32;
    // 増加傾向は比較対象となる過去2手番が揃ってからだけ加える。初回に1体
    // 観測しただけで「増加中」と誤認して予測を二倍にしない。
    let growth_per_turn = if turn.saturating_sub(history_start) >= 3 {
        recent_count
            .div_ceil(2)
            .saturating_sub(previous_count.div_ceil(2))
    } else {
        0
    };
    let expected_units = baseline_per_turn
        .saturating_add(growth_per_turn)
        .min(scan.enemy_production_slots);
    let observed_cost = state
        .history
        .iter()
        .map(|production| production.cost)
        .fold(0_u32, u32::saturating_add);
    let average_cost = observed_cost / observed_count.max(1);
    let expected_cost = average_cost.saturating_mul(expected_units);
    let dominant_unit_type = state
        .history
        .iter()
        .fold(Vec::<(UnitType, u32)>::new(), |mut counts, production| {
            if let Some((_, count)) = counts
                .iter_mut()
                .find(|(unit_type, _)| *unit_type == production.unit_type)
            {
                *count = count.saturating_add(1);
            } else {
                counts.push((production.unit_type, 1));
            }
            counts
        })
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(unit_type, _)| unit_type);
    state.pending.push_back(PendingEnemyProductionForecast {
        turn: turn.saturating_add(1),
        units: expected_units,
        cost: expected_cost,
    });
    let samples = state.evaluated_samples.max(1);
    let forecast = EnemyProductionForecastTrace {
        expected_units_next_turn: expected_units,
        expected_cost_next_turn: expected_cost,
        dominant_unit_type,
        evaluated_samples: state.evaluated_samples,
        mean_absolute_unit_error: state.absolute_unit_error_sum / samples,
        mean_absolute_cost_error: state.absolute_cost_error_sum / samples,
    };
    world.insert_resource(estimator);
    forecast
}

/// 盤面から取り出したユニット 1 体分の情報。
#[derive(Debug, Clone)]
struct UnitSnapshot {
    /// 盤面上の実Entity。純粋関数テストの合成snapshotではNoneを許容する。
    entity: Option<Entity>,
    pos: GridPosition,
    stats: UnitStats,
    hp: u32,
    free_cargo: u32,
}

/// 敵が実際に生産へ使える施設。所有者の首都から生産範囲内にある施設だけを保持する。
#[derive(Debug, Clone, Copy)]
struct EnemyFacilitySnapshot {
    pos: GridPosition,
    terrain: Terrain,
}

/// 敵施設が作戦地点へ到達させられる将来戦力の、用途別の見積り。
///
/// `expected_funds` は継続生産の必要量を作るための通常scenario、
/// `stress_funds` は敵が施設・収入をより強く使った場合の再計画監視用である。
/// 最小の進撃Goはこの値を読まない。Goを楽観的に保つことと、増援を作り続けることを
/// 同じ閾値にしないためである。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct EnemyReinforcementEnvelope {
    expected_funds: u32,
    stress_funds: u32,
}

/// 1体の敵に対する実盤面情報。
///
/// 戦力を購入価格へ換算しない。必要戦力はRollingPlanがHP・与ダメージ・移動・
/// 攻撃可能回数を手番ごとにシミュレーションして決める。
#[derive(Debug, Clone)]
struct ThreatTarget {
    entity: Option<Entity>,
    stats: UnitStats,
    position: GridPosition,
    current_hp: u32,
    /// 0は現在の局地敵。1以上はanchorへ到着してから交戦可能になる観測済み増援。
    available_turn: u32,
}

impl ThreatTarget {
    fn from_snapshot(unit: &UnitSnapshot) -> Self {
        Self {
            entity: unit.entity,
            stats: unit.stats.clone(),
            position: unit.pos,
            current_hp: unit.hp,
            available_turn: 0,
        }
    }
}

/// 1 つの作戦。対象拠点のまとまりと、そこから導出された枠を保持する。
#[derive(Debug)]
struct Operation {
    kind: OperationKind,
    /// 島campaignの永続identity。anchorや未所有施設集合が変わっても維持する。
    island_id: Option<crate::ai::islands::IslandId>,
    /// 作戦の代表地点（距離計算の基準）
    anchor: GridPosition,
    /// 編成中の戦力を逐次投入せず集結させる、自軍側の安全な地点。
    staging_anchor: GridPosition,
    /// falseの首都作戦は生産だけ進め、攻撃任務へ切り替えない。
    execution_authorized: bool,
    /// 占領完了まで敵の攻撃から生存させる必要があるcampaign占領Entity。
    protected_capture_entities: HashSet<Entity>,
    /// anchorが所有権変化で動いても同じ目的を照合するための拠点集合。
    objective_properties: Vec<GridPosition>,
    /// 防衛の硬い期限と、旧枠の診断にだけ使う作戦時間幅。
    /// 敵を作戦へ帰属させる条件には使わない。
    threat_horizon: u32,
    facts: OperationFacts,
    slots: OperationSlots,
    /// この生産計画の中で既に購入した分
    filled: OperationSlots,
    /// 自軍が生産しうるどの移動タイプでも到達できない位置にいる敵（＝迎え撃つしかない敵）
    unreachable_threats: Vec<ThreatTarget>,
    /// 自軍が生産しうるいずれかの移動タイプで到達できる位置にいる敵（＝殴りに行ける敵）
    reachable_threats: Vec<ThreatTarget>,
    /// 通常見積りで前線へ到着する将来増援。RollingPlanの仮想敵として含める。
    expected_reinforcements: Vec<EnemyPlanUnit>,
    /// 観測後に間に合うため、具体的counterと生産slotだけを予約する条件付き計画。
    reinforcement_contingencies: Vec<ReinforcementContingency>,
    /// Expectedを上回る敵の最大対抗生産額。Goは止めず、次回の再計画理由として記録する。
    stress_reinforcement_funds: u32,
    contingency_reserve_funds: u32,
}

#[derive(Debug, Clone, Copy)]
struct ReinforcementContingency {
    enemy_type: UnitType,
    enemy_contact_turn: u32,
    counter_type: UnitType,
    counter_facility: GridPosition,
    counter_build_turn: u32,
    counter_contact_turn: u32,
    attacks_required: u32,
    reserve_cost: u32,
}

/// 1手番の全施設について一度だけ作ったV4生産計画。
/// 1命令ごとの再計画で残存脅威台帳を初期化しないため、同じ計画を順に消費する。
#[derive(Resource, Debug, Default)]
struct V4ProductionTurnPlan {
    player_id: Option<PlayerId>,
    turn: u32,
    commands: VecDeque<PlannedProduction>,
}

/// 生産命令を施設ごとに返す間も、作戦実績の盤面scanは1手番に一度だけ行います。
#[derive(Resource, Debug, Default)]
struct V4ProductionObservationCache {
    player_id: Option<PlayerId>,
    turn: u32,
}

/// 計画済み列から実際に返す1命令だけをIssuedへ進める。
fn issue_planned_production(
    world: &mut World,
    turn: u32,
    planned: Option<PlannedProduction>,
) -> Vec<ProduceUnitCommand> {
    let Some(planned) = planned else {
        return Vec::new();
    };
    if let Some(step_ref) = planned
        .deployment
        .as_ref()
        .and_then(|deployment| deployment.plan_step)
        && let Some(mut registry) = world.get_resource_mut::<V4RollingPlanRegistry>()
    {
        registry.mark_issued(step_ref, turn);
    }
    if planned.capture_intent.is_some()
        && let Some(mut registry) =
            world.get_resource_mut::<campaign_execution::V4CampaignExecutionRegistry>()
    {
        registry.mark_issued(planned.command.player_id, turn, &planned.command);
    }
    vec![planned.command]
}

/// 島嶼キャンペーンの完全パッケージをV4の汎用作戦より先に処理した結果。
enum CampaignProductionControl {
    /// 今回の呼び出しで発行するキャンペーン生産命令。
    Command(ProduceUnitCommand),
    /// 高優先作戦を完成できないため、汎用生産へ予算を流さず終了する。
    BlockGeneric,
    /// 島嶼作戦の予約額を除いた余剰だけで、迎撃・戦闘枠を生産する。
    ContinueWithSurplus(u32),
    /// キャンペーン要求が無いか全行を完成済みなので、V4汎用生産へ進める。
    Continue,
}

/// 生産候補 1 件。
#[derive(Debug, Clone, Copy)]
struct SlotCandidate {
    unit_type: UnitType,
    cost: u32,
    facility: GridPosition,
    /// 枠への適合度。大きいほど良い。
    fitness: f32,
}

/// 余剰Combat生産について、あらかじめ計算した「1回の攻撃」での前線への寄与。
///
/// 1体は同一手番に複数の敵を攻撃できない。候補ごとの経路探索を終えた後は、この
/// 小さな表だけを使って工場枠と敵HPを対応付ける。工場ごとに戦闘を再探索しない。
#[derive(Debug, Clone, Copy)]
struct ImmediateCombatEngagement {
    entity: Entity,
    current_hp: u32,
    damage: u32,
    fitness: f32,
}

/// 余剰Combat候補と、到達可能な実在前線Entityへの攻撃表。
#[derive(Debug, Clone)]
struct ImmediateCombatOption {
    operation_index: usize,
    candidate: SlotCandidate,
    engagements: Vec<ImmediateCombatEngagement>,
}

#[derive(Debug, Clone, Copy)]
struct CandidateConstraints {
    remaining_funds: u32,
    per_slot_budget: u32,
}

/// 生産命令と、その命令だけが持つV4作戦意図。
#[derive(Debug)]
struct PlannedProduction {
    command: ProduceUnitCommand,
    deployment: Option<PlannedDeployment>,
    /// Capture枠は完成時点からRoadmapの受入Squadに所属させる。Combat deploymentとは
    /// 別に保持し、占領歩兵をReserve/Defenseへ落とさない。
    capture_intent: Option<campaign_execution::CampaignProductionIntent>,
}

/// Combat / Intercept枠からpending deploymentへ渡す情報。
#[derive(Debug)]
struct PlannedDeployment {
    anchor: GridPosition,
    staging_anchor: GridPosition,
    posture: deployment::DeploymentPosture,
    slot_kind: SlotKind,
    priority_enemies: Vec<Entity>,
    threat_horizon: u32,
    forecast: deployment::DeploymentForecast,
    /// 永続Combat計画の生産step。旧Intercept等ではNone。
    plan_step: Option<PlanStepRef>,
}

impl std::ops::Deref for PlannedProduction {
    type Target = ProduceUnitCommand;

    fn deref(&self) -> &Self::Target {
        &self.command
    }
}

/// V4 の生産意思決定エントリポイント。
///
/// `decide_production` から `AiVersion::uses_operation_driven_production()` が
/// true のときだけ委譲される。V1/V2/V3 の経路には一切影響しない。
pub fn decide_production_v4(world: &mut World, player_id: PlayerId) -> Vec<ProduceUnitCommand> {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    // 島嶼キャンペーン生産だけでreturnする手番も含め、作戦の実績は毎ターン観測する。
    // 生産APIは施設ごとに呼ばれるため、同じ手番の2体目以降では再走査しない。
    let already_observed = world
        .get_resource::<V4ProductionObservationCache>()
        .is_some_and(|cache| cache.player_id == Some(player_id) && cache.turn == turn);
    if !already_observed {
        observe_plan_execution(world, player_id, turn);
        world.insert_resource(V4ProductionObservationCache {
            player_id: Some(player_id),
            turn,
        });
    }

    let campaign_surplus = match decide_campaign_production_v4(world, player_id) {
        CampaignProductionControl::Command(command) => return vec![command],
        CampaignProductionControl::BlockGeneric => return Vec::new(),
        CampaignProductionControl::ContinueWithSurplus(budget) => Some(budget),
        CampaignProductionControl::Continue => None,
    };

    let mut turn_plan = world
        .remove_resource::<V4ProductionTurnPlan>()
        .unwrap_or_default();
    if turn_plan.player_id == Some(player_id) && turn_plan.turn == turn {
        let next = turn_plan.commands.pop_front();
        world.insert_resource(turn_plan);
        return issue_planned_production(world, turn, next);
    }

    let Some(mut scan) = BoardScan::collect(world, player_id) else {
        world.insert_resource(turn_plan);
        return Vec::new();
    };
    // 敵の初期配置は学習せず、前回走査後に現れた実Entityだけで次手番を予測する。
    let enemy_production_forecast = observe_enemy_production(world, player_id, turn, &scan);
    scan.enemy_production_forecast = enemy_production_forecast;
    if let Some(budget) = campaign_surplus {
        scan.funds = scan.funds.min(budget);
    }
    // 既存戦力として見積へ入れるのは、実際にV4 Combat任務へ接続済みのEntityだけ。
    // 占領・輸送など別任務中のunitを「倒せるはず」と二重計上しない。
    let committed_combat_assignments = world
        .get_resource::<deployment::V4DeploymentRegistry>()
        .map(|registry| registry.active_target_assignments(player_id))
        .unwrap_or_default();
    // Squad計画でDAGへ束縛した前進部隊を、首都作戦の既存戦力として同じ手番の
    // 生産plannerへ渡す。PlanIdの有無ではなく、親戦役の未完了区間へ実際に進む
    // という作戦状態を正本にする。
    let advancing_route_entities = world
        .get_resource::<CapitalRoutePathRegistry>()
        .zip(world.get_resource::<crate::ai::squad::SquadManager>())
        .map(|(registry, manager)| registry.advancing_combat_entities(player_id, manager))
        .unwrap_or_default();
    let produced_plan_steps = world
        .get_resource::<deployment::V4DeploymentRegistry>()
        .map(|registry| registry.produced_plan_steps(player_id))
        .unwrap_or_default();
    let mut rolling_registry = world
        .remove_resource::<V4RollingPlanRegistry>()
        .unwrap_or_default();
    rolling_registry.reconcile_produced_steps(player_id, &produced_plan_steps);
    let (planned, mut plan_trace) = plan_production_with_registry(
        &scan,
        player_id,
        campaign_surplus.is_none(),
        &committed_combat_assignments,
        &advancing_route_entities,
        turn,
        &mut rolling_registry,
    );
    plan_trace.enemy_production_forecast = enemy_production_forecast;
    // 陸続き前線ではCapture候補の選定を通常plannerへ委ねている。それでも生産完了後の
    // 受入先はRoadmap NodeのCapture Squadでなければならないため、発注意図だけを
    // Campaign実行registryへ追記する。
    let capture_intents = planned
        .iter()
        .filter_map(|planned| planned.capture_intent.clone())
        .collect::<Vec<_>>();
    if !capture_intents.is_empty() {
        world.init_resource::<campaign_execution::V4CampaignExecutionRegistry>();
        world
            .resource_mut::<campaign_execution::V4CampaignExecutionRegistry>()
            .append_turn_intents(player_id, turn, &capture_intents);
    }
    let closed_plan_ids = rolling_registry
        .audit_records(player_id)
        .into_iter()
        .filter(|audit| audit.turn == turn)
        .filter(|audit| {
            matches!(
                audit.disposition,
                PlanDisposition::Completed | PlanDisposition::Withdrawn
            )
        })
        .map(|audit| audit.plan_id)
        .collect::<HashSet<_>>();
    let active_plan_intents = rolling_registry.active_deployment_intents(
        player_id,
        scan.capital_assault_authorized,
        scan.capital_staging_anchor,
    );
    world.insert_resource(rolling_registry);

    // 診断traceとは別に、生産完了イベントと照合する作戦意図を永続化する。
    let pending = planned
        .iter()
        .enumerate()
        .filter_map(|(order, planned)| {
            let deployment = planned.deployment.as_ref()?;
            Some(deployment::PendingDeployment {
                player_id,
                turn,
                order: u32::try_from(order).unwrap_or(u32::MAX),
                facility: GridPosition {
                    x: planned.command.target_x,
                    y: planned.command.target_y,
                },
                unit_type: planned.command.unit_type,
                anchor: deployment.anchor,
                staging_anchor: deployment.staging_anchor,
                posture: deployment.posture,
                slot_kind: deployment.slot_kind,
                priority_enemies: deployment.priority_enemies.clone(),
                threat_horizon: deployment.threat_horizon,
                forecast: deployment.forecast,
                plan_step: deployment.plan_step,
                forming_slot: None,
            })
        })
        .collect::<Vec<_>>();
    let mut deployment_registry = world
        .remove_resource::<deployment::V4DeploymentRegistry>()
        .unwrap_or_default();
    deployment_registry.release_closed_plans(&closed_plan_ids);
    deployment_registry.replace_turn_orders(player_id, turn, pending);
    deployment_registry.refresh_plan_intents(&active_plan_intents);
    world.insert_resource(deployment_registry);
    // 生産判断の内訳を診断リソースへ残す（判定は行わず記録のみ）。
    if let Some(mut diagnostics) = world.get_resource_mut::<ProductionTraceDiagnostics>() {
        diagnostics.record(turn, plan_trace);
    } else {
        let mut diagnostics = ProductionTraceDiagnostics::default();
        diagnostics.record(turn, plan_trace);
        world.insert_resource(diagnostics);
    }

    turn_plan.player_id = Some(player_id);
    turn_plan.turn = turn;
    turn_plan.commands = VecDeque::from(planned);
    let next = turn_plan.commands.pop_front();
    world.insert_resource(turn_plan);
    issue_planned_production(world, turn, next)
}

/// 生産計画を実Entityの戦闘・損耗・目標HP・拠点占領へ接続する予実集計。
///
/// 診断用の金額はUnitTypeの生産費と現在HPから導出する。計画変更の判定自体は
/// 金額価値ではなく、初攻撃・敵排除・拠点占領の予定手番との差を使用する。
fn observe_plan_execution(world: &mut World, player_id: PlayerId, turn: u32) {
    let (audit_records, produced_steps) = world
        .get_resource::<deployment::V4DeploymentRegistry>()
        .map(|registry| {
            (
                registry.audit_records(player_id),
                registry.produced_plan_steps(player_id),
            )
        })
        .unwrap_or_default();

    let unit_costs = world
        .get_resource::<UnitRegistry>()
        .map(|registry| {
            audit_records
                .iter()
                .filter_map(|record| {
                    registry
                        .get_stats(record.unit_type)
                        .map(|stats| (record.unit_type, stats.cost))
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    let mut owned_properties = HashSet::new();
    let mut property_query = world.query::<(&GridPosition, &Property)>();
    for (position, property) in property_query.iter(world) {
        if property.owner_id == Some(player_id) {
            owned_properties.insert(*position);
        }
    }

    // 搭載中の敵も増援・残存目標として追うため、盤面走査の除外条件を使わない。
    let mut enemy_health = HashMap::new();
    let mut enemy_query = world.query::<(Entity, &Faction, &Health)>();
    for (entity, faction, health) in enemy_query.iter(world) {
        if faction.0 != player_id && health.current > 0 {
            enemy_health.insert(entity, health.current);
        }
    }

    let deployments = audit_records
        .into_iter()
        .filter_map(|record| {
            let plan_step = record.plan_step?;
            let combat_actuals = record.plan_combat_actuals();
            let unit_cost = unit_costs.get(&record.unit_type).copied().unwrap_or(0);
            let health = world.get::<Health>(record.entity).copied();
            let alive =
                world.get_entity(record.entity).is_ok() && health.is_some_and(|hp| hp.current > 0);
            let current_loss_value = health.map_or(unit_cost, |hp| {
                unit_cost.saturating_mul(hp.max.saturating_sub(hp.current)) / hp.max.max(1)
            });
            Some(DeploymentExecutionObservation {
                entity: record.entity,
                plan_id: plan_step.plan_id,
                plan_step: Some(plan_step),
                unit_cost,
                alive,
                // higher-priority任務へ一時preemptされSquadを失ったEntityを、Planが
                // 実行中の戦力として数えない。Defense待機はSquadを持つため残る。
                mission_active: alive && record.active && record.squad_id.is_some(),
                current_loss_value,
                // PlanIdを持つ生産Entityの全戦闘を、そのPlanの実績へ一度だけ戻す。
                // 「現在標的だけ」に絞ると、同一作戦圏内の好機標的へ切り替えた攻撃や、
                // revisionで標的が変わる前の戦闘が予実から欠落する。
                first_attack_turn: combat_actuals.first_attack_turn,
                attack_count: combat_actuals.attack_count,
                priority_attack_count: combat_actuals.priority_attack_count,
                kill_count: combat_actuals.kill_count,
                damage_value_dealt: combat_actuals.damage_value_dealt,
                counter_value_received: combat_actuals.counter_value_received,
                destroyed_value: combat_actuals.destroyed_value,
            })
        })
        .collect::<Vec<_>>();

    let mut registry = world
        .remove_resource::<V4RollingPlanRegistry>()
        .unwrap_or_default();
    registry.reconcile_produced_steps(player_id, &produced_steps);
    registry.observe_execution(
        player_id,
        turn,
        &owned_properties,
        &enemy_health,
        &deployments,
    );
    world.insert_resource(registry);
}

/// 戦術層が組んだ島嶼キャンペーンの不足を、V4固有の汎用作戦より先に生産する。
///
/// `AiTurnStrategyCache` は行動計画時に同じプレイヤーのportfolioを保持している。
/// 生産APIは1命令ごとに呼ばれるため、V3と同じcache queueへ完全パッケージを保存し、
/// shortfallの再計算による二重発注を防ぐ。高優先行を完成できないときはgenericを
/// blockし、輸送・積荷・戦闘戦力の一部だけを逐次投入しない。
fn decide_campaign_production_v4(
    world: &mut World,
    player_id: PlayerId,
) -> CampaignProductionControl {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let plan_exists = world
        .get_resource::<crate::ai::engine::AiTurnStrategyCache>()
        .is_some_and(|cache| cache.campaign_production_planned(player_id));
    if plan_exists {
        let mut cache = world
            .remove_resource::<crate::ai::engine::AiTurnStrategyCache>()
            .unwrap_or_default();
        let next = cache.take_campaign_production_command(player_id);
        let blocks_generic = cache.campaign_production_blocks_generic(player_id);
        let generic_budget = cache.campaign_production_generic_budget(player_id);
        world.insert_resource(cache);
        return match next {
            Some(command) => {
                mark_campaign_production_issued(world, player_id, turn, &command);
                CampaignProductionControl::Command(command)
            }
            None if blocks_generic => CampaignProductionControl::BlockGeneric,
            None if generic_budget.is_some_and(|budget| budget > 0) => {
                CampaignProductionControl::ContinueWithSurplus(generic_budget.unwrap_or(0))
            }
            None => CampaignProductionControl::Continue,
        };
    }

    let mut shortfalls = world
        .get_resource::<crate::ai::engine::AiTurnStrategyCache>()
        .and_then(|cache| cache.campaign_portfolio(player_id))
        .map(|portfolio| portfolio.aggregate_missing_requirements())
        .unwrap_or_default();
    if shortfalls.is_empty() {
        return CampaignProductionControl::Continue;
    }

    let Some(scan) = BoardScan::collect(world, player_id) else {
        return CampaignProductionControl::BlockGeneric;
    };

    // 陸続きで自力到達できる前線まで島campaign専用生産へ通すと、占領枠を
    // 全て買い終えるまで時系列Combat Planが起動せず、裸の占領兵を逐次損耗させる。
    // ただし橋頭堡へ戦車を到達させる構造要求は、通常生産へ渡すと歩兵へ置換されて
    // 消えてしまう。直接到達できる前線でも、その要求だけは専用経路に残す。
    // 目標一覧そのものはcampaign/roadmapに残るため、単一目標へ縮退することはない。
    let mut reach = ReachCtx::default();
    let armored_entry_cost = scan
        .available_types
        .iter()
        .filter(|(unit_type, stats)| {
            matches!(
                unit_type,
                UnitType::Tank | UnitType::MdTank | UnitType::TankZ
            ) && stats.movement_type == MovementType::Tank
        })
        .map(|(_, stats)| stats.cost)
        .min()
        .unwrap_or(0);
    shortfalls.retain_mut(|shortfall| {
        if !has_direct_capture_route(&scan, &mut reach, shortfall) {
            return true;
        }
        if shortfall.ground_combat_units == 0 {
            return false;
        }

        // 同一陸塊では占領・通常戦闘はrolling planへ委ねる。一方で橋頭堡への
        // 装甲到達は代替不能な前提条件なので、必要台数とその予約額だけを保つ。
        shortfall.light_transport_slots = 0;
        shortfall.heavy_transport_slots = 0;
        shortfall.capture_units = 0;
        shortfall.combat_units = 0;
        shortfall.reserved_budget =
            armored_entry_cost.saturating_mul(shortfall.ground_combat_units);
        true
    });
    if shortfalls.is_empty() {
        return CampaignProductionControl::Continue;
    }

    // 航空掃討は後段のrolling planへ委譲する。一方、敵領Assaultで輸送・配置枠まで
    // 予約済みの地上波はcampaign Squadへ渡さないと港で遊兵になるため、必要実体数と
    // その購入上限だけを残す。
    for shortfall in &mut shortfalls {
        if shortfall.decision != crate::ai::island_campaign::IslandCampaignDecision::Assault
            || shortfall.ground_combat_units == 0
        {
            shortfall.combat_units = 0;
        }
    }

    let enemy_stats: Vec<_> = scan
        .enemy_units
        .iter()
        .map(|unit| unit.stats.clone())
        .collect();
    let outcome = plan_campaign_with_expansion_denial_reserve(
        player_id,
        &shortfalls,
        &scan.free_facilities,
        scan.owned_airport_count,
        &scan.available_types,
        &enemy_stats,
        &scan.damage_chart,
        &scan.map,
        &scan.master_data,
        scan.funds,
    );
    world.init_resource::<campaign_execution::V4CampaignExecutionRegistry>();
    world
        .resource_mut::<campaign_execution::V4CampaignExecutionRegistry>()
        .replace_turn_intents(player_id, turn, &outcome.intents);
    let generic_budget = outcome.generic_funds;
    let mut cache = world
        .remove_resource::<crate::ai::engine::AiTurnStrategyCache>()
        .unwrap_or_default();
    cache.set_campaign_production_plan_with_generic_budget(
        player_id,
        outcome.commands,
        generic_budget,
    );
    let next = cache.take_campaign_production_command(player_id);
    let blocks_generic = cache.campaign_production_blocks_generic(player_id);
    let generic_budget = cache.campaign_production_generic_budget(player_id);
    world.insert_resource(cache);

    match next {
        Some(command) => {
            mark_campaign_production_issued(world, player_id, turn, &command);
            CampaignProductionControl::Command(command)
        }
        None if blocks_generic => CampaignProductionControl::BlockGeneric,
        None if generic_budget.is_some_and(|budget| budget > 0) => {
            CampaignProductionControl::ContinueWithSurplus(generic_budget.unwrap_or(0))
        }
        None => CampaignProductionControl::Continue,
    }
}

/// 生産施設から占領可能unitが対象へ自力到達できるかを判定する。
///
/// 「島か大陸か」ではなく実際の移動可能地形で判定するため、島map内の陸続き前線も
/// 単一大陸mapも同じOperation生産へ入り、海峡を越える前線だけ輸送工程へ残る。
fn has_direct_capture_route(
    scan: &BoardScan,
    reach: &mut ReachCtx,
    shortfall: &crate::ai::island_campaign::IslandCampaignShortfall,
) -> bool {
    scan.production_facilities
        .iter()
        .any(|(facility, terrain)| {
            scan.available_types.iter().any(|(unit_type, stats)| {
                stats.can_capture
                    && scan.can_produce(*terrain, *unit_type)
                    && reach.is_reachable(
                        &scan.map,
                        &scan.master_data,
                        (facility.x, facility.y),
                        (shortfall.target_position.x, shortfall.target_position.y),
                        stats.movement_type,
                    )
            })
        })
}

fn mark_campaign_production_issued(
    world: &mut World,
    player_id: PlayerId,
    turn: u32,
    command: &ProduceUnitCommand,
) {
    if let Some(mut registry) =
        world.get_resource_mut::<campaign_execution::V4CampaignExecutionRegistry>()
    {
        registry.mark_issued(player_id, turn, command);
    }
}

/// 盤面から生産判断に必要な観測量をすべて取り出したもの。
struct BoardScan {
    /// 同じ手番の複数作戦へ渡す不変盤面。作戦ごとの全量cloneを避ける。
    map: Arc<Map>,
    master_data: Arc<MasterDataRegistry>,
    damage_chart: Arc<DamageChart>,
    funds: u32,
    /// 生産可能な施設（未占有・生産範囲内・クールダウン対象外）
    free_facilities: Vec<(GridPosition, Terrain)>,
    /// 次ターン以降に空くことを見込める、生産範囲内の全所有施設。
    production_facilities: Vec<(GridPosition, Terrain)>,
    available_types: Vec<(UnitType, UnitStats)>,
    my_units: Vec<UnitSnapshot>,
    enemy_units: Vec<UnitSnapshot>,
    /// 首都の生産範囲内にある所有空港総数（占有中を含む）
    owned_airport_count: u32,
    /// 自軍が保有していない拠点（中立・敵）
    open_properties: Vec<GridPosition>,
    enemy_income: u32,
    enemy_production_slots: u32,
    enemy_facilities: Vec<EnemyFacilitySnapshot>,
    my_income: u32,
    /// 島campaignが実行を決めた作戦。汎用clusterへ事後照合せず、このanchorと
    /// IslandIdをCombat生産計画の入力としてそのまま使う。
    campaign_objectives: Vec<CampaignPlanningObjective>,
    /// 首都攻略部隊を前進させてよいのは、固定兵站経路の確保後だけである。
    capital_assault_authorized: bool,
    /// 編成中の首都攻略部隊を、生産施設から退避させて集結する自軍拠点。
    capital_staging_anchor: Option<GridPosition>,
    /// 敵の実生産履歴から推定した、次手番の生産量。将来脅威の見込み額にだけ使う。
    enemy_production_forecast: EnemyProductionForecastTrace,
}

#[derive(Debug, Clone)]
struct CampaignPlanningObjective {
    island_id: crate::ai::islands::IslandId,
    kind: OperationKind,
    anchor: GridPosition,
    /// 現在同時に進める局地前線。勝利ロードマップの全目標とは分けて保持する。
    objective_properties: Vec<GridPosition>,
    capture_eta: Option<u32>,
    /// anchorの局所clusterではなく、島作戦本体が要求する占領完了時の生存兵数。
    required_capture_survivors: usize,
    /// 固定兵站経路内の工程順。経路外campaignはNone。
    logistics_rank: Option<u32>,
    /// 同じ島の敵を別前線へ分配せず、この戦略作戦へ所属させる。
    forced_target_enemies: HashSet<Entity>,
    /// 施設数だけでなく、実際の占領兵が完了時点まで生存する案を評価する。
    protected_capture_entities: HashSet<Entity>,
    /// 編成中は目的地へ逐次投入せず、ここへ集結させる。
    staging_anchor: GridPosition,
    /// falseなら生産は行うが、作戦地点への攻撃任務はまだ発行しない。
    execution_authorized: bool,
}

/// 同一陸塊の首都攻略本隊を解放できるだけ、勝利ルート上の前線が進んだかを判定する。
/// 後方・側方の補助目標の所有数は、首都へ向かう主線のGo条件には含めない。
fn same_land_capital_front_reached(route_distance: u32, forward_distance: u32) -> bool {
    forward_distance.saturating_mul(2) <= route_distance.max(1)
}

/// 指定した通行可能マスを連結成分へ分け、座標から成分IDを引けるようにする。
fn terrain_component_membership(
    map: &Map,
    cells: &HashSet<GridPosition>,
) -> HashMap<GridPosition, usize> {
    let mut unseen = cells.clone();
    let mut membership = HashMap::new();
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
fn armored_entry_bridge_gates(
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
    let mut bridge_groups: HashMap<usize, Vec<GridPosition>> = HashMap::new();
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
fn movement_step_distances(
    map: &Map,
    master_data: &MasterDataRegistry,
    start: GridPosition,
    movement_type: MovementType,
) -> HashMap<GridPosition, u32> {
    let mut distances = HashMap::new();
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

fn armored_step_distances(
    map: &Map,
    master_data: &MasterDataRegistry,
    start: GridPosition,
) -> HashMap<GridPosition, u32> {
    movement_step_distances(map, master_data, start, MovementType::Tank)
}

/// 橋のない山岳地形でも、両首都の間で独立して続く装甲回廊を最大2本返す。
/// 回廊中央の薄い帯が別連結成分になることを利用し、細かなセル違いは別ルートに数えない。
fn armored_parallel_route_fronts(
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
        let mut groups: HashMap<usize, Vec<GridPosition>> = HashMap::new();
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
fn capital_route_topology_for(
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
fn derive_same_land_route_fronts(
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
fn same_land_capitals(
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

/// 静的な地形・拠点配置から、勢力ごとの前向き首都攻略DAGを一度だけ構築する。
///
/// ノードは両首都を結ぶ最短装甲回廊のセル、辺は自首都からの距離が一つ増える移動だけで
/// 構成する。従って回廊の分岐と合流は表せる一方、後退辺による循環は入り込まない。
fn build_capital_route_topology(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Option<CapitalRouteTopology> {
    let map = world.get_resource::<Map>()?;
    let master_data = world.get_resource::<MasterDataRegistry>()?;
    let island_map = world.get_resource::<crate::ai::islands::IslandMap>()?;
    let (home, enemy_capital) = same_land_capitals(world, player_id, island_id)?;
    let from_home = armored_step_distances(map, master_data, home);
    let from_enemy_capital = armored_step_distances(map, master_data, enemy_capital);
    let total_distance = from_home.get(&enemy_capital).copied()?;
    let mut fronts = derive_same_land_route_fronts(world, player_id, island_id);
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
    let mut property_routes = HashMap::new();
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
    corridor_positions.sort_unstable_by_key(|position| {
        (
            from_home.get(position).copied().unwrap_or(u32::MAX),
            position.y,
            position.x,
        )
    });
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
            progress: from_home.get(position).copied().unwrap_or(u32::MAX),
            anchor: *position,
            successor_nodes: Vec::new(),
        })
        .collect::<Vec<_>>();
    for (position, node_id) in &node_ids {
        if *node_id == goal_node {
            continue;
        }
        let progress = nodes[node_id.0].progress;
        nodes[node_id.0].successor_nodes = map
            .get_adjacent(position.x, position.y)
            .into_iter()
            .map(|(x, y)| GridPosition { x, y })
            .filter_map(|adjacent| node_ids.get(&adjacent).copied())
            .filter(|adjacent_id| nodes[adjacent_id.0].progress == progress.saturating_add(1))
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
        routes,
        property_routes,
        from_home,
    })
}

/// V4の手番開始時に、同一陸塊の首都戦役を一度だけ解析してResourceへ保存する。
pub(crate) fn prepare_capital_route_topologies(world: &mut World, player_id: PlayerId) {
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
        let exists = world
            .get_resource::<CapitalRouteTopologyRegistry>()
            .is_some_and(|registry| registry.topologies.contains_key(&key));
        if !exists && let Some(topology) = build_capital_route_topology(world, player_id, island_id)
        {
            newly_built.push((key, topology));
        }
    }
    if !newly_built.is_empty() {
        let mut registry = world
            .remove_resource::<CapitalRouteTopologyRegistry>()
            .unwrap_or_default();
        registry.topologies.extend(newly_built);
        world.insert_resource(registry);
    }
    ensure_capital_route_node_operations(world, player_id);
}

/// 静的topologyからセルNode Operationの形だけを初期化する。
///
/// 実際の状態は毎手番の盤面観測で上書きするため、ここでは地形由来の前後関係だけを
/// 構築する。既存Operationを保持し、対戦中の再初期化で担当情報を失わない。
fn ensure_capital_route_node_operations(world: &mut World, player_id: PlayerId) {
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
        // Capture Nodeは、回廊から離れすぎた拠点を取り込まない。島全域の占領を
        // 1セルNodeに背負わせず、その地域で意味を持つ周辺拠点だけを出口条件にする。
        let mut capture_targets = vec![Vec::new(); topology.nodes.len()];
        for (position, _) in &properties {
            if *position == topology.nodes[topology.start_node.0].anchor
                || island_map
                    .get_island_at(position)
                    .is_none_or(|island| island.id != key.island_id)
            {
                continue;
            }
            let Some((node, distance)) = topology
                .nodes
                .iter()
                .enumerate()
                .map(|(index, node)| {
                    (
                        index,
                        map.distance(position.x, position.y, node.anchor.x, node.anchor.y),
                    )
                })
                .min_by_key(|(index, distance)| (*distance, *index))
            else {
                continue;
            };
            if distance <= CAPITAL_ROUTE_REGION_RADIUS {
                capture_targets[node].push(*position);
            }
        }
        for targets in &mut capture_targets {
            targets.sort_unstable_by_key(|position| (position.y, position.x));
            targets.dedup();
        }
        let node_anchors = topology
            .nodes
            .iter()
            .map(|node| node.anchor)
            .collect::<Vec<_>>();
        if topology.routes.len() >= 2 {
            consolidate_nearby_capture_target_regions(&map, &node_anchors, &mut capture_targets);
        }
        // 移動用のセルDAGを、そのまま作戦Nodeにはしない。全セルをNodeにすると、1マス
        // ごとに次手番の状態更新を待ち、開けた地形の代替経路まで全て制圧する誤った
        // 前提DAGになる。地形・拠点スキャンで意味のあるMilestoneだけを選ぶ。
        let mut operation_indices = HashSet::from([topology.start_node.0, topology.goal_node.0]);
        for (index, targets) in capture_targets.iter().enumerate() {
            if !targets.is_empty() {
                operation_indices.insert(index);
            }
        }
        for route in &topology.routes {
            if let Some(index) = topology
                .nodes
                .iter()
                .position(|node| node.anchor == route.front)
            {
                operation_indices.insert(index);
            }
        }
        for index in 0..topology.nodes.len() {
            if let Some(exit) =
                capital_route_crossing_exit(&map, &topology, CapitalRouteNodeId(index))
            {
                operation_indices.insert(index);
                if let Some(exit_index) = topology.nodes.iter().position(|node| node.anchor == exit)
                {
                    operation_indices.insert(exit_index);
                }
            }
        }
        let mut operation_indices = operation_indices.into_iter().collect::<Vec<_>>();
        operation_indices.sort_unstable();
        let operation_index_set = operation_indices.iter().copied().collect::<HashSet<_>>();

        // 圧縮後のedgeは、あるMilestoneから次のMilestoneへ到達可能という地理的な
        // 選択肢を表す。途中セルは移動経路へ残すが、作戦の状態遷移を増やさない。
        let mut operation_successors = vec![Vec::new(); topology.nodes.len()];
        for &origin in &operation_indices {
            let mut pending = VecDeque::from(topology.nodes[origin].successor_nodes.clone());
            let mut visited = HashSet::new();
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
            .retain(|id, _| id.topology != key || operation_index_set.contains(&id.node.0));
        for &index in &operation_indices {
            let node = &topology.nodes[index];
            let id = CapitalRouteNodeOperationId {
                topology: key,
                node: CapitalRouteNodeId(index),
            };
            let crossing_exit =
                capital_route_crossing_exit(&map, &topology, CapitalRouteNodeId(index));
            // Nodeの形は毎手番の戦力状況でなく、DAG構築時に地形・拠点・分岐を走査して
            // 決める。単一の通過点/拠点をAreaへ膨らませず、複数目標や合流点だけを
            // 局地戦のAreaとしてSquadへ渡す。
            let scope = if crossing_exit.is_some()
                || (operation_predecessors[index].len() <= 1
                    && operation_successors[index].len() <= 1
                    && capture_targets[index].len() <= 1)
            {
                CapitalRouteNodeScope::Point
            } else {
                CapitalRouteNodeScope::Area
            };
            let mut control_area = capital_route_node_control_area(
                &map,
                &island_map,
                key.island_id,
                node.anchor,
                scope,
                &capture_targets[index],
            );
            if let Some(exit) = crossing_exit
                && !control_area.contains(&exit)
            {
                control_area.push(exit);
            }
            let objective = if capture_targets[index].is_empty() {
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
                        .copied()
                        .map(|node| CapitalRouteNodeOperationId {
                            topology: key,
                            node,
                        })
                        .collect(),
                    successors: operation_successors[index]
                        .iter()
                        .copied()
                        .map(|node| CapitalRouteNodeOperationId {
                            topology: key,
                            node,
                        })
                        .collect(),
                    objective,
                    scope,
                    control_area,
                    capture_targets: capture_targets[index].clone(),
                    crossing_exit,
                    crossed: false,
                    state: victory_roadmap::RoadmapNodeState::Locked,
                    assigned_squads: HashSet::new(),
                });
        }
    }
    world.insert_resource(operations);
}

/// 橋セルからDAGの進行方向へたどり、最初に到達する非橋セルを返す。
///
/// 橋の上に到着した事実と、障害物を越えて敵側へ出た事実を混同しないために使う。
fn capital_route_crossing_exit(
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
    let mut visited = HashSet::new();
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
/// CaptureはPoint/Areaとも、Nodeに対応付けた全対象を所有した`Secured`だけが出口である。
/// `Dominant`は局地戦況の観測値であって占領完了ではなく、後続Nodeの解放には使わない。
fn capital_route_node_exit_satisfied(operation: &CapitalRouteNodeOperation) -> bool {
    match operation.objective {
        CapitalRouteNodeObjective::Control => matches!(
            operation.state,
            victory_roadmap::RoadmapNodeState::Dominant
                | victory_roadmap::RoadmapNodeState::Secured
        ),
        CapitalRouteNodeObjective::Capture => {
            operation.state == victory_roadmap::RoadmapNodeState::Secured
        }
    }
}

/// 圧縮した地理DAGで、Nodeへ到達する代替経路のいずれかが開いたかを返す。
///
/// 同じ地点へ北回り・南回りのedgeが合流する場合、それらは「両方を制圧する」依存では
/// ない。一方の回廊で出口条件を満たせば、後段の同一Milestoneへ進める。
fn capital_route_node_predecessors_released(
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
fn refresh_capital_route_node_operations(
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
        // まず担当情報だけを毎手番リセットする。`crossed`は橋を越えた盤面事実なので
        // 消さず、Node状態は下の観測・dependency計算で再構築する。
        for index in 0..topology.nodes.len() {
            let id = CapitalRouteNodeOperationId {
                topology: key,
                node: CapitalRouteNodeId(index),
            };
            let Some(operation) = operations.operations.get_mut(&id) else {
                continue;
            };
            operation.assigned_squads.clear();
            operation.state = if index == topology.start_node.0 {
                victory_roadmap::RoadmapNodeState::Secured
            } else {
                victory_roadmap::RoadmapNodeState::Locked
            };
        }
        // DAGのedgeは進捗を必ず増やすため、前方への線形走査で前提Nodeの出口条件を
        // 評価できる。地形DAGの合流edgeは「複数経路のいずれから来てもよい」という
        // 代替関係であり、全経路の同時制圧を要求する依存辺ではない。
        for index in 0..topology.nodes.len() {
            let id = CapitalRouteNodeOperationId {
                topology: key,
                node: CapitalRouteNodeId(index),
            };
            if index == topology.start_node.0 {
                continue;
            }
            let Some(operation) = operations.operations.get(&id).cloned() else {
                continue;
            };
            let predecessors_ready =
                capital_route_node_predecessors_released(&operations.operations, &operation);
            if !predecessors_ready {
                continue;
            }

            let mut friendly_power = 0_u64;
            let mut enemy_power = 0_u64;
            let mut friendly_ground_on_exit = false;
            let mut capturer_on_target = false;
            for (owner, position, power, can_capture, is_ground) in &unit_observations {
                if operation.control_area.contains(position) {
                    if *owner == player_id {
                        friendly_power = friendly_power.saturating_add(*power);
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
            if let Some(operation) = operations.operations.get_mut(&id) {
                operation.crossed |= friendly_ground_on_exit;
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
                node: commitment.target_node,
            };
            let Some(operation) = operations.operations.get_mut(&id) else {
                continue;
            };
            operation.assigned_squads.insert(*squad_id);
        }
    }
    world.insert_resource(operations);
}

/// 構築済みDAGを現在の拠点所有へ投影し、到達済みの区間から次に止まる未確保拠点を返す。
///
/// 分岐では複数の候補が同時に現れ、合流後は一つのノードに戻る。橋を通った事実ではなく、
/// その前方にある拠点を確保済みかどうかだけで次区間への遷移を決める。
fn capital_route_dag_frontier_properties(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Option<Vec<Vec<GridPosition>>> {
    let topology = capital_route_topology_for(world, player_id, island_id)?;
    let mut property_owners = HashMap::new();
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
    // DAGは自首都からの距離順に構築済みであり、successorは必ず次の距離帯にある。
    for (index, node) in topology.nodes.iter().enumerate() {
        if !reachable[index] {
            continue;
        }
        if property_owners.get(&node.anchor).copied() != Some(Some(player_id))
            && let Some(route) = topology.property_routes.get(&node.anchor)
        {
            route_properties[route.0].push(node.anchor);
            continue;
        }
        for successor in &node.successor_nodes {
            reachable[successor.0] = true;
        }
    }
    Some(route_properties)
}

/// 首都作戦を実行へ移してよいかを、DAG上の前方区間から判定する。
///
/// 単に「自軍所有のどれかの都市が中間地点を越えたか」では、側方の都市やDAG外の
/// 施設がGo条件に混ざる。各routeで自首都から連続して確保できた先の、最初の未確保
/// 拠点を使うことで、進軍・生産・Go判定を同じ首都戦役の状態から導く。
fn capital_route_assault_authorized_from_dag(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
) -> Option<bool> {
    let topology = capital_route_topology_for(world, player_id, island_id)?;
    let total_progress = topology.nodes[topology.goal_node.0].progress.max(1);
    let frontiers = capital_route_dag_frontier_properties(world, player_id, island_id)?;
    Some(frontiers.iter().flatten().any(|frontier| {
        topology
            .nodes
            .iter()
            .find(|node| node.anchor == *frontier)
            .is_some_and(|node| node.progress.saturating_mul(2) >= total_progress)
    }))
}

/// 指定目標へ到達できるDAG内の一経路を選び、分岐を通す実在ノードを返す。
///
/// 始点直後の複数枝は当手番の戦力需要が高い方を選び、合流以降は同じ後続ノード列を使う。
/// ここで選んだウェイポイントは移動側へ渡し、経路探索が別枝へ逸れるのを防ぐ。
fn capital_route_dag_nodes_for_target(
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
    // 前手番の盤面・Squadを先にNode状態へ投影し、敵がいる中間Control Nodeを
    // 先頭未確保施設より優先する。DAGを単なる遠方waypoint列として扱わない。
    refresh_capital_route_node_operations(world, player_id, manager);
    let existing = world
        .get_resource::<CapitalRoutePathRegistry>()
        .cloned()
        .unwrap_or_default();
    let Some(island_map) = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
    else {
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

    #[derive(Debug, Clone)]
    struct RouteSquad {
        id: SquadId,
        mission: MissionType,
        target: Option<GridPosition>,
        forward_progress: u32,
        ground_member_count: usize,
        capture_member_count: usize,
        frontline_control_value: u64,
        members: BTreeSet<Entity>,
    }

    let mut squads_by_island = HashMap::<crate::ai::islands::IslandId, Vec<RouteSquad>>::new();
    for squad in manager.squads.iter().filter(|squad| {
        squad.owner_id == Some(player_id) && squad.mission_type != MissionType::Transport
    }) {
        let mut island_id = None;
        let mut ground_member_count = 0_usize;
        let mut capture_member_count = 0_usize;
        let mut frontline_control_value = 0_u64;
        let mut crosses_island_boundary = false;
        for entity in &squad.members {
            let Some(faction) = world.get::<Faction>(*entity) else {
                continue;
            };
            let Some(position) = world.get::<GridPosition>(*entity) else {
                continue;
            };
            let Some(stats) = world.get::<UnitStats>(*entity) else {
                continue;
            };
            if faction.0 != player_id
                || world.get::<Transporting>(*entity).is_some()
                || matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
            {
                continue;
            }
            let Some(member_island) = island_map.get_island_at(position).map(|island| island.id)
            else {
                continue;
            };
            if !topology_islands.contains(&member_island) {
                continue;
            }
            if island_id.is_some_and(|current| current != member_island) {
                crosses_island_boundary = true;
                break;
            }
            island_id = Some(member_island);
            ground_member_count = ground_member_count.saturating_add(1);
            if stats.can_capture {
                capture_member_count = capture_member_count.saturating_add(1);
            }
            // 兵種名には依存せず、前線で損害を受け止められる現在価値を護衛選定に使う。
            // 間接兵器より直接交戦できる高価・健全な部隊を同じ前進度で先に割り当てる。
            let health_ratio = world
                .get::<Health>(*entity)
                .map(|health| {
                    u64::from(health.current).saturating_mul(100) / u64::from(health.max.max(1))
                })
                .unwrap_or(100);
            let direct_fire_weight = if stats.min_range <= 1 { 2_u64 } else { 1_u64 };
            frontline_control_value = frontline_control_value.saturating_add(
                u64::from(stats.cost)
                    .saturating_mul(health_ratio)
                    .saturating_mul(direct_fire_weight),
            );
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
            squads_by_island
                .entry(island_id)
                .or_default()
                .push(RouteSquad {
                    id: squad.id,
                    mission: squad.mission_type.clone(),
                    target: squad.target,
                    forward_progress,
                    ground_member_count,
                    capture_member_count,
                    frontline_control_value,
                    members: squad.members.clone(),
                });
        }
    }

    let mut commitments = HashMap::new();
    let mut assignment_diagnostics = HashMap::new();
    // Area Capture Nodeでは、Capture Squadの出発許可を島全体の曖昧な戦力数ではなく
    // 同じNodeへ割り当てたControl Squadの存在で決める。Capture自身を護衛扱いに
    // して単独前進を許可することはない。
    let mut capture_squads_by_operation =
        HashMap::<CapitalRouteNodeOperationId, Vec<SquadId>>::new();
    let mut control_squads_by_operation =
        HashMap::<CapitalRouteNodeOperationId, HashSet<SquadId>>::new();
    for (island_id, mut squads) in squads_by_island {
        let Some(topology) = capital_route_topology_for(world, player_id, island_id) else {
            continue;
        };
        // 前衛枠は任務名や生成順ではなく、現在のDAG前進度で毎ターン交代する。
        // Capture/Controlの役割枠は後段で別々に数えるため、ここでは到達済みの予備を
        // 後方の旧Squadより先にAdvanceへ昇格させる。
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

        // 各枝の先頭未確保拠点を区間目標にする。橋を通っただけでは進行させず、
        // 拠点の確保により次の区間へ進む。
        let frontiers = capital_route_dag_frontier_properties(world, player_id, island_id)
            .unwrap_or_else(|| vec![Vec::new(); topology.routes.len()]);
        let targets = frontiers
            .iter()
            .map(|properties| properties.first().copied())
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
        let mut assigned_counts = vec![0_usize; topology.routes.len()];
        let mut committed_counts = vec![0_usize; topology.routes.len()];
        let mut path_unavailable_counts = vec![0_usize; topology.routes.len()];
        let mut assigned_routes = HashMap::<SquadId, usize>::new();
        // 同じ未完了Nodeへ送った地上member数と、後方待機線の順序をSquad単位で持つ。
        // NodeはEntityの所属を決めず、ここで決めたSquad指令だけが流量を制御する。
        let mut frontline_members = HashMap::<(CapitalRouteNodeOperationId, bool), usize>::new();
        let mut staged_squads = HashMap::<CapitalRouteNodeOperationId, usize>::new();
        // Dominantで主力を解放したCapture Areaごとに、占領完了まで残すControl分隊は
        // 一個だけにする。全軍を足止めせず、歩兵だけを残す状況も避ける。
        let mut retained_capture_escorts = HashSet::<CapitalRouteNodeOperationId>::new();
        let inherited_commitments = squads
            .iter()
            .filter_map(|squad| {
                inherited_capital_route_commitment(&existing, squad.id, &squad.members)
                    .cloned()
                    .map(|commitment| (squad.id, commitment))
            })
            .collect::<HashMap<_, _>>();

        // 前手番のSquad orderをまず保持する。Squadのmember数が変化しても、同じ
        // Nodeを実行する限り分岐を変えないため、回復・補充で戦略目標が揺れない。
        for squad in &squads {
            let Some(commitment) = inherited_commitments.get(&squad.id) else {
                continue;
            };
            let route = commitment.route.0;
            if commitment.player_id != player_id
                || commitment.island_id != island_id
                || targets.get(route).copied().flatten() != Some(commitment.target)
            {
                continue;
            }
            assigned_counts[route] =
                assigned_counts[route].saturating_add(squad.ground_member_count);
            assigned_routes.insert(squad.id, route);
        }

        // 個体ごとの限界価値を再計算せず、Squad全体を一つのrouteへ置く。これにより
        // 同じSquadのCombat役とCapture役が別枝へ分離しない。
        for squad in &squads {
            if assigned_routes.contains_key(&squad.id) {
                continue;
            }
            let preferred_target_route = if squad.mission == MissionType::Capture {
                squad
                    .target
                    .and_then(|target| topology.property_routes.get(&target).map(|r| r.0))
                    .filter(|route| targets.get(*route).is_some_and(|t| t.is_some()))
            } else {
                None
            };
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
            assigned_counts[route] =
                assigned_counts[route].saturating_add(squad.ground_member_count);
            assigned_routes.insert(squad.id, route);
        }

        // 各進軍枝で、未完了Capture Areaへ残す最良のControl護衛を先に指名する。
        // 単にloopで最初の候補を取ると、少し先行したReconが戦車より先に護衛枠を
        // 消費する。残存HP・価格・直接交戦能力を主基準、前進度を副基準にする。
        let preferred_capture_escort_by_route = (0..topology.routes.len())
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
            .collect::<Vec<_>>();

        for squad in &squads {
            let Some(route) = assigned_routes.get(&squad.id).copied() else {
                continue;
            };
            let Some(target) = targets[route] else {
                continue;
            };
            let Some(path_nodes) = capital_route_dag_nodes_for_target(
                world,
                player_id,
                island_id,
                CapitalRouteId(route),
                target,
            ) else {
                path_unavailable_counts[route] = path_unavailable_counts[route].saturating_add(1);
                continue;
            };
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
                path_unavailable_counts[route] = path_unavailable_counts[route].saturating_add(1);
                continue;
            }
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
                                .is_none_or(|operation| {
                                    !capital_route_node_exit_satisfied(operation)
                                })
                        })
                        .unwrap_or(target_node);
                    capital_route_active_node_for_squad(
                        &operation_path_nodes,
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
                            &operation_path_nodes,
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
            let node_operation = world
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
                });
            let Some(node_operation) = node_operation else {
                path_unavailable_counts[route] = path_unavailable_counts[route].saturating_add(1);
                continue;
            };
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
                continue;
            }
            let is_control_squad = capital_route_controls_active_node(
                &squad.mission,
                target_is_on_or_beyond_active_node,
            );
            let requested_phase = if is_capture_squad || is_control_squad {
                CapitalRouteExecutionPhase::Advance
            } else {
                capital_route_execution_phase(&squad.mission)
            };
            let directive_target = squad.target.filter(|target| path.contains(target));
            let forward_target = match requested_phase {
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
            };
            let operation_id = node_operation.id;
            let frontline_capacity = capital_route_node_frontline_capacity(&node_operation);
            let assigned_capture_members = frontline_members
                .get(&(operation_id, true))
                .copied()
                .unwrap_or(0);
            let uses_capture_lane = capital_route_uses_capture_lane(
                &node_operation,
                &squad.mission,
                squad.capture_member_count,
                assigned_capture_members,
            );
            let role_capacity =
                capital_route_role_frontline_capacity(&node_operation, uses_capture_lane);
            let (phase, execution_target) =
                if requested_phase != CapitalRouteExecutionPhase::Advance {
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
                        let target = capital_route_staging_target(
                            world,
                            player_id,
                            &path,
                            forward_target,
                            *stage_index,
                        );
                        *stage_index = stage_index.saturating_add(1);
                        (CapitalRouteExecutionPhase::Stage, target)
                    }
                };
            if is_capture_squad
                && node_operation.objective == CapitalRouteNodeObjective::Capture
                && phase == CapitalRouteExecutionPhase::Advance
            {
                capture_squads_by_operation
                    .entry(operation_id)
                    .or_default()
                    .push(squad.id);
            }
            if is_control_squad
                && !uses_capture_lane
                && phase == CapitalRouteExecutionPhase::Advance
            {
                control_squads_by_operation
                    .entry(operation_id)
                    .or_default()
                    .insert(squad.id);
            }
            committed_counts[route] =
                committed_counts[route].saturating_add(squad.ground_member_count);
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
        assignment_diagnostics.insert(
            CapitalRouteTopologyKey {
                player_id,
                island_id,
            },
            CapitalRouteAssignmentDiagnostics {
                eligible_ground_units: ground_member_count,
                route_quotas: quotas,
                committed_by_route: committed_counts,
                path_unavailable_by_route: path_unavailable_counts,
            },
        );
    }
    let enemy_positions = world
        .iter_entities()
        .filter_map(|entity| {
            let faction = entity.get::<Faction>()?;
            let position = entity.get::<GridPosition>()?;
            (faction.0 != player_id).then_some(*position)
        })
        .collect::<Vec<_>>();
    for (operation_id, capture_squads) in capture_squads_by_operation {
        let has_control_squad = control_squads_by_operation
            .get(&operation_id)
            .is_some_and(|squads| !squads.is_empty());
        for squad_id in capture_squads {
            let Some(commitment) = commitments.get(&squad_id) else {
                continue;
            };
            let local_enemy_present = enemy_positions
                .iter()
                .any(|position| commitment.control_area.contains(position));
            // 敵がいないAreaはControl Squadの到着待ちで占領を止めない。敵がいるAreaは
            // 同じNodeへControl Squadを割り当てた場合だけCapture Squadを発進させる。
            let departure_authorized = !local_enemy_present || has_control_squad;
            if let Some(squad) = manager
                .squads
                .iter_mut()
                .find(|squad| squad.id == squad_id && squad.owner_id == Some(player_id))
            {
                squad.departure_authorized = departure_authorized;
                if departure_authorized && squad.phase == crate::ai::squad::MissionPhase::Forming {
                    squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
                }
            }
        }
    }
    let mut registry = world
        .remove_resource::<CapitalRoutePathRegistry>()
        .unwrap_or_default();
    registry
        .commitments
        .retain(|_, commitment| commitment.player_id != player_id);
    registry.commitments.extend(commitments);
    let committed_squad_ids = registry.commitments.keys().copied().collect::<HashSet<_>>();
    registry
        .commitment_members
        .retain(|squad_id, _| committed_squad_ids.contains(squad_id));
    registry.commitment_members.extend(
        manager
            .squads
            .iter()
            .filter(|squad| committed_squad_ids.contains(&squad.id))
            .map(|squad| (squad.id, squad.members.clone())),
    );
    registry
        .assignment_diagnostics
        .retain(|key, _| key.player_id != player_id);
    registry
        .assignment_diagnostics
        .extend(assignment_diagnostics);
    world.insert_resource(registry);
    refresh_capital_route_node_operations(world, player_id, manager);
}

/// Entityが現在の地域Nodeへ入ったかを返す。
///
/// 橋Nodeは出口を越えるまで地域到達扱いにしない。橋上または手前にいるだけで
/// 戦術器の自由行動へ切り替えると、唯一の通路を越えずに作戦が止まるためである。
fn capital_route_region_reached(
    commitment: &CapitalRouteCommitment,
    position: GridPosition,
) -> bool {
    let reached_scope = match commitment.scope {
        // Point Nodeはanchor、橋だけは向こう岸の出口そのものへ到達して初めて到達扱いにする。
        CapitalRouteNodeScope::Point => position == commitment.execution_target,
        CapitalRouteNodeScope::Area => commitment.control_area.contains(&position),
    };
    reached_scope && (commitment.crossing_exit.is_none() || commitment.crossed)
}

/// Areaへ到達した後の移動候補を、Roadmapが指示した局地戦領域へ限定する。
///
/// `None` はDAG外、到達前、保持・補給、回復中のいずれかであり、呼び出し側の通常の
/// 候補集合を使う。`Some` の場合だけ、Control/Captureの戦術器が同じNode内で位置を
/// 選ぶ。これによりanchorへの一点直行を止めても、古いSquad目標へ逸脱しない。
pub(crate) fn capital_route_allows_tactical_position(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    position: GridPosition,
) -> Option<bool> {
    if capital_route_is_recovering(world, player_id, entity) {
        return None;
    }
    let commitment = capital_route_commitment_for_entity(world, player_id, entity)?;
    if commitment.phase != CapitalRouteExecutionPhase::Advance {
        return None;
    }
    let current = world.get::<GridPosition>(entity).copied()?;
    if !capital_route_region_reached(commitment, current) {
        return None;
    }
    let inside_control_area = commitment.control_area.contains(&position);
    let can_capture = world
        .get::<UnitStats>(entity)
        .is_some_and(|stats| stats.can_capture);
    let reserved_for_capturer = !can_capture
        && world
            .get_resource::<CapitalRouteNodeOperationRegistry>()
            .is_some_and(|operations| {
                capital_route_capture_target_is_reserved(operations, player_id, position)
            });
    Some(inside_control_area && !reserved_for_capturer)
}

/// 行動決定側へ、現在の地域作戦の戦術的な目標を返す。
///
/// 外側では回廊の入口／橋向こうの出口へ進める。地域内のControl Nodeはanchorへの
/// 引力を外して局地戦術へ委ね、Capture Nodeだけが未確保の周辺拠点を選ぶ。
/// `Some(None)` は「DAG所属だが一点目標を持たない地域Control中」を意味する。
pub(crate) fn capital_route_tactical_target(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
) -> Option<Option<GridPosition>> {
    if capital_route_is_recovering(world, player_id, entity) {
        return Some(None);
    }
    let commitment = capital_route_commitment_for_entity(world, player_id, entity)?;
    // 保持・補給は局地戦の自由移動へ切り替えず、それぞれの防衛地点・配送地点を
    // 維持する。Point/Areaの分類は前進Squadの到達後の戦術だけに適用する。
    if commitment.phase != CapitalRouteExecutionPhase::Advance {
        return Some(Some(commitment.execution_target));
    }
    let position = world.get::<GridPosition>(entity).copied()?;
    if !capital_route_region_reached(commitment, position) {
        return Some(Some(commitment.execution_target));
    }
    if commitment.objective != CapitalRouteNodeObjective::Capture {
        return Some(None);
    }
    let map = world.get_resource::<Map>()?;
    let owners = world
        .iter_entities()
        .filter_map(|property| {
            Some((
                *property.get::<GridPosition>()?,
                property.get::<Property>()?.owner_id,
            ))
        })
        .collect::<HashMap<_, _>>();
    Some(
        commitment
            .capture_targets
            .iter()
            .filter(|target| owners.get(target) != Some(&Some(player_id)))
            .min_by_key(|target| {
                (
                    map.distance(position.x, position.y, target.x, target.y),
                    target.y,
                    target.x,
                )
            })
            .copied(),
    )
}

/// 既存の戦術器向けに、地点を持つ場合だけを返す互換窓口。
#[cfg(test)]
pub(crate) fn capital_route_waypoint(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
) -> Option<GridPosition> {
    capital_route_tactical_target(world, player_id, entity).flatten()
}

/// DAG所属を保ったまま、回復だけは一時的に通常の修理経路へ委ねるかを返す。
///
/// Recover Entity に古いSquad目標を与えると、修理拠点ではなく横の占領目標へ戻って
/// しまう。経路そのものはRegistryに残し、行動選択だけを回復優先へ切り替える。
pub(crate) fn capital_route_is_recovering(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
) -> bool {
    // HP低下はmember個別の戦術例外であり、SquadのRoadmap orderは更新しない。
    // 回復が終われば同じSquad orderへ自然に復帰する。
    capital_route_commitment_for_entity(world, player_id, entity).is_some()
        && world
            .get::<Health>(entity)
            .is_some_and(|health| health.current < 70)
}

/// DAG所属Entityが指定地点を占領してよいかを返す。
///
/// Capture Nodeでは周辺拠点群だけを許可し、Control Nodeでは地域優勢を作る前に
/// 横の拠点へ目的をすり替えない。DAG外のEntityには従来どおり制約を掛けない。
pub(crate) fn capital_route_allows_capture_at(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    position: GridPosition,
) -> bool {
    let Some(commitment) = capital_route_commitment_for_entity(world, player_id, entity) else {
        return true;
    };
    world
        .get::<UnitStats>(entity)
        .is_some_and(|stats| stats.can_capture)
        // 待機線の歩兵が後方の施設を勝手に占領して、前衛Nodeの完了条件を崩さない。
        && commitment.phase != CapitalRouteExecutionPhase::Stage
        && commitment.objective == CapitalRouteNodeObjective::Capture
        && commitment.capture_targets.contains(&position)
}

/// 実行用のセル列から、この手番に到達できる最も前進した位置を返す。
///
/// 座標の直線距離ではなく、DAGセル列に沿った区間目標までの残り移動コストで比較する。
/// これにより山を挟むmap_26でも、橋へ近いが山で隔てられたセルではなく、迂回路上の
/// 次の到達セルを選ぶ。
pub(crate) fn capital_route_advance_destination(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    reachable: &BTreeSet<(usize, usize)>,
) -> Option<GridPosition> {
    if capital_route_is_recovering(world, player_id, entity) {
        return None;
    }
    let commitment = capital_route_commitment_for_entity(world, player_id, entity)?;
    let position = world.get::<GridPosition>(entity).copied()?;
    let stats = world.get::<UnitStats>(entity)?;
    let map = world.get_resource::<Map>()?;
    let master_data = world.get_resource::<MasterDataRegistry>()?;
    let topology = capital_route_topology_for(world, player_id, commitment.island_id)?;
    // 待機線は前衛の局地戦へ渡さず、指定された後方セルで行動終了する。
    // `Wait` はMoveUnitCommandを伴うため、工場上の新造unitもこのセルまで退避できる。
    if commitment.phase == CapitalRouteExecutionPhase::Stage {
        if position == commitment.execution_target {
            return reachable
                .contains(&(position.x, position.y))
                .then_some(position);
        }
        let current_progress = topology.from_home.get(&position).copied().unwrap_or(0);
        let stage_index = commitment
            .path
            .iter()
            .rposition(|cell| *cell == commitment.execution_target)?;
        return commitment
            .path
            .iter()
            .enumerate()
            .filter(|(index, _)| *index <= stage_index)
            .filter(|(_, cell)| reachable.contains(&(cell.x, cell.y)))
            .filter(|(_, cell)| {
                topology
                    .from_home
                    .get(cell)
                    .is_some_and(|progress| *progress >= current_progress)
            })
            .max_by_key(|(index, cell)| (*index, cell.y, cell.x))
            .map(|(_, cell)| *cell)
            .or_else(|| {
                reachable
                    .contains(&(position.x, position.y))
                    .then_some(position)
            });
    }
    if capital_route_region_reached(commitment, position) {
        // 地域内ではanchorへ直行させず、Controlなら局地戦術、Captureなら周辺拠点への
        // 接近スコアへ制御を渡す。
        return None;
    }
    let current_progress = topology.from_home.get(&position).copied().unwrap_or(0);
    let end_index = commitment
        .path
        .iter()
        .rposition(|cell| *cell == commitment.execution_target)?;
    // 戦闘・支援unitは「実際に占領すべき施設」だけを占領役へ空ける。Control Pointの
    // anchorや橋出口は通過・占有して初めて優勢を観測できるため、一歩手前へ止めない。
    // 既にCapture対象を占有している場合だけは、後方の到達可能セルへ戻して退避する。
    let requires_crossing = commitment.crossing_exit.is_some() && !commitment.crossed;
    let reserves_capture_target = commitment.objective == CapitalRouteNodeObjective::Capture
        && commitment
            .capture_targets
            .contains(&commitment.execution_target);
    let advance_end_index = if commitment.phase == CapitalRouteExecutionPhase::Advance
        && !stats.can_capture
        && reserves_capture_target
        && !requires_crossing
    {
        end_index.saturating_sub(1)
    } else {
        end_index
    };
    if commitment.phase == CapitalRouteExecutionPhase::Advance
        && !stats.can_capture
        && reserves_capture_target
        && !requires_crossing
        && position == commitment.execution_target
    {
        return commitment
            .path
            .iter()
            .take(end_index)
            .rev()
            .find(|cell| reachable.contains(&(cell.x, cell.y)))
            .copied();
    }
    let current_index = commitment
        .path
        .iter()
        .enumerate()
        .filter(|(_, cell)| {
            topology
                .from_home
                .get(cell)
                .is_some_and(|progress| *progress <= current_progress)
        })
        .map(|(index, _)| index)
        .max()
        .unwrap_or(0)
        .min(advance_end_index);

    commitment
        .path
        .iter()
        .enumerate()
        .filter(|(index, _)| *index > current_index)
        .filter(|(index, _)| *index <= advance_end_index)
        .filter(|(_, cell)| reachable.contains(&(cell.x, cell.y)))
        .filter_map(|(index, cell)| {
            let remaining_cost = commitment
                .path
                .iter()
                .skip(index.saturating_add(1))
                .take(advance_end_index.saturating_sub(index))
                .try_fold(0_u32, |total, next| {
                    let terrain = map.get_terrain(next.x, next.y)?;
                    let step_cost =
                        get_valid_movement_cost(master_data, stats.movement_type, terrain)?;
                    Some(total.saturating_add(step_cost))
                })?;
            Some((*cell, remaining_cost, index))
        })
        .min_by_key(|(cell, remaining_cost, index)| {
            (*remaining_cost, std::cmp::Reverse(*index), cell.y, cell.x)
        })
        .map(|(cell, _, _)| cell)
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
                .filter(|squad| squad.owner_id == Some(player_id))
                .flat_map(|squad| {
                    squad.members.iter().map(move |entity| {
                        (
                            *entity,
                            (
                                squad.id,
                                squad.target_island,
                                squad.target,
                                format!("{:?}", squad.mission_type),
                                format!("{:?}", squad.phase),
                                squad.departure_authorized,
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

/// 各進軍軸へ割り当てる余剰戦力の比率を返す。
///
/// 全軸に最低1体を置いた残りは、敵首都へ短く到達できる主攻軸へ寄せる。
/// ただし実際に敵地上部隊が多い軸には同じだけ加点し、橋頭堡を取った後の
/// 増援が初期の二分だけで止まらないようにする。
/// 同一陸塊の進軍軸が追加戦力を必要とする量。
///
/// これは「この1体を足した場合」を軸ごと・unitごとに再シミュレーションした値ではない。
/// 盤面を一巡して、奪取余地・生産基盤・敵味方の現有価値・首都への地形ETAを同じ
/// 通貨単位へ集約した、当手番の戦役全体の需要ベクトルである。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RouteForceDemand {
    /// 奪取後に得られる収益と、生産施設を無力化/確保する機会価値。
    pub opportunity_value: u64,
    /// 既にその軸へ置かれた戦力では埋められない敵戦力の価値。
    pub force_deficit_value: u64,
    /// 戦力枠を配るときに使う総需要。ゼロの軸へ戦力を強制しない。
    pub demand_value: u64,
}

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
fn same_land_route_entry_ready(
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
                // 生産したunitは次手番から行動するため、実際の到着予定へ1手番を加える。
                .map(|turns| turns.saturating_add(1));
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
    let mut observed = HashSet::new();
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
            crossed: HashMap::from([(player_id, observed)]),
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
        let route_properties =
            capital_route_dag_frontier_properties(world, player_id, assignment.island_id)
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
        let additional_route_slots =
            apportion_route_force_slots(additional_milestone_slots, &route_demands);
        let route_slots = route_properties
            .iter()
            .enumerate()
            .map(|(route, properties)| {
                usize::from(!properties.is_empty())
                    .saturating_add(additional_route_slots.get(route).copied().unwrap_or(0))
            })
            .collect::<Vec<_>>();
        let mut milestones = Vec::new();
        let mut route_milestones = Vec::new();
        for (route, mut properties) in route_properties.into_iter().enumerate() {
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
            assignment.capture_target_positions = milestones;
        }
    }
}

/// 橋で分岐する同一陸塊の局地前線を、Gateごとの独立Operationへ分ける。
/// 必要占領兵と既存所属は一意に配分し、同じEntityを複数ルートへ二重計上しない。
fn split_bridge_route_objectives(
    map: &Map,
    objectives: Vec<CampaignPlanningObjective>,
    gates: &[GridPosition],
    my_units: &[UnitSnapshot],
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
                forced_target_enemies: HashSet::new(),
                protected_capture_entities,
                staging_anchor: anchor,
                execution_authorized: objective.execution_authorized,
            });
        }
    }
    split
}

impl BoardScan {
    fn collect(world: &mut World, player_id: PlayerId) -> Option<Self> {
        let map = Arc::new(world.get_resource::<Map>()?.clone());
        let unit_registry = world.get_resource::<UnitRegistry>()?.clone();
        let damage_chart = Arc::new(world.get_resource::<DamageChart>()?.clone());
        let master_data = Arc::new(world.get_resource::<MasterDataRegistry>()?.clone());
        let funds = world
            .get_resource::<Players>()?
            .0
            .iter()
            .find(|p| p.id == player_id)
            .map(|p| p.funds)?;
        let logistics_plan = world
            .get_resource::<logistics_plan::V4LogisticsPlanRegistry>()
            .and_then(|registry| registry.plan(player_id))
            .cloned();
        let logistics_ranks = logistics_plan
            .as_ref()
            .map(|plan| {
                plan.route_islands
                    .iter()
                    .enumerate()
                    .map(|(rank, island)| (*island, u32::try_from(rank).unwrap_or(u32::MAX)))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let mut campaign_objectives: Vec<CampaignPlanningObjective> = world
            .get_resource::<crate::ai::engine::AiTurnStrategyCache>()
            .and_then(|cache| cache.campaign_portfolio(player_id))
            .map(|portfolio| {
                portfolio
                    .defenses
                    .iter()
                    .chain(portfolio.active_offensives.iter())
                    .map(|assignment| {
                        let eta = portfolio
                            .islands
                            .iter()
                            .find(|assessment| assessment.island_id == assignment.island_id)
                            .and_then(|assessment| assessment.friendly_capture_eta);
                        CampaignPlanningObjective {
                            island_id: assignment.island_id,
                            kind: if assignment.decision
                                == crate::ai::island_campaign::IslandCampaignDecision::Defend
                            {
                                OperationKind::Defense
                            } else {
                                OperationKind::Capture
                            },
                            anchor: assignment.target_position,
                            objective_properties: assignment.capture_target_positions.clone(),
                            capture_eta: eta,
                            required_capture_survivors: usize::try_from(
                                assignment.requirement.capture_units,
                            )
                            .unwrap_or(usize::MAX),
                            logistics_rank: logistics_ranks.get(&assignment.island_id).copied(),
                            forced_target_enemies: HashSet::new(),
                            protected_capture_entities: assignment
                                .capture_entities
                                .iter()
                                .copied()
                                .collect(),
                            staging_anchor: assignment.target_position,
                            execution_authorized: true,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // 同一ターン内で生産に失敗した施設を除外するためのクールダウン
        let cooldown: HashSet<(usize, usize)> = world
            .get_resource::<crate::ai::engine::AiProductionCooldown>()
            .map(|c| c.0.clone())
            .unwrap_or_default();

        // --- ユニットの走査 ---
        let mut occupied = HashSet::new();
        let mut my_units = Vec::new();
        let mut enemy_units = Vec::new();
        {
            let mut q = world.query::<(
                Entity,
                &GridPosition,
                &Faction,
                &UnitStats,
                Option<&Health>,
                Option<&CargoCapacity>,
                Option<&Transporting>,
            )>();
            for (entity, pos, faction, stats, health, cargo, transporting) in q.iter(world) {
                // 輸送中のユニットは盤面を占有しない
                if transporting.is_some() {
                    continue;
                }
                occupied.insert(*pos);
                let snapshot = UnitSnapshot {
                    entity: Some(entity),
                    pos: *pos,
                    hp: health.map_or(100, |h| h.current),
                    free_cargo: cargo.map_or(stats.max_cargo, |c| {
                        stats.max_cargo.saturating_sub(c.loaded.len() as u32)
                    }),
                    stats: stats.clone(),
                };
                if faction.0 == player_id {
                    my_units.push(snapshot);
                } else {
                    enemy_units.push(snapshot);
                }
            }
        }

        // --- 拠点の走査 ---
        let mut capital_pos = None;
        let mut open_properties = Vec::new();
        let mut facilities = Vec::new();
        let mut production_facilities = Vec::new();
        let mut enemy_income = 0u32;
        let mut enemy_production_slots = 0u32;
        let mut enemy_facilities = Vec::new();
        let mut my_income = 0u32;
        let mut owned_airport_count = 0u32;
        let mut owned_properties = Vec::new();
        let mut enemy_capital = world
            .get_resource::<victory_roadmap::VictoryRoadmapRegistry>()
            .and_then(|registry| registry.roadmap(player_id))
            .and_then(|roadmap| roadmap.enemy_capital);
        {
            let mut q = world.query::<(&GridPosition, &Property)>();
            let mut enemy_capitals = HashMap::new();
            for (pos, prop) in q.iter(world) {
                if prop.owner_id == Some(player_id) && prop.terrain == Terrain::Capital {
                    capital_pos = Some(*pos);
                } else if let Some(owner) = prop.owner_id
                    && prop.terrain == Terrain::Capital
                {
                    enemy_capitals.insert(owner, *pos);
                    enemy_capital.get_or_insert(*pos);
                }
            }
            for (pos, prop) in q.iter(world) {
                let income = master_data.landscape_income(prop.terrain.as_str());
                let is_facility = master_data.is_production_facility(prop.terrain.as_str());
                match prop.owner_id {
                    Some(owner) if owner == player_id => {
                        my_income = my_income.saturating_add(income);
                        owned_properties.push((*pos, prop.terrain));
                        if prop.terrain == Terrain::Airport
                            && crate::systems::production::is_within_production_range(
                                capital_pos.as_slice(),
                                pos.x,
                                pos.y,
                                map.topology,
                            )
                        {
                            owned_airport_count = owned_airport_count.saturating_add(1);
                        }
                        let is_in_production_range = is_facility
                            && crate::systems::production::is_within_production_range(
                                capital_pos.as_slice(),
                                pos.x,
                                pos.y,
                                map.topology,
                            );
                        if is_in_production_range {
                            production_facilities.push((*pos, prop.terrain));
                        }
                        // 現在手番に命令できるのは、全生産施設のうち空いているものだけ。
                        if is_in_production_range
                            && !occupied.contains(pos)
                            && !cooldown.contains(&(pos.x, pos.y))
                        {
                            facilities.push((*pos, prop.terrain));
                        }
                    }
                    Some(owner) => {
                        enemy_income = enemy_income.saturating_add(income);
                        let is_usable_enemy_facility = is_facility
                            && enemy_capitals.get(&owner).is_some_and(|capital| {
                                crate::systems::production::is_within_production_range(
                                    std::slice::from_ref(capital),
                                    pos.x,
                                    pos.y,
                                    map.topology,
                                )
                            });
                        if is_usable_enemy_facility {
                            enemy_production_slots += 1;
                            enemy_facilities.push(EnemyFacilitySnapshot {
                                pos: *pos,
                                terrain: prop.terrain,
                            });
                        }
                        open_properties.push(*pos);
                    }
                    None => open_properties.push(*pos),
                }
            }
        }

        if facilities.is_empty() {
            return None;
        }

        let available_types: Vec<(UnitType, UnitStats)> = unit_registry
            .0
            .iter()
            .map(|(unit_type, stats)| (*unit_type, stats.clone()))
            .collect();

        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let enemy_capital_island = enemy_capital
            .and_then(|position| island_map.get_island_at(&position))
            .map(|island| island.id);
        let home_island = capital_pos
            .and_then(|position| island_map.get_island_at(&position))
            .map(|island| island.id);
        if home_island.is_some()
            && home_island == enemy_capital_island
            && let (Some(home), Some(capital)) = (capital_pos, enemy_capital)
        {
            let gates = armored_entry_bridge_gates(&map, &master_data, home, capital);
            campaign_objectives =
                split_bridge_route_objectives(&map, campaign_objectives, &gates, &my_units);
        }
        let completed_route_island = logistics_plan.as_ref().and_then(|plan| {
            plan.route_islands
                .iter()
                .rev()
                .copied()
                .find(|island| !plan.selected_islands.contains(island))
        });
        let property_positions = owned_properties
            .iter()
            .map(|(position, _)| *position)
            .chain(open_properties.iter().copied())
            .collect::<HashSet<_>>();
        let staging_anchor = enemy_capital.and_then(|capital| {
            let forward_property = owned_properties
                .iter()
                .filter(|(position, _)| {
                    completed_route_island.is_none_or(|expected| {
                        island_map
                            .get_island_at(position)
                            .is_some_and(|island| island.id == expected)
                    })
                })
                .min_by_key(|(position, _)| {
                    map.distance(position.x, position.y, capital.x, capital.y)
                })
                .or_else(|| {
                    owned_properties.iter().min_by_key(|(position, _)| {
                        map.distance(position.x, position.y, capital.x, capital.y)
                    })
                })
                .map(|(position, _)| *position)?;
            let expected_island = island_map
                .get_island_at(&forward_property)
                .map(|island| island.id);
            // 集結地点は施設上へ置かない。前線拠点に隣接する空き地を優先し、
            // 完全編成待ちのCombat Entityが生産口を塞ぐことを防ぐ。
            (0..map.height)
                .flat_map(|y| (0..map.width).map(move |x| GridPosition { x, y }))
                .filter(|position| !property_positions.contains(position))
                .filter(|position| !occupied.contains(position))
                .filter(|position| {
                    island_map.get_island_at(position).map(|island| island.id) == expected_island
                })
                .min_by_key(|position| {
                    (
                        map.distance(
                            position.x,
                            position.y,
                            forward_property.x,
                            forward_property.y,
                        ),
                        map.distance(position.x, position.y, capital.x, capital.y),
                        position.y,
                        position.x,
                    )
                })
                .or(Some(forward_property))
        });
        let capital_assault_authorized = enemy_capital_island.is_some_and(|capital_island| {
            if home_island != Some(capital_island) {
                return logistics_plan
                    .as_ref()
                    .is_some_and(|plan| plan.selected_islands.is_empty());
            }
            let Some(home) = capital_pos else {
                return false;
            };
            let Some(capital) = enemy_capital else {
                return false;
            };
            let route_distance = map.distance(home.x, home.y, capital.x, capital.y).max(1);
            let forward_distance = owned_properties
                .iter()
                .filter(|(position, _)| {
                    island_map
                        .get_island_at(position)
                        .is_some_and(|island| island.id == capital_island)
                })
                .map(|(position, _)| map.distance(position.x, position.y, capital.x, capital.y))
                .min()
                .unwrap_or(u32::MAX);
            // DAGが存在する地図では、側方の所有地ではなく各routeの先頭未確保拠点を
            // 正本にする。Topology未初期化の単体テストなどだけ、従来の距離判定へ
            // フォールバックする。
            capital_route_assault_authorized_from_dag(world, player_id, capital_island)
                .unwrap_or_else(|| {
                    same_land_capital_front_reached(route_distance, forward_distance)
                })
        });
        if let (Some(capital), Some(capital_island), Some(staging_anchor)) =
            (enemy_capital, enemy_capital_island, staging_anchor)
        {
            campaign_objectives.push(CampaignPlanningObjective {
                island_id: capital_island,
                kind: OperationKind::AssaultCapital,
                anchor: capital,
                objective_properties: vec![capital],
                capture_eta: None,
                required_capture_survivors: 0,
                logistics_rank: None,
                // 同じ陸塊の全敵を強制対象にしない。局地Captureと首都のanchorへ
                // 到着できる敵を最寄りOperationへ一意に割り当てる。
                forced_target_enemies: HashSet::new(),
                protected_capture_entities: HashSet::new(),
                staging_anchor,
                execution_authorized: capital_assault_authorized,
            });
        }

        Some(BoardScan {
            map,
            master_data,
            damage_chart,
            funds,
            free_facilities: facilities,
            production_facilities,
            available_types,
            my_units,
            enemy_units,
            owned_airport_count,
            open_properties,
            enemy_income,
            enemy_production_slots,
            enemy_facilities,
            my_income,
            campaign_objectives,
            capital_assault_authorized,
            capital_staging_anchor: staging_anchor,
            enemy_production_forecast: EnemyProductionForecastTrace::default(),
        })
    }

    /// 指定した拠点で `unit_type` を生産できるか。
    fn can_produce(&self, terrain: Terrain, unit_type: UnitType) -> bool {
        self.master_data
            .can_produce_unit(terrain.as_str(), unit_type)
    }

    /// 生産可能なユニットのうち、占領可能で最も安いものを「基準占領ユニット」とする。
    /// 展開リードタイムや到達可能性はこのユニットの足で測る。
    fn reference_capture_unit(&self) -> Option<&UnitStats> {
        self.available_types
            .iter()
            .filter(|(unit_type, stats)| {
                stats.can_capture
                    && self
                        .production_facilities
                        .iter()
                        .any(|(_, terrain)| self.can_produce(*terrain, *unit_type))
            })
            .min_by_key(|(_, stats)| stats.cost)
            .map(|(_, stats)| stats)
    }
}

/// 距離と移動力から到達ターン数を見積もる。
fn eta_turns(map: &Map, from: &GridPosition, to: &GridPosition, movement: u32) -> u32 {
    let distance = map.distance(from.x, from.y, to.x, to.y);
    distance.div_ceil(movement.max(1))
}

/// 盤面から作戦の一覧を組み立てる。
fn build_operations(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    active_objectives: &[ActivePlanObjective],
) -> Vec<Operation> {
    let Some(reference) = scan.reference_capture_unit().cloned() else {
        return Vec::new();
    };

    // V4の作戦源は、島campaignが選んだCapture/Defenseと、既に実行を始めた
    // Combat planだけに限定する。全未所有拠点を距離で束ねる第二の目標選定器を
    // 併用すると、勝利ロードマップ外の前線へ生産予算が流れるためである。
    let mut raw = active_objectives
        .iter()
        .filter(|objective| !objective.properties.is_empty())
        .map(|objective| (objective.kind, objective.properties.clone()))
        .collect::<Vec<_>>();

    let island_map = crate::ai::islands::IslandMap::analyze(&scan.map);
    let mut campaign_clusters = scan
        .campaign_objectives
        .iter()
        .map(|objective| {
            let mut properties = if objective.kind == OperationKind::Defense {
                vec![objective.anchor]
            } else if !objective.objective_properties.is_empty() {
                objective.objective_properties.clone()
            } else {
                scan.open_properties
                    .iter()
                    .filter(|position| {
                        island_map
                            .get_island_at(position)
                            .is_some_and(|island| island.id == objective.island_id)
                    })
                    .copied()
                    .collect::<Vec<_>>()
            };
            if properties.is_empty() {
                // 全施設取得後のContest/Reinforceも、残敵への保持・掃討作戦として残す。
                properties.push(objective.anchor);
            }
            properties.sort_unstable_by_key(|position| (position.y, position.x));
            (objective, properties)
        })
        .collect::<Vec<_>>();
    // 勝利目標としての首都作戦と、現在の局地Captureは同じ島でも別Operationにする。
    // 単一大陸全体を一つの完全編成待ちへ畳むと、裸の占領兵だけが先行するためである。
    campaign_clusters.sort_by_key(|(objective, _)| {
        (
            objective.island_id.0,
            u8::from(objective.kind != OperationKind::AssaultCapital),
            objective.kind.priority_rank(),
        )
    });
    raw.retain(|(_, cluster)| {
        !campaign_clusters.iter().any(|(objective, _)| {
            objective.kind != OperationKind::AssaultCapital
                && cluster
                    .iter()
                    .any(|property| objective.objective_properties.contains(property))
        })
    });
    raw.extend(
        campaign_clusters
            .iter()
            .map(|(objective, cluster)| (objective.kind, cluster.clone())),
    );

    let campaign_for_cluster = |kind: OperationKind, cluster: &[GridPosition]| {
        campaign_clusters
            .iter()
            .find_map(|(objective, properties)| {
                (objective.kind == kind
                    && cluster.iter().any(|property| properties.contains(property)))
                .then_some(*objective)
            })
            .or_else(|| {
                // Capture→Defense/Contestの遷移でも、島そのものが同じなら同一campaign。
                campaign_clusters.iter().find_map(|(objective, _)| {
                    cluster
                        .iter()
                        .any(|property| {
                            island_map
                                .get_island_at(property)
                                .is_some_and(|island| island.id == objective.island_id)
                        })
                        .then_some(*objective)
                })
            })
    };

    // 継続中Planのsnapshotと当ターンRoadmapは、同じ島・同じ目的のOperationを
    // 同時に提示し得る。両方を別候補として残すと同じAssaultCapitalがCapture枠を
    // 二重要求し、戦闘生産を毎ターン2体分の歩兵で圧迫する。Plan継続性は後段の
    // `continuing` で合流できるため、campaign identityごとに一件へ正規化する。
    let mut seen_campaign_operations = HashSet::new();
    raw.retain(|(kind, cluster)| {
        campaign_for_cluster(*kind, cluster).is_none_or(|objective| {
            seen_campaign_operations.insert((objective.kind.priority_rank(), objective.island_id))
        })
    });

    // 上位campaignと永続Planが所有する作戦を必須として識別し、追加の局地候補だけを
    // 同時実行容量へ収める。固定4件で第5の戦略作戦を消してはならない。
    let mut scored: Vec<OperationCandidate> = raw
        .into_iter()
        .filter(|(_, cluster)| !cluster.is_empty())
        .map(|(kind, cluster)| {
            let campaign = campaign_for_cluster(kind, &cluster);
            let anchor =
                campaign.map_or_else(|| anchor_of(&cluster, scan), |objective| objective.anchor);
            let lead = facility_lead_time(scan, &anchor, reference.max_movement);
            let continuing = active_objectives.iter().any(|objective| {
                campaign_for_cluster(kind, &cluster)
                    .is_some_and(|campaign| objective.island_id == Some(campaign.island_id))
                    || (objective.island_id.is_none()
                        && objective.kind == kind
                        && cluster
                            .iter()
                            .any(|property| objective.properties.contains(property)))
            });
            OperationCandidate {
                continuing,
                lead,
                kind,
                cluster,
                required: continuing || campaign.is_some(),
            }
        })
        .collect();
    scored.sort_by_key(|candidate| {
        let logistics_rank = campaign_for_cluster(candidate.kind, &candidate.cluster)
            .and_then(|objective| objective.logistics_rank)
            .unwrap_or(u32::MAX);
        (
            candidate.kind.priority_rank(),
            logistics_rank,
            !candidate.continuing,
            candidate.lead,
            // 同条件なら拠点数の多い（面が広い）作戦を優先
            usize::MAX - candidate.cluster.len(),
        )
    });
    let scored = select_operation_candidates(scored);

    let anchors: Vec<GridPosition> = scored
        .iter()
        .map(|candidate| {
            campaign_for_cluster(candidate.kind, &candidate.cluster).map_or_else(
                || anchor_of(&candidate.cluster, scan),
                |objective| objective.staging_anchor,
            )
        })
        .collect();
    let horizons: Vec<u32> = scored
        .iter()
        .map(|candidate| operation_threat_horizon(candidate.kind, candidate.lead))
        .collect();
    let assignment_enabled = scored
        .iter()
        .map(|candidate| {
            campaign_for_cluster(candidate.kind, &candidate.cluster).is_none_or(|objective| {
                objective.kind != OperationKind::AssaultCapital || objective.execution_authorized
            })
        })
        .collect::<Vec<_>>();
    let empty_entity_set = HashSet::new();

    scored
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| {
            let OperationCandidate {
                lead,
                kind,
                cluster,
                ..
            } = candidate;
            let anchor = anchors[index];
            let planning_objective = campaign_for_cluster(kind, &cluster);
            let forced_target_enemies = planning_objective
                .map(|objective| &objective.forced_target_enemies)
                .or_else(|| {
                    active_objectives
                        .iter()
                        .find(|objective| {
                            planning_objective.is_some_and(|campaign| {
                                objective.island_id == Some(campaign.island_id)
                            }) || (objective.island_id.is_none()
                                && objective.kind == kind
                                && objective.properties == cluster)
                        })
                        .map(|objective| &objective.target_enemies)
                })
                .unwrap_or(&empty_entity_set);
            let protected_capture_entities = planning_objective
                .map(|objective| &objective.protected_capture_entities)
                .unwrap_or(&empty_entity_set);
            let mut operation = build_operation(
                scan,
                ctx,
                &reference,
                kind,
                anchor,
                &anchors,
                &horizons,
                &assignment_enabled,
                &cluster,
                forced_target_enemies,
                protected_capture_entities,
                lead,
            );
            if let Some(objective) = planning_objective {
                operation.island_id = Some(objective.island_id);
                operation.staging_anchor = objective.staging_anchor;
                operation.execution_authorized = objective.execution_authorized;
                operation.protected_capture_entities = objective.protected_capture_entities.clone();
            } else if let Some(active) = active_objectives
                .iter()
                .find(|objective| objective.kind == kind && objective.properties == cluster)
            {
                // portfolioがObserveへ一時遷移しても、active Plan由来のOperationは
                // 島identityを失わない。敵やanchorの変化で新Planへ分裂させない。
                operation.island_id = active.island_id;
            }
            operation
        })
        .collect()
}

/// クラスタの代表地点。自軍生産施設に最も近い拠点を選ぶ。
fn anchor_of(cluster: &[GridPosition], scan: &BoardScan) -> GridPosition {
    cluster
        .iter()
        .copied()
        .min_by_key(|pos| {
            scan.free_facilities
                .iter()
                .map(|(f, _)| scan.map.distance(f.x, f.y, pos.x, pos.y))
                .min()
                .unwrap_or(u32::MAX)
        })
        .unwrap_or(cluster[0])
}

/// 生産施設から代表地点までの展開リードタイム（最短）。
fn facility_lead_time(scan: &BoardScan, anchor: &GridPosition, movement: u32) -> u32 {
    scan.free_facilities
        .iter()
        .map(|(f, _)| eta_turns(&scan.map, f, anchor, movement))
        .min()
        .unwrap_or(u32::MAX)
}

fn operation_threat_horizon(kind: OperationKind, deploy_lead_time: u32) -> u32 {
    match kind {
        OperationKind::Defense => DEFENSE_THREAT_ETA,
        OperationKind::Capture => deploy_lead_time.saturating_add(CAPTURE_COMPLETION_TURNS),
        // 首都作戦の論理目標は首都だが、現在バッチの接触地点はDAGの次区間である。
        // 首都到達までの全手番を一括で見積もると、未観測の敵生産を現在要求へ
        // 膨張させるため、次区間を確保して再評価できる時点までに限定する。
        OperationKind::AssaultCapital => deploy_lead_time.saturating_add(CAPTURE_COMPLETION_TURNS),
    }
}

/// 敵が期限内に自力到着できる作戦だけを比較し、その中で最短の1件へ帰属させる。
#[allow(clippy::too_many_arguments)]
fn nearest_relevant_anchor_index(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    pos: &GridPosition,
    movement: MovementType,
    max_movement: u32,
    anchors: &[GridPosition],
    horizons: &[u32],
    assignment_enabled: &[bool],
) -> Option<usize> {
    anchors
        .iter()
        .enumerate()
        .filter(|(index, _)| assignment_enabled.get(*index).copied().unwrap_or(true))
        .filter_map(|(index, anchor)| {
            if !ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (pos.x, pos.y),
                (anchor.x, anchor.y),
                movement,
            ) {
                return None;
            }
            let eta = eta_turns(&scan.map, pos, anchor, max_movement);
            (eta <= horizons.get(index).copied().unwrap_or(0)).then_some((eta, index))
        })
        .min()
        .map(|(_, index)| index)
}

/// 敵を、地形的に到達できる最寄りの作戦へ一意に帰属させる。
///
/// 作戦の期限は毎ターン変化する予測値であり、敵が局地目標から遠いという理由で
/// Combat計画の入力から消してはならない。期限は防衛案の比較にだけ用いる。
fn nearest_reachable_anchor_index(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    pos: &GridPosition,
    movement: MovementType,
    max_movement: u32,
    anchors: &[GridPosition],
    assignment_enabled: &[bool],
) -> Option<usize> {
    anchors
        .iter()
        .enumerate()
        .filter(|(index, _)| assignment_enabled.get(*index).copied().unwrap_or(true))
        .filter_map(|(index, anchor)| {
            ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (pos.x, pos.y),
                (anchor.x, anchor.y),
                movement,
            )
            .then_some((eta_turns(&scan.map, pos, anchor, max_movement), index))
        })
        .min()
        .map(|(_, index)| index)
}

fn enemy_facility_arrival(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    facility: EnemyFacilitySnapshot,
    anchor: GridPosition,
) -> Option<(u32, u32)> {
    scan.available_types
        .iter()
        .filter(|(unit_type, stats)| {
            stats.max_cargo == 0 && scan.can_produce(facility.terrain, *unit_type)
        })
        .filter_map(|(_, stats)| {
            ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (facility.pos.x, facility.pos.y),
                (anchor.x, anchor.y),
                stats.movement_type,
            )
            .then_some((
                eta_turns(&scan.map, &facility.pos, &anchor, stats.max_movement),
                stats.cost,
            ))
        })
        .min_by_key(|(eta, cost)| (*eta, *cost))
}

/// 敵施設も期限内に到着できる最寄り作戦へ一意に割り当て、全収入の重複計上を防ぐ。
fn projected_enemy_reinforcement_envelope(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    anchors: &[GridPosition],
    horizons: &[u32],
    anchor_index: usize,
) -> EnemyReinforcementEnvelope {
    if scan.enemy_production_slots == 0 {
        return EnemyReinforcementEnvelope::default();
    }
    let mut local_slots = 0_u32;
    let mut production_capacity = 0_u32;
    let mut available_slot_turns = 0_u32;
    let mut expected_wave_capacity = 0_u32;
    for facility in &scan.enemy_facilities {
        let assignment = anchors
            .iter()
            .enumerate()
            .filter_map(|(index, anchor)| {
                // 編成中でまだ前進を許可していない首都攻略は、敵増援の配分先ではない。
                // ここへ距離0で全枠を吸わせると、実際に占領中の中央島を「増援なし」と
                // 誤認して占領兵だけを送り続けるため、実行中の局地作戦を先に評価する。
                let is_unauthorized_capital_formation = scan
                    .campaign_objectives
                    .iter()
                    .find(|objective| objective.anchor == *anchor)
                    .is_some_and(|objective| {
                        objective.kind == OperationKind::AssaultCapital
                            && !objective.execution_authorized
                    });
                if is_unauthorized_capital_formation {
                    return None;
                }
                let (eta, cost) = enemy_facility_arrival(scan, ctx, *facility, *anchor)?;
                (eta <= horizons.get(index).copied().unwrap_or(0)).then_some((eta, index, cost))
            })
            .min_by_key(|(eta, index, _)| (*eta, *index));
        let Some((eta, assigned_index, unit_cost)) = assignment else {
            continue;
        };
        if assigned_index != anchor_index {
            continue;
        }
        let production_turns = horizons[anchor_index].saturating_sub(eta);
        if production_turns == 0 {
            continue;
        }
        local_slots = local_slots.saturating_add(1);
        available_slot_turns = available_slot_turns.saturating_add(production_turns);
        production_capacity =
            production_capacity.saturating_add(unit_cost.saturating_mul(production_turns));
        // Expectedでは将来全turnではなく、次の生産波にこの施設が出せる代表unitの
        // 価格だけを使う。次波以降は次手番の盤面・観測で再計画する。
        expected_wave_capacity = expected_wave_capacity.saturating_add(unit_cost);
    }
    if local_slots == 0 {
        return EnemyReinforcementEnvelope::default();
    }
    let allocated_income_per_turn = u64::from(scan.enemy_income)
        .saturating_mul(u64::from(local_slots))
        / u64::from(scan.enemy_production_slots.max(1));
    let income_capacity =
        allocated_income_per_turn.saturating_mul(u64::from(horizons[anchor_index]));
    // 敵工場数×期限は物理上限に過ぎない。収入で賄えない額をstress scenarioへ
    // 混ぜないよう先に物理・資金両方で上限を固定する。
    let stress_capacity =
        production_capacity.min(u32::try_from(income_capacity).unwrap_or(u32::MAX));
    // 実観測をExpectedへ反映する。一方、初回観測は開幕配置と区別できないため
    // forecastが0になる。そこで次の生産波の施設価格を事前分布として下限にし、
    // 未観測=敵が生産しない、という誤った停止を防ぐ。
    let observed_wave_capacity = u64::from(scan.enemy_production_forecast.expected_cost_next_turn)
        .saturating_mul(u64::from(local_slots))
        / u64::from(scan.enemy_production_slots.max(1));
    let prior_wave_capacity =
        expected_wave_capacity.min(u32::try_from(allocated_income_per_turn).unwrap_or(u32::MAX));
    let expected_capacity = u32::try_from(observed_wave_capacity)
        .unwrap_or(u32::MAX)
        .max(prior_wave_capacity)
        .min(stress_capacity);
    EnemyReinforcementEnvelope {
        expected_funds: expected_capacity,
        stress_funds: stress_capacity,
    }
}

/// 1 つの作戦について観測量を集め、枠を導出する。
#[allow(clippy::too_many_arguments)]
fn build_operation(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    reference: &UnitStats,
    kind: OperationKind,
    anchor: GridPosition,
    anchors: &[GridPosition],
    horizons: &[u32],
    assignment_enabled: &[bool],
    cluster: &[GridPosition],
    forced_target_enemies: &HashSet<Entity>,
    protected_capture_entities: &HashSet<Entity>,
    deploy_lead_time: u32,
) -> Operation {
    // この作戦を「最寄りの作戦」とするユニットだけを、この作戦の担当として数える。
    // これにより 1 体のユニットが複数作戦に二重計上されない。
    let anchor_index = anchors.iter().position(|candidate| *candidate == anchor);

    // 基準占領ユニットが自力で到達できるかどうかで輸送要否が決まる。
    // 「島だから輸送が要る」ではなく「地形的に繋がっていないから要る」と判定する。
    let requires_transport = !scan.free_facilities.iter().any(|(f, _)| {
        ctx.is_reachable(
            &scan.map,
            &scan.master_data,
            (f.x, f.y),
            (anchor.x, anchor.y),
            reference.movement_type,
        )
    });

    // 敵戦力は、現在の局地敵と将来到着する増援に分離する。
    // 別島の敵Entityを具体的な撃破目標へ入れると、生産Entityがanchorを離れて
    // 敵初期空港まで追跡し、輸送・占領工程を前進させない。別島から期限内に
    // 到着できる敵はanchor到着時点の仮想増援として計画へ入れ、現在の任務対象にしない。
    //
    // (1) を基準占領ユニット（歩兵）の足で判定してはならない。
    // 海の向こうで拠点を取り続ける敵の占領部隊は、歩兵の足では届かないというだけで
    // 脅威の集計から丸ごと消え、撃破枠が立たず、対抗候補の採点対象にもならなくなる。
    // 実際にはヘリ・艦船・航空機で届くのだから、生産しうる移動タイプ全体で問う。
    // 「制空で応じるか対空で応じるか」が思想ではなく到達可能性の問題であるのと同じで、
    // 「敵の占領部隊を潰しに行けるか」もまた到達可能性の問題でしかない。
    let producible_movement_types: Vec<MovementType> = {
        let mut seen = HashSet::new();
        scan.available_types
            .iter()
            .map(|(_, stats)| stats.movement_type)
            .filter(|movement_type| seen.insert(*movement_type))
            .collect()
    };

    let mut reachable_threats = Vec::new();
    let mut unreachable_threats = Vec::new();
    let mut enemy_contact_eta = u32::MAX;
    let island_map = crate::ai::islands::IslandMap::analyze(&scan.map);
    let anchor_island = island_map.get_island_at(&anchor).map(|island| island.id);
    for enemy in &scan.enemy_units {
        let forced_target = enemy
            .entity
            .is_some_and(|entity| forced_target_enemies.contains(&entity));
        if !forced_target
            && nearest_reachable_anchor_index(
                scan,
                ctx,
                &enemy.pos,
                enemy.stats.movement_type,
                enemy.stats.max_movement,
                anchors,
                assignment_enabled,
            ) != anchor_index
        {
            continue;
        }
        let i_can_reach = producible_movement_types.iter().any(|movement_type| {
            ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (anchor.x, anchor.y),
                (enemy.pos.x, enemy.pos.y),
                *movement_type,
            )
        });
        let it_can_reach_me = ctx.is_reachable(
            &scan.map,
            &scan.master_data,
            (enemy.pos.x, enemy.pos.y),
            (anchor.x, anchor.y),
            enemy.stats.movement_type,
        );
        debug_assert!(
            forced_target || it_can_reach_me,
            "到達可能な最寄り作戦へ敵を帰属済み"
        );
        let arrival_eta = eta_turns(&scan.map, &enemy.pos, &anchor, enemy.stats.max_movement);
        let local_horizon = anchor_index
            .and_then(|index| horizons.get(index).copied())
            .unwrap_or(0);
        if kind == OperationKind::Capture && !forced_target && arrival_eta > local_horizon {
            // 同じ大陸の遠方敵をすべて局地護衛へ入れない。占領完了までにこの前線へ
            // 接触できない敵は、前線更新後または首都作戦Go後に別パッケージで扱う。
            continue;
        }
        if it_can_reach_me {
            enemy_contact_eta = enemy_contact_eta.min(arrival_eta);
        }
        let enemy_island = island_map.get_island_at(&enemy.pos).map(|island| island.id);
        let local_contact = enemy_island == anchor_island || arrival_eta == 0;
        if i_can_reach && local_contact {
            reachable_threats.push(ThreatTarget::from_snapshot(enemy));
        } else if !i_can_reach && local_contact {
            unreachable_threats.push(ThreatTarget::from_snapshot(enemy));
        } else if it_can_reach_me
            // 空の輸送unitは単独では占領も攻撃もできない。全ての中立前線で
            // 「将来来る輸送ヘリ」として対空購入を発生させず、実cargoを搭載して
            // 作戦を開始した時点から到着scenarioへ含める。
            && (enemy.stats.max_cargo == 0 || enemy.free_cargo < enemy.stats.max_cargo)
            && arrival_eta
                <= horizons
                    .get(anchor_index.unwrap_or(usize::MAX))
                    .copied()
                    .unwrap_or(0)
        {
            // 具体Entityを追わせず、到着後にだけ攻撃可能な増援として保持する。
            let mut incoming = ThreatTarget::from_snapshot(enemy);
            incoming.entity = None;
            incoming.position = anchor;
            incoming.available_turn = arrival_eta.max(1);
            reachable_threats.push(incoming);
        }
    }
    let enemy_combat_units = u32::try_from(reachable_threats.len()).unwrap_or(u32::MAX);
    let unreachable_threat_units = u32::try_from(unreachable_threats.len()).unwrap_or(u32::MAX);
    // --- 自軍戦力の仕分け ---
    // 敵の仕分けが済んでから数える。
    //
    // 台帳（既存戦力の計上）は、必ず `slot_fitness` の採用条件と同じ判定を使う。
    // 両者がずれると「その枠を埋められるのに、その枠の充足としては数えられない」
    // ユニットが生まれ、枠の要求が永久に減らずに同じユニットを買い続けるラチェットになる。
    // そのため排他に振り分けず、埋められる枠すべてに計上する。
    // （1 体の対空ユニットが航空脅威の抑止と地上の頭数を兼ねるのは実態としても正しい）
    //
    // ここで「最寄りの作戦か」で仕分けてはならない。自軍ユニットは自陣の生産施設で
    // 生まれるので、位置で排他に割り振ると母港に近い作戦が全部を吸い、渡洋作戦側の
    // 台帳は永久に 0 のままになる。要求が一切減らないので同じユニットを毎ターン
    // 買い続けるラチェットになる（実測で歩兵 56 体、揚陸艇 7 隻、対空 20 両）。
    // 上限（`MAX_CAPTURE_SLOTS` 等）はあくまで 1 波の規模であって、
    // 「既に持っている分」を差し引く役割は担っていない。差し引きはこの台帳の仕事。
    let mut friendly_capture_units_committed = 0u32;
    let mut friendly_combat_units_committed = 0u32;
    let mut friendly_intercept_units_committed = 0u32;
    let mut available_free_cargo_slots = 0u32;
    for unit in &scan.my_units {
        if unit.stats.can_capture {
            // Attack任務の歩兵までCapture充足へ数えると、実際の担当が全滅しても
            // 補充が止まる。排他割当でこの作戦のCapture役になっているEntityだけを
            // 構造枠へ計上し、Combat側では同じ歩兵の戦闘・生存能力を別途利用する。
            if unit
                .entity
                .is_some_and(|entity| protected_capture_entities.contains(&entity))
                && can_join_operation(
                    scan,
                    ctx,
                    &anchor,
                    requires_transport,
                    &unit.pos,
                    &unit.stats,
                )
            {
                friendly_capture_units_committed += 1;
            }
        } else if unit.stats.max_cargo == 0 {
            // 迎撃枠の条件：到達できない脅威へ有効打を持ち、かつ自力で現地へ行ける
            let self_deployable = ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (unit.pos.x, unit.pos.y),
                (anchor.x, anchor.y),
                unit.stats.movement_type,
            );
            if self_deployable
                && threats_have_counter(
                    &unit.stats,
                    &unreachable_threats,
                    &(0..unreachable_threats.len()).collect::<Vec<_>>(),
                    &scan.damage_chart,
                )
            {
                friendly_intercept_units_committed =
                    friendly_intercept_units_committed.saturating_add(1);
            }
            // 撃破枠の条件：現地へ行けて、自分が実際に届く敵に対して有効打を持つ。
            // 敵が観測できない段階では誰でも採用されうるので、台帳側も同様に全員を数える。
            if !can_join_operation(
                scan,
                ctx,
                &anchor,
                requires_transport,
                &unit.pos,
                &unit.stats,
            ) {
                continue;
            }
            let origin = if self_deployable { unit.pos } else { anchor };
            let engageable =
                reachable_threat_indices(scan, ctx, &reachable_threats, origin, &unit.stats);
            let combat_eligible = reachable_threats.is_empty()
                || threats_have_counter(
                    &unit.stats,
                    &reachable_threats,
                    &engageable,
                    &scan.damage_chart,
                );
            if combat_eligible {
                // 1体が同じ手番に攻撃できる作戦は1つだけである。unit価格による
                // 戦力価値もsortie体数も、期限内に到着できる最寄り作戦へ排他的に
                // 帰属させ、全前線で同じ1体を重複控除しない。
                let belongs_to_control_operation = nearest_relevant_anchor_index(
                    scan,
                    ctx,
                    &unit.pos,
                    unit.stats.movement_type,
                    unit.stats.max_movement,
                    anchors,
                    horizons,
                    assignment_enabled,
                ) == anchor_index;
                if !belongs_to_control_operation {
                    continue;
                }
                friendly_combat_units_committed = friendly_combat_units_committed.saturating_add(1);
            }
        }
    }

    // 輸送台帳も同じ原則で数える。輸送枠の採用条件は「その積荷をこの作戦地点へ
    // 届けられるか」なので、台帳も同じく `can_deliver_cargo` で数える。
    let cargo_movements: Vec<MovementType> = {
        let mut seen = HashSet::new();
        scan.available_types
            .iter()
            .filter(|(_, stats)| stats.can_capture)
            .map(|(_, stats)| stats.movement_type)
            .filter(|movement_type| seen.insert(*movement_type))
            .collect()
    };
    for unit in &scan.my_units {
        if unit.free_cargo == 0 {
            continue;
        }
        let deliverable = cargo_movements.iter().any(|cargo_movement| {
            can_deliver_cargo(
                &scan.map,
                &scan.master_data,
                ctx,
                &unit.pos,
                &anchor,
                unit.stats.movement_type,
                *cargo_movement,
            )
        });
        if deliverable {
            available_free_cargo_slots = available_free_cargo_slots.saturating_add(unit.free_cargo);
        }
    }

    let enemy_reinforcement = anchor_index
        .map_or_else(EnemyReinforcementEnvelope::default, |index| {
            projected_enemy_reinforcement_envelope(scan, ctx, anchors, horizons, index)
        });

    // 輸送 1 往復にかかるターン数（片道リードタイムの 2 倍）
    let transport_round_trip_turns = deploy_lead_time.saturating_mul(2).max(1);

    let facts = OperationFacts {
        target_property_count: cluster.len() as u32,
        friendly_capture_units_committed,
        enemy_combat_units,
        friendly_combat_units_committed,
        // OperationFactsの既存フィールドはExpectedを保持する。Go判定には使わず、
        // RollingPlanへ渡す通常の継続生産scenarioだけに使う。
        enemy_reinforcement_funds: enemy_reinforcement.expected_funds,
        friendly_intercept_units_committed,
        deploy_lead_time,
        enemy_contact_eta: if enemy_contact_eta == u32::MAX {
            u32::MAX
        } else {
            enemy_contact_eta
        },
        requires_transport,
        transport_round_trip_turns,
        available_free_cargo_slots,
        unreachable_threat_units,
    };

    let mut slots = derive_slots(&facts);
    // Combat枠は金額の差分ではなく、観測敵が残っていることだけで計画器を起動する。
    // 敵の将来生産余力は再評価用の監視情報であり、必要数と完了判定に混ぜない。
    slots.combat_plan_required = u32::from(!reachable_threats.is_empty());
    // AssaultCapitalも局地Captureと同じ作戦内で複数施設を前進目標として持つ。
    // 構造生産を島campaignが予約済みの手番は`allow_structural_slots`側で一括して
    // Capture/Transportを無効化するため、ここで恒久的に0へ落としてはならない。

    let mut operation = Operation {
        kind,
        island_id: None,
        anchor,
        staging_anchor: anchor,
        execution_authorized: true,
        protected_capture_entities: HashSet::new(),
        objective_properties: cluster.to_vec(),
        threat_horizon: anchor_index
            .and_then(|index| horizons.get(index).copied())
            .unwrap_or(0),
        slots,
        facts,
        filled: OperationSlots::default(),
        unreachable_threats,
        reachable_threats,
        expected_reinforcements: Vec::new(),
        reinforcement_contingencies: Vec::new(),
        stress_reinforcement_funds: enemy_reinforcement.stress_funds,
        contingency_reserve_funds: 0,
    };
    let horizon = operation.threat_horizon.max(1);
    let expected_assessment = enemy_reinforcement_assessment(
        scan,
        ctx,
        &operation,
        horizon,
        enemy_reinforcement.expected_funds,
        ReinforcementScenario::Expected,
    );
    let stress_assessment = enemy_reinforcement_assessment(
        scan,
        ctx,
        &operation,
        horizon,
        enemy_reinforcement.stress_funds,
        ReinforcementScenario::Stress,
    );
    // Expectedは現在のRollingPlanへ仮想敵として渡す。Go判定とは独立しているため、
    // 最低限の戦力で進撃を開始しつつ、到達時刻付きの次波へ備えた生産を続けられる。
    operation.expected_reinforcements = expected_assessment.reinforcements;
    // 可視敵がいなくても、期限内に前線へ到着するExpectedがあれば継続生産を開始する。
    // これは戦闘開始のGoではなく、到着予定の敵に対する編成開始である。
    operation.slots.combat_plan_required = u32::from(
        !operation.reachable_threats.is_empty() || !operation.expected_reinforcements.is_empty(),
    );
    operation.reinforcement_contingencies = stress_assessment.contingencies;
    // Stressは現在のGoや現金を固定しない。観測時に実兵種・実HPで再計画するための
    // counter候補として残し、Expected生産を予約資金で止めない。
    operation.contingency_reserve_funds = 0;
    operation
}

/// 作戦一覧と資金から、この生産フェーズで発行する生産命令を組み立てる。
#[cfg(test)]
fn plan_production(
    scan: &BoardScan,
    player_id: PlayerId,
    allow_structural_slots: bool,
    committed_combat_assignments: &HashMap<Entity, deployment::ActiveTargetAssignment>,
) -> (Vec<PlannedProduction>, ProductionPlanTrace) {
    let mut registry = V4RollingPlanRegistry::default();
    plan_production_with_registry(
        scan,
        player_id,
        allow_structural_slots,
        committed_combat_assignments,
        &AdvancingCapitalRouteEntities::default(),
        0,
        &mut registry,
    )
}

fn plan_production_with_registry(
    scan: &BoardScan,
    player_id: PlayerId,
    allow_structural_slots: bool,
    committed_combat_assignments: &HashMap<Entity, deployment::ActiveTargetAssignment>,
    advancing_route_entities: &AdvancingCapitalRouteEntities,
    turn: u32,
    plan_registry: &mut V4RollingPlanRegistry,
) -> (Vec<PlannedProduction>, ProductionPlanTrace) {
    let mut ctx = ReachCtx::default();
    let active_objectives = plan_registry.active_objectives(player_id);
    let mut operations = build_operations(scan, &mut ctx, &active_objectives);
    let mut plan_trace =
        ProductionPlanTrace::new(player_id, scan.funds, scan.free_facilities.len());

    if operations.is_empty() {
        plan_trace.fallback = true;
        // 作戦に接続されない汎用戦闘生産は行わない。戦力が必要なら、兵站確保・
        // 防衛・首都攻略のいずれかを先にOperationとして成立させる。
        return (Vec::new(), plan_trace);
    }

    // campaignと切り離した汎用clusterが、固定兵站工程のCombat予算を先取りしない。
    // 防衛を最上位に保ちつつ、Capture同士では兵站工程順、その後に敵接触ETAで並べる。
    operations.sort_by_key(|op| {
        let logistics_rank = scan
            .campaign_objectives
            .iter()
            .find(|objective| objective.anchor == op.anchor)
            .and_then(|objective| objective.logistics_rank)
            .unwrap_or(u32::MAX);
        (
            operation_priority_rank(op),
            logistics_rank,
            op.facts.enemy_contact_eta,
        )
    });
    if !allow_structural_slots {
        // 占領要員と輸送役は島嶼キャンペーン側で予約済み。余剰予算を同じ役割へ
        // 二重投入せず、観測済みの敵に対する迎撃・護衛・撃破だけへ使う。
        // ただし陸続きの前線はcampaign専用生産の対象外なので、別の渡洋作戦が
        // 同時に予約中でも構造枠を消してはならない。
        for operation in &mut operations {
            if operation.facts.requires_transport {
                operation.slots.capture_units = 0;
                operation.slots.transport_slots = 0;
            }
        }
    }
    defer_unapproved_capital_structure_for_active_segment(&mut operations);

    plan_trace.operations = operations
        .iter()
        .map(|op| ProductionOperationTrace {
            kind: op.kind,
            anchor: op.anchor,
            slots: op.slots,
            requires_transport: op.facts.requires_transport,
            enemy_combat_units: op.facts.enemy_combat_units,
            enemy_reinforcement_funds: op.facts.enemy_reinforcement_funds,
            enemy_stress_reinforcement_funds: op.stress_reinforcement_funds,
            contingency_reserve_funds: op.contingency_reserve_funds,
            reinforcement_contingencies: op
                .reinforcement_contingencies
                .iter()
                .map(|contingency| ReinforcementContingencyTrace {
                    enemy_type: contingency.enemy_type,
                    enemy_contact_turn: contingency.enemy_contact_turn,
                    counter_type: contingency.counter_type,
                    counter_facility: contingency.counter_facility,
                    counter_build_turn: contingency.counter_build_turn,
                    counter_contact_turn: contingency.counter_contact_turn,
                    attacks_required: contingency.attacks_required,
                    reserve_cost: contingency.reserve_cost,
                })
                .collect(),
            deploy_lead_time: op.facts.deploy_lead_time,
        })
        .collect();

    // 1体は1手番に1作戦しか遂行できない。過去のpriority enemyが移動して複数作戦へ
    // 分散しても、現在位置から最も近い1作戦だけへ既存Combat Entityを帰属させる。
    let mut committed_by_operation: HashMap<
        usize,
        HashMap<Entity, deployment::ActiveTargetAssignment>,
    > = HashMap::new();
    for (&entity, assignment) in committed_combat_assignments {
        let Some(unit) = scan
            .my_units
            .iter()
            .find(|unit| unit.entity == Some(entity))
        else {
            continue;
        };
        let assigned_operation = operations
            .iter()
            .enumerate()
            .filter(|(_, operation)| {
                operation.reachable_threats.iter().any(|threat| {
                    threat
                        .entity
                        .is_some_and(|enemy| assignment.targets.contains(&enemy))
                })
            })
            .min_by_key(|(_, operation)| {
                scan.map.distance(
                    unit.pos.x,
                    unit.pos.y,
                    operation.anchor.x,
                    operation.anchor.y,
                )
            })
            .map(|(index, _)| index);
        if let Some(index) = assigned_operation {
            committed_by_operation
                .entry(index)
                .or_default()
                .insert(entity, assignment.clone());
        }
    }

    let mut used_facilities: HashSet<GridPosition> = HashSet::new();
    // 占有済みという事実だけでなく、どの作戦が枠を使ったかも保持する。
    // これにより占領作戦同士の競合を「上位防衛による中断」と誤認しない。
    let mut facility_owners: HashMap<GridPosition, u32> = HashMap::new();
    // rolling plannerは1作戦につき1回だけ実行し、その混成パッケージの当手番分を
    // 施設ごとに順次消費する。施設を1つ埋めるたびに同じbeam searchをやり直さない。
    let mut rolling_plans: HashMap<usize, SelectedPlan> = HashMap::new();
    let mut seen_plan_ids = HashSet::new();
    let empty_committed_entities = HashSet::new();
    let mut remaining_funds = scan.funds;
    // この手番に発注済みの永続Plan購入。Registry上は生産完了を観測するまで
    // Plannedのまま残るため、余剰予算の計算では二重に予約しない。
    let mut issued_plan_purchase_cost = 0_u32;
    let mut commands = Vec::new();

    loop {
        let free_slots = scan
            .free_facilities
            .iter()
            .filter(|(pos, _)| !used_facilities.contains(pos))
            .count();
        if free_slots == 0 {
            break;
        }
        // 最も不足している枠を持つ作戦から順に見ていく
        let Some((op_index, slot_kind)) = most_starved_slot(&operations) else {
            break;
        };

        // 先に並ぶ作戦の観測後counterを発動できる現金は、下位作戦へ流さない。
        // 自作戦の予約は現在の確定脅威を処理するために使ってよい。
        let higher_priority_contingency_reserve = operations[..op_index]
            .iter()
            .map(|operation| operation.contingency_reserve_funds)
            .fold(0_u32, u32::saturating_add);
        let spendable_funds = remaining_funds.saturating_sub(higher_priority_contingency_reserve);
        // 1 枠あたり予算。高価なユニットで枠を食い潰さないためのソフト上限。
        let per_slot_budget = spendable_funds / free_slots as u32;

        // トレース用に、選定前の未充足率と作戦の識別情報を控えておく。
        let operation_kind = operations[op_index].kind;
        let operation_anchor = operations[op_index].anchor;
        let deficit_before = operations[op_index]
            .slots
            .deficit_ratio(slot_kind, &operations[op_index].filled);
        let remaining_funds_before = remaining_funds;

        if slot_kind == SlotKind::Combat && !rolling_plans.contains_key(&op_index) {
            let target_enemies = operations[op_index]
                .reachable_threats
                .iter()
                .filter_map(|threat| threat.entity)
                .collect::<HashSet<_>>();
            let continuation = plan_registry.continuation_for_operation(
                player_id,
                turn,
                operation_kind,
                operations[op_index].island_id,
                &operations[op_index].objective_properties,
                &target_enemies,
            );
            // 既存戦力は、PlanIdが一致する発注済み部隊だけでなく、親戦役DAGの
            // 未完了区間へ前進中の部隊から導く。ただし首都作戦と「次の区間を確保する」
            // Captureが同じ戦力を同時に数えることはない。前段では区間目標に一致する
            // Captureだけが受け取り、首都戦闘へ移るのは実行許可後である。
            // 別のPlan由来の部隊は借りず、即時Combat増援は実在敵への任務を持つ場合だけ
            // 入れるため、防衛・輸送・回復部隊を見かけの首都戦力にしない。
            let continuation_plan_id = continuation.as_ref().map(|plan| plan.plan_id);
            let mut committed_for_plan = committed_entities_for_plan(
                committed_by_operation.get(&op_index),
                continuation_plan_id,
            );
            // Stageは前衛ではないが、Roadmapが同じDAG区間へ予約した実Entityである。
            // 到着予定付きの既存戦力としてRollingPlanへ渡し、人数だけを根拠に
            // 追加生産を禁止する第二の判定器にはしない。
            let mut staged_for_plan = HashSet::new();
            if let Some(island_id) = operations[op_index].island_id {
                match operation_kind {
                    OperationKind::Capture => {
                        for objective in &operations[op_index].objective_properties {
                            if let Some(route_entities) = advancing_route_entities
                                .by_target
                                .get(&(island_id, *objective))
                            {
                                committed_for_plan.extend(route_entities.iter().copied());
                            }
                            if let Some(route_entities) = advancing_route_entities
                                .staged_by_target
                                .get(&(island_id, *objective))
                            {
                                staged_for_plan.extend(route_entities.iter().copied());
                            }
                        }
                    }
                    OperationKind::AssaultCapital if operations[op_index].execution_authorized => {
                        if let Some(route_entities) =
                            advancing_route_entities.by_island.get(&island_id)
                        {
                            committed_for_plan.extend(route_entities.iter().copied());
                        }
                        for ((staged_island, _), route_entities) in
                            &advancing_route_entities.staged_by_target
                        {
                            if *staged_island == island_id {
                                staged_for_plan.extend(route_entities.iter().copied());
                            }
                        }
                    }
                    OperationKind::Defense | OperationKind::AssaultCapital => {}
                }
            }
            let rolling_input = combat_plan_input(
                scan,
                &mut ctx,
                &operations[op_index],
                &used_facilities,
                if committed_for_plan.is_empty() {
                    &empty_committed_entities
                } else {
                    &committed_for_plan
                },
                &staged_for_plan,
                spendable_funds,
                !allow_structural_slots,
            );
            if let Some(input) = rolling_input {
                // 首都攻略の編成中は、固定購入列が今も実行可能ならその再評価だけを行う。
                // 継続すると分かっている案の全beam searchを先に実行して捨てない。
                let evaluated_continuation = continuation.map(|previous| {
                    let evaluated = evaluate_fixed_package(&input, &previous.purchases);
                    (previous, evaluated)
                });
                let reusable_candidate =
                    evaluated_continuation
                        .as_ref()
                        .and_then(|(previous, evaluated)| {
                            previous
                                .can_reuse_without_search(
                                    operation_kind,
                                    operations[op_index].execution_authorized,
                                )
                                .then(|| evaluated.as_ref().ok().cloned())
                                .flatten()
                        });
                let Some(candidate_plan) =
                    reusable_candidate.or_else(|| plan_force_package(&input))
                else {
                    clear_slot(&mut operations[op_index], SlotKind::Combat);
                    continue;
                };
                let conflicted_facilities = evaluated_continuation
                    .as_ref()
                    .map(|(previous, evaluated)| {
                        if evaluated.is_ok() {
                            return HashSet::new();
                        }
                        let mut due = previous
                            .purchases
                            .iter()
                            .filter(|purchase| purchase.build_turn == 0)
                            .copied()
                            .collect::<Vec<_>>();
                        due.sort_unstable_by_key(|purchase| {
                            (purchase.facility.y, purchase.facility.x, purchase.cost)
                        });
                        let mut affordable_funds = input.current_funds;
                        let mut conflicts = HashSet::new();
                        for purchase in due {
                            let claimed_by_higher_priority = facility_owners
                                .get(&purchase.facility)
                                .is_some_and(|owner| {
                                    *owner < operation_priority_rank(&operations[op_index])
                                });
                            let claimed_by_campaign = scan
                                .production_facilities
                                .iter()
                                .any(|(facility, _)| *facility == purchase.facility)
                                && !scan
                                    .free_facilities
                                    .iter()
                                    .any(|(facility, _)| *facility == purchase.facility);
                            if claimed_by_higher_priority || claimed_by_campaign {
                                conflicts.insert(purchase.facility);
                            } else if operation_kind == OperationKind::AssaultCapital {
                                if purchase.cost <= affordable_funds {
                                    affordable_funds =
                                        affordable_funds.saturating_sub(purchase.cost);
                                } else {
                                    // 上位作戦が当手番の現金を使った場合も、失敗ではなく
                                    // 当該施設の首都編成列を次手番以降へ繰り下げる。
                                    conflicts.insert(purchase.facility);
                                }
                            }
                        }
                        conflicts
                    })
                    .unwrap_or_default();
                let selected = plan_registry.select_for_operation(
                    player_id,
                    turn,
                    operation_kind,
                    operations[op_index].island_id,
                    operation_anchor,
                    operations[op_index].objective_properties.clone(),
                    target_enemies,
                    operations[op_index].execution_authorized,
                    evaluated_continuation,
                    candidate_plan,
                    input.hard_deadline,
                    conflicted_facilities,
                );
                if let Some(plan_id) = selected.plan_id {
                    seen_plan_ids.insert(plan_id);
                }
                let plan = &selected.plan;
                plan_trace.rolling_combat_plans.retain(|current| {
                    current.operation_kind != operation_kind || current.anchor != operation_anchor
                });
                plan_trace
                    .rolling_combat_plans
                    .push(RollingCombatPlanTrace {
                        plan_id: selected.plan_id,
                        revision: selected.revision,
                        disposition: selected.disposition,
                        replan_reason: selected.reason,
                        operation_kind,
                        anchor: operation_anchor,
                        feasible: plan.feasible,
                        purchases: plan
                            .purchases
                            .iter()
                            .copied()
                            .map(|purchase| RollingPurchaseTrace {
                                unit_type: purchase.unit_type,
                                facility: purchase.facility,
                                build_turn: purchase.build_turn,
                                cost: purchase.cost,
                            })
                            .collect(),
                        targets: plan
                            .target_forecasts
                            .iter()
                            .map(|target| RollingTargetTrace {
                                entity: target.entity,
                                unit_type: target.unit_type,
                                available_turn: target.available_turn,
                                initial_hp: target.initial_hp,
                                remaining_hp: target.remaining_hp,
                                destroyed_turn: target.destroyed_turn,
                            })
                            .collect(),
                        turn_forecasts: plan
                            .turn_forecasts
                            .iter()
                            .map(|forecast| CampaignTurnForecastTrace {
                                turn: forecast.turn,
                                enemy_arrival_hp: forecast.enemy_arrival_hp,
                                enemy_hp_removed: forecast.enemy_hp_removed,
                                friendly_hp_lost: forecast.friendly_hp_lost,
                                attack_count: forecast.attack_count,
                            })
                            .collect(),
                        first_attack_turn: plan.first_attack_turn,
                        elimination_turn: plan.elimination_turn,
                        occupation_turn: plan.occupation_turn,
                        production_cost: plan.production_cost,
                        expected_loss: plan.expected_loss,
                        surviving_combat_value: plan.surviving_combat_value,
                        required_overmatch_value: plan.required_overmatch_value,
                        overmatch_ready: plan.overmatch_ready,
                        protected_unit_count: plan.protected_unit_count,
                        protected_survivor_count: plan.protected_survivor_count,
                        required_capture_survivor_count: plan.required_capture_survivor_count,
                        candidates_considered: plan.candidates_considered,
                        candidates_pruned: plan.candidates_pruned,
                        search_truncated: plan.search_truncated,
                    });
                if selected.plan_id.is_none() {
                    // 長期Expectedを今の資金で全滅できない場合でも、best-effortは
                    // 当手番に到達可能なscreenを返している。これを捨てると「敵増援を
                    // 多く見積もるほど1体も生産しない」逆転が起きる。
                    // PlanIdなしの購入は永続計画に固定せず、次手番の実敵・資金・
                    // 生産結果で必ず再評価する即時Combatとして発行する。
                    rolling_plans.insert(op_index, selected);
                    continue;
                }
                rolling_plans.insert(op_index, selected);
            }
        }
        let rolling_plan = rolling_plans.get(&op_index);
        let mut planned_purchase = None;
        let candidate = if slot_kind == SlotKind::Combat {
            rolling_plan.and_then(|plan| {
                plan.plan
                    .current_purchases()
                    .find(|purchase| {
                        !used_facilities.contains(&purchase.facility)
                            && purchase.cost <= spendable_funds
                    })
                    .map(|purchase| {
                        planned_purchase = Some(purchase);
                        SlotCandidate {
                            unit_type: purchase.unit_type,
                            cost: purchase.cost,
                            facility: purchase.facility,
                            fitness: 1.0,
                        }
                    })
            })
        } else {
            select_candidate(
                scan,
                &mut ctx,
                &operations[op_index],
                slot_kind,
                &used_facilities,
                CandidateConstraints {
                    remaining_funds: spendable_funds,
                    per_slot_budget,
                },
            )
        };

        let Some(candidate) = candidate else {
            // この枠を満たせる候補が無い場合は、枠の要求を落として次を探す
            clear_slot(&mut operations[op_index], slot_kind);
            let reserved = (slot_kind == SlotKind::Combat)
                .then_some(rolling_plan)
                .flatten()
                .filter(|plan| plan.plan_id.is_some())
                .and_then(|plan| {
                    plan.plan
                        .purchases
                        .iter()
                        .filter(|purchase| purchase.build_turn > 0)
                        .min_by_key(|purchase| purchase.build_turn)
                });
            plan_trace.steps.push(ProductionStepTrace {
                operation_kind,
                operation_anchor,
                slot_kind,
                deficit_before,
                deficit_after: deficit_before,
                remaining_funds_before,
                decision: reserved.map_or(ProductionDecision::SlotCleared, |purchase| {
                    ProductionDecision::Reserved {
                        unit_type: purchase.unit_type,
                        cost: purchase.cost,
                        build_turn: purchase.build_turn,
                    }
                }),
            });
            continue;
        };

        // 見送り購入: 一括編成が必要な作戦で、今買える範囲に適合候補が無く、
        // 数ターン待てばより適合する候補が買えるなら、資金を貯める。
        if slot_kind != SlotKind::Combat
            && should_defer_purchase(
                scan,
                &mut ctx,
                &operations[op_index],
                slot_kind,
                spendable_funds,
                candidate.cost,
            )
        {
            plan_trace.steps.push(ProductionStepTrace {
                operation_kind,
                operation_anchor,
                slot_kind,
                deficit_before,
                deficit_after: deficit_before,
                remaining_funds_before,
                decision: ProductionDecision::Deferred {
                    unit_type: candidate.unit_type,
                    cost: candidate.cost,
                },
            });
            break;
        }

        remaining_funds = remaining_funds.saturating_sub(candidate.cost);
        if planned_purchase.is_some() {
            issued_plan_purchase_cost = issued_plan_purchase_cost.saturating_add(candidate.cost);
        }
        used_facilities.insert(candidate.facility);
        facility_owners.insert(
            candidate.facility,
            operation_priority_rank(&operations[op_index]),
        );
        let mut deployment = planned_deployment(
            scan,
            &mut ctx,
            &operations[op_index],
            slot_kind,
            &candidate,
            None,
        );
        if slot_kind == SlotKind::Combat
            && let (Some(deployment), Some(plan)) = (deployment.as_mut(), rolling_plan)
        {
            deployment.forecast = deployment::DeploymentForecast {
                first_attack_turn: plan.plan.first_attack_turn,
                elimination_turn: plan.plan.elimination_turn,
                occupation_turn: plan.plan.occupation_turn,
                package_cost: plan.plan.production_cost,
                package_size: u32::try_from(plan.plan.purchases.len()).unwrap_or(u32::MAX),
            };
            deployment.plan_step = plan
                .plan_id
                .zip(plan.revision)
                .zip(planned_purchase)
                .and_then(|((plan_id, revision), purchase)| {
                    plan_registry.current_step_ref(plan_id, revision, turn, purchase)
                });
        }
        // Combatは同じパッケージの未使用current purchaseを次の反復で選ぶ。
        // 全て消費した後は候補なしとなり、この手番のCombat枠を完了する。
        if slot_kind != SlotKind::Combat {
            record_fill(
                scan,
                &mut ctx,
                &mut operations[op_index],
                slot_kind,
                &candidate,
            );
        }
        plan_trace.steps.push(ProductionStepTrace {
            operation_kind,
            operation_anchor,
            slot_kind,
            deficit_before,
            // 購入を反映した後の未充足率。ここが下がらない枠が同一ユニットを買い続ける。
            deficit_after: operations[op_index]
                .slots
                .deficit_ratio(slot_kind, &operations[op_index].filled),
            remaining_funds_before,
            decision: ProductionDecision::Produced {
                unit_type: candidate.unit_type,
                cost: candidate.cost,
                facility: candidate.facility,
            },
        });
        let capture_intent = (slot_kind == SlotKind::Capture)
            .then(|| {
                operations[op_index].island_id.map(|island_id| {
                    campaign_execution::CampaignProductionIntent {
                        command: ProduceUnitCommand {
                            player_id,
                            target_x: candidate.facility.x,
                            target_y: candidate.facility.y,
                            unit_type: candidate.unit_type,
                        },
                        island_id,
                        role: campaign_execution::CampaignProductionRole::Capture,
                    }
                })
            })
            .flatten();
        commands.push(PlannedProduction {
            command: ProduceUnitCommand {
                player_id,
                target_x: candidate.facility.x,
                target_y: candidate.facility.y,
                unit_type: candidate.unit_type,
            },
            deployment,
            capture_intent,
        });
    }

    // 永続Planの予約は「最低限ここまでは作る」というMustであり、手番の生産停止条件
    // ではない。未発行のMustと観測後counterだけを残し、残額・空き施設があれば
    // 実在する前線の敵へ交戦可能な増援を追加する。仮想増援や未接敵の敵はここに
    // 入れないので、敵見積の不確実さで汎用兵を量産することもない。
    let contingency_reserve = operations
        .iter()
        .map(|operation| operation.contingency_reserve_funds)
        .fold(0_u32, u32::saturating_add);
    let unissued_plan_reserve = plan_registry
        .reserved_purchase_cost(player_id)
        .saturating_sub(issued_plan_purchase_cost);
    let mut immediate_reinforcement_funds =
        remaining_funds.saturating_sub(unissued_plan_reserve.saturating_add(contingency_reserve));
    // 到達性と対敵有効度は、残額を工場へ配るたびに変わらない。候補表をここで一度だけ
    // 作り、以下のループでは資金・使用済み施設だけを更新する。
    // 待機線の有無だけで余剰Combatを一律停止しない。Stage戦力は既に
    // RollingPlanの到着予定付き既存戦力へ入っているため、不足が残るときだけ
    // ここへ到達する。実在敵への有効打・到着性・HP上限による既存の枝刈りで、
    // 同一出口への無目的な量産は防ぐ。
    let immediate_combat_options = immediate_combat_options(scan, &mut ctx, &operations, player_id);
    // 1手番に1体が与えられる攻撃は1回だけなので、同じ敵HPを複数の工場枠で
    // 仮想的に何度も消費しない。ここは経路探索済みの攻撃表への割当だけであり、
    // 「1体追加ごとの将来戦闘シミュレーション」にはしない。
    let mut immediately_committed_damage = HashMap::<(usize, Entity), u32>::new();
    while immediate_reinforcement_funds > 0 {
        let free_slots = scan
            .free_facilities
            .iter()
            .filter(|(pos, _)| !used_facilities.contains(pos))
            .count();
        if free_slots == 0 {
            break;
        }
        let per_slot_budget = immediate_reinforcement_funds / free_slots as u32;
        let Some((op_index, candidate, engagement)) = select_immediate_combat_reinforcement(
            &operations,
            &immediate_combat_options,
            &used_facilities,
            &immediately_committed_damage,
            CandidateConstraints {
                remaining_funds: immediate_reinforcement_funds,
                per_slot_budget,
            },
        ) else {
            break;
        };

        let operation_kind = operations[op_index].kind;
        let operation_anchor = operations[op_index].anchor;
        let remaining_funds_before = remaining_funds;
        let deployment = planned_deployment(
            scan,
            &mut ctx,
            &operations[op_index],
            SlotKind::Combat,
            &candidate,
            Some(engagement.entity),
        );
        // 選定時に有効な実Entityを少なくとも1体確認している。ここで任務が作れない
        // なら命令を出さず停止し、任務なしCombat Entityを生まない。
        let Some(deployment) =
            deployment.filter(|deployment| !deployment.priority_enemies.is_empty())
        else {
            break;
        };

        remaining_funds = remaining_funds.saturating_sub(candidate.cost);
        immediate_reinforcement_funds =
            immediate_reinforcement_funds.saturating_sub(candidate.cost);
        let committed_damage = immediately_committed_damage
            .entry((op_index, engagement.entity))
            .or_default();
        *committed_damage = committed_damage.saturating_add(engagement.damage);
        used_facilities.insert(candidate.facility);
        facility_owners.insert(
            candidate.facility,
            operation_priority_rank(&operations[op_index]),
        );
        plan_trace.steps.push(ProductionStepTrace {
            operation_kind,
            operation_anchor,
            slot_kind: SlotKind::Combat,
            // これはMust枠を埋める購入ではない。残額で前線圧を足すため、枠不足率は
            // 0のまま記録し、rolling planとの混同を避ける。
            deficit_before: 0.0,
            deficit_after: 0.0,
            remaining_funds_before,
            decision: ProductionDecision::ProducedImmediateReinforcement {
                unit_type: candidate.unit_type,
                cost: candidate.cost,
                facility: candidate.facility,
            },
        });
        commands.push(PlannedProduction {
            command: ProduceUnitCommand {
                player_id,
                target_x: candidate.facility.x,
                target_y: candidate.facility.y,
                unit_type: candidate.unit_type,
            },
            deployment: Some(deployment),
            capture_intent: None,
        });
    }

    plan_registry.reconcile_unseen_plans(player_id, turn, &seen_plan_ids);
    plan_trace.leftover_funds = remaining_funds;
    plan_trace.reserved_funds =
        remaining_funds.min(unissued_plan_reserve.saturating_add(contingency_reserve));
    plan_trace.uncommitted_funds = remaining_funds.saturating_sub(plan_trace.reserved_funds);
    (commands, plan_trace)
}

/// 未確保のDAG区間が実在敵と交戦中なら、未許可の首都親作戦の構造枠を保留する。
///
/// 親の予約は「将来の首都占領」を保証するものであり、現在の区間を突破するCombat
/// 増援より優先するMustではない。ここで親の構造枠だけを保留すると、残額・空き工場は
/// 下段の即時増援選択へ渡り、到達性・与ダメージ・被害交換で現在前線へ投資される。
fn defer_unapproved_capital_structure_for_active_segment(operations: &mut [Operation]) {
    let active_segment_under_pressure = operations.iter().any(|operation| {
        operation.kind == OperationKind::Capture && operation.facts.enemy_combat_units > 0
    });
    if !active_segment_under_pressure {
        return;
    }
    for operation in operations {
        if operation.kind == OperationKind::AssaultCapital && !operation.execution_authorized {
            operation.slots.capture_units = 0;
            operation.slots.transport_slots = 0;
        }
    }
}

/// 見積に含めてよいのは、同じ永続Planへ実際に配属済みのEntityだけ。
fn committed_entities_for_plan(
    assignments: Option<&HashMap<Entity, deployment::ActiveTargetAssignment>>,
    continuation_plan_id: Option<plan_revision::PlanId>,
) -> HashSet<Entity> {
    assignments
        .into_iter()
        .flat_map(|assignments| assignments.iter())
        .filter_map(|(&entity, assignment)| {
            let belongs_to_continuation =
                assignment.plan_id == continuation_plan_id && assignment.plan_id.is_some();
            let is_operation_bound_immediate_combat =
                assignment.plan_id.is_none() && assignment.slot_kind == SlotKind::Combat;
            (belongs_to_continuation || is_operation_bound_immediate_combat).then_some(entity)
        })
        .collect()
}

/// 敵が保持する生産施設と収入から、作戦地点へ期限内に到着できる増援列を作る。
///
/// 現在数へ固定値を足すのではなく、各手番の資金、facility slot、生産可能兵種、移動ETAを
/// 同じ時間軸へ置く。敵の現在資金は非公開なので0から始め、将来収入だけを使う。
/// Expectedは通常の継続戦力、Stressは観測後counterの再計画対象として用途を分ける。
#[derive(Debug, Default)]
struct ReinforcementAssessment {
    reinforcements: Vec<EnemyPlanUnit>,
    contingencies: Vec<ReinforcementContingency>,
}

/// Expectedは継続生産で倒す対象、Stressは予備counterと再計画の対象として扱う。
/// どちらも進撃Goを決めないため、最小のGo条件を強めずに生産だけを強くできる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReinforcementScenario {
    Expected,
    Stress,
}

/// 敵増援ごとに接触turnと、観測後に生産する最速counterの接触turnを比較する。
/// counterが間に合う仮説は現在の撃破対象へ混ぜず、条件付き予約として保持する。
fn enemy_reinforcement_assessment(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    horizon: u32,
    scenario_budget: u32,
    scenario: ReinforcementScenario,
) -> ReinforcementAssessment {
    if scenario_budget == 0 {
        return ReinforcementAssessment::default();
    }
    let mut budget = 0_u32;
    let mut funded = 0_u32;
    let mut assessment = ReinforcementAssessment::default();
    let mut reserved_counter_slots = HashSet::new();
    let friendly_combat_types = scan
        .available_types
        .iter()
        .filter(|(_, stats)| !stats.can_capture && stats.max_cargo == 0)
        .map(|(_, stats)| stats)
        .collect::<Vec<_>>();
    let observed_enemy_types = op
        .reachable_threats
        .iter()
        .map(|threat| threat.stats.unit_type)
        .collect::<HashSet<_>>();
    // 観測済みの局地兵種と直近生産の主兵種をExpectedの候補集合にする。
    // dominant一種類だけへ絞ると、敵の航空・地上混成を一方の相性だけで見積もり、
    // 対空や占領阻止を後追いにするため、少なくとも観測兵種の混成を維持する。
    let mut expected_enemy_types = observed_enemy_types.clone();
    if let Some(unit_type) = scan.enemy_production_forecast.dominant_unit_type {
        expected_enemy_types.insert(unit_type);
    }

    let last_build_turn = match scenario {
        ReinforcementScenario::Expected => {
            EXPECTED_REINFORCEMENT_WAVES.min(horizon.saturating_sub(1))
        }
        ReinforcementScenario::Stress => horizon.saturating_sub(1),
    };
    for build_turn in 1..=last_build_turn {
        let mut projected_types = HashSet::new();
        let income = scan
            .enemy_income
            .min(scenario_budget.saturating_sub(funded));
        budget = budget.saturating_add(income);
        funded = funded.saturating_add(income);
        for facility in &scan.enemy_facilities {
            let mut candidates = scan
                .available_types
                .iter()
                .filter(|(unit_type, stats)| {
                    stats.max_cargo == 0
                        && stats.cost > 0
                        && stats.cost <= budget
                        // Expectedは観測済み混成と生産履歴の主兵種を交互に見積もる。
                        // 観測が無い場合だけ、到達可能な兵種から通常の前線圧を選ぶ。
                        && (scenario == ReinforcementScenario::Stress
                            || expected_enemy_types.is_empty()
                            || expected_enemy_types.contains(&stats.unit_type)
                            || stats.can_capture)
                        && scan.can_produce(facility.terrain, *unit_type)
                        && ctx.is_reachable(
                            &scan.map,
                            &scan.master_data,
                            (facility.pos.x, facility.pos.y),
                            (op.anchor.x, op.anchor.y),
                            stats.movement_type,
                        )
                })
                .filter_map(|(_, stats)| {
                    let eta = eta_turns(&scan.map, &facility.pos, &op.anchor, stats.max_movement);
                    let available_turn = build_turn.saturating_add(1).saturating_add(eta);
                    if available_turn > horizon {
                        return None;
                    }
                    let counter_damage = friendly_combat_types
                        .iter()
                        .map(|friendly| {
                            best_damage(&scan.damage_chart, stats.unit_type, friendly.unit_type)
                        })
                        .max()
                        .unwrap_or_default();
                    let can_be_engaged = friendly_combat_types.iter().any(|friendly| {
                        best_damage(&scan.damage_chart, friendly.unit_type, stats.unit_type) > 0
                    });
                    // Expectedでは、現在の自軍カタログに対抗兵種が無いこと自体が
                    // 生産計画で解くべき不足である。ここで敵候補を捨てると
                    // 「まだcounterを持たないから敵もいない」と誤認してしまう。
                    (scenario == ReinforcementScenario::Expected || can_be_engaged).then_some((
                        std::cmp::Reverse(counter_damage),
                        std::cmp::Reverse(u32::from(stats.can_capture)),
                        std::cmp::Reverse(stats.cost),
                        available_turn,
                        stats,
                    ))
                })
                .collect::<Vec<_>>();
            // 前線争奪では施設を奪える増援の頭数を最初にstress testする。
            // 最大単発火力を先にすると、高価な一体だけを仮定して安価な占領兵の
            // 連続生産を見落とすため、占領能力→対友軍火力→費用の順にする。
            candidates.sort_by_key(|candidate| {
                let stats = candidate.4;
                (
                    std::cmp::Reverse(
                        expected_enemy_types.contains(&stats.unit_type)
                            && !projected_types.contains(&stats.unit_type),
                    ),
                    candidate.1,
                    candidate.0,
                    candidate.2,
                )
            });
            let Some((_, _, _, available_turn, stats)) = candidates.into_iter().next() else {
                continue;
            };
            projected_types.insert(stats.unit_type);
            budget = budget.saturating_sub(stats.cost);
            let reinforcement = EnemyPlanUnit {
                entity: None,
                stats: stats.clone(),
                // ETA到達後は作戦anchorに接触するものとして交戦時間を見積もる。
                position: op.anchor,
                hp: 100,
                defense_bonus: scan
                    .map
                    .get_terrain(op.anchor.x, op.anchor.y)
                    .map_or(0, |terrain| {
                        scan.master_data.get_terrain_defense_bonus(terrain)
                    }),
                available_turn,
            };
            // Expectedは通常想定される到着戦力をすべてRollingPlanへ渡す。これが
            // 「最低Goは満たしたから生産終了」を防ぐ継続戦力の基準になる。
            // Stressだけは観測後に間に合うcounterを条件付き計画として残す。
            if scenario == ReinforcementScenario::Expected
                || stats.can_capture
                || observed_enemy_types.contains(&stats.unit_type)
            {
                assessment.reinforcements.push(reinforcement);
            } else if let Some(contingency) = fastest_observed_counter(
                scan,
                ctx,
                op.anchor,
                stats,
                build_turn,
                available_turn,
                &reserved_counter_slots,
            ) {
                reserved_counter_slots
                    .insert((contingency.counter_build_turn, contingency.counter_facility));
                assessment.contingencies.push(contingency);
            } else {
                assessment.reinforcements.push(reinforcement);
            }
        }
    }
    assessment
}

/// 観測した敵1体へ、生産slot・移動・与ダメージを満たす最速counterを返す。
fn fastest_observed_counter(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    anchor: GridPosition,
    enemy: &UnitStats,
    counter_build_turn: u32,
    enemy_contact_turn: u32,
    reserved_slots: &HashSet<(u32, GridPosition)>,
) -> Option<ReinforcementContingency> {
    scan.production_facilities
        .iter()
        .flat_map(|(facility, terrain)| {
            scan.available_types
                .iter()
                .filter(move |(unit_type, stats)| {
                    !stats.can_capture
                        && stats.max_cargo == 0
                        && scan.can_produce(*terrain, *unit_type)
                })
                .map(move |(_, stats)| (*facility, stats))
        })
        .filter(|(facility, stats)| {
            !reserved_slots.contains(&(counter_build_turn, *facility))
                && best_damage(&scan.damage_chart, stats.unit_type, enemy.unit_type) > 0
                && ctx.is_reachable(
                    &scan.map,
                    &scan.master_data,
                    (facility.x, facility.y),
                    (anchor.x, anchor.y),
                    stats.movement_type,
                )
        })
        .filter_map(|(facility, stats)| {
            let damage = best_damage(&scan.damage_chart, stats.unit_type, enemy.unit_type);
            let attacks_required = 100_u32.div_ceil(damage.max(1));
            let counter_contact_turn = counter_build_turn
                .saturating_add(1)
                .saturating_add(eta_turns(&scan.map, &facility, &anchor, stats.max_movement));
            (counter_contact_turn <= enemy_contact_turn).then_some((
                counter_contact_turn,
                attacks_required,
                stats.cost,
                facility,
                stats,
            ))
        })
        .min_by_key(|(contact, attacks, cost, facility, _)| {
            (*contact, *attacks, *cost, facility.y, facility.x)
        })
        .map(
            |(counter_contact_turn, attacks_required, reserve_cost, facility, stats)| {
                ReinforcementContingency {
                    enemy_type: enemy.unit_type,
                    enemy_contact_turn,
                    counter_type: stats.unit_type,
                    counter_facility: facility,
                    counter_build_turn,
                    counter_contact_turn,
                    attacks_required,
                    reserve_cost,
                }
            },
        )
}

/// 将来収入で賄えない累積counter費用だけを、現在残高から予約する。
///
/// 未観測増援を予約しない現在のV4では、旧対比テスト専用の補助関数である。
#[cfg(test)]
fn contingency_reserve_now(
    contingencies: &[ReinforcementContingency],
    income_per_turn: u32,
) -> u32 {
    let mut due_by_turn = HashMap::<u32, u32>::new();
    for contingency in contingencies {
        due_by_turn
            .entry(contingency.counter_build_turn)
            .and_modify(|cost| *cost = cost.saturating_add(contingency.reserve_cost))
            .or_insert(contingency.reserve_cost);
    }
    let mut turns = due_by_turn.keys().copied().collect::<Vec<_>>();
    turns.sort_unstable();
    let mut cumulative_cost = 0_u32;
    turns.into_iter().fold(0_u32, |required_now, turn| {
        cumulative_cost = cumulative_cost.saturating_add(due_by_turn[&turn]);
        required_now.max(cumulative_cost.saturating_sub(income_per_turn.saturating_mul(turn)))
    })
}

/// 観測敵と到着しうる増援を排除できる混成生産列を、探索期間の全生産slotから計画する。
///
/// `combat_plan_required`は呼び出し条件にだけ残し、候補数・生産停止・完了判定には使わない。
/// 既存unitと同じ手番に発注済みのunitを初期編成へ入れ、敵EntityのHPが0になるまで
/// ターン単位で攻撃を進めた結果から必要な購入だけを返す。
#[allow(clippy::too_many_arguments)]
fn combat_plan_input(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    used_facilities: &HashSet<GridPosition>,
    committed_combat_entities: &HashSet<Entity>,
    staged_combat_entities: &HashSet<Entity>,
    remaining_funds: u32,
    require_self_deployment: bool,
) -> Option<RollingPlanInput> {
    let enemies: Vec<_> = op
        .reachable_threats
        .iter()
        .filter(|threat| threat.current_hp > 0)
        .map(|threat| {
            let terrain = scan
                .map
                .get_terrain(threat.position.x, threat.position.y)
                .unwrap_or(Terrain::Plains);
            EnemyPlanUnit {
                entity: threat.entity,
                stats: threat.stats.clone(),
                position: threat.position,
                hp: threat.current_hp,
                defense_bonus: scan.master_data.get_terrain_defense_bonus(terrain),
                available_turn: threat.available_turn,
            }
        })
        .collect();
    let hard_deadline =
        if op.kind == OperationKind::Defense && op.facts.enemy_contact_eta != u32::MAX {
            Some(op.facts.enemy_contact_eta.max(1))
        } else {
            None
        };
    let campaign_objective = scan.campaign_objectives.iter().find(|objective| {
        objective.anchor == op.anchor || op.objective_properties.contains(&objective.anchor)
    });
    let mut capture_completion_turn =
        campaign_objective.and_then(|objective| objective.capture_eta);
    let required_capture_survivors = campaign_objective.map_or_else(
        || {
            op.objective_properties
                .iter()
                .filter(|property| scan.open_properties.contains(property))
                .count()
        },
        |objective| objective.required_capture_survivors,
    );
    let mut enemies = enemies;
    enemies.extend(op.expected_reinforcements.iter().cloned());
    if enemies.is_empty() {
        return None;
    }

    let mut existing_units = Vec::new();
    for unit in &scan.my_units {
        if unit.stats.max_cargo > 0 {
            continue;
        }
        let Some(entity) = unit.entity else {
            continue;
        };
        let committed = committed_combat_entities.contains(&entity);
        let staged = staged_combat_entities.contains(&entity);
        if !committed && !staged {
            continue;
        }
        let engageable_enemy_indices = enemies
            .iter()
            .enumerate()
            .filter_map(|(index, enemy)| {
                (best_damage(
                    &scan.damage_chart,
                    unit.stats.unit_type,
                    enemy.stats.unit_type,
                ) > 0
                    && ctx.can_reach_engagement_envelope(
                        &scan.map,
                        &scan.master_data,
                        (unit.pos.x, unit.pos.y),
                        (enemy.position.x, enemy.position.y),
                        unit.stats.movement_type,
                        unit.stats.max_range,
                    ))
                .then_some(index)
            })
            .collect::<Vec<_>>();
        if engageable_enemy_indices.is_empty() {
            continue;
        }
        existing_units.push(FriendlyPlanUnit {
            stats: unit.stats.clone(),
            position: unit.pos,
            hp: unit.hp,
            // Stageは前線にいることを仮定しない。最短移動ETAの後にだけ戦闘へ
            // 寄与する既存戦力として評価し、Advance部隊と同じ手番の火力にはしない。
            available_turn: if staged {
                eta_turns(&scan.map, &unit.pos, &op.anchor, unit.stats.max_movement).max(1)
            } else {
                0
            },
            engageable_enemy_indices,
        });
    }
    let mut protected_units = scan
        .my_units
        .iter()
        .filter(|unit| {
            unit.entity
                .is_some_and(|entity| op.protected_capture_entities.contains(&entity))
        })
        .map(|unit| FriendlyPlanUnit {
            stats: unit.stats.clone(),
            position: unit.pos,
            hp: unit.hp,
            available_turn: 0,
            // 保護対象は攻撃要員として二重計上しないため空にする。
            engageable_enemy_indices: Vec::new(),
        })
        .collect::<Vec<_>>();

    // この生産判断内で先に発注した専任CaptureだけはまだEntityになっていないため、
    // 実行中の保護対象へ投影する。将来手番の仮想Captureはここでは予約しない。
    // Combat枠で選ぶ歩兵・重歩兵も生存占領能力へ数えるため、将来費用を先取りせず
    // 現在の空き施設・現金を前線戦力へ使える。
    let missing_protected_units = required_capture_survivors.saturating_sub(protected_units.len());
    let newly_ordered_capture_units = usize::try_from(op.filled.capture_units)
        .unwrap_or(usize::MAX)
        .saturating_sub(protected_units.len())
        .min(missing_protected_units);
    let reference_capture = scan.reference_capture_unit().cloned();
    if let Some(reference_capture) = reference_capture.as_ref() {
        let targets = if op.objective_properties.is_empty() {
            std::slice::from_ref(&op.anchor)
        } else {
            op.objective_properties.as_slice()
        };
        for index in 0..newly_ordered_capture_units {
            let target = targets[index % targets.len()];
            let movement_turns = if op.facts.requires_transport {
                op.facts.deploy_lead_time.max(1)
            } else {
                scan.production_facilities
                    .iter()
                    .map(|(facility, _)| {
                        eta_turns(&scan.map, facility, &target, reference_capture.max_movement)
                    })
                    .min()
                    .unwrap_or(op.facts.deploy_lead_time)
            };
            protected_units.push(FriendlyPlanUnit {
                stats: reference_capture.clone(),
                position: target,
                hp: 100,
                available_turn: 1_u32.saturating_add(movement_turns),
                engageable_enemy_indices: Vec::new(),
            });
        }
        if capture_completion_turn.is_none()
            && required_capture_survivors > 0
            && protected_units.len() >= required_capture_survivors
        {
            capture_completion_turn = protected_units
                .iter()
                .map(|unit| unit.available_turn)
                .max()
                .map(|arrival| arrival.saturating_add(CAPTURE_COMPLETION_TURNS));
        }
    }

    // 局地Captureで首都戦用の12手番すべての施設×兵種を展開すると、敵が数体
    // 現れただけでbeam候補が急増する。局地パッケージは占領完了ETAまでに投入可能な
    // 生産枠だけを比較し、長期の全軍編成はAssaultCapital側へ分離する。
    let planning_horizon = match op.kind {
        OperationKind::Capture => capture_completion_turn
            .unwrap_or(op.threat_horizon)
            // 弾薬1の航空機などは複数の生産波が必要になる。局地ETAが短くても
            // 6手番までは候補を残し、それ以降の全軍編成だけを首都作戦へ分離する。
            .clamp(6, DEFAULT_SEARCH_TURNS),
        _ => hard_deadline.unwrap_or(DEFAULT_SEARCH_TURNS).max(1),
    };
    let mut options = production_options(
        &scan.free_facilities,
        &scan.production_facilities,
        &scan.available_types,
        &scan.master_data,
        planning_horizon,
        |facility, stats| {
            let can_engage = enemies.iter().any(|enemy| {
                best_damage(&scan.damage_chart, stats.unit_type, enemy.stats.unit_type) > 0
                    && ctx.can_reach_engagement_envelope(
                        &scan.map,
                        &scan.master_data,
                        (facility.x, facility.y),
                        (enemy.position.x, enemy.position.y),
                        stats.movement_type,
                        stats.max_range,
                    )
            });
            if require_self_deployment {
                return can_engage;
            }
            can_engage
                || can_join_operation(
                    scan,
                    ctx,
                    &op.anchor,
                    op.facts.requires_transport,
                    &facility,
                    stats,
                )
        },
    );
    // この手番に既に使った施設は将来手番には再利用できるが、build_turn=0では使えない。
    options.retain(|option| {
        option.purchase.build_turn > 0 || !used_facilities.contains(&option.purchase.facility)
    });
    for option in &mut options {
        option.engageable_enemy_indices = enemies
            .iter()
            .enumerate()
            .filter_map(|(index, enemy)| {
                (best_damage(
                    &scan.damage_chart,
                    option.stats.unit_type,
                    enemy.stats.unit_type,
                ) > 0
                    && ctx.can_reach_engagement_envelope(
                        &scan.map,
                        &scan.master_data,
                        (option.purchase.facility.x, option.purchase.facility.y),
                        (enemy.position.x, enemy.position.y),
                        option.stats.movement_type,
                        option.stats.max_range,
                    ))
                .then_some(index)
            })
            .collect();
    }

    Some(RollingPlanInput {
        map: scan.map.clone(),
        master_data: scan.master_data.clone(),
        damage_chart: scan.damage_chart.clone(),
        existing_units,
        protected_units,
        enemies,
        production_options: options,
        current_funds: remaining_funds,
        income_per_turn: scan.my_income,
        hard_deadline,
        capture_completion_turn,
        required_capture_survivors,
        delay_cost_per_turn: op.facts.target_property_count.max(1).saturating_mul(1_000),
        search_beam_width: if op.kind == OperationKind::Capture {
            32
        } else {
            rolling_plan::SEARCH_BEAM_WIDTH
        },
    })
}

/// 生産された戦闘Entityへ、実HPと与ダメージから撃破順を与える。
///
/// 価格は敵の硬さでも攻撃能力でもないため使わない。占領・輸送能力を持つ敵を先にし、
/// 同分類では必要攻撃回数が少ない敵から集中撃破する。
fn planned_deployment(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    slot_kind: SlotKind,
    candidate: &SlotCandidate,
    preferred_enemy: Option<Entity>,
) -> Option<PlannedDeployment> {
    let stats = candidate_stats(scan, candidate);
    let (threats, eligible): (&[ThreatTarget], Vec<usize>) = match slot_kind {
        SlotKind::Intercept => (
            &op.unreachable_threats,
            (0..op.unreachable_threats.len()).collect(),
        ),
        SlotKind::Combat => {
            // Combatは標的位置ではなく射撃圏へ自力展開する。艦船を陸上anchorへ
            // 仮置きすると到達判定が再び失敗するため、生産施設を常に起点とする。
            let origin = candidate.facility;
            (
                &op.reachable_threats,
                reachable_threat_indices(scan, ctx, &op.reachable_threats, origin, stats),
            )
        }
        SlotKind::Capture | SlotKind::Transport => return None,
    };
    let mut targets = eligible
        .into_iter()
        .filter_map(|index| {
            let threat = &threats[index];
            let entity = threat.entity?;
            let damage = best_damage(&scan.damage_chart, stats.unit_type, threat.stats.unit_type);
            let attacks_to_destroy = threat.current_hp.div_ceil(damage.max(1));
            let strategic_class = if threat.stats.can_capture {
                0
            } else if threat.stats.max_cargo > 0 {
                1
            } else {
                2
            };
            let incoming_damage =
                best_damage(&scan.damage_chart, threat.stats.unit_type, stats.unit_type);
            (damage > 0 && threat.current_hp > 0).then_some((
                usize::from(preferred_enemy.is_some_and(|preferred| preferred != entity)),
                std::cmp::Reverse(incoming_damage),
                strategic_class,
                attacks_to_destroy,
                threat.position.x,
                threat.position.y,
                entity.to_bits(),
                entity,
            ))
        })
        .collect::<Vec<_>>();
    targets.sort_unstable_by_key(|target| {
        (
            target.0, target.1, target.2, target.3, target.4, target.5, target.6,
        )
    });
    let priority_enemies = targets.into_iter().map(|target| target.7).collect();
    Some(PlannedDeployment {
        anchor: op.anchor,
        staging_anchor: op.staging_anchor,
        posture: if op.kind == OperationKind::AssaultCapital {
            deployment::DeploymentPosture::Forming
        } else {
            deployment::DeploymentPosture::Execute
        },
        slot_kind,
        priority_enemies,
        threat_horizon: op.threat_horizon,
        forecast: deployment::DeploymentForecast::default(),
        plan_step: None,
    })
}

/// 次に埋めるべき枠を返す。
///
/// 2 段階で選ぶ。要求が有限の枠（前提条件）を全作戦ぶん先に満たし、
/// そのうえで残額を要求が青天井の撃破枠へ注ぎ込む。
/// 有限要求と青天井要求を同じ土俵で比べてはならない（`SlotTier` 参照）。
fn most_starved_slot(operations: &[Operation]) -> Option<(usize, SlotKind)> {
    most_starved_in_tier(operations, SlotTier::Prerequisite)
        .or_else(|| most_starved_in_tier(operations, SlotTier::Residual))
}

/// 生産枠の実効優先度。すべて同じ作戦集合で比較し、別枠の緊急作戦は作らない。
/// Defenseは敵接触ETAと展開リードタイムから優先度を上げるが、必要枠だけを取得する。
fn operation_priority_rank(operation: &Operation) -> u32 {
    let defense_cannot_wait = operation.kind == OperationKind::Defense
        && operation.facts.enemy_combat_units > 0
        && operation.facts.enemy_contact_eta
            <= operation.facts.deploy_lead_time.max(1).saturating_add(1);
    match (
        operation.kind,
        operation.execution_authorized,
        defense_cannot_wait,
    ) {
        (OperationKind::Defense, _, true) => 0,
        (OperationKind::AssaultCapital, true, _) => 1,
        (OperationKind::Defense, _, false) => 2,
        (OperationKind::Capture, _, _) => 3,
        (OperationKind::AssaultCapital, false, _) => 4,
    }
}

/// 指定した段階の中で最も飢えた枠を返す。
fn most_starved_in_tier(operations: &[Operation], tier: SlotTier) -> Option<(usize, SlotKind)> {
    let mut best: Option<(usize, SlotKind, u32, f32, usize)> = None;
    for (index, op) in operations.iter().enumerate() {
        for (priority, kind) in SLOT_PRIORITY.iter().enumerate() {
            let deficit = op.slots.tier_deficit(*kind, &op.filled, tier);
            if deficit <= 0.0 {
                continue;
            }
            let operation_rank = match tier {
                SlotTier::Prerequisite => operation_priority_rank(op),
                SlotTier::Residual => 0,
            };
            // 同じ作戦優先度なら、固定された役割順で全Captureを埋めるのではなく
            // 未充足率が最大の枠を選ぶ。最初の占領兵を確保した後はCombat計画が
            // 先に100%不足となるため、掃討波を形成してから残りの占領前線を広げる。
            // 未充足率が同じ場合だけ輸送→占領→戦闘という依存順を使う。
            let better = best.is_none_or(|(_, _, best_rank, best_deficit, best_priority)| {
                operation_rank < best_rank
                    || (operation_rank == best_rank
                        && (deficit.total_cmp(&best_deficit).is_gt()
                            || (deficit.total_cmp(&best_deficit).is_eq()
                                && priority < best_priority)))
            });
            if better {
                best = Some((index, *kind, operation_rank, deficit, priority));
            }
        }
    }
    best.map(|(index, kind, _, _, _)| (index, kind))
}

/// 満たせないと判明した枠の要求を消す（無限ループ防止）。
fn clear_slot(op: &mut Operation, kind: SlotKind) {
    match kind {
        SlotKind::Intercept => op.slots.intercept_units = 0,
        SlotKind::Transport => op.slots.transport_slots = 0,
        SlotKind::Capture => op.slots.capture_units = 0,
        SlotKind::Combat => {
            op.slots.combat_plan_required = 0;
        }
    }
}

/// 購入した 1 体分を充足量へ反映する。
fn record_fill(
    scan: &BoardScan,
    _ctx: &mut ReachCtx,
    op: &mut Operation,
    kind: SlotKind,
    candidate: &SlotCandidate,
) {
    let cargo = scan
        .available_types
        .iter()
        .find(|(unit_type, _)| *unit_type == candidate.unit_type)
        .map(|(_, stats)| stats.max_cargo)
        .unwrap_or(0);
    match kind {
        SlotKind::Intercept => {
            op.filled.intercept_units = op.filled.intercept_units.saturating_add(1);
        }
        SlotKind::Transport => {
            op.filled.transport_slots = op.filled.transport_slots.saturating_add(cargo.max(1))
        }
        SlotKind::Capture => op.filled.capture_units += 1,
        SlotKind::Combat => unreachable!("Combat購入はRollingPlan経路だけで処理する"),
    }
}

/// 候補の能力値を生産可能一覧から復元する。候補は同じ一覧から生成されるため必ず存在する。
fn candidate_stats<'a>(scan: &'a BoardScan, candidate: &SlotCandidate) -> &'a UnitStats {
    scan.available_types
        .iter()
        .find(|(unit_type, _)| *unit_type == candidate.unit_type)
        .map(|(_, stats)| stats)
        .expect("生産候補は生産可能ユニット一覧に存在する")
}

/// 指定地点から実際に交戦できる脅威の添字を返す。
fn reachable_threat_indices(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    threats: &[ThreatTarget],
    origin: GridPosition,
    stats: &UnitStats,
) -> Vec<usize> {
    threats
        .iter()
        .enumerate()
        .filter(|(_, threat)| {
            ctx.can_reach_engagement_envelope(
                &scan.map,
                &scan.master_data,
                (origin.x, origin.y),
                (threat.position.x, threat.position.y),
                stats.movement_type,
                stats.max_range,
            )
        })
        .map(|(index, _)| index)
        .collect()
}

fn threats_have_counter(
    unit: &UnitStats,
    threats: &[ThreatTarget],
    eligible_indices: &[usize],
    chart: &DamageChart,
) -> bool {
    eligible_indices
        .iter()
        .any(|index| best_damage(chart, unit.unit_type, threats[*index].stats.unit_type) > 0)
}

/// 構造枠・迎撃枠の候補を、実能力と購入費のROIで比較する。
/// Combatはこの関数を通らず、RollingPlanが具体的な戦闘scheduleを比較する。
fn normalized_candidate_fitness(
    kind: SlotKind,
    raw_fitness: f32,
    cost: u32,
    per_slot_budget: u32,
) -> f32 {
    let _ = (kind, per_slot_budget);
    let opportunity_cost = cost.max(1);
    raw_fitness * 1000.0 / opportunity_cost as f32
}

/// 指定の枠を満たす最良の候補を選ぶ。
fn select_candidate(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    kind: SlotKind,
    used_facilities: &HashSet<GridPosition>,
    constraints: CandidateConstraints,
) -> Option<SlotCandidate> {
    let mut best: Option<SlotCandidate> = None;
    let mut best_over_budget: Option<SlotCandidate> = None;

    for (facility, terrain) in &scan.free_facilities {
        if used_facilities.contains(facility) {
            continue;
        }
        for (unit_type, stats) in &scan.available_types {
            if !scan.can_produce(*terrain, *unit_type) {
                continue;
            }
            let Some(fitness) = slot_fitness(scan, ctx, op, kind, facility, stats) else {
                continue;
            };
            if stats.cost == 0 || stats.cost > constraints.remaining_funds {
                continue;
            }
            // CombatはRollingPlan専用で、この候補選定へ入らない。残る枠はいずれも
            // 体数・輸送容量なので、同じ実能力なら安い方を選ぶ。
            let count_denominated = true;
            let candidate = SlotCandidate {
                unit_type: *unit_type,
                cost: stats.cost,
                facility: *facility,
                fitness: normalized_candidate_fitness(
                    kind,
                    fitness,
                    stats.cost,
                    constraints.per_slot_budget,
                ),
            };
            let slot = if count_denominated && stats.cost > constraints.per_slot_budget.max(1) {
                &mut best_over_budget
            } else {
                &mut best
            };
            let better = slot.is_none_or(|current| {
                if count_denominated {
                    // 同性能なら安い方が多く揃う
                    (candidate.fitness, current.cost) > (current.fitness, candidate.cost)
                } else {
                    // 枠が制約なので、同性能なら大きい方を投入する
                    (candidate.fitness, candidate.cost) > (current.fitness, current.cost)
                }
            });
            if better {
                *slot = Some(candidate);
            }
        }
    }

    // 1 枠あたり予算に収まる候補を優先し、無ければ予算超過でも買える候補を使う
    best.or(best_over_budget)
}

/// 観測済み前線へ届くCombat増援の候補表を、各手番に一度だけ作る。
///
/// RollingPlanのように候補ごとに将来ターンを探索し直すのではなく、各候補について
/// (1) 実Entityへ交戦圏まで到達できるか、(2) 前線全体の残HPをどれだけ削れるか、
/// (3) その敵が占領・輸送を行えるか、を一度に集約して比較する。予約済みの将来敵は
/// 含めないため、推測量を勝利用の余剰生産へ取り違えない。
fn immediate_combat_options(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    operations: &[Operation],
    player_id: PlayerId,
) -> Vec<ImmediateCombatOption> {
    let mut options = Vec::new();
    let unit_positions = scanned_occupants(scan, player_id);

    for (op_index, operation) in operations.iter().enumerate() {
        // 実Entityが現在の前線へ出ていない作戦は、余剰Combatの送り先にしない。
        if !operation
            .reachable_threats
            .iter()
            .any(|threat| threat.entity.is_some() && threat.available_turn == 0)
        {
            continue;
        }
        for (facility, terrain) in &scan.free_facilities {
            for (unit_type, stats) in &scan.available_types {
                if !scan.can_produce(*terrain, *unit_type) || stats.cost == 0 {
                    continue;
                }
                let engagements = immediate_combat_engagements(
                    scan,
                    ctx,
                    operation,
                    facility,
                    stats,
                    player_id,
                    &unit_positions,
                );
                if engagements.is_empty() {
                    continue;
                }
                options.push(ImmediateCombatOption {
                    operation_index: op_index,
                    candidate: SlotCandidate {
                        unit_type: *unit_type,
                        cost: stats.cost,
                        facility: *facility,
                        // 即時増援では資金より工場枠が先に尽きる。価格あたりのダメージで
                        // 正規化すると、余剰資金を抱えたまま歩兵だけを量産するため、
                        // 同じ1枠が前線全体へ与える実効ダメージを主評価にする。
                        fitness: 0.0,
                    },
                    engagements,
                });
            }
        }
    }

    options
}

/// Mustを確保した後の残額を、作成済み候補表から最も有効なCombat増援へ割り当てる。
///
/// 残額と使用済み施設だけが購入ごとに変わる。到達性・敵HP・対敵ダメージは候補表へ
/// 固定済みなので、工場数に比例して経路探索を繰り返さない。
fn select_immediate_combat_reinforcement(
    operations: &[Operation],
    options: &[ImmediateCombatOption],
    used_facilities: &HashSet<GridPosition>,
    committed_damage: &HashMap<(usize, Entity), u32>,
    constraints: CandidateConstraints,
) -> Option<(usize, SlotCandidate, ImmediateCombatEngagement)> {
    let mut within_budget: Option<(usize, SlotCandidate, ImmediateCombatEngagement)> = None;
    let mut over_budget: Option<(usize, SlotCandidate, ImmediateCombatEngagement)> = None;
    for option in options {
        let op_index = option.operation_index;
        let candidate = option.candidate;
        if used_facilities.contains(&candidate.facility)
            || candidate.cost > constraints.remaining_funds
        {
            continue;
        }
        // 候補ごとに「まだ割り当てられていない実敵HP」への最大の1攻撃を選ぶ。
        // ここで対象を一つに絞ることで、単体が前線全員を同時に攻撃できるかのような
        // 合算スコアを防ぎ、同手番の複数工場も別の敵へ自然に分散できる。
        let Some(engagement) = option
            .engagements
            .iter()
            .filter_map(|engagement| {
                let already_committed = committed_damage
                    .get(&(op_index, engagement.entity))
                    .copied()
                    .unwrap_or_default();
                let remaining_hp = engagement.current_hp.saturating_sub(already_committed);
                (remaining_hp > 0).then_some((*engagement, remaining_hp))
            })
            .map(|(engagement, remaining_hp)| {
                let usable_damage = engagement.damage.min(remaining_hp);
                let marginal_fitness =
                    engagement.fitness * usable_damage as f32 / engagement.damage.max(1) as f32;
                (engagement, marginal_fitness)
            })
            .max_by(|(left, left_fitness), (right, right_fitness)| {
                left_fitness
                    .total_cmp(right_fitness)
                    .then_with(|| right.entity.to_bits().cmp(&left.entity.to_bits()))
            })
            .map(|(engagement, _)| engagement)
        else {
            continue;
        };
        let slot = if candidate.cost <= constraints.per_slot_budget.max(1) {
            &mut within_budget
        } else {
            &mut over_budget
        };
        let better = slot
            .as_ref()
            .is_none_or(|(current_op_index, current, current_engagement)| {
                let candidate_priority = operation_priority_rank(&operations[op_index]);
                let current_priority = operation_priority_rank(&operations[*current_op_index]);
                candidate_priority < current_priority
                    || (candidate_priority == current_priority
                        && (engagement
                            .fitness
                            .total_cmp(&current_engagement.fitness)
                            .is_gt()
                            || (engagement
                                .fitness
                                .total_cmp(&current_engagement.fitness)
                                .is_eq()
                                && candidate.cost < current.cost)))
            });
        if better {
            *slot = Some((op_index, candidate, engagement));
        }
    }

    within_budget.or(over_budget)
}

/// 実在前線全体に対する、1体の増援候補の集約有効度。
///
/// 同じ価格なら早く射撃でき、敵の反撃より大きく削れる候補を優先する。また、資金が
/// 潤沢な局面で「安いが遅く脆い歩兵」を工場枠の節約として誤採用しないよう、価格は
/// ここでの主評価に入れない。資金制約は呼び出し側の残額・工場あたり予算で扱う。
fn immediate_combat_engagements(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    operation: &Operation,
    facility: &GridPosition,
    stats: &UnitStats,
    player_id: PlayerId,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
) -> Vec<ImmediateCombatEngagement> {
    let mut engagements = Vec::new();
    for threat in &operation.reachable_threats {
        // available_turnが正の敵は観測済みでもまだ局地前線にいない。Mustの再評価対象に
        // 留め、余剰の即時投入先にしてはならない。
        let Some(entity) = threat.entity else {
            continue;
        };
        if threat.available_turn != 0 || threat.current_hp == 0 {
            continue;
        }
        let Some(action_distance) = calculate_action_distance_to_range(
            &scan.map,
            &scan.master_data,
            unit_positions,
            (facility.x, facility.y),
            (threat.position.x, threat.position.y),
            stats.movement_type,
            stats.max_movement,
            stats.max_fuel,
            stats.min_range,
            stats.max_range,
            player_id,
            &mut ctx.action_turns,
        ) else {
            continue;
        };
        let damage = best_damage(&scan.damage_chart, stats.unit_type, threat.stats.unit_type);
        if damage == 0 {
            continue;
        }
        // 占領・輸送役を止める価値は、単なる射撃ユニットより高い。価格やユニット名で
        // はなく、その敵が盤面へ与えられる行動能力だけから重みを導く。
        let strategic_weight = if threat.stats.can_capture {
            2.0
        } else if threat.stats.max_cargo > 0 {
            1.5
        } else {
            1.0
        };
        // 新規生産unitはこの手番には撃てない。射撃位置への行動ターンに1を足して、
        // 早く前線へ届く候補ほど大きくする。`calculate_action_distance_to_range` は
        // 間接unitの最小射程・移動後攻撃不可も含む。
        let arrival_turns = action_distance.turns.saturating_add(1);
        let arrival_weight = 1.0 / (arrival_turns.saturating_add(1) as f32);
        let received_damage =
            best_damage(&scan.damage_chart, threat.stats.unit_type, stats.unit_type);
        // 被ダメージを無視して高火力unitだけを並べると、前線へ着いた直後の悪い交換を
        // 選んでしまう。1回の交戦で相手HPへ与えられる割合と、相互ダメージ比を掛ける。
        let attack_fraction = damage.min(threat.current_hp) as f32 / threat.current_hp as f32;
        let exchange_ratio = damage as f32 / damage.saturating_add(received_damage).max(1) as f32;
        engagements.push(ImmediateCombatEngagement {
            entity,
            current_hp: threat.current_hp,
            damage,
            fitness: strategic_weight * arrival_weight * attack_fraction * exchange_ratio,
        });
    }
    engagements
}

/// 生産候補の経路計算に必要な、現在の占有マスを組み立てる。
///
/// `BoardScan` は相手のPlayerIdを保持しないため、相手側には自軍と異なるダミーIDを
/// 置く。距離計算が必要とするのは「自軍か否か」だけであり、敵勢力どうしを区別する
/// 必要はない。
fn scanned_occupants(
    scan: &BoardScan,
    player_id: PlayerId,
) -> HashMap<(usize, usize), OccupantInfo> {
    let opposing_player = PlayerId(player_id.0.saturating_add(1));
    scan.my_units
        .iter()
        .map(|unit| (unit, player_id))
        .chain(scan.enemy_units.iter().map(|unit| (unit, opposing_player)))
        .map(|(unit, owner)| {
            (
                (unit.pos.x, unit.pos.y),
                OccupantInfo {
                    player_id: owner,
                    is_transport: unit.stats.max_cargo > 0,
                    unit_type: unit.stats.unit_type,
                    loadable_types: unit.stats.loadable_unit_types.clone(),
                    free_slots: unit.free_cargo,
                },
            )
        })
        .collect()
}

/// 積荷を目標へ届けられるか。
///
/// 降車の可否はゲームのルール（`can_unload_from_terrain`）が決めており、
/// 艦船は港か浅瀬に接岸しないと積荷を降ろせない。海上に浮いたまま
/// 隣のマスへ降ろすことはできないので、「目標の隣まで行けるか」で
/// 判定しても渡洋作戦の成否とは対応しない。そこで
///   (1) 輸送自身が到達でき、かつ降車が許される揚陸地点があり、
///   (2) その隣接マスから積荷が自力で目標まで行ける
/// マスが存在するかどうかを見る。
///
/// 地形ルールは `can_unload_from_terrain` に、隣接の定義は
/// `map.get_adjacent` に委ねるため、特定のマップやトポロジーに依存しない。
fn can_deliver_cargo(
    map: &Map,
    registry: &MasterDataRegistry,
    ctx: &mut ReachCtx,
    from: &GridPosition,
    anchor: &GridPosition,
    transport_movement: MovementType,
    cargo_movement: MovementType,
) -> bool {
    let key = DeliveryKey {
        transport_movement,
        cargo_movement,
        start: *from,
        target: *anchor,
    };
    if let Some(cached) = ctx.delivery.get(&key) {
        return *cached;
    }

    let mut result = false;
    'outer: for y in 0..map.height {
        for x in 0..map.width {
            let Some(terrain) = map.get_terrain(x, y) else {
                continue;
            };
            // 揚陸が許される地形か（艦船なら港・浅瀬のみ）
            if !can_unload_from_terrain(Some(transport_movement), Some(terrain)) {
                continue;
            }
            // 輸送自身がその揚陸地点まで行けるか
            if !ctx.is_reachable(map, registry, (from.x, from.y), (x, y), transport_movement) {
                continue;
            }
            // 降ろした先から積荷が目標へ行けるか。
            // `is_reachable` は積荷が進入できない地形を非連結として弾くので、
            // 降車先そのものの通行可否もここで同時に判定される。
            for (ax, ay) in map.get_adjacent(x, y) {
                if ctx.is_reachable(
                    map,
                    registry,
                    (ax, ay),
                    (anchor.x, anchor.y),
                    cargo_movement,
                ) {
                    result = true;
                    break 'outer;
                }
            }
        }
    }

    ctx.delivery.insert(key, result);
    result
}

/// `from` にいる（あるいはそこで生産される）ユニットが、この作戦へ投入できるか。
///
/// 成立するのは次のいずれか。
/// (1) 自力で作戦地点まで到達できる
/// (2) それを積める輸送ユニットを空き施設で生産でき、その輸送が積荷を目標へ揚陸できる
///
/// この関数は **購入候補の採用判定（`slot_fitness`）と既存戦力の計上（台帳）の
/// 両方から呼ばれなければならない**。片側だけ条件を変えると「その枠を埋められるのに
/// 充足としては数えられない」ユニットが生まれ、要求が永久に減らずに同じユニットを
/// 買い続けるラチェットになる。
/// 判定はユニット名ではなく能力（`max_cargo` / `loadable_unit_types`）で行う。
fn can_join_operation(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    anchor: &GridPosition,
    requires_transport: bool,
    from: &GridPosition,
    stats: &UnitStats,
) -> bool {
    // 自力で作戦地点まで到達できるか
    if ctx.is_reachable(
        &scan.map,
        &scan.master_data,
        (from.x, from.y),
        (anchor.x, anchor.y),
        stats.movement_type,
    ) {
        return true;
    }
    if !requires_transport {
        return false;
    }
    // 空き施設で生産できる輸送ユニットの候補を先に洗い出す（借用を分離するため）
    let carriers: Vec<(GridPosition, MovementType)> = scan
        .free_facilities
        .iter()
        .flat_map(|(facility, terrain)| {
            scan.available_types
                .iter()
                .filter(|(unit_type, carrier)| {
                    carrier.max_cargo > 0
                        && carrier.loadable_unit_types.contains(&stats.unit_type)
                        && scan.can_produce(*terrain, *unit_type)
                })
                .map(move |(_, carrier)| (*facility, carrier.movement_type))
        })
        .collect();

    carriers.into_iter().any(|(facility, movement_type)| {
        can_deliver_cargo(
            &scan.map,
            &scan.master_data,
            ctx,
            &facility,
            anchor,
            movement_type,
            stats.movement_type,
        )
    })
}

/// ユニットが指定枠にどれだけ適合するかを返す。適合しない場合は `None`。
fn slot_fitness(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    kind: SlotKind,
    facility: &GridPosition,
    stats: &UnitStats,
) -> Option<f32> {
    // 施設から作戦地点まで自力で到達できるか
    let self_deployable = ctx.is_reachable(
        &scan.map,
        &scan.master_data,
        (facility.x, facility.y),
        (op.anchor.x, op.anchor.y),
        stats.movement_type,
    );

    match kind {
        SlotKind::Capture => {
            if !stats.can_capture {
                return None;
            }
            // 自力で行けないなら、実際に運べる輸送手段が存在することが前提。
            // 「輸送枠が立っている」だけでは運搬が成立する保証にならない。
            // 台帳（`build_operation` の自軍仕分け）と同じ関数を通すこと。
            if !can_join_operation(
                scan,
                ctx,
                &op.anchor,
                op.facts.requires_transport,
                facility,
                stats,
            ) {
                return None;
            }
            Some(1.0)
        }
        SlotKind::Transport => {
            if stats.max_cargo == 0 {
                return None;
            }
            // 「占領ユニットを積める」だけでは足りない。
            // その積荷を実際に目標へ揚陸できるところまで確かめる。
            // ここを緩めると運用の当てがない輸送を買い続けることになり、
            // 逆に目標マス自体への到達を求めると艦船が永久に候補から外れる。
            let deliverable = scan
                .available_types
                .iter()
                .filter(|(unit_type, cargo)| {
                    cargo.can_capture && stats.loadable_unit_types.contains(unit_type)
                })
                .map(|(_, cargo)| cargo.movement_type)
                .collect::<Vec<_>>()
                .into_iter()
                .any(|cargo_movement| {
                    can_deliver_cargo(
                        &scan.map,
                        &scan.master_data,
                        ctx,
                        facility,
                        &op.anchor,
                        stats.movement_type,
                        cargo_movement,
                    )
                });
            if !deliverable {
                return None;
            }
            Some(stats.max_cargo as f32)
        }
        SlotKind::Intercept => {
            if op.unreachable_threats.is_empty() {
                return None;
            }
            // 迎撃には「その脅威に届く」ことと「自力で現地へ行ける」ことの両方が要る。
            // 対空戦車が海を渡れないために選ばれないのは、この 2 条件の帰結。
            if !self_deployable {
                return None;
            }
            // 価格ではなく、次の一撃で実際に削れるHPを適合度にする。
            let value = op
                .unreachable_threats
                .iter()
                .map(|threat| {
                    best_damage(&scan.damage_chart, stats.unit_type, threat.stats.unit_type)
                        .min(threat.current_hp) as f32
                })
                .sum::<f32>();
            if value <= 0.0 { None } else { Some(value) }
        }
        SlotKind::Combat => None,
    }
}

/// 主武器・副武器のうち有効な方のダメージ。
fn best_damage(chart: &DamageChart, attacker: UnitType, defender: UnitType) -> u32 {
    chart.get_base_damage(attacker, defender).unwrap_or(0).max(
        chart
            .get_base_damage_secondary(attacker, defender)
            .unwrap_or(0),
    )
}

/// 見送り購入（資金を貯めて上位の候補を買う）を行うべきか。
///
/// 一括編成が必要な作戦に限り、いま買える候補より明確に適合度の高い候補が
/// `RESERVATION_PATIENCE_TURNS` 以内の収入で買えるなら、今ターンは生産しない。
fn should_defer_purchase(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    kind: SlotKind,
    remaining_funds: u32,
    affordable_cost: u32,
) -> bool {
    if acquisition_mode(&op.facts) != AcquisitionMode::SquadPackage {
        return false;
    }
    if scan.my_income == 0 {
        return false;
    }
    // 輸送が要る作戦で輸送枠がまだ空いているのに他の枠を先に埋めるのは避ける
    let mut best_future: Option<(f32, u32)> = None;
    for (facility, terrain) in &scan.free_facilities {
        for (unit_type, stats) in &scan.available_types {
            if !scan.can_produce(*terrain, *unit_type) || stats.cost <= remaining_funds {
                continue;
            }
            let Some(fitness) = slot_fitness(scan, ctx, op, kind, facility, stats) else {
                continue;
            };
            let scaled = fitness * 1000.0 / stats.cost as f32;
            if best_future.is_none_or(|(current, _)| scaled > current) {
                best_future = Some((scaled, stats.cost));
            }
        }
    }
    let Some((future_fitness, future_cost)) = best_future else {
        return false;
    };
    // 現在買える候補の適合度
    let current_fitness = scan
        .available_types
        .iter()
        .filter(|(_, stats)| stats.cost <= remaining_funds && stats.cost == affordable_cost)
        .filter_map(|(_, stats)| {
            scan.free_facilities.iter().find_map(|(facility, terrain)| {
                if !scan.can_produce(*terrain, stats.unit_type) {
                    return None;
                }
                slot_fitness(scan, ctx, op, kind, facility, stats)
                    .map(|f| f * 1000.0 / stats.cost as f32)
            })
        })
        .fold(0.0f32, f32::max);

    if future_fitness <= current_fitness {
        return false;
    }
    let shortfall = future_cost.saturating_sub(remaining_funds);
    let turns_to_afford = shortfall.div_ceil(scan.my_income.max(1));
    turns_to_afford <= RESERVATION_PATIENCE_TURNS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::GridTopology;

    /// テスト用に、DAG orderを読むEntityを具体Squadへ接続する。
    ///
    /// 本番と同じく `Entity -> UnitOperationRegistry -> SquadId -> route order` を
    /// 通るため、Entityを直接キーにした旧来のfixtureを再導入しない。
    fn assign_route_squad(
        world: &mut World,
        player: PlayerId,
        mission: MissionType,
        members: &[Entity],
    ) -> SquadId {
        let mut manager = world
            .remove_resource::<crate::ai::squad::SquadManager>()
            .unwrap_or_default();
        let squad = manager.create_owned_squad(mission, player);
        squad.members.extend(members.iter().copied());
        let squad_id = squad.id;
        world.insert_resource(manager);

        let mut assignments = world
            .remove_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
            .unwrap_or_default();
        for entity in members {
            assignments.assign(
                *entity,
                crate::ai::operation_assignment::UnitOperationAssignment {
                    owner: crate::ai::operation_assignment::OperationOwner::TacticalSquad {
                        player_id: player,
                        squad_id,
                    },
                    squad_id: Some(squad_id),
                    role: crate::ai::operation_assignment::OperationUnitRole::Member,
                    assigned_turn: 0,
                },
            );
        }
        world.insert_resource(assignments);
        squad_id
    }

    #[test]
    fn frontline_capacity_follows_scanned_node_geometry() {
        let player = PlayerId(1);
        let island = crate::ai::islands::IslandId(0);
        let operation = |scope, control_area, capture_targets| CapitalRouteNodeOperation {
            id: CapitalRouteNodeOperationId {
                topology: CapitalRouteTopologyKey {
                    player_id: player,
                    island_id: island,
                },
                node: CapitalRouteNodeId(1),
            },
            anchor: pos(2, 2),
            predecessors: Vec::new(),
            successors: Vec::new(),
            objective: CapitalRouteNodeObjective::Capture,
            scope,
            control_area,
            capture_targets,
            crossing_exit: None,
            crossed: false,
            state: victory_roadmap::RoadmapNodeState::Ready,
            assigned_squads: HashSet::new(),
        };

        // 橋などの一点Nodeは、部隊数ではなく通過地形そのものから一枠とする。
        assert_eq!(
            capital_route_node_frontline_capacity(&operation(
                CapitalRouteNodeScope::Point,
                vec![pos(2, 2), pos(3, 2), pos(4, 2)],
                vec![pos(4, 2), pos(5, 2)],
            )),
            1
        );
        // Areaは、局地戦の実セル数または占領対象数の大きい方だけを前衛へ送る。
        assert_eq!(
            capital_route_node_frontline_capacity(&operation(
                CapitalRouteNodeScope::Area,
                vec![pos(2, 2), pos(3, 2), pos(4, 2)],
                vec![pos(4, 2), pos(5, 2), pos(6, 2), pos(7, 2)],
            )),
            4
        );
    }

    #[test]
    fn nearby_capture_region_has_one_owner_operation() {
        let map = flat_map(12, 3);
        let anchors = vec![pos(1, 1), pos(2, 2), pos(4, 1), pos(8, 1)];
        let mut targets = vec![
            vec![pos(1, 1)],
            vec![pos(2, 2)],
            vec![pos(4, 1)],
            vec![pos(8, 1)],
        ];

        consolidate_nearby_capture_target_regions(&map, &anchors, &mut targets);

        assert_eq!(targets[0], vec![pos(1, 1), pos(4, 1), pos(2, 2)]);
        assert!(targets[1].is_empty());
        assert!(targets[2].is_empty());
        assert_eq!(
            targets[3],
            vec![pos(8, 1)],
            "局地半径を越えた拠点まで一つの完了条件へ混ぜない"
        );
    }

    #[test]
    fn linear_capture_corridor_keeps_independent_operations() {
        let map = flat_map(12, 3);
        let anchors = vec![pos(1, 1), pos(2, 1), pos(4, 1)];
        let mut targets = vec![vec![pos(1, 1)], vec![pos(2, 1)], vec![pos(4, 1)]];

        consolidate_nearby_capture_target_regions(&map, &anchors, &mut targets);

        assert_eq!(
            targets,
            vec![vec![pos(1, 1)], vec![pos(2, 1)], vec![pos(4, 1)]],
            "一直線の回廊は一Areaへ束ねず、前方へ順番に進める"
        );
    }

    #[test]
    fn capture_node_requires_secured_for_both_area_and_point() {
        let player = PlayerId(1);
        let topology = CapitalRouteTopologyKey {
            player_id: player,
            island_id: crate::ai::islands::IslandId(0),
        };
        let operation = |scope| CapitalRouteNodeOperation {
            id: CapitalRouteNodeOperationId {
                topology,
                node: CapitalRouteNodeId(1),
            },
            anchor: pos(2, 2),
            predecessors: Vec::new(),
            successors: Vec::new(),
            objective: CapitalRouteNodeObjective::Capture,
            scope,
            control_area: vec![pos(2, 2)],
            capture_targets: vec![pos(2, 2)],
            crossing_exit: None,
            crossed: false,
            state: victory_roadmap::RoadmapNodeState::Dominant,
            assigned_squads: HashSet::new(),
        };

        assert!(
            !capital_route_node_exit_satisfied(&operation(CapitalRouteNodeScope::Area)),
            "Areaの局地優勢も全対象の所有権確保までは後続を開けない"
        );
        assert!(
            !capital_route_node_exit_satisfied(&operation(CapitalRouteNodeScope::Point)),
            "単一Capture拠点は所有権確保まで後続を開けない"
        );
    }

    #[test]
    fn dominant_area_capture_retains_one_escort_until_secured() {
        let player = PlayerId(1);
        let topology = CapitalRouteTopologyKey {
            player_id: player,
            island_id: crate::ai::islands::IslandId(0),
        };
        let rear_node = CapitalRouteNodeId(1);
        let active_node = CapitalRouteNodeId(2);
        let rear_id = CapitalRouteNodeOperationId {
            topology,
            node: rear_node,
        };
        let operation = |id, state| CapitalRouteNodeOperation {
            id,
            anchor: pos(id.node.0, 2),
            predecessors: Vec::new(),
            successors: Vec::new(),
            objective: CapitalRouteNodeObjective::Capture,
            scope: CapitalRouteNodeScope::Area,
            control_area: vec![pos(id.node.0, 2)],
            capture_targets: vec![pos(id.node.0, 2)],
            crossing_exit: None,
            crossed: false,
            state,
            assigned_squads: HashSet::new(),
        };
        let mut registry = CapitalRouteNodeOperationRegistry::default();
        registry.operations.insert(
            rear_id,
            operation(rear_id, victory_roadmap::RoadmapNodeState::Dominant),
        );

        assert_eq!(
            capital_route_dominant_capture_escort_node(
                &[rear_node, active_node],
                active_node,
                &registry,
                topology,
            ),
            Some(rear_node),
            "主力が次Nodeへ進んでも未確保AreaにはControl護衛を残す"
        );

        registry
            .operations
            .get_mut(&rear_id)
            .expect("後方Capture Nodeが存在する")
            .state = victory_roadmap::RoadmapNodeState::Secured;
        assert_eq!(
            capital_route_dominant_capture_escort_node(
                &[rear_node, active_node],
                active_node,
                &registry,
                topology,
            ),
            None,
            "占領完了後は護衛も次Nodeへ解放する"
        );
    }

    #[test]
    fn capture_frontline_reserves_both_capturer_and_control_lanes() {
        let player = PlayerId(1);
        let island = crate::ai::islands::IslandId(0);
        let operation = |scope, control_area, capture_targets| CapitalRouteNodeOperation {
            id: CapitalRouteNodeOperationId {
                topology: CapitalRouteTopologyKey {
                    player_id: player,
                    island_id: island,
                },
                node: CapitalRouteNodeId(1),
            },
            anchor: pos(2, 2),
            predecessors: Vec::new(),
            successors: Vec::new(),
            objective: CapitalRouteNodeObjective::Capture,
            scope,
            control_area,
            capture_targets,
            crossing_exit: None,
            crossed: false,
            state: victory_roadmap::RoadmapNodeState::Ready,
            assigned_squads: HashSet::new(),
        };

        let point = operation(
            CapitalRouteNodeScope::Point,
            vec![pos(2, 2)],
            vec![pos(2, 2)],
        );
        assert_eq!(capital_route_role_frontline_capacity(&point, true), 1);
        assert_eq!(
            capital_route_role_frontline_capacity(&point, false),
            1,
            "一点占領でも隣接護衛をStageへ落とさない"
        );

        let area = operation(
            CapitalRouteNodeScope::Area,
            vec![pos(1, 2), pos(2, 2), pos(3, 2), pos(4, 2)],
            vec![pos(2, 2), pos(3, 2)],
        );
        assert_eq!(capital_route_role_frontline_capacity(&area, true), 2);
        assert_eq!(capital_route_role_frontline_capacity(&area, false), 2);
    }

    #[test]
    fn nearest_capture_capable_combat_squad_can_take_the_capture_lane() {
        let player = PlayerId(1);
        let island = crate::ai::islands::IslandId(0);
        let mut operation = CapitalRouteNodeOperation {
            id: CapitalRouteNodeOperationId {
                topology: CapitalRouteTopologyKey {
                    player_id: player,
                    island_id: island,
                },
                node: CapitalRouteNodeId(1),
            },
            anchor: pos(2, 2),
            predecessors: Vec::new(),
            successors: Vec::new(),
            objective: CapitalRouteNodeObjective::Capture,
            scope: CapitalRouteNodeScope::Area,
            control_area: vec![pos(2, 2), pos(3, 2)],
            capture_targets: vec![pos(3, 2)],
            crossing_exit: None,
            crossed: false,
            state: victory_roadmap::RoadmapNodeState::Ready,
            assigned_squads: HashSet::new(),
        };

        assert!(capital_route_uses_capture_lane(
            &operation,
            &MissionType::Attack,
            1,
            0,
        ));
        assert!(
            !capital_route_uses_capture_lane(&operation, &MissionType::Attack, 1, 1),
            "占領枠が埋まった後の戦闘可能歩兵はControl枠へ回す"
        );
        assert!(
            capital_route_uses_capture_lane(&operation, &MissionType::Capture, 1, 1),
            "余剰Capture SquadはControl扱いにせずCapture待機線へ置く"
        );
        assert!(!capital_route_uses_capture_lane(
            &operation,
            &MissionType::Attack,
            0,
            0,
        ));

        operation.objective = CapitalRouteNodeObjective::Control;
        assert!(!capital_route_uses_capture_lane(
            &operation,
            &MissionType::Capture,
            1,
            0,
        ));
    }

    #[test]
    fn forward_stage_squad_outranks_rear_advance_squad_for_frontline_rotation() {
        let forward_stage =
            capital_route_frontline_sort_key(12, &MissionType::Attack, 0, 1_000, SquadId(200));
        let rear_advance =
            capital_route_frontline_sort_key(3, &MissionType::Attack, 0, 20_000, SquadId(1));

        assert!(
            forward_stage < rear_advance,
            "前線到達済みの予備を古い後方Squadより先にAdvanceへ昇格させる"
        );
    }

    #[test]
    fn durable_direct_fire_squad_wins_same_frontline_escort_slot() {
        let armored =
            capital_route_frontline_sort_key(8, &MissionType::Attack, 0, 20_000, SquadId(20));
        let light =
            capital_route_frontline_sort_key(8, &MissionType::Attack, 0, 5_000, SquadId(10));

        assert!(
            armored < light,
            "同じ前進度なら耐久・価格・直接交戦能力が高いControl分隊を護衛へ回す"
        );
    }

    #[test]
    fn area_control_region_excludes_remote_milestones() {
        let map = flat_map(30, 5);
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let anchor = pos(15, 2);
        let island = island_map.get_island_at(&anchor).unwrap().id;
        let capture_target = pos(14, 2);

        let area = capital_route_node_control_area(
            &map,
            &island_map,
            island,
            anchor,
            CapitalRouteNodeScope::Area,
            &[capture_target],
        );

        assert!(area.contains(&anchor));
        assert!(area.contains(&capture_target));
        assert!(
            !area.contains(&pos(22, 2)),
            "後方Milestoneを局地優勢へ数えない"
        );
        assert!(
            !area.contains(&pos(8, 2)),
            "後続Milestoneの敵を現在Nodeへ数えない"
        );
        assert!(area.iter().all(|position| {
            map.distance(anchor.x, anchor.y, position.x, position.y) <= CAPITAL_ROUTE_REGION_RADIUS
        }));
    }

    #[test]
    fn hold_and_supply_remain_addressable_inside_the_route_dag() {
        let player = PlayerId(1);
        let island = crate::ai::islands::IslandId(0);
        let hold = GridPosition { x: 4, y: 4 };
        let supply = GridPosition { x: 7, y: 4 };
        let segment_target = GridPosition { x: 9, y: 4 };
        let mut world = World::new();
        let hold_entity = world.spawn(hold).id();
        let supply_entity = world.spawn(supply).id();
        let recover_entity = world
            .spawn((
                hold,
                Health {
                    current: 60,
                    max: 100,
                },
            ))
            .id();
        let hold_squad =
            assign_route_squad(&mut world, player, MissionType::Defense, &[hold_entity]);
        let supply_squad =
            assign_route_squad(&mut world, player, MissionType::Transport, &[supply_entity]);
        let recover_squad =
            assign_route_squad(&mut world, player, MissionType::Attack, &[recover_entity]);
        world.insert_resource(CapitalRoutePathRegistry {
            commitments: HashMap::from([
                (
                    hold_squad,
                    CapitalRouteCommitment {
                        player_id: player,
                        island_id: island,
                        route: CapitalRouteId(0),
                        target: segment_target,
                        target_node: CapitalRouteNodeId(2),
                        objective: CapitalRouteNodeObjective::Control,
                        scope: CapitalRouteNodeScope::Point,
                        control_area: vec![segment_target],
                        capture_targets: Vec::new(),
                        crossing_exit: None,
                        crossed: false,
                        execution_target: hold,
                        frontline_capacity: 1,
                        path_nodes: vec![
                            CapitalRouteNodeId(0),
                            CapitalRouteNodeId(1),
                            CapitalRouteNodeId(2),
                        ],
                        path: vec![hold, supply, segment_target],
                        phase: CapitalRouteExecutionPhase::Hold,
                    },
                ),
                (
                    supply_squad,
                    CapitalRouteCommitment {
                        player_id: player,
                        island_id: island,
                        route: CapitalRouteId(0),
                        target: segment_target,
                        target_node: CapitalRouteNodeId(2),
                        objective: CapitalRouteNodeObjective::Control,
                        scope: CapitalRouteNodeScope::Point,
                        control_area: vec![segment_target],
                        capture_targets: Vec::new(),
                        crossing_exit: None,
                        crossed: false,
                        execution_target: supply,
                        frontline_capacity: 1,
                        path_nodes: vec![
                            CapitalRouteNodeId(0),
                            CapitalRouteNodeId(1),
                            CapitalRouteNodeId(2),
                        ],
                        path: vec![hold, supply, segment_target],
                        phase: CapitalRouteExecutionPhase::Supply,
                    },
                ),
                (
                    recover_squad,
                    CapitalRouteCommitment {
                        player_id: player,
                        island_id: island,
                        route: CapitalRouteId(0),
                        target: segment_target,
                        target_node: CapitalRouteNodeId(2),
                        objective: CapitalRouteNodeObjective::Control,
                        scope: CapitalRouteNodeScope::Point,
                        control_area: vec![segment_target],
                        capture_targets: Vec::new(),
                        crossing_exit: None,
                        crossed: false,
                        execution_target: segment_target,
                        frontline_capacity: 1,
                        path_nodes: vec![
                            CapitalRouteNodeId(0),
                            CapitalRouteNodeId(1),
                            CapitalRouteNodeId(2),
                        ],
                        path: vec![hold, supply, segment_target],
                        phase: CapitalRouteExecutionPhase::Advance,
                    },
                ),
            ]),
            ..CapitalRoutePathRegistry::default()
        });

        // 回復以外は、突撃役だけでなくDAG内の区間終点を持つ。
        assert_eq!(
            capital_route_waypoint(&world, player, hold_entity),
            Some(hold)
        );
        assert_eq!(
            capital_route_waypoint(&world, player, supply_entity),
            Some(supply)
        );
        assert_eq!(capital_route_waypoint(&world, player, recover_entity), None);
        assert!(!capital_route_is_recovering(&world, player, hold_entity));
        assert!(capital_route_is_recovering(&world, player, recover_entity));
    }

    #[test]
    fn route_order_is_shared_by_members_of_the_same_squad() {
        let player = PlayerId(1);
        let island = crate::ai::islands::IslandId(0);
        let target = pos(6, 1);
        let mut world = World::new();
        let first = world.spawn(pos(1, 1)).id();
        let second = world.spawn(pos(1, 1)).id();
        let unassigned = world.spawn_empty().id();
        let squad_id =
            assign_route_squad(&mut world, player, MissionType::Attack, &[first, second]);
        world.insert_resource(CapitalRoutePathRegistry {
            commitments: HashMap::from([(
                squad_id,
                CapitalRouteCommitment {
                    player_id: player,
                    island_id: island,
                    route: CapitalRouteId(0),
                    target,
                    target_node: CapitalRouteNodeId(1),
                    objective: CapitalRouteNodeObjective::Control,
                    scope: CapitalRouteNodeScope::Point,
                    control_area: vec![target],
                    capture_targets: Vec::new(),
                    crossing_exit: None,
                    crossed: false,
                    execution_target: target,
                    frontline_capacity: 1,
                    path_nodes: vec![CapitalRouteNodeId(0), CapitalRouteNodeId(1)],
                    path: vec![pos(1, 1), target],
                    phase: CapitalRouteExecutionPhase::Advance,
                },
            )]),
            ..CapitalRoutePathRegistry::default()
        });

        assert_eq!(capital_route_waypoint(&world, player, first), Some(target));
        assert_eq!(capital_route_waypoint(&world, player, second), Some(target));
        assert_eq!(capital_route_waypoint(&world, player, unassigned), None);
    }

    #[test]
    fn recreated_squad_inherits_one_previous_squad_order_atomically() {
        let player = PlayerId(1);
        let island = crate::ai::islands::IslandId(0);
        let member = Entity::from_raw(100);
        let reinforcement = Entity::from_raw(101);
        let old_squad = SquadId(4);
        let new_squad = SquadId(9);
        let target = pos(6, 1);
        let commitment = CapitalRouteCommitment {
            player_id: player,
            island_id: island,
            route: CapitalRouteId(0),
            target,
            target_node: CapitalRouteNodeId(2),
            objective: CapitalRouteNodeObjective::Control,
            scope: CapitalRouteNodeScope::Point,
            control_area: vec![target],
            capture_targets: Vec::new(),
            crossing_exit: None,
            crossed: false,
            execution_target: target,
            frontline_capacity: 1,
            path_nodes: vec![CapitalRouteNodeId(0), CapitalRouteNodeId(2)],
            path: vec![pos(1, 1), target],
            phase: CapitalRouteExecutionPhase::Advance,
        };
        let registry = CapitalRoutePathRegistry {
            commitments: HashMap::from([(old_squad, commitment)]),
            commitment_members: HashMap::from([(old_squad, BTreeSet::from([member]))]),
            assignment_diagnostics: HashMap::new(),
        };

        let inherited = inherited_capital_route_commitment(
            &registry,
            new_squad,
            &BTreeSet::from([member, reinforcement]),
        );
        assert_eq!(
            inherited.map(|order| order.target_node),
            Some(CapitalRouteNodeId(2)),
            "再編と補充でIDが変わっても、旧Squadが一意ならorderを全体へ移す"
        );

        let mut conflicting = registry.clone();
        let other_squad = SquadId(5);
        conflicting.commitments.insert(
            other_squad,
            CapitalRouteCommitment {
                target_node: CapitalRouteNodeId(3),
                ..conflicting.commitments[&old_squad].clone()
            },
        );
        conflicting
            .commitment_members
            .insert(other_squad, BTreeSet::from([reinforcement]));
        assert!(
            inherited_capital_route_commitment(
                &conflicting,
                new_squad,
                &BTreeSet::from([member, reinforcement]),
            )
            .is_none(),
            "異なる旧Squadを統合した場合はEntityごとの命令を混在させない"
        );
    }

    #[test]
    fn only_capturer_claims_an_unsecured_route_milestone() {
        let player = PlayerId(1);
        let island = crate::ai::islands::IslandId(0);
        let target = pos(5, 1);
        let mut world = World::new();
        let infantry = world
            .spawn(UnitStats {
                unit_type: UnitType::Infantry,
                can_capture: true,
                ..UnitStats::mock()
            })
            .id();
        let tank = world
            .spawn(UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                can_capture: false,
                ..UnitStats::mock()
            })
            .id();
        let infantry_squad =
            assign_route_squad(&mut world, player, MissionType::Capture, &[infantry]);
        let tank_squad = assign_route_squad(&mut world, player, MissionType::Attack, &[tank]);
        let commitment = |route| CapitalRouteCommitment {
            player_id: player,
            island_id: island,
            route: CapitalRouteId(route),
            target,
            target_node: CapitalRouteNodeId(1),
            objective: CapitalRouteNodeObjective::Capture,
            scope: CapitalRouteNodeScope::Point,
            control_area: vec![target],
            capture_targets: vec![target],
            crossing_exit: None,
            crossed: false,
            execution_target: target,
            frontline_capacity: 1,
            path_nodes: vec![CapitalRouteNodeId(0), CapitalRouteNodeId(1)],
            path: vec![pos(1, 1), target],
            phase: CapitalRouteExecutionPhase::Advance,
        };
        world.insert_resource(CapitalRoutePathRegistry {
            commitments: HashMap::from([
                (infantry_squad, commitment(0)),
                (tank_squad, commitment(1)),
            ]),
            ..CapitalRoutePathRegistry::default()
        });

        assert!(capital_route_allows_capture_at(
            &world, player, infantry, target
        ));
        assert!(
            !capital_route_allows_capture_at(&world, player, infantry, pos(4, 1)),
            "Capture Nodeは回廊外の拠点を占領させない"
        );
        assert!(
            !capital_route_allows_capture_at(&world, player, tank, target),
            "占領能力のない戦車はCapture Nodeの担当にならない"
        );

        // 待機線の歩兵は同じCapture作戦に所属し続けるが、前衛枠が空くまで占領を始めない。
        world
            .resource_mut::<CapitalRoutePathRegistry>()
            .commitments
            .get_mut(&infantry_squad)
            .expect("歩兵SquadのDAG所属を保持する")
            .phase = CapitalRouteExecutionPhase::Stage;
        assert!(
            !capital_route_allows_capture_at(&world, player, infantry, target),
            "待機線のCapture Squadは前衛Nodeを先取りしない"
        );

        // 局地交戦でHoldへ移っても、横の拠点を占領してDAG進行を逸らしてはならない。
        world
            .resource_mut::<CapitalRoutePathRegistry>()
            .commitments
            .get_mut(&infantry_squad)
            .expect("歩兵SquadのDAG所属を保持する")
            .phase = CapitalRouteExecutionPhase::Hold;
        assert!(
            capital_route_allows_capture_at(&world, player, infantry, target),
            "Holdへ切り替わっても、Capture Nodeの占領対象だけは維持する"
        );
    }

    #[test]
    fn squad_mission_alone_defines_route_execution_phase() {
        assert_eq!(
            capital_route_execution_phase(&MissionType::Attack),
            CapitalRouteExecutionPhase::Advance
        );
        assert_eq!(
            capital_route_execution_phase(&MissionType::Defense),
            CapitalRouteExecutionPhase::Hold,
            "Defense SquadはDAG orderを保持する"
        );
        assert_eq!(
            capital_route_execution_phase(&MissionType::Transport),
            CapitalRouteExecutionPhase::Supply
        );
    }

    #[test]
    fn forward_defense_squad_is_reassigned_as_current_node_control() {
        assert!(capital_route_controls_active_node(
            &MissionType::Attack,
            false
        ));
        assert!(capital_route_controls_active_node(
            &MissionType::Defense,
            true
        ));
        assert!(
            !capital_route_controls_active_node(&MissionType::Defense, false),
            "本土側だけを指定するDefense Squadは現在Nodeから動かさない"
        );
        assert!(!capital_route_controls_active_node(
            &MissionType::Capture,
            true
        ));
    }

    #[test]
    fn capture_outside_the_active_route_keeps_its_squad_order() {
        // map_25の外周拠点のように、前方にあっても現在DAG経路に
        // 属さない目標はfalseとし、中央最短線へ上書きしない。
        assert!(!capital_route_capture_joins_active_node(
            &MissionType::Capture,
            true,
            false,
            false,
        ));
        assert!(capital_route_capture_joins_active_node(
            &MissionType::Capture,
            true,
            true,
            false,
        ));
        assert!(!capital_route_capture_joins_active_node(
            &MissionType::Capture,
            true,
            true,
            true,
        ));
        assert!(capital_route_capture_joins_active_node(
            &MissionType::Capture,
            false,
            false,
            true,
        ));
        assert!(capital_route_capture_joins_active_node(
            &MissionType::Attack,
            true,
            false,
            true,
        ));
    }

    #[test]
    fn unfinished_capture_node_reserves_property_cells_for_capturers() {
        let player = PlayerId(1);
        let topology = CapitalRouteTopologyKey {
            player_id: player,
            island_id: crate::ai::islands::IslandId(0),
        };
        let operation_id = CapitalRouteNodeOperationId {
            topology,
            node: CapitalRouteNodeId(1),
        };
        let target = pos(10, 11);
        let mut registry = CapitalRouteNodeOperationRegistry::default();
        registry.operations.insert(
            operation_id,
            CapitalRouteNodeOperation {
                id: operation_id,
                anchor: pos(8, 11),
                predecessors: Vec::new(),
                successors: Vec::new(),
                objective: CapitalRouteNodeObjective::Capture,
                scope: CapitalRouteNodeScope::Area,
                control_area: vec![pos(8, 11), target],
                capture_targets: vec![target],
                crossing_exit: None,
                crossed: false,
                state: victory_roadmap::RoadmapNodeState::Capturing,
                assigned_squads: HashSet::new(),
            },
        );

        assert!(capital_route_capture_target_is_reserved(
            &registry, player, target,
        ));
        registry
            .operations
            .get_mut(&operation_id)
            .expect("テスト対象Nodeが存在する")
            .state = victory_roadmap::RoadmapNodeState::Secured;
        assert!(!capital_route_capture_target_is_reserved(
            &registry, player, target,
        ));
    }

    #[test]
    fn capital_mainline_depends_on_forward_progress_not_auxiliary_property_count() {
        assert!(same_land_capital_front_reached(16, 6));
        assert!(same_land_capital_front_reached(32, 16));
        assert!(!same_land_capital_front_reached(32, 17));
    }

    #[test]
    fn bridge_gates_split_one_island_into_two_active_route_fronts() {
        let master_data = MasterDataRegistry::load().unwrap();
        let mut map = flat_map(7, 7);
        for y in 0..map.height {
            map.set_terrain(3, y, Terrain::River).unwrap();
        }
        map.set_terrain(3, 1, Terrain::Bridge).unwrap();
        map.set_terrain(3, 5, Terrain::Bridge).unwrap();
        let gates = armored_entry_bridge_gates(&map, &master_data, pos(0, 3), pos(6, 3));
        assert_eq!(gates, vec![pos(3, 1), pos(3, 5)]);

        let objective = CampaignPlanningObjective {
            island_id: crate::ai::islands::IslandId(0),
            kind: OperationKind::Capture,
            anchor: pos(2, 1),
            objective_properties: vec![pos(2, 1), pos(4, 1), pos(2, 5), pos(4, 5)],
            capture_eta: Some(4),
            required_capture_survivors: 5,
            logistics_rank: None,
            forced_target_enemies: HashSet::new(),
            protected_capture_entities: HashSet::new(),
            staging_anchor: pos(2, 1),
            execution_authorized: true,
        };
        let routes = split_bridge_route_objectives(&map, vec![objective], &gates, &[]);

        assert_eq!(routes.len(), 2);
        assert_eq!(
            routes
                .iter()
                .map(|route| route.required_capture_survivors)
                .sum::<usize>(),
            5
        );
        assert_eq!(routes[0].anchor.y, 1);
        assert_eq!(routes[1].anchor.y, 5);
        assert!(
            routes
                .iter()
                .all(|route| route.objective_properties.len() == 2)
        );
    }

    #[test]
    fn route_milestone_requires_the_nearest_unsecured_property_before_advancing() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignPortfolio,
            IslandCampaignRequirement,
        };

        let master_data = MasterDataRegistry::load().unwrap();
        let mut map = flat_map(7, 7);
        for y in 0..map.height {
            map.set_terrain(3, y, Terrain::River).unwrap();
        }
        map.set_terrain(3, 1, Terrain::Bridge).unwrap();
        map.set_terrain(3, 5, Terrain::Bridge).unwrap();
        map.set_terrain(0, 3, Terrain::Capital).unwrap();
        map.set_terrain(6, 3, Terrain::Capital).unwrap();
        for target in [pos(2, 1), pos(4, 1), pos(2, 5), pos(4, 5)] {
            map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        }
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map.get_island_at(&pos(0, 3)).unwrap().id;
        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let mut world = World::new();
        world.insert_resource(map);
        world.insert_resource(master_data);
        world.insert_resource(island_map);
        world.spawn((
            pos(0, 3),
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((pos(6, 3), Property::new(Terrain::Capital, Some(enemy), 100)));
        let first_north = world
            .spawn((pos(2, 1), Property::new(Terrain::City, None, 100)))
            .id();
        world.spawn((pos(4, 1), Property::new(Terrain::City, None, 100)));
        let first_south = world
            .spawn((pos(2, 5), Property::new(Terrain::City, None, 100)))
            .id();
        world.spawn((pos(4, 5), Property::new(Terrain::City, None, 100)));
        prepare_capital_route_topologies(&mut world, player);
        assert!(
            !capital_route_assault_authorized_from_dag(&world, player, island_id)
                .expect("二回廊DAGの初期Go判定"),
            "最初の橋頭堡を確保する前に、首都作戦だけを実行へ移さない"
        );
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 2,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 2_000,
        };
        let mut portfolio = IslandCampaignPortfolio {
            active_offensives: vec![IslandCampaignAssignment {
                island_id,
                decision: IslandCampaignDecision::Contest,
                target_position: pos(3, 3),
                capture_target_positions: vec![pos(3, 3)],
                priority_enemy_types: Vec::new(),
                requirement: requirement.clone(),
                purchase_shortfall: requirement,
                allocated_budget: 0,
                transport_entities: Vec::new(),
                capture_entities: Vec::new(),
                combat_entities: Vec::new(),
                operation_ready: true,
                continued_from_existing_squad: false,
            }],
            ..IslandCampaignPortfolio::default()
        };

        refine_same_land_route_milestones(&world, player, &mut portfolio);
        assert_eq!(
            portfolio.active_offensives[0].capture_target_positions,
            vec![pos(2, 1), pos(2, 5)],
            "橋の通過だけを優先せず、各枝で最初に未確保の施設をMilestoneにする"
        );

        world.get_mut::<Property>(first_north).unwrap().owner_id = Some(player);
        world.get_mut::<Property>(first_south).unwrap().owner_id = Some(player);
        assert!(
            capital_route_assault_authorized_from_dag(&world, player, island_id)
                .expect("前方区間を確保した後のDAG Go判定"),
            "DAGの両枝で先頭拠点を確保したら、旧来の側方都市数ではなく前方区間の進捗でGoする"
        );
        refine_same_land_route_milestones(&world, player, &mut portfolio);
        assert_eq!(
            portfolio.active_offensives[0].capture_target_positions,
            vec![pos(4, 1), pos(4, 5)],
            "手前の施設を確保した枝だけ、次の橋向こうの施設へ進める"
        );
    }

    #[test]
    fn map6_mountain_corridors_create_two_route_fronts() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_6",
            GridTopology::Square,
        )
        .expect("map_6を初期化できる");
        let player = PlayerId(1);
        let own_capital = world
            .iter_entities()
            .find_map(|entity| {
                let position = entity.get::<GridPosition>()?;
                let property = entity.get::<Property>()?;
                (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                    .then_some(*position)
            })
            .expect("自首都");
        let island = world
            .resource::<crate::ai::islands::IslandMap>()
            .get_island_at(&own_capital)
            .expect("首都の島")
            .id;

        let fronts = same_land_route_fronts(&world, player, island);

        assert_eq!(
            fronts.len(),
            2,
            "山で分かれるmap_6を単一前線へ潰さない: fronts={fronts:?}"
        );
        assert!(fronts[0].y < fronts[1].y, "北・南の別回廊を前線として得る");

        let p2 = PlayerId(2);
        let p2_fronts = same_land_route_fronts(&world, p2, island);
        assert_eq!(
            p2_fronts.len(),
            2,
            "後攻P2でもmap_6は2本のルート前線を得る: p2_fronts={p2_fronts:?}"
        );
    }

    #[test]
    fn map6_p2_capital_route_discovers_both_frontiers() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_6",
            GridTopology::Square,
        )
        .unwrap();
        let player = PlayerId(2);
        let own_capital = world
            .iter_entities()
            .find_map(|entity| {
                let position = entity.get::<GridPosition>()?;
                let property = entity.get::<Property>()?;
                (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                    .then_some(*position)
            })
            .expect("自首都");
        let island = world
            .resource::<crate::ai::islands::IslandMap>()
            .get_island_at(&own_capital)
            .expect("首都の島")
            .id;

        prepare_capital_route_topologies(&mut world, player);
        let topology = capital_route_topology_for(&world, player, island)
            .expect("P2の首都戦役DAGを構築できる");
        assert_eq!(topology.routes.len(), 2, "map_6の2進軍軸を保持する");

        let frontiers = capital_route_dag_frontier_properties(&world, player, island)
            .expect("両ルートの拠点を検出できる");
        assert_eq!(frontiers.len(), 2);
        assert!(!frontiers[0].is_empty(), "Route 0の拠点を検出する");
        assert!(!frontiers[1].is_empty(), "Route 1の拠点を検出する");
    }

    #[test]
    fn map1_single_corridor_still_builds_a_capital_route_dag() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_1",
            GridTopology::Square,
        )
        .expect("map_1を初期化できる");
        let player = PlayerId(1);
        let own_capital = world
            .iter_entities()
            .find_map(|entity| {
                let position = entity.get::<GridPosition>()?;
                let property = entity.get::<Property>()?;
                (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                    .then_some(*position)
            })
            .expect("自首都");
        let island = world
            .resource::<crate::ai::islands::IslandMap>()
            .get_island_at(&own_capital)
            .expect("首都の島")
            .id;

        prepare_capital_route_topologies(&mut world, player);
        let topology = capital_route_topology_for(&world, player, island)
            .expect("単一路でも首都戦役DAGを構築する");
        assert_eq!(
            topology.routes.len(),
            1,
            "map_1を複数routeへ水増しせず、一本の首都進軍路として管理する"
        );
        let start_node = topology.start_node;
        let manager = crate::ai::squad::SquadManager::default();
        refresh_capital_route_node_operations(&mut world, player, &manager);
        let node_operations = world.resource::<CapitalRouteNodeOperationRegistry>();
        let start_operation = node_operations
            .operations
            .get(&CapitalRouteNodeOperationId {
                topology: CapitalRouteTopologyKey {
                    player_id: player,
                    island_id: island,
                },
                node: start_node,
            })
            .expect("静的DAGの開始Nodeを作戦台帳へ展開する");
        assert_eq!(
            start_operation.state,
            victory_roadmap::RoadmapNodeState::Secured,
            "自首都を始点Nodeとして盤面投影する"
        );
    }

    #[test]
    fn map26_builds_a_forward_dag_independently_for_each_player() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_26",
            GridTopology::Square,
        )
        .expect("map_26を初期化できる");
        let player_one = PlayerId(1);
        let player_two = PlayerId(2);
        let capital_for = |world: &World, player| {
            world
                .iter_entities()
                .find_map(|entity| {
                    let position = entity.get::<GridPosition>()?;
                    let property = entity.get::<Property>()?;
                    (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                        .then_some(*position)
                })
                .expect("各勢力の首都")
        };
        let player_one_capital = capital_for(&world, player_one);
        let player_two_capital = capital_for(&world, player_two);
        let island_id = world
            .resource::<crate::ai::islands::IslandMap>()
            .get_island_at(&player_one_capital)
            .expect("首都の島")
            .id;

        prepare_capital_route_topologies(&mut world, player_one);
        prepare_capital_route_topologies(&mut world, player_two);

        let registry = world.resource::<CapitalRouteTopologyRegistry>();
        let first = registry
            .topologies
            .get(&CapitalRouteTopologyKey {
                player_id: player_one,
                island_id,
            })
            .expect("先攻のDAG");
        let second = registry
            .topologies
            .get(&CapitalRouteTopologyKey {
                player_id: player_two,
                island_id,
            })
            .expect("後攻のDAG");
        assert_eq!(first.nodes[first.start_node.0].anchor, player_one_capital);
        assert_eq!(second.nodes[second.start_node.0].anchor, player_two_capital);
        for topology in [first, second] {
            assert!(topology.routes.len() >= 2, "橋の北・南を別の入口として持つ");
            assert!(topology.nodes.iter().all(|node| {
                node.successor_nodes
                    .iter()
                    .all(|successor| topology.nodes[successor.0].progress > node.progress)
            }));
            let mut inbound = vec![0_u32; topology.nodes.len()];
            for node in &topology.nodes {
                for successor in &node.successor_nodes {
                    inbound[successor.0] = inbound[successor.0].saturating_add(1);
                }
            }
            assert!(
                inbound.iter().any(|count| *count >= 2),
                "分岐した橋回廊が後段で合流するDAGになっている"
            );
        }
    }

    #[test]
    fn map26_bridge_node_requires_a_ground_unit_to_reach_its_exit() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_26",
            GridTopology::Square,
        )
        .expect("map_26を初期化できる");
        let player = PlayerId(1);
        let island_id = world
            .iter_entities()
            .find_map(|entity| {
                let position = entity.get::<GridPosition>()?;
                let property = entity.get::<Property>()?;
                (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                    .then_some(*position)
            })
            .and_then(|capital| {
                world
                    .resource::<crate::ai::islands::IslandMap>()
                    .get_island_at(&capital)
                    .map(|island| island.id)
            })
            .expect("先攻首都の島");
        prepare_capital_route_topologies(&mut world, player);
        ensure_capital_route_node_operations(&mut world, player);

        let topology_key = CapitalRouteTopologyKey {
            player_id: player,
            island_id,
        };
        let topology = world
            .resource::<CapitalRouteTopologyRegistry>()
            .topologies
            .get(&topology_key)
            .cloned()
            .expect("先攻の首都攻略DAG");
        let operation_count = world
            .resource::<CapitalRouteNodeOperationRegistry>()
            .operations
            .values()
            .filter(|operation| operation.id.topology == topology_key)
            .count();
        assert!(
            operation_count < topology.nodes.len(),
            "移動用の全セルを作戦Nodeへ昇格させず、地形・拠点Milestoneへ圧縮する: {operation_count}/{}",
            topology.nodes.len()
        );
        let joined_operation = world
            .resource::<CapitalRouteNodeOperationRegistry>()
            .operations
            .values()
            .find(|operation| {
                operation.id.topology == topology_key && operation.predecessors.len() >= 2
            })
            .cloned()
            .expect("map_26の圧縮DAGには代替回廊が合流するMilestoneがある");
        let mut simulated_operations = world
            .resource::<CapitalRouteNodeOperationRegistry>()
            .operations
            .clone();
        for predecessor in &joined_operation.predecessors {
            simulated_operations
                .get_mut(predecessor)
                .expect("合流前Node")
                .state = victory_roadmap::RoadmapNodeState::Locked;
        }
        simulated_operations
            .get_mut(&joined_operation.predecessors[0])
            .expect("選択した経路の前Node")
            .state = victory_roadmap::RoadmapNodeState::Secured;
        assert!(
            capital_route_node_predecessors_released(&simulated_operations, &joined_operation),
            "地理的な合流は全前任Nodeの同時制圧を要求せず、到達済みの一経路を使う"
        );
        let bridge_operation = world
            .resource::<CapitalRouteNodeOperationRegistry>()
            .operations
            .values()
            .filter(|operation| operation.id.topology == topology_key)
            .filter(|operation| operation.crossing_exit.is_some())
            .min_by_key(|operation| topology.nodes[operation.id.node.0].progress)
            .cloned()
            .expect("map_26の先攻DAGには橋を越えるNodeがある");
        assert!(
            world
                .resource::<CapitalRouteNodeOperationRegistry>()
                .operations
                .values()
                .any(|operation| {
                    operation.id.topology == topology_key
                        && operation.scope == CapitalRouteNodeScope::Area
                }),
            "map_26の分岐/合流Nodeは地形スキャンでAreaとして分類する"
        );
        assert_eq!(
            bridge_operation.scope,
            CapitalRouteNodeScope::Point,
            "橋は局地Areaではなく、出口へ通過すべきPoint Nodeとして分類する"
        );
        let crossing_exit = bridge_operation.crossing_exit.expect("橋向こうの出口");
        let bridge_progress = topology.nodes[bridge_operation.id.node.0].progress;

        // 前段を確保済み・優勢にして、橋Nodeだけの通過条件を単独で観測できるようにする。
        // 実際の対局ではこの状態は前段NodeのCapture/Control完了後に得られる。
        let property_entities = world
            .iter_entities()
            .filter_map(|entity| entity.contains::<Property>().then_some(entity.id()))
            .collect::<Vec<_>>();
        for property in property_entities {
            world
                .entity_mut(property)
                .get_mut::<Property>()
                .expect("Propertyを持つEntity")
                .owner_id = Some(player);
        }
        for node in topology
            .nodes
            .iter()
            .filter(|node| node.progress < bridge_progress)
        {
            world.spawn((
                Faction(player),
                node.anchor,
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    cost: 100_000,
                    movement_type: MovementType::Tank,
                    ..UnitStats::mock()
                },
            ));
        }
        let manager = crate::ai::squad::SquadManager::default();
        refresh_capital_route_node_operations(&mut world, player, &manager);
        let bridge_id = bridge_operation.id;
        let operation = world
            .resource::<CapitalRouteNodeOperationRegistry>()
            .operations
            .get(&bridge_id)
            .expect("橋Nodeの状態");
        assert!(
            !operation.crossed && !capital_route_node_exit_satisfied(operation),
            "橋の手前まで前段を制圧しても、出口へ出るまではNodeを完了させない: {:?}",
            operation.state
        );

        // 橋の上に立っても未通過である。入口・橋上・出口を混同しない。
        world.spawn((
            Faction(player),
            bridge_operation.anchor,
            Health {
                current: 100,
                max: 100,
            },
            UnitStats {
                cost: 100_000,
                movement_type: MovementType::Tank,
                ..UnitStats::mock()
            },
        ));
        refresh_capital_route_node_operations(&mut world, player, &manager);
        let operation = world
            .resource::<CapitalRouteNodeOperationRegistry>()
            .operations
            .get(&bridge_id)
            .expect("橋上到着後のNode状態");
        assert!(
            !operation.crossed && !capital_route_node_exit_satisfied(operation),
            "橋上への到着だけではmap_26の通過Milestoneを閉じない: {:?}",
            operation.state
        );

        // 初めて向こう岸の非橋セルへ出た時点で、Control Nodeを後続Nodeへ解放できる。
        world.spawn((
            Faction(player),
            crossing_exit,
            Health {
                current: 100,
                max: 100,
            },
            UnitStats {
                cost: 100_000,
                movement_type: MovementType::Tank,
                ..UnitStats::mock()
            },
        ));
        refresh_capital_route_node_operations(&mut world, player, &manager);
        let operation = world
            .resource::<CapitalRouteNodeOperationRegistry>()
            .operations
            .get(&bridge_id)
            .expect("橋頭堡到達後のNode状態");
        assert!(operation.crossed, "出口セルへの地上部隊到達を記録する");
        assert!(
            capital_route_node_exit_satisfied(operation),
            "橋頭堡を得たControl Nodeだけが後続Nodeを解放できる: {:?}",
            operation.state
        );
    }

    #[test]
    fn map6_mountain_routes_keep_a_milestone_on_each_corridor() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_6",
            GridTopology::Square,
        )
        .expect("map_6を初期化できる");
        let player = PlayerId(1);
        let own_capital = world
            .iter_entities()
            .find_map(|entity| {
                let position = entity.get::<GridPosition>()?;
                let property = entity.get::<Property>()?;
                (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                    .then_some(*position)
            })
            .expect("自首都");
        let island_id = world
            .resource::<crate::ai::islands::IslandMap>()
            .get_island_at(&own_capital)
            .expect("首都の島")
            .id;
        let requirement = crate::ai::island_campaign::IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 4,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 4_000,
        };
        let assignment = crate::ai::island_campaign::IslandCampaignAssignment {
            island_id,
            decision: crate::ai::island_campaign::IslandCampaignDecision::Secure,
            target_position: own_capital,
            capture_target_positions: vec![own_capital],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: requirement,
            allocated_budget: 4_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        let mut portfolio = crate::ai::island_campaign::IslandCampaignPortfolio {
            active_offensives: vec![assignment],
            ..crate::ai::island_campaign::IslandCampaignPortfolio::default()
        };

        refine_same_land_route_milestones(&world, player, &mut portfolio);

        let targets = &portfolio.active_offensives[0].capture_target_positions;
        let fronts = same_land_route_fronts(&world, player, island_id);
        let represented_routes = targets
            .iter()
            .filter_map(|target| {
                fronts
                    .iter()
                    .enumerate()
                    .min_by_key(|(route, front)| {
                        (
                            world
                                .resource::<Map>()
                                .distance(target.x, target.y, front.x, front.y),
                            *route,
                        )
                    })
                    .map(|(route, _)| route)
            })
            .collect::<HashSet<_>>();
        assert_eq!(
            represented_routes.len(),
            2,
            "山の二回廊をロードマップへ残し、片側の占領目標だけでcampaignを上書きしない: {targets:?}"
        );
    }

    #[test]
    fn route_force_slots_follow_bulk_demand_without_minimum_per_route() {
        let north = RouteForceDemand {
            opportunity_value: 6_000,
            force_deficit_value: 3_000,
            demand_value: 9_000,
        };
        let south = RouteForceDemand {
            opportunity_value: 2_000,
            force_deficit_value: 1_000,
            demand_value: 3_000,
        };
        assert_eq!(apportion_route_force_slots(6, &[north, south]), vec![5, 1]);

        let dormant = RouteForceDemand {
            opportunity_value: 0,
            force_deficit_value: 0,
            demand_value: 0,
        };
        assert_eq!(
            apportion_route_force_slots(6, &[dormant, north]),
            vec![0, 6],
            "需要ゼロのrouteへ最低一枠を強制しない"
        );
    }

    #[test]
    fn map26_bridge_route_fronts_have_post_bridge_positions() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_26",
            GridTopology::Square,
        )
        .expect("map_26 initialization should succeed");
        let player = PlayerId(1);
        let own_capital = world
            .iter_entities()
            .find_map(|entity| {
                let position = entity.get::<GridPosition>()?;
                let property = entity.get::<Property>()?;
                (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                    .then_some(*position)
            })
            .expect("own capital");
        let island = world
            .resource::<crate::ai::islands::IslandMap>()
            .get_island_at(&own_capital)
            .expect("capital island")
            .id;

        let gates = same_land_route_fronts(&world, player, island);
        let bridgeheads = same_land_route_breakthrough_positions(&world, player, island);

        assert_eq!(gates.len(), 2, "map_26 gates={gates:?}");
        assert_eq!(bridgeheads.len(), 2, "map_26 bridgeheads={bridgeheads:?}");
        let distances = armored_step_distances(world.resource::<Map>(), &master_data, own_capital);
        for (gate, bridgehead) in gates.iter().zip(&bridgeheads) {
            assert!(
                distances[bridgehead] > distances[gate],
                "gate={gate:?}, bridgehead={bridgehead:?}"
            );
        }

        assert_eq!(
            same_land_pending_bridgeheads(&world, player, island),
            bridgeheads.iter().copied().map(Some).collect::<Vec<_>>(),
            "開始時点では橋向こうへ地上部隊がいないため、両軸とも突破未了"
        );
        assert_eq!(
            same_land_armored_entry_shortfall(&world, player, island),
            0,
            "占領vanguardが遠い初手は、橋突破tankより収入・占領を先に進める"
        );
        world.spawn((
            Faction(player),
            gates[0],
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                can_capture: true,
                max_movement: 3,
                ..UnitStats::mock()
            },
        ));
        assert_eq!(
            same_land_armored_entry_shortfall(&world, player, island),
            1,
            "一体のvanguardを二つのrouteへ二重計上しない"
        );
        world.spawn((
            Faction(player),
            gates[1],
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                can_capture: true,
                max_movement: 3,
                ..UnitStats::mock()
            },
        ));
        assert_eq!(
            same_land_armored_entry_shortfall(&world, player, island),
            2,
            "各routeの占領vanguardがtank生産より先着する時だけ突破要求を開く"
        );
        world.spawn((
            Faction(player),
            bridgeheads[0],
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                ..UnitStats::mock()
            },
        ));
        assert_eq!(
            same_land_pending_bridgeheads(&world, player, island),
            vec![None, Some(bridgeheads[1])],
            "片方の橋頭堡へ地上部隊が入れば、そのルートだけ通過Milestoneを閉じる"
        );
        assert_eq!(
            same_land_armored_entry_shortfall(&world, player, island),
            1,
            "突破済みrouteは要求から消え、残る未突破routeだけを生産対象にする"
        );
    }

    fn flat_map(width: usize, height: usize) -> Map {
        Map {
            width,
            height,
            tiles: vec![Terrain::Plains; width * height],
            topology: GridTopology::Square,
        }
    }

    fn pos(x: usize, y: usize) -> GridPosition {
        GridPosition { x, y }
    }

    fn capture_objective(anchor: GridPosition) -> Vec<CampaignPlanningObjective> {
        vec![CampaignPlanningObjective {
            island_id: crate::ai::islands::IslandId(0),
            kind: OperationKind::Capture,
            anchor,
            objective_properties: vec![anchor],
            capture_eta: None,
            required_capture_survivors: 0,
            logistics_rank: Some(0),
            forced_target_enemies: HashSet::new(),
            protected_capture_entities: HashSet::new(),
            staging_anchor: anchor,
            execution_authorized: true,
        }]
    }

    #[test]
    fn enemy_production_forecast_learns_new_entities_and_records_next_turn_error() {
        let player = PlayerId(1);
        let mut world = World::new();
        let mut scan = multi_factory_scan();
        scan.enemy_production_slots = 3;

        // 初回の盤面は開幕配置を含むため、実生産として学習しない。
        assert_eq!(
            observe_enemy_production(&mut world, player, 1, &scan).expected_units_next_turn,
            0
        );

        let tank = scan
            .available_types
            .iter()
            .find(|(unit_type, _)| *unit_type == UnitType::Tank)
            .map(|(_, stats)| stats.clone())
            .expect("戦車のマスターデータ");
        let produced = world.spawn_empty().id();
        scan.enemy_units = vec![UnitSnapshot {
            entity: Some(produced),
            pos: pos(7, 2),
            stats: tank.clone(),
            hp: 100,
            free_cargo: 0,
        }];
        let predicted = observe_enemy_production(&mut world, player, 2, &scan);
        assert_eq!(predicted.expected_units_next_turn, 1);
        assert_eq!(predicted.expected_cost_next_turn, tank.cost);
        assert_eq!(predicted.dominant_unit_type, Some(UnitType::Tank));
        assert_eq!(predicted.evaluated_samples, 0);

        // 次手番に実生産が無ければ、直前の「1体」予測を誤差として採点する。
        scan.enemy_units.clear();
        let audited = observe_enemy_production(&mut world, player, 3, &scan);
        assert_eq!(audited.evaluated_samples, 1);
        assert_eq!(audited.mean_absolute_unit_error, 1);
        assert_eq!(audited.mean_absolute_cost_error, tank.cost);
    }

    #[test]
    fn campaign_objective_is_the_only_capture_source_and_keeps_exact_anchor() {
        let mut scan = multi_factory_scan();
        let campaign_anchor = pos(8, 2);
        scan.campaign_objectives = vec![CampaignPlanningObjective {
            island_id: crate::ai::islands::IslandId(0),
            kind: OperationKind::Capture,
            anchor: campaign_anchor,
            objective_properties: vec![campaign_anchor],
            capture_eta: Some(3),
            required_capture_survivors: 1,
            logistics_rank: Some(0),
            forced_target_enemies: HashSet::new(),
            protected_capture_entities: HashSet::new(),
            staging_anchor: campaign_anchor,
            execution_authorized: true,
        }];

        let operations = build_operations(&scan, &mut ReachCtx::default(), &[]);
        let captures = operations
            .iter()
            .filter(|operation| operation.kind == OperationKind::Capture)
            .collect::<Vec<_>>();

        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].anchor, campaign_anchor);
    }

    #[test]
    fn capital_objective_keeps_local_front_as_an_independent_operation() {
        let mut scan = multi_factory_scan();
        let local_anchor = pos(6, 2);
        let capital = pos(8, 2);
        let second_front = pos(7, 1);
        let island_id = crate::ai::islands::IslandId(0);
        scan.open_properties = vec![local_anchor, second_front, capital];
        scan.campaign_objectives = vec![
            CampaignPlanningObjective {
                island_id,
                kind: OperationKind::Capture,
                anchor: local_anchor,
                objective_properties: vec![local_anchor, second_front],
                capture_eta: Some(3),
                required_capture_survivors: 1,
                logistics_rank: Some(0),
                forced_target_enemies: HashSet::new(),
                protected_capture_entities: HashSet::new(),
                staging_anchor: local_anchor,
                execution_authorized: true,
            },
            CampaignPlanningObjective {
                island_id,
                kind: OperationKind::AssaultCapital,
                anchor: capital,
                objective_properties: vec![capital],
                capture_eta: None,
                required_capture_survivors: 0,
                logistics_rank: None,
                forced_target_enemies: HashSet::new(),
                protected_capture_entities: HashSet::new(),
                staging_anchor: local_anchor,
                execution_authorized: false,
            },
        ];

        let active = ActivePlanObjective {
            kind: OperationKind::AssaultCapital,
            island_id: Some(island_id),
            properties: vec![capital],
            target_enemies: HashSet::new(),
        };
        let operations = build_operations(&scan, &mut ReachCtx::default(), &[active]);
        let same_island = operations
            .iter()
            .filter(|operation| operation.island_id == Some(island_id))
            .collect::<Vec<_>>();
        assert_eq!(
            same_island.len(),
            2,
            "継続PlanとRoadmapが同じ首都作戦を提示しても二重化しない"
        );
        let local = same_island
            .iter()
            .find(|operation| operation.kind == OperationKind::Capture)
            .expect("局地Capture作戦");
        assert_eq!(local.anchor, local_anchor);
        assert_eq!(local.objective_properties, vec![second_front, local_anchor]);
        assert_eq!(local.slots.capture_units, 2);
        assert!(!local.facts.requires_transport);

        let assault = same_island
            .iter()
            .find(|operation| operation.kind == OperationKind::AssaultCapital)
            .expect("勝利ロードマップの首都作戦");
        // 首都は論理目標として保持し、見積りと接敵の基準だけを未完了DAG区間へ置く。
        assert_eq!(assault.anchor, local_anchor);
        assert_eq!(assault.objective_properties, vec![capital]);
        assert_eq!(assault.slots.capture_units, 1);
        assert!(!assault.execution_authorized);
    }

    /// 到達ターン数は距離を移動力で割り上げた値になる
    #[test]
    fn eta_is_distance_divided_by_movement() {
        let map = flat_map(20, 20);
        assert_eq!(eta_turns(&map, &pos(0, 0), &pos(6, 0), 3), 2);
        assert_eq!(eta_turns(&map, &pos(0, 0), &pos(7, 0), 3), 3);
        // 移動力 0 でも 0 除算しない
        assert_eq!(eta_turns(&map, &pos(0, 0), &pos(2, 0), 0), 2);
    }

    /// 期限までに来られない敵は、単なる最寄り作戦へ押し込まない。
    #[test]
    fn enemy_is_assigned_only_when_it_can_arrive_before_an_objective_deadline() {
        let scan = multi_factory_scan();
        let mut ctx = ReachCtx::default();
        let anchors = vec![pos(1, 2), pos(8, 2)];
        let enemy = pos(4, 2);

        // 近い側でも1ターンかかるため、今ターン完了の作戦には無関係。
        assert_eq!(
            nearest_relevant_anchor_index(
                &scan,
                &mut ctx,
                &enemy,
                MovementType::Infantry,
                3,
                &anchors,
                &[0, 1],
                &[true, true],
            ),
            None
        );
        // 期限内なら最短の1作戦だけへ決定的に帰属する。
        assert_eq!(
            nearest_relevant_anchor_index(
                &scan,
                &mut ctx,
                &enemy,
                MovementType::Infantry,
                3,
                &anchors,
                &[1, 1],
                &[true, true],
            ),
            Some(0)
        );
        // 未許可の首都編成は敵の局地帰属先にしない。敵本拠地側からでも期限内に
        // Capture前線へ来られる敵は、実行中の局地護衛計画が引き受ける。
        assert_eq!(
            nearest_relevant_anchor_index(
                &scan,
                &mut ctx,
                &pos(8, 2),
                MovementType::Infantry,
                3,
                &anchors,
                &[3, 12],
                &[true, false],
            ),
            Some(0)
        );
    }

    /// 既存Combat 1体を複数前線の撃破要求から同時に控除してはならない。
    #[test]
    fn existing_combat_sorties_are_committed_to_only_one_operation() {
        let infantry = UnitStats {
            can_capture: true,
            movement_type: MovementType::Infantry,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1_000)
        };
        let helicopter = UnitStats {
            movement_type: MovementType::Air,
            max_movement: 8,
            max_fuel: 99,
            min_range: 1,
            max_range: 1,
            ..stats(UnitType::Bcopters, 7_500)
        };
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 65);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Bcopters, 0);
        let anchors = vec![pos(5, 1), pos(15, 1)];
        let horizons = vec![5, 5];
        let scan = BoardScan {
            map: flat_map(20, 3).into(),
            master_data: MasterDataRegistry::load().unwrap().into(),
            damage_chart: damage_chart.into(),
            funds: 20_000,
            free_facilities: vec![(pos(0, 1), Terrain::Factory), (pos(1, 1), Terrain::Airport)],
            production_facilities: vec![
                (pos(0, 1), Terrain::Factory),
                (pos(1, 1), Terrain::Airport),
            ],
            available_types: vec![
                (UnitType::Infantry, infantry.clone()),
                (UnitType::Bcopters, helicopter.clone()),
            ],
            my_units: vec![UnitSnapshot {
                entity: Some(Entity::from_raw(900)),
                pos: pos(3, 1),
                stats: helicopter,
                hp: 100,
                free_cargo: 0,
            }],
            enemy_units: anchors
                .iter()
                .enumerate()
                .map(|(index, position)| UnitSnapshot {
                    entity: Some(Entity::from_raw(910 + index as u32)),
                    pos: *position,
                    stats: infantry.clone(),
                    hp: 100,
                    free_cargo: 0,
                })
                .collect(),
            owned_airport_count: 1,
            open_properties: anchors.clone(),
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 5_000,
            campaign_objectives: Vec::new(),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
            enemy_production_forecast: EnemyProductionForecastTrace::default(),
        };
        let mut ctx = ReachCtx::default();
        let near = build_operation(
            &scan,
            &mut ctx,
            &infantry,
            OperationKind::Capture,
            anchors[0],
            &anchors,
            &horizons,
            &[true, true],
            &[anchors[0]],
            &HashSet::new(),
            &HashSet::new(),
            2,
        );
        let far = build_operation(
            &scan,
            &mut ctx,
            &infantry,
            OperationKind::Capture,
            anchors[1],
            &anchors,
            &horizons,
            &[true, true],
            &[anchors[1]],
            &HashSet::new(),
            &HashSet::new(),
            5,
        );

        assert!(near.facts.friendly_combat_units_committed > 0);
        assert_eq!(far.facts.friendly_combat_units_committed, 0);
    }

    #[test]
    fn committed_combat_keeps_plan_ownership_and_accepts_operation_bound_reinforcements() {
        let own_entity = Entity::from_raw(900);
        let foreign_entity = Entity::from_raw(901);
        let immediate_entity = Entity::from_raw(902);
        let intercept_entity = Entity::from_raw(903);
        let own_plan = plan_revision::PlanId(7);
        let assignments = HashMap::from([
            (
                own_entity,
                deployment::ActiveTargetAssignment {
                    plan_id: Some(own_plan),
                    slot_kind: SlotKind::Combat,
                    targets: HashSet::new(),
                },
            ),
            (
                foreign_entity,
                deployment::ActiveTargetAssignment {
                    plan_id: Some(plan_revision::PlanId(8)),
                    slot_kind: SlotKind::Combat,
                    targets: HashSet::new(),
                },
            ),
            (
                immediate_entity,
                deployment::ActiveTargetAssignment {
                    plan_id: None,
                    slot_kind: SlotKind::Combat,
                    targets: HashSet::new(),
                },
            ),
            (
                intercept_entity,
                deployment::ActiveTargetAssignment {
                    plan_id: None,
                    slot_kind: SlotKind::Intercept,
                    targets: HashSet::new(),
                },
            ),
        ]);

        assert_eq!(
            committed_entities_for_plan(Some(&assignments), None),
            HashSet::from([immediate_entity]),
            "実在敵へ割り当てた即時Combat増援は、次の作戦見積でも継続して数える"
        );
        assert_eq!(
            committed_entities_for_plan(Some(&assignments), Some(own_plan)),
            HashSet::from([own_entity, immediate_entity]),
            "別PlanとInterceptは混ぜず、同じPlanと即時Combatだけを数える"
        );
    }

    #[test]
    fn advancing_route_registry_exposes_the_current_segment_roster() {
        let player = PlayerId(1);
        let island = crate::ai::islands::IslandId(4);
        let advancing = Entity::from_raw(904);
        let staged = Entity::from_raw(907);
        let holding = Entity::from_raw(905);
        let supplying = Entity::from_raw(906);
        let mut manager = crate::ai::squad::SquadManager::default();
        let advancing_squad = manager.create_owned_squad(MissionType::Attack, player);
        advancing_squad.members.insert(advancing);
        let advancing_squad_id = advancing_squad.id;
        let staged_squad = manager.create_owned_squad(MissionType::Attack, player);
        staged_squad.members.insert(staged);
        let staged_squad_id = staged_squad.id;
        let holding_squad = manager.create_owned_squad(MissionType::Defense, player);
        holding_squad.members.insert(holding);
        let holding_squad_id = holding_squad.id;
        let supplying_squad = manager.create_owned_squad(MissionType::Transport, player);
        supplying_squad.members.insert(supplying);
        let supplying_squad_id = supplying_squad.id;
        let commitment = |phase| CapitalRouteCommitment {
            player_id: player,
            island_id: island,
            route: CapitalRouteId(0),
            target: pos(5, 1),
            target_node: CapitalRouteNodeId(1),
            objective: CapitalRouteNodeObjective::Control,
            scope: CapitalRouteNodeScope::Point,
            control_area: vec![pos(5, 1)],
            capture_targets: Vec::new(),
            crossing_exit: None,
            crossed: false,
            execution_target: pos(5, 1),
            frontline_capacity: 1,
            path_nodes: vec![CapitalRouteNodeId(0), CapitalRouteNodeId(1)],
            path: vec![pos(0, 1), pos(5, 1)],
            phase,
        };
        let registry = CapitalRoutePathRegistry {
            commitments: HashMap::from([
                (
                    advancing_squad_id,
                    commitment(CapitalRouteExecutionPhase::Advance),
                ),
                (
                    staged_squad_id,
                    commitment(CapitalRouteExecutionPhase::Stage),
                ),
                (
                    holding_squad_id,
                    commitment(CapitalRouteExecutionPhase::Hold),
                ),
                (
                    supplying_squad_id,
                    commitment(CapitalRouteExecutionPhase::Supply),
                ),
            ]),
            commitment_members: HashMap::new(),
            assignment_diagnostics: HashMap::new(),
        };

        let advancing_entities = registry.advancing_combat_entities(player, &manager);
        assert_eq!(
            advancing_entities.by_island,
            HashMap::from([(island, HashSet::from([advancing]))]),
            "保持・補給Squadを見かけの攻略戦力にしない"
        );
        assert_eq!(
            advancing_entities.by_target,
            HashMap::from([((island, pos(5, 1)), HashSet::from([advancing]))]),
            "前進部隊は首都全体ではなく、現在のDAG区間目標へ渡す"
        );
        assert_eq!(
            advancing_entities.staged_by_target,
            HashMap::from([((island, pos(5, 1)), HashSet::from([staged]))]),
            "待機線も人数だけでなく、同じDAG区間へ向かうEntityとして保持する"
        );
    }

    #[test]
    fn staged_route_member_is_counted_after_its_front_arrival_eta() {
        let mut scan = mixed_threat_multi_factory_scan();
        let staged = Entity::from_raw(903);
        let anti_air = scan
            .available_types
            .iter()
            .find(|(unit_type, _)| *unit_type == UnitType::AntiAir)
            .map(|(_, stats)| stats.clone())
            .expect("fixture has an anti-air counter");
        scan.my_units.push(UnitSnapshot {
            entity: Some(staged),
            pos: pos(1, 1),
            stats: anti_air,
            hp: 100,
            free_cargo: 0,
        });
        let mut ctx = ReachCtx::default();
        let operations = build_operations(&scan, &mut ctx, &[]);
        let operation = operations
            .iter()
            .find(|operation| operation.kind == OperationKind::Capture)
            .expect("capture operation");

        let input = combat_plan_input(
            &scan,
            &mut ctx,
            operation,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::from([staged]),
            scan.funds,
            false,
        )
        .expect("visible enemy creates a combat plan input");

        assert_eq!(input.existing_units.len(), 1);
        assert!(
            input.existing_units[0].available_turn > 0,
            "Stage部隊を前衛と同じ手番の火力にせず、到着後の既存戦力として評価する"
        );
    }

    #[test]
    fn combat_plan_projects_only_the_capturer_ordered_this_turn() {
        let mut scan = mixed_threat_multi_factory_scan();
        let anchor = pos(6, 2);
        let second_front = pos(7, 2);
        scan.open_properties = vec![anchor, second_front];
        scan.campaign_objectives = vec![CampaignPlanningObjective {
            island_id: crate::ai::islands::IslandId(0),
            kind: OperationKind::Capture,
            anchor,
            objective_properties: vec![anchor, second_front],
            capture_eta: None,
            required_capture_survivors: 2,
            logistics_rank: Some(0),
            forced_target_enemies: HashSet::new(),
            protected_capture_entities: HashSet::new(),
            staging_anchor: anchor,
            execution_authorized: true,
        }];
        let mut ctx = ReachCtx::default();
        let mut operations = build_operations(&scan, &mut ctx, &[]);
        let operation = operations
            .iter_mut()
            .find(|operation| operation.kind == OperationKind::Capture)
            .expect("局地Capture作戦");
        // 同じ生産手番で、占領兵1体を先に発注した状態を再現する。
        operation.filled.capture_units = 1;

        let input = combat_plan_input(
            &scan,
            &mut ctx,
            operation,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            scan.funds,
            false,
        )
        .expect("観測敵がいるためCombat計画入力が作られる");

        assert_eq!(input.protected_units.len(), 1);
        assert_eq!(input.required_capture_survivors, 2);
        assert!(
            input
                .protected_units
                .iter()
                .all(|unit| unit.available_turn > 0)
        );
        assert!(input.capture_completion_turn.is_none());
        assert_eq!(input.current_funds, scan.funds);
    }

    /// 実生産ペースを、期限内に到着できる最寄りの1作戦だけへ見積もる。
    #[test]
    fn enemy_reinforcement_funds_is_local_unique_and_deadline_bounded() {
        let mut scan = multi_factory_scan();
        scan.enemy_income = 6000;
        scan.enemy_production_slots = 1;
        scan.enemy_production_forecast = EnemyProductionForecastTrace {
            expected_cost_next_turn: 1_000,
            ..EnemyProductionForecastTrace::default()
        };
        scan.enemy_facilities = vec![EnemyFacilitySnapshot {
            pos: pos(8, 2),
            terrain: Terrain::Factory,
        }];
        let anchors = vec![pos(6, 2), pos(0, 2)];
        let horizons = vec![4, 4];
        let mut ctx = ReachCtx::default();

        // 最安の歩兵は1ターンで到着する。Expectedは次の一波だけ、Stressは
        // 作戦期限までの3生産ターンを局地予算にする。
        let local = projected_enemy_reinforcement_envelope(&scan, &mut ctx, &anchors, &horizons, 0);
        assert_eq!(local.expected_funds, 1000);
        assert_eq!(local.stress_funds, 3000);
        assert_eq!(
            projected_enemy_reinforcement_envelope(&scan, &mut ctx, &anchors, &horizons, 1),
            EnemyReinforcementEnvelope::default()
        );

        // 到着期限が0なら、この施設はどの作戦の脅威にもならない。
        assert_eq!(
            projected_enemy_reinforcement_envelope(&scan, &mut ctx, &anchors, &[0, 0], 0),
            EnemyReinforcementEnvelope::default()
        );
    }

    #[test]
    fn expected_reinforcement_starts_production_without_changing_the_launch_gate() {
        let mut scan = multi_factory_scan();
        scan.enemy_income = 6_000;
        scan.enemy_production_slots = 1;
        scan.enemy_facilities = vec![EnemyFacilitySnapshot {
            pos: pos(8, 2),
            terrain: Terrain::Factory,
        }];
        // 初回観測は開幕配置と区別できないためforecastが空でも、敵施設と収入が
        // ある限りExpectedを0にしない。
        scan.enemy_production_forecast = EnemyProductionForecastTrace::default();
        let mut ctx = ReachCtx::default();
        let operations = build_operations(&scan, &mut ctx, &[]);
        let capture = operations
            .iter()
            .find(|operation| operation.kind == OperationKind::Capture)
            .expect("capture objective");

        assert!(!capture.expected_reinforcements.is_empty());
        assert!(capture.facts.enemy_reinforcement_funds > 0);
        assert_eq!(
            capture.slots.combat_plan_required, 1,
            "Expectedは継続生産を起動する"
        );
        assert!(
            !scan.capital_assault_authorized,
            "Expectedを増やしてもDAG・兵站の進撃Goを変更しない"
        );
    }

    #[test]
    fn unauthorized_capital_formation_does_not_hide_reinforcements_from_capture() {
        let mut scan = multi_factory_scan();
        scan.enemy_income = 6_000;
        scan.enemy_production_slots = 1;
        scan.enemy_production_forecast = EnemyProductionForecastTrace {
            expected_cost_next_turn: 1_000,
            ..EnemyProductionForecastTrace::default()
        };
        scan.enemy_facilities = vec![EnemyFacilitySnapshot {
            pos: pos(8, 2),
            terrain: Terrain::Factory,
        }];
        let capital = pos(8, 2);
        let capture = pos(6, 2);
        scan.campaign_objectives = vec![
            CampaignPlanningObjective {
                island_id: crate::ai::islands::IslandId(1),
                kind: OperationKind::AssaultCapital,
                anchor: capital,
                objective_properties: vec![capital],
                capture_eta: None,
                required_capture_survivors: 0,
                logistics_rank: None,
                forced_target_enemies: HashSet::new(),
                protected_capture_entities: HashSet::new(),
                staging_anchor: pos(0, 2),
                execution_authorized: false,
            },
            CampaignPlanningObjective {
                island_id: crate::ai::islands::IslandId(2),
                kind: OperationKind::Capture,
                anchor: capture,
                objective_properties: vec![capture],
                capture_eta: Some(4),
                required_capture_survivors: 3,
                logistics_rank: Some(0),
                forced_target_enemies: HashSet::new(),
                protected_capture_entities: HashSet::new(),
                staging_anchor: capture,
                execution_authorized: true,
            },
        ];
        let anchors = vec![capital, capture];
        let horizons = vec![12, 4];
        let mut ctx = ReachCtx::default();

        assert_eq!(
            projected_enemy_reinforcement_envelope(&scan, &mut ctx, &anchors, &horizons, 0),
            EnemyReinforcementEnvelope::default()
        );
        assert!(
            projected_enemy_reinforcement_envelope(&scan, &mut ctx, &anchors, &horizons, 1)
                .expected_funds
                > 0
        );
    }

    #[test]
    fn enemy_reinforcement_assessment_places_each_purchase_on_the_time_axis() {
        let mut scan = multi_factory_scan();
        scan.enemy_income = 1_000;
        scan.enemy_production_slots = 1;
        scan.enemy_facilities = vec![EnemyFacilitySnapshot {
            pos: pos(8, 2),
            terrain: Terrain::Factory,
        }];
        Arc::make_mut(&mut scan.damage_chart).insert_damage(UnitType::Infantry, UnitType::Tank, 20);
        Arc::make_mut(&mut scan.damage_chart).insert_damage(UnitType::Tank, UnitType::Infantry, 80);
        let mut op = operation(
            OperationKind::Capture,
            OperationSlots::default(),
            OperationSlots::default(),
        );
        op.anchor = pos(6, 2);
        op.facts.enemy_reinforcement_funds = 3_000;

        let assessment = enemy_reinforcement_assessment(
            &scan,
            &mut ReachCtx::default(),
            &op,
            5,
            op.facts.enemy_reinforcement_funds,
            ReinforcementScenario::Stress,
        );
        let mut arrivals = assessment
            .reinforcements
            .iter()
            .map(|enemy| (enemy.available_turn, enemy.stats.unit_type))
            .chain(
                assessment
                    .contingencies
                    .iter()
                    .map(|plan| (plan.enemy_contact_turn, plan.enemy_type)),
            )
            .collect::<Vec<_>>();
        arrivals.sort_unstable_by_key(|(turn, _)| *turn);

        assert_eq!(arrivals.len(), 3);
        assert_eq!(
            arrivals.iter().map(|(turn, _)| *turn).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        assert!(
            arrivals
                .iter()
                .all(|(_, unit_type)| *unit_type == UnitType::Infantry)
        );
    }

    #[test]
    fn contingency_reserves_only_the_funding_gap_before_future_income() {
        let plans = vec![
            ReinforcementContingency {
                enemy_type: UnitType::Infantry,
                enemy_contact_turn: 3,
                counter_type: UnitType::Bcopters,
                counter_facility: pos(1, 1),
                counter_build_turn: 1,
                counter_contact_turn: 3,
                attacks_required: 2,
                reserve_cost: 7_500,
            },
            ReinforcementContingency {
                enemy_type: UnitType::Infantry,
                enemy_contact_turn: 4,
                counter_type: UnitType::Bcopters,
                counter_facility: pos(2, 1),
                counter_build_turn: 2,
                counter_contact_turn: 4,
                attacks_required: 2,
                reserve_cost: 7_500,
            },
        ];

        assert_eq!(contingency_reserve_now(&plans, 5_000), 5_000);
        assert_eq!(contingency_reserve_now(&plans, 8_000), 0);
    }

    /// テスト用のユニット諸元。
    fn stats(unit_type: UnitType, cost: u32) -> UnitStats {
        let master = MasterDataRegistry::load().unwrap();
        let weapon_stats = master
            .create_unit_stats(&crate::resources::master_data::UnitName(
                unit_type.as_str().to_owned(),
            ))
            .unwrap();
        UnitStats {
            unit_type,
            cost,
            max_ammo1: weapon_stats.max_ammo1,
            max_ammo2: weapon_stats.max_ammo2,
            min_range: weapon_stats.min_range,
            max_range: weapon_stats.max_range,
            ..UnitStats::mock()
        }
    }

    /// 揚陸判定用のマップを組み立てる。
    ///
    /// 横一列のレーンを、左の陸地／中央の海／右の陸地に区切る。
    /// `landing` に地形を与えると右岸の入口 (x=6, y=1) をその地形に差し替えられる。
    /// 例: `Terrain::Shoal` を与えれば艦船が接岸できる揚陸地点になり、
    /// `None` のままなら海から陸へ乗り上げる手段が無いマップになる。
    ///
    /// レイアウト（各行 x=0..8 共通）:
    ///   Plains Port | Sea Sea Sea Sea | (landing) Plains Plains
    fn strait_map(landing: Option<Terrain>) -> Map {
        let width = 9;
        let height = 3;
        let mut tiles = vec![Terrain::Sea; width * height];
        for y in 0..height {
            for x in 0..width {
                let terrain = match x {
                    0 => Terrain::Plains,
                    1 => Terrain::Port,
                    6 => landing.unwrap_or(Terrain::Sea),
                    7..=8 => Terrain::Plains,
                    _ => Terrain::Sea,
                };
                tiles[y * width + x] = terrain;
            }
        }
        Map {
            width,
            height,
            tiles,
            topology: GridTopology::Square,
        }
    }

    /// 揚陸地点（港・浅瀬）が対岸にあれば、艦船は陸上ユニットを目標へ届けられる
    #[test]
    fn ship_can_deliver_land_cargo_through_a_beachhead() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(Some(Terrain::Shoal));
        let mut ctx = ReachCtx::default();

        assert!(can_deliver_cargo(
            &map,
            &registry,
            &mut ctx,
            &pos(1, 1),
            &pos(8, 1),
            MovementType::Ship,
            MovementType::Infantry,
        ));
    }

    /// 対岸に接岸できる地形が無ければ、隣接マスが陸地でも積荷は降ろせない
    ///
    /// 「目標の隣まで行けるか」で判定すると、海に浮いたままの艦船が
    /// 陸へ積荷を降ろせることになってしまうため、この区別が必要。
    #[test]
    fn ship_cannot_deliver_land_cargo_without_a_beachhead() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(None);
        let mut ctx = ReachCtx::default();

        // 対岸の陸地 (7,1) は海 (6,1) と隣接しているが、
        // 艦船は海の上では降車できないので不成立。
        assert!(!can_deliver_cargo(
            &map,
            &registry,
            &mut ctx,
            &pos(1, 1),
            &pos(8, 1),
            MovementType::Ship,
            MovementType::Infantry,
        ));
    }

    #[test]
    fn ranged_ship_can_join_land_assault_from_reachable_firing_envelope() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(None);
        let mut ctx = ReachCtx::default();

        assert!(!ctx.is_reachable(&map, &registry, (1, 1), (8, 1), MovementType::Ship,));
        assert!(ctx.can_reach_engagement_envelope(
            &map,
            &registry,
            (1, 1),
            (8, 1),
            MovementType::Ship,
            3,
        ));
    }

    /// 自陣側の港からでも、自陣の陸地が目標なら当然届けられる（退行検出用）
    #[test]
    fn ship_can_deliver_cargo_back_to_its_own_shore() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(None);
        let mut ctx = ReachCtx::default();

        assert!(can_deliver_cargo(
            &map,
            &registry,
            &mut ctx,
            &pos(1, 1),
            &pos(0, 1),
            MovementType::Ship,
            MovementType::Infantry,
        ));
    }

    /// 艦船以外の輸送（空輸など）は地形に縛られず、どこへでも降ろせる
    #[test]
    fn air_transport_is_not_restricted_by_landing_terrain() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = strait_map(None);
        let mut ctx = ReachCtx::default();

        assert!(can_deliver_cargo(
            &map,
            &registry,
            &mut ctx,
            &pos(1, 1),
            &pos(8, 1),
            MovementType::Air,
            MovementType::Infantry,
        ));
    }

    /// Combat計画は購入価格ではなく、期限内の具体的な攻撃列で充足する。
    #[test]
    fn combat_plan_does_not_treat_purchase_price_as_destroyed_enemy_value() {
        let infantry = UnitStats {
            can_capture: true,
            movement_type: MovementType::Infantry,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1_000)
        };
        let helicopter = UnitStats {
            movement_type: MovementType::Air,
            max_movement: 8,
            ..stats(UnitType::Bcopters, 7_500)
        };
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 65);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Bcopters, 0);
        let enemy_positions = [pos(8, 0), pos(8, 1), pos(8, 2)];
        let scan = BoardScan {
            map: flat_map(10, 3).into(),
            master_data: MasterDataRegistry::load().unwrap().into(),
            damage_chart: damage_chart.into(),
            funds: 22_500,
            free_facilities: vec![
                (pos(0, 1), Terrain::Factory),
                (pos(1, 0), Terrain::Airport),
                (pos(1, 1), Terrain::Airport),
                (pos(1, 2), Terrain::Airport),
            ],
            production_facilities: vec![
                (pos(0, 1), Terrain::Factory),
                (pos(1, 0), Terrain::Airport),
                (pos(1, 1), Terrain::Airport),
                (pos(1, 2), Terrain::Airport),
            ],
            available_types: vec![
                (UnitType::Infantry, infantry.clone()),
                (UnitType::Bcopters, helicopter),
            ],
            my_units: Vec::new(),
            enemy_units: enemy_positions
                .iter()
                .enumerate()
                .map(|(index, position)| UnitSnapshot {
                    entity: Some(Entity::from_raw(1_000 + index as u32)),
                    pos: *position,
                    stats: infantry.clone(),
                    hp: 100,
                    free_cargo: 0,
                })
                .collect(),
            owned_airport_count: 3,
            open_properties: enemy_positions.to_vec(),
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 5_000,
            campaign_objectives: capture_objective(enemy_positions[0]),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
            enemy_production_forecast: EnemyProductionForecastTrace::default(),
        };

        let (commands, trace) = plan_production(&scan, PlayerId(1), false, &HashMap::new());

        // 占領役を1体確保してもCombat計画を消さず、65%攻撃を何回実行できるかを
        // シミュレーションした攻撃列を保持する。実弾薬では1機が6回攻撃して
        // 3体を排除できるため、価格を敵価値へ換算したCombat要求にはしない。
        // 余剰生産は、同じ前線に到着して追加の有効打を与えられる時だけ出す。
        // このfixtureでは一機が弾薬内で全3体を排除できるため、価格だけで二機目を
        // 強制しない。これはplanの必要数を敵価値へ換算しないことの確認でもある。
        assert_eq!(commands.len(), 2, "commands={commands:?}, trace={trace:?}");
        assert_eq!(
            commands
                .iter()
                .filter(|command| command.unit_type == UnitType::Infantry)
                .count(),
            1
        );
        assert_eq!(
            commands
                .iter()
                .filter(|command| command.unit_type == UnitType::Bcopters)
                .count(),
            1
        );
        let combat_plan = &trace.rolling_combat_plans[0];
        assert_eq!(combat_plan.targets.len(), 3);
        assert!(
            combat_plan
                .targets
                .iter()
                .all(|target| target.remaining_hp == 0),
            "combat_plan={combat_plan:?}"
        );
        assert_eq!(combat_plan.purchases.len(), 1);
        assert_eq!(
            trace
                .steps
                .iter()
                .filter(|step| matches!(
                    step.decision,
                    ProductionDecision::ProducedImmediateReinforcement { .. }
                ))
                .count(),
            0,
            "追加の有効打がないため、余剰資金を無目的な即時増援へ使わない"
        );
    }

    /// テスト用の作戦。枠の充足状況だけを見たいので敵情報は空にしておく。
    fn operation(kind: OperationKind, slots: OperationSlots, filled: OperationSlots) -> Operation {
        Operation {
            kind,
            island_id: None,
            anchor: pos(0, 0),
            staging_anchor: pos(0, 0),
            execution_authorized: true,
            protected_capture_entities: HashSet::new(),
            objective_properties: vec![pos(0, 0)],
            threat_horizon: 0,
            facts: OperationFacts::default(),
            slots,
            filled,
            unreachable_threats: Vec::new(),
            reachable_threats: Vec::new(),
            expected_reinforcements: Vec::new(),
            reinforcement_contingencies: Vec::new(),
            stress_reinforcement_funds: 0,
            contingency_reserve_funds: 0,
        }
    }

    /// 海峡マップ上に、母港の輸送艦 1 隻だけを置いた盤面を作る。
    ///
    /// 対岸 (8,1) を獲りにいく作戦から見て、この輸送艦は
    /// 「距離では母港側が最寄り」だが「積荷を対岸へ届けられる」という位置関係になる。
    fn strait_scan() -> BoardScan {
        let infantry = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1000)
        };
        let lander = UnitStats {
            movement_type: MovementType::Ship,
            max_movement: 6,
            max_cargo: 2,
            ..stats(UnitType::Lander, 12000)
        };

        BoardScan {
            map: strait_map(Some(Terrain::Shoal)).into(),
            master_data: MasterDataRegistry::load().unwrap().into(),
            damage_chart: DamageChart::new().into(),
            funds: 20000,
            free_facilities: vec![(pos(1, 1), Terrain::Port)],
            production_facilities: vec![(pos(1, 1), Terrain::Port)],
            available_types: vec![
                (UnitType::Infantry, infantry),
                (UnitType::Lander, lander.clone()),
            ],
            // 母港 (1,1) に停泊したままの輸送艦。空き搭載スロット 2。
            my_units: vec![UnitSnapshot {
                entity: None,
                pos: pos(1, 1),
                stats: lander,
                hp: 100,
                free_cargo: 2,
            }],
            enemy_units: Vec::new(),
            owned_airport_count: 0,
            open_properties: vec![pos(8, 1)],
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 1000,
            campaign_objectives: Vec::new(),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
            enemy_production_forecast: EnemyProductionForecastTrace::default(),
        }
    }

    /// 輸送台帳は「最寄り作戦」ではなく「その作戦へ届けられるか」で数える
    ///
    /// 輸送ユニットは自軍港湾に生まれて港に留まるため、位置で仕分けると常に
    /// 母港に近い作戦の台帳に載る。渡洋する作戦側の空き搭載スロットは永久に 0 となり、
    /// 輸送枠の要求が減らないまま揚陸艇を延々と買い増すラチェットになる。
    #[test]
    fn transports_are_ledgered_by_delivery_ability_not_proximity() {
        let scan = strait_scan();
        let mut ctx = ReachCtx::default();
        let reference = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1000)
        };
        // 母港側と対岸側、2 つの作戦地点がある盤面
        let anchors = vec![pos(0, 1), pos(8, 1)];
        let horizons = vec![5; anchors.len()];

        // 前提: 母港の輸送艦は距離では母港側の作戦が最寄りである
        assert!(
            eta_turns(&scan.map, &pos(1, 1), &anchors[0], 6)
                < eta_turns(&scan.map, &pos(1, 1), &anchors[1], 6)
        );

        let overseas = build_operation(
            &scan,
            &mut ctx,
            &reference,
            OperationKind::Capture,
            anchors[1],
            &anchors,
            &horizons,
            &[true, true],
            &[anchors[1]],
            &HashSet::new(),
            &HashSet::new(),
            3,
        );

        // それでも「対岸へ積荷を届けられる」以上、渡洋作戦の台帳に載らねばならない
        assert_eq!(overseas.facts.available_free_cargo_slots, 2);
    }

    #[test]
    fn loaded_enemy_transport_on_another_island_is_a_future_arrival() {
        let mut scan = strait_scan();
        scan.enemy_units.push(UnitSnapshot {
            entity: Some(Entity::from_raw(700)),
            pos: pos(1, 1),
            stats: UnitStats {
                movement_type: MovementType::Air,
                max_movement: 7,
                max_cargo: 2,
                loadable_unit_types: vec![UnitType::Infantry],
                ..stats(UnitType::TransportHelicopter, 4_000)
            },
            hp: 100,
            free_cargo: 1,
        });
        let reference = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1_000)
        };
        let anchor = pos(8, 1);
        let mut ctx = ReachCtx::default();
        let operation = build_operation(
            &scan,
            &mut ctx,
            &reference,
            OperationKind::Capture,
            anchor,
            &[anchor],
            &[5],
            &[true],
            &[anchor],
            &HashSet::new(),
            &HashSet::new(),
            3,
        );

        assert_eq!(operation.reachable_threats.len(), 1);
        let threat = &operation.reachable_threats[0];
        assert_eq!(threat.entity, None, "別島の敵Entityを追跡対象にしない");
        assert!(threat.available_turn > 0, "anchor到着後の増援として扱う");
        assert_eq!(threat.position, anchor);
    }

    #[test]
    fn empty_enemy_transport_does_not_create_anti_air_demand_on_every_island() {
        let mut scan = strait_scan();
        scan.enemy_units.push(UnitSnapshot {
            entity: Some(Entity::from_raw(700)),
            pos: pos(1, 1),
            stats: UnitStats {
                movement_type: MovementType::Air,
                max_movement: 7,
                max_cargo: 2,
                loadable_unit_types: vec![UnitType::Infantry],
                ..stats(UnitType::TransportHelicopter, 4_000)
            },
            hp: 100,
            free_cargo: 2,
        });
        let reference = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1_000)
        };
        let anchor = pos(8, 1);
        let operation = build_operation(
            &scan,
            &mut ReachCtx::default(),
            &reference,
            OperationKind::Capture,
            anchor,
            &[anchor],
            &[5],
            &[true],
            &[anchor],
            &HashSet::new(),
            &HashSet::new(),
            3,
        );

        assert!(operation.reachable_threats.is_empty());
    }

    /// 平地マップに工場 3 基と未取得拠点を置いた、生産ループ検証用の盤面。
    ///
    /// 空き施設を複数持たせることで「同一ターン内に複数施設へ発注が飛ぶ」状況を作り、
    /// その内訳がトレースに残るかを確かめられるようにする。
    fn multi_factory_scan() -> BoardScan {
        let infantry = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1000)
        };
        let tank = UnitStats {
            max_movement: 6,
            ..stats(UnitType::Tank, 7000)
        };

        BoardScan {
            map: flat_map(9, 5).into(),
            master_data: MasterDataRegistry::load().unwrap().into(),
            damage_chart: DamageChart::new().into(),
            funds: 20000,
            free_facilities: vec![
                (pos(1, 1), Terrain::Factory),
                (pos(1, 2), Terrain::Factory),
                (pos(1, 3), Terrain::Factory),
            ],
            production_facilities: vec![
                (pos(1, 1), Terrain::Factory),
                (pos(1, 2), Terrain::Factory),
                (pos(1, 3), Terrain::Factory),
            ],
            available_types: vec![(UnitType::Infantry, infantry), (UnitType::Tank, tank)],
            my_units: Vec::new(),
            enemy_units: Vec::new(),
            owned_airport_count: 0,
            open_properties: vec![pos(6, 2)],
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 1000,
            campaign_objectives: capture_objective(pos(6, 2)),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
            enemy_production_forecast: EnemyProductionForecastTrace::default(),
        }
    }

    /// 航空・地上の未対処脅威と工場3基を持つ、限界価値の統合テスト盤面。
    fn mixed_threat_multi_factory_scan() -> BoardScan {
        let infantry = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1000)
        };
        let anti_air = UnitStats {
            max_movement: 6,
            ..stats(UnitType::AntiAir, 8000)
        };
        let tank = UnitStats {
            max_movement: 6,
            ..stats(UnitType::Tank, 7000)
        };
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::AntiAir, UnitType::Bcopters, 120);
        damage_chart.insert_damage(UnitType::Bcopters, UnitType::AntiAir, 10);
        damage_chart.insert_damage(UnitType::AntiAir, UnitType::Infantry, 0);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::AntiAir, 20);
        damage_chart.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Tank, 0);

        BoardScan {
            map: flat_map(9, 5).into(),
            master_data: MasterDataRegistry::load().unwrap().into(),
            damage_chart: damage_chart.into(),
            funds: 17000,
            free_facilities: vec![
                (pos(1, 0), Terrain::Factory),
                (pos(1, 1), Terrain::Factory),
                (pos(1, 2), Terrain::Factory),
                (pos(1, 3), Terrain::Factory),
            ],
            production_facilities: vec![
                (pos(1, 0), Terrain::Factory),
                (pos(1, 1), Terrain::Factory),
                (pos(1, 2), Terrain::Factory),
                (pos(1, 3), Terrain::Factory),
            ],
            available_types: vec![
                (UnitType::Infantry, infantry.clone()),
                (UnitType::AntiAir, anti_air),
                (UnitType::Tank, tank),
            ],
            my_units: Vec::new(),
            enemy_units: vec![
                UnitSnapshot {
                    entity: Some(Entity::from_raw(101)),
                    pos: pos(6, 1),
                    stats: stats(UnitType::Bcopters, 8000),
                    hp: 100,
                    free_cargo: 0,
                },
                UnitSnapshot {
                    entity: Some(Entity::from_raw(102)),
                    pos: pos(6, 3),
                    stats: infantry,
                    hp: 100,
                    free_cargo: 0,
                },
            ],
            owned_airport_count: 0,
            open_properties: vec![pos(6, 2)],
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 1000,
            campaign_objectives: capture_objective(pos(6, 2)),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
            enemy_production_forecast: EnemyProductionForecastTrace::default(),
        }
    }

    /// キャンペーンの予約超過分は、構造枠を二重購入せず、自力展開可能な対敵戦力へ使う。
    #[test]
    fn campaign_surplus_targets_enemy_infantry_without_buying_stranded_tank() {
        let mut map = flat_map(9, 3);
        for y in 0..map.height {
            map.set_terrain(4, y, Terrain::Sea).unwrap();
        }
        let infantry = UnitStats {
            can_capture: true,
            max_movement: 3,
            max_fuel: 99,
            ..stats(UnitType::Infantry, 1_000)
        };
        let tank = UnitStats {
            movement_type: MovementType::Tank,
            max_movement: 6,
            max_fuel: 70,
            ..stats(UnitType::Tank, 6_000)
        };
        let fighter = UnitStats {
            movement_type: MovementType::Air,
            max_movement: 8,
            max_fuel: 70,
            daily_fuel_consumption: 5,
            ..stats(UnitType::Fighter, 16_000)
        };
        let transport = UnitStats {
            movement_type: MovementType::Air,
            max_movement: 7,
            max_cargo: 2,
            loadable_unit_types: vec![UnitType::Infantry],
            ..stats(UnitType::TransportHelicopter, 4_000)
        };
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Fighter, UnitType::Infantry, 80);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Fighter, 0);
        damage_chart.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Tank, 0);
        let scan = BoardScan {
            map: map.into(),
            master_data: MasterDataRegistry::load().unwrap().into(),
            damage_chart: damage_chart.into(),
            funds: 20_000,
            free_facilities: vec![(pos(1, 1), Terrain::Factory), (pos(2, 1), Terrain::Airport)],
            production_facilities: vec![
                (pos(1, 1), Terrain::Factory),
                (pos(2, 1), Terrain::Airport),
            ],
            available_types: vec![
                (UnitType::Infantry, infantry.clone()),
                (UnitType::Tank, tank),
                (UnitType::Fighter, fighter),
                (UnitType::TransportHelicopter, transport),
            ],
            my_units: Vec::new(),
            enemy_units: vec![UnitSnapshot {
                entity: Some(Entity::from_raw(901)),
                pos: pos(7, 1),
                stats: infantry,
                hp: 100,
                free_cargo: 0,
            }],
            owned_airport_count: 1,
            open_properties: vec![pos(7, 1)],
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 5_000,
            campaign_objectives: capture_objective(pos(7, 1)),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
            enemy_production_forecast: EnemyProductionForecastTrace::default(),
        };

        let (commands, trace) = plan_production(&scan, PlayerId(1), false, &HashMap::new());

        assert_eq!(commands.len(), 1, "commands={commands:?}, trace={trace:?}");
        assert_eq!(commands[0].unit_type, UnitType::Fighter);
        assert!(commands.iter().all(|command| {
            !matches!(
                command.unit_type,
                UnitType::Infantry | UnitType::TransportHelicopter | UnitType::Tank
            )
        }));
    }

    /// Mustを満たした後の増援は、到達不能な工場や仮想敵へ流さず、実在する前線Entityを
    /// 撃てる種類だけを選び、そのEntityをpriorityとして任務へ接続する。
    #[test]
    fn immediate_reinforcement_targets_a_live_reachable_frontline_enemy() {
        let mut map = flat_map(9, 3);
        for y in 0..map.height {
            map.set_terrain(4, y, Terrain::Sea).unwrap();
        }
        let infantry = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1_000)
        };
        let tank = UnitStats {
            movement_type: MovementType::Tank,
            max_movement: 6,
            ..stats(UnitType::Tank, 6_000)
        };
        let fighter = UnitStats {
            movement_type: MovementType::Air,
            max_movement: 8,
            max_fuel: 70,
            daily_fuel_consumption: 5,
            ..stats(UnitType::Fighter, 16_000)
        };
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Fighter, UnitType::Infantry, 80);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Fighter, 0);
        damage_chart.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Tank, 0);
        let enemy = Entity::from_raw(902);
        let scan = BoardScan {
            map: map.into(),
            master_data: MasterDataRegistry::load().unwrap().into(),
            damage_chart: damage_chart.into(),
            funds: 20_000,
            free_facilities: vec![(pos(1, 1), Terrain::Factory), (pos(2, 1), Terrain::Airport)],
            production_facilities: vec![
                (pos(1, 1), Terrain::Factory),
                (pos(2, 1), Terrain::Airport),
            ],
            available_types: vec![
                (UnitType::Infantry, infantry.clone()),
                (UnitType::Tank, tank),
                (UnitType::Fighter, fighter),
            ],
            my_units: Vec::new(),
            enemy_units: vec![UnitSnapshot {
                entity: Some(enemy),
                pos: pos(7, 1),
                stats: infantry,
                hp: 100,
                free_cargo: 0,
            }],
            owned_airport_count: 1,
            open_properties: vec![pos(7, 1)],
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 5_000,
            campaign_objectives: capture_objective(pos(7, 1)),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
            enemy_production_forecast: EnemyProductionForecastTrace::default(),
        };
        let mut ctx = ReachCtx::default();
        let operations = build_operations(&scan, &mut ctx, &[]);

        let options = immediate_combat_options(&scan, &mut ctx, &operations, PlayerId(1));
        let (operation_index, candidate, engagement) = select_immediate_combat_reinforcement(
            &operations,
            &options,
            &HashSet::new(),
            &HashMap::new(),
            CandidateConstraints {
                remaining_funds: scan.funds,
                per_slot_budget: 10_000,
            },
        )
        .expect("実在前線へ届く増援を選ぶ");

        assert_eq!(candidate.unit_type, UnitType::Fighter);
        let deployment = planned_deployment(
            &scan,
            &mut ctx,
            &operations[operation_index],
            SlotKind::Combat,
            &candidate,
            Some(engagement.entity),
        )
        .expect("Combat増援に任務を付ける");
        assert_eq!(deployment.priority_enemies, vec![enemy]);
        assert!(deployment.plan_step.is_none());
    }

    /// 余剰Combatは候補表を一度だけ作るが、1体が同じ敵全員を同時に攻撃できるとは
    /// 見なさない。工場枠をまとめて割り当てるときは、先に必要HPを満たした敵ではなく
    /// 次の実在敵を担当させる。
    #[test]
    fn immediate_reinforcement_package_spreads_damage_across_live_enemies() {
        let first_enemy = Entity::from_raw(904);
        let second_enemy = Entity::from_raw(905);
        let candidate = |facility| SlotCandidate {
            unit_type: UnitType::Tank,
            cost: 7_000,
            facility,
            fitness: 0.0,
        };
        let engagements = vec![
            ImmediateCombatEngagement {
                entity: first_enemy,
                current_hp: 100,
                damage: 100,
                fitness: 1.0,
            },
            ImmediateCombatEngagement {
                entity: second_enemy,
                current_hp: 100,
                damage: 100,
                fitness: 0.9,
            },
        ];
        let options = vec![
            ImmediateCombatOption {
                operation_index: 0,
                candidate: candidate(pos(1, 1)),
                engagements: engagements.clone(),
            },
            ImmediateCombatOption {
                operation_index: 0,
                candidate: candidate(pos(2, 1)),
                engagements,
            },
        ];
        let operations = vec![operation(
            OperationKind::Capture,
            OperationSlots::default(),
            OperationSlots::default(),
        )];
        let (operation_index, _, first) = select_immediate_combat_reinforcement(
            &operations,
            &options,
            &HashSet::new(),
            &HashMap::new(),
            CandidateConstraints {
                remaining_funds: 14_000,
                per_slot_budget: 7_000,
            },
        )
        .expect("最初の前線敵へ増援を割り当てる");
        assert_eq!(first.entity, first_enemy);

        let mut used_facilities = HashSet::new();
        used_facilities.insert(pos(1, 1));
        let mut committed_damage = HashMap::new();
        committed_damage.insert((operation_index, first.entity), first.damage);
        let (_, _, second) = select_immediate_combat_reinforcement(
            &operations,
            &options,
            &used_facilities,
            &committed_damage,
            CandidateConstraints {
                remaining_funds: 7_000,
                per_slot_budget: 7_000,
            },
        )
        .expect("別の未充足前線敵へ増援を割り当てる");
        assert_eq!(second.entity, second_enemy);
    }

    /// 工場が余っているのではなく「同じ工場枠で何を出すか」が制約の局面では、価格あたり
    /// の効率で安い歩兵へ寄せず、前線全体をより大きく削れる戦闘unitを選ぶ。
    #[test]
    fn immediate_reinforcement_values_factory_slot_over_unit_price() {
        let infantry = UnitStats {
            can_capture: true,
            max_movement: 3,
            max_fuel: 99,
            ..stats(UnitType::Infantry, 1_000)
        };
        let tank = UnitStats {
            max_movement: 6,
            max_fuel: 70,
            ..stats(UnitType::Tank, 7_000)
        };
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Infantry, 70);
        damage_chart.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        let enemy = Entity::from_raw(903);
        let scan = BoardScan {
            map: flat_map(9, 3).into(),
            master_data: MasterDataRegistry::load().unwrap().into(),
            damage_chart: damage_chart.into(),
            funds: 10_000,
            free_facilities: vec![(pos(1, 1), Terrain::Factory)],
            production_facilities: vec![(pos(1, 1), Terrain::Factory)],
            available_types: vec![
                (UnitType::Infantry, infantry.clone()),
                (UnitType::Tank, tank),
            ],
            my_units: Vec::new(),
            enemy_units: vec![UnitSnapshot {
                entity: Some(enemy),
                pos: pos(6, 1),
                stats: infantry,
                hp: 100,
                free_cargo: 0,
            }],
            owned_airport_count: 0,
            open_properties: vec![pos(6, 1)],
            enemy_income: 0,
            enemy_production_slots: 0,
            enemy_facilities: Vec::new(),
            my_income: 1_000,
            campaign_objectives: capture_objective(pos(6, 1)),
            capital_assault_authorized: false,
            capital_staging_anchor: None,
            enemy_production_forecast: EnemyProductionForecastTrace::default(),
        };
        let mut ctx = ReachCtx::default();
        let operations = build_operations(&scan, &mut ctx, &[]);
        let options = immediate_combat_options(&scan, &mut ctx, &operations, PlayerId(1));

        let (_, candidate, _) = select_immediate_combat_reinforcement(
            &operations,
            &options,
            &HashSet::new(),
            &HashMap::new(),
            CandidateConstraints {
                remaining_funds: scan.funds,
                per_slot_budget: scan.funds,
            },
        )
        .expect("両候補とも実在前線へ届く");

        assert_eq!(candidate.unit_type, UnitType::Tank);
    }

    /// 同一手番の混成パッケージに、航空・地上の両脅威へ有効なunitを含める。
    #[test]
    fn multi_factory_plan_switches_after_air_threat_is_covered() {
        let scan = mixed_threat_multi_factory_scan();
        let (commands, trace) = plan_production(&scan, PlayerId(0), true, &HashMap::new());
        let combat_types: Vec<UnitType> = commands
            .iter()
            .map(|command| command.unit_type)
            .filter(|unit_type| !matches!(unit_type, UnitType::Infantry))
            .collect();

        assert_eq!(
            combat_types.len(),
            2,
            "commands={commands:?}, trace={trace:?}"
        );
        assert!(combat_types.contains(&UnitType::AntiAir));
        assert!(combat_types.contains(&UnitType::Tank));
    }

    /// 増援予算だけでは敵兵種を特定できないため、敵未観測の撃破枠から汎用兵を作らない。
    #[test]
    fn combat_slot_without_observed_threat_does_not_produce() {
        let mut scan = multi_factory_scan();
        scan.enemy_income = 10_000;
        scan.enemy_production_slots = 1;

        let (commands, trace) = plan_production(&scan, PlayerId(0), true, &HashMap::new());

        assert!(
            commands
                .iter()
                .all(|command| command.unit_type != UnitType::Tank)
        );
        assert!(trace.steps.iter().all(|step| {
            step.slot_kind != SlotKind::Combat
                || !matches!(step.decision, ProductionDecision::Produced { .. })
        }));
    }

    /// 生産トレースは、発行した命令 1 件ごとに「どの作戦のどの枠から出たか」を残す
    ///
    /// 「同一ターン内に同じユニットが全施設へ発注される」現象を切り分けるには、
    /// 発注とトレースが 1 対 1 で対応していなければならない。ズレた時点で
    /// 診断そのものが無意味になるため、記録専用であるという不変条件をここで固定する。
    #[test]
    fn production_trace_attributes_every_command_to_a_slot() {
        let scan = multi_factory_scan();
        let (commands, trace) = plan_production(&scan, PlayerId(0), true, &HashMap::new());

        // 作戦が立つ盤面なので fallback には落ちない
        assert!(!trace.fallback);
        assert!(!trace.operations.is_empty());
        assert!(!commands.is_empty());
        assert_eq!(trace.funds, scan.funds);
        assert_eq!(trace.free_facility_count, scan.free_facilities.len());

        // 発注は 1 件残らず、通常購入または即時前線増援として記録される。
        let produced: Vec<_> = trace
            .steps
            .iter()
            .filter_map(|step| match &step.decision {
                ProductionDecision::Produced {
                    unit_type,
                    cost,
                    facility,
                } => Some((*unit_type, *cost, *facility)),
                ProductionDecision::ProducedImmediateReinforcement {
                    unit_type,
                    cost,
                    facility,
                } => Some((*unit_type, *cost, *facility)),
                _ => None,
            })
            .collect();
        assert_eq!(produced.len(), commands.len());
        for (command, (unit_type, _, facility)) in commands.iter().zip(produced.iter()) {
            assert_eq!(command.unit_type, *unit_type);
            assert_eq!(command.target_x, facility.x);
            assert_eq!(command.target_y, facility.y);
            // 発注先は必ず空き施設のいずれか
            assert!(scan.free_facilities.iter().any(|(f, _)| f == facility));
        }

        // 資金の収支が合うこと（余剰資金の積み上がりを測る土台になる）
        let spent: u32 = produced.iter().map(|(_, cost, _)| *cost).sum();
        assert_eq!(trace.leftover_funds, scan.funds - spent);

        // 種別ごとの体数集計も命令と一致する（工場数への張り付きを数える入口）
        assert_eq!(
            trace.produced_counts().values().sum::<usize>(),
            commands.len()
        );
    }

    /// 届けられない作戦地点の台帳には載せない（二重計上の歯止め）
    #[test]
    fn transports_are_not_ledgered_for_unreachable_anchors() {
        let mut scan = strait_scan();
        // 接岸できる地形を消すと、艦船は対岸へ陸上ユニットを降ろせなくなる
        scan.map = strait_map(None).into();
        let mut ctx = ReachCtx::default();
        let reference = UnitStats {
            can_capture: true,
            max_movement: 3,
            ..stats(UnitType::Infantry, 1000)
        };
        let anchors = vec![pos(0, 1), pos(8, 1)];
        let horizons = vec![5; anchors.len()];

        let overseas = build_operation(
            &scan,
            &mut ctx,
            &reference,
            OperationKind::Capture,
            anchors[1],
            &anchors,
            &horizons,
            &[true, true],
            &[anchors[1]],
            &HashSet::new(),
            &HashSet::new(),
            3,
        );

        assert_eq!(overseas.facts.available_free_cargo_slots, 0);
    }

    /// 前提条件どうしの競合では、作戦の優先度が枠の優先順位より上位に効く
    #[test]
    fn operation_priority_outranks_slot_priority_among_prerequisites() {
        let ops = vec![
            // 占領作戦の輸送枠（枠としては先だが、作戦としては後回し）
            operation(
                OperationKind::Capture,
                OperationSlots {
                    transport_slots: 4,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
            // 防衛作戦の戦闘計画（枠としては最後だが、作戦が最優先）。
            operation(
                OperationKind::Defense,
                OperationSlots {
                    combat_plan_required: 1,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
        ];

        assert_eq!(most_starved_slot(&ops), Some((1, SlotKind::Combat)));
    }

    /// 同一前線では占領兵を全数先買いせず、最初の占領兵に続いて掃討計画を起動する。
    #[test]
    fn combat_plan_follows_first_capture_before_remaining_fronts_expand() {
        let slots = OperationSlots {
            capture_units: 8,
            combat_plan_required: 1,
            ..OperationSlots::default()
        };
        let empty = operation(OperationKind::Capture, slots, OperationSlots::default());
        assert_eq!(most_starved_slot(&[empty]), Some((0, SlotKind::Capture)));

        let first_capture_ready = operation(
            OperationKind::Capture,
            slots,
            OperationSlots {
                capture_units: 1,
                ..OperationSlots::default()
            },
        );
        assert_eq!(
            most_starved_slot(&[first_capture_ready]),
            Some((0, SlotKind::Combat))
        );
    }

    #[test]
    fn active_dag_segment_defers_unapproved_capital_capture_slots() {
        let mut segment = operation(
            OperationKind::Capture,
            OperationSlots {
                combat_plan_required: 1,
                ..OperationSlots::default()
            },
            OperationSlots::default(),
        );
        segment.facts.enemy_combat_units = 3;
        let capital_slots = OperationSlots {
            capture_units: 1,
            transport_slots: 2,
            ..OperationSlots::default()
        };
        let mut capital = operation(
            OperationKind::AssaultCapital,
            capital_slots,
            OperationSlots::default(),
        );
        capital.execution_authorized = false;
        let mut operations = vec![segment, capital];

        defer_unapproved_capital_structure_for_active_segment(&mut operations);

        assert_eq!(operations[0].slots.combat_plan_required, 1);
        assert_eq!(operations[1].slots.capture_units, 0);
        assert_eq!(operations[1].slots.transport_slots, 0);
    }

    /// 前線の分類はmap名ではなく、占領可能unitの実到達性だけで決める。
    #[test]
    fn direct_front_uses_operation_production_while_strait_keeps_transport_campaign() {
        use crate::ai::island_campaign::{IslandCampaignDecision, IslandCampaignShortfall};
        use crate::ai::islands::IslandId;

        let shortfall = |target_position| IslandCampaignShortfall {
            island_id: IslandId(0),
            decision: IslandCampaignDecision::Assault,
            target_position,
            light_transport_slots: 0,
            heavy_transport_slots: 0,
            capture_units: 4,
            ground_combat_units: 0,
            combat_units: 0,
            priority_enemy_types: Vec::new(),
            reserved_budget: 4_000,
            priority_rank: 0,
        };

        let mut mainland = strait_scan();
        mainland.map = flat_map(10, 3).into();
        mainland.production_facilities = vec![(pos(1, 1), Terrain::Factory)];
        mainland.free_facilities = mainland.production_facilities.clone();
        let mut reach = ReachCtx::default();
        assert!(has_direct_capture_route(
            &mainland,
            &mut reach,
            &shortfall(pos(8, 1))
        ));

        let overseas = strait_scan();
        let mut reach = ReachCtx::default();
        assert!(!has_direct_capture_route(
            &overseas,
            &mut reach,
            &shortfall(pos(8, 1))
        ));
    }

    /// 具体的な敵を持つ最優先防衛作戦は、後順位の輸送より先に計画する。
    #[test]
    fn a_top_priority_combat_plan_precedes_a_lower_priority_transport() {
        let ops = vec![
            operation(
                OperationKind::Defense,
                OperationSlots {
                    combat_plan_required: 1,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
            // 渡洋する占領作戦。輸送が無ければ 1 歩も進めない。
            operation(
                OperationKind::Capture,
                OperationSlots {
                    transport_slots: 2,
                    ..OperationSlots::default()
                },
                OperationSlots::default(),
            ),
        ];

        assert_eq!(most_starved_slot(&ops), Some((0, SlotKind::Combat)));
    }

    #[test]
    fn authorized_capital_outranks_non_imminent_local_operations_only() {
        let mut capture = operation(
            OperationKind::Capture,
            OperationSlots {
                transport_slots: 1,
                ..OperationSlots::default()
            },
            OperationSlots::default(),
        );
        capture.execution_authorized = true;
        let mut capital = operation(
            OperationKind::AssaultCapital,
            OperationSlots {
                combat_plan_required: 1,
                ..OperationSlots::default()
            },
            OperationSlots::default(),
        );
        capital.execution_authorized = true;
        let mut distant_defense = operation(
            OperationKind::Defense,
            OperationSlots {
                combat_plan_required: 1,
                ..OperationSlots::default()
            },
            OperationSlots::default(),
        );
        distant_defense.facts.enemy_combat_units = 1;
        distant_defense.facts.enemy_contact_eta = 6;
        distant_defense.facts.deploy_lead_time = 1;

        let ops = vec![capture, distant_defense, capital];
        assert_eq!(most_starved_slot(&ops), Some((2, SlotKind::Combat)));
    }

    #[test]
    fn defense_urgency_raises_normal_operation_priority_without_deleting_capital() {
        let mut capital = operation(
            OperationKind::AssaultCapital,
            OperationSlots {
                combat_plan_required: 1,
                ..OperationSlots::default()
            },
            OperationSlots::default(),
        );
        capital.execution_authorized = true;
        let mut defense = operation(
            OperationKind::Defense,
            OperationSlots {
                combat_plan_required: 1,
                ..OperationSlots::default()
            },
            OperationSlots::default(),
        );
        defense.facts.enemy_combat_units = 1;
        defense.facts.enemy_contact_eta = 1;
        defense.facts.deploy_lead_time = 2;

        let ops = vec![capital, defense];
        assert_eq!(operation_priority_rank(&ops[0]), 1);
        assert_eq!(operation_priority_rank(&ops[1]), 0);
        assert_eq!(most_starved_slot(&ops), Some((1, SlotKind::Combat)));
        assert_eq!(ops[0].slots.combat_plan_required, 1);
    }

    /// 島作戦の不足はV4汎用作戦より先に発注し、不完全なパッケージの途中で
    /// 余った施設を汎用生産へ開放しない。
    #[test]
    fn v4_prioritizes_campaign_package_and_blocks_generic_when_incomplete() {
        use crate::ai::engine::AiTurnStrategyCache;
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignPortfolio,
            IslandCampaignRequirement,
        };
        use crate::ai::islands::IslandId;
        use crate::resources::Players;
        use crate::resources::master_data::MasterDataRegistry;

        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_3",
            GridTopology::Hex,
        )
        .expect("map_3 should initialize");
        let player_id = PlayerId(1);

        // 輸送船だけは買えるが、同時必須の占領要員までは買えない資金に固定する。
        world
            .resource_mut::<Players>()
            .0
            .iter_mut()
            .find(|player| player.id == player_id)
            .expect("player 1 should exist")
            .funds = 16_500;

        let requirement = IslandCampaignRequirement {
            preferred_transport: Some(UnitType::Lander),
            transport_slots: 2,
            capture_units: 1,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 17_500,
        };
        let assignment = IslandCampaignAssignment {
            island_id: IslandId(1),
            decision: IslandCampaignDecision::Reinforce,
            target_position: pos(23, 24),
            capture_target_positions: vec![pos(23, 24)],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: requirement,
            allocated_budget: 0,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        let mut cache = AiTurnStrategyCache::default();
        cache.set_campaign_portfolio(
            player_id,
            IslandCampaignPortfolio {
                active_offensives: vec![assignment],
                ..IslandCampaignPortfolio::default()
            },
        );
        world.insert_resource(cache);

        let first = decide_production_v4(&mut world, player_id);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].unit_type, UnitType::Lander);

        // 同一手番の次呼び出しでは、不足した占領要員を無視してgenericを出さない。
        assert!(decide_production_v4(&mut world, player_id).is_empty());
        assert!(
            world
                .resource::<AiTurnStrategyCache>()
                .campaign_production_blocks_generic(player_id)
        );
    }
}
