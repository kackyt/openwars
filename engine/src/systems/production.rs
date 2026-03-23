use crate::components::*;
use crate::events::*;
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
/// 自軍の首都のある場所から生産可能範囲内にあるかどうかを判定するエンジン側の純粋なドメイン関数
pub fn is_within_production_range(
    capital_pos: Option<(usize, usize)>,
    target_x: usize,
    target_y: usize,
) -> bool {
    if let Some(cp) = capital_pos {
        let distance = (target_x as isize - cp.0 as isize).unsigned_abs()
            + (target_y as isize - cp.1 as isize).unsigned_abs();
        distance <= 3
    } else {
        false
    }
}

pub fn produce_unit_system(
    mut commands: Commands,
    mut produce_events: EventReader<ProduceUnitCommand>,
    mut players: ResMut<Players>,
    match_state: Res<MatchState>,
    _map: Res<Map>,
    q_properties: Query<(&GridPosition, &Property)>,
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

        let mut is_valid_property = false;

        for (pos, prop) in q_properties.iter() {
            if prop.owner_id == Some(event.player_id)
                && pos.x == event.target_x
                && pos.y == event.target_y
                && (prop.terrain == Terrain::Factory
                    || prop.terrain == Terrain::Capital
                    || prop.terrain == Terrain::Airport
                    || prop.terrain == Terrain::Port)
            {
                is_valid_property = true;
            }
        }

        if !is_valid_property {
            continue;
        }

        let mut capital_pos = None;
        for (pos, prop) in q_properties.iter() {
            if prop.owner_id == Some(event.player_id) && prop.terrain == Terrain::Capital {
                capital_pos = Some((pos.x, pos.y));
                break;
            }
        }

        if !is_within_production_range(capital_pos, event.target_x, event.target_y) {
            continue; // Too far from Capital
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

        commands.spawn((
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
            stats,
            HasMoved(true), // Produced units cannot move immediately
            ActionCompleted(true),
        ));
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
}

