use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;

/// 輸送ユニットへの積載コマンド(`LoadUnitCommand`)を処理するシステム。
///
/// 【処理の流れ】
/// 1. 輸送ユニットと積載対象ユニットが同座標にあり、同じプレイヤーの所有であることを確認します。
/// 2. 輸送ユニットの容量(`CargoCapacity`)と積載可能タイプ(`loadable_unit_types`)の条件を満たしているか確認します。
/// 3. 積載対象ユニットを輸送ユニットの `CargoCapacity` に追加します。
/// 4. 積載対象ユニットに `Transporting` コンポーネントを付与し、行動済み(`ActionCompleted`)にします。
/// 指定されたユニットを搭載可能な、隣接する輸送ユニットエンティティのリストを返します。
pub fn get_loadable_transports(world: &mut World, unit: Entity) -> Vec<Entity> {
    let mut targets = vec![];
    let (u_pos, u_stats, unit_faction) = {
        let mut q_unit = world.query::<(&GridPosition, &UnitStats, &Faction)>();
        let Ok((u_pos, u_stats, u_faction)) = q_unit.get(world, unit) else {
            return targets;
        };
        (*u_pos, u_stats.clone(), u_faction.0)
    };

    let mut q_transports = world.query_filtered::<
        (Entity, &GridPosition, &Faction, &UnitStats, &CargoCapacity),
        With<Faction>,
    >();
    for (t_ent, t_pos, t_faction, t_stats, t_cargo) in q_transports.iter(world) {
        if t_ent != unit && t_faction.0 == unit_faction {
            let dist = (u_pos.x as i64 - t_pos.x as i64).unsigned_abs() as u32
                + (u_pos.y as i64 - t_pos.y as i64).unsigned_abs() as u32;
            if dist == 1 {
                // 空き容量があり、かつ搭載可能タイプに含まれているか
                if t_cargo.loaded.len() < t_cargo.max as usize
                    && t_stats.loadable_unit_types.contains(&u_stats.unit_type)
                {
                    targets.push(t_ent);
                }
            }
        }
    }

    targets
}

/// 指定された輸送ユニットからユニットを降車させることが可能な、隣接マスのリストを返します。
pub fn get_droppable_tiles(world: &mut World, transport: Entity) -> Vec<(usize, usize)> {
    let mut targets = vec![];
    let t_pos = {
        let mut q_trans = world.query::<(&GridPosition, &UnitStats)>();
        let Ok((t_pos, _t_stats)) = q_trans.get(world, transport) else {
            return targets;
        };
        *t_pos
    };

    let (map_w, map_h) = if let Some(map) = world.get_resource::<crate::resources::Map>() {
        (map.width, map.height)
    } else {
        return targets;
    };

    // 周囲1マスの座標をチェック
    let neighbors = [
        (t_pos.x as i64 - 1, t_pos.y as i64),
        (t_pos.x as i64 + 1, t_pos.y as i64),
        (t_pos.x as i64, t_pos.y as i64 - 1),
        (t_pos.x as i64, t_pos.y as i64 + 1),
    ];

    for (nx, ny) in neighbors {
        if nx >= 0 && nx < map_w as i64 && ny >= 0 && ny < map_h as i64 {
            let x = nx as usize;
            let y = ny as usize;

            // TODO: 本来は「降ろすユニットの移動タイプ」で通行可能か判定すべきだが、
            // 今は簡単のため「空きマスであること」のみをチェックする（必要に応じて後で強化）
            let mut q_units = world.query_filtered::<&GridPosition, With<crate::components::Faction>>();
            let mut is_occupied = false;
            for u_pos in q_units.iter(world) {
                if u_pos.x == x && u_pos.y == y {
                    is_occupied = true;
                    break;
                }
            }

            if !is_occupied {
                targets.push((x, y));
            }
        }
    }

    targets
}

#[allow(clippy::type_complexity)]
pub fn load_unit_system(
    mut load_events: EventReader<LoadUnitCommand>,
    mut commands: Commands,
    mut q_units: Query<(
        Entity,
        &mut GridPosition,
        &Faction,
        &UnitStats,
        &mut ActionCompleted,
        Option<&mut CargoCapacity>,
        Option<&Transporting>,
    )>,
    match_state: Res<MatchState>,
    players: Res<Players>,
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Main {
        return;
    }
    let active_player_id = players.0[match_state.active_player_index.0].id;

    for event in load_events.read() {
        let (trans_pos, trans_faction, trans_stats, trans_capacity) =
            match q_units.get(event.transport_entity) {
                Ok((_, p, f, s, _, c, _)) => (
                    *p,
                    f.0,
                    s.clone(),
                    c.map(|cap| (cap.max, cap.loaded.len() as u32)),
                ),
                _ => continue,
            };

        if trans_faction != active_player_id {
            continue;
        }

        let (unit_pos, unit_faction, unit_stats, unit_action, unit_trans) =
            match q_units.get(event.unit_entity) {
                Ok((_, p, f, s, a, _, t)) => (*p, f.0, s.clone(), a.0, t.is_some()),
                _ => continue,
            };

        if unit_faction != active_player_id || unit_action || unit_trans {
            continue;
        }
        if trans_pos != unit_pos {
            continue;
        } // Must be on same tile to load

        #[allow(clippy::collapsible_if)]
        if trans_capacity.is_some_and(|(max_cap, loaded_len)| {
            loaded_len < max_cap
                && trans_stats
                    .loadable_unit_types
                    .contains(&unit_stats.unit_type)
        }) {
            if let Ok([transport, mut unit]) =
                q_units.get_many_mut([event.transport_entity, event.unit_entity])
            {
                if let Some(mut cap) = transport.5 {
                    cap.loaded.push(event.unit_entity);
                }
                unit.1.x = 9999; // Move off map
                unit.1.y = 9999;
                unit.4.0 = true; // Action completed
                commands
                    .entity(event.unit_entity)
                    .insert(Transporting(event.transport_entity));
            }
        }
    }
}

/// 輸送ユニットからの降車コマンド(`UnloadUnitCommand`)を処理するシステム。
///
/// 【処理の流れ】
/// 1. 降車対象ユニットが指定された輸送ユニットに積載されていることを確認します。
/// 2. 降車対象ユニットがこのターンに積載されたばかりでないか（`ActionCompleted`フラグがリセットされているか）確認します。
/// 3. 降車先の座標が輸送ユニットから距離1の空きマスであることを確認します。
/// 4. 輸送ユニットの `CargoCapacity` からユニットを削除し、`Transporting` コンポーネントを外します。
/// 5. 降車ユニットの座標(`GridPosition`)を更新し、行動済み(`ActionCompleted`)にします。
/// 6. 輸送ユニット自身も行動済み(`ActionCompleted`)にします。
#[allow(clippy::type_complexity)]
pub fn unload_unit_system(
    mut commands: Commands,
    mut unload_events: EventReader<UnloadUnitCommand>,
    mut q_units: Query<(
        Entity,
        &mut GridPosition,
        &Faction,
        &mut ActionCompleted,
        Option<&mut CargoCapacity>,
        Option<&Transporting>,
    )>,
    match_state: Res<MatchState>,
    players: Res<Players>,
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Main {
        return;
    }
    let active_player_id = players.0[match_state.active_player_index.0].id;

    for event in unload_events.read() {
        let (trans_pos, trans_faction, trans_action) = match q_units.get(event.transport_entity) {
            Ok((_, p, f, a, _, _)) => (*p, f.0, a.0),
            _ => continue,
        };

        if trans_faction != active_player_id || trans_action {
            continue;
        }

        let (_cargo_faction, cargo_action, cargo_trans) = match q_units.get(event.cargo_entity) {
            Ok((_, _, f, a, _, t)) => (f.0, a.0, t.map(|x| x.0)),
            _ => continue,
        };

        if cargo_trans != Some(event.transport_entity) {
            continue;
        }
        if cargo_action {
            continue;
        } // Cannot unload on the same turn it was loaded

        let dist = (trans_pos.x as i64 - event.target_x as i64).unsigned_abs() as u32
            + (trans_pos.y as i64 - event.target_y as i64).unsigned_abs() as u32;

        if dist != 1 {
            continue;
        }

        // Check if target is occupied
        let mut occupied = false;
        for (_, p, _, _, _, t) in q_units.iter() {
            if p.x == event.target_x && p.y == event.target_y && t.is_none() {
                occupied = true;
                break;
            }
        }
        if occupied {
            continue;
        }

        if let Ok([mut transport, mut cargo]) =
            q_units.get_many_mut([event.transport_entity, event.cargo_entity])
        {
            if let Some(mut cap) = transport.4 {
                cap.loaded.retain(|&e| e != event.cargo_entity);
            }
            transport.3.0 = true; // Transport action completed

            cargo.1.x = event.target_x;
            cargo.1.y = event.target_y;
            cargo.3.0 = true; // Unloaded unit is completed for the turn
            commands.entity(event.cargo_entity).remove::<Transporting>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_and_unload_unit_system() {
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

        world.insert_resource(Events::<LoadUnitCommand>::default());
        world.insert_resource(Events::<UnloadUnitCommand>::default());

        let transport_entity = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    cost: 5000,
                    max_movement: 6,
                    movement_type: MovementType::Air,
                    max_fuel: 99,
                    max_ammo1: 0,
                    max_ammo2: 0,
                    min_range: 1,
                    max_range: 1,
                    daily_fuel_consumption: 2,
                    can_capture: false,
                    can_supply: false,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                },
                CargoCapacity {
                    max: 2,
                    loaded: vec![],
                },
            ))
            .id();

        let cargo_entity = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                ActionCompleted(false),
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
                },
            ))
            .id();

        world.send_event(LoadUnitCommand {
            transport_entity,
            unit_entity: cargo_entity,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(load_unit_system);
        schedule.add_systems(unload_unit_system);
        schedule.run(&mut world);

        // Check load results
        let transport_cap = world.get::<CargoCapacity>(transport_entity).unwrap();
        assert_eq!(transport_cap.loaded.len(), 1);
        assert_eq!(transport_cap.loaded[0], cargo_entity);

        let cargo_trans = world.get::<Transporting>(cargo_entity).unwrap();
        assert_eq!(cargo_trans.0, transport_entity);

        let act = world.get::<ActionCompleted>(cargo_entity).unwrap();
        assert!(act.0); // Unit uses action when loaded

        // Fast forward action flags and try unloading
        world
            .get_mut::<ActionCompleted>(transport_entity)
            .unwrap()
            .0 = false;
        world.get_mut::<ActionCompleted>(cargo_entity).unwrap().0 = false;

        world.send_event(UnloadUnitCommand {
            transport_entity,
            cargo_entity,
            target_x: 6,
            target_y: 5,
        });

        schedule.run(&mut world);

        let transport_cap = world.get::<CargoCapacity>(transport_entity).unwrap();
        assert_eq!(transport_cap.loaded.len(), 0);

        assert!(world.get::<Transporting>(cargo_entity).is_none());

        let cargo_pos = world.get::<GridPosition>(cargo_entity).unwrap();
        assert_eq!(cargo_pos.x, 6);
        assert_eq!(cargo_pos.y, 5);

        let trans_act = world.get::<ActionCompleted>(transport_entity).unwrap();
        assert!(trans_act.0);

        let cargo_act = world.get::<ActionCompleted>(cargo_entity).unwrap();
        assert!(cargo_act.0);
    }
}
