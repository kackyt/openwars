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
    pub can_merge: bool,
    pub can_wait: bool,
}

/// 指定されたユニットが現在実行可能なアクションを判定して返します。
pub fn get_available_actions(
    world: &mut World,
    unit_entity: Entity,
    is_moved: bool,
) -> AvailableActions {
    let can_load = !transport::get_loadable_transports(world, unit_entity).is_empty();
    let can_merge = !merge::get_mergable_targets(world, unit_entity).is_empty();

    AvailableActions {
        can_attack: !combat::get_attackable_targets(world, unit_entity, !is_moved).is_empty(),
        can_capture: property::get_capturable_property(world, unit_entity).is_some(),
        can_supply: !supply::get_suppliable_targets(world, unit_entity).is_empty(),
        can_load,
        can_drop: {
            let mut has_passengers = false;
            let mut q_cargo = world.query::<&CargoCapacity>();
            if let Ok(cargo) = q_cargo.get(world, unit_entity) {
                has_passengers = !cargo.loaded.is_empty();
            }
            has_passengers
        },
        can_merge,
        // 移動先が搭載または合流対象である場合、待機は不可（重なり防止）
        can_wait: !is_moved || (!can_load && !can_merge),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::*;
    use crate::resources::*;

    #[test]
    fn test_get_available_actions_on_transport() {
        let mut world = World::new();

        // ユニット種別登録
        let mut registry = std::collections::HashMap::new();
        let u_type = UnitType::Infantry;
        let t_type = UnitType::SupplyTruck;

        registry.insert(
            u_type,
            UnitStats {
                unit_type: u_type,
                ..Default::default()
            },
        );
        registry.insert(
            t_type,
            UnitStats {
                unit_type: t_type,
                max_cargo: 1,
                loadable_unit_types: vec![u_type],
                ..Default::default()
            },
        );
        world.insert_resource(UnitRegistry(registry));

        // プレイヤー設定
        let player_id = PlayerId(1);

        // 輸送ユニット設置 (SupplyTruck)
        let apc = world
            .spawn((
                GridPosition { x: 1, y: 1 },
                Faction(player_id),
                UnitStats {
                    unit_type: t_type,
                    max_cargo: 1,
                    loadable_unit_types: vec![u_type],
                    ..Default::default()
                },
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
            ))
            .id();

        // 歩兵ユニット設置 (APCと同じ位置)
        let infantry = world
            .spawn((
                GridPosition { x: 1, y: 1 },
                Faction(player_id),
                UnitStats {
                    unit_type: u_type,
                    ..Default::default()
                },
            ))
            .id();

        // 移動後のアクション判定
        let actions = get_available_actions(&mut world, infantry, true);

        assert!(actions.can_load, "Should be able to load into APC");
        assert!(
            !actions.can_wait,
            "Should NOT be able to wait on APC (overlapping)"
        );
        assert!(
            !actions.can_merge,
            "Should NOT be able to merge (different unit types/not compatible)"
        );

        // 移動前（待機中）ならWaitは可能
        let actions_before = get_available_actions(&mut world, infantry, false);
        assert!(
            actions_before.can_wait,
            "Wait should be allowed if not moved yet"
        );
    }
}
