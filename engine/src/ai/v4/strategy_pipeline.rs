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
    crate::ai::v4::capital_route::topology::prepare_capital_route_topologies(world, player_id);
    crate::ai::v4::capital_route::force_demand::refine_same_land_route_milestones(
        world, player_id, portfolio,
    );
    crate::ai::v4::capital_route::force_demand::observe_same_land_route_bridgeheads(
        world,
        player_id,
        &portfolio.active_offensives,
    );
}

/// Squad解析の前にDAGを確実に初期化する。Strategy単体テストもこの順序に依存しない。
pub(crate) fn before_squad_planning(world: &mut World, player_id: PlayerId) {
    if uses_small_map_expansion(world, player_id) {
        crate::ai::v4::capital_route::topology::prepare_capital_route_topologies(world, player_id);
    }
}

/// 提案側で精錬・観測済みのportfolioをRoadmapで採用し、実働Squadへ返す。
/// 精錬と橋頭堡観測は `prepare_strategy` にだけ置き、採用時は繰り返さない。
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
    super::victory_roadmap::reconcile_campaign_roadmap(world, player_id, &portfolio, manager);
    super::victory_roadmap::current_turn_portfolio(world, player_id, turn).unwrap_or(portfolio)
}

/// Squad構築後はDAGの実行区間とRoadmap実績を更新し、採用portfolioは選び直さない。
pub(crate) fn after_squad_planning(
    world: &mut World,
    player_id: PlayerId,
    portfolio: &IslandCampaignPortfolio,
    manager: &mut crate::ai::squad::SquadManager,
) {
    if !uses_small_map_expansion(world, player_id) {
        return;
    }
    crate::ai::v4::capital_route::commitment::refresh_capital_route_path_commitments(
        world, player_id, manager,
    );
    super::victory_roadmap::observe_adopted_campaign_roadmap(world, player_id, portfolio, manager);
}
