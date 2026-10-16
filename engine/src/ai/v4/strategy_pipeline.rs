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
    crate::ai::v4::capital_route::prepare_capital_route_topologies(world, player_id);
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
        crate::ai::v4::capital_route::prepare_capital_route_topologies(world, player_id);
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

/// 生産snapshotの前にだけ用意する小規模経路の戦略入力。永続cacheは増やさない。
pub(in crate::ai::v4) struct ProductionScanPreparation {
    pub(in crate::ai::v4) opening_policy: super::rolling_plan::OpeningProductionPolicy,
    pub(in crate::ai::v4) portfolio: IslandCampaignPortfolio,
    pub(in crate::ai::v4) logistics_plan: Option<super::logistics_plan::V4LogisticsPlan>,
    pub(in crate::ai::v4) pending_capture_replacements:
        crate::collections::HashSet<crate::ai::islands::IslandId>,
}

/// 旧collectのprofile固定・未採用時の解析を、World更新を許す入口へ明示的に移す。
/// 採用済みportfolioがあれば再解析せず、なければ同じ準備を行って今回のsnapshotへ渡す。
pub(in crate::ai::v4) fn prepare_production_scan(
    world: &mut World,
    player_id: PlayerId,
) -> Option<ProductionScanPreparation> {
    if !crate::ai::code_path::is_extended_at("v4/mod.rs::collect") {
        return None;
    }
    world.get_resource::<crate::resources::Map>()?;
    world.get_resource::<crate::resources::UnitRegistry>()?;
    world.get_resource::<crate::resources::DamageChart>()?;
    world.get_resource::<crate::resources::master_data::MasterDataRegistry>()?;
    let opening_policy = match crate::ai::strategy_profile::profile_for(world, player_id) {
        V4StrategyProfile::Standard => super::rolling_plan::OpeningProductionPolicy::Standard,
        V4StrategyProfile::SmallMapExpansion => {
            super::rolling_plan::OpeningProductionPolicy::SmallMapExpansion
        }
    };
    // 資金の観測に失敗する入力では、旧collect同様に戦略解析まで進めない。
    world
        .get_resource::<crate::resources::Players>()?
        .0
        .iter()
        .find(|player| player.id == player_id)?;
    // 旧snapshotは解析前の兵站・再占領契約を読んでいた。解析による台帳更新後の
    // 値へすり替えないよう、今回の入力としてこの時点の観測を保持する。
    let logistics_plan = world
        .get_resource::<super::logistics_plan::V4LogisticsPlanRegistry>()
        .and_then(|registry| registry.plan(player_id))
        .cloned();
    let pending_capture_replacements = world
        .get_resource::<super::victory_roadmap::VictoryRoadmapRegistry>()
        .map(|roadmap| {
            roadmap
                .operations_for(player_id)
                .into_iter()
                .filter(|operation| {
                    roadmap.capture_replacement_pending(player_id, operation.island_id)
                })
                .map(|operation| operation.island_id)
                .collect()
        })
        .unwrap_or_default();
    let portfolio = world
        .get_resource::<crate::ai::planning::decision::AdoptedTurnPortfolio>()
        .and_then(|cache| cache.campaign_portfolio(player_id))
        .cloned()
        .unwrap_or_else(|| {
            let mut portfolio =
                crate::ai::island_campaign_analysis::analyze_island_campaign(world, player_id);
            prepare_strategy(world, player_id, &mut portfolio);
            portfolio
        });
    Some(ProductionScanPreparation {
        opening_policy,
        portfolio,
        logistics_plan,
        pending_capture_replacements,
    })
}
