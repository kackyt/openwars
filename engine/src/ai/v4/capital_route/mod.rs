//! 首都ルートDAG。各利用側は責務別の担当モジュールを参照する。

pub(crate) mod commitment;
pub(crate) mod force_demand;
pub(crate) mod model;
pub(crate) mod node_operation;
pub(crate) mod tactical;
pub(crate) mod topology;

#[cfg(test)]
mod tests;

/// 静的DAGを準備してから、そのトポロジーに対応するNode作戦を用意する。
pub(crate) fn prepare_capital_route_topologies(
    world: &mut bevy_ecs::prelude::World,
    player_id: crate::components::PlayerId,
) {
    topology::cache_capital_route_topologies(world, player_id);
    node_operation::ensure_capital_route_node_operations(world, player_id);
}

/// 橋頭堡を起点にした首都攻略DAG（自軍首都の無い敵首都島）に居る自軍unit。
pub(crate) fn beachhead_route_units(
    world: &bevy_ecs::prelude::World,
    player_id: crate::components::PlayerId,
) -> crate::collections::HashSet<bevy_ecs::prelude::Entity> {
    let islands = world
        .get_resource::<model::CapitalRouteTopologyRegistry>()
        .map(|registry| {
            registry
                .topologies
                .iter()
                .filter(|(key, topology)| key.player_id == player_id && topology.beachhead)
                .map(|(key, _)| key.island_id)
                .collect::<crate::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() else {
        return crate::collections::HashSet::default();
    };
    if islands.is_empty() {
        return crate::collections::HashSet::default();
    }
    world
        .iter_entities()
        .filter(|entity| {
            entity
                .get::<crate::components::Faction>()
                .is_some_and(|faction| faction.0 == player_id)
                && entity.get::<crate::components::UnitStats>().is_some()
                && entity.get::<crate::components::Transporting>().is_none()
                && entity
                    .get::<crate::components::GridPosition>()
                    .and_then(|position| island_map.get_island_at(position))
                    .is_some_and(|island| islands.contains(&island.id))
        })
        .map(|entity| entity.id())
        .collect()
}

/// 橋頭堡を起点にした首都攻略DAGの起点（揚陸部隊を届ける浜の陸側）。
pub(crate) fn beachhead_route_origins(
    world: &bevy_ecs::prelude::World,
    player_id: crate::components::PlayerId,
) -> Vec<crate::components::GridPosition> {
    let mut origins: Vec<_> = world
        .get_resource::<model::CapitalRouteTopologyRegistry>()
        .map(|registry| {
            registry
                .topologies
                .iter()
                .filter(|(key, topology)| key.player_id == player_id && topology.beachhead)
                .map(|(_, topology)| topology.nodes[topology.start_node.0].anchor)
                .collect()
        })
        .unwrap_or_default();
    origins.sort_unstable_by_key(|position| (position.y, position.x));
    origins
}

/// 診断用: unitの所属Squadが持つ首都攻略DAGの指令の要約。
pub(crate) fn describe_route_order(
    world: &bevy_ecs::prelude::World,
    entity: bevy_ecs::prelude::Entity,
) -> String {
    let Some(manager) = world.get_resource::<crate::ai::squad::SquadManager>() else {
        return "no-manager".into();
    };
    let Some(squad) = manager
        .squads
        .iter()
        .find(|squad| squad.members.contains(&entity))
    else {
        return "no-squad".into();
    };
    let order = world
        .get_resource::<model::CapitalRoutePathRegistry>()
        .and_then(|registry| registry.commitments.get(&squad.id))
        .map(|commitment| {
            format!(
                "route{:?} {:?}/{:?} exec{:?} targets{:?}",
                commitment.route,
                commitment.objective,
                commitment.scope,
                (commitment.execution_target.x, commitment.execution_target.y),
                commitment
                    .capture_targets
                    .iter()
                    .map(|target| (target.x, target.y))
                    .collect::<Vec<_>>()
            )
        })
        .unwrap_or_else(|| "no-order".into());
    format!(
        "{:?}/{:?} {order}",
        squad.plan.mission_type, squad.plan.phase
    )
}

/// 橋頭堡を起点にした首都攻略DAGがある島（揚陸済みの敵首都島）。
pub(crate) fn beachhead_route_islands(
    world: &bevy_ecs::prelude::World,
    player_id: crate::components::PlayerId,
) -> crate::collections::HashSet<crate::ai::islands::IslandId> {
    world
        .get_resource::<model::CapitalRouteTopologyRegistry>()
        .map(|registry| {
            registry
                .topologies
                .iter()
                .filter(|(key, topology)| key.player_id == player_id && topology.beachhead)
                .map(|(key, _)| key.island_id)
                .collect()
        })
        .unwrap_or_default()
}

/// 橋頭堡を起点にした首都攻略の、艦砲で支援すべき地点。
///
/// - 起点（橋頭堡）
/// - 各進軍路の前線拠点（次に確保する拠点）
/// - 前提が解放済みで、敵が局地優勢な（Contested）Nodeのanchor
///
/// 戦艦はこれらの周りの敵を優先して撃ち、撃てない戦艦はこれらを射程に捉える位置へ移る。
pub(crate) fn beachhead_support_anchors(
    world: &bevy_ecs::prelude::World,
    player_id: crate::components::PlayerId,
) -> Vec<crate::components::GridPosition> {
    let mut anchors = Vec::new();
    let Some(registry) = world.get_resource::<model::CapitalRouteTopologyRegistry>() else {
        return anchors;
    };
    for (key, topology) in registry
        .topologies
        .iter()
        .filter(|(key, topology)| key.player_id == player_id && topology.beachhead)
    {
        anchors.push(topology.nodes[topology.start_node.0].anchor);
        if let Some(fronts) =
            node_operation::capital_route_current_fronts(world, player_id, key.island_id)
        {
            anchors.extend(fronts);
        }
        if let Some(operations) = world.get_resource::<model::CapitalRouteNodeOperationRegistry>() {
            anchors.extend(
                operations
                    .operations
                    .values()
                    .filter(|operation| {
                        operation.id.topology == *key
                            && operation.state
                                == crate::ai::v4::victory_roadmap::RoadmapNodeState::Contested
                    })
                    .map(|operation| operation.anchor),
            );
        }
    }
    anchors.sort_unstable_by_key(|position| (position.y, position.x));
    anchors.dedup();
    anchors
}
