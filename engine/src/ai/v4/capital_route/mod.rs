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
