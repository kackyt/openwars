use super::strategic_selection::{
    RoadmapStrategicCandidateRegistry, StrategicCandidateEvaluation, StrategicCandidateKind,
    purpose_for, select_strategic_portfolio,
};
use super::turn_plan::{RoadmapOperationDirective, RoadmapTurnPlan, RoadmapTurnPlanRegistry};
use super::*;

fn operation_with_entity(
    registry: &mut VictoryRoadmapRegistry,
    player: PlayerId,
    island: IslandId,
    entity: Entity,
    role: OperationEntityRole,
) -> StrategicOperationId {
    let roadmap = registry.ensure_roadmap(player, 1, None, None, 1);
    let operation = StrategicOperationId(999);
    registry.operations.insert(
        operation,
        StrategicOperation {
            id: operation,
            roadmap_id: roadmap,
            player_id: player,
            island_id: island,
            purpose: StrategicPurpose::CaptureIsland,
            created_turn: 1,
            last_observed_turn: 1,
            tactical_anchor: GridPosition { x: 3, y: 3 },
            objective_properties: vec![GridPosition { x: 3, y: 3 }],
            owned_objective_count: 0,
            node_state: RoadmapNodeState::Ready,
            phase: OperationPhase::Forming,
            planned_completion_turn: None,
            actual_completion_turn: None,
            assigned_transports: HashSet::default(),
            assigned_capturers: HashSet::default(),
            assigned_combat: HashSet::default(),
            combat_plan_ids: HashSet::default(),
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
    registry.bind_entity_exclusive(operation, entity, role);
    operation
}

#[test]
fn capture_replacement_is_pending_only_between_loss_and_reorder() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let island = IslandId(2);
    let capturer = Entity::from_raw(41);
    let mut registry = VictoryRoadmapRegistry::default();
    let operation_id = operation_with_entity(
        &mut registry,
        player,
        island,
        capturer,
        OperationEntityRole::Capture,
    );

    assert!(!registry.capture_replacement_pending(player, island));
    registry.record_destroyed_entity(capturer, 6);
    assert!(registry.capture_replacement_pending(player, island));

    registry
        .operations
        .get_mut(&operation_id)
        .expect("局地作戦")
        .recovery_history
        .push(OperationRecoveryAction {
            kind: OperationRecoveryKind::RequestReplacement,
            cause: OperationIssueKind::CapturerDestroyed,
            completed_turn: 7,
            entity: Some(Entity::from_raw(42)),
            detail: "capture replacement ordered".to_string(),
        });
    assert!(!registry.capture_replacement_pending(player, island));
}

fn strategic_assignment(
    island_id: IslandId,
    decision: IslandCampaignDecision,
    shortfall: u32,
    continued: bool,
) -> IslandCampaignAssignment {
    IslandCampaignAssignment {
        island_id,
        decision,
        target_position: GridPosition {
            x: island_id.0,
            y: 0,
        },
        capture_target_positions: Vec::new(),
        priority_enemy_types: Vec::new(),
        requirement: crate::ai::island_campaign::IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: shortfall,
        },
        purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: shortfall,
        },
        allocated_budget: 0,
        transport_entities: Vec::new(),
        capture_entities: Vec::new(),
        combat_entities: Vec::new(),
        operation_ready: true,
        continued_from_existing_squad: continued,
    }
}

#[test]
fn post_formation_observation_keeps_adoption_and_refreshes_squad_directives() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let position = GridPosition { x: 1, y: 0 };
    let island = IslandId(1);
    let mut world = World::new();
    world.insert_resource(
        crate::ai::strategy_profile::V4StrategyProfileRegistry::with_profile(
            player,
            crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion,
        ),
    );
    world.insert_resource(IslandMap {
        islands: vec![crate::ai::islands::Island {
            id: island,
            tiles: HashSet::from_iter([position]),
        }],
    });
    let entity = world.spawn((Faction(player), position)).id();
    let mut assignment = strategic_assignment(island, IslandCampaignDecision::Expand, 0, true);
    assignment.capture_entities.push(entity);
    let adopted = IslandCampaignPortfolio {
        islands: Vec::new(),
        active_offensives: vec![assignment],
        defenses: Vec::new(),
    };
    // 採用時の監査値を固定し、編成後の再評価で上書きされないことを検査する。
    let mut candidates = RoadmapStrategicCandidateRegistry::default();
    candidates.by_player.insert(
        player,
        vec![StrategicCandidateEvaluation {
            kind: StrategicCandidateKind::Focus,
            offensive_islands: vec![island],
            score: 123_456,
            selected: true,
        }],
    );
    world.insert_resource(candidates);
    let mut manager = SquadManager::new();
    let mut old_squad = manager.create_squad(MissionType::Capture);
    old_squad.plan.owner_id = Some(player);
    old_squad.plan.target_island = Some(island);
    old_squad.members.insert(entity);
    let old_id = old_squad.id;
    crate::ai::v4::strategy_pipeline::after_squad_planning(
        &mut world,
        player,
        &adopted,
        &mut manager,
    );
    assert_eq!(
        world
            .resource::<RoadmapStrategicCandidateRegistry>()
            .by_player[&player][0]
            .score,
        123_456
    );
    assert_eq!(
        world.resource::<RoadmapTurnPlanRegistry>().plans[&player].portfolio,
        adopted
    );

    // 同じ実EntityのSquadが作り直された場合も、採用案は固定して参照だけを更新する。
    manager.squads.clear();
    let mut new_squad = manager.create_squad(MissionType::Capture);
    new_squad.plan.owner_id = Some(player);
    new_squad.plan.target_island = Some(island);
    new_squad.members.insert(entity);
    let new_id = new_squad.id;
    assert_ne!(old_id, new_id);
    crate::ai::v4::strategy_pipeline::after_squad_planning(
        &mut world,
        player,
        &adopted,
        &mut manager,
    );
    let plan = &world.resource::<RoadmapTurnPlanRegistry>().plans[&player];
    assert_eq!(plan.portfolio, adopted);
    assert!(
        plan.directives
            .iter()
            .any(|directive| directive.squad_ids == vec![new_id])
    );
    assert!(
        plan.directives
            .iter()
            .all(|directive| !directive.squad_ids.contains(&old_id))
    );
    assert_eq!(
        world
            .resource::<RoadmapStrategicCandidateRegistry>()
            .by_player[&player][0]
            .score,
        123_456
    );
}

#[test]
fn strategy_candidate_search_prunes_to_a_single_best_focus_without_dropping_defense() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let defense = strategic_assignment(IslandId(9), IslandCampaignDecision::Defend, 0, true);
    let portfolio = IslandCampaignPortfolio {
        islands: Vec::new(),
        active_offensives: vec![
            strategic_assignment(IslandId(1), IslandCampaignDecision::Expand, 100_000, false),
            strategic_assignment(IslandId(2), IslandCampaignDecision::Assault, 0, true),
            strategic_assignment(IslandId(3), IslandCampaignDecision::Contest, 100_000, false),
            // 上位3件だけを分岐対象にする。第4候補は候補爆発を防ぐため除外する。
            strategic_assignment(IslandId(4), IslandCampaignDecision::Secure, 100_000, false),
        ],
        defenses: vec![defense.clone()],
    };

    let (selected, evaluations) = select_strategic_portfolio(&portfolio);

    assert_eq!(selected.defenses, vec![defense]);
    assert_eq!(selected.active_offensives.len(), 1);
    assert_eq!(selected.active_offensives[0].island_id, IslandId(2));
    assert_eq!(
        evaluations.len(),
        7,
        "実行可能な攻勢があるためHoldを除いた上位3Nodeの候補だけを比較する"
    );
    assert_eq!(
        evaluations
            .iter()
            .filter(|evaluation| evaluation.selected)
            .count(),
        1
    );
}

#[test]
fn node_state_distinguishes_dominance_capture_and_blocked() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let combat = Entity::from_raw(42);
    let mut registry = VictoryRoadmapRegistry::default();
    let operation = operation_with_entity(
        &mut registry,
        player,
        IslandId(2),
        combat,
        OperationEntityRole::Combat,
    );
    let current = registry.operations.get_mut(&operation).unwrap();
    current.phase = OperationPhase::Suppress;
    current.assigned_combat.insert(combat);
    current.planned_suppression_turn = Some(3);
    registry.refresh_node_states(player);
    assert_eq!(
        registry.operations[&operation].node_state,
        RoadmapNodeState::Dominant
    );

    registry.operations.get_mut(&operation).unwrap().phase = OperationPhase::Capture;
    registry.refresh_node_states(player);
    assert_eq!(
        registry.operations[&operation].node_state,
        RoadmapNodeState::Capturing
    );

    let current = registry.operations.get_mut(&operation).unwrap();
    current.phase = OperationPhase::Blocked;
    current.blocked_reason = Some("transport destroyed".to_owned());
    registry.refresh_node_states(player);
    assert_eq!(
        registry.operations[&operation].node_state,
        RoadmapNodeState::Blocked
    );
}

#[test]
fn observed_island_nodes_remain_in_the_roadmap_without_assignments() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let mut registry = VictoryRoadmapRegistry::default();
    let roadmap = registry.ensure_roadmap(player, 1, None, None, 0);
    let assessment = |island_id, state, decision| IslandCampaignAssessment {
        island_id,
        state,
        decision,
        state_reason: String::new(),
        decision_reason: String::new(),
        pause_cause: None,
        neutral_properties: 0,
        friendly_properties: 0,
        enemy_properties: 0,
        friendly_combat_units: 0,
        enemy_combat_units: 0,
        friendly_arrival_eta: None,
        enemy_arrival_eta: None,
        friendly_capture_eta: None,
        enemy_capture_eta: None,
        roi_production_sites: 0,
        transport_eta: None,
        expansion_payback_turns: None,
        required_budget: 0,
        allocated_budget: 0,
    };
    let secured = assessment(
        IslandId(1),
        crate::ai::island_campaign::IslandCampaignState::Secured,
        IslandCampaignDecision::Secure,
    );
    let pending = assessment(
        IslandId(2),
        crate::ai::island_campaign::IslandCampaignState::OpenNeutral,
        IslandCampaignDecision::Expand,
    );
    let secured_anchor = GridPosition { x: 1, y: 1 };
    let pending_anchor = GridPosition { x: 2, y: 2 };
    registry.reconcile_observed_island(
        roadmap,
        player,
        1,
        &secured,
        secured_anchor,
        vec![secured_anchor],
        &HashSet::from_iter([secured_anchor]),
    );
    registry.reconcile_observed_island(
        roadmap,
        player,
        1,
        &pending,
        pending_anchor,
        vec![pending_anchor],
        &HashSet::default(),
    );
    registry.refresh_node_states(player);

    let secured_operation = registry
        .operation_keys
        .get(&(player, IslandId(1), StrategicOperationScope::Regional))
        .copied()
        .expect("確保済み島の観測Node");
    let pending_operation = registry
        .operation_keys
        .get(&(player, IslandId(2), StrategicOperationScope::Regional))
        .copied()
        .expect("未着手島の観測Node");
    assert!(!registry.operations[&secured_operation].active);
    assert_eq!(
        registry.operations[&secured_operation].node_state,
        RoadmapNodeState::Secured
    );
    assert!(!registry.operations[&pending_operation].active);
    assert_eq!(
        registry.operations[&pending_operation].node_state,
        RoadmapNodeState::Locked
    );
}

#[test]
fn successor_stays_locked_until_every_predecessor_is_dominant_or_secured() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let combat = Entity::from_raw(42);
    let mut registry = VictoryRoadmapRegistry::default();
    let predecessor = operation_with_entity(
        &mut registry,
        player,
        IslandId(1),
        combat,
        OperationEntityRole::Combat,
    );
    let mut second_predecessor = registry.operations[&predecessor].clone();
    second_predecessor.id = StrategicOperationId(1_000);
    second_predecessor.island_id = IslandId(2);
    second_predecessor.assigned_combat.clear();
    second_predecessor.phase = OperationPhase::Forming;
    let second_predecessor_id = second_predecessor.id;
    registry
        .operations
        .insert(second_predecessor_id, second_predecessor);
    let mut successor = registry.operations[&predecessor].clone();
    successor.id = StrategicOperationId(1_001);
    successor.island_id = IslandId(3);
    successor.assigned_combat.clear();
    successor.phase = OperationPhase::Forming;
    successor.blocked_reason = None;
    let successor_id = successor.id;
    registry.operations.insert(successor_id, successor);
    registry.roadmaps.get_mut(&player).unwrap().dependencies = vec![
        RoadmapDependency {
            predecessor,
            successor: successor_id,
            kind: RoadmapDependencyKind::Logistics,
        },
        RoadmapDependency {
            predecessor: second_predecessor_id,
            successor: successor_id,
            kind: RoadmapDependencyKind::Logistics,
        },
    ];

    registry.refresh_node_states(player);
    assert_eq!(
        registry.operations[&successor_id].node_state,
        RoadmapNodeState::Locked
    );

    let first = registry.operations.get_mut(&predecessor).unwrap();
    first.phase = OperationPhase::Suppress;
    first.assigned_combat.insert(combat);
    first.planned_suppression_turn = Some(2);
    registry.refresh_node_states(player);
    assert_eq!(
        registry.operations[&successor_id].node_state,
        RoadmapNodeState::Locked,
        "合流Nodeは片方の前提だけで解放しない"
    );

    registry
        .operations
        .get_mut(&second_predecessor_id)
        .unwrap()
        .phase = OperationPhase::Completed;
    registry.refresh_node_states(player);
    assert_eq!(
        registry.operations[&successor_id].node_state,
        RoadmapNodeState::Ready
    );
}

#[test]
fn locked_operation_is_not_projected_to_the_squad_turn_plan() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let island = IslandId(2);
    let mut registry = VictoryRoadmapRegistry::default();
    let operation = operation_with_entity(
        &mut registry,
        player,
        island,
        Entity::from_raw(42),
        OperationEntityRole::Capture,
    );
    registry.operation_keys.insert(
        (player, island, StrategicOperationScope::Regional),
        operation,
    );
    let assignment = IslandCampaignAssignment {
        island_id: island,
        decision: IslandCampaignDecision::Expand,
        target_position: GridPosition { x: 3, y: 3 },
        capture_target_positions: vec![GridPosition { x: 3, y: 3 }],
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
        capture_entities: Vec::new(),
        combat_entities: Vec::new(),
        operation_ready: true,
        continued_from_existing_squad: false,
    };
    let portfolio = IslandCampaignPortfolio {
        islands: Vec::new(),
        active_offensives: vec![assignment],
        defenses: Vec::new(),
    };

    registry.operations.get_mut(&operation).unwrap().node_state = RoadmapNodeState::Locked;
    assert!(
        registry
            .approved_portfolio(player, None, &portfolio)
            .active_offensives
            .is_empty()
    );

    registry.operations.get_mut(&operation).unwrap().node_state = RoadmapNodeState::Ready;
    assert_eq!(
        registry
            .approved_portfolio(player, None, &portfolio)
            .active_offensives
            .len(),
        1
    );
}

#[test]
fn logistics_route_and_capital_segment_create_deterministic_dependencies() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let mut registry = VictoryRoadmapRegistry::default();
    let first = operation_with_entity(
        &mut registry,
        player,
        IslandId(1),
        Entity::from_raw(41),
        OperationEntityRole::Combat,
    );
    let mut second_operation = registry.operations[&first].clone();
    second_operation.id = StrategicOperationId(1_000);
    second_operation.island_id = IslandId(2);
    let second = second_operation.id;
    registry.operations.insert(second, second_operation);
    let mut capital_regional_operation = registry.operations[&first].clone();
    capital_regional_operation.id = StrategicOperationId(1_001);
    capital_regional_operation.island_id = IslandId(3);
    let capital_regional = capital_regional_operation.id;
    registry
        .operations
        .insert(capital_regional, capital_regional_operation);
    let mut capital_operation = registry.operations[&first].clone();
    capital_operation.id = StrategicOperationId(1_002);
    capital_operation.island_id = IslandId(3);
    capital_operation.purpose = StrategicPurpose::AssaultCapital;
    let capital = capital_operation.id;
    registry.operations.insert(capital, capital_operation);
    registry.operation_keys.extend([
        (
            (player, IslandId(1), StrategicOperationScope::Regional),
            first,
        ),
        (
            (player, IslandId(2), StrategicOperationScope::Regional),
            second,
        ),
        (
            (player, IslandId(3), StrategicOperationScope::Regional),
            capital_regional,
        ),
        (
            (player, IslandId(3), StrategicOperationScope::Capital),
            capital,
        ),
    ]);
    let logistics_plan = crate::ai::v4::logistics_plan::V4LogisticsPlan {
        plan_id: 1,
        player_id: player,
        created_turn: 1,
        revised_turn: 1,
        last_observed_turn: 1,
        revision: 0,
        route_islands: vec![IslandId(1), IslandId(2)],
        selected_islands: vec![IslandId(1), IslandId(2)],
        stages: Vec::new(),
        direct_metrics: Default::default(),
        selected_metrics: Default::default(),
        current_forecast_metrics: Default::default(),
        replan_reason: crate::ai::v4::logistics_plan::LogisticsReplanReason::InitialSelection,
    };

    registry.rebuild_dependencies(player, Some(IslandId(3)), Some(&logistics_plan));

    assert_eq!(
        registry.roadmap(player).unwrap().dependencies,
        vec![
            RoadmapDependency {
                predecessor: first,
                successor: second,
                kind: RoadmapDependencyKind::Logistics,
            },
            RoadmapDependency {
                predecessor: second,
                successor: capital,
                kind: RoadmapDependencyKind::Logistics,
            },
            RoadmapDependency {
                predecessor: capital_regional,
                successor: capital,
                kind: RoadmapDependencyKind::CapitalRoute,
            },
        ]
    );
}

#[test]
fn turn_plan_rejects_squad_shared_by_multiple_operations() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let squad = crate::ai::squad::SquadId(7);
    let plan = RoadmapTurnPlan {
        player_id: player,
        turn: 1,
        portfolio: IslandCampaignPortfolio::default(),
        directives: vec![
            RoadmapOperationDirective {
                operation_id: StrategicOperationId(1),
                island_id: IslandId(1),
                purpose: StrategicPurpose::CaptureIsland,
                node_state: RoadmapNodeState::Ready,
                target: GridPosition { x: 1, y: 1 },
                squad_ids: vec![squad],
            },
            RoadmapOperationDirective {
                operation_id: StrategicOperationId(2),
                island_id: IslandId(2),
                purpose: StrategicPurpose::DefendIsland,
                node_state: RoadmapNodeState::Ready,
                target: GridPosition { x: 2, y: 2 },
                squad_ids: vec![squad],
            },
        ],
    };

    assert!(!plan.is_consistent());
}

#[test]
fn operation_identity_does_not_depend_on_enemy_entity_or_anchor() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
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
        &HashSet::default(),
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
        &HashSet::default(),
        OperationPhase::Transit,
    );

    assert_eq!(first, second);
    let operation = registry.operations.get(&first).expect("作戦");
    assert_eq!(
        operation.objective_properties,
        vec![GridPosition { x: 12, y: 11 }],
        "作戦実績は維持しつつ、成功済みMilestoneを現在目標から除く"
    );
    assert_eq!(operation.tactical_anchor, GridPosition { x: 12, y: 11 });
}

#[test]
fn regional_milestone_does_not_overwrite_capital_operation_on_the_same_island() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let island = IslandId(7);
    let capital = GridPosition { x: 20, y: 4 };
    let local_target = GridPosition { x: 10, y: 4 };
    let assignment = IslandCampaignAssignment {
        island_id: island,
        decision: IslandCampaignDecision::Contest,
        target_position: local_target,
        capture_target_positions: vec![local_target],
        priority_enemy_types: Vec::new(),
        requirement: crate::ai::island_campaign::IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 0,
        },
        purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 0,
        },
        allocated_budget: 0,
        transport_entities: Vec::new(),
        capture_entities: Vec::new(),
        combat_entities: Vec::new(),
        operation_ready: false,
        continued_from_existing_squad: false,
    };
    let mut registry = VictoryRoadmapRegistry::default();
    let roadmap = registry.ensure_roadmap(player, 1, Some(capital), Some(island), 1);
    let capital_operation =
        registry.ensure_capital_objective(roadmap, player, 1, island, capital, false);

    assert_eq!(
        purpose_for(&assignment, Some(capital)),
        StrategicPurpose::CaptureIsland
    );
    let regional_operation = registry.reconcile_assignment(
        roadmap,
        player,
        1,
        &assignment,
        purpose_for(&assignment, Some(capital)),
        assignment.capture_target_positions.clone(),
        &HashSet::default(),
        OperationPhase::Forming,
    );

    assert_ne!(capital_operation, regional_operation);
    assert_eq!(registry.operations.len(), 2);
    assert_eq!(
        registry.operations[&capital_operation].tactical_anchor,
        capital
    );
    assert_eq!(
        registry.operations[&regional_operation].tactical_anchor,
        local_target
    );
}
