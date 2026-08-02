use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;
use std::collections::HashSet;

/// 輸送ユニットが現在地の地形から積載ユニットを降ろせるかを判定します。
pub fn can_unload_from_terrain(
    movement_type: Option<MovementType>,
    terrain: Option<Terrain>,
) -> bool {
    if movement_type != Some(MovementType::Ship) {
        return true;
    }

    matches!(terrain, Some(Terrain::Port | Terrain::Shoal))
}

/// 輸送ユニットへの積載コマンド(`LoadUnitCommand`)を処理するシステム。
///
/// 【処理の流れ】
/// 1. 輸送ユニットと積載対象ユニットが同座標にあり、同じプレイヤーの所有であることを確認します。
/// 2. 輸送ユニットの容量(`CargoCapacity`)と積載可能タイプ(`loadable_unit_types`)の条件を満たしているか確認します。
/// 3. 積載対象ユニットを輸送ユニットの `CargoCapacity` に追加します。
/// 4. 積載対象ユニットに `Transporting` コンポーネントを付与し、行動済み(`ActionCompleted`)にします。
///
pub fn get_loadable_transports(world: &mut World, unit: Entity) -> Vec<Entity> {
    let Some(u_pos) = world.get::<GridPosition>(unit).cloned() else {
        return vec![];
    };
    get_loadable_transports_at(world, unit, u_pos)
}

/// 指定された位置でユニットを搭載可能な、輸送ユニットエンティティのリストを返します。
pub fn get_loadable_transports_at(
    world: &mut World,
    unit: Entity,
    u_pos: GridPosition,
) -> Vec<Entity> {
    let mut targets = vec![];
    let (u_type, unit_faction) = {
        let mut q_unit = world.query::<(&UnitStats, &Faction)>();
        let Ok((u_stats, u_faction)) = q_unit.get(world, unit) else {
            return targets;
        };
        (u_stats.unit_type, u_faction.0)
    };

    let mut q_transports = world.query_filtered::<
        (Entity, &GridPosition, &Faction, &UnitStats, &CargoCapacity),
        (With<Faction>, Without<Transporting>),
    >();
    for (t_ent, t_pos, t_faction, t_stats, t_cargo) in q_transports.iter(world) {
        if t_ent != unit && t_faction.0 == unit_faction {
            let dist = (u_pos.x as i64 - t_pos.x as i64).unsigned_abs() as u32
                + (u_pos.y as i64 - t_pos.y as i64).unsigned_abs() as u32;
            if dist == 0 {
                // 空き容量があり、かつ搭載可能タイプに含まれているか
                if t_cargo.loaded.len() < t_cargo.max as usize
                    && t_stats.loadable_unit_types.contains(&u_type)
                {
                    targets.push(t_ent);
                }
            }
        }
    }

    targets
}

pub fn get_droppable_tiles(
    world: &mut World,
    transport: Entity,
    cargo_entity: Entity,
) -> Vec<(usize, usize)> {
    let Some(t_pos) = world.get::<GridPosition>(transport).cloned() else {
        return vec![];
    };
    get_droppable_tiles_at(world, transport, cargo_entity, t_pos)
}

/// 指定された位置において、輸送ユニットからユニットを降車させることが可能な隣接マスのリストを返します。
pub fn get_droppable_tiles_at(
    world: &mut World,
    transport: Entity,
    cargo_entity: Entity,
    t_pos: GridPosition,
) -> Vec<(usize, usize)> {
    let mut targets = vec![];
    let (cargo_movement_type, trans_movement_type) = {
        let mut q_trans = world.query::<(&CargoCapacity, Option<&UnitStats>)>();
        let mut q_unit = world.query::<&UnitStats>();

        let Ok((cargo, transport_stats)) = q_trans.get(world, transport) else {
            return targets;
        };

        if !cargo.loaded.contains(&cargo_entity) {
            return targets;
        }

        let Ok(stats) = q_unit.get(world, cargo_entity) else {
            return targets;
        };
        (
            stats.movement_type,
            transport_stats.map(|stats| stats.movement_type),
        )
    };

    // 1. ユニットがいる座標を事前に取得
    use std::collections::HashSet;
    let mut occupied_positions = HashSet::new();
    let mut q_units =
        world.query_filtered::<&GridPosition, (With<Faction>, Without<Transporting>)>();
    for u_pos in q_units.iter(world) {
        occupied_positions.insert((u_pos.x, u_pos.y));
    }

    // 2. リソースを取得
    let (neighbors, map, master_data) = if let (Some(map), Some(md)) = (
        world.get_resource::<crate::resources::Map>(),
        world.get_resource::<crate::resources::master_data::MasterDataRegistry>(),
    ) {
        // トポロジー（スクエア=4近傍/ヘックス=6近傍）に応じた隣接マスを取得
        (map.get_adjacent(t_pos.x, t_pos.y), map, md)
    } else {
        return targets;
    };

    // 輸送船は接岸可能な港または浅瀬からのみ降車できる
    if !can_unload_from_terrain(trans_movement_type, map.get_terrain(t_pos.x, t_pos.y)) {
        return targets;
    }

    for (x, y) in neighbors {
        // 地形通行可能判定
        let Some(terrain) = map.get_terrain(x, y) else {
            continue;
        };

        if crate::systems::movement::get_valid_movement_cost(
            master_data,
            cargo_movement_type,
            terrain,
        )
        .is_none()
        {
            continue;
        }

        if !occupied_positions.contains(&(x, y)) {
            targets.push((x, y));
        }
    }

    targets
}

#[allow(clippy::type_complexity)]
pub fn load_unit_system(
    mut load_events: EventReader<LoadUnitCommand>,
    mut loaded_writer: EventWriter<UnitLoadedEvent>,
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

        let (unit_pos, unit_faction, unit_type, unit_action, unit_trans) =
            match q_units.get(event.unit_entity) {
                Ok((_, p, f, s, a, _, t)) => (*p, f.0, s.unit_type, a.0, t.is_some()),
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
            loaded_len < max_cap && trans_stats.loadable_unit_types.contains(&unit_type)
        }) {
            if let Ok([mut transport, mut unit]) =
                q_units.get_many_mut([event.transport_entity, event.unit_entity])
            {
                if let Some(cap) = transport.5.as_mut() {
                    cap.loaded.push(event.unit_entity);
                }
                unit.1.x = 9999; // Move off map
                unit.1.y = 9999;
                unit.4.0 = true; // Action completed

                // 輸送ユニットも行動済みにする
                transport.4.0 = true;

                commands
                    .entity(event.unit_entity)
                    .insert(Transporting(event.transport_entity));
                // 搭載を行った輸送ユニットの再移動と移動後アクションを禁止する
                commands
                    .entity(event.transport_entity)
                    .insert(HasMoved(true));

                // 積載完了イベントを送出
                loaded_writer.send(UnitLoadedEvent {
                    transport: event.transport_entity,
                    cargo: event.unit_entity,
                });

                // アクション確定時に移動履歴を削除
                commands.remove_resource::<PendingMove>();
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
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub fn unload_unit_system(
    mut commands: Commands,
    mut unload_events: EventReader<UnloadUnitCommand>,
    mut unloaded_writer: EventWriter<UnitUnloadedEvent>,
    mut set: ParamSet<(
        Query<(
            Entity,
            &mut GridPosition,
            &Faction,
            &mut ActionCompleted,
            Option<&mut CargoCapacity>,
            Option<&Transporting>,
            &UnitStats,
        )>,
        Query<&ActionCompleted>,
    )>,
    match_state: Res<MatchState>,
    players: Res<Players>,
    map: Res<Map>,
    master_data: Res<MasterDataRegistry>,
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Main {
        return;
    }
    let active_player_id = players.0[match_state.active_player_index.0].id;

    for event in unload_events.read() {
        let (trans_pos, trans_faction, _trans_action, trans_movement_type) =
            match set.p0().get(event.transport_entity) {
                Ok((_, p, f, a, _, _, s)) => (*p, f.0, a.0, s.movement_type),
                _ => continue,
            };

        // 勢力のチェックのみ行い、行動済みチェックは降車ロジック内で行う、
        // あるいは複数降車を許可するためにここでは緩和する
        if trans_faction != active_player_id {
            continue;
        }

        // 候補表示と同じルールで、輸送船が接岸可能な地形にいることを検証する
        if !can_unload_from_terrain(
            Some(trans_movement_type),
            map.get_terrain(trans_pos.x, trans_pos.y),
        ) {
            continue;
        }

        let (cargo_action, cargo_trans, cargo_movement_type) =
            match set.p0().get(event.cargo_entity) {
                Ok((_, _, _, a, _, t, s)) => (a.0, t.map(|x| x.0), s.movement_type),
                _ => continue,
            };

        if cargo_trans != Some(event.transport_entity) {
            continue;
        }
        if cargo_action {
            continue;
        } // Cannot unload on the same turn it was loaded

        // トポロジー（スクエア/ヘックス）に応じた距離で隣接判定する
        let dist = map.distance(trans_pos.x, trans_pos.y, event.target_x, event.target_y);

        if dist != 1 {
            continue;
        }

        // Check terrain passability for the cargo
        let terrain = if let Some(t) = map.get_terrain(event.target_x, event.target_y) {
            t
        } else {
            continue;
        };
        if crate::systems::movement::get_valid_movement_cost(
            &master_data,
            cargo_movement_type,
            terrain,
        )
        .is_none()
        {
            continue;
        }

        // Check if target is occupied
        let mut occupied = false;
        for (_, p, _, _, _, t, _) in set.p0().iter() {
            if p.x == event.target_x && p.y == event.target_y && t.is_none() {
                occupied = true;
                break;
            }
        }
        if occupied {
            continue;
        }

        let mut has_active_cargo = false;
        let mut loaded_units = Vec::new();

        // 1. まず輸送ユニットの loaded リスト（から今回の cargo を除いたもの）を取得する
        if let Some(cap) = set.p0().get(event.transport_entity).ok().and_then(|t| t.4) {
            loaded_units = cap
                .loaded
                .iter()
                .filter(|&&e| e != event.cargo_entity)
                .copied()
                .collect();
        }

        // 2. set.p0() の借用は終わったので、set.p1() を安全に使える！
        if !loaded_units.is_empty() {
            let q_action = set.p1();
            has_active_cargo = loaded_units.iter().any(|&e| {
                if let Ok(action_completed) = q_action.get(e) {
                    !action_completed.0
                } else {
                    false
                }
            });
        }

        // 3. 最後に、可変更新を行う（set.p0() を再度 mutable 借用）
        let mut q_units = set.p0();
        if let Ok([mut transport, mut cargo]) =
            q_units.get_many_mut([event.transport_entity, event.cargo_entity])
        {
            if let Some(ref mut cap) = transport.4 {
                cap.loaded.retain(|&e| e != event.cargo_entity);
            }

            if !has_active_cargo {
                transport.3.0 = true;
            }

            cargo.1.x = event.target_x;
            cargo.1.y = event.target_y;
            cargo.3.0 = true; // Unloaded unit is completed for the turn
            commands.entity(event.cargo_entity).remove::<Transporting>();
            // 降車を行った輸送ユニットの再移動と移動後アクションを禁止する
            commands
                .entity(event.transport_entity)
                .insert(HasMoved(true));

            // 降車完了イベントを送出
            unloaded_writer.send(UnitUnloadedEvent {
                transport: event.transport_entity,
                cargo: event.cargo_entity,
                target_x: event.target_x,
                target_y: event.target_y,
            });

            // アクション確定時に移動履歴を削除
            commands.remove_resource::<PendingMove>();
        }
    }
}

/// 輸送ユニットが新たに被弾した際、搭載ユニットのHPを輸送ユニットのHP以下に同期させます。
///
/// 平時に常時同期すると、損傷した空母によるターン開始時の修理まで取り消してしまうため、
/// 攻撃イベントでHP減少が確認できた輸送ユニットだけを対象にします。
#[allow(clippy::type_complexity)]
pub fn sync_cargo_health_system(
    mut attacked_events: EventReader<UnitAttackedEvent>,
    mut set: ParamSet<(
        Query<(Entity, &Health, &CargoCapacity)>,
        Query<(&Transporting, &mut Health)>,
    )>,
) {
    let mut damaged_transports = HashSet::new();
    for event in attacked_events.read() {
        if event.defender_hp_after < event.defender_hp_before {
            damaged_transports.insert(event.defender);
        }
        if event.attacker_hp_after < event.attacker_hp_before {
            damaged_transports.insert(event.attacker);
        }
    }

    // 攻撃イベントを伴わない撃破でも、既存どおり搭載ユニットを撃破状態にします。
    let updates: Vec<(Entity, u32, Vec<Entity>)> = set
        .p0()
        .iter()
        .filter(|(entity, health, _)| damaged_transports.contains(entity) || health.is_destroyed())
        .map(|(entity, health, cargo)| (entity, health.current, cargo.loaded.clone()))
        .collect();

    let mut cargo_query = set.p1();
    for (transport, transport_hp, cargo_entities) in updates {
        for cargo in cargo_entities {
            if let Ok((transporting, mut cargo_hp)) = cargo_query.get_mut(cargo)
                && transporting.0 == transport
                && cargo_hp.current > transport_hp
            {
                cargo_hp.current = transport_hp;
            }
        }
    }
}

/// 輸送ユニットが破壊された際、搭載されていたユニットも破壊するシステム。
pub fn cleanup_cargo_system(
    mut commands: Commands,
    mut destroyed_events: EventReader<UnitDestroyedEvent>,
    q_cargo: Query<(Entity, &Transporting)>,
) {
    for event in destroyed_events.read() {
        for (cargo_ent, trans) in q_cargo.iter() {
            if trans.0 == event.entity {
                // 輸送ユニットが破壊されたので、搭載ユニットも破壊（デスポーン）
                commands.entity(cargo_ent).despawn();
            }
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
        world.insert_resource(Events::<UnitLoadedEvent>::default());
        world.insert_resource(Events::<UnitUnloadedEvent>::default());

        // Insert Map and MasterDataRegistry for terrain checks
        let mut map = Map::new(10, 10, Terrain::Plains, GridTopology::Square);
        for x in 0..10 {
            for y in 0..10 {
                let _ = map.set_terrain(x, y, Terrain::Plains);
            }
        }

        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());

        let transport_entity = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    max_fuel: 99,
                    daily_fuel_consumption: 2,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
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
                    max_fuel: 99,
                    max_ammo1: 9,
                    can_capture: true,
                    ..UnitStats::mock()
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

    #[test]
    fn test_get_loadable_transports() {
        let mut world = World::new();

        let transport_entity = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
            ))
            .id();

        let cargo_entity = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    ..UnitStats::mock()
                },
            ))
            .id();

        // 同一座標なので見つかるはず
        let targets = get_loadable_transports(&mut world, cargo_entity);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0], transport_entity);

        // 座標をずらすと見つからなくなるはず
        world.get_mut::<GridPosition>(cargo_entity).unwrap().x = 6;
        let targets = get_loadable_transports(&mut world, cargo_entity);
        assert_eq!(targets.len(), 0);

        // 容量がいっぱいだと見つからないはず
        world.get_mut::<GridPosition>(cargo_entity).unwrap().x = 5;
        world
            .get_mut::<CargoCapacity>(transport_entity)
            .unwrap()
            .loaded
            .push(Entity::from_raw(999));
        let targets = get_loadable_transports(&mut world, cargo_entity);
        assert_eq!(targets.len(), 0);
    }

    #[test]
    fn test_get_droppable_tiles() {
        let mut world = World::new();

        // マップとマスターデータのセットアップ
        let map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());

        let transport_entity = world
            .spawn((
                GridPosition { x: 1, y: 1 },
                Faction(PlayerId(1)),
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
            ))
            .id();

        let cargo_entity = world
            .spawn((
                GridPosition { x: 999, y: 999 }, // 搭載中を想定
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    ..UnitStats::mock()
                },
                Transporting(transport_entity),
            ))
            .id();

        world
            .get_mut::<CargoCapacity>(transport_entity)
            .unwrap()
            .loaded
            .push(cargo_entity);

        // 初期状態：周囲4マス空いている
        let tiles = get_droppable_tiles(&mut world, transport_entity, cargo_entity);
        assert_eq!(tiles.len(), 4);

        // 隣接マス (1, 0) に他のユニットを配置
        world.spawn((GridPosition { x: 1, y: 0 }, Faction(PlayerId(1))));

        let tiles = get_droppable_tiles(&mut world, transport_entity, cargo_entity);
        assert_eq!(tiles.len(), 3);
        assert!(!tiles.contains(&(1, 0)));

        // 地形を通行不能にする (1, 1 -> 0, 1 を海にする)
        let mut map = world.get_resource_mut::<Map>().unwrap();
        map.set_terrain(0, 1, Terrain::Sea).unwrap();

        let tiles = get_droppable_tiles(&mut world, transport_entity, cargo_entity);
        // 歩兵は海を通行できないので、(0, 1) も除外されるはず
        assert_eq!(tiles.len(), 2);
        assert!(!tiles.contains(&(0, 1)));
    }

    /// ヘックスモードでは降車先候補が周囲6マスになることの確認 (#37)
    #[test]
    fn test_get_droppable_tiles_hex() {
        let mut world = World::new();

        // ヘックスマップとマスターデータのセットアップ
        let map = Map::new(5, 5, Terrain::Plains, GridTopology::Hex);
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());

        let transport_entity = world
            .spawn((
                GridPosition { x: 1, y: 1 },
                Faction(PlayerId(1)),
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
            ))
            .id();

        let cargo_entity = world
            .spawn((
                GridPosition { x: 999, y: 999 }, // 搭載中を想定
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    ..UnitStats::mock()
                },
                Transporting(transport_entity),
            ))
            .id();

        world
            .get_mut::<CargoCapacity>(transport_entity)
            .unwrap()
            .loaded
            .push(cargo_entity);

        // (1,1) は奇数行なので odd-r レイアウトの6近傍すべてが候補になる
        let mut tiles = get_droppable_tiles(&mut world, transport_entity, cargo_entity);
        tiles.sort();
        assert_eq!(
            tiles,
            vec![(0, 1), (1, 0), (1, 2), (2, 0), (2, 1), (2, 2)],
            "ヘックスモードでは周囲6マスが降車候補になるはず"
        );
    }

    #[test]
    fn test_get_droppable_tiles_mixed_cargo() {
        let mut world = World::new();

        // 1. マップとマスターデータのセットアップ
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        // (0, 1) を「山」にする（歩兵は入れるが、車両は入れない）
        map.set_terrain(0, 1, Terrain::Mountain).unwrap();
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());

        // 2. 輸送ユニット（輸送ヘリ）の配置
        let transport_entity = world
            .spawn((
                GridPosition { x: 1, y: 1 },
                Faction(PlayerId(1)),
                CargoCapacity {
                    max: 2,
                    loaded: vec![],
                },
            ))
            .id();

        // 3. 乗員1：歩兵（山に入れる）
        let infantry_entity = world
            .spawn((
                GridPosition { x: 999, y: 999 },
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    ..UnitStats::mock()
                },
                Transporting(transport_entity),
            ))
            .id();

        // 4. 乗員2：偵察車（山に入れない。ここでは簡易的に戦車系移動タイプを想定）
        let vehicle_entity = world
            .spawn((
                GridPosition { x: 999, y: 999 },
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Recon,
                    movement_type: MovementType::Tank,
                    ..UnitStats::mock()
                },
                Transporting(transport_entity),
            ))
            .id();

        // 5. 積載
        world
            .get_mut::<CargoCapacity>(transport_entity)
            .unwrap()
            .loaded = vec![infantry_entity, vehicle_entity];

        // 6. 検証
        // 歩兵を選択した場合：山 (0, 1) を含む周囲が降車可能
        let tiles_inf = get_droppable_tiles(&mut world, transport_entity, infantry_entity);
        assert!(
            tiles_inf.contains(&(0, 1)),
            "Infantry should be able to drop on Mountain"
        );

        // 車両を選択した場合：山 (0, 1) は降車不可
        let tiles_veh = get_droppable_tiles(&mut world, transport_entity, vehicle_entity);
        assert!(
            !tiles_veh.contains(&(0, 1)),
            "Vehicle should NOT be able to drop on Mountain"
        );
    }

    #[test]
    fn test_cargo_health_sync_on_damage() {
        let mut world = World::new();
        world.init_resource::<Events<UnitAttackedEvent>>();

        // 輸送ユニット (HP 100)
        let transport_entity = world
            .spawn((
                Health {
                    current: 100,
                    max: 100,
                },
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
            ))
            .id();

        // 搭載ユニット1 (HP 100) - 輸送ユニットと同レベル
        let cargo1_entity = world
            .spawn((
                Health {
                    current: 100,
                    max: 100,
                },
                Transporting(transport_entity),
            ))
            .id();

        // 搭載ユニット2 (HP 40) - 輸送ユニットより低い
        let cargo2_entity = world
            .spawn((
                Health {
                    current: 40,
                    max: 100,
                },
                Transporting(transport_entity),
            ))
            .id();

        world
            .get_mut::<CargoCapacity>(transport_entity)
            .unwrap()
            .loaded = vec![cargo1_entity, cargo2_entity];

        let mut schedule = Schedule::default();
        schedule.add_systems(sync_cargo_health_system);

        // 1. 輸送ユニットにダメージ (HP 100 -> 60)
        world.get_mut::<Health>(transport_entity).unwrap().current = 60;
        world.send_event(UnitAttackedEvent {
            attacker: Entity::PLACEHOLDER,
            defender: transport_entity,
            damage_dealt: 40,
            counter_damage_dealt: None,
            attacker_hp_before: 100,
            attacker_hp_after: 100,
            defender_hp_before: 100,
            defender_hp_after: 60,
        });
        schedule.run(&mut world);

        // cargo1 は 60 になるはず
        assert_eq!(world.get::<Health>(cargo1_entity).unwrap().current, 60);
        // cargo2 は 40 のまま（増えない）はず
        assert_eq!(world.get::<Health>(cargo2_entity).unwrap().current, 40);

        // 2. 輸送ユニット撃破 (HP 60 -> 0)
        world.get_mut::<Health>(transport_entity).unwrap().current = 0;
        schedule.run(&mut world);

        // 両方 0 になるはず
        assert_eq!(world.get::<Health>(cargo1_entity).unwrap().current, 0);
        assert_eq!(world.get::<Health>(cargo2_entity).unwrap().current, 0);
    }

    #[test]
    fn test_cargo_health_is_not_clamped_without_new_transport_damage() {
        let mut world = World::new();
        world.init_resource::<Events<UnitAttackedEvent>>();

        let transport = world
            .spawn((
                Health {
                    current: 40,
                    max: 100,
                },
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
            ))
            .id();
        let cargo = world
            .spawn((
                Health {
                    current: 60,
                    max: 100,
                },
                Transporting(transport),
            ))
            .id();
        world.get_mut::<CargoCapacity>(transport).unwrap().loaded = vec![cargo];

        let mut schedule = Schedule::default();
        schedule.add_systems(sync_cargo_health_system);
        schedule.run(&mut world);

        // 損傷済み空母がサービスした搭載機のHPは、新たな被弾があるまで維持します。
        assert_eq!(world.get::<Health>(cargo).unwrap().current, 60);
    }

    #[test]
    fn test_multiple_unload_sequence() {
        let mut world = World::new();

        let ms = MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        };
        world.insert_resource(ms);
        world.insert_resource(Players(vec![Player::new(1, "P1".to_string())]));

        world.insert_resource(Events::<UnloadUnitCommand>::default());
        world.insert_resource(Events::<UnitUnloadedEvent>::default());

        let map = Map::new(10, 10, Terrain::Plains, GridTopology::Square);
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());

        let transport_entity = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: vec![],
                },
            ))
            .id();

        let cargo1 = world
            .spawn((
                GridPosition { x: 999, y: 999 },
                Faction(PlayerId(1)),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    ..UnitStats::mock()
                },
                Transporting(transport_entity),
            ))
            .id();

        let cargo2 = world
            .spawn((
                GridPosition { x: 999, y: 999 },
                Faction(PlayerId(1)),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    ..UnitStats::mock()
                },
                Transporting(transport_entity),
            ))
            .id();

        world
            .get_mut::<CargoCapacity>(transport_entity)
            .unwrap()
            .loaded = vec![cargo1, cargo2];

        let mut schedule = Schedule::default();
        schedule.add_systems(unload_unit_system);

        // 1回目の降車
        world.send_event(UnloadUnitCommand {
            transport_entity,
            cargo_entity: cargo1,
            target_x: 6,
            target_y: 5,
        });
        schedule.run(&mut world);

        // 1人目を降ろした時点（まだ未行動の歩兵が残っている）では、輸送ユニットは行動済みにならない
        assert!(!world.get::<ActionCompleted>(transport_entity).unwrap().0);
        assert_eq!(
            world
                .get::<CargoCapacity>(transport_entity)
                .unwrap()
                .loaded
                .len(),
            1
        );

        // 2回目の降車
        world.send_event(UnloadUnitCommand {
            transport_entity,
            cargo_entity: cargo2,
            target_x: 4,
            target_y: 5,
        });
        schedule.run(&mut world);

        // これで全て降ろしたので、輸送艦は行動済みになるはず
        assert!(world.get::<ActionCompleted>(transport_entity).unwrap().0);
        assert_eq!(
            world
                .get::<CargoCapacity>(transport_entity)
                .unwrap()
                .loaded
                .len(),
            0
        );
    }

    #[test]
    fn test_load_exhausts_transport() {
        let mut world = World::new();
        world.insert_resource(MatchState::default());
        world.insert_resource(Players(vec![Player::new(1, "P1".to_string())]));
        world.insert_resource(Events::<LoadUnitCommand>::default());
        world.insert_resource(Events::<UnitLoadedEvent>::default());

        let transport = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::SupplyTruck,
                    max_cargo: 1,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
            ))
            .id();

        let cargo = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    ..UnitStats::mock()
                },
            ))
            .id();

        world.send_event(LoadUnitCommand {
            transport_entity: transport,
            unit_entity: cargo,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(load_unit_system);
        schedule.run(&mut world);

        // 積載後、輸送ユニットも行動済みになるはず
        assert!(world.get::<ActionCompleted>(transport).unwrap().0);
    }

    #[test]
    fn test_undo_prevention_on_transport_actions() {
        let mut world = World::new();
        world.insert_resource(MatchState::default());
        world.insert_resource(Players(vec![Player::new(1, "P1".to_string())]));
        world.insert_resource(Events::<UnloadUnitCommand>::default());
        world.insert_resource(Events::<UnitUnloadedEvent>::default());
        world.insert_resource(Map::new(10, 10, Terrain::Plains, GridTopology::Square));
        world.insert_resource(MasterDataRegistry::load().unwrap());

        let transport = world
            .spawn((
                GridPosition { x: 5, y: 5 },
                Faction(PlayerId(1)),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::SupplyTruck,
                    max_cargo: 1,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
            ))
            .id();

        let cargo = world
            .spawn((
                GridPosition { x: 999, y: 999 },
                Faction(PlayerId(1)),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    ..UnitStats::mock()
                },
                Transporting(transport),
            ))
            .id();
        world.get_mut::<CargoCapacity>(transport).unwrap().loaded = vec![cargo];

        // 移動履歴を設定
        world.insert_resource(PendingMove {
            unit_entity: transport,
            original_pos: GridPosition { x: 1, y: 1 },
            original_fuel: Fuel {
                current: 20,
                max: 20,
            },
        });

        world.send_event(UnloadUnitCommand {
            transport_entity: transport,
            cargo_entity: cargo,
            target_x: 6,
            target_y: 5,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(unload_unit_system);
        schedule.run(&mut world);

        // 降車後、PendingMove が削除されているはず
        assert!(world.get_resource::<PendingMove>().is_none());
    }

    #[test]
    fn test_shoal_transport_load_reachability() {
        use crate::systems::movement::{OccupantInfo, calculate_reachable_tiles};
        use std::collections::HashMap;

        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        // (2, 2) を浅瀬 (Shoal), (3, 2) を海 (Sea) に設定
        let _ = map.set_terrain(2, 2, Terrain::Shoal);
        let _ = map.set_terrain(3, 2, Terrain::Sea);

        let master_data = MasterDataRegistry::load().unwrap();
        let p1 = PlayerId(1);

        let mut unit_positions = HashMap::new();

        // (2, 2) 浅瀬に空きありの自軍輸送船 (Lander) を配置
        unit_positions.insert(
            (2, 2),
            OccupantInfo {
                player_id: p1,
                is_transport: true,
                unit_type: UnitType::Lander,
                loadable_types: vec![UnitType::Infantry],
                free_slots: 1,
            },
        );

        // (3, 2) 海に空きありの自軍輸送船 (Lander) を配置
        unit_positions.insert(
            (3, 2),
            OccupantInfo {
                player_id: p1,
                is_transport: true,
                unit_type: UnitType::Lander,
                loadable_types: vec![UnitType::Infantry],
                free_slots: 1,
            },
        );

        // 歩兵 (Infantry, MovementType::Infantry) が (1, 2) 平地から移動開始 (移動力 3)
        let reachable = calculate_reachable_tiles(
            &map,
            &unit_positions,
            (1, 2),
            MovementType::Infantry,
            3,
            99,
            p1,
            UnitType::Infantry,
            &master_data,
        );

        // 浅瀬の自軍輸送船 (2, 2) には進入（到達）できる
        assert!(reachable.contains(&(2, 2)));

        // 海の自軍輸送船 (3, 2) には進入（到達）できない
        assert!(!reachable.contains(&(3, 2)));

        // 満載の場合のテスト
        unit_positions.insert(
            (2, 2),
            OccupantInfo {
                player_id: p1,
                is_transport: true,
                unit_type: UnitType::Lander,
                loadable_types: vec![UnitType::Infantry],
                free_slots: 0, // 満載
            },
        );

        let reachable_full = calculate_reachable_tiles(
            &map,
            &unit_positions,
            (1, 2),
            MovementType::Infantry,
            3,
            99,
            p1,
            UnitType::Infantry,
            &master_data,
        );

        // 満載の場合は進入できない
        assert!(!reachable_full.contains(&(2, 2)));
    }

    #[test]
    fn test_lander_unload_terrain_restriction() {
        let mut world = World::new();
        world.insert_resource(MatchState::default());
        world.insert_resource(Players(vec![Player::new(1, "P1".to_string())]));
        world.insert_resource(Events::<UnloadUnitCommand>::default());
        world.insert_resource(Events::<UnitUnloadedEvent>::default());

        let mut map = Map::new(5, 5, Terrain::Sea, GridTopology::Square);
        let _ = map.set_terrain(2, 2, Terrain::Sea);
        let _ = map.set_terrain(1, 2, Terrain::Plains);
        let _ = map.set_terrain(3, 3, Terrain::Shoal);
        let _ = map.set_terrain(3, 2, Terrain::Plains);
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());

        // (2, 2) Sea に輸送船 (Lander) を配置
        let transport = world
            .spawn((
                GridPosition { x: 2, y: 2 },
                Faction(PlayerId(1)),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::Lander,
                    movement_type: MovementType::Ship,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: vec![],
                },
            ))
            .id();

        let cargo = world
            .spawn((
                GridPosition { x: 999, y: 999 },
                Faction(PlayerId(1)),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    ..UnitStats::mock()
                },
                Transporting(transport),
            ))
            .id();

        world.get_mut::<CargoCapacity>(transport).unwrap().loaded = vec![cargo];

        // Sea 上での get_droppable_tiles は空を返すはず
        let droppable = get_droppable_tiles(&mut world, transport, cargo);
        assert!(droppable.is_empty());

        // 輸送船を (3, 3) Shoal (浅瀬) に移動
        world.get_mut::<GridPosition>(transport).unwrap().x = 3;
        world.get_mut::<GridPosition>(transport).unwrap().y = 3;

        // Shoal 上では隣接する Plains (3, 2) への降車可能マスが返るはず
        let droppable_shoal = get_droppable_tiles(&mut world, transport, cargo);
        assert!(droppable_shoal.contains(&(3, 2)));

        // 実際に降車コマンドを実行
        world.send_event(UnloadUnitCommand {
            transport_entity: transport,
            cargo_entity: cargo,
            target_x: 3,
            target_y: 2,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(unload_unit_system);
        schedule.run(&mut world);

        // 降車成功
        let cargo_pos = world.get::<GridPosition>(cargo).unwrap();
        assert_eq!(cargo_pos.x, 3);
        assert_eq!(cargo_pos.y, 2);

        // 降車後、輸送ユニットに HasMoved(true) が付与されていることを検証
        assert!(world.get::<HasMoved>(transport).is_some_and(|h| h.0));
    }
}
