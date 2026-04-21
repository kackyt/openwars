use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;
use std::collections::HashSet;

fn apply_daily_updates_for_unit(
    stats: &UnitStats,
    pos: &GridPosition,
    map: &Map,
    fuel: &mut Fuel,
    hp: &mut Health,
) {
    if hp.is_destroyed() {
        return;
    }
    if stats.movement_type == MovementType::Air {
        let terrain = map.get_terrain(pos.x, pos.y);
        if terrain != Some(Terrain::Airport) {
            if fuel.current == 0 {
                hp.current = 0; // Destroyed
            } else {
                fuel.current = fuel.current.saturating_sub(stats.daily_fuel_consumption);
            }
        }
    }
}

/// 全ユニットに対して日次更新（燃料消費、墜落判定）を適用します。
#[allow(clippy::type_complexity)]
fn run_daily_update_for_all(
    q_units: &mut Query<(
        Entity,
        &mut HasMoved,
        &mut ActionCompleted,
        &Faction,
        &UnitStats,
        &mut Fuel,
        &mut Ammo,
        &mut Health,
        &GridPosition,
    )>,
    map: &Map,
) {
    for (_, _, _, _, stats, mut fuel, _, mut hp, pos) in q_units.iter_mut() {
        apply_daily_updates_for_unit(stats, pos, map, &mut fuel, &mut hp);
    }
}

/// フェーズの進行、ターンの切り替え、拠点による資金増加と自動補給を管理します。
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub fn next_phase_system(
    mut commands: Commands,
    mut match_state: ResMut<MatchState>,
    mut next_phase_events: EventReader<NextPhaseCommand>,
    mut phase_changed_events: EventWriter<GamePhaseChangedEvent>,
    mut q_units: Query<(
        Entity,
        &mut HasMoved,
        &mut ActionCompleted,
        &Faction,
        &UnitStats,
        &mut Fuel,
        &mut Ammo,
        &mut Health,
        &GridPosition,
    )>,
    mut players: ResMut<Players>,
    q_properties: Query<(&GridPosition, &Property)>,
    map: Res<Map>,
    registry: Res<MasterDataRegistry>,
) {
    if match_state.game_over.is_some() {
        return;
    }

    for _ in next_phase_events.read() {
        // ターン終了命令受信時に全ユニットの状態をリセット
        for (_, mut has_moved, mut action_completed, _, _, _, _, _, _) in q_units.iter_mut() {
            has_moved.0 = false;
            action_completed.0 = false;
        }

        // 移動履歴を強制削除 (ターン終了時は移動中であってはならない)
        commands.remove_resource::<PendingMove>();
        // 常に次のプレイヤーの Main フェーズまで一気に進めます。
        // これまでは EndTurn フェーズで止まっていましたが、2回クリックが必要になるためアトミック化します。

        match_state.active_player_index.0 += 1;

        // プレイヤー一周による日次更新
        if match_state.active_player_index.0 >= players.0.len() {
            match_state.active_player_index.0 = 0;
            match_state.current_turn_number.0 += 1;

            // 全ユニットの日次更新（燃料消費・墜落）を実行
            run_daily_update_for_all(&mut q_units, &map);
        }

        match_state.current_phase = Phase::Main;
        let active_player_id = players.0[match_state.active_player_index.0].id;

        // 次のプレイヤーの補給、資金増加
        process_resupply(
            active_player_id,
            &mut players,
            &q_properties,
            &mut q_units,
            &registry,
        );

        // UIへ通知 (Mainフェーズ開始のみ通知)
        phase_changed_events.send(GamePhaseChangedEvent {
            new_phase: Phase::Main,
            active_player: active_player_id,
        });
    }
}

#[allow(clippy::type_complexity)]
fn process_resupply(
    active_player_id: PlayerId,
    players: &mut Players,
    q_properties: &Query<(&GridPosition, &Property)>,
    q_units: &mut Query<(
        Entity,
        &mut HasMoved,
        &mut ActionCompleted,
        &Faction,
        &UnitStats,
        &mut Fuel,
        &mut Ammo,
        &mut Health,
        &GridPosition,
    )>,
    registry: &MasterDataRegistry,
) {
    // Apply property resupply
    let mut owned_properties = HashSet::new();
    let mut budget_increase = 0;
    for (pos, prop) in q_properties.iter() {
        if prop.owner_id == Some(active_player_id) {
            // Count for income
            budget_increase += registry.landscape_income(prop.terrain.as_str());

            // Collect for resupply check
            if prop.terrain == Terrain::City
                || prop.terrain == Terrain::Airport
                || prop.terrain == Terrain::Factory
                || prop.terrain == Terrain::Port
                || prop.terrain == Terrain::Capital
            {
                owned_properties.insert((pos.x, pos.y));
            }
        }
    }

    // Add funds
    let active_player_idx = players
        .0
        .iter()
        .position(|p| p.id == active_player_id)
        .unwrap();
    players.0[active_player_idx].funds += budget_increase;

    // Property resupply
    for (_, _, _, faction, stats, mut fuel, mut ammo, mut hp, pos) in q_units.iter_mut() {
        if faction.0 == active_player_id {
            // 日次更新 (燃料消費、墜落判定) は next_phase_system でラウンド単位で行われるためここでは削除
            if hp.is_destroyed() {
                continue;
            }
            if owned_properties.contains(&(pos.x, pos.y)) {
                // 回復・補充にかかるコストを計算
                // HP回復（最大20回復）
                let hp_to_restore = 20.min(hp.max.saturating_sub(hp.current));
                let repair_cost = (stats.cost as f32 * (hp_to_restore as f32 / 100.0)) as u32;

                let ammo_diff = (stats.max_ammo1.saturating_sub(ammo.ammo1))
                    + (stats.max_ammo2.saturating_sub(ammo.ammo2));
                let fuel_diff = stats.max_fuel.saturating_sub(fuel.current);
                let resupply_cost = ammo_diff * 15 + fuel_diff * 5;

                let total_cost = repair_cost + resupply_cost;

                if players.0[active_player_idx].funds >= total_cost && total_cost > 0 {
                    players.0[active_player_idx].funds -= total_cost;
                    hp.current = (hp.current + hp_to_restore).min(hp.max);
                    fuel.current = stats.max_fuel;
                    ammo.ammo1 = stats.max_ammo1;
                    ammo.ammo2 = stats.max_ammo2;
                } else if players.0[active_player_idx].funds >= resupply_cost && resupply_cost > 0 {
                    // 資金不足で修理はできないが、補給だけはできる場合
                    players.0[active_player_idx].funds -= resupply_cost;
                    fuel.current = stats.max_fuel;
                    ammo.ammo1 = stats.max_ammo1;
                    ammo.ammo2 = stats.max_ammo2;
                }
            }
        }
    }
}

/// ユニットの待機コマンドを処理します。
pub fn wait_unit_system(
    mut wait_events: EventReader<WaitUnitCommand>,
    mut q_units: Query<(&Faction, &mut ActionCompleted)>,
    players: Res<Players>,
    match_state: Res<MatchState>,
    mut commands: Commands,
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Main {
        return;
    }
    let active_player = players.0[match_state.active_player_index.0].id;

    for ev in wait_events.read() {
        if let Ok((faction, mut action_comp)) = q_units.get_mut(ev.unit_entity) {
            if faction.0 != active_player {
                continue;
            }
            action_comp.0 = true;
            // アクション確定時に移動履歴を削除
            commands.remove_resource::<PendingMove>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_world() -> (World, Schedule) {
        let mut world = World::new();
        let mut schedule = Schedule::default();

        world.insert_resource(MatchState::default());
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));
        world.insert_resource(Map::new(5, 5, Terrain::Plains, GridTopology::Square));
        world.insert_resource(MasterDataRegistry::load().unwrap());
        world.init_resource::<Events<NextPhaseCommand>>();
        world.init_resource::<Events<GamePhaseChangedEvent>>();

        schedule.add_systems(next_phase_system);

        (world, schedule)
    }

    #[test]
    fn test_turn_progression() {
        let (mut world, mut schedule) = setup_world();

        // Initially Player 1, Turn 1
        {
            let ms = world.resource::<MatchState>();
            assert_eq!(ms.active_player_index.0, 0);
            assert_eq!(ms.current_turn_number.0, 1);
        }

        // P1 -> P2
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);
        {
            let ms = world.resource::<MatchState>();
            assert_eq!(ms.active_player_index.0, 1);
            assert_eq!(ms.current_turn_number.0, 1);
            assert_eq!(ms.current_phase, Phase::Main);
        }

        // P2 -> P1 (New Turn)
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);
        {
            let ms = world.resource::<MatchState>();
            assert_eq!(ms.active_player_index.0, 0);
            assert_eq!(ms.current_turn_number.0, 2);
        }
    }

    #[test]
    fn test_air_unit_fuel_and_crash() {
        let (mut world, mut schedule) = setup_world();

        // Spawn a bomber for P1 (consumes 5 fuel per round)
        let bomber = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Bomber,
                    movement_type: MovementType::Air,
                    daily_fuel_consumption: 5,
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 10,
                    max: 50,
                },
                Health {
                    current: 100,
                    max: 100,
                },
                HasMoved(false),
                ActionCompleted(false),
                Ammo {
                    ammo1: 0,
                    max_ammo1: 0,
                    ammo2: 0,
                    max_ammo2: 0,
                },
            ))
            .id();

        // Round 1: P1 -> P2 (NextPhaseCommand 1)
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);
        {
            let fuel = world.get::<Fuel>(bomber).unwrap();
            assert_eq!(
                fuel.current, 10,
                "Fuel should not decrease on midway phase change"
            );
        }

        // Round 1 ends: P2 -> P1 (NextPhaseCommand 2)
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);
        {
            let fuel = world.get::<Fuel>(bomber).unwrap();
            assert_eq!(
                fuel.current, 5,
                "Fuel should decrease exactly once per full round"
            );
        }

        // Round 2 ends: P2 -> P1 (NextPhaseCommand 4 total)
        world.send_event(NextPhaseCommand); // P1 -> P2
        schedule.run(&mut world);
        world.send_event(NextPhaseCommand); // P2 -> P1
        schedule.run(&mut world);
        {
            let fuel = world.get::<Fuel>(bomber).unwrap();
            assert_eq!(fuel.current, 0, "Fuel should be 0");
        }

        // Round 3 ends: P2 -> P1 (Crash)
        world.send_event(NextPhaseCommand); // P1 -> P2
        schedule.run(&mut world);
        world.send_event(NextPhaseCommand); // P2 -> P1
        schedule.run(&mut world);
        {
            let hp = world.get::<Health>(bomber).unwrap();
            assert_eq!(
                hp.current, 0,
                "Aircraft with 0 fuel not on airport should crash"
            );
        }
    }

    #[test]
    fn test_all_units_reset_on_next_phase() {
        let (mut world, mut schedule) = setup_world();

        // P1 unit (will act)
        let p1_unit = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                Faction(PlayerId(1)),
                UnitStats::mock(),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: 10,
                    max: 10,
                },
                Ammo {
                    ammo1: 0,
                    max_ammo1: 0,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                HasMoved(true),
                ActionCompleted(true),
            ))
            .id();

        // P2 unit (already acted somehow, maybe in previous turn)
        let p2_unit = world
            .spawn((
                GridPosition { x: 1, y: 1 },
                Faction(PlayerId(2)),
                UnitStats::mock(),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: 10,
                    max: 10,
                },
                Ammo {
                    ammo1: 0,
                    max_ammo1: 0,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                HasMoved(true),
                ActionCompleted(true),
            ))
            .id();

        // P1 turn ends -> P2 turn starts
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);

        // BOTH units should be reset immediately
        let p1_moved = world.get::<HasMoved>(p1_unit).unwrap();
        let p1_action = world.get::<ActionCompleted>(p1_unit).unwrap();
        let p2_moved = world.get::<HasMoved>(p2_unit).unwrap();
        let p2_action = world.get::<ActionCompleted>(p2_unit).unwrap();

        assert!(!p1_moved.0, "P1 unit should be reset");
        assert!(!p1_action.0, "P1 unit should be reset");
        assert!(!p2_moved.0, "P2 unit should be reset");
        assert!(!p2_action.0, "P2 unit should be reset");
    }
}
