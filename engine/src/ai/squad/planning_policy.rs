//! 小規模Squad編成の採用・占領順序方策。

use super::{
    CapturePlanningContext, GridPosition, PlayerId, Property, SquadManager, Terrain, World,
    is_terrain_reachable,
};
pub(super) fn adopt_portfolio(
    world: &mut World,
    player: PlayerId,
    manager: &SquadManager,
    portfolio: &crate::ai::island_campaign::IslandCampaignPortfolio,
    is_v4: bool,
) -> crate::ai::island_campaign::IslandCampaignPortfolio {
    if !is_v4 {
        return portfolio.clone();
    }
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    crate::ai::v4::strategy_pipeline::portfolio_for_squads(
        world,
        player,
        turn,
        portfolio.clone(),
        manager,
    )
}

/// 小規模マップの占領目標順: 自首都から遠い順（同点は敵首都に近い順）。
/// 小規模プロファイルでない場合は通常の順序を使う。
pub(super) fn sort_small_map_capture_targets(
    world: &mut World,
    player: PlayerId,
    targets: &mut [GridPosition],
    context: &CapturePlanningContext<'_>,
) {
    if crate::ai::strategy_profile::current_profile(world, player)
        != crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion
    {
        super::formation::sort_capture_targets_by_nearest_infantry(targets, context);
        return;
    }
    let enemy_capital = {
        let mut properties = world.query::<(&GridPosition, &Property)>();
        properties.iter(world).find_map(|(position, property)| {
            (property.terrain == Terrain::Capital
                && property.owner_id != Some(player)
                && property.owner_id.is_some())
            .then_some(*position)
        })
    };
    // 小規模かつ自首都なしでは従来どおり入力順を保つ。通常sortへ戻さない。
    let Some(capital) = context.capital else {
        return;
    };
    targets.sort_unstable_by_key(|target| {
        let distance = context
            .map
            .distance(capital.x, capital.y, target.x, target.y);
        let enemy_distance = enemy_capital
            .map(|enemy| context.map.distance(enemy.x, enemy.y, target.x, target.y))
            .unwrap_or(0);
        (
            std::cmp::Reverse(distance),
            enemy_distance,
            target.y,
            target.x,
        )
    });
}

/// 小規模マップの占領要員: 同島で到達できる歩兵のうち、目標に最も近いもの。
pub(super) fn select_nearest_capture_infantry(
    context: &CapturePlanningContext<'_>,
    island: crate::ai::islands::IslandId,
    target: &GridPosition,
) -> Option<usize> {
    context
        .infantry
        .iter()
        .enumerate()
        .filter(|(_, (_, position, stats))| {
            context
                .island_map
                .get_island_at(position)
                .is_some_and(|candidate| candidate.id == island)
                && is_terrain_reachable(
                    context.map,
                    context.registry,
                    (position.x, position.y),
                    (target.x, target.y),
                    stats.movement_type,
                )
        })
        .min_by_key(|(_, (entity, position, _))| {
            (
                context
                    .map
                    .distance(position.x, position.y, target.x, target.y),
                entity.to_bits(),
            )
        })
        .map(|(index, _)| index)
}

/// ゲームの戦略状況に基づいて、自動的に部隊の構築と新規メンバーの割り当てを行います。
pub(crate) fn plan_squads(world: &mut World, perspective_player: PlayerId) {
    super::formation::plan_squads_with_policy(
        world,
        perspective_player,
        super::formation::PlanningPolicy::SmallMap,
    );
}
