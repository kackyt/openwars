use crate::ai::v4::ProductionPolicy;
use crate::ai::v4::capital_route::commitment::{
    capital_route_capture_joins_active_node, capital_route_controls_active_node,
    capital_route_dominant_capture_escort_node, capital_route_execution_phase,
    capital_route_frontline_sort_key, capital_route_node_frontline_capacity,
    capital_route_role_frontline_capacity, capital_route_uses_capture_lane,
    route_target_node_anchor, squad_target_route,
};
use crate::ai::v4::capital_route::force_demand::split_bridge_route_objectives;
use crate::ai::v4::capital_route::force_demand::{
    apportion_route_force_slots, refine_same_land_route_milestones,
    same_land_armored_entry_shortfall, same_land_pending_bridgeheads,
    same_land_route_breakthrough_positions,
};
use crate::ai::v4::capital_route::model::{
    CAPITAL_ROUTE_REGION_RADIUS, CapitalRouteCommitment, CapitalRouteExecutionPhase,
    CapitalRouteId, CapitalRouteNodeId, CapitalRouteNodeObjective, CapitalRouteNodeOperation,
    CapitalRouteNodeOperationId, CapitalRouteNodeOperationRegistry, CapitalRouteNodeScope,
    CapitalRoutePathRegistry, CapitalRouteTopologyKey, CapitalRouteTopologyRegistry,
    RouteForceDemand,
};
use crate::ai::v4::capital_route::node_operation::{
    capital_route_assault_authorized_from_dag, capital_route_dag_frontier_properties,
    capital_route_node_control_area, capital_route_node_exit_satisfied,
    capital_route_node_predecessors_released, consolidate_nearby_capture_target_regions,
    ensure_capital_route_node_operations, refresh_capital_route_node_operations,
};
use crate::ai::v4::capital_route::prepare_capital_route_topologies;
use crate::ai::v4::capital_route::tactical::{
    capital_route_allows_capture_at, capital_route_capture_target_is_reserved,
    capital_route_is_recovering, capital_route_waypoint,
};
use crate::ai::v4::capital_route::topology::{
    armored_entry_bridge_gates, armored_step_distances, capital_route_topology_for,
    same_land_capital_front_reached, same_land_route_fronts,
};
use crate::ai::v4::production_pipeline::test_exports::{build_operations, combat_plan_input};
use crate::ai::v4::test_support::*;
use crate::ai::v4::{
    CampaignPlanningObjective, Entity, Faction, GridPosition, HashMap, HashSet, Health, Map,
    MasterDataRegistry, MissionType, MovementType, OperationKind, PlayerId, Property, ReachCtx,
    SquadId, Terrain, UnitSnapshot, UnitStats, UnitType, World, assignment, operation,
    victory_roadmap,
};
use crate::resources::GridTopology;

#[test]
fn frontline_capacity_follows_scanned_node_geometry() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let island = crate::ai::islands::IslandId(0);
    let operation = |scope, control_area, capture_targets| CapitalRouteNodeOperation {
        id: CapitalRouteNodeOperationId {
            topology: CapitalRouteTopologyKey {
                player_id: player,
                island_id: island,
            },
            node: CapitalRouteNodeId(1),
        },
        anchor: pos(2, 2),
        predecessors: Vec::new(),
        successors: Vec::new(),
        objective: CapitalRouteNodeObjective::Capture,
        scope,
        control_area,
        capture_targets,
        crossing_exit: None,
        crossed: false,
        state: victory_roadmap::RoadmapNodeState::Ready,
        assigned_squads: HashSet::default(),
    };

    // 橋などの一点Nodeは、部隊数ではなく通過地形そのものから一枠とする。
    assert_eq!(
        capital_route_node_frontline_capacity(&operation(
            CapitalRouteNodeScope::Point,
            vec![pos(2, 2), pos(3, 2), pos(4, 2)],
            vec![pos(4, 2), pos(5, 2)],
        )),
        1
    );
    // Areaは、局地戦の実セル数または占領対象数の大きい方だけを前衛へ送る。
    assert_eq!(
        capital_route_node_frontline_capacity(&operation(
            CapitalRouteNodeScope::Area,
            vec![pos(2, 2), pos(3, 2), pos(4, 2)],
            vec![pos(4, 2), pos(5, 2), pos(6, 2), pos(7, 2)],
        )),
        4
    );
}

#[test]
fn nearby_capture_region_has_one_owner_operation() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let map = flat_map(12, 3);
    let anchors = vec![pos(1, 1), pos(2, 2), pos(4, 1), pos(8, 1)];
    let mut targets = vec![
        vec![pos(1, 1)],
        vec![pos(2, 2)],
        vec![pos(4, 1)],
        vec![pos(8, 1)],
    ];

    consolidate_nearby_capture_target_regions(&map, &anchors, &mut targets);

    assert_eq!(targets[0], vec![pos(1, 1), pos(4, 1), pos(2, 2)]);
    assert!(targets[1].is_empty());
    assert!(targets[2].is_empty());
    assert_eq!(
        targets[3],
        vec![pos(8, 1)],
        "局地半径を越えた拠点まで一つの完了条件へ混ぜない"
    );
}

#[test]
fn linear_capture_corridor_keeps_independent_operations() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let map = flat_map(12, 3);
    let anchors = vec![pos(1, 1), pos(2, 1), pos(4, 1)];
    let mut targets = vec![vec![pos(1, 1)], vec![pos(2, 1)], vec![pos(4, 1)]];

    consolidate_nearby_capture_target_regions(&map, &anchors, &mut targets);

    assert_eq!(
        targets,
        vec![vec![pos(1, 1)], vec![pos(2, 1)], vec![pos(4, 1)]],
        "一直線の回廊は一Areaへ束ねず、前方へ順番に進める"
    );
}

#[test]
fn capture_node_requires_secured_for_both_area_and_point() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let topology = CapitalRouteTopologyKey {
        player_id: player,
        island_id: crate::ai::islands::IslandId(0),
    };
    let operation = |scope| CapitalRouteNodeOperation {
        id: CapitalRouteNodeOperationId {
            topology,
            node: CapitalRouteNodeId(1),
        },
        anchor: pos(2, 2),
        predecessors: Vec::new(),
        successors: Vec::new(),
        objective: CapitalRouteNodeObjective::Capture,
        scope,
        control_area: vec![pos(2, 2)],
        capture_targets: vec![pos(2, 2)],
        crossing_exit: None,
        crossed: false,
        state: victory_roadmap::RoadmapNodeState::Dominant,
        assigned_squads: HashSet::default(),
    };

    assert!(
        !capital_route_node_exit_satisfied(&operation(CapitalRouteNodeScope::Area)),
        "Areaの局地優勢も全対象の所有権確保までは後続を開けない"
    );
    assert!(
        !capital_route_node_exit_satisfied(&operation(CapitalRouteNodeScope::Point)),
        "単一Capture拠点は所有権確保まで後続を開けない"
    );
}

#[test]
fn dominant_area_capture_retains_one_escort_until_secured() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let topology = CapitalRouteTopologyKey {
        player_id: player,
        island_id: crate::ai::islands::IslandId(0),
    };
    let rear_node = CapitalRouteNodeId(1);
    let active_node = CapitalRouteNodeId(2);
    let rear_id = CapitalRouteNodeOperationId {
        topology,
        node: rear_node,
    };
    let operation = |id, state| CapitalRouteNodeOperation {
        id,
        anchor: pos(id.node.0, 2),
        predecessors: Vec::new(),
        successors: Vec::new(),
        objective: CapitalRouteNodeObjective::Capture,
        scope: CapitalRouteNodeScope::Area,
        control_area: vec![pos(id.node.0, 2)],
        capture_targets: vec![pos(id.node.0, 2)],
        crossing_exit: None,
        crossed: false,
        state,
        assigned_squads: HashSet::default(),
    };
    let mut registry = CapitalRouteNodeOperationRegistry::default();
    registry.operations.insert(
        rear_id,
        operation(rear_id, victory_roadmap::RoadmapNodeState::Dominant),
    );

    assert_eq!(
        capital_route_dominant_capture_escort_node(
            &[rear_node, active_node],
            active_node,
            &registry,
            topology,
        ),
        Some(rear_node),
        "主力が次Nodeへ進んでも未確保AreaにはControl護衛を残す"
    );

    registry
        .operations
        .get_mut(&rear_id)
        .expect("後方Capture Nodeが存在する")
        .state = victory_roadmap::RoadmapNodeState::Secured;
    assert_eq!(
        capital_route_dominant_capture_escort_node(
            &[rear_node, active_node],
            active_node,
            &registry,
            topology,
        ),
        None,
        "占領完了後は護衛も次Nodeへ解放する"
    );
}

#[test]
fn capture_frontline_reserves_both_capturer_and_control_lanes() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let island = crate::ai::islands::IslandId(0);
    let operation = |scope, control_area, capture_targets| CapitalRouteNodeOperation {
        id: CapitalRouteNodeOperationId {
            topology: CapitalRouteTopologyKey {
                player_id: player,
                island_id: island,
            },
            node: CapitalRouteNodeId(1),
        },
        anchor: pos(2, 2),
        predecessors: Vec::new(),
        successors: Vec::new(),
        objective: CapitalRouteNodeObjective::Capture,
        scope,
        control_area,
        capture_targets,
        crossing_exit: None,
        crossed: false,
        state: victory_roadmap::RoadmapNodeState::Ready,
        assigned_squads: HashSet::default(),
    };

    let point = operation(
        CapitalRouteNodeScope::Point,
        vec![pos(2, 2)],
        vec![pos(2, 2)],
    );
    assert_eq!(capital_route_role_frontline_capacity(&point, true), 1);
    assert_eq!(
        capital_route_role_frontline_capacity(&point, false),
        1,
        "一点占領でも隣接護衛をStageへ落とさない"
    );

    let area = operation(
        CapitalRouteNodeScope::Area,
        vec![pos(1, 2), pos(2, 2), pos(3, 2), pos(4, 2)],
        vec![pos(2, 2), pos(3, 2)],
    );
    assert_eq!(capital_route_role_frontline_capacity(&area, true), 2);
    assert_eq!(capital_route_role_frontline_capacity(&area, false), 2);
}

#[test]
fn nearest_capture_capable_combat_squad_can_take_the_capture_lane() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let island = crate::ai::islands::IslandId(0);
    let mut operation = CapitalRouteNodeOperation {
        id: CapitalRouteNodeOperationId {
            topology: CapitalRouteTopologyKey {
                player_id: player,
                island_id: island,
            },
            node: CapitalRouteNodeId(1),
        },
        anchor: pos(2, 2),
        predecessors: Vec::new(),
        successors: Vec::new(),
        objective: CapitalRouteNodeObjective::Capture,
        scope: CapitalRouteNodeScope::Area,
        control_area: vec![pos(2, 2), pos(3, 2)],
        capture_targets: vec![pos(3, 2)],
        crossing_exit: None,
        crossed: false,
        state: victory_roadmap::RoadmapNodeState::Ready,
        assigned_squads: HashSet::default(),
    };

    assert!(capital_route_uses_capture_lane(
        &operation,
        &MissionType::Attack,
        1,
        0,
    ));
    assert!(
        !capital_route_uses_capture_lane(&operation, &MissionType::Attack, 1, 1),
        "占領枠が埋まった後の戦闘可能歩兵はControl枠へ回す"
    );
    assert!(
        capital_route_uses_capture_lane(&operation, &MissionType::Capture, 1, 1),
        "余剰Capture SquadはControl扱いにせずCapture待機線へ置く"
    );
    assert!(!capital_route_uses_capture_lane(
        &operation,
        &MissionType::Attack,
        0,
        0,
    ));

    operation.objective = CapitalRouteNodeObjective::Control;
    assert!(!capital_route_uses_capture_lane(
        &operation,
        &MissionType::Capture,
        1,
        0,
    ));
}

#[test]
fn durable_direct_fire_squad_wins_same_frontline_escort_slot() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let armored = capital_route_frontline_sort_key(8, &MissionType::Attack, 0, 20_000, SquadId(20));
    let light = capital_route_frontline_sort_key(8, &MissionType::Attack, 0, 5_000, SquadId(10));

    assert!(
        armored < light,
        "同じ前進度なら耐久・価格・直接交戦能力が高いControl分隊を護衛へ回す"
    );
}

#[test]
fn area_control_region_excludes_remote_milestones() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let map = flat_map(30, 5);
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let anchor = pos(15, 2);
    let island = island_map.get_island_at(&anchor).unwrap().id;
    let capture_target = pos(14, 2);

    let area = capital_route_node_control_area(
        &map,
        &island_map,
        island,
        anchor,
        CapitalRouteNodeScope::Area,
        &[capture_target],
    );

    assert!(area.contains(&anchor));
    assert!(area.contains(&capture_target));
    assert!(
        !area.contains(&pos(22, 2)),
        "後方Milestoneを局地優勢へ数えない"
    );
    assert!(
        !area.contains(&pos(8, 2)),
        "後続Milestoneの敵を現在Nodeへ数えない"
    );
    assert!(area.iter().all(|position| {
        map.distance(anchor.x, anchor.y, position.x, position.y) <= CAPITAL_ROUTE_REGION_RADIUS
    }));
}

#[test]
fn hold_and_supply_remain_addressable_inside_the_route_dag() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let island = crate::ai::islands::IslandId(0);
    let hold = GridPosition { x: 4, y: 4 };
    let supply = GridPosition { x: 7, y: 4 };
    let segment_target = GridPosition { x: 9, y: 4 };
    let mut world = World::new();
    let hold_entity = world.spawn(hold).id();
    let supply_entity = world.spawn(supply).id();
    let recover_entity = world
        .spawn((
            hold,
            Health {
                current: 60,
                max: 100,
            },
        ))
        .id();
    let hold_squad = assign_route_squad(&mut world, player, MissionType::Defense, &[hold_entity]);
    let supply_squad =
        assign_route_squad(&mut world, player, MissionType::Transport, &[supply_entity]);
    let recover_squad =
        assign_route_squad(&mut world, player, MissionType::Attack, &[recover_entity]);
    world.insert_resource(CapitalRoutePathRegistry {
        commitments: HashMap::from_iter([
            (
                hold_squad,
                CapitalRouteCommitment {
                    player_id: player,
                    island_id: island,
                    route: CapitalRouteId(0),
                    target: segment_target,
                    target_node: CapitalRouteNodeId(2),
                    objective: CapitalRouteNodeObjective::Control,
                    scope: CapitalRouteNodeScope::Point,
                    control_area: vec![segment_target],
                    capture_targets: Vec::new(),
                    crossing_exit: None,
                    crossed: false,
                    execution_target: hold,
                    frontline_capacity: 1,
                    path_nodes: vec![
                        CapitalRouteNodeId(0),
                        CapitalRouteNodeId(1),
                        CapitalRouteNodeId(2),
                    ],
                    path: vec![hold, supply, segment_target],
                    phase: CapitalRouteExecutionPhase::Hold,
                },
            ),
            (
                supply_squad,
                CapitalRouteCommitment {
                    player_id: player,
                    island_id: island,
                    route: CapitalRouteId(0),
                    target: segment_target,
                    target_node: CapitalRouteNodeId(2),
                    objective: CapitalRouteNodeObjective::Control,
                    scope: CapitalRouteNodeScope::Point,
                    control_area: vec![segment_target],
                    capture_targets: Vec::new(),
                    crossing_exit: None,
                    crossed: false,
                    execution_target: supply,
                    frontline_capacity: 1,
                    path_nodes: vec![
                        CapitalRouteNodeId(0),
                        CapitalRouteNodeId(1),
                        CapitalRouteNodeId(2),
                    ],
                    path: vec![hold, supply, segment_target],
                    phase: CapitalRouteExecutionPhase::Supply,
                },
            ),
            (
                recover_squad,
                CapitalRouteCommitment {
                    player_id: player,
                    island_id: island,
                    route: CapitalRouteId(0),
                    target: segment_target,
                    target_node: CapitalRouteNodeId(2),
                    objective: CapitalRouteNodeObjective::Control,
                    scope: CapitalRouteNodeScope::Point,
                    control_area: vec![segment_target],
                    capture_targets: Vec::new(),
                    crossing_exit: None,
                    crossed: false,
                    execution_target: segment_target,
                    frontline_capacity: 1,
                    path_nodes: vec![
                        CapitalRouteNodeId(0),
                        CapitalRouteNodeId(1),
                        CapitalRouteNodeId(2),
                    ],
                    path: vec![hold, supply, segment_target],
                    phase: CapitalRouteExecutionPhase::Advance,
                },
            ),
        ]),
        ..CapitalRoutePathRegistry::default()
    });

    // 回復以外は、突撃役だけでなくDAG内の区間終点を持つ。
    assert_eq!(
        capital_route_waypoint(&world, player, hold_entity),
        Some(hold)
    );
    assert_eq!(
        capital_route_waypoint(&world, player, supply_entity),
        Some(supply)
    );
    assert_eq!(capital_route_waypoint(&world, player, recover_entity), None);
    assert!(!capital_route_is_recovering(&world, player, hold_entity));
    assert!(capital_route_is_recovering(&world, player, recover_entity));
}

#[test]
fn route_order_is_shared_by_members_of_the_same_squad() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let island = crate::ai::islands::IslandId(0);
    let target = pos(6, 1);
    let mut world = World::new();
    let first = world.spawn(pos(1, 1)).id();
    let second = world.spawn(pos(1, 1)).id();
    let unassigned = world.spawn_empty().id();
    let squad_id = assign_route_squad(&mut world, player, MissionType::Attack, &[first, second]);
    world.insert_resource(CapitalRoutePathRegistry {
        commitments: HashMap::from_iter([(
            squad_id,
            CapitalRouteCommitment {
                player_id: player,
                island_id: island,
                route: CapitalRouteId(0),
                target,
                target_node: CapitalRouteNodeId(1),
                objective: CapitalRouteNodeObjective::Control,
                scope: CapitalRouteNodeScope::Point,
                control_area: vec![target],
                capture_targets: Vec::new(),
                crossing_exit: None,
                crossed: false,
                execution_target: target,
                frontline_capacity: 1,
                path_nodes: vec![CapitalRouteNodeId(0), CapitalRouteNodeId(1)],
                path: vec![pos(1, 1), target],
                phase: CapitalRouteExecutionPhase::Advance,
            },
        )]),
        ..CapitalRoutePathRegistry::default()
    });

    assert_eq!(capital_route_waypoint(&world, player, first), Some(target));
    assert_eq!(capital_route_waypoint(&world, player, second), Some(target));
    assert_eq!(capital_route_waypoint(&world, player, unassigned), None);
}

#[test]
fn only_capturer_claims_an_unsecured_route_milestone() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let island = crate::ai::islands::IslandId(0);
    let target = pos(5, 1);
    let mut world = World::new();
    let infantry = world
        .spawn(UnitStats {
            unit_type: UnitType::Infantry,
            can_capture: true,
            ..UnitStats::mock()
        })
        .id();
    let tank = world
        .spawn(UnitStats {
            unit_type: UnitType::Tank,
            movement_type: MovementType::Tank,
            can_capture: false,
            ..UnitStats::mock()
        })
        .id();
    let infantry_squad = assign_route_squad(&mut world, player, MissionType::Capture, &[infantry]);
    let tank_squad = assign_route_squad(&mut world, player, MissionType::Attack, &[tank]);
    let commitment = |route| CapitalRouteCommitment {
        player_id: player,
        island_id: island,
        route: CapitalRouteId(route),
        target,
        target_node: CapitalRouteNodeId(1),
        objective: CapitalRouteNodeObjective::Capture,
        scope: CapitalRouteNodeScope::Point,
        control_area: vec![target],
        capture_targets: vec![target],
        crossing_exit: None,
        crossed: false,
        execution_target: target,
        frontline_capacity: 1,
        path_nodes: vec![CapitalRouteNodeId(0), CapitalRouteNodeId(1)],
        path: vec![pos(1, 1), target],
        phase: CapitalRouteExecutionPhase::Advance,
    };
    world.insert_resource(CapitalRoutePathRegistry {
        commitments: HashMap::from_iter([
            (infantry_squad, commitment(0)),
            (tank_squad, commitment(1)),
        ]),
        ..CapitalRoutePathRegistry::default()
    });

    assert!(capital_route_allows_capture_at(
        &world, player, infantry, target
    ));
    assert!(
        !capital_route_allows_capture_at(&world, player, infantry, pos(4, 1)),
        "Capture Nodeは回廊外の拠点を占領させない"
    );
    assert!(
        !capital_route_allows_capture_at(&world, player, tank, target),
        "占領能力のない戦車はCapture Nodeの担当にならない"
    );

    // 待機線の歩兵は同じCapture作戦に所属し続けるが、前衛枠が空くまで占領を始めない。
    world
        .resource_mut::<CapitalRoutePathRegistry>()
        .commitments
        .get_mut(&infantry_squad)
        .expect("歩兵SquadのDAG所属を保持する")
        .phase = CapitalRouteExecutionPhase::Stage;
    assert!(
        !capital_route_allows_capture_at(&world, player, infantry, target),
        "待機線のCapture Squadは前衛Nodeを先取りしない"
    );

    // 局地交戦でHoldへ移っても、横の拠点を占領してDAG進行を逸らしてはならない。
    world
        .resource_mut::<CapitalRoutePathRegistry>()
        .commitments
        .get_mut(&infantry_squad)
        .expect("歩兵SquadのDAG所属を保持する")
        .phase = CapitalRouteExecutionPhase::Hold;
    assert!(
        capital_route_allows_capture_at(&world, player, infantry, target),
        "Holdへ切り替わっても、Capture Nodeの占領対象だけは維持する"
    );
}

#[test]
fn squad_mission_alone_defines_route_execution_phase() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    assert_eq!(
        capital_route_execution_phase(&MissionType::Attack),
        CapitalRouteExecutionPhase::Advance
    );
    assert_eq!(
        capital_route_execution_phase(&MissionType::Defense),
        CapitalRouteExecutionPhase::Hold,
        "Defense SquadはDAG orderを保持する"
    );
    assert_eq!(
        capital_route_execution_phase(&MissionType::Transport),
        CapitalRouteExecutionPhase::Supply
    );
}

#[test]
fn forward_defense_squad_is_reassigned_as_current_node_control() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    assert!(capital_route_controls_active_node(
        &MissionType::Attack,
        false
    ));
    assert!(capital_route_controls_active_node(
        &MissionType::Defense,
        true
    ));
    assert!(
        !capital_route_controls_active_node(&MissionType::Defense, false),
        "本土側だけを指定するDefense Squadは現在Nodeから動かさない"
    );
    assert!(!capital_route_controls_active_node(
        &MissionType::Capture,
        true
    ));
}

#[test]
fn capture_outside_the_active_route_keeps_its_squad_order() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    // map_25の外周拠点のように、前方にあっても現在DAG経路に
    // 属さない目標はfalseとし、中央最短線へ上書きしない。
    assert!(!capital_route_capture_joins_active_node(
        &MissionType::Capture,
        true,
        false,
        false,
    ));
    assert!(capital_route_capture_joins_active_node(
        &MissionType::Capture,
        true,
        true,
        false,
    ));
    assert!(!capital_route_capture_joins_active_node(
        &MissionType::Capture,
        true,
        true,
        true,
    ));
    assert!(capital_route_capture_joins_active_node(
        &MissionType::Capture,
        false,
        false,
        true,
    ));
    assert!(capital_route_capture_joins_active_node(
        &MissionType::Attack,
        true,
        false,
        true,
    ));
}

#[test]
fn unfinished_capture_node_reserves_property_cells_for_capturers() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let topology = CapitalRouteTopologyKey {
        player_id: player,
        island_id: crate::ai::islands::IslandId(0),
    };
    let operation_id = CapitalRouteNodeOperationId {
        topology,
        node: CapitalRouteNodeId(1),
    };
    let target = pos(10, 11);
    let mut registry = CapitalRouteNodeOperationRegistry::default();
    registry.operations.insert(
        operation_id,
        CapitalRouteNodeOperation {
            id: operation_id,
            anchor: pos(8, 11),
            predecessors: Vec::new(),
            successors: Vec::new(),
            objective: CapitalRouteNodeObjective::Capture,
            scope: CapitalRouteNodeScope::Area,
            control_area: vec![pos(8, 11), target],
            capture_targets: vec![target],
            crossing_exit: None,
            crossed: false,
            state: victory_roadmap::RoadmapNodeState::Capturing,
            assigned_squads: HashSet::default(),
        },
    );

    assert!(capital_route_capture_target_is_reserved(
        &registry, player, target,
    ));
    registry
        .operations
        .get_mut(&operation_id)
        .expect("テスト対象Nodeが存在する")
        .state = victory_roadmap::RoadmapNodeState::Secured;
    assert!(!capital_route_capture_target_is_reserved(
        &registry, player, target,
    ));
}

#[test]
fn capital_mainline_depends_on_forward_progress_not_auxiliary_property_count() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    assert!(same_land_capital_front_reached(16, 6));
    assert!(same_land_capital_front_reached(32, 16));
    assert!(!same_land_capital_front_reached(32, 17));
}

#[test]
fn bridge_gates_split_one_island_into_two_active_route_fronts() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let master_data = MasterDataRegistry::load().unwrap();
    let mut map = flat_map(7, 7);
    for y in 0..map.height {
        map.set_terrain(3, y, Terrain::River).unwrap();
    }
    map.set_terrain(3, 1, Terrain::Bridge).unwrap();
    map.set_terrain(3, 5, Terrain::Bridge).unwrap();
    let gates = armored_entry_bridge_gates(&map, &master_data, pos(0, 3), pos(6, 3));
    assert_eq!(gates, vec![pos(3, 1), pos(3, 5)]);

    let objective = CampaignPlanningObjective {
        island_id: crate::ai::islands::IslandId(0),
        kind: OperationKind::Capture,
        anchor: pos(2, 1),
        objective_properties: vec![pos(2, 1), pos(4, 1), pos(2, 5), pos(4, 5)],
        capture_eta: Some(4),
        required_capture_survivors: 5,
        logistics_rank: None,
        forced_target_enemies: HashSet::default(),
        protected_capture_entities: HashSet::default(),
        capture_replacement_pending: false,
        staging_anchor: pos(2, 1),
        execution_authorized: true,
    };
    let targets = HashMap::default();
    let routes = split_bridge_route_objectives(&map, vec![objective], &gates, &[], &targets);

    assert_eq!(routes.len(), 2);
    assert_eq!(
        routes
            .iter()
            .map(|route| route.required_capture_survivors)
            .sum::<usize>(),
        5
    );
    assert_eq!(routes[0].anchor.y, 1);
    assert_eq!(routes[1].anchor.y, 5);
    assert!(
        routes
            .iter()
            .all(|route| route.objective_properties.len() == 2)
    );
}

#[test]
fn route_milestone_requires_the_nearest_unsecured_property_before_advancing() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    use crate::ai::island_campaign::{
        IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignPortfolio,
        IslandCampaignRequirement,
    };

    let master_data = MasterDataRegistry::load().unwrap();
    let mut map = flat_map(7, 7);
    for y in 0..map.height {
        map.set_terrain(3, y, Terrain::River).unwrap();
    }
    map.set_terrain(3, 1, Terrain::Bridge).unwrap();
    map.set_terrain(3, 5, Terrain::Bridge).unwrap();
    map.set_terrain(0, 3, Terrain::Capital).unwrap();
    map.set_terrain(6, 3, Terrain::Capital).unwrap();
    for target in [pos(2, 1), pos(4, 1), pos(2, 5), pos(4, 5)] {
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
    }
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let island_id = island_map.get_island_at(&pos(0, 3)).unwrap().id;
    let player = PlayerId(1);
    let enemy = PlayerId(2);
    let mut world = World::new();
    world.insert_resource(map);
    world.insert_resource(master_data);
    world.insert_resource(island_map);
    world.spawn((
        pos(0, 3),
        Property::new(Terrain::Capital, Some(player), 100),
    ));
    world.spawn((pos(6, 3), Property::new(Terrain::Capital, Some(enemy), 100)));
    let first_north = world
        .spawn((pos(2, 1), Property::new(Terrain::City, None, 100)))
        .id();
    world.spawn((pos(4, 1), Property::new(Terrain::City, None, 100)));
    let first_south = world
        .spawn((pos(2, 5), Property::new(Terrain::City, None, 100)))
        .id();
    world.spawn((pos(4, 5), Property::new(Terrain::City, None, 100)));
    prepare_capital_route_topologies(&mut world, player);
    assert!(
        !capital_route_assault_authorized_from_dag(&world, player, island_id)
            .expect("二回廊DAGの初期Go判定"),
        "最初の橋頭堡を確保する前に、首都作戦だけを実行へ移さない"
    );
    let requirement = IslandCampaignRequirement {
        preferred_transport: None,
        transport_slots: 0,
        capture_units: 2,
        ground_combat_units: 0,
        combat_units: 0,
        total_budget: 2_000,
    };
    let mut portfolio = IslandCampaignPortfolio {
        active_offensives: vec![IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Contest,
            target_position: pos(3, 3),
            capture_target_positions: vec![pos(3, 3)],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: requirement,
            allocated_budget: 0,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: false,
        }],
        ..IslandCampaignPortfolio::default()
    };

    refine_same_land_route_milestones(&world, player, &mut portfolio);
    assert_eq!(
        portfolio.active_offensives[0].capture_target_positions,
        vec![pos(2, 1), pos(2, 5)],
        "橋の通過だけを優先せず、各枝で最初に未確保の施設をMilestoneにする"
    );

    // 提案で精錬した入力は採用の正本。採用中に再び盤面を読んで精錬し直さない。
    world.insert_resource(
        crate::ai::strategy_profile::V4StrategyProfileRegistry::with_profile(
            player,
            crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion,
        ),
    );
    let mut prepared = portfolio.clone();
    prepared.active_offensives[0].continued_from_existing_squad = true;
    crate::ai::v4::strategy_pipeline::prepare_strategy(&mut world, player, &mut prepared);
    let prepared_targets = prepared.active_offensives[0]
        .capture_target_positions
        .clone();
    world.get_mut::<Property>(first_north).unwrap().owner_id = Some(player);
    world.get_mut::<Property>(first_south).unwrap().owner_id = Some(player);
    let adopted = crate::ai::v4::strategy_pipeline::portfolio_for_squads(
        &mut world,
        player,
        0,
        prepared,
        &crate::ai::squad::SquadManager::new(),
    );
    assert_eq!(adopted.active_offensives.len(), 1, "継続中の作戦は採用する");
    assert_eq!(
        adopted.active_offensives[0].capture_target_positions, prepared_targets,
        "採用は既に精錬した提案を使い、盤面更新後の次Milestoneへ精錬し直さない"
    );
    assert!(
        capital_route_assault_authorized_from_dag(&world, player, island_id)
            .expect("前方区間を確保した後のDAG Go判定"),
        "DAGの両枝で先頭拠点を確保したら、旧来の側方都市数ではなく前方区間の進捗でGoする"
    );
    refine_same_land_route_milestones(&world, player, &mut portfolio);
    assert_eq!(
        portfolio.active_offensives[0].capture_target_positions,
        vec![pos(4, 1), pos(4, 5)],
        "手前の施設を確保した枝だけ、次の橋向こうの施設へ進める"
    );
}

#[test]
fn map6_mountain_corridors_create_two_route_fronts() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let master_data = MasterDataRegistry::load().unwrap();
    let (world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
        &master_data,
        "map_6",
        GridTopology::Square,
    )
    .expect("map_6を初期化できる");
    let player = PlayerId(1);
    let own_capital = world
        .iter_entities()
        .find_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                .then_some(*position)
        })
        .expect("自首都");
    let island = world
        .resource::<crate::ai::islands::IslandMap>()
        .get_island_at(&own_capital)
        .expect("首都の島")
        .id;

    let fronts = same_land_route_fronts(&world, player, island);

    assert_eq!(
        fronts.len(),
        2,
        "山で分かれるmap_6を単一前線へ潰さない: fronts={fronts:?}"
    );
    assert!(fronts[0].y < fronts[1].y, "北・南の別回廊を前線として得る");

    let p2 = PlayerId(2);
    let p2_fronts = same_land_route_fronts(&world, p2, island);
    assert_eq!(
        p2_fronts.len(),
        2,
        "後攻P2でもmap_6は2本のルート前線を得る: p2_fronts={p2_fronts:?}"
    );
}

#[test]
fn map6_p2_capital_route_discovers_both_frontiers() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let master_data = MasterDataRegistry::load().unwrap();
    let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
        &master_data,
        "map_6",
        GridTopology::Square,
    )
    .unwrap();
    let player = PlayerId(2);
    let own_capital = world
        .iter_entities()
        .find_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                .then_some(*position)
        })
        .expect("自首都");
    let island = world
        .resource::<crate::ai::islands::IslandMap>()
        .get_island_at(&own_capital)
        .expect("首都の島")
        .id;

    prepare_capital_route_topologies(&mut world, player);
    let topology =
        capital_route_topology_for(&world, player, island).expect("P2の首都戦役DAGを構築できる");
    assert_eq!(topology.routes.len(), 2, "map_6の2進軍軸を保持する");

    let frontiers = capital_route_dag_frontier_properties(&world, player, island)
        .expect("両ルートの拠点を検出できる");
    assert_eq!(frontiers.len(), 2);
    assert!(!frontiers[0].is_empty(), "Route 0の拠点を検出する");
    assert!(!frontiers[1].is_empty(), "Route 1の拠点を検出する");
}

#[test]
fn map1_single_corridor_still_builds_a_capital_route_dag() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let master_data = MasterDataRegistry::load().unwrap();
    let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
        &master_data,
        "map_1",
        GridTopology::Square,
    )
    .expect("map_1を初期化できる");
    let player = PlayerId(1);
    let own_capital = world
        .iter_entities()
        .find_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                .then_some(*position)
        })
        .expect("自首都");
    let island = world
        .resource::<crate::ai::islands::IslandMap>()
        .get_island_at(&own_capital)
        .expect("首都の島")
        .id;

    prepare_capital_route_topologies(&mut world, player);
    let topology = capital_route_topology_for(&world, player, island)
        .expect("単一路でも首都戦役DAGを構築する");
    assert_eq!(
        topology.routes.len(),
        1,
        "map_1を複数routeへ水増しせず、一本の首都進軍路として管理する"
    );
    let start_node = topology.start_node;
    let manager = crate::ai::squad::SquadManager::default();
    refresh_capital_route_node_operations(&mut world, player, &manager);
    let node_operations = world.resource::<CapitalRouteNodeOperationRegistry>();
    let start_operation = node_operations
        .operations
        .get(&CapitalRouteNodeOperationId {
            topology: CapitalRouteTopologyKey {
                player_id: player,
                island_id: island,
            },
            node: start_node,
        })
        .expect("静的DAGの開始Nodeを作戦台帳へ展開する");
    assert_eq!(
        start_operation.state,
        victory_roadmap::RoadmapNodeState::Secured,
        "自首都を始点Nodeとして盤面投影する"
    );
}

/// 戦略解析の首都攻略DAG準備hookは拡張経路のV4だけが呼ぶ（F03の経路差を固定する）。
/// 同じ盤面・同じプロファイルでも、通常経路の戦略解析はDAGトポロジを作らない。
#[test]
fn strategy_analysis_prepares_capital_route_dag_only_on_extended_path() {
    let master_data = MasterDataRegistry::load().unwrap();
    let player = PlayerId(1);
    for (path, builds_dag) in [
        (crate::ai::code_path::CodePath::MainCompatible, false),
        (crate::ai::code_path::CodePath::Extended, true),
    ] {
        let _guard = crate::ai::code_path::enter(path);
        let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_26",
            GridTopology::Square,
        )
        .expect("map_26を初期化できる");
        let mut settings = crate::ai::ai_version::PlayerAiSettings::new();
        settings.set_version(player, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(settings);
        // DAG準備hookは小規模マップ展開のプロファイルでだけ動くため、明示的に確定する。
        world.insert_resource(
            crate::ai::strategy_profile::V4StrategyProfileRegistry::with_profile(
                player,
                crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion,
            ),
        );

        crate::ai::strategy::analyze_strategy(&mut world, player);

        let has_dag = world
            .get_resource::<CapitalRouteTopologyRegistry>()
            .is_some_and(|registry| {
                registry
                    .topologies
                    .keys()
                    .any(|key| key.player_id == player)
            });
        assert_eq!(has_dag, builds_dag, "{path:?}");
    }
}

#[test]
fn map26_builds_a_forward_dag_independently_for_each_player() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let master_data = MasterDataRegistry::load().unwrap();
    let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
        &master_data,
        "map_26",
        GridTopology::Square,
    )
    .expect("map_26を初期化できる");
    let player_one = PlayerId(1);
    let player_two = PlayerId(2);
    let capital_for = |world: &World, player| {
        world
            .iter_entities()
            .find_map(|entity| {
                let position = entity.get::<GridPosition>()?;
                let property = entity.get::<Property>()?;
                (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                    .then_some(*position)
            })
            .expect("各勢力の首都")
    };
    let player_one_capital = capital_for(&world, player_one);
    let player_two_capital = capital_for(&world, player_two);
    let island_id = world
        .resource::<crate::ai::islands::IslandMap>()
        .get_island_at(&player_one_capital)
        .expect("首都の島")
        .id;

    prepare_capital_route_topologies(&mut world, player_one);
    prepare_capital_route_topologies(&mut world, player_two);

    let registry = world.resource::<CapitalRouteTopologyRegistry>();
    let first = registry
        .topologies
        .get(&CapitalRouteTopologyKey {
            player_id: player_one,
            island_id,
        })
        .expect("先攻のDAG");
    let second = registry
        .topologies
        .get(&CapitalRouteTopologyKey {
            player_id: player_two,
            island_id,
        })
        .expect("後攻のDAG");
    assert_eq!(first.nodes[first.start_node.0].anchor, player_one_capital);
    assert_eq!(second.nodes[second.start_node.0].anchor, player_two_capital);
    for topology in [first, second] {
        assert!(topology.routes.len() >= 2, "橋の北・南を別の入口として持つ");
        assert!(topology.nodes.iter().all(|node| {
            node.successor_nodes
                .iter()
                .all(|successor| topology.nodes[successor.0].progress > node.progress)
        }));
        let mut inbound = vec![0_u32; topology.nodes.len()];
        for node in &topology.nodes {
            for successor in &node.successor_nodes {
                inbound[successor.0] = inbound[successor.0].saturating_add(1);
            }
        }
        assert!(
            inbound.iter().any(|count| *count >= 2),
            "分岐した橋回廊が後段で合流するDAGになっている"
        );
    }
}

#[test]
fn map26_bridge_node_requires_a_ground_unit_to_reach_its_exit() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let master_data = MasterDataRegistry::load().unwrap();
    let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
        &master_data,
        "map_26",
        GridTopology::Square,
    )
    .expect("map_26を初期化できる");
    let player = PlayerId(1);
    let island_id = world
        .iter_entities()
        .find_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                .then_some(*position)
        })
        .and_then(|capital| {
            world
                .resource::<crate::ai::islands::IslandMap>()
                .get_island_at(&capital)
                .map(|island| island.id)
        })
        .expect("先攻首都の島");
    prepare_capital_route_topologies(&mut world, player);
    ensure_capital_route_node_operations(&mut world, player);

    let topology_key = CapitalRouteTopologyKey {
        player_id: player,
        island_id,
    };
    let topology = world
        .resource::<CapitalRouteTopologyRegistry>()
        .topologies
        .get(&topology_key)
        .cloned()
        .expect("先攻の首都攻略DAG");
    let operation_count = world
        .resource::<CapitalRouteNodeOperationRegistry>()
        .operations
        .values()
        .filter(|operation| operation.id.topology == topology_key)
        .count();
    assert!(
        operation_count < topology.nodes.len(),
        "移動用の全セルを作戦Nodeへ昇格させず、地形・拠点Milestoneへ圧縮する: {operation_count}/{}",
        topology.nodes.len()
    );
    let joined_operation = world
        .resource::<CapitalRouteNodeOperationRegistry>()
        .operations
        .values()
        .find(|operation| {
            operation.id.topology == topology_key && operation.predecessors.len() >= 2
        })
        .cloned()
        .expect("map_26の圧縮DAGには代替回廊が合流するMilestoneがある");
    let mut simulated_operations = world
        .resource::<CapitalRouteNodeOperationRegistry>()
        .operations
        .clone();
    for predecessor in &joined_operation.predecessors {
        simulated_operations
            .get_mut(predecessor)
            .expect("合流前Node")
            .state = victory_roadmap::RoadmapNodeState::Locked;
    }
    simulated_operations
        .get_mut(&joined_operation.predecessors[0])
        .expect("選択した経路の前Node")
        .state = victory_roadmap::RoadmapNodeState::Secured;
    assert!(
        capital_route_node_predecessors_released(&simulated_operations, &joined_operation),
        "地理的な合流は全前任Nodeの同時制圧を要求せず、到達済みの一経路を使う"
    );
    let bridge_operation = world
        .resource::<CapitalRouteNodeOperationRegistry>()
        .operations
        .values()
        .filter(|operation| operation.id.topology == topology_key)
        .filter(|operation| operation.crossing_exit.is_some())
        .min_by_key(|operation| topology.nodes[operation.id.node.0].progress)
        .cloned()
        .expect("map_26の先攻DAGには橋を越えるNodeがある");
    assert!(
        world
            .resource::<CapitalRouteNodeOperationRegistry>()
            .operations
            .values()
            .any(|operation| {
                operation.id.topology == topology_key
                    && operation.scope == CapitalRouteNodeScope::Area
            }),
        "map_26の分岐/合流Nodeは地形スキャンでAreaとして分類する"
    );
    assert_eq!(
        bridge_operation.scope,
        CapitalRouteNodeScope::Point,
        "橋は局地Areaではなく、出口へ通過すべきPoint Nodeとして分類する"
    );
    let crossing_exit = bridge_operation.crossing_exit.expect("橋向こうの出口");
    let bridge_progress = topology.nodes[bridge_operation.id.node.0].progress;

    // 前段を確保済み・優勢にして、橋Nodeだけの通過条件を単独で観測できるようにする。
    // 実際の対局ではこの状態は前段NodeのCapture/Control完了後に得られる。
    let property_entities = world
        .iter_entities()
        .filter_map(|entity| entity.contains::<Property>().then_some(entity.id()))
        .collect::<Vec<_>>();
    for property in property_entities {
        world
            .entity_mut(property)
            .get_mut::<Property>()
            .expect("Propertyを持つEntity")
            .owner_id = Some(player);
    }
    for node in topology
        .nodes
        .iter()
        .filter(|node| node.progress < bridge_progress)
    {
        world.spawn((
            Faction(player),
            node.anchor,
            Health {
                current: 100,
                max: 100,
            },
            UnitStats {
                cost: 100_000,
                movement_type: MovementType::Tank,
                ..UnitStats::mock()
            },
        ));
    }
    let manager = crate::ai::squad::SquadManager::default();
    refresh_capital_route_node_operations(&mut world, player, &manager);
    let bridge_id = bridge_operation.id;
    let operation = world
        .resource::<CapitalRouteNodeOperationRegistry>()
        .operations
        .get(&bridge_id)
        .expect("橋Nodeの状態");
    assert!(
        !operation.crossed && !capital_route_node_exit_satisfied(operation),
        "橋の手前まで前段を制圧しても、出口へ出るまではNodeを完了させない: {:?}",
        operation.state
    );

    // 橋の上に立っても未通過である。入口・橋上・出口を混同しない。
    world.spawn((
        Faction(player),
        bridge_operation.anchor,
        Health {
            current: 100,
            max: 100,
        },
        UnitStats {
            cost: 100_000,
            movement_type: MovementType::Tank,
            ..UnitStats::mock()
        },
    ));
    refresh_capital_route_node_operations(&mut world, player, &manager);
    let operation = world
        .resource::<CapitalRouteNodeOperationRegistry>()
        .operations
        .get(&bridge_id)
        .expect("橋上到着後のNode状態");
    assert!(
        !operation.crossed && !capital_route_node_exit_satisfied(operation),
        "橋上への到着だけではmap_26の通過Milestoneを閉じない: {:?}",
        operation.state
    );

    // 初めて向こう岸の非橋セルへ出た時点で、Control Nodeを後続Nodeへ解放できる。
    world.spawn((
        Faction(player),
        crossing_exit,
        Health {
            current: 100,
            max: 100,
        },
        UnitStats {
            cost: 100_000,
            movement_type: MovementType::Tank,
            ..UnitStats::mock()
        },
    ));
    refresh_capital_route_node_operations(&mut world, player, &manager);
    let operation = world
        .resource::<CapitalRouteNodeOperationRegistry>()
        .operations
        .get(&bridge_id)
        .expect("橋頭堡到達後のNode状態");
    assert!(operation.crossed, "出口セルへの地上部隊到達を記録する");
    assert!(
        capital_route_node_exit_satisfied(operation),
        "橋頭堡を得たControl Nodeだけが後続Nodeを解放できる: {:?}",
        operation.state
    );
}

#[test]
fn map6_mountain_routes_keep_a_milestone_on_each_corridor() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let master_data = MasterDataRegistry::load().unwrap();
    let (world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
        &master_data,
        "map_6",
        GridTopology::Square,
    )
    .expect("map_6を初期化できる");
    let player = PlayerId(1);
    let own_capital = world
        .iter_entities()
        .find_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                .then_some(*position)
        })
        .expect("自首都");
    let island_id = world
        .resource::<crate::ai::islands::IslandMap>()
        .get_island_at(&own_capital)
        .expect("首都の島")
        .id;
    let requirement = crate::ai::island_campaign::IslandCampaignRequirement {
        preferred_transport: None,
        transport_slots: 0,
        capture_units: 4,
        ground_combat_units: 0,
        combat_units: 0,
        total_budget: 4_000,
    };
    let assignment = crate::ai::island_campaign::IslandCampaignAssignment {
        island_id,
        decision: crate::ai::island_campaign::IslandCampaignDecision::Secure,
        target_position: own_capital,
        capture_target_positions: vec![own_capital],
        priority_enemy_types: Vec::new(),
        requirement: requirement.clone(),
        purchase_shortfall: requirement,
        allocated_budget: 4_000,
        transport_entities: Vec::new(),
        capture_entities: Vec::new(),
        combat_entities: Vec::new(),
        operation_ready: false,
        continued_from_existing_squad: false,
    };
    let mut portfolio = crate::ai::island_campaign::IslandCampaignPortfolio {
        active_offensives: vec![assignment],
        ..crate::ai::island_campaign::IslandCampaignPortfolio::default()
    };

    refine_same_land_route_milestones(&world, player, &mut portfolio);

    let targets = &portfolio.active_offensives[0].capture_target_positions;
    let fronts = same_land_route_fronts(&world, player, island_id);
    let represented_routes = targets
        .iter()
        .filter_map(|target| {
            fronts
                .iter()
                .enumerate()
                .min_by_key(|(route, front)| {
                    (
                        world
                            .resource::<Map>()
                            .distance(target.x, target.y, front.x, front.y),
                        *route,
                    )
                })
                .map(|(route, _)| route)
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        represented_routes.len(),
        2,
        "山の二回廊をロードマップへ残し、片側の占領目標だけでcampaignを上書きしない: {targets:?}"
    );
}

#[test]
fn route_force_slots_follow_bulk_demand_without_minimum_per_route() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let north = RouteForceDemand {
        opportunity_value: 6_000,
        force_deficit_value: 3_000,
        demand_value: 9_000,
    };
    let south = RouteForceDemand {
        opportunity_value: 2_000,
        force_deficit_value: 1_000,
        demand_value: 3_000,
    };
    assert_eq!(apportion_route_force_slots(6, &[north, south]), vec![5, 1]);

    let dormant = RouteForceDemand {
        opportunity_value: 0,
        force_deficit_value: 0,
        demand_value: 0,
    };
    assert_eq!(
        apportion_route_force_slots(6, &[dormant, north]),
        vec![0, 6],
        "需要ゼロのrouteへ最低一枠を強制しない"
    );
}

#[test]
fn map26_bridge_route_fronts_have_post_bridge_positions() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let master_data = MasterDataRegistry::load().unwrap();
    let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
        &master_data,
        "map_26",
        GridTopology::Square,
    )
    .expect("map_26 initialization should succeed");
    let player = PlayerId(1);
    let own_capital = world
        .iter_entities()
        .find_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                .then_some(*position)
        })
        .expect("own capital");
    let island = world
        .resource::<crate::ai::islands::IslandMap>()
        .get_island_at(&own_capital)
        .expect("capital island")
        .id;

    let gates = same_land_route_fronts(&world, player, island);
    let bridgeheads = same_land_route_breakthrough_positions(&world, player, island);

    assert_eq!(gates.len(), 2, "map_26 gates={gates:?}");
    assert_eq!(bridgeheads.len(), 2, "map_26 bridgeheads={bridgeheads:?}");
    let distances = armored_step_distances(world.resource::<Map>(), &master_data, own_capital);
    for (gate, bridgehead) in gates.iter().zip(&bridgeheads) {
        assert!(
            distances[bridgehead] > distances[gate],
            "gate={gate:?}, bridgehead={bridgehead:?}"
        );
    }

    assert_eq!(
        same_land_pending_bridgeheads(&world, player, island),
        bridgeheads.iter().copied().map(Some).collect::<Vec<_>>(),
        "開始時点では橋向こうへ地上部隊がいないため、両軸とも突破未了"
    );
    assert_eq!(
        same_land_armored_entry_shortfall(&world, player, island),
        0,
        "占領vanguardが遠い初手は、橋突破tankより収入・占領を先に進める"
    );
    world.spawn((
        Faction(player),
        gates[0],
        UnitStats {
            unit_type: UnitType::Infantry,
            movement_type: MovementType::Infantry,
            can_capture: true,
            max_movement: 3,
            ..UnitStats::mock()
        },
    ));
    assert_eq!(
        same_land_armored_entry_shortfall(&world, player, island),
        1,
        "一体のvanguardを二つのrouteへ二重計上しない"
    );
    world.spawn((
        Faction(player),
        gates[1],
        UnitStats {
            unit_type: UnitType::Infantry,
            movement_type: MovementType::Infantry,
            can_capture: true,
            max_movement: 3,
            ..UnitStats::mock()
        },
    ));
    assert_eq!(
        same_land_armored_entry_shortfall(&world, player, island),
        2,
        "各routeの占領vanguardがtank生産より先着する時だけ突破要求を開く"
    );
    world.spawn((
        Faction(player),
        bridgeheads[0],
        UnitStats {
            unit_type: UnitType::Tank,
            movement_type: MovementType::Tank,
            ..UnitStats::mock()
        },
    ));
    assert_eq!(
        same_land_pending_bridgeheads(&world, player, island),
        vec![None, Some(bridgeheads[1])],
        "片方の橋頭堡へ地上部隊が入れば、そのルートだけ通過Milestoneを閉じる"
    );
    assert_eq!(
        same_land_armored_entry_shortfall(&world, player, island),
        1,
        "突破済みrouteは要求から消え、残る未突破routeだけを生産対象にする"
    );
}

#[test]
fn advancing_route_registry_exposes_the_current_segment_roster() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let island = crate::ai::islands::IslandId(4);
    let advancing = Entity::from_raw(904);
    let staged = Entity::from_raw(907);
    let holding = Entity::from_raw(905);
    let supplying = Entity::from_raw(906);
    let mut manager = crate::ai::squad::SquadManager::default();
    let mut advancing_squad = manager.create_owned_squad(MissionType::Attack, player);
    advancing_squad.members.insert(advancing);
    let advancing_squad_id = advancing_squad.id;
    let mut staged_squad = manager.create_owned_squad(MissionType::Attack, player);
    staged_squad.members.insert(staged);
    let staged_squad_id = staged_squad.id;
    let mut holding_squad = manager.create_owned_squad(MissionType::Defense, player);
    holding_squad.members.insert(holding);
    let holding_squad_id = holding_squad.id;
    let mut supplying_squad = manager.create_owned_squad(MissionType::Transport, player);
    supplying_squad.members.insert(supplying);
    let supplying_squad_id = supplying_squad.id;
    let commitment = |phase| CapitalRouteCommitment {
        player_id: player,
        island_id: island,
        route: CapitalRouteId(0),
        target: pos(5, 1),
        target_node: CapitalRouteNodeId(1),
        objective: CapitalRouteNodeObjective::Control,
        scope: CapitalRouteNodeScope::Point,
        control_area: vec![pos(5, 1)],
        capture_targets: Vec::new(),
        crossing_exit: None,
        crossed: false,
        execution_target: pos(5, 1),
        frontline_capacity: 1,
        path_nodes: vec![CapitalRouteNodeId(0), CapitalRouteNodeId(1)],
        path: vec![pos(0, 1), pos(5, 1)],
        phase,
    };
    let registry = CapitalRoutePathRegistry {
        commitments: HashMap::from_iter([
            (
                advancing_squad_id,
                commitment(CapitalRouteExecutionPhase::Advance),
            ),
            (
                staged_squad_id,
                commitment(CapitalRouteExecutionPhase::Stage),
            ),
            (
                holding_squad_id,
                commitment(CapitalRouteExecutionPhase::Hold),
            ),
            (
                supplying_squad_id,
                commitment(CapitalRouteExecutionPhase::Supply),
            ),
        ]),
        squads_by_operation: HashMap::default(),
        assignment_diagnostics: HashMap::default(),
    };

    let advancing_entities = registry.advancing_combat_entities(player, &manager);
    assert_eq!(
        advancing_entities.by_island,
        HashMap::from_iter([(island, HashSet::from_iter([advancing]))]),
        "保持・補給Squadを見かけの攻略戦力にしない"
    );
    assert_eq!(
        advancing_entities.by_target,
        HashMap::from_iter([((island, pos(5, 1)), HashSet::from_iter([advancing]))]),
        "前進部隊は首都全体ではなく、現在のDAG区間目標へ渡す"
    );
    assert_eq!(
        advancing_entities.staged_by_target,
        HashMap::from_iter([((island, pos(5, 1)), HashSet::from_iter([staged]))]),
        "待機線も人数だけでなく、同じDAG区間へ向かうEntityとして保持する"
    );
}

#[test]
fn staged_route_member_is_counted_after_its_front_arrival_eta() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut scan = mixed_threat_multi_factory_scan();
    let staged = Entity::from_raw(903);
    let anti_air = scan
        .available_types
        .iter()
        .find(|(unit_type, _)| *unit_type == UnitType::AntiAir)
        .map(|(_, stats)| stats.clone())
        .expect("fixture has an anti-air counter");
    scan.my_units.push(UnitSnapshot {
        entity: Some(staged),
        pos: pos(1, 1),
        stats: anti_air,
        hp: 100,
        free_cargo: 0,
    });
    let mut ctx = ReachCtx::default();
    let operations = build_operations(
        &scan,
        &mut ctx,
        &[],
        PlayerId(1),
        ProductionPolicy::SmallMap,
    );
    let operation = operations
        .iter()
        .find(|operation| operation.kind == OperationKind::Capture)
        .expect("capture operation");

    let input = combat_plan_input(
        &scan,
        &mut ctx,
        operation,
        &HashSet::default(),
        &HashSet::default(),
        &HashSet::from_iter([staged]),
        scan.funds,
        false,
        PlayerId(1),
        ProductionPolicy::SmallMap,
    )
    .expect("visible enemy creates a combat plan input");

    assert_eq!(input.existing_units.len(), 1);
    assert!(
        input.existing_units[0].available_turn > 0,
        "Stage部隊を前衛と同じ手番の火力にせず、到着後の既存戦力として評価する"
    );
}

/// map_6で、`player` の首都がある島と、DAG・Node作戦を用意したWorldを返す。
fn map6_route_world(player: PlayerId) -> (World, crate::ai::islands::IslandId) {
    let master_data = MasterDataRegistry::load().unwrap();
    let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
        &master_data,
        "map_6",
        GridTopology::Square,
    )
    .unwrap();
    let own_capital = world
        .iter_entities()
        .find_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                .then_some(*position)
        })
        .expect("自首都");
    let island = world
        .resource::<crate::ai::islands::IslandMap>()
        .get_island_at(&own_capital)
        .expect("首都の島")
        .id;
    prepare_capital_route_topologies(&mut world, player);
    ensure_capital_route_node_operations(&mut world, player);
    (world, island)
}

/// 区間目標の拠点が地域Nodeの占領対象にまとめられているだけなら、そのNodeのanchorを
/// 目標にする。拠点セルのままだと経路の終点に作戦がなく、枝全体がDAGから外れる。
#[test]
fn route_target_property_resolves_to_its_capture_node_anchor() {
    let player = PlayerId(2);
    let (world, island) = map6_route_world(player);
    let operations = world.resource::<CapitalRouteNodeOperationRegistry>();
    let player_operations = operations
        .operations
        .values()
        .filter(|operation| operation.id.topology.player_id == player)
        .collect::<Vec<_>>();
    let frontiers = capital_route_dag_frontier_properties(&world, player, island).unwrap();

    let mut resolved_inside_area = 0;
    for property in frontiers.iter().flatten().copied() {
        let target = route_target_node_anchor(&world, player, island, property);
        if player_operations
            .iter()
            .any(|operation| operation.anchor == property)
        {
            assert_eq!(target, property, "拠点自身がNodeなら置き換えない");
        } else if let Some(owner) = player_operations
            .iter()
            .filter(|operation| operation.capture_targets.contains(&property))
            .min_by_key(|operation| operation.id.node.0)
        {
            assert_eq!(
                target, owner.anchor,
                "{property:?}を含む自首都側のNodeを目標にする"
            );
            resolved_inside_area += 1;
        }
    }
    assert!(
        resolved_inside_area > 0,
        "map_6の区間目標には地域Nodeにまとめられた拠点がある: {frontiers:?}"
    );
}

/// DAGはSquadを、作戦が与えた目標の属する枝へ置く。
#[test]
fn squad_route_follows_the_branch_of_its_own_target() {
    let player = PlayerId(2);
    let (world, island) = map6_route_world(player);
    let topology = capital_route_topology_for(&world, player, island).unwrap();
    let map = world.resource::<Map>();
    let open = vec![Some(pos(0, 0)); topology.routes.len()];
    let south = topology.routes[1].front;

    // 前線そのものを守るSquadは、その前線の枝へ入る。
    assert_eq!(
        squad_target_route(topology, map, &MissionType::Defense, Some(south), &open),
        Some(1)
    );
    // 枝の拠点を目標にするSquadは、任務によらずその拠点の枝へ入る。
    let (property, route) = topology
        .property_routes
        .iter()
        .map(|(property, route)| (*property, route.0))
        .min_by_key(|(property, _)| (property.y, property.x))
        .unwrap();
    for mission in [MissionType::Capture, MissionType::Attack] {
        assert_eq!(
            squad_target_route(topology, map, &mission, Some(property), &open),
            Some(route)
        );
    }
    // 拠点でも前線でもない目標は、戦闘役なら最寄りの前線の枝、占領役なら枝を決めない。
    let north = topology.routes[0].front;
    let near_south = [(0, 1), (1, 0), (0, -1), (-1, 0), (1, 1), (-1, -1)]
        .into_iter()
        .filter_map(|(dx, dy): (isize, isize)| {
            Some(pos(
                south.x.checked_add_signed(dx)?,
                south.y.checked_add_signed(dy)?,
            ))
        })
        .find(|cell| {
            !topology.property_routes.contains_key(cell)
                && map.distance(cell.x, cell.y, south.x, south.y)
                    < map.distance(cell.x, cell.y, north.x, north.y)
        })
        .expect("南の前線の隣に拠点でないマスがある");
    assert_eq!(
        squad_target_route(topology, map, &MissionType::Attack, Some(near_south), &open),
        Some(1)
    );
    assert_eq!(
        squad_target_route(
            topology,
            map,
            &MissionType::Capture,
            Some(near_south),
            &open
        ),
        None
    );
    // 区間目標の無い枝には入れない。
    let closed = vec![Some(pos(0, 0)), None];
    assert_eq!(
        squad_target_route(topology, map, &MissionType::Defense, Some(south), &closed),
        None
    );
}
