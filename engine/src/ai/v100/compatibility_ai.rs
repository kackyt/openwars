//! V100/V200専用AIの1ステップ実行器。
//!
//! 行動と生産の判断は同じ`v100`配下へ分離し、ここでは命令の発行順だけを管理する。

use crate::ai::engine::{AiActionCooldown, AiProductionCooldown, execute_ai_command};
use crate::components::{ActionCompleted, CargoCapacity, PlayerId};
use crate::events::{NextPhaseCommand, ProduceUnitCommand};
use bevy_ecs::prelude::*;

/// V100/V200の手番を1ステップ進める。既存V1〜V4の決定器は使用しない。
pub(super) fn execute_turn(world: &mut World, player_id: PlayerId) -> Option<String> {
    let skipped = world
        .get_resource::<AiActionCooldown>()
        .map(|value| value.0.clone())
        .unwrap_or_default();
    if let Some((entity, command)) = super::action::decide_action(world, player_id, &skipped) {
        let continues_rom_drop = match &command {
            crate::ai::engine::AiCommand::Drop { cargo_entity, .. } => {
                world.get::<CargoCapacity>(entity).is_some_and(|capacity| {
                    capacity.loaded.iter().any(|loaded| {
                        *loaded != *cargo_entity
                            && world
                                .get::<ActionCompleted>(*loaded)
                                .is_some_and(|completed| !completed.0)
                    })
                })
            }
            _ => false,
        };
        let text = format!("{:?}", command);
        execute_ai_command(world, entity, command);
        // ROM命令08が上下ニブルへ同時指定する2人目は、OpenWarsの次stepで
        // 状態反映後に降ろす。積荷が尽きた時点で通常どおり輸送役を完了させる。
        if !continues_rom_drop {
            world
                .get_resource_or_insert_with(AiActionCooldown::default)
                .0
                .insert(entity);
        }
        return Some(text);
    }
    if let Some(command) = super::production::decide_production(world, player_id) {
        let text = format!("{:?}", command);
        super::unit_record::reserve_production_record(world, &command);
        world
            .get_resource_or_insert_with(AiProductionCooldown::default)
            .0
            .insert((command.target_x, command.target_y));
        world
            .get_resource_mut::<Events<ProduceUnitCommand>>()?
            .send(command);
        return Some(text);
    }
    world
        .get_resource_mut::<Events<NextPhaseCommand>>()?
        .send(NextPhaseCommand);
    None
}
