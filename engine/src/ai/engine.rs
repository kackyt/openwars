use super::eval::evaluate_board;
use super::pruning::is_suicidal_attack;
use crate::components::{
    ActionCompleted, Faction, GridPosition, HasMoved, Health, PlayerId, UnitStats,
};
use crate::events::{AttackUnitCommand, CapturePropertyCommand, MoveUnitCommand, WaitUnitCommand};
use crate::resources::DamageChart;
use crate::systems::{combat::get_attackable_targets, get_available_actions};
use bevy_ecs::prelude::*;

#[derive(Debug, Clone)]
pub enum AiCommand {
    Move {
        target_pos: GridPosition,
    },
    Attack {
        target_pos: GridPosition,
        target_entity: Entity,
    },
    Capture {
        target_pos: GridPosition,
    },
    Wait {
        target_pos: GridPosition,
    },
}

/// AIの思考エンジン。未行動のユニットに対して最も評価の高いコマンドを決定します。
pub fn decide_ai_action(world: &mut World, player_id: PlayerId) -> Option<(Entity, AiCommand)> {
    // 未行動の自軍ユニットを取得
    let mut movable_units = Vec::new();
    {
        let mut query = world
            .query_filtered::<(Entity, &Faction, &HasMoved, &ActionCompleted), With<UnitStats>>();
        for (entity, faction, has_moved, action_completed) in query.iter(world) {
            if faction.0 == player_id && !has_moved.0 && !action_completed.0 {
                movable_units.push(entity);
            }
        }
    }

    if movable_units.is_empty() {
        return None; // 全ユニット行動済み
    }

    // 最初のユニットについて行動を決定（優先度順などは今後拡張可能）
    let unit_entity = movable_units[0];

    let original_pos = {
        let pos = world.get::<GridPosition>(unit_entity)?;
        *pos
    };

    let damage_chart = world.resource::<DamageChart>().clone();

    // 候補となる行動とその評価スコアを保持する
    let mut best_score = i32::MIN;
    let mut best_command = AiCommand::Wait {
        target_pos: original_pos,
    };

    // 簡単のため、今回は移動を伴わない「その場での行動」のみを列挙してシミュレートする。
    // （本来は get_reachable_cells などで全移動先を走査し、クローンしたワールドで evaluate_board を実行するべきだが、
    // ECS全体のクローンが重いため、ヒューリスティックに評価する）

    // 待機アクションのスコア
    let wait_score = evaluate_board(world, player_id);
    if wait_score > best_score {
        best_score = wait_score;
        best_command = AiCommand::Wait {
            target_pos: original_pos,
        };
    }

    // 占領アクションのスコア
    let actions = get_available_actions(world, unit_entity, false);
    if actions.can_capture {
        // 占領進行によるボーナスを加算して評価
        let capture_score = wait_score + 500;
        if capture_score > best_score {
            best_score = capture_score;
            best_command = AiCommand::Capture {
                target_pos: original_pos,
            };
        }
    }

    // 攻撃アクションのスコア
    let targets = get_attackable_targets(world, unit_entity, true);
    for target_entity in targets {
        if !is_suicidal_attack(world, unit_entity, target_entity, &damage_chart) {
            // カミカゼでない攻撃は高い評価を与える（ダメージ量に基づく加算等）
            let net_value = {
                let mut expected_damage_value = 0;
                let mut expected_self_damage_value = 0;

                let mut query = world.query::<(&Health, &UnitStats)>();
                if let (Ok((atk_health, atk_stats)), Ok((def_health, def_stats))) = (
                    query.get(world, unit_entity),
                    query.get(world, target_entity),
                ) && def_health.max > 0
                    && atk_health.max > 0
                {
                    let base_damage = damage_chart
                        .get_base_damage(atk_stats.unit_type, def_stats.unit_type)
                        .unwrap_or(0);
                    let atk_display = atk_health.current.div_ceil(10);
                    let expected_damage = (base_damage * atk_display) / 10;
                    let actual_damage = std::cmp::min(expected_damage, def_health.current);
                    expected_damage_value =
                        (actual_damage as i32 * def_stats.cost as i32) / def_health.max as i32;

                    let remaining_enemy_hp = def_health.current.saturating_sub(actual_damage);
                    let is_indirect = atk_stats.min_range > 1;
                    if remaining_enemy_hp > 0 && !is_indirect {
                        let counter_base = damage_chart
                            .get_base_damage(def_stats.unit_type, atk_stats.unit_type)
                            .unwrap_or(0);
                        let remaining_display = remaining_enemy_hp.div_ceil(10);
                        let expected_counter = (counter_base * remaining_display) / 10;
                        let actual_counter = std::cmp::min(expected_counter, atk_health.current);
                        expected_self_damage_value =
                            (actual_counter as i32 * atk_stats.cost as i32) / atk_health.max as i32;
                    }
                }
                expected_damage_value - expected_self_damage_value
            };

            let attack_score = wait_score + 1000 + net_value;
            if attack_score > best_score {
                best_score = attack_score;
                best_command = AiCommand::Attack {
                    target_pos: original_pos,
                    target_entity,
                };
            }
        }
    }

    Some((unit_entity, best_command))
}

/// AIコマンドを実際のエンジンコマンドに変換し、キューに送信する
pub fn execute_ai_command(world: &mut World, unit_entity: Entity, command: AiCommand) {
    match command {
        AiCommand::Move { target_pos } => {
            if let Some(mut evs) = world.get_resource_mut::<Events<MoveUnitCommand>>() {
                evs.send(MoveUnitCommand {
                    unit_entity,
                    target_x: target_pos.x,
                    target_y: target_pos.y,
                });
            }
            if let Some(mut evs) = world.get_resource_mut::<Events<WaitUnitCommand>>() {
                evs.send(WaitUnitCommand { unit_entity });
            }
        }
        AiCommand::Attack {
            target_pos,
            target_entity,
        } => {
            if let Some(mut evs) = world.get_resource_mut::<Events<MoveUnitCommand>>() {
                evs.send(MoveUnitCommand {
                    unit_entity,
                    target_x: target_pos.x,
                    target_y: target_pos.y,
                });
            }
            if let Some(mut evs) = world.get_resource_mut::<Events<AttackUnitCommand>>() {
                evs.send(AttackUnitCommand {
                    attacker_entity: unit_entity,
                    defender_entity: target_entity,
                });
            }
        }
        AiCommand::Capture { target_pos } => {
            if let Some(mut evs) = world.get_resource_mut::<Events<MoveUnitCommand>>() {
                evs.send(MoveUnitCommand {
                    unit_entity,
                    target_x: target_pos.x,
                    target_y: target_pos.y,
                });
            }
            if let Some(mut evs) = world.get_resource_mut::<Events<CapturePropertyCommand>>() {
                evs.send(CapturePropertyCommand { unit_entity });
            }
        }
        AiCommand::Wait { target_pos } => {
            if let Some(mut evs) = world.get_resource_mut::<Events<MoveUnitCommand>>() {
                evs.send(MoveUnitCommand {
                    unit_entity,
                    target_x: target_pos.x,
                    target_y: target_pos.y,
                });
            }
            if let Some(mut evs) = world.get_resource_mut::<Events<WaitUnitCommand>>() {
                evs.send(WaitUnitCommand { unit_entity });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{Health, UnitStats};
    use crate::resources::UnitType;

    #[test]
    fn test_decide_ai_action_no_units() {
        let mut world = World::new();
        assert!(decide_ai_action(&mut world, PlayerId(1)).is_none());
    }

    #[test]
    fn test_decide_ai_action_wait() {
        let mut world = World::new();
        world.spawn((
            PlayerId(1),
            Faction(PlayerId(1)),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 0, y: 0 },
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 1000,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));
        world.insert_resource(DamageChart::new());
        // Since there is no enemy to attack and no property to capture, it should return Wait.
        let action = decide_ai_action(&mut world, PlayerId(1));
        assert!(action.is_some());
        if let Some((_, AiCommand::Wait { .. })) = action {
        } else {
            panic!("Expected Wait command");
        }
    }

    #[test]
    fn test_decide_ai_action_attack() {
        let mut world = World::new();
        let mut dc = DamageChart::new();
        dc.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        world.insert_resource(dc);
        world.insert_resource(crate::resources::Map {
            width: 10,
            height: 10,
            tiles: vec![crate::resources::Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        let attacker = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    cost: 7000,
                    min_range: 1,
                    max_range: 1,
                    max_ammo1: 10,
                    max_ammo2: 10,
                    movement_type: crate::resources::MovementType::Tank,
                    max_movement: 6,
                    max_fuel: 99,
                    daily_fuel_consumption: 0,
                    can_capture: false,
                    can_supply: false,
                    max_cargo: 0,
                    loadable_unit_types: vec![],
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: 10,
                    max_ammo1: 10,
                    ammo2: 10,
                    max_ammo2: 10,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        let defender = world
            .spawn((
                p2,
                Faction(p2),
                GridPosition { x: 1, y: 2 }, // adjacent
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    min_range: 1,
                    max_range: 1,
                    max_ammo1: 10,
                    max_ammo2: 10,
                    movement_type: crate::resources::MovementType::Infantry,
                    max_movement: 3,
                    max_fuel: 99,
                    daily_fuel_consumption: 0,
                    can_capture: true,
                    can_supply: false,
                    max_cargo: 0,
                    loadable_unit_types: vec![],
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: 10,
                    max_ammo1: 10,
                    ammo2: 10,
                    max_ammo2: 10,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        let action = decide_ai_action(&mut world, p1);
        assert!(action.is_some());
        if let Some((entity, AiCommand::Attack { target_entity, .. })) = action {
            assert_eq!(entity, attacker);
            assert_eq!(target_entity, defender);
        } else {
            panic!("Expected Attack command");
        }
    }

    #[test]
    fn test_decide_ai_action_capture() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(crate::resources::Map {
            width: 10,
            height: 10,
            tiles: vec![crate::resources::Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        let p1 = PlayerId(1);

        let unit = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    can_capture: true,
                    movement_type: crate::resources::MovementType::Infantry,
                    max_movement: 3,
                    max_fuel: 99,
                    daily_fuel_consumption: 0,
                    can_supply: false,
                    max_cargo: 0,
                    loadable_unit_types: vec![],
                    max_ammo1: 0,
                    max_ammo2: 0,
                    min_range: 0,
                    max_range: 0,
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: 10,
                    max_ammo1: 10,
                    ammo2: 10,
                    max_ammo2: 10,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        // Neutral property on the same tile
        world.spawn((
            GridPosition { x: 1, y: 1 },
            crate::components::Property::new(crate::resources::Terrain::City, None, 200),
        ));

        let action = decide_ai_action(&mut world, p1);
        assert!(action.is_some());
        if let Some((entity, AiCommand::Capture { .. })) = action {
            assert_eq!(entity, unit);
        } else {
            panic!("Expected Capture command");
        }
    }
}

/// 一度の呼び出しで、該当勢力のAI行動（生産、または1ユニットの行動）を1ステップ実行し、イベントを発行します。
/// 行動可能ユニットがなくなったらターン終了コマンドを発行します。
/// 何らかの行動を実行した場合は true、ターンが終了した場合は false を返します。
pub fn execute_ai_turn(world: &mut World, active_player: PlayerId) -> bool {
    // 1. 生産行動
    let prod_commands = super::production::decide_production(world, active_player);
    if !prod_commands.is_empty() {
        if let Some(mut events) =
            world.get_resource_mut::<Events<crate::events::ProduceUnitCommand>>()
        {
            for cmd in prod_commands {
                events.send(cmd);
            }
        }
        // 生産の直後は一旦状態更新を待つために true を返す
        return true;
    }

    // 2. ユニット行動
    if let Some((entity, command)) = decide_ai_action(world, active_player) {
        execute_ai_command(world, entity, command);
        return true;
    }

    // 3. 全行動完了 -> ターン終了
    if let Some(mut end_events) =
        world.get_resource_mut::<Events<crate::events::NextPhaseCommand>>()
    {
        end_events.send(crate::events::NextPhaseCommand);
    }
    false
}
