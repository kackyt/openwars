//! V4島嶼キャンペーン所属ユニットの戦術制約と目標解決。
use crate::components::{GridPosition, PlayerId};
use bevy_ecs::prelude::*;

#[derive(Debug, Clone)]
pub(crate) struct CampaignActionContext {
    pub(crate) owner: crate::ai::operation_assignment::OperationOwner,
    pub(crate) island_id: crate::ai::islands::IslandId,
    pub(crate) mission_type: crate::ai::squad::MissionType,
    pub(crate) target: Option<GridPosition>,
    /// 局地Captureの最小護衛が合流可能か。falseでも現在位置での反撃は許可する。
    pub(crate) departure_authorized: bool,
}

/// V4のEntityは正本registryが指す具体Squadだけから行動方針を受け取る。
/// Squad全件を行動候補ごとに走査せず、Entity→割当を平均O(1)で引いた後に
/// concrete SquadIdを一度だけ解決する。
pub(crate) fn campaign_action_context(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    manager: &crate::ai::squad::SquadManager,
) -> Result<Option<CampaignActionContext>, ()> {
    if crate::ai::resolve_player_ai_version(world, player_id)
        != crate::ai::ai_version::AiVersion::V4
    {
        return Ok(None);
    }
    let Some(assignment) = world
        .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
        .and_then(|registry| registry.assignment(entity))
    else {
        return Ok(None);
    };
    let crate::ai::operation_assignment::OperationOwner::Campaign { island_id, .. } =
        assignment.owner
    else {
        return Ok(None);
    };
    let Some(squad_id) = assignment.squad_id else {
        return Err(());
    };
    let Some(squad) = manager.squads.iter().find(|squad| squad.id == squad_id) else {
        return Err(());
    };
    // 首都攻略DAGの外側では地域への入口へ進むが、地域内ではControl/Captureの
    // 指令に従う。`Some(None)`はanchor直行を止めて局地戦術へ任せる明示値であり、
    // その場合に古いSquad目標へフォールバックしてはならない。
    let target = if crate::ai::code_path::is_extended_at("engine.rs::campaign_action_context") {
        crate::ai::v4::capital_route_tactical_target(world, player_id, entity)
            .unwrap_or(squad.target)
    } else {
        squad.target
    };
    Ok(Some(CampaignActionContext {
        owner: assignment.owner,
        island_id,
        mission_type: squad.mission_type.clone(),
        target,
        departure_authorized: squad.departure_authorized,
    }))
}

pub(crate) fn campaign_position_is_on_target_island(
    world: &World,
    context: &CampaignActionContext,
    position: GridPosition,
) -> bool {
    world
        .get_resource::<crate::ai::islands::IslandMap>()
        .and_then(|islands| islands.get_island_at(&position))
        .is_some_and(|island| island.id == context.island_id)
}

/// 同じcampaignに属する占領可能unitが施設取得へ役割を切り替えてよい任務。
/// Attack歩兵も敵を押し退けた後は同一作戦島を占領し、ZOC優位を収入へ接続する。
pub(crate) fn campaign_mission_allows_capture(mission: &crate::ai::squad::MissionType) -> bool {
    matches!(
        mission,
        crate::ai::squad::MissionType::Capture | crate::ai::squad::MissionType::Attack
    )
}
