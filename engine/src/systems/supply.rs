use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;

/// 補給車などによる隣接ユニットへの補給コマンド(`SupplyUnitCommand`)を処理するシステム。
///
/// 【処理の流れ】
/// 1. 補給者(`supplier_entity`)が自軍であり、行動済みでなく、補給能力(`can_supply`)を持つことを確認します。
/// 2. 補給対象(`target_entity`)が自軍であり、補給者と隣接（距離が1）していることを確認します。
/// 3. 対象の燃料(`Fuel`)と弾薬(`Ammo`)を最大値まで回復(`resupply`)させます。
/// 4. 補給者の `ActionCompleted` を true に設定します。
///
/// 指定されたユニットが現在補給可能な対象エンティティのリストを返します。
/// 補給能力（can_supply）を持つユニットの隣接マスにいる同勢力ユニットが対象です。
pub fn get_suppliable_targets(world: &mut World, supplier: Entity) -> Vec<Entity> {
    let mut targets = vec![];
    let (s_pos, unit_faction) = {
        let mut q_supplier = world.query::<(&GridPosition, &UnitStats, &Faction)>();
        let Ok((s_pos, s_stats, s_faction)) = q_supplier.get(world, supplier) else {
            return targets;
        };

        if !s_stats.can_supply {
            return targets;
        }
        (*s_pos, s_faction.0)
    };

    let mut q_targets = world.query_filtered::<(Entity, &GridPosition, &Faction), With<Faction>>();
    for (t_ent, t_pos, t_faction) in q_targets.iter(world) {
        if t_ent != supplier && t_faction.0 == unit_faction {
            let dist = (s_pos.x as i64 - t_pos.x as i64).unsigned_abs() as u32
                + (s_pos.y as i64 - t_pos.y as i64).unsigned_abs() as u32;
            if dist == 1 {
                targets.push(t_ent);
            }
        }
    }

    targets
}

#[allow(clippy::type_complexity)]
pub fn supply_unit_system(
    mut supply_events: EventReader<SupplyUnitCommand>,
    mut q_units: Query<(
        Entity,
        &GridPosition,
        &Faction,
        &UnitStats,
        &Health,
        &mut ActionCompleted,
        Option<&mut Fuel>,
        Option<&mut Ammo>,
    )>,
    match_state: Res<MatchState>,
    players: Res<Players>,
    mut commands: Commands,
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Main {
        return;
    }
    let active_player_id = players.0[match_state.active_player_index.0].id;

    for event in supply_events.read() {
        let (sup_pos, sup_faction, sup_stats, sup_hp, sup_action) =
            match q_units.get_mut(event.supplier_entity) {
                Ok((_, p, f, s, h, a, _, _)) => (*p, f.0, s.clone(), *h, a),
                _ => continue,
            };

        if sup_faction != active_player_id
            || sup_action.0
            || sup_hp.is_destroyed()
            || !sup_stats.can_supply
        {
            continue;
        }

        let (tar_pos, tar_faction, tar_hp) = match q_units.get(event.target_entity) {
            Ok((_, p, f, _, h, _, _, _)) => (*p, f.0, *h),
            _ => continue,
        };

        if tar_faction != active_player_id || tar_hp.is_destroyed() {
            continue;
        }

        let dist = (sup_pos.x as i64 - tar_pos.x as i64).unsigned_abs() as u32
            + (sup_pos.y as i64 - tar_pos.y as i64).unsigned_abs() as u32;

        if dist != 1 {
            continue;
        }

        // Apply supply using get_many_mut
        if let Ok([supplier, target]) =
            q_units.get_many_mut([event.supplier_entity, event.target_entity])
        {
            let (_, _, _, _, _, mut sup_action, _, _) = supplier;
            let (_, _, _, tar_stats, _, _, tar_fuel_opt, tar_ammo_opt) = target;

            sup_action.0 = true; // 補給者は行動完了状態になる

            let max_fuel = tar_stats.max_fuel;
            if let Some(mut fuel) = tar_fuel_opt {
                fuel.current = max_fuel; // 燃料を最大値まで回復
            }

            let max_a1 = tar_stats.max_ammo1;
            let max_a2 = tar_stats.max_ammo2;
            if let Some(mut ammo) = tar_ammo_opt {
                ammo.ammo1 = max_a1; // 主武器と副武器の弾薬を最大値まで回復
                ammo.ammo2 = max_a2;
            }
            // 補給確定時に移動履歴を削除
            commands.remove_resource::<PendingMove>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supply_unit_system() {
        let mut world = World::new();

        let ms = MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        };
        world.insert_resource(ms);
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));

        world.insert_resource(Events::<SupplyUnitCommand>::default());

        let supplier_entity = world
            .spawn((
                GridPosition { x: 2, y: 2 },
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    unit_type: UnitType::SupplyTruck,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: MovementType::ArmoredCar,
                    max_fuel: 99,
                    max_ammo1: 0,
                    max_ammo2: 0,
                    min_range: 1,
                    max_range: 1,
                    daily_fuel_consumption: 0,
                    can_capture: false,
                    can_supply: true, // CAN SUPPLY
                    max_cargo: 0,
                    loadable_unit_types: vec![],
                    ..UnitStats::mock()
                },
                ActionCompleted(false),
            ))
            .id();

        let target_entity = world
            .spawn((
                GridPosition { x: 3, y: 2 },
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: MovementType::Infantry,
                    max_fuel: 99,
                    max_ammo1: 9,
                    max_ammo2: 0,
                    min_range: 1,
                    max_range: 1,
                    daily_fuel_consumption: 0,
                    can_capture: true,
                    can_supply: false,
                    max_cargo: 0,
                    loadable_unit_types: vec![],
                    ..UnitStats::mock()
                },
                ActionCompleted(false),
                Fuel {
                    current: 10,
                    max: 99,
                },
                Ammo {
                    ammo1: 1,
                    max_ammo1: 9,
                    ammo2: 0,
                    max_ammo2: 0,
                },
            ))
            .id();

        // モックのPendingMoveを挿入
        world.insert_resource(PendingMove {
            unit_entity: supplier_entity,
            original_pos: GridPosition { x: 2, y: 1 },
            original_fuel: Fuel {
                current: 20,
                max: 99,
            },
        });

        world.send_event(SupplyUnitCommand {
            supplier_entity,
            target_entity,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(supply_unit_system);
        schedule.run(&mut world);

        // Check if supplier used its action
        let act1 = world.get::<ActionCompleted>(supplier_entity).unwrap();
        assert!(act1.0);

        // Check if target was supplied
        let fuel = world.get::<Fuel>(target_entity).unwrap();
        assert_eq!(fuel.current, 99);

        let ammo = world.get::<Ammo>(target_entity).unwrap();
        assert_eq!(ammo.ammo1, 9);

        // PendingMove should be removed
        assert!(world.get_resource::<PendingMove>().is_none());
    }

    #[test]
    fn test_get_suppliable_targets() {
        let mut world = World::new();

        let inf_stats = UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1000,
            max_movement: 3,
            movement_type: MovementType::Infantry,
            max_fuel: 99,
            max_ammo1: 9,
            max_ammo2: 0,
            min_range: 1,
            max_range: 1,
            daily_fuel_consumption: 0,
            can_capture: true,
            can_supply: false,
            max_cargo: 0,
            loadable_unit_types: vec![],
            ..UnitStats::mock()
        };

        // Supplier (Supply Truck)
        let supplier = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::SupplyTruck,
                    can_supply: true,
                    ..inf_stats.clone()
                },
            ))
            .id();

        // Valid target (Adjacent, same faction)
        let target_ok = world
            .spawn((
                GridPosition { x: 5, y: 6 },
                Faction(PlayerId(1)),
                inf_stats.clone(),
            ))
            .id();

        // Invalid: Too far
        let _target_far = world
            .spawn((
                GridPosition { x: 5, y: 7 },
                Faction(PlayerId(1)),
                inf_stats.clone(),
            ))
            .id();

        // Invalid: Different faction
        let _target_enemy = world
            .spawn((
                GridPosition { x: 6, y: 5 },
                Faction(PlayerId(2)),
                inf_stats.clone(),
            ))
            .id();

        let targets = get_suppliable_targets(&mut world, supplier);
        assert_eq!(targets.len(), 1);
        assert!(targets.contains(&target_ok));

        // Test with can_supply = false
        world.get_mut::<UnitStats>(supplier).unwrap().can_supply = false;
        let targets_none = get_suppliable_targets(&mut world, supplier);
        assert!(targets_none.is_empty());
    }
}
