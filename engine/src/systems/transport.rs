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
///
pub fn get_loadable_transports(world: &mut World, unit: Entity) -> Vec<Entity> {
    let u_pos = *world.get::<GridPosition>(unit).unwrap();
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
    let t_pos = *world.get::<GridPosition>(transport).unwrap();
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
    let cargo_movement_type = {
        let mut q_trans = world.query::<&CargoCapacity>();
        let mut q_unit = world.query::<&UnitStats>();

        let Ok(cargo) = q_trans.get(world, transport) else {
            return targets;
        };

        if !cargo.loaded.contains(&cargo_entity) {
            return targets;
        }

        let Ok(stats) = q_unit.get(world, cargo_entity) else {
            return targets;
        };
        stats.movement_type
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
    let (map_w, map_h, master_data) = if let (Some(map), Some(md)) = (
        world.get_resource::<crate::resources::Map>(),
        world.get_resource::<crate::resources::master_data::MasterDataRegistry>(),
    ) {
        (map.width, map.height, md)
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

            // 地形通行可能判定
            let terrain = if let Some(map) = world.get_resource::<crate::resources::Map>() {
                if let Some(t) = map.get_terrain(x, y) {
                    t
                } else {
                    continue;
                }
            } else {
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
#[allow(clippy::type_complexity)]
pub fn unload_unit_system(
    mut commands: Commands,
    mut unload_events: EventReader<UnloadUnitCommand>,
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
        let (trans_pos, trans_faction, _trans_action) = match set.p0().get(event.transport_entity) {
            Ok((_, p, f, a, _, _, _)) => (*p, f.0, a.0),
            _ => continue,
        };

        // 勢力のチェックのみ行い、行動済みチェックは降車ロジック内で行う、
        // あるいは複数降車を許可するためにここでは緩和する
        if trans_faction != active_player_id {
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

        let dist = (trans_pos.x as i64 - event.target_x as i64).unsigned_abs() as u32
            + (trans_pos.y as i64 - event.target_y as i64).unsigned_abs() as u32;

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

        let mut q_units = set.p0();
        if let Ok([mut transport, mut cargo]) =
            q_units.get_many_mut([event.transport_entity, event.cargo_entity])
        {
            if let Some(ref mut cap) = transport.4 {
                cap.loaded.retain(|&e| e != event.cargo_entity);
            }

            // はじめて降車した時点で、輸送ユニットを行動済みにする
            // これによりUIでグレーアウトされ、移動のキャンセルもできなくなる
            transport.3.0 = true;

            cargo.1.x = event.target_x;
            cargo.1.y = event.target_y;
            cargo.3.0 = true; // Unloaded unit is completed for the turn
            commands.entity(event.cargo_entity).remove::<Transporting>();

            // アクション確定時に移動履歴を削除
            commands.remove_resource::<PendingMove>();
        }
    }
}

/// 輸送ユニットのHPが減少した際、搭載されているユニットのHPを輸送ユニットのHP以下に同期させます。
/// (cargo_hp = min(cargo_hp, transport_hp))
#[allow(clippy::type_complexity)]
pub fn sync_cargo_health_system(
    mut set: ParamSet<(
        Query<(&Transporting, Entity)>,
        Query<&mut Health>,
        Query<&Health>,
    )>,
) {
    let mut updates = Vec::new();

    // 1. 更新が必要な積載ユニットを特定
    {
        let links: Vec<(Entity, Entity)> = set.p0().iter().map(|(t, c)| (c, t.0)).collect();
        let q_health = set.p2();

        for (cargo_ent, transport_ent) in links {
            if let Ok(c_hp) = q_health.get(cargo_ent)
                && let Ok(t_hp) = q_health.get(transport_ent)
                && c_hp.current > t_hp.current
            {
                updates.push((cargo_ent, t_hp.current));
            }
        }
    }

    // 2. HPの更新を適用
    let mut q_health_mut = set.p1();
    for (ent, new_hp) in updates {
        if let Ok(mut hp) = q_health_mut.get_mut(ent) {
            hp.current = new_hp;
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
    fn test_multiple_unload_sequence() {
        let mut world = World::new();

        let ms = MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        };
        world.insert_resource(ms);
        world.insert_resource(Players(vec![Player::new(1, "P1".to_string())]));

        world.insert_resource(Events::<UnloadUnitCommand>::default());

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

        // 1人目を降ろした時点で、輸送ユニットは行動済みになる（仕様）
        assert!(world.get::<ActionCompleted>(transport_entity).unwrap().0);
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
}
