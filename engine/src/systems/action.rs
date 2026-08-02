use crate::components::*;
use crate::resources::{MatchState, Phase, Players};
use crate::systems::{combat, merge, supply, transport};
use bevy_ecs::prelude::*;

/// 指定ユニットを現在のプレイヤーが操作対象として選択できるかを判定します。
pub fn is_unit_selectable(world: &World, unit_entity: Entity) -> bool {
    let Some(match_state) = world.get_resource::<MatchState>() else {
        return false;
    };
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Main {
        return false;
    }

    let Some(active_player) = world
        .get_resource::<Players>()
        .and_then(|players| players.0.get(match_state.active_player_index.0))
    else {
        return false;
    };

    let Some(faction) = world.get::<Faction>(unit_entity) else {
        return false;
    };
    if faction.0 != active_player.id
        || world
            .get::<ActionCompleted>(unit_entity)
            .is_some_and(|action| action.0)
        || world.get::<Transporting>(unit_entity).is_some()
        || world
            .get::<Health>(unit_entity)
            .is_some_and(Health::is_destroyed)
    {
        return false;
    }

    true
}

/// 指定ユニットが新たな移動先を選択できるかを判定します。
pub fn can_unit_move(world: &World, unit_entity: Entity) -> bool {
    is_unit_selectable(world, unit_entity)
        && !world
            .get::<HasMoved>(unit_entity)
            .is_some_and(|has_moved| has_moved.0)
}

/// 既存の移動済み状態と指定座標への実移動の双方を考慮して、移動後かを判定します。
pub fn is_unit_moved_at(world: &World, unit_entity: Entity, destination: GridPosition) -> bool {
    world
        .get::<HasMoved>(unit_entity)
        .is_some_and(|has_moved| has_moved.0)
        || world
            .get::<GridPosition>(unit_entity)
            .is_some_and(|position| *position != destination)
}

/// ユニットが現在実行可能なアクションをまとめた構造体
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
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
    let Some(u_pos) = world.get::<GridPosition>(unit_entity).copied() else {
        return AvailableActions::default();
    };
    get_available_actions_at(world, unit_entity, u_pos, is_moved)
}

/// 指定された位置おける、ユニットの利用可能なアクションを返します。
pub fn get_available_actions_at(
    world: &mut World,
    unit_entity: Entity,
    u_pos: GridPosition,
    is_moved: bool,
) -> AvailableActions {
    let (can_load, can_merge) = {
        let loadable = transport::get_loadable_transports_at(world, unit_entity, u_pos);
        let mergable = merge::get_mergable_targets_at(world, unit_entity, u_pos);
        (!loadable.is_empty(), !mergable.is_empty())
    };

    let (can_capture, can_repair) = {
        let (unit_stats, unit_faction) = {
            let mut q_unit = world.query::<(&UnitStats, &Faction)>();
            let Ok((u_stats, u_faction)) = q_unit.get(world, unit_entity) else {
                return AvailableActions::default();
            };
            (u_stats.clone(), u_faction.0)
        };

        if !unit_stats.can_capture {
            (false, false)
        } else {
            let mut capturable = false;
            let mut repairable = false;
            let mut q_properties = world.query::<(&GridPosition, &Property)>();
            for (p_pos, p_prop) in q_properties.iter(world) {
                if p_pos.x == u_pos.x && p_pos.y == u_pos.y {
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

    let is_occupied_by_other = {
        let mut q_occupants = world
            .query_filtered::<(Entity, &GridPosition), (With<Faction>, Without<Transporting>)>();
        q_occupants
            .iter(world)
            .any(|(e, p)| e != unit_entity && p.x == u_pos.x && p.y == u_pos.y)
    };

    AvailableActions {
        can_attack: !is_occupied_by_other
            && !combat::get_attackable_targets_at(world, unit_entity, u_pos, !is_moved).is_empty(),
        can_capture: !is_occupied_by_other && can_capture,
        can_repair: !is_occupied_by_other && can_repair,
        can_supply: !is_occupied_by_other
            && !supply::get_suppliable_targets_at(world, unit_entity, u_pos).is_empty(),
        can_load,
        can_drop: !is_occupied_by_other && {
            let loaded_passengers = {
                let mut q_cargo = world.query::<&CargoCapacity>();
                q_cargo
                    .get(world, unit_entity)
                    .map(|cargo| cargo.loaded.clone())
                    .unwrap_or_default()
            };
            let mut can_drop = false;
            for passenger in loaded_passengers {
                if let Some(action) = world.get::<ActionCompleted>(passenger)
                    && !action.0
                    && !transport::get_droppable_tiles_at(world, unit_entity, passenger, u_pos)
                        .is_empty()
                {
                    can_drop = true;
                    break;
                }
            }
            can_drop
        },
        can_merge,
        // 空きマスであるか、移動していない（元の位置に留まる）場合のみ待機可能
        // 搭載や合流が可能なマスであっても、通常の「待機」で重なることは許さない
        can_wait: !is_occupied_by_other || !is_moved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::*;

    fn setup_selection_world() -> World {
        let mut world = World::new();
        world.insert_resource(MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        });
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));
        world
    }

    #[test]
    fn test_unit_selection_and_movement_are_decided_by_engine_state() {
        let mut world = setup_selection_world();
        let own_unit = world
            .spawn((
                GridPosition { x: 1, y: 1 },
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                ActionCompleted(false),
                HasMoved(false),
            ))
            .id();
        let moved_unit = world
            .spawn((
                GridPosition { x: 2, y: 2 },
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                ActionCompleted(false),
                HasMoved(true),
            ))
            .id();
        let enemy_unit = world
            .spawn((
                GridPosition { x: 3, y: 3 },
                Faction(PlayerId(2)),
                ActionCompleted(false),
                HasMoved(false),
            ))
            .id();

        assert!(is_unit_selectable(&world, own_unit));
        assert!(can_unit_move(&world, own_unit));
        assert!(is_unit_selectable(&world, moved_unit));
        assert!(!can_unit_move(&world, moved_unit));
        assert!(!is_unit_selectable(&world, enemy_unit));
    }

    #[test]
    fn test_is_unit_moved_at_considers_component_and_destination() {
        let mut world = World::new();
        let unit = world
            .spawn((GridPosition { x: 1, y: 1 }, HasMoved(false)))
            .id();

        assert!(!is_unit_moved_at(&world, unit, GridPosition { x: 1, y: 1 }));
        assert!(is_unit_moved_at(&world, unit, GridPosition { x: 2, y: 1 }));

        world.get_mut::<HasMoved>(unit).unwrap().0 = true;
        assert!(is_unit_moved_at(&world, unit, GridPosition { x: 1, y: 1 }));
    }

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

    #[test]
    fn test_supply_availability_uses_legal_targets_at_destination() {
        let mut world = World::new();
        let supplier = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::SupplyTruck,
                    can_supply: true,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let target = world
            .spawn((
                GridPosition { x: 2, y: 0 },
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::Fighter,
                    ..UnitStats::mock()
                },
            ))
            .id();

        let destination = GridPosition { x: 1, y: 0 };
        assert!(get_available_actions_at(&mut world, supplier, destination, true).can_supply);

        // 行動済みの対象だけでは補給アクションを表示しない
        world.get_mut::<ActionCompleted>(target).unwrap().0 = true;
        assert!(!get_available_actions_at(&mut world, supplier, destination, true).can_supply);

        // 補給対象外の航空ユニットも補給アクションを表示しない
        world.get_mut::<ActionCompleted>(target).unwrap().0 = false;
        world.get_mut::<UnitStats>(target).unwrap().unit_type = UnitType::Bomber;
        assert!(!get_available_actions_at(&mut world, supplier, destination, true).can_supply);
    }

    #[test]
    fn test_cannot_wait_on_occupied_tile() {
        let mut world = World::new();
        let p1 = PlayerId(1);
        let inf_type = UnitType::Infantry;

        // Setup Registry
        let mut registry = std::collections::HashMap::new();
        registry.insert(
            inf_type,
            UnitStats {
                unit_type: inf_type,
                ..UnitStats::mock()
            },
        );
        world.insert_resource(UnitRegistry(registry));

        // Spawn existing unit at (1,0)
        world.spawn((GridPosition { x: 1, y: 0 }, Faction(p1), UnitStats::mock()));

        // Spawn current unit at (0,0)
        let unit = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                Faction(p1),
                UnitStats {
                    unit_type: inf_type,
                    ..UnitStats::mock()
                },
                ActionCompleted(false),
            ))
            .id();

        // Check actions at (1,0) after moving
        let actions = get_available_actions_at(&mut world, unit, GridPosition { x: 1, y: 0 }, true);

        assert!(
            !actions.can_wait,
            "Should not be able to wait on occupied tile"
        );
        // can_merge might be true depending on compatibility, but that's fine.
        // The point is can_wait must be false.
    }
}
