//! 占領役への物件責務と到達目標。

use super::{
    CampaignResponsibility, Entity, GridPosition, HashSet, Map, MasterDataRegistry, MissionType,
    PlayerId, Property, SquadManager, Terrain, TerrainConnectivity, UnitStats, World,
    campaign_member_distance_to_position,
};
pub(super) fn nearest_campaign_property_target_with_connectivity(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    members: &[Entity],
    excluded_targets: &HashSet<GridPosition>,
    connectivity: &mut TerrainConnectivity,
) -> Option<GridPosition> {
    let island_map = world.get_resource::<crate::ai::islands::IslandMap>()?;
    let map = world.get_resource::<Map>()?;
    let registry = world.get_resource::<MasterDataRegistry>()?;
    world
        .iter_entities()
        .filter_map(|entity| {
            let target = entity.get::<GridPosition>().copied()?;
            let property = entity.get::<Property>()?;
            if property.owner_id == Some(player_id)
                || excluded_targets.contains(&target)
                || island_map
                    .get_island_at(&target)
                    .is_none_or(|island| island.id != island_id)
            {
                return None;
            }
            let distance = members
                .iter()
                .filter_map(|member| {
                    let position = world.get::<GridPosition>(*member)?;
                    let stats = world.get::<UnitStats>(*member)?;
                    connectivity
                        .is_reachable(
                            map,
                            registry,
                            (position.x, position.y),
                            (target.x, target.y),
                            stats.movement_type,
                        )
                        .then_some(position.x.abs_diff(target.x) + position.y.abs_diff(target.y))
                })
                .min()?;
            Some((distance, target.y, target.x, target))
        })
        .min_by_key(|candidate| (candidate.0, candidate.1, candidate.2))
        .map(|candidate| candidate.3)
}

pub(super) fn campaign_assignment_capture_responsibilities(
    world: &World,
    manager: &SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    members: &[Entity],
    connectivity: &mut TerrainConnectivity,
) -> Vec<CampaignResponsibility> {
    let mut targets: Vec<_> = assignment
        .capture_target_positions
        .iter()
        .copied()
        .filter(|target| {
            world.iter_entities().any(|entity| {
                entity.get::<GridPosition>() == Some(target)
                    && entity
                        .get::<Property>()
                        .is_some_and(|property| property.owner_id != Some(player_id))
            })
        })
        .collect();
    if crate::ai::strategy_profile::current_profile(world, player_id)
        == crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion
    {
        if let Some(map) = world.get_resource::<Map>() {
            let properties = world
                .iter_entities()
                .filter_map(|p| {
                    Some((
                        *p.get::<GridPosition>()?,
                        p.get::<Property>()?.owner_id,
                        p.get::<Property>()?.terrain,
                    ))
                })
                .collect::<Vec<_>>();
            let my_capital = properties.iter().find_map(|(pos, owner, terrain)| {
                (*owner == Some(player_id) && *terrain == Terrain::Capital).then_some(*pos)
            });
            let enemy_capital = properties.iter().find_map(|(pos, owner, terrain)| {
                (*owner != Some(player_id) && owner.is_some() && *terrain == Terrain::Capital)
                    .then_some(*pos)
            });
            if let Some(my_cap) = my_capital {
                targets.sort_unstable_by_key(|t| {
                    let d_my = map.distance(my_cap.x, my_cap.y, t.x, t.y);
                    let d_enemy = enemy_capital
                        .map(|ec| map.distance(ec.x, ec.y, t.x, t.y))
                        .unwrap_or(0);
                    (std::cmp::Reverse(d_my), d_enemy, t.y, t.x)
                });
            }
        }
    }
    if targets.is_empty()
        && let Some(target) = nearest_campaign_property_target_with_connectivity(
            world,
            player_id,
            assignment.island_id,
            members,
            &HashSet::default(),
            connectivity,
        )
    {
        targets.push(target);
    }

    let mut remaining = members.to_vec();
    remaining.sort_by_key(|entity| entity.to_bits());
    let mut responsibilities: Vec<CampaignResponsibility> = Vec::new();

    // 生産時に期限・経路まで検証して具体化したCapture任務は、次回Campaign分析の
    // 「最寄り物件」へ即座に戻さない。対象が未取得かつ本人が到達可能な間だけ保持し、
    // 取得済み・到達不能なら下の通常割当へ自然に戻す。
    let mut pinned_members = HashSet::default();
    // 未所有物件マス上にすでに乗っている歩兵のうち、すでに占領進行中であるか、
    // または既存Squadの目標としてその物件に到達している歩兵は、
    // 他の物件へ移籍させず、その物件の占領完遂を最優先としてピン留めする。
    for member in &remaining {
        let Some(member_pos) = world.get::<GridPosition>(*member).copied() else {
            continue;
        };
        let is_capturing_or_committed = world.iter_entities().any(|entity| {
            entity.get::<GridPosition>() == Some(&member_pos)
                && entity.get::<Property>().is_some_and(|property| {
                    property.owner_id != Some(player_id)
                        && (property.capture_points < property.max_capture_points
                            || manager.proposal_view().squads.iter().any(|squad| {
                                squad.plan.owner_id == Some(player_id)
                                    && squad.plan.mission_type == MissionType::Capture
                                    && squad.plan.target == Some(member_pos)
                                    && squad.members.contains(member)
                            }))
                })
        });
        if is_capturing_or_committed {
            pinned_members.insert(*member);
            if let Some(responsibility) = responsibilities
                .iter_mut()
                .find(|responsibility| responsibility.target == member_pos)
            {
                responsibility.members.push(*member);
            } else {
                responsibilities.push(CampaignResponsibility {
                    mission_type: MissionType::Capture,
                    target: member_pos,
                    members: vec![*member],
                });
            }
        }
    }

    for member in &remaining {
        if pinned_members.contains(member) {
            continue;
        }
        let pinned_target = manager
            .proposal_view()
            .squads
            .iter()
            .filter(|squad| {
                squad.plan.owner_id == Some(player_id)
                    && squad.plan.mission_type == MissionType::Capture
                    && squad.plan.target_island == Some(assignment.island_id)
                    && squad.members.contains(member)
            })
            .filter_map(|squad| squad.plan.target)
            .find(|target| {
                world.iter_entities().any(|entity| {
                    entity.get::<GridPosition>() == Some(target)
                        && entity
                            .get::<Property>()
                            .is_some_and(|property| property.owner_id != Some(player_id))
                }) && campaign_member_distance_to_position(world, *member, *target, connectivity)
                    .is_some()
            });
        let Some(target) = pinned_target else {
            continue;
        };
        let is_small_expansion = crate::ai::strategy_profile::current_profile(world, player_id)
            == crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion;
        let member_pos = world.get::<GridPosition>(*member).copied();
        if is_small_expansion && member_pos != Some(target) {
            // SmallMapExpansion では、目標物件に未到達の歩兵はピン留めせず、
            // 奥の目標から順に最寄り歩兵を割り当てる全体最適マッチングに委ねる。
            continue;
        }
        pinned_members.insert(*member);
        if let Some(responsibility) = responsibilities
            .iter_mut()
            .find(|responsibility| responsibility.target == target)
        {
            responsibility.members.push(*member);
        } else {
            responsibilities.push(CampaignResponsibility {
                mission_type: MissionType::Capture,
                target,
                members: vec![*member],
            });
        }
    }
    remaining.retain(|member| !pinned_members.contains(member));
    let pinned_targets = responsibilities
        .iter()
        .map(|responsibility| responsibility.target)
        .collect::<HashSet<_>>();
    targets.retain(|target| !pinned_targets.contains(target));

    for target in targets {
        let Some((index, _)) = remaining
            .iter()
            .enumerate()
            .filter_map(|(index, member)| {
                campaign_member_distance_to_position(world, *member, target, connectivity)
                    .map(|distance| (index, (distance, member.to_bits())))
            })
            .min_by_key(|(_, score)| *score)
        else {
            continue;
        };
        let member = remaining.remove(index);
        responsibilities.push(CampaignResponsibility {
            mission_type: MissionType::Capture,
            target,
            members: vec![member],
        });
        if remaining.is_empty() {
            break;
        }
    }

    // 同一陸地の首都戦役では余剰占領兵を別施設へ展開する。海洋戦では上陸点へ
    // 戦力を集中する必要があるため、従来どおり同じ目標への増援を許す。
    let spread_over_land_routes =
        crate::ai::v4::capital_route::topology::is_same_land_capital_island(
            world,
            player_id,
            assignment.island_id,
        );
    let mut claimed_targets = responsibilities
        .iter()
        .map(|responsibility| responsibility.target)
        .collect::<HashSet<_>>();
    for member in remaining {
        let excluded_targets = if spread_over_land_routes {
            &claimed_targets
        } else {
            &HashSet::default()
        };
        if let Some(target) = nearest_campaign_property_target_with_connectivity(
            world,
            player_id,
            assignment.island_id,
            &[member],
            excluded_targets,
            connectivity,
        ) {
            claimed_targets.insert(target);
            responsibilities.push(CampaignResponsibility {
                mission_type: MissionType::Capture,
                target,
                members: vec![member],
            });
        } else if let Some(responsibility) =
            responsibilities.iter_mut().min_by_key(|responsibility| {
                (
                    campaign_member_distance_to_position(
                        world,
                        member,
                        responsibility.target,
                        connectivity,
                    )
                    .unwrap_or(usize::MAX),
                    responsibility.target.y,
                    responsibility.target.x,
                )
            })
        {
            // 未担当施設が尽きてもEntityを無所属へ落とさず、最寄りの既存責務へ増員する。
            responsibility.members.push(member);
        }
    }
    responsibilities
}
