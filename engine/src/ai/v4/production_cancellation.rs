//! 発行直前の検証で取り消した購入案の意図を、同じ手番のうちに解放する（D-5）。
//!
//! 取り消しは「未発行の取消」であり、生産の失敗や遅延ではない。購入案と同時に保存した
//! 配備意図・受入slot予約・占領意図だけを外し、他player・他手番・発行済み・完成済みの
//! 記録には触れない。永続Planの工程は発行前（Planned）のまま残り、次手番の継続判定が
//! 生産失敗として扱って同じ計画内で再配置する（Issuedへ進めていないので戻す状態は無い）。

use crate::ai::v4::*;

/// 汎用V4生産の発行待ち列で取り消した購入案の意図を解放する。
pub(in crate::ai::v4) fn release_cancelled_purchases(
    world: &mut World,
    turn: u32,
    cancelled: &[PlannedProduction],
) {
    for planned in cancelled {
        if planned.deployment.is_some()
            && let Some(mut registry) = world.get_resource_mut::<deployment::V4DeploymentRegistry>()
        {
            registry.cancel_unissued_order(
                planned.command.player_id,
                turn,
                GridPosition {
                    x: planned.command.target_x,
                    y: planned.command.target_y,
                },
                planned.command.unit_type,
            );
        }
        if let Some(intent) = &planned.capture_intent {
            release_campaign_record(world, intent.command.player_id, turn, &intent.command);
        }
    }
}

/// 島嶼campaign専用列で取り消した命令の発注記録を解放する。
pub(in crate::ai::v4) fn release_cancelled_campaign_commands(
    world: &mut World,
    player_id: PlayerId,
    planned_turn: u32,
    cancelled: &[ProduceUnitCommand],
) {
    for command in cancelled {
        release_campaign_record(world, player_id, planned_turn, command);
    }
}

fn release_campaign_record(
    world: &mut World,
    player_id: PlayerId,
    turn: u32,
    command: &ProduceUnitCommand,
) {
    if let Some(mut registry) =
        world.get_resource_mut::<campaign_execution::V4CampaignExecutionRegistry>()
    {
        registry.cancel_unissued(player_id, turn, command);
    }
}
