//! V4のターン入口で選ばれる戦略パイプライン。
//!
//! 小規模マップ向けのDAG/Roadmap処理を各利用者が独自に有効化してはいけない。
//! このモジュールだけがプロファイルを解釈し、Strategy・Squad・生産が同じ作戦系を
//! 参照する境界にする。

use crate::ai::island_campaign::IslandCampaignPortfolio;
use crate::ai::strategy_profile::{V4StrategyProfile, current_profile};
use crate::components::PlayerId;
use bevy_ecs::prelude::*;

fn uses_small_map_expansion(world: &World, player_id: PlayerId) -> bool {
    current_profile(world, player_id) == V4StrategyProfile::SmallMapExpansion
}

/// Strategy解析時に小規模マップ用の静的DAGと当ターンのMilestoneを作る。
pub(crate) fn prepare_strategy(
    world: &mut World,
    player_id: PlayerId,
    portfolio: &mut IslandCampaignPortfolio,
) {
    if !uses_small_map_expansion(world, player_id) {
        return;
    }
    super::prepare_capital_route_topologies(world, player_id);
    super::refine_same_land_route_milestones(world, player_id, portfolio);
    super::observe_same_land_route_bridgeheads(world, player_id, &portfolio.active_offensives);
}

/// Squad解析の前にDAGを確実に初期化する。Strategy単体テストもこの順序に依存しない。
pub(crate) fn before_squad_planning(world: &mut World, player_id: PlayerId) {
    if uses_small_map_expansion(world, player_id) {
        super::prepare_capital_route_topologies(world, player_id);
    }
}

/// Roadmapを小規模パイプラインの正本として更新し、実働Squadが読むportfolioを返す。
pub(crate) fn portfolio_for_squads(
    world: &mut World,
    player_id: PlayerId,
    turn: u32,
    portfolio: IslandCampaignPortfolio,
    manager: &crate::ai::squad::SquadManager,
) -> IslandCampaignPortfolio {
    if !uses_small_map_expansion(world, player_id) {
        return portfolio;
    }
    let mut portfolio = portfolio;
    super::refine_same_land_route_milestones(world, player_id, &mut portfolio);
    super::observe_same_land_route_bridgeheads(world, player_id, &portfolio.active_offensives);
    super::victory_roadmap::reconcile_campaign_roadmap(world, player_id, &portfolio, manager);
    super::victory_roadmap::current_turn_portfolio(world, player_id, turn).unwrap_or(portfolio)
}

/// Squadを構築し終えた後、DAGの実行区間とRoadmapを同時に更新する。
pub(crate) fn after_squad_planning(
    world: &mut World,
    player_id: PlayerId,
    portfolio: &IslandCampaignPortfolio,
    manager: &mut crate::ai::squad::SquadManager,
) {
    if !uses_small_map_expansion(world, player_id) {
        return;
    }
    super::refresh_capital_route_path_commitments(world, player_id, manager);
    super::victory_roadmap::reconcile_campaign_roadmap(world, player_id, portfolio, manager);
}
