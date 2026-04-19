use super::eval::evaluate_board;
use super::pruning::is_suicidal_attack;
use crate::components::{ActionCompleted, Faction, GridPosition, HasMoved, PlayerId, UnitStats};
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
            let attack_score = wait_score + 1000;
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
