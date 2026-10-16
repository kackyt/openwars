//! 勝利Roadmapのturn_plan責務。

use super::dependency_graph::RoadmapNodeState;
use super::{
    GridPosition, HashMap, HashSet, IslandCampaignPortfolio, IslandId, PlayerId, Resource,
    StrategicOperationId, StrategicPurpose, World,
};

/// Roadmapがこの手番に実行すると確定したOperationの、Squad投影用要約。
///
/// 生産slotの予約と複数戦略案の探索は未決定であるため、ここでは既存の
/// IslandCampaign analyzerが提案した割当を一手番だけ凍結する。これにより
/// 後段のSquad再編器が別の島作戦を発明せず、同じ入力を投影できる。
#[derive(Debug, Clone)]
pub(crate) struct RoadmapOperationDirective {
    pub operation_id: StrategicOperationId,
    pub island_id: IslandId,
    pub purpose: StrategicPurpose,
    pub node_state: RoadmapNodeState,
    pub target: GridPosition,
    pub squad_ids: Vec<crate::ai::squad::SquadId>,
}

/// 1 player・1手番のRoadmap決定を保持する不変スナップショット。
#[derive(Debug, Clone)]
pub(crate) struct RoadmapTurnPlan {
    pub player_id: PlayerId,
    pub turn: u32,
    pub portfolio: IslandCampaignPortfolio,
    pub directives: Vec<RoadmapOperationDirective>,
}

impl RoadmapTurnPlan {
    /// Roadmapが同じSquadを複数Operationへ同時に投影していないことを検証する。
    ///
    /// これは候補選択ではなく、TurnPlanをSquadReconcilerへ渡す直前の整合性検査である。
    /// 空のSquad集合はForming Operationを表すため許可する。
    pub(super) fn is_consistent(&self) -> bool {
        let mut operation_ids = HashSet::default();
        let mut objectives = HashSet::default();
        let mut squad_owners = HashMap::default();
        self.directives.iter().all(|directive| {
            let unique_operation = operation_ids.insert(directive.operation_id);
            let unique_objective =
                objectives.insert((directive.island_id, directive.purpose, directive.target));
            let active_node = directive.node_state != RoadmapNodeState::Locked;
            let squads_are_exclusive = directive.squad_ids.iter().all(|squad_id| {
                squad_owners
                    .insert(*squad_id, directive.operation_id)
                    .is_none_or(|owner| owner == directive.operation_id)
            });
            unique_operation && unique_objective && active_node && squads_are_exclusive
        })
    }
}

/// playerごとの当ターンRoadmap指示。Squad編成・行動器はこれを読み、
/// `IslandCampaignDiagnostics` のような観測専用Resourceを意思決定に使わない。
#[derive(Resource, Debug, Default, Clone)]
pub(crate) struct RoadmapTurnPlanRegistry {
    pub(super) plans: HashMap<PlayerId, RoadmapTurnPlan>,
}

impl RoadmapTurnPlanRegistry {
    pub(super) fn replace(&mut self, plan: RoadmapTurnPlan) {
        self.plans.insert(plan.player_id, plan);
    }

    pub(super) fn portfolio_for_turn(
        &self,
        player_id: PlayerId,
        turn: u32,
    ) -> Option<&IslandCampaignPortfolio> {
        self.plans
            .get(&player_id)
            .filter(|plan| plan.turn == turn)
            .filter(|plan| plan.is_consistent())
            .map(|plan| &plan.portfolio)
    }
}

/// 当ターンにRoadmapが採用した島作戦を返す。
///
/// 戦略案探索は保留中なので、これは既存analyzerの出力をRoadmapが受理した結果である。
/// 受理後のSquad再編ではこのsnapshotだけを参照し、再度portfolioを分析しない。
pub(crate) fn current_turn_portfolio(
    world: &World,
    player_id: PlayerId,
    turn: u32,
) -> Option<IslandCampaignPortfolio> {
    world
        .get_resource::<RoadmapTurnPlanRegistry>()
        .and_then(|plans| plans.portfolio_for_turn(player_id, turn))
        .cloned()
}
