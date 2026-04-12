use crate::components::*;
use crate::systems::{combat, merge, property, supply, transport};
use bevy_ecs::prelude::*;

/// ユニットが現在実行可能なアクションをまとめた構造体
#[derive(Debug, Clone, Copy)]
pub struct AvailableActions {
    pub can_attack: bool,
    pub can_capture: bool,
    pub can_supply: bool,
    pub can_load: bool,
    pub can_drop: bool,
    pub can_join: bool,
}

/// 指定されたユニットが現在実行可能なアクションを判定して返します。
pub fn get_available_actions(
    world: &mut World,
    unit_entity: Entity,
    is_moved: bool,
) -> AvailableActions {
    AvailableActions {
        can_attack: !combat::get_attackable_targets(world, unit_entity, !is_moved).is_empty(),
        can_capture: property::get_capturable_property(world, unit_entity).is_some(),
        can_supply: !supply::get_suppliable_targets(world, unit_entity).is_empty(),
        can_load: !transport::get_loadable_transports(world, unit_entity).is_empty(),
        can_drop: {
            let mut has_passengers = false;
            let mut q_cargo = world.query::<&CargoCapacity>();
            if let Ok(cargo) = q_cargo.get(world, unit_entity) {
                has_passengers = !cargo.loaded.is_empty();
            }
            has_passengers
        },
        can_join: !merge::get_mergable_targets(world, unit_entity).is_empty(),
    }
}
