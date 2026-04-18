use crate::components::*;
use crate::systems::{combat, merge, supply, transport};
use bevy_ecs::prelude::*;

/// ユニットが現在実行可能なアクションをまとめた構造体
#[derive(Debug, Clone, Copy)]
pub struct AvailableActions {
    pub can_attack: bool,
    pub can_capture: bool,
    pub can_repair: bool, // 自軍拠点修復が可能か
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

    let (can_capture, can_repair) = {
        let (unit_pos, unit_stats, unit_faction) = {
            let mut q_unit = world.query::<(&GridPosition, &UnitStats, &Faction)>();
            let Ok((u_pos, u_stats, u_faction)) = q_unit.get(world, unit_entity) else {
                return AvailableActions {
                    can_attack: false,
                    can_capture: false,
                    can_repair: false,
                    can_supply: false,
                    can_load: false,
                    can_drop: false,
                    can_merge: false,
                    can_wait: false,
                };
            };
            (*u_pos, u_stats.clone(), u_faction.0)
        };

        if !unit_stats.can_capture {
            (false, false)
        } else {
            let mut capturable = false;
            let mut repairable = false;
            let mut q_properties = world.query::<(&GridPosition, &Property)>();
            for (p_pos, p_prop) in q_properties.iter(world) {
                if p_pos.x == unit_pos.x && p_pos.y == unit_pos.y {
                    let max_points = p_prop.max_capture_points;
                    if max_points > 0 {
                        if p_prop.owner_id == Some(unit_faction) {
                            if p_prop.capture_points < max_points {
                                repairable = true;
                            }
                        } else {
                            capturable = true;
                        }
                    }
                    break;
                }
            }
            (capturable, repairable)
        }
    };

    AvailableActions {
        can_attack: !combat::get_attackable_targets(world, unit_entity, !is_moved).is_empty(),
        can_capture,
        can_repair,
        can_supply: !supply::get_suppliable_targets(world, unit_entity).is_empty(),
        can_load,
        can_drop: {
            let mut can_drop = false;
            let mut q_cargo = world.query::<&CargoCapacity>();
            if let Ok(cargo) = q_cargo.get(world, unit_entity) {
                for &passenger in &cargo.loaded {
                    if let Some(action) = world.get::<ActionCompleted>(passenger) {
                        if !action.0 {
                            can_drop = true;
                            break;
                        }
                    }
                }
            }
            can_drop
        },
        can_merge,
        // 移動先が搭載または合流対象である場合、待機は不可（重なり防止）
        can_wait: !is_moved || (!can_load && !can_merge),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                ..UnitStats::mock()
            },
        );
        registry.insert(
            t_type,
            UnitStats {
                unit_type: t_type,
                max_cargo: 1,
                loadable_unit_types: vec![u_type],
                ..UnitStats::mock()
            },
        );
        world.insert_resource(UnitRegistry(registry));

        // プレイヤー設定
        let player_id = PlayerId(1);

        // 輸送ユニット設置 (SupplyTruck)
        let _ = world
            .spawn((
                GridPosition { x: 1, y: 1 },
                Faction(player_id),
                UnitStats {
                    unit_type: t_type,
                    max_cargo: 1,
                    loadable_unit_types: vec![u_type],
                    ..UnitStats::mock()
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
                    ..UnitStats::mock()
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
