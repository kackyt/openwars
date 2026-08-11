use crate::ai::island_campaign::{
    CampaignSustainmentCoverage, IslandCampaignCandidate, IslandCampaignDecision,
    preferred_logistics_target,
};
use crate::ai::islands::IslandId;
use crate::components::{GridPosition, PlayerId};
use crate::resources::Map;
use bevy_ecs::prelude::Resource;
use std::collections::{HashMap, HashSet};

/// 予測占領手番をこの幅だけ超過した場合、同じ経路を含めて見積り直す。
const COMPLETION_DELAY_GRACE_TURNS: u32 = 1;
/// 兵站のないカテゴリを抱えたまま最終強襲へ進む場合の通常シナリオ上の準備遅延。
const NORMAL_UNSUPPORTED_CATEGORY_TURNS: u32 = 2;
/// 悲観シナリオでは、補給・修理不能による帰投と再出撃の遅れをより重く扱う。
const PESSIMISTIC_UNSUPPORTED_CATEGORY_TURNS: u32 = 3;
/// 首都侵攻を開始してから敵が対抗生産できる最小の観測窓。
const ENEMY_RESPONSE_WINDOW_TURNS: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogisticsReplanReason {
    InitialSelection,
    ObjectiveAdvanced,
    CompletionDelayed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogisticsStageForecast {
    pub island_id: IslandId,
    pub planned_completion_turn: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogisticsRouteMetrics {
    pub normal_assault_ready_turn: u32,
    pub pessimistic_assault_ready_turn: u32,
    pub projected_income_per_turn: u32,
    pub acquisition_cost: u32,
    pub staging_distance: u32,
    pub unsupported_categories: u32,
}

/// V4が現在コミットしている、最終強襲前の兵站経路。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V4LogisticsPlan {
    pub plan_id: u64,
    pub player_id: PlayerId,
    pub created_turn: u32,
    pub revised_turn: u32,
    pub last_observed_turn: u32,
    pub revision: u32,
    /// 作成時に地理・施設配置から固定した経路。工程完了後もidentityとして残す。
    pub route_islands: Vec<IslandId>,
    /// 固定経路のうち、まだ確保が必要な島。
    pub selected_islands: Vec<IslandId>,
    pub stages: Vec<LogisticsStageForecast>,
    pub direct_metrics: LogisticsRouteMetrics,
    /// 最後にcommitまたはrevisionした時点の固定計画値。
    pub selected_metrics: LogisticsRouteMetrics,
    /// 現在盤面から同じ選択島を完遂した場合の最新予測。
    pub current_forecast_metrics: LogisticsRouteMetrics,
    pub replan_reason: LogisticsReplanReason,
}

/// 毎ターン変わる候補評価と、実行中の兵站経路を分離して保持する。
#[derive(Resource, Debug, Default)]
pub struct V4LogisticsPlanRegistry {
    next_plan_id: u64,
    plans: HashMap<PlayerId, V4LogisticsPlan>,
}

impl V4LogisticsPlanRegistry {
    pub fn plan(&self, player_id: PlayerId) -> Option<&V4LogisticsPlan> {
        self.plans.get(&player_id)
    }
}

#[derive(Debug, Clone)]
struct LogisticsOption {
    island_id: IslandId,
    capital_distance: u32,
    home_distance: u32,
    normal_completion_offset: u32,
    pessimistic_completion_offset: u32,
    income_per_turn: u32,
    acquisition_cost: u32,
    ground: bool,
    air: bool,
    sea: bool,
}

#[derive(Debug, Clone)]
struct RouteEvaluation {
    selected_indices: Vec<usize>,
    stages: Vec<LogisticsStageForecast>,
    metrics: LogisticsRouteMetrics,
}

#[derive(Debug, Clone, Copy)]
struct RouteContext {
    turn: u32,
    friendly_income_per_turn: u32,
    enemy_income_per_turn: u32,
    available_funds: u32,
    direct_distance: u32,
    direct_transport_eta: u32,
    assault_budget: u32,
    initial_coverage: CampaignSustainmentCoverage,
    require_ground: bool,
    require_air: bool,
    require_sea: bool,
}

fn ceil_div(value: u32, divisor: u32) -> u32 {
    value.div_ceil(divisor.max(1))
}

fn missing_categories(coverage: CampaignSustainmentCoverage, context: RouteContext) -> u32 {
    u32::from(context.require_ground && !coverage.ground)
        .saturating_add(u32::from(context.require_air && !coverage.air))
        .saturating_add(u32::from(context.require_sea && !coverage.sea))
}

fn scaled_staging_eta(context: RouteContext, staging_distance: u32) -> u32 {
    if staging_distance == 0 {
        return 0;
    }
    if context.direct_distance == 0 {
        return context.direct_transport_eta.max(1);
    }
    ceil_div(
        context
            .direct_transport_eta
            .max(1)
            .saturating_mul(staging_distance),
        context.direct_distance,
    )
}

fn evaluate_route(
    context: RouteContext,
    options: &[LogisticsOption],
    mut selected_indices: Vec<usize>,
) -> RouteEvaluation {
    selected_indices.sort_unstable_by_key(|index| {
        let option = &options[*index];
        (
            option.home_distance,
            option.capital_distance,
            option.island_id.0,
        )
    });
    let mut coverage = context.initial_coverage;
    let mut projected_income = context.friendly_income_per_turn;
    let mut acquisition_cost = 0_u32;
    let mut normal_acquisition = 0_u32;
    let mut pessimistic_acquisition = 0_u32;
    let mut staging_distance = context.direct_distance;
    let mut stages = Vec::new();
    let mut cumulative_acquisition_cost = 0_u32;
    for index in &selected_indices {
        let option = &options[*index];
        coverage.ground |= option.ground;
        coverage.air |= option.air;
        coverage.sea |= option.sea;
        projected_income = projected_income.saturating_add(option.income_per_turn);
        acquisition_cost = acquisition_cost.saturating_add(option.acquisition_cost);
        cumulative_acquisition_cost =
            cumulative_acquisition_cost.saturating_add(option.acquisition_cost);
        normal_acquisition = normal_acquisition.max(option.normal_completion_offset);
        pessimistic_acquisition = pessimistic_acquisition.max(option.pessimistic_completion_offset);
        staging_distance = staging_distance.min(option.capital_distance);
        stages.push(LogisticsStageForecast {
            island_id: option.island_id,
            planned_completion_turn: context
                .turn
                .saturating_add(option.normal_completion_offset)
                .saturating_add(ceil_div(
                    cumulative_acquisition_cost.saturating_sub(context.available_funds),
                    context.friendly_income_per_turn,
                )),
        });
    }

    let funding_delay = ceil_div(
        acquisition_cost.saturating_sub(context.available_funds),
        context.friendly_income_per_turn,
    );
    let staging_eta = scaled_staging_eta(context, staging_distance);
    let formation_turns = ceil_div(context.assault_budget, projected_income);
    let unsupported = missing_categories(coverage, context);
    let normal_offset = funding_delay
        .saturating_add(normal_acquisition)
        .saturating_add(staging_eta)
        .saturating_add(formation_turns)
        .saturating_add(unsupported.saturating_mul(NORMAL_UNSUPPORTED_CATEGORY_TURNS));
    let response_delay = ceil_div(
        context
            .enemy_income_per_turn
            .saturating_mul(ENEMY_RESPONSE_WINDOW_TURNS),
        projected_income,
    );
    let pessimistic_offset = funding_delay
        .saturating_add(pessimistic_acquisition)
        .saturating_add(staging_eta.saturating_add(u32::from(!selected_indices.is_empty())))
        .saturating_add(formation_turns)
        .saturating_add(response_delay)
        .saturating_add(unsupported.saturating_mul(PESSIMISTIC_UNSUPPORTED_CATEGORY_TURNS));
    RouteEvaluation {
        selected_indices,
        stages,
        metrics: LogisticsRouteMetrics {
            normal_assault_ready_turn: context.turn.saturating_add(normal_offset),
            pessimistic_assault_ready_turn: context.turn.saturating_add(pessimistic_offset),
            projected_income_per_turn: projected_income,
            acquisition_cost,
            staging_distance,
            unsupported_categories: unsupported,
        },
    }
}

/// 地形・施設配置だけから、補給hubと敵側の争奪要地を選ぶ。
/// 資金、現在収入、敵戦力、毎手番のETAは経路identityへ使用しない。
fn choose_route(
    context: RouteContext,
    options: &[LogisticsOption],
) -> (RouteEvaluation, RouteEvaluation) {
    let direct = evaluate_route(context, options, Vec::new());
    let mut selected = Vec::new();
    let mut coverage = context.initial_coverage;
    while missing_categories(coverage, context) > 0 {
        let selected_set = selected.iter().copied().collect::<HashSet<_>>();
        let Some(index) = options
            .iter()
            .enumerate()
            .filter(|(index, _)| !selected_set.contains(index))
            .filter(|(_, option)| {
                (!coverage.ground && option.ground)
                    || (!coverage.air && context.require_air && option.air)
                    || (!coverage.sea && context.require_sea && option.sea)
            })
            .min_by_key(|(_, option)| {
                (
                    option.home_distance.max(option.capital_distance),
                    option.home_distance.saturating_add(option.capital_distance),
                    option.capital_distance,
                    option.island_id.0,
                )
            })
            .map(|(index, _)| index)
        else {
            break;
        };
        coverage.ground |= options[index].ground;
        coverage.air |= options[index].air;
        coverage.sea |= options[index].sea;
        selected.push(index);
    }

    // 敵側に進む未選択島を1つ、前進争奪要地とする。任意の平均値や現在収入を
    // 閾値にせず、首都間の位置関係と施設に埋め込まれた恒久収入だけで順位付けする。
    let selected_set = selected.iter().copied().collect::<HashSet<_>>();
    if let Some(index) = options
        .iter()
        .enumerate()
        .filter(|(index, option)| {
            !selected_set.contains(index) && option.capital_distance < option.home_distance
        })
        .min_by_key(|(_, option)| {
            (
                std::cmp::Reverse(option.income_per_turn),
                option.capital_distance,
                option.home_distance.saturating_add(option.capital_distance),
                option.island_id.0,
            )
        })
        .map(|(index, _)| index)
    {
        selected.push(index);
    }
    (direct, evaluate_route(context, options, selected))
}

fn candidate_options(
    candidates: &[IslandCampaignCandidate],
    assault_index: usize,
    assault_target: GridPosition,
    home_position: GridPosition,
    coverage: CampaignSustainmentCoverage,
    map: &Map,
    committed_islands: &HashSet<IslandId>,
) -> Vec<LogisticsOption> {
    let assault = candidates[assault_index].clone();
    let assault_eta = assault.transport_eta.unwrap_or(u32::MAX);
    let mut options = candidates
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != assault_index)
        .filter(|(_, candidate)| {
            // 逐次増援を完全に0へ固定することはできないため、兵站工程は全施設を
            // 確保して補給・修理に使える時点で完了とする。残敵は保持・掃討工程で扱う。
            !committed_islands.contains(&candidate.assessment.island_id)
                || candidate
                    .assessment
                    .neutral_properties
                    .saturating_add(candidate.assessment.enemy_properties)
                    > 0
        })
        .filter(|(_, candidate)| {
            committed_islands.contains(&candidate.assessment.island_id)
                || matches!(
                    candidate.assessment.decision,
                    IslandCampaignDecision::Expand
                        | IslandCampaignDecision::Secure
                        | IslandCampaignDecision::Contest
                        | IslandCampaignDecision::Reinforce
                )
                || (candidate.assessment.decision == IslandCampaignDecision::Assault
                    && candidate.existing_operation.is_some())
        })
        .filter(|(_, candidate)| {
            candidate
                .ground_sustainment_sites
                .saturating_add(candidate.air_sustainment_sites)
                .saturating_add(candidate.sea_sustainment_sites)
                > 0
                && (committed_islands.contains(&candidate.assessment.island_id)
                    || (candidate
                        .transport_eta
                        .is_some_and(|eta| eta <= assault_eta)
                        && preferred_logistics_target(candidate, &assault, coverage).is_some()))
        })
        .map(|(_, candidate)| {
            let remaining_properties = candidate
                .assessment
                .neutral_properties
                .saturating_add(candidate.assessment.enemy_properties)
                .max(1);
            let capture_units = candidate.requirement.capture_units.max(1);
            let capture_turns = remaining_properties
                .div_ceil(capture_units)
                .saturating_mul(2);
            let transport_and_deployment = candidate
                .transport_eta
                .unwrap_or(0)
                .saturating_add(u32::from(candidate.transport_eta.unwrap_or(0) > 0));
            let normal_completion = transport_and_deployment.saturating_add(capture_turns);
            let enemy_contest_delay = u32::from(
                candidate
                    .assessment
                    .enemy_arrival_eta
                    .is_some_and(|enemy_eta| enemy_eta <= transport_and_deployment),
            )
            .saturating_add(u32::from(candidate.assessment.enemy_combat_units > 0));
            LogisticsOption {
                island_id: candidate.assessment.island_id,
                capital_distance: map.distance(
                    candidate.target_position.x,
                    candidate.target_position.y,
                    assault_target.x,
                    assault_target.y,
                ),
                home_distance: map.distance(
                    home_position.x,
                    home_position.y,
                    candidate.target_position.x,
                    candidate.target_position.y,
                ),
                normal_completion_offset: normal_completion,
                pessimistic_completion_offset: normal_completion
                    .saturating_add(enemy_contest_delay),
                income_per_turn: candidate.island_income_per_turn,
                acquisition_cost: candidate.requirement.total_budget,
                ground: candidate.ground_sustainment_sites > 0,
                air: candidate.air_sustainment_sites > 0,
                sea: candidate.sea_sustainment_sites > 0,
            }
        })
        .collect::<Vec<_>>();
    options.sort_by_key(|option| option.island_id.0);
    options
}

fn selected_evaluation(
    context: RouteContext,
    options: &[LogisticsOption],
    islands: &[IslandId],
) -> (RouteEvaluation, bool) {
    let indices = islands
        .iter()
        .filter_map(|island| {
            options
                .iter()
                .position(|option| option.island_id == *island)
        })
        .collect::<Vec<_>>();
    let objective_advanced = indices.len() < islands.len();
    (
        evaluate_route(context, options, indices),
        objective_advanced,
    )
}

/// 現在盤面の候補を比較し、コミット済み経路を維持または明示的に改訂したうえで
/// 選択島を兵站前提へ昇格する。
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_persistent_logistics_plan(
    registry: &mut V4LogisticsPlanRegistry,
    player_id: PlayerId,
    turn: u32,
    candidates: &mut [IslandCampaignCandidate],
    map: &Map,
    friendly_income_per_turn: u32,
    enemy_income_per_turn: u32,
    available_funds: u32,
    forward_coverage: CampaignSustainmentCoverage,
    home_position: Option<GridPosition>,
    enemy_capital: Option<(IslandId, GridPosition)>,
) {
    for candidate in candidates.iter_mut() {
        candidate.logistics_prerequisite = false;
        candidate.logistics_priority_rank = None;
    }
    let Some((enemy_capital_island, enemy_capital_position)) = enemy_capital else {
        return;
    };
    let Some(assault_index) = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.assessment.decision == IslandCampaignDecision::Assault
                && candidate.existing_operation.is_none()
                && candidate.assessment.island_id == enemy_capital_island
        })
        .min_by_key(|(_, candidate)| {
            (
                candidate.transport_eta.unwrap_or(u32::MAX),
                candidate.assessment.required_budget,
                candidate.assessment.island_id.0,
            )
        })
        .map(|(index, _)| index)
    else {
        return;
    };
    let assault = candidates[assault_index].clone();
    let Some(home) = home_position else {
        return;
    };
    let assault_uses_air = assault
        .assault_transport_types
        .contains(&crate::resources::UnitType::TransportHelicopter);
    let assault_uses_sea = assault
        .assault_transport_types
        .contains(&crate::resources::UnitType::Lander);
    // 実行schedule未確定の暫定候補がヘリと揚陸艇を列挙していても、両補給網を
    // 同時必須にはしない。中立島争奪へ直ちに使える航空路を優先する。
    let require_air = assault_uses_air || !assault_uses_sea;
    let require_sea = assault_uses_sea && !assault_uses_air;
    let context = RouteContext {
        turn,
        friendly_income_per_turn,
        enemy_income_per_turn,
        available_funds,
        direct_distance: map.distance(
            home.x,
            home.y,
            enemy_capital_position.x,
            enemy_capital_position.y,
        ),
        direct_transport_eta: assault.transport_eta.unwrap_or(u32::MAX / 4),
        assault_budget: assault.requirement.total_budget,
        initial_coverage: forward_coverage,
        require_ground: true,
        require_air,
        require_sea,
    };
    let committed_islands = registry
        .plans
        .get(&player_id)
        .map(|plan| plan.route_islands.iter().copied().collect())
        .unwrap_or_default();
    let options = candidate_options(
        candidates,
        assault_index,
        enemy_capital_position,
        home,
        forward_coverage,
        map,
        &committed_islands,
    );
    let direct = evaluate_route(context, &options, Vec::new());

    let (
        selected,
        planned_metrics,
        mut planned_stages,
        reason,
        revision,
        created_turn,
        revised_turn,
        plan_id,
        route_islands,
    ) = if let Some(current_plan) = registry.plans.get(&player_id) {
        let (current, objective_advanced) =
            selected_evaluation(context, &options, &current_plan.route_islands);
        let delayed = current.stages.iter().any(|forecast| {
            current_plan.stages.iter().any(|planned| {
                planned.island_id == forecast.island_id
                    && forecast.planned_completion_turn
                        > planned
                            .planned_completion_turn
                            .saturating_add(COMPLETION_DELAY_GRACE_TURNS)
            })
        }) || current_plan.stages.iter().any(|stage| {
            turn > stage
                .planned_completion_turn
                .saturating_add(COMPLETION_DELAY_GRACE_TURNS)
                && candidates.iter().any(|candidate| {
                    candidate.assessment.island_id == stage.island_id
                        && candidate
                            .assessment
                            .neutral_properties
                            .saturating_add(candidate.assessment.enemy_properties)
                            > 0
                })
        });
        if objective_advanced || delayed {
            let reason = if objective_advanced {
                LogisticsReplanReason::ObjectiveAdvanced
            } else {
                LogisticsReplanReason::CompletionDelayed
            };
            // 地理的経路は固定し、動的な変化は同じ経路の工程予定だけを改訂する。
            let revised = current.clone();
            (
                revised.clone(),
                revised.metrics,
                revised.stages.clone(),
                reason,
                current_plan.revision.saturating_add(1),
                current_plan.created_turn,
                turn,
                current_plan.plan_id,
                current_plan.route_islands.clone(),
            )
        } else {
            (
                current,
                current_plan.selected_metrics,
                current_plan.stages.clone(),
                current_plan.replan_reason,
                current_plan.revision,
                current_plan.created_turn,
                current_plan.revised_turn,
                current_plan.plan_id,
                current_plan.route_islands.clone(),
            )
        }
    } else {
        let (_, best) = choose_route(context, &options);
        let route_islands = best
            .selected_indices
            .iter()
            .map(|index| options[*index].island_id)
            .collect::<Vec<_>>();
        registry.next_plan_id = registry.next_plan_id.saturating_add(1);
        (
            best.clone(),
            best.metrics,
            best.stages.clone(),
            LogisticsReplanReason::InitialSelection,
            0,
            turn,
            turn,
            registry.next_plan_id,
            route_islands,
        )
    };

    let mut ordered_indices = selected.selected_indices.clone();
    ordered_indices.sort_unstable_by_key(|index| {
        let option = &options[*index];
        (
            option.home_distance,
            option.capital_distance,
            option.island_id.0,
        )
    });
    let selected_islands = ordered_indices
        .iter()
        .map(|index| options[*index].island_id)
        .collect::<Vec<_>>();
    planned_stages.sort_by_key(|stage| {
        selected_islands
            .iter()
            .position(|island| *island == stage.island_id)
            .unwrap_or(usize::MAX)
    });
    for (priority_rank, island) in selected_islands.iter().enumerate() {
        let Some(candidate) = candidates
            .iter_mut()
            .find(|candidate| candidate.assessment.island_id == *island)
        else {
            continue;
        };
        candidate.logistics_prerequisite = true;
        candidate.logistics_priority_rank = Some(u32::try_from(priority_rank).unwrap_or(u32::MAX));
        if let Some(target) = preferred_logistics_target(candidate, &assault, forward_coverage) {
            candidate.target_position = target;
            if let Some(index) = candidate
                .capture_target_positions
                .iter()
                .position(|position| *position == target)
            {
                candidate.capture_target_positions.swap(0, index);
            }
        }
        candidate.assessment.decision_reason = format!(
            "勝利ロードマップの兵站経路plan {}で首都攻略開始を通常T{}・悲観T{}へ短縮する",
            plan_id,
            selected.metrics.normal_assault_ready_turn,
            selected.metrics.pessimistic_assault_ready_turn
        );
    }
    registry.plans.insert(
        player_id,
        V4LogisticsPlan {
            plan_id,
            player_id,
            created_turn,
            revised_turn,
            last_observed_turn: turn,
            revision,
            route_islands,
            selected_islands,
            stages: planned_stages,
            direct_metrics: direct.metrics,
            selected_metrics: planned_metrics,
            current_forecast_metrics: selected.metrics,
            replan_reason: reason,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(
        id: usize,
        completion: u32,
        pessimistic: u32,
        distance: u32,
        income: u32,
        cost: u32,
        air: bool,
    ) -> LogisticsOption {
        LogisticsOption {
            island_id: IslandId(id),
            capital_distance: distance,
            home_distance: 20,
            normal_completion_offset: completion,
            pessimistic_completion_offset: pessimistic,
            income_per_turn: income,
            acquisition_cost: cost,
            ground: true,
            air,
            sea: false,
        }
    }

    fn context() -> RouteContext {
        RouteContext {
            turn: 1,
            friendly_income_per_turn: 14_000,
            enemy_income_per_turn: 14_000,
            available_funds: 14_000,
            direct_distance: 28,
            direct_transport_eta: 4,
            assault_budget: 25_000,
            initial_coverage: CampaignSustainmentCoverage::default(),
            require_ground: true,
            require_air: true,
            require_sea: false,
        }
    }

    #[test]
    fn chooses_early_staging_island_instead_of_direct_assault() {
        let options = vec![option(3, 5, 5, 14, 3_000, 11_000, true)];

        let (direct, selected) = choose_route(context(), &options);

        assert_eq!(selected.selected_indices, vec![0]);
        assert!(
            selected.metrics.pessimistic_assault_ready_turn
                < direct.metrics.pessimistic_assault_ready_turn
        );
    }

    #[test]
    fn adds_enemy_side_strategic_island_after_nearby_supply_hub() {
        let mut options = vec![
            option(2, 6, 6, 12, 3_000, 17_000, true),
            option(3, 5, 5, 8, 5_000, 11_000, true),
        ];
        options[0].home_distance = 12;
        options[1].home_distance = 20;

        let (_, selected) = choose_route(context(), &options);

        assert_eq!(
            selected
                .selected_indices
                .iter()
                .map(|index| options[*index].island_id)
                .collect::<Vec<_>>(),
            vec![IslandId(2), IslandId(3)]
        );
    }

    #[test]
    fn dynamic_budget_and_income_change_schedule_but_not_geographic_route() {
        let mut options = vec![
            option(2, 6, 7, 12, 3_000, 17_000, true),
            option(3, 5, 8, 8, 5_000, 11_000, true),
        ];
        options[0].home_distance = 12;
        options[1].home_distance = 20;
        let first_context = context();
        let mut changed_context = context();
        changed_context.friendly_income_per_turn = 3_000;
        changed_context.enemy_income_per_turn = 30_000;
        changed_context.available_funds = 0;
        changed_context.direct_transport_eta = 9;

        let (_, first) = choose_route(first_context, &options);
        let (_, changed) = choose_route(changed_context, &options);

        assert_eq!(first.selected_indices, changed.selected_indices);
        assert_ne!(
            first.metrics.pessimistic_assault_ready_turn,
            changed.metrics.pessimistic_assault_ready_turn
        );
    }

    #[test]
    fn does_not_add_slow_low_income_island() {
        let options = vec![
            option(3, 5, 5, 14, 3_000, 11_000, true),
            option(7, 12, 15, 20, 1_000, 20_000, false),
        ];

        let (_, selected) = choose_route(context(), &options);

        assert_eq!(selected.selected_indices, vec![0]);
    }
}
