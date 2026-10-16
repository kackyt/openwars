//! 首都ルートDAGの型と台帳。所属の正本はSquadの作戦IDで参照する。

use crate::ai::v4::*;

/// 同一陸塊Campaignで、どの橋を地上部隊が実際に越えたかを保持する。
/// 橋の向こうで北南の進軍路が再合流する地図でも、片方を越えただけで両方を
/// 完了にしないため、地形連結成分ではなく橋Gate単位で記録する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::ai::v4) struct RouteBreakthroughKey {
    pub(in crate::ai::v4) island_id: crate::ai::islands::IslandId,
    pub(in crate::ai::v4) gate: GridPosition,
}

#[derive(Resource, Debug, Default)]
pub(crate) struct RouteBreakthroughRegistry {
    pub(in crate::ai::v4) crossed: HashMap<PlayerId, HashSet<RouteBreakthroughKey>>,
}

/// 首都戦役における進軍DAGを特定するキー。
///
/// 地形と拠点配置は対戦中に変わらないため、同じ勢力・島のDAGを毎手番に再構築しない。
/// 先攻・後攻はそれぞれ自首都を始点にして独立に構築する。片方のDAGを逆順には読まない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::ai::v4) struct CapitalRouteTopologyKey {
    pub(in crate::ai::v4) player_id: PlayerId,
    pub(in crate::ai::v4) island_id: crate::ai::islands::IslandId,
}

/// 首都戦役DAG内のrouteを識別する値オブジェクト。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::ai::v4) struct CapitalRouteId(pub(in crate::ai::v4) usize);

/// 首都戦役DAG内の区間ノードを識別する値オブジェクト。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::ai::v4) struct CapitalRouteNodeId(pub(in crate::ai::v4) usize);

/// 静的なセルDAGのNodeを、毎手番に観測・実行できる作戦として識別するID。
///
/// `VictoryRoadmap` の島Campaign IDとは混ぜない。同一島の地上進軍を表すため、
/// topologyとその中のNode番号の組で一意にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::ai::v4) struct CapitalRouteNodeOperationId {
    pub(in crate::ai::v4) topology: CapitalRouteTopologyKey,
    pub(in crate::ai::v4) node: CapitalRouteNodeId,
}

/// DAG NodeがSquadへ要求する達成内容。
///
/// Nodeのanchorは地理上の代表点であって、Entityがそこへ到着するだけでは達成にならない。
/// Controlは回廊地域での優勢、Captureは地域内に紐づく拠点群の確保を出口条件にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ai::v4) enum CapitalRouteNodeObjective {
    Control,
    Capture,
}

/// 地形・拠点スキャンから決める、DAG Nodeの戦術的な広がり。
///
/// Pointは橋・細い通過点・単一拠点のように、到達すべき一点が明確なNodeである。
/// Areaは分岐点または複数拠点を含む局地戦であり、Squadには中心座標ではなく地域を渡す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ai::v4) enum CapitalRouteNodeScope {
    Point,
    Area,
}

/// 首都間の最短装甲回廊を構成する一マス分のDAGノード。
///
/// 同距離の別ノードは並列に進め、後段の同一ノードへ合流できる。
#[derive(Debug, Clone)]
pub(in crate::ai::v4) struct CapitalRouteNode {
    pub(in crate::ai::v4) progress: u32,
    pub(in crate::ai::v4) anchor: GridPosition,
    pub(in crate::ai::v4) successor_nodes: Vec<CapitalRouteNodeId>,
}

/// 始点から分岐する進軍軸を、戦力配分の集計にだけ使う。
///
/// 実際のMilestoneの前後関係は `CapitalRouteTopology::nodes` のDAGで表す。
#[derive(Debug, Clone)]
pub(in crate::ai::v4) struct CapitalRoute {
    pub(in crate::ai::v4) front: GridPosition,
}

/// 一度だけ地形・拠点を走査して構築する、勢力ごとの前向き首都攻略DAG。
///
/// 所有者・unit・損耗は持たない。毎手番にはこのDAGへ盤面を投影するだけにする。
#[derive(Debug, Clone)]
pub(in crate::ai::v4) struct CapitalRouteTopology {
    pub(in crate::ai::v4) start_node: CapitalRouteNodeId,
    pub(in crate::ai::v4) goal_node: CapitalRouteNodeId,
    pub(in crate::ai::v4) nodes: Vec<CapitalRouteNode>,
    pub(in crate::ai::v4) routes: Vec<CapitalRoute>,
    pub(in crate::ai::v4) property_routes: HashMap<GridPosition, CapitalRouteId>,
    pub(in crate::ai::v4) from_home: HashMap<GridPosition, u32>,
}

#[derive(Resource, Debug, Default)]
pub(crate) struct CapitalRouteTopologyRegistry {
    pub(in crate::ai::v4) topologies: HashMap<CapitalRouteTopologyKey, CapitalRouteTopology>,
}

/// 首都攻略セルNodeを盤面へ投影した作戦状態。
///
/// topologyは地形だけを持つ静的データのままにし、所有権・前提解放・担当Squadは
/// この台帳だけに置く。これによりセルDAGもRoadmapと同じく「状態を持つNode」になる。
#[derive(Debug, Clone)]
pub(in crate::ai::v4) struct CapitalRouteNodeOperation {
    pub(in crate::ai::v4) id: CapitalRouteNodeOperationId,
    pub(in crate::ai::v4) anchor: GridPosition,
    pub(in crate::ai::v4) predecessors: Vec<CapitalRouteNodeOperationId>,
    pub(in crate::ai::v4) successors: Vec<CapitalRouteNodeOperationId>,
    pub(in crate::ai::v4) objective: CapitalRouteNodeObjective,
    pub(in crate::ai::v4) scope: CapitalRouteNodeScope,
    /// anchorと前後のDAGセルで構成する局地作戦地域。戦術器はこの地域に入った後、
    /// anchor一点ではなく敵・地形・占領対象を見て行動する。
    pub(in crate::ai::v4) control_area: Vec<GridPosition>,
    /// Capture Nodeが確保すべき、回廊周辺の拠点群。
    pub(in crate::ai::v4) capture_targets: Vec<GridPosition>,
    /// 橋Nodeだけが持つ敵側の出口セル。ここへ地上部隊が出るまで、橋手前で優勢でも
    /// Nodeを完了にしない。
    pub(in crate::ai::v4) crossing_exit: Option<GridPosition>,
    /// 一度でも橋向こうへ出たという盤面事実を保持する。通過した部隊が次手番にさらに
    /// 前進しても、橋Nodeを未達へ戻さない。
    pub(in crate::ai::v4) crossed: bool,
    pub(in crate::ai::v4) state: victory_roadmap::RoadmapNodeState,
    pub(in crate::ai::v4) assigned_squads: HashSet<SquadId>,
}

/// Area Nodeを局地戦として観測する半径。
///
/// 前後のMilestone座標をそのまま同じ地域へ入れると、map_25のような長い回廊で
/// 自軍首都・中央戦線・敵首都が一つのAreaになり、後方部隊だけでNodeをDominantと
/// 誤判定する。拠点をNodeへ対応付ける半径とそろえ、実際の近傍セルだけを扱う。
pub(in crate::ai::v4) const CAPITAL_ROUTE_REGION_RADIUS: u32 = 3;

#[derive(Resource, Debug, Default)]
pub(crate) struct CapitalRouteNodeOperationRegistry {
    pub(in crate::ai::v4) operations:
        HashMap<CapitalRouteNodeOperationId, CapitalRouteNodeOperation>,
}

/// 地上Squadへ束縛した、首都攻略DAGの実行区間。
///
/// `target` の座標だけでは山・海峡を迂回する経路が決まらない。そのため分岐から
/// 現在の未確保拠点までのセル列を保持する。Entityはこのorderを持つSquadの
/// memberとしてだけ経路を参照し、DAG自身がEntityへ別の目標を配らない。
#[derive(Debug, Clone)]
pub(in crate::ai::v4) struct CapitalRouteCommitment {
    pub(in crate::ai::v4) player_id: PlayerId,
    pub(in crate::ai::v4) island_id: crate::ai::islands::IslandId,
    pub(in crate::ai::v4) route: CapitalRouteId,
    /// 区間の完了を判定する先頭未確保拠点。所有権が変わるまで進軍区間は閉じない。
    pub(in crate::ai::v4) target: GridPosition,
    /// `target` が対応するセルDAG Node。SquadはこのNode Operationを実行する。
    pub(in crate::ai::v4) target_node: CapitalRouteNodeId,
    /// 現在のNodeが要求する達成内容。移動先の一点ではなく、Squadの局地任務を
    /// 決めるために保持する。
    pub(in crate::ai::v4) objective: CapitalRouteNodeObjective,
    /// 地形・拠点スキャンで決まったPoint/Areaの扱い。
    pub(in crate::ai::v4) scope: CapitalRouteNodeScope,
    /// 現在のNodeに属する回廊地域。
    pub(in crate::ai::v4) control_area: Vec<GridPosition>,
    /// Capture Nodeで占領を許可する周辺拠点群。
    pub(in crate::ai::v4) capture_targets: Vec<GridPosition>,
    /// 橋Nodeの通過出口と、通過済みの観測結果。
    pub(in crate::ai::v4) crossing_exit: Option<GridPosition>,
    pub(in crate::ai::v4) crossed: bool,
    /// このEntityが現在のphaseで移動する区間内の終点。
    ///
    /// 防衛はGateまたは最後に確保した拠点まで、地上輸送は配送先まで移動する。
    /// したがって、両者もDAGの所属を保ったまま通常の突撃役と異なる移動をできる。
    pub(in crate::ai::v4) execution_target: GridPosition,
    /// 前線Nodeへ同時に入れてよい地上member数。地形から決める収容枠であり、
    /// 敵の数だけ無制限に後続を集めないためにSquad指令へ付随させる。
    pub(in crate::ai::v4) frontline_capacity: usize,
    /// 経路の座標列と同じ順序のNode列。地理制約だけでなく、どのNodeを通過するかを
    /// 監査・状態遷移へ渡す。
    pub(in crate::ai::v4) path_nodes: Vec<CapitalRouteNodeId>,
    pub(in crate::ai::v4) path: Vec<GridPosition>,
    pub(in crate::ai::v4) phase: CapitalRouteExecutionPhase,
}

/// DAG区間でのSquadの役割。回復はmemberごとの戦術的な一時状態であり、
/// Squadの戦略orderを書き換えない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ai::v4) enum CapitalRouteExecutionPhase {
    /// Nodeの実行枠を使い、占領・戦闘・突破を担当する前衛Squad。
    Advance,
    /// 前衛枠が埋まっている間、確保済みのDAGセル列へ退避して次の波を待つSquad。
    /// Squadの所属は維持し、DAGがEntityを直接操作することはない。
    Stage,
    Hold,
    Supply,
}

#[derive(Resource, Debug, Default, Clone)]
pub(crate) struct CapitalRoutePathRegistry {
    /// EntityではなくSquadをキーにする。memberの追加・回復・退却があっても、
    /// Squad orderのidentityとDAG上の担当区間は変わらない。
    pub(in crate::ai::v4) commitments: HashMap<SquadId, CapitalRouteCommitment>,
    /// 論理作戦IDから現在の実行形へ引く索引。指令本文やmember集合を二重保持しない。
    pub(in crate::ai::v4) squads_by_operation: HashMap<crate::ai::squad::SquadOperationId, SquadId>,
    pub(in crate::ai::v4) assignment_diagnostics:
        HashMap<CapitalRouteTopologyKey, CapitalRouteAssignmentDiagnostics>,
}

/// 首都戦役DAGの現在区間へ前進している実Entityを、島と区間目標で集計した結果。
///
/// `AssaultCapital`は勝利条件として常在する一方、前段の未確保拠点は`Capture`として
/// 実行する。両者に同じ戦力を同時に見積もらせず、現在の区間を持つ子作戦だけが
/// 既存戦力として受け取るために、目標座標別の集計も保持する。
#[derive(Debug, Default)]
pub(in crate::ai::v4) struct AdvancingCapitalRouteEntities {
    pub(in crate::ai::v4) by_island: HashMap<crate::ai::islands::IslandId, HashSet<Entity>>,
    pub(in crate::ai::v4) by_target:
        HashMap<(crate::ai::islands::IslandId, GridPosition), HashSet<Entity>>,
    /// 前衛枠が埋まり待機線へ回ったmember。待機中でも作戦に帰属した実戦力であり、
    /// 生産plannerは前線へ到達するまでの時間を付けて既存戦力へ含める。
    ///
    /// 人数だけを持って追加Combatを一律に止めると、plannerは待機部隊を戦力として
    /// 数えない一方で生産だけを止める矛盾になり、資金が残ったまま前線が崩れる。
    pub(in crate::ai::v4) staged_by_target:
        HashMap<(crate::ai::islands::IslandId, GridPosition), HashSet<Entity>>,
}

impl CapitalRoutePathRegistry {
    /// 首都戦役DAGで実際に前進中の戦力を、島と現在区間ごとに返す。
    ///
    /// DAGの経路・Squad・生産plannerが別々の所有権を持つと、盤上では前進している
    /// 戦力を首都攻略の見積だけがゼロと誤認する。回復・輸送・区間保持はここへ入れず、
    /// 次の未確保区間へ進むCombat候補だけを親作戦の既存戦力として共有する。
    pub(in crate::ai::v4) fn advancing_combat_entities(
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
            if squad.plan.owner_id != Some(player_id) {
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

/// 経路へ入れなかった理由を、Squad外観ではなく割当処理の時点で残す。
/// これにより「DAG所属が少ない」を戦力配分不足と経路構築失敗に分けて観測できる。
#[derive(Debug, Default, Clone)]
pub(in crate::ai::v4) struct CapitalRouteAssignmentDiagnostics {
    pub(in crate::ai::v4) eligible_ground_units: usize,
    pub(in crate::ai::v4) route_quotas: Vec<usize>,
    pub(in crate::ai::v4) committed_by_route: Vec<usize>,
    pub(in crate::ai::v4) path_unavailable_by_route: Vec<usize>,
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
