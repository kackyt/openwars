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
    CapitalRouteId, CapitalRouteNodeObjective, CapitalRouteNodeOperation,
    CapitalRouteNodeOperationId, CapitalRouteNodeOperationRegistry, CapitalRouteNodeScope,
    CapitalRoutePathRegistry, CapitalRouteTopologyKey, CapitalRouteTopologyRegistry,
    RouteForceDemand,
};
use crate::ai::v4::capital_route::node_operation::{
    RegionSeed, attach_capture_targets_to_corridor, capital_assault_geometry,
    capital_route_assault_authorized_from_dag, capital_route_current_fronts,
    capital_route_dag_frontier_properties, capital_route_node_exit_satisfied,
    capital_route_node_predecessors_released, consolidate_nearby_capture_target_regions,
    ensure_capital_route_node_operations, paint_capital_route_regions,
    refresh_capital_route_node_operations,
};
use crate::ai::v4::capital_route::prepare_capital_route_topologies;
use crate::ai::v4::capital_route::tactical::{
    capital_route_allows_capture_at, capital_route_capture_target_is_reserved,
    capital_route_is_recovering, capital_route_waypoint,
};
use crate::ai::v4::capital_route::topology::{
    armored_entry_bridge_gates, armored_step_distances, capital_route_topology_for, progress_order,
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
            anchor: pos(2, 2),
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
        local_control: false,
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
    let mut regions = vec![
        (pos(1, 1), vec![pos(1, 1)]),
        (pos(2, 2), vec![pos(2, 2)]),
        (pos(4, 1), vec![pos(4, 1)]),
        (pos(8, 1), vec![pos(8, 1)]),
    ];

    consolidate_nearby_capture_target_regions(&map, &mut regions);

    assert_eq!(regions[0].1, vec![pos(1, 1), pos(4, 1), pos(2, 2)]);
    assert!(regions[1].1.is_empty());
    assert!(regions[2].1.is_empty());
    assert_eq!(
        regions[3].1,
        vec![pos(8, 1)],
        "局地半径を越えた拠点まで一つの完了条件へ混ぜない"
    );
}

#[test]
fn linear_capture_corridor_keeps_independent_operations() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let map = flat_map(12, 3);
    let mut regions = vec![
        (pos(1, 1), vec![pos(1, 1)]),
        (pos(2, 1), vec![pos(2, 1)]),
        (pos(4, 1), vec![pos(4, 1)]),
    ];

    consolidate_nearby_capture_target_regions(&map, &mut regions);

    assert_eq!(
        regions,
        vec![
            (pos(1, 1), vec![pos(1, 1)]),
            (pos(2, 1), vec![pos(2, 1)]),
            (pos(4, 1), vec![pos(4, 1)]),
        ],
        "一直線の回廊は一Areaへ束ねず、前方へ順番に進める"
    );
}

#[test]
fn capture_targets_attach_to_nearest_corridor_cell_within_radius() {
    let map = flat_map(12, 8);
    let corridor = vec![pos(0, 0), pos(1, 0), pos(2, 0), pos(3, 0), pos(4, 0)];

    let regions = attach_capture_targets_to_corridor(
        &map,
        &corridor,
        [
            pos(2, 3),
            pos(1, 2),
            pos(4, 1),
            pos(4, 1),
            pos(9, 0),
            pos(2, 6),
        ],
    );

    assert_eq!(
        regions,
        vec![
            (pos(1, 0), vec![pos(1, 2)]),
            (pos(2, 0), vec![pos(2, 3)]),
            (pos(4, 0), vec![pos(4, 1)]),
        ],
        "最寄りの回廊セルへ結びつけ、半径を越える拠点は取り込まず、目標を持つセルだけを進行順に返す"
    );
}

#[test]
fn capture_target_tie_prefers_home_side_corridor_cell() {
    let map = flat_map(6, 4);
    let corridor = vec![pos(1, 0), pos(3, 0)];

    let regions = attach_capture_targets_to_corridor(&map, &corridor, [pos(2, 1)]);

    assert_eq!(
        regions,
        vec![(pos(1, 0), vec![pos(2, 1)])],
        "等距離なら自首都側の回廊セルを選ぶ"
    );
}

#[test]
fn area_forward_release_is_independent_of_capture_completion() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let topology = CapitalRouteTopologyKey {
        player_id: player,
        island_id: crate::ai::islands::IslandId(0),
    };
    let operation = |scope| CapitalRouteNodeOperation {
        id: CapitalRouteNodeOperationId {
            topology,
            anchor: pos(2, 2),
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
        local_control: true,
        state: victory_roadmap::RoadmapNodeState::Dominant,
        assigned_squads: HashSet::default(),
    };

    assert!(
        capital_route_node_exit_satisfied(&operation(CapitalRouteNodeScope::Area)),
        "地域を制圧した前衛は、施設の占領担当を残して次地域へ進める"
    );
    assert!(
        !capital_route_node_exit_satisfied(&operation(CapitalRouteNodeScope::Point)),
        "単一Capture拠点は所有権確保まで後続を開けない"
    );
    let mut sustained = operation(CapitalRouteNodeScope::Area);
    sustained.state = victory_roadmap::RoadmapNodeState::Capturing;
    sustained.local_control = true;
    assert!(
        capital_route_node_exit_satisfied(&sustained),
        "占領継続中も局地優勢なら前衛を解放する"
    );
    sustained.local_control = false;
    assert!(
        !capital_route_node_exit_satisfied(&sustained),
        "優勢を失えば通過許可も撤回する"
    );
}

#[test]
fn controlled_capture_area_retains_one_escort_until_secured() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let topology = CapitalRouteTopologyKey {
        player_id: player,
        island_id: crate::ai::islands::IslandId(0),
    };
    let rear_node = pos(1, 2);
    let active_node = pos(2, 2);
    let rear_id = CapitalRouteNodeOperationId {
        topology,
        anchor: rear_node,
    };
    let operation = |id, state| CapitalRouteNodeOperation {
        id,
        anchor: id.anchor,
        predecessors: Vec::new(),
        successors: Vec::new(),
        objective: CapitalRouteNodeObjective::Capture,
        scope: CapitalRouteNodeScope::Area,
        control_area: vec![id.anchor],
        capture_targets: vec![id.anchor],
        crossing_exit: None,
        crossed: false,
        local_control: true,
        state,
        assigned_squads: HashSet::default(),
    };
    let mut registry = CapitalRouteNodeOperationRegistry::default();
    registry.operations.insert(
        rear_id,
        operation(rear_id, victory_roadmap::RoadmapNodeState::Capturing),
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
            anchor: pos(2, 2),
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
        local_control: false,
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
            anchor: pos(2, 2),
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
        local_control: false,
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

fn area_seed(anchor: GridPosition, capture_targets: &[GridPosition]) -> RegionSeed<'_> {
    RegionSeed {
        anchor,
        scope: CapitalRouteNodeScope::Area,
        capture_targets,
    }
}

#[test]
fn area_control_region_excludes_remote_milestones() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let map = flat_map(30, 5);
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let anchor = pos(15, 2);
    let island = island_map.get_island_at(&anchor).unwrap().id;
    let capture_target = pos(14, 2);
    let rear = pos(8, 2);
    let forward = pos(22, 2);

    let targets = [capture_target];
    let regions = paint_capital_route_regions(
        &map,
        &island_map,
        island,
        &[
            area_seed(rear, &[]),
            area_seed(anchor, &targets),
            area_seed(forward, &[]),
        ],
    );
    let area = &regions[1];

    assert!(area.contains(&anchor));
    assert!(area.contains(&capture_target));
    assert!(
        !area.contains(&forward),
        "後方Milestoneを局地優勢へ数えない"
    );
    assert!(
        !area.contains(&rear),
        "後続Milestoneの敵を現在Nodeへ数えない"
    );
    // 中点より相手側のセルは取らない。
    assert!(
        area.iter().all(|cell| (12..=18).contains(&cell.x)),
        "{area:?}"
    );
}

/// 塗り分けた地域は重ならず、陸地の各セルは陸路で最も近い代表点の地域に属する。
#[test]
fn painted_regions_partition_land_without_overlap() {
    let map = flat_map(12, 7);
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let island = island_map.get_island_at(&pos(0, 0)).unwrap().id;
    let anchors = [pos(1, 1), pos(10, 1), pos(5, 5)];
    let seeds = anchors.map(|anchor| area_seed(anchor, &[]));
    let regions = paint_capital_route_regions(&map, &island_map, island, &seeds);
    let mut seen = HashSet::default();
    for region in &regions {
        for cell in region {
            assert!(seen.insert(*cell), "{cell:?} が複数の地域に属する");
        }
    }
    assert!(
        seen.iter().all(|cell| anchors
            .iter()
            .any(|a| map.distance(cell.x, cell.y, a.x, a.y) <= CAPITAL_ROUTE_REGION_RADIUS)),
        "代表点から半径を超えるセルは塗らない"
    );
    // (6,0) はどの代表点からも4マス以上離れている。
    assert!(seen.contains(&pos(3, 2)));
    assert!(!seen.contains(&pos(6, 0)), "半径外は塗らない");
    for (index, region) in regions.iter().enumerate() {
        for cell in region {
            let own = map.distance(cell.x, cell.y, anchors[index].x, anchors[index].y);
            assert!(
                anchors
                    .iter()
                    .all(|other| own <= map.distance(cell.x, cell.y, other.x, other.y)),
                "{cell:?} は最寄りの代表点の地域に属する"
            );
        }
    }
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
                    target_anchor: segment_target,
                    objective: CapitalRouteNodeObjective::Control,
                    scope: CapitalRouteNodeScope::Point,
                    control_area: vec![segment_target],
                    capture_targets: Vec::new(),
                    crossing_exit: None,
                    crossed: false,
                    execution_target: hold,
                    frontline_capacity: 1,
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
                    target_anchor: segment_target,
                    objective: CapitalRouteNodeObjective::Control,
                    scope: CapitalRouteNodeScope::Point,
                    control_area: vec![segment_target],
                    capture_targets: Vec::new(),
                    crossing_exit: None,
                    crossed: false,
                    execution_target: supply,
                    frontline_capacity: 1,
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
                    target_anchor: segment_target,
                    objective: CapitalRouteNodeObjective::Control,
                    scope: CapitalRouteNodeScope::Point,
                    control_area: vec![segment_target],
                    capture_targets: Vec::new(),
                    crossing_exit: None,
                    crossed: false,
                    execution_target: segment_target,
                    frontline_capacity: 1,
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
                target_anchor: target,
                objective: CapitalRouteNodeObjective::Control,
                scope: CapitalRouteNodeScope::Point,
                control_area: vec![target],
                capture_targets: Vec::new(),
                crossing_exit: None,
                crossed: false,
                execution_target: target,
                frontline_capacity: 1,
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
        target_anchor: target,
        objective: CapitalRouteNodeObjective::Capture,
        scope: CapitalRouteNodeScope::Point,
        control_area: vec![target],
        capture_targets: vec![target],
        crossing_exit: None,
        crossed: false,
        execution_target: target,
        frontline_capacity: 1,
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
        anchor: pos(8, 11),
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
            local_control: false,
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
    let home = topology.nodes[topology.start_node.0].anchor;
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
            anchor: home,
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
        assert!(topology.nodes.iter().enumerate().all(|(index, node)| {
            node.successor_nodes
                .iter()
                .all(|successor| successor.0 > index)
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
        .min_by_key(|operation| topology.from_home[&operation.anchor])
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
    let bridge_progress = topology.from_home[&bridge_operation.anchor];

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
        .filter(|node| topology.from_home[&node.anchor] < bridge_progress)
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
                max_range: 1,
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
            max_range: 1,
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
            max_range: 1,
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
        target_anchor: pos(5, 1),
        objective: CapitalRouteNodeObjective::Control,
        scope: CapitalRouteNodeScope::Point,
        control_area: vec![pos(5, 1)],
        capture_targets: Vec::new(),
        crossing_exit: None,
        crossed: false,
        execution_target: pos(5, 1),
        frontline_capacity: 1,
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

/// 進軍路の現在の前線は、その進軍路の前線拠点のうち最も前進した拠点である。
#[test]
fn current_route_fronts_are_the_most_advanced_frontier_properties() {
    let player = PlayerId(2);
    let (world, island) = map6_route_world(player);
    let topology = capital_route_topology_for(&world, player, island).expect("map_6の進軍回廊");
    let frontiers = capital_route_dag_frontier_properties(&world, player, island).unwrap();

    let fronts = capital_route_current_fronts(&world, player, island).unwrap();

    assert_eq!(fronts.len(), topology.routes.len());
    for (route, front) in fronts.iter().enumerate() {
        assert!(
            frontiers[route].contains(front),
            "route {route} の前線 {front:?} は前線拠点 {:?} の一つ",
            frontiers[route]
        );
        let front_progress = topology.from_home[front];
        assert!(
            frontiers[route]
                .iter()
                .all(|property| topology.from_home[property] <= front_progress),
            "route {route} の前線は最も前進した前線拠点"
        );
    }
    assert!(
        fronts
            .iter()
            .zip(&topology.routes)
            .any(|(front, route)| *front != route.front),
        "固定の識別点ではなく、盤面の所有状況から前線を求める"
    );
}

/// 敵首都以外の拠点をすべて確保した進軍路では、敵首都を前線にする。
#[test]
fn current_route_fronts_fall_back_to_enemy_capital_once_cleared() {
    let player = PlayerId(2);
    let (mut world, island) = map6_route_world(player);
    let goal = {
        let topology = capital_route_topology_for(&world, player, island).unwrap();
        topology.nodes[topology.goal_node.0].anchor
    };
    let properties = world
        .iter_entities()
        .filter_map(|entity| {
            let property = entity.get::<Property>()?;
            (property.terrain != Terrain::Capital).then_some(entity.id())
        })
        .collect::<Vec<_>>();
    for entity in properties {
        world
            .entity_mut(entity)
            .get_mut::<Property>()
            .expect("Propertyを持つEntity")
            .owner_id = Some(player);
    }

    let fronts = capital_route_current_fronts(&world, player, island).unwrap();

    assert_eq!(fronts, vec![goal; fronts.len()]);
}

/// `player` の首都戦役で、敵首都から `radius` より離れた拠点をすべて `player` の所有にする。
fn own_properties_outside_capital_region(
    world: &mut World,
    player: PlayerId,
    capital: GridPosition,
    radius: u32,
) {
    let map = world.resource::<Map>().clone();
    let properties = world
        .iter_entities()
        .filter_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            entity.get::<Property>()?;
            (map.distance(position.x, position.y, capital.x, capital.y) > radius)
                .then_some(entity.id())
        })
        .collect::<Vec<_>>();
    for entity in properties {
        world
            .entity_mut(entity)
            .get_mut::<Property>()
            .expect("Propertyを持つEntity")
            .owner_id = Some(player);
    }
}

/// どちらの進軍路も首都地域へ達していない序盤は、首都攻略段階に入らない。
#[test]
fn capital_assault_waits_until_one_route_reaches_the_capital_region() {
    let player = PlayerId(2);
    let (world, island) = map6_route_world(player);

    assert_eq!(capital_assault_geometry(&world, player, island), None);
}

/// 首都の地域の外の拠点をすべて確保すると首都攻略に入り、待機地点は首都の入口の外
/// （首都から11ステップ手前の回廊セル）になる。
#[test]
fn capital_assault_starts_once_only_the_capital_region_remains() {
    let player = PlayerId(2);
    let (mut world, island) = map6_route_world(player);
    let (goal, goal_progress) = {
        let topology = capital_route_topology_for(&world, player, island).unwrap();
        let goal = topology.nodes[topology.goal_node.0].anchor;
        (goal, topology.from_home[&goal])
    };
    own_properties_outside_capital_region(&mut world, player, goal, CAPITAL_ROUTE_REGION_RADIUS);

    let geometry = capital_assault_geometry(&world, player, island).expect("首都攻略の段階");

    let topology = capital_route_topology_for(&world, player, island).unwrap();
    assert_eq!(geometry.capital, goal);
    assert!(topology.corridor.contains(&geometry.staging));
    assert_eq!(topology.from_home[&geometry.staging] + 11, goal_progress);
}

/// 区間目標の拠点が地域Nodeの占領対象にまとめられているだけなら、そのNodeのanchorを
/// 目標にする。拠点セルのままだと経路の終点に作戦がなく、枝全体がDAGから外れる。
#[test]
fn route_target_property_resolves_to_its_capture_node_anchor() {
    let player = PlayerId(2);
    let (world, island) = map6_route_world(player);
    let topology = capital_route_topology_for(&world, player, island).expect("map_6の進軍回廊");
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
            .min_by_key(|operation| progress_order(&topology.from_home, operation.anchor))
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

/// 首都周辺の施設は首都地域の並行目標であり、首都への中継Nodeにはしない。
#[test]
fn capital_region_owns_adjacent_factories_without_internal_dependencies() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let master = MasterDataRegistry::load().unwrap();
    for player in [PlayerId(1), PlayerId(2)] {
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master,
            "map_20",
            GridTopology::Hex,
        )
        .unwrap();
        prepare_capital_route_topologies(&mut world, player);
        ensure_capital_route_node_operations(&mut world, player);
        let registry = world.resource::<CapitalRouteNodeOperationRegistry>();
        let topologies = world.resource::<CapitalRouteTopologyRegistry>();
        for (key, topology) in &topologies.topologies {
            if key.player_id != player {
                continue;
            }
            let goal = topology.nodes[topology.goal_node.0].anchor;
            let goal_id = CapitalRouteNodeOperationId {
                topology: *key,
                anchor: goal,
            };
            let region = &registry.operations[&goal_id];
            assert_eq!(region.scope, CapitalRouteNodeScope::Area);
            let map = world.resource::<Map>();
            for entity in world.iter_entities() {
                let (Some(position), Some(property)) =
                    (entity.get::<GridPosition>(), entity.get::<Property>())
                else {
                    continue;
                };
                if property.terrain != Terrain::Factory
                    || map.distance(position.x, position.y, goal.x, goal.y) > 1
                {
                    continue;
                }
                assert!(
                    region.capture_targets.contains(position),
                    "首都隣接工場 {position:?} は同じ地域の目標"
                );
                assert!(
                    !registry
                        .operations
                        .keys()
                        .any(|id| id.topology == *key && id.anchor == *position),
                    "地域内部に施設Nodeを作らない"
                );
            }
            for target in &region.capture_targets {
                assert_eq!(
                    registry
                        .operations
                        .values()
                        .filter(|operation| operation.id.topology == *key
                            && operation.capture_targets.contains(target))
                        .count(),
                    1
                );
            }
        }
    }
}

fn standard_region_world() -> (World, Entity, SquadId) {
    use crate::ai::operation_assignment::{
        OperationOwner, OperationUnitRole, UnitOperationAssignment, UnitOperationRegistry,
    };
    use crate::components::{ActionCompleted, Ammo, Fuel, HasMoved};
    let (mut world, _) = crate::setup::create_world();
    let map = flat_map(25, 5);
    world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
    world.insert_resource(map);
    let master = MasterDataRegistry::load().unwrap();
    let stats = master
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Tank.as_str().to_owned(),
        ))
        .unwrap();
    let entity = world
        .spawn((
            Faction(PlayerId(1)),
            pos(10, 2),
            Health {
                current: 100,
                max: 100,
            },
            Ammo {
                ammo1: stats.max_ammo1,
                max_ammo1: stats.max_ammo1,
                ammo2: stats.max_ammo2,
                max_ammo2: stats.max_ammo2,
            },
            Fuel {
                current: stats.max_fuel,
                max: stats.max_fuel,
            },
            HasMoved(false),
            ActionCompleted(false),
            stats,
        ))
        .id();
    world.insert_resource(master);
    world.insert_resource(crate::resources::DamageChart::new());
    let mut settings = crate::ai::PlayerAiSettings::new();
    settings.set_version(PlayerId(1), crate::ai::AiVersion::V4);
    world.insert_resource(settings);
    let squad = assign_route_squad(&mut world, PlayerId(1), MissionType::Defense, &[entity]);
    world
        .resource_mut::<crate::ai::squad::SquadManager>()
        .squads[0]
        .plan
        .target = Some(pos(20, 2));
    world.resource_mut::<UnitOperationRegistry>().assign(
        entity,
        UnitOperationAssignment {
            owner: OperationOwner::Campaign {
                player_id: PlayerId(1),
                island_id: crate::ai::islands::IslandId(0),
            },
            squad_id: Some(squad),
            role: OperationUnitRole::Member,
            assigned_turn: 1,
        },
    );
    world.insert_resource(CapitalRoutePathRegistry {
        commitments: HashMap::from_iter([(
            squad,
            CapitalRouteCommitment {
                player_id: PlayerId(1),
                island_id: crate::ai::islands::IslandId(0),
                route: CapitalRouteId(0),
                target: pos(2, 2),
                target_anchor: pos(2, 2),
                objective: CapitalRouteNodeObjective::Control,
                scope: CapitalRouteNodeScope::Area,
                control_area: vec![pos(2, 2)],
                capture_targets: Vec::new(),
                crossing_exit: None,
                crossed: false,
                execution_target: pos(2, 2),
                frontline_capacity: 1,
                path: vec![pos(10, 2), pos(2, 2)],
                phase: CapitalRouteExecutionPhase::Hold,
            },
        )]),
        ..Default::default()
    });
    (world, entity, squad)
}

#[test]
fn standard_v4_uses_committed_region_goal() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let (mut world, entity, _) = standard_region_world();
    let decision =
        crate::ai::tactics::v2::decide_action(&mut world, PlayerId(1), &HashSet::default());
    assert!(
        matches!(decision,Some((actor,crate::ai::command::AiCommand::Wait{target_pos})) if actor==entity && target_pos.x<10),
        "地域の保持地点へ西進する: {decision:?}"
    );
}

/// 地域入口を代表座標へ固定せず、占有された入口を避ける。
#[test]
fn standard_region_entry_uses_free_perimeter() {
    let (mut world, entity, squad) = standard_region_world();
    let mut paths = world.resource_mut::<CapitalRoutePathRegistry>();
    let order = paths.commitments.get_mut(&squad).unwrap();
    order.phase = CapitalRouteExecutionPhase::Advance;
    order.target_anchor = pos(15, 2);
    order.execution_target = pos(15, 2);
    order.control_area = vec![pos(14, 2), pos(14, 3), pos(15, 2), pos(15, 3)];
    super::tactical::prepare_region_tactical_cache(&mut world);
    let first = super::tactical::capital_route_tactical_target(&world, PlayerId(1), entity)
        .flatten()
        .unwrap();
    assert_eq!(first, pos(14, 2));
    let stats = world.get::<UnitStats>(entity).unwrap().clone();
    world.spawn((Faction(PlayerId(1)), first, stats));
    let next = super::tactical::capital_route_tactical_target(&world, PlayerId(1), entity)
        .flatten()
        .unwrap();
    assert_ne!(next, first);
    assert!([pos(14, 3), pos(15, 2), pos(15, 3)].contains(&next));
    // 占有変化は距離の再計算なしで反映し、地形変化時だけ共有距離を破棄する。
    for y in 0..5 {
        world
            .resource_mut::<Map>()
            .set_terrain(12, y, Terrain::Sea)
            .unwrap();
    }
    super::tactical::prepare_region_tactical_cache(&mut world);
    assert_eq!(
        super::tactical::capital_route_tactical_target(&world, PlayerId(1), entity),
        Some(None)
    );
}

/// 地域内の占領役は同じ施設へ集中させない。
#[test]
fn standard_region_capturers_claim_distinct_properties() {
    use crate::ai::operation_assignment::{
        OperationOwner, OperationUnitRole, UnitOperationAssignment, UnitOperationRegistry,
    };
    let (mut world, entity, squad) = standard_region_world();
    world
        .resource_mut::<crate::ai::squad::SquadManager>()
        .squads[0]
        .plan
        .mission_type = MissionType::Capture;
    let stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Infantry.as_str().to_owned(),
        ))
        .unwrap();
    *world.get_mut::<UnitStats>(entity).unwrap() = stats.clone();
    let second = world
        .spawn((
            Faction(PlayerId(1)),
            pos(10, 3),
            stats,
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    world
        .resource_mut::<crate::ai::squad::SquadManager>()
        .squads[0]
        .members
        .insert(second);
    world.resource_mut::<UnitOperationRegistry>().assign(
        second,
        UnitOperationAssignment {
            owner: OperationOwner::Campaign {
                player_id: PlayerId(1),
                island_id: crate::ai::islands::IslandId(0),
            },
            squad_id: Some(squad),
            role: OperationUnitRole::Member,
            assigned_turn: 1,
        },
    );
    for cell in [pos(12, 2), pos(12, 3)] {
        world.spawn((cell, Property::new(Terrain::City, Some(PlayerId(2)), 200)));
    }
    let mut paths = world.resource_mut::<CapitalRoutePathRegistry>();
    let order = paths.commitments.get_mut(&squad).unwrap();
    order.phase = CapitalRouteExecutionPhase::Advance;
    order.objective = CapitalRouteNodeObjective::Capture;
    order.control_area = vec![
        pos(10, 2),
        pos(10, 3),
        pos(11, 2),
        pos(11, 3),
        pos(12, 2),
        pos(12, 3),
    ];
    order.capture_targets = vec![pos(12, 2), pos(12, 3)];
    let target = |unit| {
        super::tactical::capital_route_tactical_target(&world, PlayerId(1), unit)
            .flatten()
            .unwrap()
    };
    assert_eq!(target(entity), pos(12, 2));
    assert_eq!(target(second), pos(12, 3));
}

/// 前方の敵は排除すべき対象であり、進入口がある地域の目標を消さない。
#[test]
fn standard_region_entry_keeps_goal_beyond_enemy_screen() {
    let (mut world, entity, squad) = standard_region_world();
    let stats = world.get::<UnitStats>(entity).unwrap().clone();
    for y in 0..5 {
        world.spawn((Faction(PlayerId(2)), pos(12, y), stats.clone()));
    }
    let mut paths = world.resource_mut::<CapitalRoutePathRegistry>();
    let order = paths.commitments.get_mut(&squad).unwrap();
    order.phase = CapitalRouteExecutionPhase::Advance;
    order.execution_target = pos(15, 2);
    order.control_area = vec![pos(14, 2), pos(14, 3), pos(15, 2), pos(15, 3)];
    let target = super::tactical::capital_route_tactical_target(&world, PlayerId(1), entity);
    assert_eq!(
        target,
        Some(Some(pos(14, 2))),
        "敵に遮られても地域への進軍目標を維持する"
    );
    // 地域内の進入可能マスを全て敵が占めた場合も、攻撃目標を残す。
    for cell in [pos(14, 2), pos(14, 3), pos(15, 2), pos(15, 3)] {
        world.spawn((Faction(PlayerId(2)), cell, stats.clone()));
    }
    assert_eq!(
        super::tactical::capital_route_tactical_target(&world, PlayerId(1), entity),
        Some(Some(pos(14, 2)))
    );
}

/// 回廊外の防衛・射撃位置を、全員共通の後方拠点へ置き換えない。
#[test]
fn route_hold_preserves_concrete_defense_position_outside_corridor() {
    let player = PlayerId(1);
    let (mut world, island) = map6_route_world(player);
    let topology = capital_route_topology_for(&world, player, island).unwrap();
    let frontier_progress = capital_route_dag_frontier_properties(&world, player, island)
        .unwrap()
        .into_iter()
        .flatten()
        .filter_map(|target| topology.from_home.get(&target).copied())
        .min()
        .unwrap();
    let destination = *topology
        .from_home
        .iter()
        .filter(|(cell, progress)| {
            **progress > 0 && **progress < frontier_progress && !topology.corridor.contains(cell)
        })
        .min_by_key(|(cell, progress)| (**progress, cell.y, cell.x))
        .unwrap()
        .0;
    let stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Tank.as_str().to_owned(),
        ))
        .unwrap();
    let entity = world
        .spawn((
            Faction(player),
            destination,
            stats,
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    let squad = assign_route_squad(&mut world, player, MissionType::Defense, &[entity]);
    let mut manager = world
        .remove_resource::<crate::ai::squad::SquadManager>()
        .unwrap();
    manager.squads[0].plan.target = Some(destination);
    manager.squads[0].plan.target_island = Some(island);
    super::commitment::refresh_capital_route_path_commitments(&mut world, player, &mut manager);
    let order = &world.resource::<CapitalRoutePathRegistry>().commitments[&squad];
    assert_eq!(order.phase, CapitalRouteExecutionPhase::Hold);
    assert_eq!(
        order.execution_target, destination,
        "防衛位置を後方の代表地点へ集約しない"
    );
}

/// 地域指示を一時解除した負傷部隊は、修理地点までの途中にも移動できる。
#[test]
fn standard_region_recovery_can_approach_distant_repair_site() {
    let (mut world, entity, _) = standard_region_world();
    world.get_mut::<Health>(entity).unwrap().current = 60;
    world
        .resource_mut::<Map>()
        .set_terrain(2, 2, Terrain::Factory)
        .unwrap();
    world.spawn((
        pos(2, 2),
        Property::new(Terrain::Factory, Some(PlayerId(1)), 200),
    ));
    let decision =
        crate::ai::tactics::v2::decide_action(&mut world, PlayerId(1), &HashSet::default());
    assert!(
        matches!(decision,Some((actor,crate::ai::command::AiCommand::Wait{target_pos})) if actor==entity && target_pos.x<10 && target_pos.x>2),
        "一手で工場へ着けなくても西へ修理移動する: {decision:?}"
    );
}

#[test]
fn every_capital_route_gate_reaches_goal_without_changing_corridor() {
    let master = MasterDataRegistry::load().unwrap();
    for map_name in ["map_20", "map_26", "map_47", "map_6"] {
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master,
            map_name,
            GridTopology::Hex,
        )
        .unwrap();
        for player in [PlayerId(1), PlayerId(2)] {
            prepare_capital_route_topologies(&mut world, player);
        }
        let registry = world.resource::<CapitalRouteTopologyRegistry>();
        for topology in registry.topologies.values() {
            let mut reaches_goal = vec![false; topology.nodes.len()];
            reaches_goal[topology.goal_node.0] = true;
            for index in (0..topology.nodes.len()).rev() {
                reaches_goal[index] |= topology.nodes[index]
                    .successor_nodes
                    .iter()
                    .any(|next| reaches_goal[next.0]);
            }
            let mut from_start = vec![false; topology.nodes.len()];
            from_start[topology.start_node.0] = true;
            for index in 0..topology.nodes.len() {
                if from_start[index] {
                    for next in &topology.nodes[index].successor_nodes {
                        from_start[next.0] = true;
                    }
                }
            }
            for route in &topology.routes {
                let node = topology.node_at(route.front).unwrap();
                assert!(
                    from_start[node.0],
                    "{map_name}: 入口 {:?} に出発点から到達できる",
                    route.front
                );
                assert!(
                    reaches_goal[node.0],
                    "{map_name}: 入口 {:?} から首都へ進める",
                    route.front
                );
            }
        }
    }
}

#[test]
fn explicit_capital_orders_keep_destination_and_traverse_their_assigned_gate() {
    let master = MasterDataRegistry::load().unwrap();
    for player in [PlayerId(1), PlayerId(2)] {
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master,
            "map_20",
            GridTopology::Hex,
        )
        .unwrap();
        prepare_capital_route_topologies(&mut world, player);
        ensure_capital_route_node_operations(&mut world, player);
        let topology = world
            .resource::<CapitalRouteTopologyRegistry>()
            .topologies
            .iter()
            .find(|(key, _)| key.player_id == player)
            .map(|(_, topology)| topology)
            .unwrap();
        let home = topology.nodes[topology.start_node.0].anchor;
        let goal = topology.nodes[topology.goal_node.0].anchor;
        let island = world
            .resource::<crate::ai::islands::IslandMap>()
            .get_island_at(&home)
            .unwrap()
            .id;
        let stats = master
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Mech.as_str().to_owned(),
            ))
            .unwrap();
        let mut ids = Vec::new();
        for mission in [
            MissionType::Attack,
            MissionType::Capture,
            MissionType::Defense,
        ] {
            let entity = world
                .spawn((
                    Faction(player),
                    home,
                    stats.clone(),
                    Health {
                        current: 100,
                        max: 100,
                    },
                ))
                .id();
            ids.push(assign_route_squad(&mut world, player, mission, &[entity]));
        }
        let mut manager = world
            .remove_resource::<crate::ai::squad::SquadManager>()
            .unwrap();
        for (index, id) in ids.iter().enumerate() {
            let squad = manager
                .squads
                .iter_mut()
                .find(|squad| squad.id == *id)
                .unwrap();
            squad.plan.target = Some(if index == 2 { home } else { goal });
            squad.plan.target_island = Some(island);
        }
        super::commitment::refresh_capital_route_path_commitments(&mut world, player, &mut manager);
        let orders = world.resource::<CapitalRoutePathRegistry>();
        let topology = capital_route_topology_for(&world, player, island).unwrap();
        for id in &ids[..2] {
            let order = &orders.commitments[id];
            assert_eq!(order.target, goal, "首都命令を側方拠点で上書きしない");
            assert!(
                order.path.contains(&topology.routes[order.route.0].front),
                "担当回廊を通る"
            );
        }
        let defense = &orders.commitments[&ids[2]];
        assert_eq!(defense.phase, CapitalRouteExecutionPhase::Hold);
        assert_eq!(
            defense.execution_target, home,
            "別の防衛任務を首都へ集中させない"
        );
    }
}

#[test]
fn regional_entry_moves_onto_position_instead_of_stopping_at_weapon_range() {
    use crate::components::{Ammo, Fuel};
    for unit_type in [UnitType::Tank, UnitType::Mech, UnitType::HeavySpGun] {
        let (mut world, entity, squad) = standard_region_world();
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                unit_type.as_str().to_owned(),
            ))
            .unwrap();
        world.entity_mut(entity).insert((
            pos(13, 2),
            Ammo {
                ammo1: stats.max_ammo1,
                max_ammo1: stats.max_ammo1,
                ammo2: stats.max_ammo2,
                max_ammo2: stats.max_ammo2,
            },
            Fuel {
                current: stats.max_fuel,
                max: stats.max_fuel,
            },
            stats,
        ));
        let mut paths = world.resource_mut::<CapitalRoutePathRegistry>();
        let order = paths.commitments.get_mut(&squad).unwrap();
        order.phase = CapitalRouteExecutionPhase::Advance;
        order.scope = CapitalRouteNodeScope::Area;
        order.objective = CapitalRouteNodeObjective::Control;
        order.control_area = vec![pos(14, 2)];
        order.execution_target = pos(14, 2);
        let decision =
            crate::ai::tactics::v2::decide_action(&mut world, PlayerId(1), &HashSet::default());
        assert!(
            matches!(decision, Some((actor, crate::ai::command::AiCommand::Wait { target_pos }))
            if actor == entity && target_pos == pos(14, 2)),
            "{unit_type:?}: 射程内の手前で止まらず指定地域へ入る: {decision:?}"
        );
    }
}

#[test]
fn regional_suppression_aims_at_enemy_from_outside_then_advances_to_open_ground() {
    let (mut world, entity, squad) = standard_region_world();
    let stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::HeavySpGun.as_str().to_owned(),
        ))
        .unwrap();
    world.entity_mut(entity).insert((pos(12, 2), stats));
    let enemy_stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Infantry.as_str().to_owned(),
        ))
        .unwrap();
    let enemy = world
        .spawn((
            Faction(PlayerId(2)),
            pos(15, 2),
            enemy_stats,
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    world
        .resource_mut::<crate::resources::DamageChart>()
        .insert_damage(UnitType::HeavySpGun, UnitType::Infantry, 70);
    let mut paths = world.resource_mut::<CapitalRoutePathRegistry>();
    let order = paths.commitments.get_mut(&squad).unwrap();
    order.phase = CapitalRouteExecutionPhase::Advance;
    order.scope = CapitalRouteNodeScope::Area;
    order.objective = CapitalRouteNodeObjective::Control;
    order.control_area = vec![pos(14, 2), pos(15, 2)];
    order.execution_target = pos(14, 2);
    assert_eq!(
        super::tactical::capital_route_tactical_target(&world, PlayerId(1), entity),
        Some(Some(pos(15, 2))),
        "地域外の火力へ排除対象を渡す"
    );
    let decision =
        crate::ai::tactics::v2::decide_action(&mut world, PlayerId(1), &HashSet::default());
    assert!(
        matches!(decision, Some((actor, crate::ai::command::AiCommand::Attack { target_entity, .. })) if actor==entity && target_entity==enemy),
        "射撃位置から地域内の敵を攻撃する: {decision:?}"
    );
    world.despawn(enemy);
    let decision =
        crate::ai::tactics::v2::decide_action(&mut world, PlayerId(1), &HashSet::default());
    assert!(
        matches!(decision, Some((actor, crate::ai::command::AiCommand::Wait { target_pos })) if actor==entity && target_pos==pos(14, 2)),
        "敵排除後に入口手前で止まらず空いた地域へ進む: {decision:?}"
    );
}

/// 前提地域が未制圧でも、現地へ実到達した部隊をLockedとして無視・後退指示しない。
#[test]
fn actual_arrival_observes_contested_region_and_keeps_forward_squad_there() {
    let master = MasterDataRegistry::load().unwrap();
    let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
        &master,
        "map_20",
        GridTopology::Hex,
    )
    .unwrap();
    let player = PlayerId(1);
    prepare_capital_route_topologies(&mut world, player);
    ensure_capital_route_node_operations(&mut world, player);
    let (key, topology) = world
        .resource::<CapitalRouteTopologyRegistry>()
        .topologies
        .iter()
        .find(|(key, _)| key.player_id == player)
        .unwrap();
    let key = *key;
    let goal = topology.nodes[topology.goal_node.0].anchor;
    let id = CapitalRouteNodeOperationId {
        topology: key,
        anchor: goal,
    };
    let area = world
        .resource::<CapitalRouteNodeOperationRegistry>()
        .operations[&id]
        .control_area
        .clone();
    let operations = &world
        .resource::<CapitalRouteNodeOperationRegistry>()
        .operations;
    let predecessors = &operations[&id].predecessors;
    let arrival = area
        .iter()
        .copied()
        .filter(|at| {
            *at != goal
                && predecessors
                    .iter()
                    .all(|id| !operations[id].control_area.contains(at))
                && world
                    .resource::<Map>()
                    .get_terrain(at.x, at.y)
                    .is_some_and(|terrain| {
                        crate::systems::movement::get_valid_movement_cost(
                            &master,
                            MovementType::Tank,
                            terrain,
                        )
                        .is_some()
                    })
        })
        .min_by_key(|at| progress_order(&topology.from_home, *at))
        .unwrap();
    let stats = master
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Tank.as_str().to_owned(),
        ))
        .unwrap();
    let forward = world
        .spawn((
            Faction(player),
            arrival,
            stats.clone(),
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    for at in area.into_iter().filter(|at| *at != arrival).take(2) {
        world.spawn((
            Faction(PlayerId(2)),
            at,
            stats.clone(),
            Health {
                current: 100,
                max: 100,
            },
        ));
    }
    let squad_id = assign_route_squad(&mut world, player, MissionType::Attack, &[forward]);
    let mut manager = world
        .remove_resource::<crate::ai::squad::SquadManager>()
        .unwrap();
    let squad = manager
        .squads
        .iter_mut()
        .find(|squad| squad.id == squad_id)
        .unwrap();
    squad.plan.target = Some(goal);
    squad.plan.target_island = Some(key.island_id);
    let carrier = world.spawn_empty().id();
    world
        .entity_mut(forward)
        .insert(crate::components::Transporting(carrier));
    refresh_capital_route_node_operations(&mut world, player, &manager);
    assert_eq!(
        world
            .resource::<CapitalRouteNodeOperationRegistry>()
            .operations[&id]
            .state,
        victory_roadmap::RoadmapNodeState::Locked,
        "輸送中の荷物を到達済み戦力へ数えない"
    );
    world
        .entity_mut(forward)
        .remove::<crate::components::Transporting>();
    refresh_capital_route_node_operations(&mut world, player, &manager);
    let registry = world.resource::<CapitalRouteNodeOperationRegistry>();
    assert!(!capital_route_node_predecessors_released(
        &registry.operations,
        &registry.operations[&id]
    ));
    assert_eq!(
        registry.operations[&id].state,
        victory_roadmap::RoadmapNodeState::Contested,
        "現地の戦闘をLockedで隠さない"
    );
    super::commitment::refresh_capital_route_path_commitments(&mut world, player, &mut manager);
    let order = &world.resource::<CapitalRoutePathRegistry>().commitments[&squad_id];
    assert_eq!(order.target_anchor, goal, "前衛を前提地域へ呼び戻さない");
    assert_eq!(order.phase, CapitalRouteExecutionPhase::Advance);
}

#[test]
fn regional_control_area_does_not_include_nearby_ground_across_an_uncrossed_bridge() {
    let mut map = flat_map(9, 5);
    for y in 0..5 {
        map.set_terrain(4, y, Terrain::River).unwrap();
    }
    map.set_terrain(4, 2, Terrain::Bridge).unwrap();
    let islands = crate::ai::islands::IslandMap::analyze(&map);
    let island = islands.get_island_at(&pos(3, 2)).unwrap().id;
    let regions = paint_capital_route_regions(&map, &islands, island, &[area_seed(pos(3, 2), &[])]);
    let area = &regions[0];
    assert!(area.contains(&pos(3, 3)));
    assert!(
        !area.contains(&pos(5, 2)),
        "橋の対岸を同じ局地制圧地域へ数えない"
    );
}

/// 首都という終点と、目の前の地域の制圧工程を同じ座標へ潰さない。
#[test]
fn regional_offensive_has_capital_destination_and_a_separate_current_region() {
    let player = PlayerId(1);
    let (mut world, island) = map6_route_world(player);
    let topology = capital_route_topology_for(&world, player, island).unwrap();
    let home = topology.nodes[topology.start_node.0].anchor;
    let goal = topology.nodes[topology.goal_node.0].anchor;
    let front = capital_route_dag_frontier_properties(&world, player, island)
        .unwrap()
        .into_iter()
        .flatten()
        .next()
        .unwrap();
    let stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Tank.as_str().to_owned(),
        ))
        .unwrap();
    let entity = world
        .spawn((
            Faction(player),
            home,
            stats,
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    let id = assign_route_squad(&mut world, player, MissionType::Attack, &[entity]);
    let mut manager = world
        .remove_resource::<crate::ai::squad::SquadManager>()
        .unwrap();
    let squad = manager
        .squads
        .iter_mut()
        .find(|squad| squad.id == id)
        .unwrap();
    squad.plan.target = Some(front);
    squad.plan.target_island = Some(island);
    super::commitment::refresh_capital_route_path_commitments(&mut world, player, &mut manager);
    let order = &world.resource::<CapitalRoutePathRegistry>().commitments[&id];
    assert_eq!(order.target, goal, "前進作戦は首都という終点を失わない");
    assert_ne!(
        order.target_anchor, goal,
        "手前の未制圧地域の処理を飛ばさない"
    );
    assert_eq!(order.phase, CapitalRouteExecutionPhase::Advance);
}

/// DAGの制圧役を指示台帳だけに留めず、実際の攻撃可否まで渡す。
#[test]
fn advancing_control_accepts_required_attack_but_holding_defense_does_not() {
    use crate::components::Ammo;
    let (mut world, entity, squad) = standard_region_world();
    world.entity_mut(entity).insert(pos(13, 2));
    let enemy_stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::TankZ.as_str().to_owned(),
        ))
        .unwrap();
    let enemy = world
        .spawn((
            Faction(PlayerId(2)),
            pos(14, 2),
            Health {
                current: 100,
                max: 100,
            },
            Ammo {
                ammo1: enemy_stats.max_ammo1,
                max_ammo1: enemy_stats.max_ammo1,
                ammo2: enemy_stats.max_ammo2,
                max_ammo2: enemy_stats.max_ammo2,
            },
            enemy_stats,
        ))
        .id();
    let mut chart = world.resource_mut::<crate::resources::DamageChart>();
    chart.insert_damage(UnitType::Tank, UnitType::TankZ, 5);
    chart.insert_damage(UnitType::TankZ, UnitType::Tank, 90);
    let mut paths = world.resource_mut::<CapitalRoutePathRegistry>();
    let order = paths.commitments.get_mut(&squad).unwrap();
    order.phase = CapitalRouteExecutionPhase::Advance;
    order.scope = CapitalRouteNodeScope::Area;
    order.control_area = vec![pos(14, 2)];
    order.execution_target = pos(14, 2);
    let decision =
        crate::ai::tactics::v2::decide_action(&mut world, PlayerId(1), &HashSet::default());
    assert!(
        matches!(decision, Some((actor, crate::ai::command::AiCommand::Attack { target_entity, .. })) if actor == entity && target_entity == enemy),
        "前進Controlの必要攻撃をDefenseとして拒否しない: {decision:?}"
    );
    world
        .resource_mut::<CapitalRoutePathRegistry>()
        .commitments
        .get_mut(&squad)
        .unwrap()
        .phase = CapitalRouteExecutionPhase::Hold;
    let decision =
        crate::ai::tactics::v2::decide_action(&mut world, PlayerId(1), &HashSet::default());
    assert!(
        !matches!(decision, Some((_, crate::ai::command::AiCommand::Attack { target_entity, .. })) if target_entity == enemy),
        "Holdの守備隊まで不利交換へ転用しない: {decision:?}"
    );
}

/// 橋を越える先遣だけをAdvanceにして、橋を撃ち開く戦車を後方待機へ落とさない。
#[test]
fn control_bridge_assigns_both_crossing_lead_and_combat_support() {
    let master = MasterDataRegistry::load().unwrap();
    let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
        &master,
        "map_26",
        GridTopology::Square,
    )
    .unwrap();
    let player = PlayerId(1);
    let ids = world
        .iter_entities()
        .filter_map(|entity| {
            let property = entity.get::<Property>()?;
            (property.terrain != Terrain::Capital || property.owner_id == Some(player))
                .then_some(entity.id())
        })
        .collect::<Vec<_>>();
    for id in ids {
        world.get_mut::<Property>(id).unwrap().owner_id = Some(player);
    }
    prepare_capital_route_topologies(&mut world, player);
    ensure_capital_route_node_operations(&mut world, player);
    let (key, topology) = world
        .resource::<CapitalRouteTopologyRegistry>()
        .topologies
        .iter()
        .find(|(key, _)| key.player_id == player)
        .unwrap();
    let island = key.island_id;
    let goal = topology.nodes[topology.goal_node.0].anchor;
    let gate = topology.routes[0].front;
    let map = world.resource::<Map>();
    let bank = map
        .get_adjacent(gate.x, gate.y)
        .into_iter()
        .map(|(x, y)| pos(x, y))
        .find(|at| {
            topology
                .from_home
                .get(at)
                .is_some_and(|progress| *progress < topology.from_home[&gate])
                && map.get_terrain(at.x, at.y) != Some(Terrain::Bridge)
        })
        .unwrap();
    let mut squads = Vec::new();
    for (unit_type, mission) in [
        (UnitType::Infantry, MissionType::Capture),
        (UnitType::Tank, MissionType::Attack),
        (UnitType::Artillery, MissionType::Attack),
        (UnitType::HeavySpGun, MissionType::Attack),
    ] {
        let stats = master
            .create_unit_stats(&crate::resources::master_data::UnitName(
                unit_type.as_str().to_owned(),
            ))
            .unwrap();
        let entity = world
            .spawn((
                Faction(player),
                bank,
                stats,
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        squads.push(assign_route_squad(&mut world, player, mission, &[entity]));
    }
    let mut manager = world
        .remove_resource::<crate::ai::squad::SquadManager>()
        .unwrap();
    for squad in &mut manager.squads {
        squad.plan.target = Some(goal);
        squad.plan.target_island = Some(island);
    }
    super::commitment::refresh_capital_route_path_commitments(&mut world, player, &mut manager);
    let paths = world.resource::<CapitalRoutePathRegistry>();
    for id in &squads {
        let order = &paths.commitments[id];
        assert_eq!(order.scope, CapitalRouteNodeScope::Point);
        assert_eq!(
            order.phase,
            CapitalRouteExecutionPhase::Advance,
            "通過役の枠を使わず、複数の間接火力を橋の戦闘へ送る"
        );
    }
    assert_eq!(
        paths.commitments[&squads[0]].target_anchor,
        paths.commitments[&squads[1]].target_anchor
    );
}

fn topology_route_count(
    world: &World,
    player: PlayerId,
    island: crate::ai::islands::IslandId,
) -> usize {
    capital_route_topology_for(world, player, island)
        .unwrap()
        .routes
        .len()
}

/// 砲兵の射撃位置と進軍作戦の最終目的を混同しない。
#[test]
fn gun_route_membership_preserves_capital_destination_despite_rear_firing_position() {
    let player = PlayerId(1);
    let (mut world, island) = map6_route_world(player);
    let topology = capital_route_topology_for(&world, player, island).unwrap();
    let home = topology.nodes[topology.start_node.0].anchor;
    let goal = topology.nodes[topology.goal_node.0].anchor;
    let property = capital_route_dag_frontier_properties(&world, player, island)
        .unwrap()
        .into_iter()
        .flatten()
        .next()
        .unwrap();
    let front = topology.routes[topology.property_routes[&property].0].front;
    let stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Artillery.as_str().to_owned(),
        ))
        .unwrap();
    let entity = world
        .spawn((
            Faction(player),
            home,
            stats,
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    let id = assign_route_squad(&mut world, player, MissionType::Attack, &[entity]);
    let mut manager = world
        .remove_resource::<crate::ai::squad::SquadManager>()
        .unwrap();
    let squad = manager
        .squads
        .iter_mut()
        .find(|squad| squad.id == id)
        .unwrap();
    // 射撃位置は自領後方でも、本隊の進軍路と最終目的は失わない。
    squad.plan.target = Some(home);
    squad.plan.target_island = Some(island);
    manager.route_operation_members.insert(entity, front);
    super::commitment::refresh_capital_route_path_commitments(&mut world, player, &mut manager);
    let order = &world.resource::<CapitalRoutePathRegistry>().commitments[&id];
    // 枝は割当枠と小隊目標で決まる（出撃位置から最寄りの枝へ固定しない）。
    assert!(order.route.0 < topology_route_count(&world, player, island));
    assert_eq!(order.target, goal, "前進作戦は首都という終点を失わない");
    assert_ne!(
        order.target_anchor, goal,
        "手前の未制圧地域の処理を飛ばさない"
    );
    assert_eq!(order.phase, CapitalRouteExecutionPhase::Advance);
}

/// 橋の出口を踏む役と、その外から撃つ役の完了条件を混同しない。
#[test]
fn point_fire_support_does_not_require_occupancy_while_assault_units_advance() {
    use crate::ai::operation_assignment::{
        OperationOwner, OperationUnitRole, UnitOperationAssignment, UnitOperationRegistry,
    };
    let (mut world, gun, squad) = standard_region_world();
    let gun_stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Artillery.as_str().to_owned(),
        ))
        .unwrap();
    world.entity_mut(gun).insert(gun_stats);
    let mut paths = world.resource_mut::<CapitalRoutePathRegistry>();
    let order = paths.commitments.get_mut(&squad).unwrap();
    order.phase = CapitalRouteExecutionPhase::Advance;
    order.scope = CapitalRouteNodeScope::Point;
    order.objective = CapitalRouteNodeObjective::Control;
    order.control_area = vec![pos(14, 2)];
    order.execution_target = pos(14, 2);
    order.target_anchor = pos(14, 2);
    let direct_order = order.clone();
    let stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Tank.as_str().to_owned(),
        ))
        .unwrap();
    let tank = world
        .spawn((
            Faction(PlayerId(1)),
            pos(5, 2),
            stats,
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    let tank_squad = assign_route_squad(&mut world, PlayerId(1), MissionType::Attack, &[tank]);
    world.resource_mut::<UnitOperationRegistry>().assign(
        tank,
        UnitOperationAssignment {
            owner: OperationOwner::Campaign {
                player_id: PlayerId(1),
                island_id: crate::ai::islands::IslandId(0),
            },
            squad_id: Some(tank_squad),
            role: OperationUnitRole::Member,
            assigned_turn: 1,
        },
    );
    world
        .resource_mut::<CapitalRoutePathRegistry>()
        .commitments
        .insert(tank_squad, direct_order);
    assert!(
        !super::tactical::capital_route_target_requires_occupancy(
            &world,
            PlayerId(1),
            gun,
            pos(14, 2)
        ),
        "間接火力は出口へ乗らず射程内から支える"
    );
    assert!(
        super::tactical::capital_route_target_requires_occupancy(
            &world,
            PlayerId(1),
            tank,
            pos(14, 2)
        ),
        "戦車は射程内の手前で接近完了とせず、空いた出口へ進む"
    );
    world.despawn(tank);
    assert!(
        super::tactical::capital_route_target_requires_occupancy(
            &world,
            PlayerId(1),
            gun,
            pos(14, 2)
        ),
        "通過役がいなくなれば砲兵自身でも出口へ進める"
    );
}

/// 通常戦術（tactics/v2）は攻撃対象を地域内外で絞り込まず、既存の採点だけで選ぶ。
/// 地域の障害を先に撃つ順序付けは、手番ループの撃破→進入の段（fire_follow_order）が担う。
/// （試作の「地域外の有利交換を戦略標的から外す」制約は計画3-2で差し戻した。）
#[test]
fn standard_tactics_do_not_filter_attack_targets_by_region() {
    let (mut world, gun, squad) = standard_region_world();
    let stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Artillery.as_str().to_owned(),
        ))
        .unwrap();
    world.entity_mut(gun).insert(stats);
    let make_enemy = |world: &mut World, kind: UnitType, at, hp| {
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                kind.as_str().to_owned(),
            ))
            .unwrap();
        world
            .spawn((
                Faction(PlayerId(2)),
                at,
                stats,
                Health {
                    current: hp,
                    max: 100,
                },
            ))
            .id()
    };
    let blocker = make_enemy(&mut world, UnitType::Infantry, pos(14, 2), 100);
    let unrelated = make_enemy(&mut world, UnitType::HeavySpGun, pos(5, 2), 20);
    let mut chart = world.resource_mut::<crate::resources::DamageChart>();
    chart.insert_damage(UnitType::Artillery, UnitType::Infantry, 95);
    chart.insert_damage(UnitType::Artillery, UnitType::HeavySpGun, 88);
    let mut paths = world.resource_mut::<CapitalRoutePathRegistry>();
    let order = paths.commitments.get_mut(&squad).unwrap();
    order.phase = CapitalRouteExecutionPhase::Advance;
    order.scope = CapitalRouteNodeScope::Point;
    order.objective = CapitalRouteNodeObjective::Control;
    order.control_area = vec![pos(14, 2)];
    order.target_anchor = pos(14, 2);
    order.execution_target = pos(14, 2);
    let decision =
        crate::ai::tactics::v2::decide_action(&mut world, PlayerId(1), &HashSet::default());
    assert!(
        matches!(decision, Some((actor, crate::ai::command::AiCommand::Attack { target_entity, .. }))
        if actor==gun && target_entity==unrelated),
        "地域外の有利な射撃を禁止しない（採点どおり高価な敵を撃つ）: {decision:?}"
    );
    world.despawn(unrelated);
    let decision =
        crate::ai::tactics::v2::decide_action(&mut world, PlayerId(1), &HashSet::default());
    assert!(
        matches!(decision, Some((actor, crate::ai::command::AiCommand::Attack { target_entity, .. }))
        if actor==gun && target_entity==blocker),
        "地域内の障害も通常の攻撃候補に残る: {decision:?}"
    );
}

/// 工場からの退避だけ古いSquad目標へ進むと、最初の一歩で別の進軍路へ逸れる。
#[test]
fn standard_factory_relief_uses_each_units_committed_region() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::MainCompatible);
    let (mut world, entity, squad) = standard_region_world();
    let player = PlayerId(1);
    for (at, terrain) in [
        (pos(10, 2), Terrain::Factory),
        (pos(10, 3), Terrain::Capital),
    ] {
        world
            .resource_mut::<Map>()
            .set_terrain(at.x, at.y, terrain)
            .unwrap();
        world.spawn((at, Property::new(terrain, Some(player), 100)));
    }
    world
        .resource_mut::<CapitalRoutePathRegistry>()
        .commitments
        .get_mut(&squad)
        .unwrap()
        .phase = CapitalRouteExecutionPhase::Advance;
    let decision = crate::ai::tactics::relief::decide_early_campaign_site_relief(
        &mut world,
        player,
        &HashSet::default(),
        true,
    );
    assert!(
        matches!(decision, Some((actor, crate::ai::command::AiCommand::Wait { target_pos }))
        if actor==entity && target_pos.x < 10),
        "旧Squadの東側目標でなく確定済み西側地域へ工場を空ける: {decision:?}"
    );
}

/// 敵が残る地域への進入と射撃支援を同じ「射程内への接近」で終わらせない。
#[test]
fn direct_assault_keeps_physical_entry_while_regional_enemy_remains() {
    let (mut world, entity, squad) = standard_region_world();
    world.entity_mut(entity).insert(pos(13, 2));
    let stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Infantry.as_str().to_owned(),
        ))
        .unwrap();
    let enemy = world
        .spawn((
            Faction(PlayerId(2)),
            pos(15, 2),
            stats,
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    world
        .resource_mut::<crate::resources::DamageChart>()
        .insert_damage(UnitType::Tank, UnitType::Infantry, 70);
    let mut paths = world.resource_mut::<CapitalRoutePathRegistry>();
    let order = paths.commitments.get_mut(&squad).unwrap();
    order.phase = CapitalRouteExecutionPhase::Advance;
    order.scope = CapitalRouteNodeScope::Area;
    order.objective = CapitalRouteNodeObjective::Control;
    order.control_area = vec![pos(14, 2), pos(15, 2)];
    order.execution_target = pos(14, 2);
    assert_eq!(
        super::tactical::capital_route_tactical_target(&world, PlayerId(1), entity),
        Some(Some(pos(14, 2))),
        "敵への射程ではなく空き入口への実進入を要求する"
    );
    assert!(super::tactical::capital_route_target_requires_occupancy(
        &world,
        PlayerId(1),
        entity,
        pos(14, 2)
    ));
    let decision =
        crate::ai::tactics::v2::decide_action(&mut world, PlayerId(1), &HashSet::default());
    assert!(
        matches!(decision, Some((actor, crate::ai::command::AiCommand::Attack { target_entity, target_pos })) if actor == entity && target_entity == enemy && target_pos == pos(14, 2)),
        "地域内に進入し、残敵への攻撃を継続する: {decision:?}"
    );
}
