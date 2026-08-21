//! V100/V200専用AI。
//!
//! 既存V1〜V4の部隊計画・評価・生産器へ依存せず、逐次的に命令を決定する。

pub(crate) mod action;
pub(crate) mod candidate_field;
pub(crate) mod compatibility_ai;
/// V100/V200の能力ベース対応規則。
pub(crate) mod compatibility_profile;
pub(crate) mod production;
pub(crate) mod route_field;
pub(crate) mod transport;
pub(crate) mod unit_record;

/// V100/V200の手番を1ステップ実行する。
pub(crate) fn execute_turn(
    world: &mut bevy_ecs::prelude::World,
    player_id: crate::components::PlayerId,
) -> Option<String> {
    compatibility_ai::execute_turn(world, player_id)
}
