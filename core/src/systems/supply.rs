use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;

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
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::MovementAndAttack {
        return;
    }
    let active_player_id = players.0[match_state.active_player_index].id;

    for event in supply_events.read() {
        let (sup_pos, sup_faction, sup_stats, sup_hp, sup_action) =
            match q_units.get_mut(event.supplier_entity) {
                Ok((_, p, f, s, h, a, _, _)) => (p.clone(), f.0, s.clone(), h.clone(), a),
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
            Ok((_, p, f, _, h, _, _, _)) => (p.clone(), f.0, h.clone()),
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
        if let Ok([mut supplier, target]) =
            q_units.get_many_mut([event.supplier_entity, event.target_entity])
        {
            supplier.5.0 = true; // Action completed

            let max_fuel = target.3.max_fuel;
            if let Some(mut fuel) = target.6 {
                fuel.current = max_fuel;
            }

            let max_a1 = target.3.max_ammo1;
            let max_a2 = target.3.max_ammo2;
            if let Some(mut ammo) = target.7 {
                ammo.ammo1 = max_a1;
                ammo.ammo2 = max_a2;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supply_unit_system() {
        let mut world = World::new();

        let mut match_state = MatchState::default();
        match_state.current_phase = Phase::MovementAndAttack;
        world.insert_resource(match_state);
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));

        world.insert_resource(Events::<SupplyUnitCommand>::default());

        let supplier_entity = world
            .spawn((
                GridPosition { x: 2, y: 2 },
                Faction(1),
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    unit_type: UnitType::SupplyTruck,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: MovementType::Tires,
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
                },
                ActionCompleted(false),
            ))
            .id();

        let target_entity = world
            .spawn((
                GridPosition { x: 3, y: 2 },
                Faction(1),
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: MovementType::Foot,
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
    }
}
