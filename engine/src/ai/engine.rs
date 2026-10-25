//! AIの外部入口。内部の選択・状態・命令適用は各所有moduleへ分離する。
pub use crate::ai::{
    AiActionCooldown, AiCommand, AiProductionCooldown, clear_ai_cooldowns_system,
    execute_ai_command,
};
use crate::components::PlayerId;
use bevy_ecs::prelude::*;

/// 一度の呼び出しで、該当勢力のAI行動（生産、または1ユニットの行動）を1ステップ実行し、イベントを発行します。
/// 行動可能ユニットがなくなったらターン終了コマンドを発行します。
/// 何らかの行動を実行した場合はその行動内容（文字列）を `Some` で返し、ターンが終了した場合は `None` を返します。
/// AIのメイン実行エントリーポイント。
pub fn execute_ai_turn(world: &mut World, active_player: PlayerId) -> Option<String> {
    // 決着後の命令は全て無視される。行動を返し続けると、同じ命令を出し続けて手番が終わらない
    // （手番の途中で敵首都を落として勝った場合など）。
    if world
        .get_resource::<crate::resources::MatchState>()
        .is_some_and(|state| state.game_over.is_some())
    {
        return None;
    }
    crate::ai::wait_probe::begin_step(world);
    let ai_version = crate::ai::resolve_player_ai_version(world, active_player);

    match ai_version {
        crate::ai::ai_version::AiVersion::V1 => {
            crate::ai::v1::execute_ai_turn_v1(world, active_player)
        }
        // V100/V200は既存V1〜V4の決定器を呼ばない独立した逐次決定器を使用する。
        crate::ai::ai_version::AiVersion::V100 | crate::ai::ai_version::AiVersion::V200 => {
            crate::ai::v100::execute_turn(world, active_player)
        }
        // V3/V4 は V2 と同じ部隊編成・ビーム探索パイプラインを共有し、
        // タイル評価 (decide_ai_action_v2) と盤面評価の中でバージョン別の強化を行う
        // （V4 の差分は生産判断のみで、行動決定パイプラインは V3 と同一）
        crate::ai::ai_version::AiVersion::V2
        | crate::ai::ai_version::AiVersion::V3
        | crate::ai::ai_version::AiVersion::V4 => {
            crate::ai::runtime::turn::execute_turn_step(world, active_player)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::master_data::MasterDataRegistry;
    use crate::resources::{GameOverCondition, GridTopology, MatchState};

    #[test]
    fn ai_takes_no_action_after_the_game_is_decided() {
        let registry = MasterDataRegistry::load().unwrap();
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &registry,
            "map_1",
            GridTopology::Hex,
        )
        .unwrap();
        world.resource_mut::<MatchState>().game_over = Some(GameOverCondition::Winner(PlayerId(1)));
        assert_eq!(execute_ai_turn(&mut world, PlayerId(1)), None);
    }
}
