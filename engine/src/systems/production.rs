use crate::components::*;
use crate::events::*;
pub use crate::resources::master_data::MasterDataRegistry;
use crate::resources::*;
use bevy_ecs::prelude::*;

/// ユニットの生産コマンド(`ProduceUnitCommand`)を処理するシステム。
///
/// 【処理の流れ】
/// 1. コマンドを発行したプレイヤーがアクティブプレイヤーであることを確認します。
/// 2. ターゲット座標が自軍の生産拠点（都市または空港）であることを確認します。
/// 3. 自軍の首都からの距離が3マス以内であることを確認します。
/// 4. プレイヤーの資金(`funds`)が生産コスト(`cost`)以上であることを確認し、資金を消費します。
/// 5. 新しいユニットの実体(`Entity`)をコンポーネント群と共に生成（スポーン）します。
///    ※生産された直後は行動できないため、`HasMoved` と `ActionCompleted` を true にします。
///
/// 首都からの生産可能範囲（マンハッタン距離）
pub const PRODUCTION_RANGE: usize = 3;

pub fn is_within_production_range(
    capital_pos: Option<GridPosition>,
    target_x: usize,
    target_y: usize,
) -> bool {
    if let Some(cp) = capital_pos {
        let distance = (target_x as isize - cp.x as isize).unsigned_abs()
            + (target_y as isize - cp.y as isize).unsigned_abs();
        distance <= PRODUCTION_RANGE
    } else {
        false
    }
}

fn check_production_rules(
    is_occupied: bool,
    landscape_name: Option<&str>,
    capital_pos: Option<GridPosition>,
    target_x: usize,
    target_y: usize,
    unit_type: UnitType,
    master_data: &MasterDataRegistry,
) -> Result<(), String> {
    if is_occupied {
        return Err("Tile is occupied!".to_string());
    }

    let Some(ln) = landscape_name else {
        return Err("Not a friendly property!".to_string());
    };

    if !master_data.can_produce_unit(ln, unit_type) {
        return Err(format!("Cannot produce {:?} at {}", unit_type, ln));
    }

    if !is_within_production_range(capital_pos, target_x, target_y) {
        return Err("Too far from Capital!".to_string());
    }

    Ok(())
}

pub fn can_produce_at_tile(
    world: &mut World,
    player_id: PlayerId,
    target_x: usize,
    target_y: usize,
    master_data: &MasterDataRegistry,
) -> Result<(), String> {
    // この関数では全ユニット種別をチェックする代わりに「何かしら生産可能か」を確認する
    // 簡略化のため、occupancy と range だけチェックする
    let is_occupied = world
        .query_filtered::<&GridPosition, (With<Faction>, Without<Transporting>)>()
        .iter(world)
        .any(|pos| pos.x == target_x && pos.y == target_y);
    if is_occupied {
        return Err("Tile is occupied!".to_string());
    }

    let mut capital_pos = None;
    let mut is_valid_facility = false;
    let mut q_prop = world.query::<(&GridPosition, &Property)>();
    for (pos, prop) in q_prop.iter(world) {
        if prop.owner_id == Some(player_id) {
            if prop.terrain == Terrain::Capital {
                capital_pos = Some(*pos);
            }
            if pos.x == target_x
                && pos.y == target_y
                && master_data.is_production_facility(prop.terrain.as_str())
            {
                is_valid_facility = true;
            }
        }
    }

    if !is_valid_facility {
        return Err("Not a production facility!".to_string());
    }

    if !is_within_production_range(capital_pos, target_x, target_y) {
        return Err("Too far from Capital!".to_string());
    }

    Ok(())
}

pub fn can_produce_at(
    world: &mut World,
    player_id: PlayerId,
    target_x: usize,
    target_y: usize,
    unit_type: UnitType,
    master_data: &MasterDataRegistry,
) -> Result<(), String> {
    let is_occupied = world
        .query_filtered::<&GridPosition, (With<Faction>, Without<Transporting>)>()
        .iter(world)
        .any(|pos| pos.x == target_x && pos.y == target_y);

    let mut landscape_name = None;
    let mut capital_pos = None;
    let mut q_prop = world.query::<(&GridPosition, &Property)>();
    for (pos, prop) in q_prop.iter(world) {
        if prop.owner_id == Some(player_id) {
            if pos.x == target_x && pos.y == target_y {
                landscape_name = Some(prop.terrain.as_str());
            }
            if prop.terrain == Terrain::Capital {
                capital_pos = Some(*pos);
            }
        }
    }

    check_production_rules(
        is_occupied,
        landscape_name,
        capital_pos,
        target_x,
        target_y,
        unit_type,
        master_data,
    )
}

#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub fn produce_unit_system(
    mut commands: Commands,
    mut produce_events: EventReader<ProduceUnitCommand>,
    mut players: ResMut<Players>,
    match_state: Res<MatchState>,
    q_properties: Query<(&GridPosition, &Property)>,
    q_units: Query<&GridPosition, (With<Faction>, Without<Transporting>)>,
    master_data: Res<MasterDataRegistry>,
    unit_registry: Res<UnitRegistry>,
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Main {
        return;
    }
    let active_player_id = players.0[match_state.active_player_index.0].id;

    for event in produce_events.read() {
        if event.player_id != active_player_id {
            continue;
        }

        let is_occupied = q_units
            .iter()
            .any(|pos| pos.x == event.target_x && pos.y == event.target_y);

        let mut landscape_name = None;
        let mut capital_pos = None;
        for (pos, prop) in q_properties.iter() {
            if prop.owner_id == Some(event.player_id) {
                if pos.x == event.target_x && pos.y == event.target_y {
                    landscape_name = Some(prop.terrain.as_str());
                }
                if prop.terrain == Terrain::Capital {
                    capital_pos = Some(*pos);
                }
            }
        }

        if let Err(_e) = check_production_rules(
            is_occupied,
            landscape_name,
            capital_pos,
            event.target_x,
            event.target_y,
            event.unit_type,
            &master_data,
        ) {
            continue;
        }

        // イベントで指定されたプレイヤーを可変参照で取得する（存在しない場合はスキップ）
        let Some(player) = players.0.iter_mut().find(|p| p.id == event.player_id) else {
            continue;
        };
        let stats = match unit_registry.get_stats(event.unit_type) {
            Some(s) => s.clone(),
            None => continue,
        };

        if player.funds < stats.cost {
            continue; // Insufficient funds
        }

        player.funds -= stats.cost;

        let spawn_cmd = commands.spawn((
            GridPosition {
                x: event.target_x,
                y: event.target_y,
            },
            Faction(event.player_id),
            Health {
                current: 100,
                max: 100,
            },
            Fuel {
                current: stats.max_fuel,
                max: stats.max_fuel,
            },
            Ammo {
                ammo1: stats.max_ammo1,
                max_ammo1: stats.max_ammo1,
                ammo2: stats.max_ammo2,
                max_ammo2: stats.max_ammo2,
            },
            stats.clone(),
            HasMoved(true), // Produced units cannot move immediately
            ActionCompleted(true),
        ));

        // 輸送ユニットの場合、CargoCapacityコンポーネントを追加
        if stats.max_cargo > 0 {
            let entity = spawn_cmd.id();
            commands.entity(entity).insert(CargoCapacity {
                max: stats.max_cargo,
                loaded: vec![],
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_produce_unit_system() {
        let mut world = World::new();

        world.insert_resource(MatchState::default());
        world.insert_resource(Players(vec![
            Player {
                id: PlayerId(1),
                name: "P1".to_string(),
                funds: 2000,
            },
            Player {
                id: PlayerId(2),
                name: "P2".to_string(),
                funds: 0,
            },
        ]));

        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(2, 0, Terrain::Factory).unwrap();
        world.insert_resource(map);

        world.insert_resource(Events::<ProduceUnitCommand>::default());
        world.insert_resource(MasterDataRegistry::load().unwrap());

        // Spawn properties
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(PlayerId(1))),
        ));
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(Terrain::Factory, Some(PlayerId(1))),
        ));

        let stats = UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1000,
            max_movement: 3,
            movement_type: MovementType::Infantry,
            max_fuel: 99,
            max_ammo1: 0,
            max_ammo2: 0,
            min_range: 1,
            max_range: 1,
            daily_fuel_consumption: 0,
            can_capture: true,
            can_supply: false,
            max_cargo: 0,
            loadable_unit_types: vec![],
        };

        let mut registry = UnitRegistry(std::collections::HashMap::new());
        registry.0.insert(UnitType::Infantry, stats);
        world.insert_resource(registry);
        world.send_event(ProduceUnitCommand {
            player_id: PlayerId(1),
            target_x: 2,
            target_y: 0,
            unit_type: UnitType::Infantry,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(produce_unit_system);
        schedule.run(&mut world);

        // Check if unit was spawned
        let mut query = world.query::<(&Faction, &UnitStats, &GridPosition)>();
        let mut iter = query.iter(&world);
        let (faction, spawned_stats, pos) = iter.next().expect("Unit should have been spawned");
        assert_eq!(faction.0, PlayerId(1));
        assert_eq!(pos.x, 2);
        assert_eq!(pos.y, 0);
        assert_eq!(spawned_stats.unit_type, UnitType::Infantry);

        // Check if funds were deducted
        let players = world.resource::<Players>();
        assert_eq!(players.0[0].funds, 1000); // 2000 - 1000
    }

    #[test]
    fn test_production_collision() {
        let mut world = World::new();

        world.insert_resource(MatchState::default());
        world.insert_resource(Players(vec![Player {
            id: PlayerId(1),
            name: "P1".to_string(),
            funds: 2000,
        }]));

        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(2, 0, Terrain::Factory).unwrap();
        world.insert_resource(map);

        world.init_resource::<Events<ProduceUnitCommand>>();
        world.insert_resource(MasterDataRegistry::load().unwrap());

        // 首都を配置
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(PlayerId(1))),
        ));

        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(Terrain::Factory, Some(PlayerId(1))),
        ));

        // 既にユニットを配置
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Faction(PlayerId(1)),
            Health {
                current: 100,
                max: 100,
            },
            UnitStats::mock(),
        ));

        let mut registry = UnitRegistry(std::collections::HashMap::new());
        registry.0.insert(
            UnitType::Infantry,
            UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                ..UnitStats::mock()
            },
        );
        world.insert_resource(registry);

        world.send_event(ProduceUnitCommand {
            player_id: PlayerId(1),
            target_x: 2,
            target_y: 0,
            unit_type: UnitType::Infantry,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(produce_unit_system);
        schedule.run(&mut world);

        // 資金が減っていないことを確認
        let players = world.resource::<Players>();
        assert_eq!(players.0[0].funds, 2000);

        // ユニットが増えていないことを確認 (既存の1体のみ)
        // Note: GridPositionを持つエンティティは Property と Unit の2つあるはず
        let mut query = world.query_filtered::<&GridPosition, With<Faction>>();
        let count = query.iter(&world).filter(|p| p.x == 2 && p.y == 0).count();
        assert_eq!(count, 1);
    }
}
