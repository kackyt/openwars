use bevy_ecs::prelude::*;

use crate::components::*;
use crate::events::{MergeUnitCommand, UnitDestroyedEvent, UnitMergedEvent};
use crate::resources::{MatchState, PendingMove, Phase};

pub fn get_mergable_targets(world: &mut World, unit: Entity) -> Vec<Entity> {
    let Some(unit_pos) = world.get::<GridPosition>(unit).cloned() else {
        return vec![];
    };
    get_mergable_targets_at(world, unit, unit_pos)
}

/// 指定された位置で合流可能な対象エンティティのリストを返します。
pub fn get_mergable_targets_at(
    world: &mut World,
    unit: Entity,
    unit_pos: GridPosition,
) -> Vec<Entity> {
    let mut targets = vec![];
    // 搭載中のunitと、搭載物を持つ輸送役は合流させない。
    if world.get::<Transporting>(unit).is_some()
        || world
            .get::<CargoCapacity>(unit)
            .is_some_and(|cargo| !cargo.loaded.is_empty())
    {
        return targets;
    }
    let (unit_stats, unit_faction) = {
        let mut q_unit = world.query::<(&UnitStats, &Faction)>();
        let Ok((stats, faction)) = q_unit.get(world, unit) else {
            return targets;
        };
        (stats.clone(), faction.0)
    };

    let mut q_targets =
        world.query_filtered::<(Entity, &GridPosition, &Faction, &UnitStats), With<Faction>>();
    for (t_ent, t_pos, t_faction, t_stats) in q_targets.iter(world) {
        if t_ent != unit
            && world.get::<Transporting>(t_ent).is_none()
            && world
                .get::<CargoCapacity>(t_ent)
                .is_none_or(|cargo| cargo.loaded.is_empty())
            && t_faction.0 == unit_faction
            && t_stats.unit_type == unit_stats.unit_type
            && unit_pos.x == t_pos.x
            && unit_pos.y == t_pos.y
        {
            targets.push(t_ent);
        }
    }

    targets
}

#[allow(clippy::type_complexity)]
pub fn merge_unit_system(
    mut commands: Commands,
    mut merge_events: EventReader<MergeUnitCommand>,
    mut merged_events: EventWriter<UnitMergedEvent>,
    mut destroyed_events: EventWriter<UnitDestroyedEvent>,
    mut q_units: Query<(
        Entity,
        &mut Health,
        &mut Fuel,
        &mut Ammo,
        &mut ActionCompleted,
        &UnitStats,
        &Faction,
        &GridPosition,
    )>,
    match_state: Res<MatchState>,
    q_transport: Query<(Option<&Transporting>, Option<&CargoCapacity>)>,
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Main {
        return;
    }

    for event in merge_events.read() {
        // 命令が直接送られた場合も、搭載関係を失う合流を拒否する。
        if [event.source_entity, event.target_entity]
            .into_iter()
            .any(|entity| {
                q_transport.get(entity).is_ok_and(|(parent, cargo)| {
                    parent.is_some() || cargo.is_some_and(|capacity| !capacity.loaded.is_empty())
                })
            })
        {
            continue;
        }
        // Collect source data
        let mut source_data = None;
        if let Ok((s_entity, s_health, s_fuel, s_ammo, _, s_stats, s_faction, s_pos)) =
            q_units.get(event.source_entity)
        {
            source_data = Some((
                s_entity,
                s_health.current,
                s_fuel.current,
                s_ammo.ammo1,
                s_ammo.ammo2,
                s_stats.unit_type,
                s_faction.0,
                *s_pos,
            ));
        }

        if let Some((s_entity, s_hp, s_fuel_val, s_ammo1_val, s_ammo2_val, s_type, s_fac, s_pos)) =
            source_data
            && let Ok((
                t_entity,
                mut t_health,
                mut t_fuel,
                mut t_ammo,
                mut t_action,
                t_stats,
                t_faction,
                t_pos,
            )) = q_units.get_mut(event.target_entity)
        {
            // マージ条件を検証
            if s_entity == t_entity
                || s_fac != t_faction.0
                || s_type != t_stats.unit_type
                || s_pos.x != t_pos.x
                || s_pos.y != t_pos.y
            {
                continue;
            }

            // マージを実行
            t_health.current = std::cmp::min(t_health.max, t_health.current + s_hp);
            t_fuel.current = std::cmp::min(t_fuel.max, t_fuel.current + s_fuel_val);
            t_ammo.ammo1 = std::cmp::min(t_ammo.max_ammo1, t_ammo.ammo1 + s_ammo1_val);
            t_ammo.ammo2 = std::cmp::min(t_ammo.max_ammo2, t_ammo.ammo2 + s_ammo2_val);

            t_action.0 = true;

            commands.entity(s_entity).despawn();

            merged_events.send(UnitMergedEvent {
                source_entity: s_entity,
                target_entity: t_entity,
                refunded_funds: 0, // refund logic removed per spec update
            });

            destroyed_events.send(UnitDestroyedEvent { entity: s_entity });

            // 合流確定時に移動履歴を削除
            commands.remove_resource::<PendingMove>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::events::*;
    use crate::resources::*;

    #[test]
    fn test_merge_unit_system() {
        let mut world = World::new();

        let ms = MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        };
        world.insert_resource(ms);

        world.insert_resource(Events::<MergeUnitCommand>::default());
        world.insert_resource(Events::<UnitMergedEvent>::default());
        world.insert_resource(Events::<UnitDestroyedEvent>::default());

        let inf_stats = UnitStats {
            ammo1_cost: 0,
            ammo2_cost: 0,
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
        };

        // Target unit
        let target_entity = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                Health {
                    current: 40,
                    max: 100,
                },
                Fuel {
                    current: 20,
                    max: 99,
                },
                Ammo {
                    ammo1: 2,
                    max_ammo1: 9,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                inf_stats.clone(),
                ActionCompleted(false),
            ))
            .id();

        // Source unit
        let source_entity = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                Health {
                    current: 70, // 40 + 70 = 110, capped at 100
                    max: 100,
                },
                Fuel {
                    current: 80, // 20 + 80 = 100, capped at 99
                    max: 99,
                },
                Ammo {
                    ammo1: 8, // 2 + 8 = 10, capped at 9
                    max_ammo1: 9,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                inf_stats.clone(),
                ActionCompleted(false),
            ))
            .id();

        world.send_event(MergeUnitCommand {
            source_entity,
            target_entity,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(merge_unit_system);
        schedule.run(&mut world);

        // Source entity should be despawned
        assert!(world.get_entity(source_entity).is_err());

        // Target entity should be updated
        let t_health = world.get::<Health>(target_entity).unwrap();
        assert_eq!(t_health.current, 100);

        let t_fuel = world.get::<Fuel>(target_entity).unwrap();
        assert_eq!(t_fuel.current, 99);

        let t_ammo = world.get::<Ammo>(target_entity).unwrap();
        assert_eq!(t_ammo.ammo1, 9);

        let t_action = world.get::<ActionCompleted>(target_entity).unwrap();
        assert!(t_action.0);

        // Check events
        let merged_events = world.resource::<Events<UnitMergedEvent>>();
        let mut reader1 = merged_events.get_cursor();
        let events1: Vec<_> = reader1.read(merged_events).collect();
        assert_eq!(events1.len(), 1);
        assert_eq!(events1[0].source_entity, source_entity);
        assert_eq!(events1[0].target_entity, target_entity);
        assert_eq!(events1[0].refunded_funds, 0);

        let destroyed_events = world.resource::<Events<UnitDestroyedEvent>>();
        let mut reader2 = destroyed_events.get_cursor();
        let events2: Vec<_> = reader2.read(destroyed_events).collect();
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].entity, source_entity);
    }

    #[test]
    fn test_merging_with_loaded_cargo_is_prohibited() {
        let mut world = World::new();

        let ms = MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        };
        world.insert_resource(ms);
        world.insert_resource(Events::<MergeUnitCommand>::default());
        world.insert_resource(Events::<UnitMergedEvent>::default());
        world.insert_resource(Events::<UnitDestroyedEvent>::default());

        let apc_stats = UnitStats {
            unit_type: UnitType::SupplyTruck,
            cost: 5000,
            max_movement: 6,
            movement_type: MovementType::ArmoredCar,
            max_fuel: 99,
            max_ammo1: 0,
            max_ammo2: 0,
            min_range: 0,
            max_range: 0,
            daily_fuel_consumption: 0,
            can_capture: false,
            can_supply: true,
            max_cargo: 1,
            loadable_unit_types: vec![UnitType::Infantry],
            ..UnitStats::mock()
        };

        // 歩兵 (apc_a に搭載中)
        let infantry = world
            .spawn((
                GridPosition { x: 9999, y: 9999 },
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    ..UnitStats::mock()
                },
                ActionCompleted(false),
            ))
            .id();

        // 載荷中の装甲車A (HP 50)
        let apc_a = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                Health {
                    current: 50,
                    max: 100,
                },
                Fuel {
                    current: 50,
                    max: 99,
                },
                Ammo {
                    ammo1: 0,
                    max_ammo1: 0,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                apc_stats.clone(),
                CargoCapacity {
                    max: 1,
                    loaded: vec![infantry],
                },
                ActionCompleted(false),
            ))
            .id();

        world.entity_mut(infantry).insert(Transporting(apc_a));

        // 空の装甲車B (HP 50)
        let apc_b = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                Health {
                    current: 50,
                    max: 100,
                },
                Fuel {
                    current: 50,
                    max: 99,
                },
                Ammo {
                    ammo1: 0,
                    max_ammo1: 0,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                apc_stats.clone(),
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
                ActionCompleted(false),
            ))
            .id();

        // 1. 載荷中の装甲車Aからの合流候補は空であること（合流禁止）
        let targets_from_a =
            get_mergable_targets_at(&mut world, apc_a, GridPosition { x: 5, y: 5 });
        assert!(
            targets_from_a.is_empty(),
            "載荷中の輸送ユニットは合流不可であるべき"
        );

        // 2. 空の装甲車Bから載荷中の装甲車Aへの合流候補も空であること
        let targets_from_b =
            get_mergable_targets_at(&mut world, apc_b, GridPosition { x: 5, y: 5 });
        assert!(
            targets_from_b.is_empty(),
            "載荷中の相手への合流は不可であるべき"
        );

        // 3. 搭載中の歩兵からの合流候補も空であること
        let targets_from_inf =
            get_mergable_targets_at(&mut world, infantry, GridPosition { x: 5, y: 5 });
        assert!(
            targets_from_inf.is_empty(),
            "搭載中のユニットは合流不可であるべき"
        );

        // 4. システム実行時にも載荷ユニットの合流はスキップされ、歩兵が孤立しないこと
        world.send_event(MergeUnitCommand {
            source_entity: apc_a,
            target_entity: apc_b,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(merge_unit_system);
        schedule.run(&mut world);

        // 合流が拒絶され、apc_a も存続し、歩兵も apc_a に搭載されたままであること
        assert!(
            world.get_entity(apc_a).is_ok(),
            "載荷装甲車は合流で消滅しないこと"
        );
        let cargo = world.get::<CargoCapacity>(apc_a).unwrap();
        assert_eq!(cargo.loaded, vec![infantry]);
        let trans = world.get::<Transporting>(infantry).unwrap();
        assert_eq!(trans.0, apc_a);
    }
}
