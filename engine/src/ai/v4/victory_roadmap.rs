use crate::ai::island_campaign::{
    IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignPortfolio,
};
use crate::ai::islands::{IslandId, IslandMap};
use crate::ai::squad::{MissionPhase, MissionType, SquadManager, TransportPhase};
use crate::ai::turn_distance::{TurnDistanceCache, calculate_turn_distance};
use crate::ai::v4::deployment::V4DeploymentRegistry;
use crate::ai::v4::plan_revision::{ActiveCombatPlanSummary, PlanId, V4RollingPlanRegistry};
use crate::components::{
    CargoCapacity, Faction, GridPosition, Health, PlayerId, Property, Transporting, UnitStats,
};
use crate::events::{
    PropertyCaptureProgressedEvent, PropertyCapturedEvent, UnitAttackedEvent, UnitDestroyedEvent,
    UnitLoadedEvent, UnitMovedEvent, UnitSuppliedEvent, UnitUnloadedEvent, UnitWaitedEvent,
};
use crate::resources::{Map, MatchState, Terrain, master_data::MasterDataRegistry};
use crate::systems::movement::OccupantInfo;
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};

/// 勝利条件までの親計画ID。局地PlanIdと分離し、勝利経路が変わるまで維持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VictoryRoadmapId(pub u64);

/// 島単位の不変な作戦ID。敵Entityや毎ターン変わるanchorをidentityに含めない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrategicOperationId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VictoryRoute {
    CapitalCapture,
    EnemyAnnihilation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StrategicPurpose {
    CaptureIsland,
    DefendIsland,
    AssaultCapital,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CampaignStepKind {
    Produce,
    Move,
    Load,
    Transit,
    Drop,
    Attack,
    Capture,
    Hold,
    Supply,
    Wait,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationPhase {
    Forming,
    Pickup,
    Transit,
    Drop,
    Suppress,
    Capture,
    Hold,
    Completed,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationEntityRole {
    Transport,
    Capture,
    Combat,
}

/// 未完工程の直接原因。単なる「未完」から、再計画で取るべき行動を分離する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationIssueKind {
    AwaitingProduction,
    ProductionDelayed,
    ProducedEntityUnassigned,
    AssignmentLost,
    TransportUnassigned,
    TransportDestroyed,
    CapturerDestroyed,
    CombatUnitDestroyed,
    CargoLostWithTransport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationIssue {
    pub kind: OperationIssueKind,
    pub detected_turn: u32,
    pub entity: Option<Entity>,
    pub related_entity: Option<Entity>,
    pub detail: String,
}

/// 検知した原因に対して実際に確認できた復旧行動。原因検知だけを再計画実績に数えない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationRecoveryKind {
    RetryProduction,
    RestoreAssignment,
    AssignTransport,
    RequestReplacement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecoveryAction {
    pub kind: OperationRecoveryKind,
    pub cause: OperationIssueKind,
    pub completed_turn: u32,
    pub entity: Option<Entity>,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepExecutionTotals {
    pub moves: u32,
    pub loads: u32,
    pub drops: u32,
    pub attacks: u32,
    pub captures: u32,
    pub completed_captures: u32,
    pub supplies: u32,
    pub waits: u32,
    pub deviations: u32,
}

#[derive(Debug, Clone)]
pub struct StrategicOperation {
    pub id: StrategicOperationId,
    pub roadmap_id: VictoryRoadmapId,
    pub player_id: PlayerId,
    pub island_id: IslandId,
    pub purpose: StrategicPurpose,
    pub created_turn: u32,
    pub last_observed_turn: u32,
    pub tactical_anchor: GridPosition,
    /// 作戦開始時に固定した全目的拠点。revisionで別島の集合へ差し替えない。
    pub objective_properties: Vec<GridPosition>,
    pub owned_objective_count: usize,
    pub phase: OperationPhase,
    pub planned_completion_turn: Option<u32>,
    pub actual_completion_turn: Option<u32>,
    pub assigned_transports: HashSet<Entity>,
    pub assigned_capturers: HashSet<Entity>,
    pub assigned_combat: HashSet<Entity>,
    pub combat_plan_ids: HashSet<PlanId>,
    pub planned_suppression_turn: Option<u32>,
    pub execution: StepExecutionTotals,
    pub last_step: Option<CampaignStepKind>,
    pub last_progress_turn: Option<u32>,
    pub blocked_reason: Option<String>,
    pub current_issues: Vec<OperationIssue>,
    pub issue_history: Vec<OperationIssue>,
    pub recovery_history: Vec<OperationRecoveryAction>,
    pub replan_count: u32,
    pub last_replan_turn: Option<u32>,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct VictoryRoadmap {
    pub id: VictoryRoadmapId,
    pub player_id: PlayerId,
    pub route: VictoryRoute,
    pub created_turn: u32,
    pub last_observed_turn: u32,
    pub enemy_capital: Option<GridPosition>,
    pub enemy_capital_island: Option<IslandId>,
    pub planned_victory_turn: Option<u32>,
    pub actual_victory_turn: Option<u32>,
    pub initial_enemy_unit_count: usize,
    pub current_enemy_unit_count: usize,
    pub operation_ids: Vec<StrategicOperationId>,
}

#[derive(Debug, Clone, Copy)]
struct EntityOperationBinding {
    operation_id: StrategicOperationId,
    role: OperationEntityRole,
}

/// 勝利条件、島作戦、実Entity、実行Eventを同じidentityで監査する永続Resource。
#[derive(Resource, Debug, Default)]
pub struct VictoryRoadmapRegistry {
    next_roadmap_id: u64,
    next_operation_id: u64,
    roadmaps: HashMap<PlayerId, VictoryRoadmap>,
    operations: HashMap<StrategicOperationId, StrategicOperation>,
    operation_keys: HashMap<(PlayerId, IslandId, StrategicPurpose), StrategicOperationId>,
    entity_bindings: HashMap<Entity, EntityOperationBinding>,
    transport_manifests: HashMap<Entity, HashSet<Entity>>,
}

impl VictoryRoadmapRegistry {
    pub fn roadmap(&self, player_id: PlayerId) -> Option<&VictoryRoadmap> {
        self.roadmaps.get(&player_id)
    }

    pub fn operations_for(&self, player_id: PlayerId) -> Vec<&StrategicOperation> {
        let mut operations = self
            .operations
            .values()
            .filter(|operation| operation.player_id == player_id)
            .collect::<Vec<_>>();
        operations.sort_unstable_by_key(|operation| operation.id.0);
        operations
    }

    fn ensure_roadmap(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        enemy_capital: Option<GridPosition>,
        enemy_capital_island: Option<IslandId>,
        enemy_unit_count: usize,
    ) -> VictoryRoadmapId {
        if let Some(roadmap) = self.roadmaps.get_mut(&player_id) {
            roadmap.last_observed_turn = turn;
            roadmap.enemy_capital = enemy_capital;
            roadmap.enemy_capital_island = enemy_capital_island;
            roadmap.current_enemy_unit_count = enemy_unit_count;
            return roadmap.id;
        }
        self.next_roadmap_id = self.next_roadmap_id.saturating_add(1);
        let id = VictoryRoadmapId(self.next_roadmap_id);
        self.roadmaps.insert(
            player_id,
            VictoryRoadmap {
                id,
                player_id,
                route: if enemy_capital.is_some() {
                    VictoryRoute::CapitalCapture
                } else {
                    VictoryRoute::EnemyAnnihilation
                },
                created_turn: turn,
                last_observed_turn: turn,
                enemy_capital,
                enemy_capital_island,
                planned_victory_turn: None,
                actual_victory_turn: None,
                initial_enemy_unit_count: enemy_unit_count,
                current_enemy_unit_count: enemy_unit_count,
                operation_ids: Vec::new(),
            },
        );
        id
    }

    fn ensure_capital_objective(
        &mut self,
        roadmap_id: VictoryRoadmapId,
        player_id: PlayerId,
        turn: u32,
        island_id: IslandId,
        capital: GridPosition,
        owned: bool,
    ) -> StrategicOperationId {
        let key = (player_id, island_id, StrategicPurpose::AssaultCapital);
        let operation_id = if let Some(id) = self.operation_keys.get(&key).copied() {
            id
        } else {
            self.next_operation_id = self.next_operation_id.saturating_add(1);
            let id = StrategicOperationId(self.next_operation_id);
            self.operation_keys.insert(key, id);
            self.operations.insert(
                id,
                StrategicOperation {
                    id,
                    roadmap_id,
                    player_id,
                    island_id,
                    purpose: StrategicPurpose::AssaultCapital,
                    created_turn: turn,
                    last_observed_turn: turn,
                    tactical_anchor: capital,
                    objective_properties: vec![capital],
                    owned_objective_count: usize::from(owned),
                    phase: OperationPhase::Forming,
                    planned_completion_turn: None,
                    actual_completion_turn: None,
                    assigned_transports: HashSet::new(),
                    assigned_capturers: HashSet::new(),
                    assigned_combat: HashSet::new(),
                    combat_plan_ids: HashSet::new(),
                    planned_suppression_turn: None,
                    execution: StepExecutionTotals::default(),
                    last_step: None,
                    last_progress_turn: None,
                    blocked_reason: Some(
                        "no executable capital assault schedule has been selected".to_owned(),
                    ),
                    current_issues: Vec::new(),
                    issue_history: Vec::new(),
                    recovery_history: Vec::new(),
                    replan_count: 0,
                    last_replan_turn: None,
                    active: true,
                },
            );
            if let Some(roadmap) = self.roadmaps.get_mut(&player_id) {
                roadmap.operation_ids.push(id);
            }
            id
        };
        let operation = self
            .operations
            .get_mut(&operation_id)
            .expect("作成済み首都作戦");
        operation.last_observed_turn = turn;
        operation.tactical_anchor = capital;
        operation.owned_objective_count = usize::from(owned);
        operation.active = true;
        operation.assigned_transports.clear();
        operation.assigned_capturers.clear();
        operation.assigned_combat.clear();
        operation.combat_plan_ids.clear();
        operation.planned_suppression_turn = None;
        if owned {
            operation.phase = OperationPhase::Completed;
            operation.actual_completion_turn.get_or_insert(turn);
            operation.blocked_reason = None;
        } else {
            operation.phase = OperationPhase::Forming;
            operation.planned_completion_turn = None;
            operation.blocked_reason =
                Some("no executable capital assault schedule has been selected".to_owned());
        }
        operation_id
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_assignment(
        &mut self,
        roadmap_id: VictoryRoadmapId,
        player_id: PlayerId,
        turn: u32,
        assignment: &IslandCampaignAssignment,
        purpose: StrategicPurpose,
        objectives: Vec<GridPosition>,
        owned_properties: &HashSet<GridPosition>,
        phase: OperationPhase,
    ) -> StrategicOperationId {
        let key = (player_id, assignment.island_id, purpose);
        let operation_id = if let Some(id) = self.operation_keys.get(&key).copied() {
            id
        } else {
            self.next_operation_id = self.next_operation_id.saturating_add(1);
            let id = StrategicOperationId(self.next_operation_id);
            self.operation_keys.insert(key, id);
            self.operations.insert(
                id,
                StrategicOperation {
                    id,
                    roadmap_id,
                    player_id,
                    island_id: assignment.island_id,
                    purpose,
                    created_turn: turn,
                    last_observed_turn: turn,
                    tactical_anchor: assignment.target_position,
                    objective_properties: objectives,
                    owned_objective_count: 0,
                    phase: OperationPhase::Forming,
                    planned_completion_turn: None,
                    actual_completion_turn: None,
                    assigned_transports: HashSet::new(),
                    assigned_capturers: HashSet::new(),
                    assigned_combat: HashSet::new(),
                    combat_plan_ids: HashSet::new(),
                    planned_suppression_turn: None,
                    execution: StepExecutionTotals::default(),
                    last_step: None,
                    last_progress_turn: None,
                    blocked_reason: None,
                    current_issues: Vec::new(),
                    issue_history: Vec::new(),
                    recovery_history: Vec::new(),
                    replan_count: 0,
                    last_replan_turn: None,
                    active: true,
                },
            );
            if let Some(roadmap) = self.roadmaps.get_mut(&player_id) {
                roadmap.operation_ids.push(id);
            }
            id
        };

        let operation = self
            .operations
            .get_mut(&operation_id)
            .expect("作成済みStrategicOperation");
        operation.last_observed_turn = turn;
        operation.tactical_anchor = assignment.target_position;
        operation.owned_objective_count = operation
            .objective_properties
            .iter()
            .filter(|position| owned_properties.contains(position))
            .count();
        operation.assigned_transports = assignment.transport_entities.iter().copied().collect();
        operation.assigned_capturers = assignment.capture_entities.iter().copied().collect();
        operation.assigned_combat = assignment.combat_entities.iter().copied().collect();
        operation.active = true;
        operation.blocked_reason = None;
        operation.phase = if !operation.objective_properties.is_empty()
            && operation.owned_objective_count == operation.objective_properties.len()
        {
            operation.actual_completion_turn.get_or_insert(turn);
            OperationPhase::Completed
        } else {
            phase
        };
        operation_id
    }

    fn bind_assignment_entities(
        &mut self,
        operation_id: StrategicOperationId,
        assignment: &IslandCampaignAssignment,
    ) {
        for entity in &assignment.transport_entities {
            self.bind_entity_exclusive(operation_id, *entity, OperationEntityRole::Transport);
        }
        for entity in &assignment.capture_entities {
            self.bind_entity_exclusive(operation_id, *entity, OperationEntityRole::Capture);
        }
        for entity in &assignment.combat_entities {
            self.bind_entity_exclusive(operation_id, *entity, OperationEntityRole::Combat);
        }
    }

    /// Roadmap監査側もEntityを複数作戦へ残さない。旧作戦の逆集合から同時に除去する。
    fn bind_entity_exclusive(
        &mut self,
        operation_id: StrategicOperationId,
        entity: Entity,
        role: OperationEntityRole,
    ) {
        if let Some(previous) = self.entity_bindings.get(&entity).copied()
            && previous.operation_id != operation_id
            && let Some(operation) = self.operations.get_mut(&previous.operation_id)
        {
            operation.assigned_transports.remove(&entity);
            operation.assigned_capturers.remove(&entity);
            operation.assigned_combat.remove(&entity);
        }
        if let Some(operation) = self.operations.get_mut(&operation_id) {
            operation.assigned_transports.remove(&entity);
            operation.assigned_capturers.remove(&entity);
            operation.assigned_combat.remove(&entity);
            match role {
                OperationEntityRole::Transport => {
                    operation.assigned_transports.insert(entity);
                }
                OperationEntityRole::Capture => {
                    operation.assigned_capturers.insert(entity);
                }
                OperationEntityRole::Combat => {
                    operation.assigned_combat.insert(entity);
                }
            }
        }
        self.entity_bindings
            .insert(entity, EntityOperationBinding { operation_id, role });
    }

    /// inactive作戦は実績履歴だけを残し、現在のEntity割当を所有し続けない。
    fn release_inactive_assignments(&mut self, player_id: PlayerId) {
        for operation in self
            .operations
            .values_mut()
            .filter(|operation| operation.player_id == player_id && !operation.active)
        {
            operation.assigned_transports.clear();
            operation.assigned_capturers.clear();
            operation.assigned_combat.clear();
        }
        self.entity_bindings.retain(|_, binding| {
            self.operations
                .get(&binding.operation_id)
                .is_some_and(|operation| operation.active || operation.player_id != player_id)
        });
    }

    fn replace_operation_issues(
        &mut self,
        operation_id: StrategicOperationId,
        issues: Vec<OperationIssue>,
        recoveries: Vec<OperationRecoveryAction>,
    ) {
        let Some(operation) = self.operations.get_mut(&operation_id) else {
            return;
        };
        for issue in &issues {
            let already_recorded = operation.issue_history.iter().any(|recorded| {
                recorded.kind == issue.kind
                    && recorded.detected_turn == issue.detected_turn
                    && recorded.entity == issue.entity
                    && recorded.related_entity == issue.related_entity
            });
            if !already_recorded {
                operation.issue_history.push(issue.clone());
            }
        }
        for recovery in recoveries {
            let already_recorded = operation.recovery_history.iter().any(|recorded| {
                recorded.kind == recovery.kind
                    && recorded.cause == recovery.cause
                    && recorded.completed_turn == recovery.completed_turn
                    && recorded.entity == recovery.entity
            });
            if !already_recorded {
                operation.replan_count = operation.replan_count.saturating_add(1);
                operation.last_replan_turn = Some(recovery.completed_turn);
                operation.recovery_history.push(recovery);
            }
        }
        operation.current_issues = issues;
    }

    fn record_destroyed_entity(&mut self, entity: Entity, turn: u32) {
        let Some(binding) = self.entity_bindings.get(&entity).copied() else {
            return;
        };
        let kind = match binding.role {
            OperationEntityRole::Transport => OperationIssueKind::TransportDestroyed,
            OperationEntityRole::Capture => OperationIssueKind::CapturerDestroyed,
            OperationEntityRole::Combat => OperationIssueKind::CombatUnitDestroyed,
        };
        let issue = OperationIssue {
            kind,
            detected_turn: turn,
            entity: Some(entity),
            related_entity: None,
            detail: format!(
                "assigned {:?} Entity {} was destroyed",
                binding.role,
                entity.to_bits()
            ),
        };
        if let Some(operation) = self.operations.get_mut(&binding.operation_id) {
            operation.current_issues.push(issue.clone());
            operation.issue_history.push(issue);
            operation.phase = OperationPhase::Blocked;
        }

        if binding.role == OperationEntityRole::Transport {
            let cargo = self.transport_manifests.remove(&entity).unwrap_or_default();
            for cargo_entity in cargo {
                let cargo_issue = OperationIssue {
                    kind: OperationIssueKind::CargoLostWithTransport,
                    detected_turn: turn,
                    entity: Some(cargo_entity),
                    related_entity: Some(entity),
                    detail: format!(
                        "cargo Entity {} was lost with transport Entity {}",
                        cargo_entity.to_bits(),
                        entity.to_bits()
                    ),
                };
                if let Some(operation) = self.operations.get_mut(&binding.operation_id) {
                    operation.current_issues.push(cargo_issue.clone());
                    operation.issue_history.push(cargo_issue);
                }
            }
        }
    }

    fn record_entity_step(&mut self, entity: Entity, turn: u32, step: CampaignStepKind) {
        let Some(binding) = self.entity_bindings.get(&entity).copied() else {
            return;
        };
        let Some(operation) = self.operations.get_mut(&binding.operation_id) else {
            return;
        };
        operation.last_step = Some(step);
        operation.last_progress_turn = Some(turn);
        match step {
            CampaignStepKind::Move | CampaignStepKind::Transit => {
                operation.execution.moves = operation.execution.moves.saturating_add(1);
            }
            CampaignStepKind::Load => {
                operation.execution.loads = operation.execution.loads.saturating_add(1);
            }
            CampaignStepKind::Drop => {
                operation.execution.drops = operation.execution.drops.saturating_add(1);
            }
            CampaignStepKind::Attack => {
                operation.execution.attacks = operation.execution.attacks.saturating_add(1);
            }
            CampaignStepKind::Capture => {
                operation.execution.captures = operation.execution.captures.saturating_add(1);
            }
            CampaignStepKind::Supply => {
                operation.execution.supplies = operation.execution.supplies.saturating_add(1);
            }
            CampaignStepKind::Wait => {
                operation.execution.waits = operation.execution.waits.saturating_add(1);
            }
            CampaignStepKind::Produce | CampaignStepKind::Hold => {}
        }
    }

    fn record_move(
        &mut self,
        entity: Entity,
        from: GridPosition,
        to: GridPosition,
        turn: u32,
        island_map: &IslandMap,
    ) {
        let Some(binding) = self.entity_bindings.get(&entity).copied() else {
            return;
        };
        let source_island = island_map.get_island_at(&from).map(|island| island.id);
        let destination_island = island_map.get_island_at(&to).map(|island| island.id);
        if let Some(operation) = self.operations.get_mut(&binding.operation_id)
            && binding.role != OperationEntityRole::Transport
            // 出発島で搭載地点へ寄る移動は逸脱ではない。実際に目的島上にいたEntityが
            // 目的島外へ出た場合だけを逸脱とし、作戦全体のphaseで推測しない。
            && source_island == Some(operation.island_id)
            && destination_island.is_some_and(|island| island != operation.island_id)
        {
            operation.execution.deviations = operation.execution.deviations.saturating_add(1);
            operation.phase = OperationPhase::Blocked;
            operation.blocked_reason = Some(format!(
                "Entity {} moved to island {} outside operation island {}",
                entity.to_bits(),
                destination_island.map_or(usize::MAX, |island| island.0),
                operation.island_id.0
            ));
        }
        self.record_entity_step(entity, turn, CampaignStepKind::Move);
    }
}

fn purpose_for(
    assignment: &IslandCampaignAssignment,
    enemy_capital_island: Option<IslandId>,
) -> StrategicPurpose {
    if enemy_capital_island == Some(assignment.island_id) {
        StrategicPurpose::AssaultCapital
    } else if assignment.decision == IslandCampaignDecision::Defend {
        StrategicPurpose::DefendIsland
    } else {
        StrategicPurpose::CaptureIsland
    }
}

fn operation_entity_is_alive(world: &World, entity: Entity) -> bool {
    world.get_entity(entity).is_ok()
}

fn entity_is_owned_by_island_squad(
    manager: &SquadManager,
    player_id: PlayerId,
    island_id: IslandId,
    entity: Entity,
) -> bool {
    manager.squads.iter().any(|squad| {
        squad.owner_id == Some(player_id)
            && squad.target_island == Some(island_id)
            && (squad.members.contains(&entity)
                || squad.cargo_entities.contains(&entity)
                || squad.delivered_cargo.contains(&entity))
    })
}

fn diagnose_operation_execution(
    world: &World,
    manager: &SquadManager,
    player_id: PlayerId,
    turn: u32,
    assignment: &IslandCampaignAssignment,
    previous_entities: &[(Entity, OperationEntityRole)],
    production_records: &[crate::ai::v4::campaign_execution::CampaignProductionRecord],
) -> Vec<OperationIssue> {
    use crate::ai::v4::campaign_execution::{CampaignProductionRole, CampaignProductionStatus};

    let current_entities = assignment
        .transport_entities
        .iter()
        .chain(assignment.capture_entities.iter())
        .chain(assignment.combat_entities.iter())
        .copied()
        .collect::<HashSet<_>>();
    let mut issues = Vec::new();

    for (entity, role) in previous_entities {
        if !operation_entity_is_alive(world, *entity) {
            let kind = match role {
                OperationEntityRole::Transport => OperationIssueKind::TransportDestroyed,
                OperationEntityRole::Capture => OperationIssueKind::CapturerDestroyed,
                OperationEntityRole::Combat => OperationIssueKind::CombatUnitDestroyed,
            };
            issues.push(OperationIssue {
                kind,
                detected_turn: turn,
                entity: Some(*entity),
                related_entity: None,
                detail: format!(
                    "previously assigned {:?} Entity {} no longer exists",
                    role,
                    entity.to_bits()
                ),
            });
        } else if !current_entities.contains(entity)
            && !entity_is_owned_by_island_squad(manager, player_id, assignment.island_id, *entity)
        {
            issues.push(OperationIssue {
                kind: OperationIssueKind::AssignmentLost,
                detected_turn: turn,
                entity: Some(*entity),
                related_entity: None,
                detail: format!(
                    "live {:?} Entity {} disappeared from the operation assignment",
                    role,
                    entity.to_bits()
                ),
            });
        }
    }

    let island_map = world.get_resource::<IslandMap>();
    for entity in &assignment.capture_entities {
        if !operation_entity_is_alive(world, *entity) {
            continue;
        }
        let landed = world.get::<GridPosition>(*entity).is_some_and(|position| {
            island_map
                .and_then(|map| map.get_island_at(position))
                .is_some_and(|island| island.id == assignment.island_id)
        });
        if landed || world.get::<Transporting>(*entity).is_some() {
            continue;
        }
        let assigned_transport = manager.squads.iter().any(|squad| {
            squad.owner_id == Some(player_id)
                && squad.target_island == Some(assignment.island_id)
                && squad.cargo_entities.contains(entity)
                && squad.transport_entity.is_some()
        });
        if !assigned_transport {
            issues.push(OperationIssue {
                kind: OperationIssueKind::TransportUnassigned,
                detected_turn: turn,
                entity: Some(*entity),
                related_entity: None,
                detail: format!(
                    "capture Entity {} is alive but has no transport for island {}",
                    entity.to_bits(),
                    assignment.island_id.0
                ),
            });
        }
    }

    // 同じ役割の再発注がある場合、過去の遅延ではなく最新の発注状態を現在原因にする。
    for role in [
        CampaignProductionRole::Transport,
        CampaignProductionRole::Capture,
        CampaignProductionRole::Combat,
    ] {
        let Some(record) = production_records
            .iter()
            .filter(|record| record.role == role)
            .max_by_key(|record| record.planned_turn)
        else {
            continue;
        };
        let issue = match record.status {
            CampaignProductionStatus::Planned | CampaignProductionStatus::Issued => {
                Some((OperationIssueKind::AwaitingProduction, None))
            }
            CampaignProductionStatus::Delayed => {
                Some((OperationIssueKind::ProductionDelayed, None))
            }
            CampaignProductionStatus::Produced => {
                let entity = record.entity;
                entity
                    .filter(|entity| {
                        !current_entities.contains(entity)
                            && !entity_is_owned_by_island_squad(
                                manager,
                                player_id,
                                assignment.island_id,
                                *entity,
                            )
                    })
                    .map(|entity| (OperationIssueKind::ProducedEntityUnassigned, Some(entity)))
            }
            CampaignProductionStatus::Lost | CampaignProductionStatus::Assigned => None,
        };
        if let Some((kind, entity)) = issue {
            issues.push(OperationIssue {
                kind,
                detected_turn: turn,
                entity,
                related_entity: None,
                detail: format!(
                    "campaign {:?} production at ({},{}) is {:?}",
                    role, record.facility_x, record.facility_y, record.status
                ),
            });
        }
    }

    issues.sort_by_key(|issue| {
        (
            format!("{:?}", issue.kind),
            issue.entity.map_or(u64::MAX, Entity::to_bits),
        )
    });
    issues.dedup_by(|left, right| {
        left.kind == right.kind
            && left.entity == right.entity
            && left.related_entity == right.related_entity
    });
    issues
}

fn diagnose_operation_recoveries(
    world: &World,
    manager: &SquadManager,
    player_id: PlayerId,
    turn: u32,
    assignment: &IslandCampaignAssignment,
    previous_issues: &[OperationIssue],
    production_records: &[crate::ai::v4::campaign_execution::CampaignProductionRecord],
) -> Vec<OperationRecoveryAction> {
    use crate::ai::v4::campaign_execution::{CampaignProductionRole, CampaignProductionStatus};

    let assigned_entities = assignment
        .transport_entities
        .iter()
        .chain(assignment.capture_entities.iter())
        .chain(assignment.combat_entities.iter())
        .copied()
        .collect::<HashSet<_>>();
    let mut recoveries = Vec::new();
    for issue in previous_issues {
        let recovery = match issue.kind {
            OperationIssueKind::AwaitingProduction => None,
            OperationIssueKind::ProductionDelayed => production_records
                .iter()
                .filter(|record| record.planned_turn >= issue.detected_turn)
                .filter(|record| {
                    matches!(
                        record.status,
                        CampaignProductionStatus::Planned
                            | CampaignProductionStatus::Issued
                            | CampaignProductionStatus::Produced
                            | CampaignProductionStatus::Assigned
                    )
                })
                .max_by_key(|record| record.planned_turn)
                .map(|record| OperationRecoveryAction {
                    kind: OperationRecoveryKind::RetryProduction,
                    cause: issue.kind,
                    completed_turn: turn,
                    entity: record.entity,
                    detail: format!(
                        "retried {:?} production for island {} on turn {}",
                        record.role, assignment.island_id.0, record.planned_turn
                    ),
                }),
            OperationIssueKind::ProducedEntityUnassigned | OperationIssueKind::AssignmentLost => {
                issue.entity.and_then(|entity| {
                    (assigned_entities.contains(&entity)
                        || entity_is_owned_by_island_squad(
                            manager,
                            player_id,
                            assignment.island_id,
                            entity,
                        ))
                    .then(|| OperationRecoveryAction {
                        kind: OperationRecoveryKind::RestoreAssignment,
                        cause: issue.kind,
                        completed_turn: turn,
                        entity: Some(entity),
                        detail: format!(
                            "restored Entity {} to island {} operation",
                            entity.to_bits(),
                            assignment.island_id.0
                        ),
                    })
                })
            }
            OperationIssueKind::TransportUnassigned => issue.entity.and_then(|entity| {
                let loaded = world.get::<Transporting>(entity).is_some();
                let assigned_transport = manager.squads.iter().any(|squad| {
                    squad.owner_id == Some(player_id)
                        && squad.target_island == Some(assignment.island_id)
                        && squad.cargo_entities.contains(&entity)
                        && squad.transport_entity.is_some()
                });
                (loaded || assigned_transport).then(|| OperationRecoveryAction {
                    kind: OperationRecoveryKind::AssignTransport,
                    cause: issue.kind,
                    completed_turn: turn,
                    entity: Some(entity),
                    detail: format!(
                        "assigned transport to capture Entity {} for island {}",
                        entity.to_bits(),
                        assignment.island_id.0
                    ),
                })
            }),
            OperationIssueKind::TransportDestroyed
            | OperationIssueKind::CapturerDestroyed
            | OperationIssueKind::CombatUnitDestroyed
            | OperationIssueKind::CargoLostWithTransport => {
                let role = match issue.kind {
                    OperationIssueKind::TransportDestroyed => CampaignProductionRole::Transport,
                    OperationIssueKind::CombatUnitDestroyed => CampaignProductionRole::Combat,
                    OperationIssueKind::CapturerDestroyed
                    | OperationIssueKind::CargoLostWithTransport => CampaignProductionRole::Capture,
                    _ => unreachable!(),
                };
                production_records
                    .iter()
                    .filter(|record| record.role == role)
                    .filter(|record| record.planned_turn >= issue.detected_turn)
                    .filter(|record| {
                        !matches!(
                            record.status,
                            CampaignProductionStatus::Delayed | CampaignProductionStatus::Lost
                        )
                    })
                    .max_by_key(|record| record.planned_turn)
                    .map(|record| OperationRecoveryAction {
                        kind: OperationRecoveryKind::RequestReplacement,
                        cause: issue.kind,
                        completed_turn: turn,
                        entity: record.entity,
                        detail: format!(
                            "requested replacement {:?} for island {} on turn {}",
                            role, assignment.island_id.0, record.planned_turn
                        ),
                    })
            }
        };
        if let Some(recovery) = recovery {
            recoveries.push(recovery);
        }
    }
    recoveries.sort_by_key(|recovery| {
        (
            format!("{:?}", recovery.kind),
            format!("{:?}", recovery.cause),
            recovery.entity.map_or(u64::MAX, Entity::to_bits),
        )
    });
    recoveries.dedup_by(|left, right| {
        left.kind == right.kind && left.cause == right.cause && left.entity == right.entity
    });
    recoveries
}

fn operation_phase(
    world: &World,
    island_map: &IslandMap,
    assignment: &IslandCampaignAssignment,
    manager: &SquadManager,
    player_id: PlayerId,
) -> OperationPhase {
    let transport_phase = manager
        .squads
        .iter()
        .filter(|squad| {
            squad.owner_id == Some(player_id)
                && squad.mission_type == MissionType::Transport
                && squad.target_island == Some(assignment.island_id)
        })
        .filter_map(|squad| match squad.phase {
            MissionPhase::Transport(phase) => Some(phase),
            _ => None,
        })
        .next();
    let entity_is_deployed = |entity: &Entity| {
        world.get::<Transporting>(*entity).is_none()
            && world.get::<GridPosition>(*entity).is_some_and(|position| {
                island_map
                    .get_island_at(position)
                    .is_some_and(|island| island.id == assignment.island_id)
            })
    };
    let combat_is_deployed = assignment.combat_entities.iter().any(&entity_is_deployed);
    let capturer_is_deployed = assignment.capture_entities.iter().any(entity_is_deployed);
    match transport_phase {
        Some(TransportPhase::Pickup) => OperationPhase::Pickup,
        Some(TransportPhase::Transit) | Some(TransportPhase::Return) => OperationPhase::Transit,
        Some(TransportPhase::Drop) => OperationPhase::Drop,
        None if combat_is_deployed => OperationPhase::Suppress,
        None if capturer_is_deployed => OperationPhase::Capture,
        None => OperationPhase::Forming,
    }
}

#[derive(Debug, Clone)]
struct CapturerForecast {
    position: GridPosition,
    stats: UnitStats,
    health: Health,
    /// 現在手番から数え、島内で行動可能になるまでの手番数。
    available_turn: u32,
}

struct CaptureRouteContext<'a> {
    map: &'a Map,
    master_data: &'a MasterDataRegistry,
    occupied: &'a HashMap<(usize, usize), OccupantInfo>,
    player_id: PlayerId,
    cache: TurnDistanceCache,
}

impl CaptureRouteContext<'_> {
    fn distance(
        &mut self,
        start: GridPosition,
        target: GridPosition,
        stats: &UnitStats,
    ) -> Option<u32> {
        let distance = calculate_turn_distance(
            self.map,
            self.master_data,
            self.occupied,
            (start.x, start.y),
            (target.x, target.y),
            stats.movement_type,
            stats.max_movement.max(1),
            0,
            self.player_id,
            &mut self.cache,
        );
        (distance.turns != u32::MAX).then_some(distance.turns)
    }
}

fn capture_power(health: Health) -> u32 {
    health
        .current
        .saturating_add(9)
        .div_ceil(10)
        .saturating_mul(10)
}

/// 担当占領兵を全未所有拠点へ分担し、最後の拠点を取り終える絶対手番を返す。
/// 移動・搭載・降車のどれかを実Entityから説明できない場合は楽観値を返さない。
#[allow(clippy::too_many_arguments)]
fn estimate_full_objective_completion(
    world: &mut World,
    player_id: PlayerId,
    turn: u32,
    island_id: IslandId,
    assignment: &IslandCampaignAssignment,
    objectives: &[GridPosition],
    property_snapshots: &[(GridPosition, Property)],
    island_map: &IslandMap,
    manager: &SquadManager,
    suppression_turn: Option<u32>,
) -> Result<u32, String> {
    let mut remaining = property_snapshots
        .iter()
        .filter(|(position, property)| {
            objectives.contains(position) && property.owner_id != Some(player_id)
        })
        .copied()
        .collect::<Vec<_>>();
    if remaining.is_empty() {
        return Ok(turn);
    }

    let mut occupied = HashMap::new();
    let mut unit_query = world.query::<(
        Entity,
        &GridPosition,
        &Faction,
        &UnitStats,
        Option<&CargoCapacity>,
        Option<&Transporting>,
    )>();
    for (_, position, faction, stats, capacity, transporting) in unit_query.iter(world) {
        if transporting.is_some() {
            continue;
        }
        occupied.insert(
            (position.x, position.y),
            OccupantInfo {
                player_id: faction.0,
                is_transport: stats.max_cargo > 0,
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots: capacity
                    .map(|capacity| capacity.max.saturating_sub(capacity.loaded.len() as u32))
                    .unwrap_or(0),
            },
        );
    }

    let map = world
        .get_resource::<Map>()
        .ok_or_else(|| "map resource missing".to_owned())?;
    let master_data = world
        .get_resource::<MasterDataRegistry>()
        .ok_or_else(|| "master data missing".to_owned())?;
    let mut route = CaptureRouteContext {
        map,
        master_data,
        occupied: &occupied,
        player_id,
        cache: TurnDistanceCache::default(),
    };
    let suppression_delay = suppression_turn.unwrap_or(turn).saturating_sub(turn);

    let mut capturer_entities = assignment
        .capture_entities
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    for squad in manager
        .squads
        .iter()
        .filter(|squad| squad.owner_id == Some(player_id) && squad.target_island == Some(island_id))
    {
        for entity in squad
            .cargo_entities
            .iter()
            .chain(squad.delivered_cargo.iter())
        {
            if world
                .get::<UnitStats>(*entity)
                .is_some_and(|stats| stats.can_capture)
            {
                capturer_entities.insert(*entity);
            }
        }
    }

    let mut capturers = Vec::new();
    for entity in capturer_entities {
        let Some(stats) = world.get::<UnitStats>(entity).cloned() else {
            continue;
        };
        let Some(health) = world.get::<Health>(entity).copied() else {
            continue;
        };
        if !stats.can_capture || capture_power(health) == 0 {
            continue;
        }
        let transporting = world.get::<Transporting>(entity).copied();
        let current_position = world.get::<GridPosition>(entity).copied();
        if transporting.is_none()
            && current_position.is_some_and(|position| {
                island_map
                    .get_island_at(&position)
                    .is_some_and(|island| island.id == island_id)
            })
        {
            capturers.push(CapturerForecast {
                position: current_position.expect("is_someで確認済み"),
                stats,
                health,
                available_turn: suppression_delay,
            });
            continue;
        }

        let squad = manager.squads.iter().find(|squad| {
            squad.owner_id == Some(player_id)
                && squad.target_island == Some(island_id)
                && (squad.cargo_entities.contains(&entity)
                    || squad.delivered_cargo.contains(&entity))
        });
        let transport = transporting
            .map(|transporting| transporting.0)
            .or_else(|| squad.and_then(|squad| squad.transport_entity))
            .ok_or_else(|| format!("capture Entity {} has no transport", entity.to_bits()))?;
        let transport_position = world
            .get::<GridPosition>(transport)
            .copied()
            .ok_or_else(|| format!("transport Entity {} has no position", transport.to_bits()))?;
        let transport_stats = world
            .get::<UnitStats>(transport)
            .ok_or_else(|| format!("transport Entity {} has no stats", transport.to_bits()))?;
        let pickup = squad
            .and_then(|squad| squad.pickup_position)
            .unwrap_or(transport_position);
        let mut ready = 0_u32;
        let departure = if transporting.is_some() {
            transport_position
        } else {
            let cargo_position = current_position.ok_or_else(|| {
                format!("capture Entity {} has no pickup position", entity.to_bits())
            })?;
            let cargo_eta = route
                .distance(cargo_position, pickup, &stats)
                .ok_or_else(|| {
                    format!("capture Entity {} cannot reach pickup", entity.to_bits())
                })?;
            let transport_eta = route
                .distance(transport_position, pickup, transport_stats)
                .ok_or_else(|| {
                    format!(
                        "transport Entity {} cannot reach pickup",
                        transport.to_bits()
                    )
                })?;
            ready = cargo_eta.max(transport_eta).saturating_add(1);
            pickup
        };
        let transit = route
            .distance(departure, assignment.target_position, transport_stats)
            .ok_or_else(|| format!("transport Entity {} cannot reach drop", transport.to_bits()))?;
        ready = ready.saturating_add(transit).saturating_add(1);
        capturers.push(CapturerForecast {
            position: assignment.target_position,
            stats,
            health,
            available_turn: ready.max(suppression_delay),
        });
    }
    if capturers.is_empty() {
        return Err("no assigned capture Entity can execute the operation".to_owned());
    }

    remaining.sort_unstable_by_key(|(position, _)| (position.y, position.x));
    while !remaining.is_empty() {
        let mut best: Option<(u32, usize, usize, u32)> = None;
        for (worker_index, worker) in capturers.iter().enumerate() {
            for (property_index, (position, property)) in remaining.iter().enumerate() {
                let Some(move_turns) = route.distance(worker.position, *position, &worker.stats)
                else {
                    continue;
                };
                let turns_to_capture = property
                    .capture_points
                    .div_ceil(capture_power(worker.health));
                let completion = worker
                    .available_turn
                    .saturating_add(move_turns)
                    .saturating_add(turns_to_capture);
                let candidate = (completion, worker_index, property_index, move_turns);
                if best.is_none_or(|current| candidate < current) {
                    best = Some(candidate);
                }
            }
        }
        let Some((completion, worker_index, property_index, _)) = best else {
            return Err(
                "an objective property is unreachable by every assigned capturer".to_owned(),
            );
        };
        let (position, _) = remaining.remove(property_index);
        capturers[worker_index].position = position;
        capturers[worker_index].available_turn = completion;
    }
    Ok(turn.saturating_add(
        capturers
            .iter()
            .map(|worker| worker.available_turn)
            .max()
            .unwrap_or(0),
    ))
}

/// 毎ターンの島portfolioを、不変な勝利ロードマップと子作戦へ照合する。
pub(crate) fn reconcile_campaign_roadmap(
    world: &mut World,
    player_id: PlayerId,
    portfolio: &IslandCampaignPortfolio,
    manager: &SquadManager,
) {
    let turn = world
        .get_resource::<MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let Some(island_map) = world.get_resource::<IslandMap>().cloned() else {
        return;
    };
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

    let mut registry = world
        .remove_resource::<VictoryRoadmapRegistry>()
        .unwrap_or_default();
    let roadmap_id = registry.ensure_roadmap(
        player_id,
        turn,
        enemy_capital,
        enemy_capital_island,
        enemy_unit_count,
    );
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
    // 実行可能な輸送・掃討・占領scheduleが選ばれるまでは明示的なblocked子作戦とする。
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

    for assignment in portfolio
        .defenses
        .iter()
        .chain(portfolio.active_offensives.iter())
    {
        let purpose = purpose_for(assignment, enemy_capital_island);
        let previous_entities = registry
            .operation_keys
            .get(&(player_id, assignment.island_id, purpose))
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
            .get(&(player_id, assignment.island_id, purpose))
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
        let objectives = if purpose == StrategicPurpose::AssaultCapital {
            enemy_capital.into_iter().collect::<Vec<_>>()
        } else {
            let mut positions = property_snapshots
                .iter()
                .filter_map(|(position, _)| {
                    island_map
                        .get_island_at(position)
                        .is_some_and(|island| island.id == assignment.island_id)
                        .then_some(*position)
                })
                .collect::<Vec<_>>();
            positions.sort_unstable_by_key(|position| (position.y, position.x));
            positions
        };
        let phase = operation_phase(world, &island_map, assignment, manager, player_id);
        let matching_combat_plans = combat_plan_summaries
            .iter()
            .filter(|plan| {
                island_map
                    .get_island_at(&plan.anchor)
                    .is_some_and(|island| island.id == assignment.island_id)
                    || plan.objective_properties.iter().any(|position| {
                        island_map
                            .get_island_at(position)
                            .is_some_and(|island| island.id == assignment.island_id)
                    })
            })
            .collect::<Vec<&ActiveCombatPlanSummary>>();
        let local_enemy_count = enemy_positions
            .iter()
            .filter(|position| {
                island_map
                    .get_island_at(position)
                    .is_some_and(|island| island.id == assignment.island_id)
            })
            .count();
        let suppression_turn = if local_enemy_count == 0 {
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
        };
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
                &property_snapshots,
                &island_map,
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
            &owned_properties,
            phase,
        );
        if let Some(operation) = registry.operations.get_mut(&operation_id) {
            operation.combat_plan_ids = matching_combat_plans
                .iter()
                .map(|plan| plan.plan_id)
                .collect();
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
        let issues = diagnose_operation_execution(
            world,
            manager,
            player_id,
            turn,
            assignment,
            &previous_entities,
            &production_records,
        );
        let recoveries = diagnose_operation_recoveries(
            world,
            manager,
            player_id,
            turn,
            assignment,
            &previous_issues,
            &production_records,
        );
        registry.replace_operation_issues(operation_id, issues, recoveries);
        registry.bind_assignment_entities(operation_id, assignment);
        for squad in manager.squads.iter().filter(|squad| {
            squad.owner_id == Some(player_id) && squad.target_island == Some(assignment.island_id)
        }) {
            if let Some(transport) = squad.transport_entity {
                registry.bind_entity_exclusive(
                    operation_id,
                    transport,
                    OperationEntityRole::Transport,
                );
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
    }

    // Combat枠で生産されたEntityはIslandCampaignAssignmentとは別台帳にいる。
    // PlanIdを介して同じStrategicOperationへ接続し、攻撃Eventを作戦実績へ集約する。
    for record in deployment_records
        .into_iter()
        .filter(|record| record.active)
    {
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
        registry.bind_entity_exclusive(operation_id, record.entity, OperationEntityRole::Combat);
    }

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
                .map(move |entity| (*entity, operation.island_id))
        })
        .collect::<Vec<_>>();
    world.insert_resource(registry);
    if let Some(mut execution) =
        world.get_resource_mut::<crate::ai::v4::campaign_execution::V4CampaignExecutionRegistry>()
    {
        for (entity, island_id) in assigned_campaign_entities {
            execution.mark_assigned(entity, island_id, turn);
        }
    }
}

/// action result Eventを、担当EntityのStrategicOperationへ照合する。
#[allow(clippy::too_many_arguments)]
pub fn audit_victory_roadmap_system(
    match_state: Res<MatchState>,
    island_map: Option<Res<IslandMap>>,
    mut moved: EventReader<UnitMovedEvent>,
    mut attacked: EventReader<UnitAttackedEvent>,
    mut loaded: EventReader<UnitLoadedEvent>,
    mut unloaded: EventReader<UnitUnloadedEvent>,
    mut capture_progressed: EventReader<PropertyCaptureProgressedEvent>,
    mut captured: EventReader<PropertyCapturedEvent>,
    mut supplied: EventReader<UnitSuppliedEvent>,
    mut waited: EventReader<UnitWaitedEvent>,
    mut destroyed: EventReader<UnitDestroyedEvent>,
    mut registry: ResMut<VictoryRoadmapRegistry>,
    campaign_execution: Option<
        ResMut<crate::ai::v4::campaign_execution::V4CampaignExecutionRegistry>,
    >,
    operation_assignments: Option<ResMut<crate::ai::operation_assignment::UnitOperationRegistry>>,
) {
    let turn = match_state.current_turn_number.0;
    for event in moved.read() {
        if let Some(island_map) = island_map.as_deref() {
            registry.record_move(event.entity, event.from, event.to, turn, island_map);
        } else {
            registry.record_entity_step(event.entity, turn, CampaignStepKind::Move);
        }
    }
    for event in attacked.read() {
        registry.record_entity_step(event.attacker, turn, CampaignStepKind::Attack);
    }
    for event in loaded.read() {
        registry
            .transport_manifests
            .entry(event.transport)
            .or_default()
            .insert(event.cargo);
        registry.record_entity_step(event.transport, turn, CampaignStepKind::Load);
        registry.record_entity_step(event.cargo, turn, CampaignStepKind::Load);
    }
    for event in unloaded.read() {
        if let Some(manifest) = registry.transport_manifests.get_mut(&event.transport) {
            manifest.remove(&event.cargo);
        }
        registry.record_entity_step(event.transport, turn, CampaignStepKind::Drop);
        registry.record_entity_step(event.cargo, turn, CampaignStepKind::Drop);
    }
    for event in capture_progressed.read() {
        registry.record_entity_step(event.unit, turn, CampaignStepKind::Capture);
        if event.completed
            && let Some(binding) = registry.entity_bindings.get(&event.unit).copied()
            && let Some(operation) = registry.operations.get_mut(&binding.operation_id)
        {
            operation.execution.completed_captures =
                operation.execution.completed_captures.saturating_add(1);
        }
    }
    for event in captured.read() {
        let position = GridPosition {
            x: event.x,
            y: event.y,
        };
        for operation in registry.operations.values_mut().filter(|operation| {
            operation.active && operation.objective_properties.contains(&position)
        }) {
            operation.last_progress_turn = Some(turn);
            operation.last_step = Some(CampaignStepKind::Capture);
        }
    }
    for event in supplied.read() {
        registry.record_entity_step(event.supplier, turn, CampaignStepKind::Supply);
        registry.record_entity_step(event.target, turn, CampaignStepKind::Supply);
    }
    for event in waited.read() {
        registry.record_entity_step(event.entity, turn, CampaignStepKind::Wait);
    }
    let mut campaign_execution = campaign_execution;
    let mut operation_assignments = operation_assignments;
    for event in destroyed.read() {
        if let Some(execution) = campaign_execution.as_deref_mut() {
            execution.mark_destroyed(event.entity, turn);
        }
        if let Some(assignments) = operation_assignments.as_deref_mut() {
            assignments.release_entity(event.entity);
        }
        registry.record_destroyed_entity(event.entity, turn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{GridTopology, MovementType};

    #[test]
    fn operation_identity_does_not_depend_on_enemy_entity_or_anchor() {
        let player = PlayerId(1);
        let island = IslandId(2);
        let assignment = IslandCampaignAssignment {
            island_id: island,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 10, y: 10 },
            capture_target_positions: vec![GridPosition { x: 10, y: 10 }],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 1_000,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 1_000,
            },
            allocated_budget: 1_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        let mut registry = VictoryRoadmapRegistry::default();
        let roadmap = registry.ensure_roadmap(player, 1, None, None, 3);
        let first = registry.reconcile_assignment(
            roadmap,
            player,
            1,
            &assignment,
            StrategicPurpose::CaptureIsland,
            vec![GridPosition { x: 10, y: 10 }],
            &HashSet::new(),
            OperationPhase::Forming,
        );
        let mut moved_anchor = assignment.clone();
        moved_anchor.target_position = GridPosition { x: 12, y: 11 };
        let second = registry.reconcile_assignment(
            roadmap,
            player,
            2,
            &moved_anchor,
            StrategicPurpose::CaptureIsland,
            vec![GridPosition { x: 12, y: 11 }],
            &HashSet::new(),
            OperationPhase::Transit,
        );

        assert_eq!(first, second);
        let operation = registry.operations.get(&first).expect("作戦");
        assert_eq!(
            operation.objective_properties,
            vec![GridPosition { x: 10, y: 10 }],
            "毎ターンのanchor変化で目的拠点を差し替えない"
        );
        assert_eq!(operation.tactical_anchor, GridPosition { x: 12, y: 11 });
    }

    #[test]
    fn roadmap_binding_moves_entity_out_of_previous_operation_atomically() {
        let player = PlayerId(1);
        let entity = Entity::from_raw(42);
        let assignment = |island_id: IslandId| IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition {
                x: island_id.0,
                y: 0,
            },
            capture_target_positions: vec![GridPosition {
                x: island_id.0,
                y: 0,
            }],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 1_000,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 1_000,
            transport_entities: Vec::new(),
            capture_entities: vec![entity],
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let first_assignment = assignment(IslandId(2));
        let second_assignment = assignment(IslandId(3));
        let mut registry = VictoryRoadmapRegistry::default();
        let roadmap = registry.ensure_roadmap(player, 1, None, None, 1);
        let first = registry.reconcile_assignment(
            roadmap,
            player,
            1,
            &first_assignment,
            StrategicPurpose::CaptureIsland,
            first_assignment.capture_target_positions.clone(),
            &HashSet::new(),
            OperationPhase::Forming,
        );
        registry.bind_assignment_entities(first, &first_assignment);
        let second = registry.reconcile_assignment(
            roadmap,
            player,
            1,
            &second_assignment,
            StrategicPurpose::CaptureIsland,
            second_assignment.capture_target_positions.clone(),
            &HashSet::new(),
            OperationPhase::Forming,
        );
        registry.bind_assignment_entities(second, &second_assignment);

        assert!(
            !registry.operations[&first]
                .assigned_capturers
                .contains(&entity)
        );
        assert!(
            registry.operations[&second]
                .assigned_capturers
                .contains(&entity)
        );
        assert_eq!(registry.entity_bindings[&entity].operation_id, second);

        registry.operations.get_mut(&second).unwrap().active = false;
        registry.release_inactive_assignments(player);
        assert!(registry.operations[&second].assigned_capturers.is_empty());
        assert!(!registry.entity_bindings.contains_key(&entity));
    }

    #[test]
    fn issue_detection_is_not_counted_as_replanning_until_recovery_occurs() {
        let player = PlayerId(1);
        let island = IslandId(2);
        let entity = Entity::from_raw(42);
        let assignment = IslandCampaignAssignment {
            island_id: island,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 3, y: 4 },
            capture_target_positions: vec![GridPosition { x: 3, y: 4 }],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 1_000,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 1_000,
            transport_entities: Vec::new(),
            capture_entities: vec![entity],
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let mut registry = VictoryRoadmapRegistry::default();
        let roadmap = registry.ensure_roadmap(player, 1, None, None, 1);
        let operation_id = registry.reconcile_assignment(
            roadmap,
            player,
            1,
            &assignment,
            StrategicPurpose::CaptureIsland,
            assignment.capture_target_positions.clone(),
            &HashSet::new(),
            OperationPhase::Forming,
        );
        registry.bind_assignment_entities(operation_id, &assignment);
        let issue = OperationIssue {
            kind: OperationIssueKind::AssignmentLost,
            detected_turn: 2,
            entity: Some(entity),
            related_entity: None,
            detail: "assignment disappeared".to_owned(),
        };

        registry.replace_operation_issues(operation_id, vec![issue.clone()], Vec::new());
        assert_eq!(registry.operations[&operation_id].replan_count, 0);
        assert_eq!(
            registry
                .entity_bindings
                .get(&entity)
                .and_then(|binding| registry.operations.get(&binding.operation_id))
                .map(|operation| operation.island_id),
            Some(island),
            "Roadmap監査でもEntityの作戦bindingを保持する"
        );
        let island_map = IslandMap {
            islands: vec![
                crate::ai::islands::Island {
                    id: IslandId(1),
                    tiles: HashSet::from([
                        GridPosition { x: 0, y: 0 },
                        GridPosition { x: 1, y: 0 },
                    ]),
                },
                crate::ai::islands::Island {
                    id: island,
                    tiles: HashSet::from([GridPosition { x: 3, y: 4 }]),
                },
            ],
        };
        registry.record_move(
            entity,
            GridPosition { x: 0, y: 0 },
            GridPosition { x: 1, y: 0 },
            2,
            &island_map,
        );
        assert_eq!(
            registry.operations[&operation_id].execution.deviations, 0,
            "搭載前の出発島内移動は逸脱ではない"
        );

        registry.replace_operation_issues(
            operation_id,
            Vec::new(),
            vec![OperationRecoveryAction {
                kind: OperationRecoveryKind::RestoreAssignment,
                cause: issue.kind,
                completed_turn: 3,
                entity: Some(entity),
                detail: "restored assignment".to_owned(),
            }],
        );
        let operation = &registry.operations[&operation_id];
        assert_eq!(operation.replan_count, 1);
        assert_eq!(operation.last_replan_turn, Some(3));
        assert_eq!(operation.recovery_history.len(), 1);
    }

    #[test]
    fn capturer_waiting_on_source_island_keeps_operation_forming() {
        let player = PlayerId(1);
        let target_island = IslandId(2);
        let mut world = World::new();
        let capturer = world.spawn(GridPosition { x: 0, y: 0 }).id();
        let island_map = IslandMap {
            islands: vec![
                crate::ai::islands::Island {
                    id: IslandId(1),
                    tiles: HashSet::from([GridPosition { x: 0, y: 0 }]),
                },
                crate::ai::islands::Island {
                    id: target_island,
                    tiles: HashSet::from([GridPosition { x: 3, y: 4 }]),
                },
            ],
        };
        let assignment = IslandCampaignAssignment {
            island_id: target_island,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 3, y: 4 },
            capture_target_positions: vec![GridPosition { x: 3, y: 4 }],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: Some(crate::resources::UnitType::TransportHelicopter),
                transport_slots: 1,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 6_000,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: Some(crate::resources::UnitType::TransportHelicopter),
                transport_slots: 1,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 5_000,
            },
            allocated_budget: 1_000,
            transport_entities: Vec::new(),
            capture_entities: vec![capturer],
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: true,
        };

        assert_eq!(
            operation_phase(
                &world,
                &island_map,
                &assignment,
                &SquadManager::default(),
                player,
            ),
            OperationPhase::Forming
        );
    }

    #[test]
    fn completion_forecast_covers_every_property_not_only_the_anchor() {
        let player = PlayerId(1);
        let mut world = World::new();
        let map = Map::new(5, 1, Terrain::Plains, GridTopology::Square);
        let island_map = IslandMap::analyze(&map);
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let capturer = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    can_capture: true,
                    max_movement: 3,
                    movement_type: MovementType::Infantry,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let first = GridPosition { x: 2, y: 0 };
        let second = GridPosition { x: 4, y: 0 };
        let properties = vec![
            (first, Property::new(Terrain::City, None, 100)),
            (second, Property::new(Terrain::City, None, 100)),
        ];
        let assignment = IslandCampaignAssignment {
            island_id: island_map.get_island_at(&first).unwrap().id,
            decision: IslandCampaignDecision::Expand,
            target_position: first,
            capture_target_positions: vec![first, second],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 1_000,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 1_000,
            transport_entities: Vec::new(),
            capture_entities: vec![capturer],
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: false,
        };

        let completion = estimate_full_objective_completion(
            &mut world,
            player,
            1,
            assignment.island_id,
            &assignment,
            &[first, second],
            &properties,
            &island_map,
            &SquadManager::default(),
            Some(1),
        )
        .unwrap();

        assert_eq!(completion, 5, "2拠点目の移動・占領まで期限へ含める");
    }

    #[test]
    fn capital_objective_exists_before_an_assault_package_is_executable() {
        let player = PlayerId(1);
        let capital = GridPosition { x: 20, y: 4 };
        let island = IslandId(7);
        let mut registry = VictoryRoadmapRegistry::default();
        let roadmap = registry.ensure_roadmap(player, 1, Some(capital), Some(island), 6);

        let first = registry.ensure_capital_objective(roadmap, player, 1, island, capital, false);
        let second = registry.ensure_capital_objective(roadmap, player, 2, island, capital, false);

        assert_eq!(first, second);
        let operation = registry.operations.get(&first).unwrap();
        assert!(operation.active);
        assert_eq!(operation.purpose, StrategicPurpose::AssaultCapital);
        assert_eq!(operation.objective_properties, vec![capital]);
        assert_eq!(operation.planned_completion_turn, None);
        assert!(operation.blocked_reason.is_some());
    }

    #[test]
    fn destroyed_transport_classifies_carrier_and_loaded_cargo_separately() {
        let player = PlayerId(2);
        let island = IslandId(3);
        let transport = Entity::from_raw(40);
        let cargo = Entity::from_raw(41);
        let assignment = IslandCampaignAssignment {
            island_id: island,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 10, y: 10 },
            capture_target_positions: vec![GridPosition { x: 10, y: 10 }],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: Some(crate::resources::UnitType::TransportHelicopter),
                transport_slots: 1,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 5_000,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 5_000,
            transport_entities: vec![transport],
            capture_entities: vec![cargo],
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let mut registry = VictoryRoadmapRegistry::default();
        let roadmap = registry.ensure_roadmap(player, 1, None, None, 3);
        let operation_id = registry.reconcile_assignment(
            roadmap,
            player,
            1,
            &assignment,
            StrategicPurpose::CaptureIsland,
            assignment.capture_target_positions.clone(),
            &HashSet::new(),
            OperationPhase::Transit,
        );
        registry.bind_assignment_entities(operation_id, &assignment);
        registry
            .transport_manifests
            .entry(transport)
            .or_default()
            .insert(cargo);

        registry.record_destroyed_entity(transport, 2);

        let operation = registry.operations.get(&operation_id).unwrap();
        assert!(operation.current_issues.iter().any(|issue| {
            issue.kind == OperationIssueKind::TransportDestroyed && issue.entity == Some(transport)
        }));
        assert!(operation.current_issues.iter().any(|issue| {
            issue.kind == OperationIssueKind::CargoLostWithTransport
                && issue.entity == Some(cargo)
                && issue.related_entity == Some(transport)
        }));
        assert_eq!(operation.last_replan_turn, None);
        assert_eq!(operation.replan_count, 0, "損耗検知だけを再計画扱いしない");
        assert!(operation.recovery_history.is_empty());
    }
}
