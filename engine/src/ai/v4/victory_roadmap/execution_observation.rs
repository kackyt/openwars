//! 勝利Roadmapのexecution_observation責務。

use super::dependency_graph::{
    RoadmapNodeState, StrategicOperationScope, operation_scope, rolling_operation_kind,
};
use super::strategic_selection::purpose_for;
use super::turn_plan::{RoadmapOperationDirective, RoadmapTurnPlan, RoadmapTurnPlanRegistry};
use super::{
    ActiveCombatPlanSummary, Faction, GridPosition, HashSet, IslandCampaignPortfolio, IslandMap,
    MatchState, OperationEntityRole, OperationKind, PlayerId, Property, SquadManager,
    StrategicPurpose, Terrain, Transporting, V4DeploymentRegistry, V4RollingPlanRegistry,
    VictoryRoadmapRegistry, World, diagnose_operation_execution, diagnose_operation_recoveries,
    estimate_full_objective_completion, operation_phase,
};

use super::{IslandId, OperationIssue, StrategicOperationId};
use crate::ai::squad::Squad;
use crate::ai::v4::campaign_execution::CampaignProductionRecord;
use crate::ai::v4::deployment::DeploymentAuditRecord;
use crate::ai::v4::logistics_plan::V4LogisticsPlan;
use bevy_ecs::prelude::Entity;

/// 今回の観測だけで共有する盤面事実。台帳更新後に取り直さない。
struct ExecutionFacts {
    turn: u32,
    island_map: IslandMap,
    property_snapshots: Vec<(GridPosition, Property)>,
    enemy_capital: Option<GridPosition>,
    enemy_capital_island: Option<IslandId>,
    enemy_unit_count: usize,
    enemy_positions: Vec<GridPosition>,
    combat_plan_summaries: Vec<ActiveCombatPlanSummary>,
    logistics_plan: Option<V4LogisticsPlan>,
    deployment_records: Vec<DeploymentAuditRecord>,
    owned_properties: HashSet<GridPosition>,
}
impl ExecutionFacts {
    fn collect(world: &mut World, player_id: PlayerId) -> Option<Self> {
        let turn = world
            .get_resource::<MatchState>()
            .map_or(0, |state| state.current_turn_number.0);
        let island_map = world.get_resource::<IslandMap>()?.clone();
        let mut property_snapshots = Vec::new();
        let mut property_query = world.query::<(&GridPosition, &Property)>();
        for (position, property) in property_query.iter(world) {
            property_snapshots.push((*position, *property));
        }
        let enemy_capital = property_snapshots
            .iter()
            .find(|(_, property)| {
                property.terrain == Terrain::Capital && property.owner_id != Some(player_id)
            })
            .map(|(position, _)| *position);
        let enemy_capital_island = enemy_capital
            .and_then(|position| island_map.get_island_at(&position))
            .map(|island| island.id);
        let enemy_unit_count = {
            let mut units = world.query::<&Faction>();
            units
                .iter(world)
                .filter(|faction| faction.0 != player_id)
                .count()
        };
        let enemy_positions = {
            let mut units = world.query::<(&Faction, &GridPosition, Option<&Transporting>)>();
            units
                .iter(world)
                .filter_map(|(faction, position, transporting)| {
                    (faction.0 != player_id && transporting.is_none()).then_some(*position)
                })
                .collect::<Vec<_>>()
        };
        let combat_plan_summaries = world
            .get_resource::<V4RollingPlanRegistry>()
            .map(|registry| registry.active_combat_plan_summaries(player_id))
            .unwrap_or_default();
        let logistics_plan = world
            .get_resource::<crate::ai::v4::logistics_plan::V4LogisticsPlanRegistry>()
            .and_then(|registry| registry.plan(player_id))
            .cloned();
        let deployment_records = world
            .get_resource::<V4DeploymentRegistry>()
            .map(|registry| registry.audit_records(player_id))
            .unwrap_or_default();
        let owned_properties = property_snapshots
            .iter()
            .filter_map(|(position, property)| {
                (property.owner_id == Some(player_id)).then_some(*position)
            })
            .collect::<HashSet<_>>();
        Some(Self {
            turn,
            island_map,
            property_snapshots,
            enemy_capital,
            enemy_capital_island,
            enemy_unit_count,
            enemy_positions,
            combat_plan_summaries,
            logistics_plan,
            deployment_records,
            owned_properties,
        })
    }
}

struct ExecutionContext<'a> {
    player_id: PlayerId,
    portfolio: &'a IslandCampaignPortfolio,
    manager: &'a SquadManager,
    facts: &'a ExecutionFacts,
}
struct AssignmentHistory {
    entities: Vec<(Entity, OperationEntityRole)>,
    issues: Vec<OperationIssue>,
    production: Vec<CampaignProductionRecord>,
}

/// 実績の照合結果。採用の承認に必要な首都位置だけを台帳と一緒に戻す。
pub(super) struct RoadmapObservation {
    pub(super) registry: VictoryRoadmapRegistry,
    pub(super) enemy_capital: Option<GridPosition>,
}

/// 観測→旧工程照合→島と所属→首都→実績確定の順で台帳を更新する。
pub(super) fn observe_campaign_execution(
    world: &mut World,
    player_id: PlayerId,
    portfolio: &IslandCampaignPortfolio,
    manager: &SquadManager,
) -> Option<RoadmapObservation> {
    let facts = ExecutionFacts::collect(world, player_id)?;
    let context = ExecutionContext {
        player_id,
        portfolio,
        manager,
        facts: &facts,
    };
    let mut registry = world
        .remove_resource::<VictoryRoadmapRegistry>()
        .unwrap_or_default();
    registry.expire_pending_steps(player_id, facts.turn);
    let roadmap_id = registry.ensure_roadmap(
        player_id,
        facts.turn,
        facts.enemy_capital,
        facts.enemy_capital_island,
        facts.enemy_unit_count,
    );
    context.prepare_observation(&mut registry, roadmap_id);
    context.observe_islands(&mut registry, roadmap_id);
    let has_regional_segment = context.capital_has_regional_segment();
    context.observe_assignments(world, &mut registry, roadmap_id);
    context.observe_capital(world, &mut registry, has_regional_segment);
    context.bind_combat_deployments(&mut registry);
    context.finish_observation(&mut registry);
    Some(RoadmapObservation {
        registry,
        enemy_capital: facts.enemy_capital,
    })
}

impl ExecutionContext<'_> {
    /// 前回の所属を解き、勝利条件としての常在首都Nodeを用意する。
    fn prepare_observation(
        &self,
        registry: &mut VictoryRoadmapRegistry,
        roadmap_id: super::VictoryRoadmapId,
    ) {
        let player_id = self.player_id;
        let turn = self.facts.turn;
        let enemy_capital = self.facts.enemy_capital;
        let enemy_capital_island = self.facts.enemy_capital_island;
        let owned_properties = &self.facts.owned_properties;
        for operation in registry
            .operations
            .values_mut()
            .filter(|operation| operation.player_id == player_id)
        {
            operation.active = false;
        }
        registry.entity_bindings.retain(|_, binding| {
            registry
                .operations
                .get(&binding.operation_id)
                .is_some_and(|op| op.player_id != player_id)
        });

        // 局地portfolioにまだ現れなくても、勝利条件そのものを親計画から消さない。
        // 実行schedule未選択は失敗ではなく、dependencyが満たされるまでLocked/Readyで待つ。
        if let (Some(capital), Some(capital_island)) = (enemy_capital, enemy_capital_island) {
            registry.ensure_capital_objective(
                roadmap_id,
                player_id,
                turn,
                capital_island,
                capital,
                owned_properties.contains(&capital),
            );
        }
    }

    /// 未採用の島も候補全集合から観測Nodeへ照合する。
    fn observe_islands(
        &self,
        registry: &mut VictoryRoadmapRegistry,
        roadmap_id: super::VictoryRoadmapId,
    ) {
        let player_id = self.player_id;
        let portfolio = self.portfolio;
        let turn = self.facts.turn;
        let island_map = &self.facts.island_map;
        let property_snapshots = &self.facts.property_snapshots;
        let enemy_capital = self.facts.enemy_capital;
        let enemy_capital_island = self.facts.enemy_capital_island;
        let owned_properties = &self.facts.owned_properties;
        let assigned_regional_islands = portfolio
            .defenses
            .iter()
            .chain(portfolio.active_offensives.iter())
            .filter(|assignment| {
                purpose_for(assignment, enemy_capital) != StrategicPurpose::AssaultCapital
            })
            .map(|assignment| assignment.island_id)
            .collect::<HashSet<_>>();
        // `portfolio.islands` は作戦候補ではなく全島評価である。ここで全島を観測Nodeへ
        // 写し、allocatorが今手番にassignmentを作らなかった島もRoadmapから消さない。
        // 敵首都島は常在AssaultCapital Nodeが担当し、Regional Nodeは実際に前段の
        // Milestoneが提案されたときだけ同じscopeで追加される。
        for assessment in &portfolio.islands {
            if Some(assessment.island_id) == enemy_capital_island
                || assigned_regional_islands.contains(&assessment.island_id)
            {
                continue;
            }
            let mut objectives = property_snapshots
                .iter()
                .filter(|(position, property)| {
                    property.max_capture_points > 0
                        && island_map
                            .get_island_at(position)
                            .is_some_and(|island| island.id == assessment.island_id)
                })
                .map(|(position, _)| *position)
                .collect::<Vec<_>>();
            objectives.sort_unstable_by_key(|position| (position.y, position.x));
            objectives.dedup();
            let tactical_anchor = objectives.first().copied().or_else(|| {
                island_map
                    .islands
                    .iter()
                    .find(|island| island.id == assessment.island_id)
                    .and_then(|island| {
                        let mut tiles = island.tiles.iter().copied().collect::<Vec<_>>();
                        tiles.sort_unstable_by_key(|position| (position.y, position.x));
                        tiles.first().copied()
                    })
            });
            if let Some(tactical_anchor) = tactical_anchor {
                registry.reconcile_observed_island(
                    roadmap_id,
                    player_id,
                    turn,
                    assessment,
                    tactical_anchor,
                    objectives,
                    owned_properties,
                );
            }
        }
    }

    /// 首都親作戦へ所属を引き継ぐ前段区間が残っているか。
    fn capital_has_regional_segment(&self) -> bool {
        let portfolio = self.portfolio;
        let enemy_capital = self.facts.enemy_capital;
        let enemy_capital_island = self.facts.enemy_capital_island;

        enemy_capital_island.is_some_and(|capital_island| {
            portfolio
                .defenses
                .iter()
                .chain(portfolio.active_offensives.iter())
                .any(|assignment| {
                    assignment.island_id == capital_island
                        && purpose_for(assignment, enemy_capital)
                            != StrategicPurpose::AssaultCapital
                })
        })
    }

    /// 採用順のまま各assignmentを実行作戦へ照合する。
    fn observe_assignments(
        &self,
        world: &mut World,
        registry: &mut VictoryRoadmapRegistry,
        roadmap_id: super::VictoryRoadmapId,
    ) {
        let portfolio = self.portfolio;
        for assignment in portfolio
            .defenses
            .iter()
            .chain(portfolio.active_offensives.iter())
        {
            self.observe_assignment(world, registry, roadmap_id, assignment);
        }
    }

    /// 更新前の所属・問題と生産実績を同じ作戦キーで取得する。
    fn assignment_history(
        &self,
        world: &World,
        registry: &VictoryRoadmapRegistry,
        assignment: &super::IslandCampaignAssignment,
        purpose: StrategicPurpose,
    ) -> AssignmentHistory {
        let player_id = self.player_id;
        let operation_key = (player_id, assignment.island_id, operation_scope(purpose));
        let previous_entities = registry
            .operation_keys
            .get(&operation_key)
            .and_then(|operation_id| registry.operations.get(operation_id))
            .map(|operation| {
                operation
                    .assigned_transports
                    .iter()
                    .map(|entity| (*entity, OperationEntityRole::Transport))
                    .chain(
                        operation
                            .assigned_capturers
                            .iter()
                            .map(|entity| (*entity, OperationEntityRole::Capture)),
                    )
                    .chain(
                        operation
                            .assigned_combat
                            .iter()
                            .map(|entity| (*entity, OperationEntityRole::Combat)),
                    )
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let previous_issues = registry
            .operation_keys
            .get(&operation_key)
            .and_then(|operation_id| registry.operations.get(operation_id))
            .map(|operation| operation.current_issues.clone())
            .unwrap_or_default();
        let production_records = world
            .get_resource::<crate::ai::v4::campaign_execution::V4CampaignExecutionRegistry>()
            .map(|execution| {
                execution
                    .records_for(player_id, assignment.island_id)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        AssignmentHistory {
            entities: previous_entities,
            issues: previous_issues,
            production: production_records,
        }
    }

    /// 首都終端と局地Milestoneで異なる目標集合を保持する。
    fn assignment_objectives(
        &self,
        assignment: &super::IslandCampaignAssignment,
        purpose: StrategicPurpose,
    ) -> Vec<GridPosition> {
        let enemy_capital = self.facts.enemy_capital;

        if purpose == StrategicPurpose::AssaultCapital {
            // 首都島でも首都1点だけへ縮約しない。IslandCampaignが観測した全未所有
            // 施設を同じ勝利作戦の前線目標として保持し、首都は終端目標として補う。
            let mut positions = assignment.capture_target_positions.clone();
            if let Some(capital) = enemy_capital
                && !positions.contains(&capital)
            {
                positions.push(capital);
            }
            positions
        } else {
            // 局地作戦は全島の施設一覧ではなく、portfolioが今選んだMilestoneだけを持つ。
            // 空集合でも防衛・集結地点を明示し、作戦所属Entityの行き先を失わせない。
            let mut positions = assignment.capture_target_positions.clone();
            if positions.is_empty() {
                positions.push(assignment.target_position);
            }
            positions
        }
    }

    /// 同じ作戦種別と島へ接続するCombat計画だけを照合する。
    fn matching_combat_plans(
        &self,
        assignment: &super::IslandCampaignAssignment,
        purpose: StrategicPurpose,
    ) -> Vec<&ActiveCombatPlanSummary> {
        let island_map = &self.facts.island_map;
        let combat_plan_summaries = &self.facts.combat_plan_summaries;
        let expected_plan_kind = rolling_operation_kind(purpose);

        combat_plan_summaries
            .iter()
            .filter(|plan| {
                plan.kind == expected_plan_kind
                    && (island_map
                        .get_island_at(&plan.anchor)
                        .is_some_and(|island| island.id == assignment.island_id)
                        || plan.objective_properties.iter().any(|position| {
                            island_map
                                .get_island_at(position)
                                .is_some_and(|island| island.id == assignment.island_id)
                        }))
            })
            .collect::<Vec<&ActiveCombatPlanSummary>>()
    }

    /// 実在敵と実行可能なCombat計画から制圧予定を求める。
    fn suppression_turn(
        &self,
        local_enemy_count: usize,
        matching_combat_plans: &[&ActiveCombatPlanSummary],
    ) -> Option<u32> {
        let turn = self.facts.turn;

        if local_enemy_count == 0 {
            Some(turn)
        } else if !matching_combat_plans.is_empty()
            && matching_combat_plans.iter().all(|plan| {
                plan.remaining_target_count == 0 || plan.planned_elimination_turn.is_some()
            })
        {
            matching_combat_plans
                .iter()
                .filter(|plan| plan.remaining_target_count > 0)
                .filter_map(|plan| plan.planned_elimination_turn)
                .max()
                .or(Some(turn))
        } else {
            None
        }
    }

    /// 目標・制圧予測・実績・所属を一つの採用assignmentへ反映する。
    fn observe_assignment(
        &self,
        world: &mut World,
        registry: &mut VictoryRoadmapRegistry,
        roadmap_id: super::VictoryRoadmapId,
        assignment: &super::IslandCampaignAssignment,
    ) {
        let player_id = self.player_id;
        let manager = self.manager;
        let turn = self.facts.turn;
        let island_map = &self.facts.island_map;
        let property_snapshots = &self.facts.property_snapshots;
        let enemy_capital = self.facts.enemy_capital;
        let owned_properties = &self.facts.owned_properties;
        let purpose = purpose_for(assignment, enemy_capital);
        let history = self.assignment_history(world, registry, assignment, purpose);
        let objectives = self.assignment_objectives(assignment, purpose);
        let phase = operation_phase(world, island_map, assignment, manager, player_id);
        let matching_combat_plans = self.matching_combat_plans(assignment, purpose);
        let local_enemy_count = self
            .facts
            .enemy_positions
            .iter()
            .filter(|position| {
                self.facts
                    .island_map
                    .get_island_at(position)
                    .is_some_and(|island| island.id == assignment.island_id)
            })
            .count();
        let suppression_turn = self.suppression_turn(local_enemy_count, &matching_combat_plans);
        let completion_forecast = if local_enemy_count > 0 && suppression_turn.is_none() {
            Err(format!(
                "{} local enemies remain but no executable suppression forecast exists",
                local_enemy_count
            ))
        } else {
            estimate_full_objective_completion(
                world,
                player_id,
                turn,
                assignment.island_id,
                assignment,
                &objectives,
                property_snapshots,
                island_map,
                manager,
                suppression_turn,
            )
        };
        let operation_id = registry.reconcile_assignment(
            roadmap_id,
            player_id,
            turn,
            assignment,
            purpose,
            objectives,
            owned_properties,
            phase,
        );
        let persistent_combat_entities = self.update_combat_forecast(
            registry,
            operation_id,
            &matching_combat_plans,
            suppression_turn,
            completion_forecast,
        );
        self.observe_assignment_issues(
            world,
            registry,
            operation_id,
            assignment,
            &history,
            &persistent_combat_entities,
        );
        registry.bind_assignment_entities(operation_id, assignment);
        self.bind_island_squads(world, registry, operation_id, assignment.island_id);
    }

    /// Combat計画IDと完了見通しを同時に更新する。
    fn update_combat_forecast(
        &self,
        registry: &mut VictoryRoadmapRegistry,
        operation_id: StrategicOperationId,
        matching_combat_plans: &[&ActiveCombatPlanSummary],
        suppression_turn: Option<u32>,
        completion_forecast: Result<u32, String>,
    ) -> HashSet<Entity> {
        let deployment_records = &self.facts.deployment_records;
        let matching_plan_ids = matching_combat_plans
            .iter()
            .map(|plan| plan.plan_id)
            .collect::<HashSet<_>>();
        let persistent_combat_entities = deployment_records
            .iter()
            .filter(|record| record.active)
            .filter(|record| {
                record
                    .plan_step
                    .is_some_and(|step| matching_plan_ids.contains(&step.plan_id))
            })
            .map(|record| record.entity)
            .collect::<HashSet<_>>();
        if let Some(operation) = registry.operations.get_mut(&operation_id) {
            operation.combat_plan_ids = matching_plan_ids;
            operation.planned_suppression_turn = suppression_turn;
            match completion_forecast {
                Ok(completion_turn) => {
                    operation.planned_completion_turn = Some(completion_turn);
                    operation.blocked_reason = None;
                }
                Err(reason) => {
                    operation.planned_completion_turn = None;
                    operation.blocked_reason = Some(reason);
                }
            }
        }
        persistent_combat_entities
    }

    /// 前回との照合結果を原因と回復履歴へ戻す。
    #[allow(clippy::too_many_arguments)]
    fn observe_assignment_issues(
        &self,
        world: &World,
        registry: &mut VictoryRoadmapRegistry,
        operation_id: StrategicOperationId,
        assignment: &super::IslandCampaignAssignment,
        history: &AssignmentHistory,
        persistent_combat_entities: &HashSet<Entity>,
    ) {
        let player_id = self.player_id;
        let manager = self.manager;
        let turn = self.facts.turn;
        let issues = diagnose_operation_execution(
            world,
            manager,
            player_id,
            turn,
            assignment,
            &history.entities,
            persistent_combat_entities,
            &history.production,
        );
        let recoveries = diagnose_operation_recoveries(
            world,
            manager,
            player_id,
            turn,
            assignment,
            &history.issues,
            persistent_combat_entities,
            &history.production,
        );
        registry.replace_operation_issues(operation_id, issues, recoveries);
    }

    /// 島に属する実働Squadを走査順のまま束縛する。
    fn bind_island_squads(
        &self,
        world: &World,
        registry: &mut VictoryRoadmapRegistry,
        operation_id: StrategicOperationId,
        island_id: IslandId,
    ) {
        let player_id = self.player_id;
        let manager = self.manager;
        for squad in manager.squads.iter().filter(|squad| {
            squad.plan.owner_id == Some(player_id) && squad.plan.target_island == Some(island_id)
        }) {
            bind_squad_entities(world, registry, operation_id, squad);
        }
    }

    /// 常在首都作戦の形成を観測し、前段完了後だけSquadを引き継ぐ。
    fn observe_capital(
        &self,
        world: &World,
        registry: &mut VictoryRoadmapRegistry,
        capital_has_active_regional_segment: bool,
    ) {
        let player_id = self.player_id;
        let turn = self.facts.turn;
        let island_map = &self.facts.island_map;
        let enemy_capital_island = self.facts.enemy_capital_island;
        let combat_plan_summaries = &self.facts.combat_plan_summaries;
        if let Some(capital_island) = enemy_capital_island
            && let Some(operation_id) = registry
                .operation_keys
                .get(&(player_id, capital_island, StrategicOperationScope::Capital))
                .copied()
        {
            let capital_plans = combat_plan_summaries
                .iter()
                .filter(|plan| {
                    plan.kind == OperationKind::AssaultCapital
                        && (island_map
                            .get_island_at(&plan.anchor)
                            .is_some_and(|island| island.id == capital_island)
                            || plan.objective_properties.iter().any(|position| {
                                island_map
                                    .get_island_at(position)
                                    .is_some_and(|island| island.id == capital_island)
                            }))
                })
                .collect::<Vec<_>>();
            if let Some(operation) = registry.operations.get_mut(&operation_id) {
                operation.combat_plan_ids = capital_plans.iter().map(|plan| plan.plan_id).collect();
                operation.planned_suppression_turn = if capital_plans.is_empty() {
                    None
                } else if capital_plans
                    .iter()
                    .all(|plan| plan.remaining_target_count == 0)
                {
                    Some(turn)
                } else {
                    capital_plans
                        .iter()
                        .filter(|plan| plan.remaining_target_count > 0)
                        .filter_map(|plan| plan.planned_elimination_turn)
                        .max()
                };
                if !capital_plans.is_empty() {
                    operation.blocked_reason =
                        operation.planned_suppression_turn.is_none().then(|| {
                            "capital combat plan is forming but has no executable suppression turn"
                                .to_owned()
                        });
                }
            }
            if !capital_has_active_regional_segment {
                // 中間区間が閉じた後は、親作戦がSquadから実Entityを引き継ぐ。これにより
                // Capitalへ進んだ時点でCampaign→Reserve→Campaignへ分断しない。
                self.bind_island_squads(world, registry, operation_id, capital_island);
            }
        }
    }

    /// 生産済みCombat EntityをPlanId経由で作戦実績へ接続する。
    fn bind_combat_deployments(&self, registry: &mut VictoryRoadmapRegistry) {
        let player_id = self.player_id;
        let deployment_records = &self.facts.deployment_records;
        for record in deployment_records.iter().filter(|record| record.active) {
            let Some(plan_id) = record.plan_step.map(|step| step.plan_id) else {
                continue;
            };
            let Some(operation_id) = registry.operations.values().find_map(|operation| {
                (operation.player_id == player_id
                    && operation.active
                    && operation.combat_plan_ids.contains(&plan_id))
                .then_some(operation.id)
            }) else {
                continue;
            };
            registry.bind_entity_exclusive(
                operation_id,
                record.entity,
                OperationEntityRole::Combat,
            );
        }
    }

    /// 所有・依存・進行状態を確定して非活動作戦の所属を解く。
    fn finish_observation(&self, registry: &mut VictoryRoadmapRegistry) {
        let player_id = self.player_id;
        let turn = self.facts.turn;
        let enemy_capital = self.facts.enemy_capital;
        let enemy_capital_island = self.facts.enemy_capital_island;
        let enemy_unit_count = self.facts.enemy_unit_count;
        let logistics_plan = &self.facts.logistics_plan;
        let owned_properties = &self.facts.owned_properties;
        registry.refresh_objective_ownership(player_id, owned_properties);
        registry.rebuild_dependencies(player_id, enemy_capital_island, logistics_plan.as_ref());
        registry.refresh_node_states(player_id);

        // 完了・撤回済み作戦は履歴として残すが、Entity集合を第二の割当正本にしない。
        registry.release_inactive_assignments(player_id);

        if let Some(roadmap) = registry.roadmaps.get_mut(&player_id) {
            roadmap.planned_victory_turn = enemy_capital_island.and_then(|capital_island| {
                registry
                    .operations
                    .values()
                    .find(|operation| {
                        operation.player_id == player_id
                            && operation.island_id == capital_island
                            && operation.purpose == StrategicPurpose::AssaultCapital
                    })
                    .and_then(|operation| operation.planned_completion_turn)
            });
            if enemy_capital.is_none() || enemy_unit_count == 0 {
                roadmap.actual_victory_turn.get_or_insert(turn);
            }
        }
    }
}

/// 一Squadの輸送役とcargoを正本へ同じ順序で束縛する。
fn bind_squad_entities(
    world: &World,
    registry: &mut VictoryRoadmapRegistry,
    operation_id: StrategicOperationId,
    squad: &Squad,
) {
    if let Some(transport) = *squad.transport_entity {
        registry.bind_entity_exclusive(operation_id, transport, OperationEntityRole::Transport);
    }
    for cargo in squad
        .cargo_entities
        .iter()
        .chain(squad.delivered_cargo.iter())
    {
        let role = if world
            .get::<crate::components::UnitStats>(*cargo)
            .is_some_and(|stats| stats.can_capture)
        {
            OperationEntityRole::Capture
        } else {
            OperationEntityRole::Combat
        };
        registry.bind_entity_exclusive(operation_id, *cargo, role);
    }
}

/// 照合済み台帳と指令を公開する。portfolioは呼出側で採用済みのものを受け取る。
pub(super) fn publish_roadmap_observation(
    world: &mut World,
    player_id: PlayerId,
    portfolio: IslandCampaignPortfolio,
    manager: &SquadManager,
    mut registry: VictoryRoadmapRegistry,
) {
    let turn = world
        .get_resource::<MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let assigned_campaign_entities = registry
        .operations
        .values()
        .filter(|operation| operation.player_id == player_id && operation.active)
        .flat_map(|operation| {
            operation
                .assigned_transports
                .iter()
                .chain(operation.assigned_capturers.iter())
                .chain(operation.assigned_combat.iter())
                .copied()
        })
        .collect::<Vec<_>>();
    registry.refresh_node_states(player_id);
    let mut directives = registry
        .operations
        .values()
        .filter(|operation| {
            operation.player_id == player_id
                && operation.active
                && operation.node_state != RoadmapNodeState::Locked
        })
        .map(|operation| {
            let operation_entities = operation
                .assigned_transports
                .iter()
                .chain(operation.assigned_capturers.iter())
                .chain(operation.assigned_combat.iter())
                .copied()
                .collect::<HashSet<_>>();
            let mut squad_ids = manager
                .squads
                .iter()
                .filter(|squad| squad.plan.owner_id == Some(player_id))
                .filter(|squad| {
                    squad
                        .members
                        .iter()
                        .any(|entity| operation_entities.contains(entity))
                })
                .map(|squad| squad.id)
                .collect::<Vec<_>>();
            squad_ids.sort_unstable_by_key(|squad_id| squad_id.0);
            squad_ids.dedup();
            RoadmapOperationDirective {
                operation_id: operation.id,
                island_id: operation.island_id,
                purpose: operation.purpose,
                node_state: operation.node_state,
                target: operation.tactical_anchor,
                squad_ids,
            }
        })
        .collect::<Vec<_>>();
    directives.sort_unstable_by_key(|directive| directive.operation_id.0);
    let mut turn_plans = world
        .remove_resource::<RoadmapTurnPlanRegistry>()
        .unwrap_or_default();
    turn_plans.replace(RoadmapTurnPlan {
        player_id,
        turn,
        portfolio,
        directives,
    });
    world.insert_resource(turn_plans);
    world.insert_resource(registry);
    if let Some(mut execution) =
        world.get_resource_mut::<crate::ai::v4::campaign_execution::V4CampaignExecutionRegistry>()
    {
        for entity in assigned_campaign_entities {
            execution.mark_assigned(entity, turn);
        }
    }
}
