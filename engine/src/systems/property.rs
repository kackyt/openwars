use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;

/// 拠点の占領・修理コマンド(`CapturePropertyCommand`)を処理するシステム。
///
/// 【処理の流れ】
/// 1. ユニットが占領能力を持ち、行動済みでないことを確認します。
/// 2. ユニットの現在地と同じ座標にある拠点(`Property`)を取得します。
/// 3. すでに自軍の拠点であれば、耐久値（占領ポイント）を回復（修理）します。
/// 4. 敵軍または中立の拠点であれば、ユニットのHPに応じたアクションパワーで占領ポイントを減らします。
/// 5. 占領ポイントが0以下になった場合、拠点の所有者を自軍に変更し、`PropertyCapturedEvent` を発行します。
/// 6. ユニットの `ActionCompleted` を true に設定します。
///
/// 指定されたユニットが現在地で占領可能な拠点エンティティを返します。
pub fn get_capturable_property(world: &mut World, unit: Entity) -> Option<Entity> {
    let (unit_pos, unit_stats, unit_faction) = {
        let mut q_unit = world.query::<(&GridPosition, &UnitStats, &Faction)>();
        let Ok((u_pos, u_stats, u_faction)) = q_unit.get(world, unit) else {
            return None;
        };
        (*u_pos, u_stats.clone(), u_faction.0)
    };

    if !unit_stats.can_capture {
        return None;
    }

    let mut q_properties = world.query::<(Entity, &GridPosition, &Property)>();
    for (p_ent, p_pos, p_prop) in q_properties.iter(world) {
        if p_pos.x == unit_pos.x && p_pos.y == unit_pos.y && p_prop.owner_id != Some(unit_faction) {
            return Some(p_ent);
        }
    }

    None
}

#[allow(clippy::too_many_arguments)]
pub fn capture_property_system(
    mut capture_events: EventReader<CapturePropertyCommand>,
    mut captured_events: EventWriter<PropertyCapturedEvent>,
    mut q_units: Query<(
        Entity,
        &GridPosition,
        &Faction,
        &Health,
        &UnitStats,
        &mut ActionCompleted,
    )>,
    mut q_properties: Query<(&GridPosition, &mut Property)>,
    match_state: Res<MatchState>,
    players: Res<Players>,
    _map: Res<Map>,
    mut commands: Commands,
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Main {
        return;
    }
    let active_player_id = players.0[match_state.active_player_index.0].id;

    for event in capture_events.read() {
        let Ok((_, pos, faction, hp, stats, mut action)) = q_units.get_mut(event.unit_entity)
        else {
            continue;
        };

        if faction.0 != active_player_id || action.0 || hp.is_destroyed() || !stats.can_capture {
            continue;
        }

        let action_power = hp.get_display_hp() * 10;
        let pos = *pos;
        let mut captured = false;
        let mut new_owner = None;

        for (prop_pos, mut prop) in q_properties.iter_mut() {
            if prop_pos.x == pos.x && prop_pos.y == pos.y {
                let max_points = prop.terrain.max_capture_points();
                if max_points == 0 {
                    continue; // Not capturable
                }

                if prop.owner_id == Some(active_player_id) {
                    // Repair
                    prop.capture_points =
                        std::cmp::min(prop.capture_points + action_power, max_points);
                } else {
                    // Capture
                    if prop.capture_points <= action_power {
                        prop.owner_id = Some(active_player_id);
                        prop.capture_points = max_points;
                        captured = true;
                        new_owner = Some(active_player_id);
                    } else {
                        prop.capture_points -= action_power;
                    }
                }
                action.0 = true;
                break;
            }
        }

        if captured {
            captured_events.send(PropertyCapturedEvent {
                x: pos.x,
                y: pos.y,
                new_owner,
            });
        }
        // アクション確定時に移動履歴を削除
        commands.remove_resource::<PendingMove>();
    }
}

/// 勝敗判定システム。ターン終了時または拠点が占領された後に呼ばれるべきです。
pub fn victory_check_system(
    mut match_state: ResMut<MatchState>,
    players: Res<Players>,
    q_properties: Query<&Property>,
    q_units: Query<(&Faction, &Health)>,
) {
    if match_state.game_over.is_some() {
        return;
    }

    let mut alive_players = Vec::new();
    for player in &players.0 {
        let mut has_capital = false;
        for prop in q_properties.iter() {
            if prop.owner_id == Some(player.id) && prop.terrain == Terrain::Capital {
                has_capital = true;
                break;
            }
        }

        let mut has_units = false;
        for (u_fac, u_hp) in q_units.iter() {
            if u_fac.0 == player.id && !u_hp.is_destroyed() {
                has_units = true;
                break;
            }
        }

        let is_annihilated = match_state.current_turn_number.0 > 1 && !has_units;
        if has_capital && !is_annihilated {
            alive_players.push(player.id);
        }
    }

    if alive_players.len() == 1 {
        match_state.game_over = Some(GameOverCondition::Winner(alive_players[0]));
    } else if alive_players.is_empty() {
        match_state.game_over = Some(GameOverCondition::Draw);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capture_property_system() {
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

        world.insert_resource(Map::new(5, 5, Terrain::Plains, GridTopology::Square));
        world.insert_resource(Events::<CapturePropertyCommand>::default());
        world.insert_resource(Events::<PropertyCapturedEvent>::default());

        let unit_entity = world
            .spawn((
                GridPosition { x: 2, y: 2 },
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
                    max_ammo1: 0,
                    max_ammo2: 0,
                    min_range: 1,
                    max_range: 1,
                    daily_fuel_consumption: 0,
                    can_capture: true,
                    can_supply: false,
                    max_cargo: 0,
                    loadable_unit_types: vec![],
                    ..Default::default()
                },
                ActionCompleted(false),
            ))
            .id();

        world.spawn((
            GridPosition { x: 2, y: 2 },
            Property {
                terrain: Terrain::City,
                owner_id: None,
                capture_points: 200, // max is 200
            },
        ));

        // Add dummy capitals so players aren't instantly defeated upon capture
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property {
                terrain: Terrain::Capital,
                owner_id: Some(PlayerId(1)),
                capture_points: 200,
            },
        ));
        world.spawn((
            GridPosition { x: 4, y: 4 },
            Property {
                terrain: Terrain::Capital,
                owner_id: Some(PlayerId(2)),
                capture_points: 200,
            },
        ));
        world.spawn((
            GridPosition { x: 4, y: 4 },
            Faction(PlayerId(2)),
            Health {
                current: 100,
                max: 100,
            },
        )); // dummy unit for P2

        world.send_event(CapturePropertyCommand { unit_entity });

        let mut schedule = Schedule::default();
        schedule.add_systems(capture_property_system);
        schedule.run(&mut world);

        // Action power is 10 * 10 = 100
        let mut query = world.query::<&Property>();
        let mut iter = query.iter(&world);
        let prop = iter.find(|p| p.terrain == Terrain::City).unwrap();

        assert_eq!(prop.capture_points, 100);
        assert_eq!(prop.owner_id, None);

        let action = world.get::<ActionCompleted>(unit_entity).unwrap();
        assert!(action.0); // Used action

        // Reset action and capture again
        world.get_mut::<ActionCompleted>(unit_entity).unwrap().0 = false;
        world.send_event(CapturePropertyCommand { unit_entity });
        schedule.run(&mut world);

        let mut iter = query.iter(&world);
        let prop = iter.find(|p| p.terrain == Terrain::City).unwrap();

        assert_eq!(prop.capture_points, 200); // Reset after capture
        assert_eq!(prop.owner_id, Some(PlayerId(1)));

        let events = world.resource::<Events<PropertyCapturedEvent>>();
        let mut cursor = events.get_cursor();
        let evs: Vec<_> = cursor.read(events).collect();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].x, 2);
        assert_eq!(evs[0].new_owner, Some(PlayerId(1)));
    }

    #[test]
    fn test_victory_check_winner() {
        let mut world = World::new();
        let ms = MatchState {
            current_turn_number: TurnNumber(2),
            ..Default::default()
        };
        world.insert_resource(ms);
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));

        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property {
                terrain: Terrain::Capital,
                owner_id: Some(PlayerId(1)),
                capture_points: 200,
            },
        ));
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Faction(PlayerId(1)),
            Health {
                current: 100,
                max: 100,
            },
        ));

        // P2 has no capital and no units -> gets eliminated

        let mut schedule = Schedule::default();
        schedule.add_systems(victory_check_system);
        schedule.run(&mut world);

        let ms = world.resource::<MatchState>();
        assert_eq!(ms.game_over, Some(GameOverCondition::Winner(PlayerId(1))));
    }

    #[test]
    fn test_victory_check_draw() {
        let mut world = World::new();
        let ms = MatchState {
            current_turn_number: TurnNumber(2),
            ..Default::default()
        };
        world.insert_resource(ms);
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));

        // No players have units or capitals, they all get eliminated -> Draw

        let mut schedule = Schedule::default();
        schedule.add_systems(victory_check_system);
        schedule.run(&mut world);

        let ms = world.resource::<MatchState>();
        assert_eq!(ms.game_over, Some(GameOverCondition::Draw));
    }

    #[test]
    fn test_victory_check_turn1_exception() {
        let mut world = World::new();
        let ms = MatchState {
            current_turn_number: TurnNumber(1), // Turn 1!
            ..Default::default()
        };
        world.insert_resource(ms);
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));

        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property {
                terrain: Terrain::Capital,
                owner_id: Some(PlayerId(1)),
                capture_points: 200,
            },
        ));
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property {
                terrain: Terrain::Capital,
                owner_id: Some(PlayerId(2)),
                capture_points: 200,
            },
        ));
        // No units for either player, but since it's turn 1 they shouldn't be annihilated yet!

        let mut schedule = Schedule::default();
        schedule.add_systems(victory_check_system);
        schedule.run(&mut world);

        let ms = world.resource::<MatchState>();
        assert_eq!(ms.game_over, None); // Game should not be over yet
    }
}
