//! 同じ進軍路の攻勢を続けながら、後方の保持地点への脅威だけに守備役を残す。

use super::*;

/// 守備役の選定には既存の局地戦闘割当を使う。対応できない脅威が残る場合は全員を保持し、
/// 戦力価値の合計だけで「守れる」と見なして部隊を引き抜かない。
pub(super) fn duties(
    world: &World,
    map: &Map,
    player: PlayerId,
    island: crate::ai::islands::IslandId,
    order: &RouteOrder,
    members: &[Entity],
    connectivity: &mut crate::ai::turn_distance::TerrainConnectivity,
) -> Vec<CampaignResponsibility> {
    let guard = order.staging;
    if guard == order.key_point {
        return Vec::new();
    }
    let holds_property = world.iter_entities().any(|entity| {
        entity.get::<GridPosition>() == Some(&guard)
            && entity
                .get::<Property>()
                .is_some_and(|p| p.owner_id == Some(player))
    });
    if !holds_property
        && map.get_terrain(guard.x, guard.y) != Some(crate::resources::Terrain::Bridge)
    {
        return Vec::new();
    }
    let Some(master) = world.get_resource::<crate::resources::master_data::MasterDataRegistry>()
    else {
        return Vec::new();
    };
    let mut cache = crate::ai::turn_distance::ActionTurnDistanceCache::default();
    let mut arrival = |at: GridPosition, stats: &UnitStats, to: GridPosition| {
        crate::ai::v4::property_control::capture_arrival_turns(
            map,
            master,
            (at.x, at.y),
            (to.x, to.y),
            stats.movement_type,
            stats.max_movement,
            stats.max_fuel,
            player,
            &mut cache,
        )
    };
    let threats = world
        .iter_entities()
        .filter_map(|entity| {
            let faction = entity.get::<Faction>()?;
            let stats = entity.get::<UnitStats>()?;
            let at = *entity.get::<GridPosition>()?;
            if faction.0 == player
                || entity.get::<crate::components::Transporting>().is_some()
                || (stats.max_range == 0 && !stats.can_capture)
            {
                return None;
            }
            let to_guard = arrival(at, stats, guard)?;
            let to_front = arrival(at, stats, order.key_point);
            (to_guard <= KEY_POINT_ENEMY_TURNS && to_front.is_none_or(|eta| to_guard < eta))
                .then_some(at)
        })
        .collect::<crate::collections::HashSet<_>>();
    if threats.is_empty() {
        return Vec::new();
    }
    let allocated = super::super::objective_allocation::route_operation_responsibilities(
        world,
        player,
        island,
        members,
        guard,
        connectivity,
    );
    let covered = allocated
        .iter()
        .filter(|duty| threats.contains(&duty.target))
        .map(|duty| duty.target)
        .collect::<crate::collections::HashSet<_>>();
    let assigned = if covered == threats {
        allocated
            .iter()
            .filter(|duty| threats.contains(&duty.target))
            .flat_map(|duty| duty.members.iter().copied())
            .collect::<crate::collections::HashSet<_>>()
    } else {
        // 未対応の敵を残して守備隊を縮小しない。攻勢不成立の理由を局地に限定する。
        members.iter().copied().collect()
    };
    let members = members
        .iter()
        .copied()
        .filter(|entity| assigned.contains(entity))
        .collect::<Vec<_>>();
    if members.is_empty() {
        return Vec::new();
    }
    vec![CampaignResponsibility {
        mission_type: MissionType::Defense,
        target: guard,
        members,
    }]
}
