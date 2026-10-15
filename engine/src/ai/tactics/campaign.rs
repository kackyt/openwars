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
            .unwrap_or(squad.plan.target)
    } else {
        squad.plan.target
    };
    Ok(Some(CampaignActionContext {
        owner: assignment.owner,
        island_id,
        mission_type: squad.plan.mission_type.clone(),
        target,
        departure_authorized: squad.plan.departure_authorized,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ai_version::{AiVersion, PlayerAiSettings};
    use crate::ai::islands::IslandId;
    use crate::ai::operation_assignment::{
        OperationOwner, OperationUnitRole, UnitOperationAssignment, UnitOperationRegistry,
    };
    use crate::ai::planning::assignment::LedgerKey;
    use crate::ai::squad::{MissionType, SquadManager};

    /// S1: 作戦ownerだけを残して具体Squadを失ったEntityは、履歴のSquadIdがまだ一覧に
    /// 残っていても、そのSquadの任務・目標を受け取らない（Errで再接続待ちになる）。
    /// 現在bindingを持つEntityは従来どおりSquadの任務・目標を受け取る。
    /// small_mapの行動選択はこのErrで、DAG指令が無ければ候補を出さない（tactics/small_map.rs）。
    #[test]
    fn detached_campaign_entity_does_not_read_history_squad() {
        let player = PlayerId(1);
        let owner = OperationOwner::Campaign {
            player_id: player,
            island_id: IslandId(0),
        };
        let [bound, detached] = [Entity::from_raw(1), Entity::from_raw(2)];
        let mut manager = SquadManager::default();
        let mut squad = manager.create_owned_squad(MissionType::Capture, player);
        squad.plan.target = Some(GridPosition { x: 3, y: 4 });
        squad.members.insert(bound);
        let squad_id = squad.id;
        let mut registry = UnitOperationRegistry::default();
        for entity in [bound, detached] {
            registry.assign(
                entity,
                UnitOperationAssignment {
                    owner,
                    squad_id: Some(squad_id),
                    role: OperationUnitRole::Member,
                    assigned_turn: 1,
                },
            );
        }
        registry.apply_detach_squad(LedgerKey::fixture(), detached);
        let mut settings = PlayerAiSettings::new();
        settings.set_version(player, AiVersion::V4);
        let mut world = World::new();
        world.insert_resource(settings);
        world.insert_resource(registry);

        let context = campaign_action_context(&world, player, bound, &manager)
            .expect("現在bindingのSquadを解決する")
            .expect("V4のCampaign所属");
        assert_eq!(context.mission_type, MissionType::Capture);
        assert!(campaign_action_context(&world, player, detached, &manager).is_err());
        // DAG指令もSquadIdで引くため、現在bindingの無いEntityには指令が無い。
        assert_eq!(
            crate::ai::v4::capital_route_tactical_target(&world, player, detached),
            None
        );
        assert_eq!(
            world
                .resource::<UnitOperationRegistry>()
                .previous_squad(detached),
            Some(squad_id)
        );
    }
}
