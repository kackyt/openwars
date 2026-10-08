//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

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
            &HashSet::new(),
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
    let mut pinned_members = HashSet::new();
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
                            || manager.squads.iter().any(|squad| {
                                squad.owner_id == Some(player_id)
                                    && squad.mission_type == MissionType::Capture
                                    && squad.target == Some(member_pos)
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
            .squads
            .iter()
            .filter(|squad| {
                squad.owner_id == Some(player_id)
                    && squad.mission_type == MissionType::Capture
                    && squad.target_island == Some(assignment.island_id)
                    && squad.members.contains(member)
            })
            .filter_map(|squad| squad.target)
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
        crate::ai::v4::is_same_land_capital_island(world, player_id, assignment.island_id);
    let mut claimed_targets = responsibilities
        .iter()
        .map(|responsibility| responsibility.target)
        .collect::<HashSet<_>>();
    for member in remaining {
        let excluded_targets = if spread_over_land_routes {
            &claimed_targets
        } else {
            &HashSet::new()
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

pub(super) fn campaign_combat_responsibilities(
    world: &World,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    members: &[Entity],
    connectivity: &mut TerrainConnectivity,
) -> Vec<CampaignResponsibility> {
    let island_id = assignment.island_id;
    let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() else {
        return Vec::new();
    };
    let enemies: Vec<_> = world
        .iter_entities()
        .filter_map(|entity| {
            let target = entity.get::<GridPosition>().copied()?;
            let faction = entity.get::<Faction>()?;
            (faction.0 != player_id
                && island_map
                    .get_island_at(&target)
                    .is_some_and(|island| island.id == island_id))
            .then_some((entity.id(), target))
        })
        .collect();

    let route_fronts = crate::ai::v4::same_land_route_fronts(world, player_id, island_id);
    if route_fronts.len() >= 2 {
        let Some(map) = world.get_resource::<Map>() else {
            return Vec::new();
        };
        let pending_bridgeheads =
            crate::ai::v4::same_land_pending_bridgeheads(world, player_id, island_id);
        let breakthrough_positions =
            crate::ai::v4::same_land_route_breakthrough_positions(world, player_id, island_id);
        let nearest_front = |position: GridPosition| {
            route_fronts
                .iter()
                .enumerate()
                .min_by_key(|(index, front)| {
                    (
                        map.distance(position.x, position.y, front.x, front.y),
                        *index,
                    )
                })
                .map_or(0, |(index, _)| index)
        };
        let route_demands =
            crate::ai::v4::same_land_route_force_demands(world, player_id, island_id);
        let route_slots = crate::ai::v4::apportion_route_force_slots(members.len(), &route_demands);
        let route_members = distribute_members_across_route_fronts(
            world,
            map,
            members,
            &route_fronts,
            &route_slots,
        );
        let mut route_enemies = vec![Vec::new(); route_fronts.len()];
        for (enemy, position) in enemies {
            route_enemies[nearest_front(position)].push((enemy, position));
        }
        let mut route_capture_targets = vec![Vec::new(); route_fronts.len()];
        for target in &assignment.capture_target_positions {
            route_capture_targets[nearest_front(*target)].push(*target);
        }
        for (route, targets) in route_capture_targets.iter_mut().enumerate() {
            targets.sort_unstable_by_key(|target| {
                (
                    map.distance(
                        target.x,
                        target.y,
                        route_fronts[route].x,
                        route_fronts[route].y,
                    ),
                    target.y,
                    target.x,
                )
            });
        }
        let mut responsibilities = Vec::new();
        for (route, route_members) in route_members.into_iter().enumerate() {
            if route_members.is_empty() {
                continue;
            }
            if let Some(bridgehead) = pending_bridgeheads.get(route).and_then(|target| *target) {
                // 橋の手前に敵や未所有施設が残っていても、橋頭堡に地上部隊が立つまでは
                // ルート任務を局地戦へ戻さない。ここで初めて「突破」を実行目標にする。
                responsibilities.push(CampaignResponsibility {
                    mission_type: MissionType::Defense,
                    target: bridgehead,
                    members: route_members,
                });
                continue;
            }
            if route_enemies[route].is_empty() {
                // 接敵前は橋そのものではなく、その先で占領兵が狙う現在Milestoneまで進む。
                // これによりGate到着を突破成功と誤認せず、橋向こうの施設を橋頭堡にする。
                responsibilities.push(CampaignResponsibility {
                    mission_type: MissionType::Defense,
                    target: route_capture_targets[route]
                        .first()
                        .copied()
                        .or_else(|| breakthrough_positions.get(route).copied())
                        .unwrap_or(route_fronts[route]),
                    members: route_members,
                });
            } else {
                responsibilities.extend(campaign_combat_responsibilities_for_enemies(
                    world,
                    &route_members,
                    std::mem::take(&mut route_enemies[route]),
                    connectivity,
                ));
            }
        }
        return responsibilities;
    }

    campaign_combat_responsibilities_for_enemies(world, members, enemies, connectivity)
}

pub(super) fn prepare_campaign_local_assignment_with_connectivity(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    connectivity: &mut TerrainConnectivity,
) {
    if !crate::ai::code_path::ext_enabled(
        "squad.rs::prepare_campaign_local_assignment_with_connectivity",
    ) {
        return super::prepare_campaign_local_assignment_with_connectivity(
            world,
            manager,
            player_id,
            assignment,
            connectivity,
        );
    }
    use crate::ai::island_campaign::IslandCampaignDecision;

    let local_entities = |entities: &[Entity]| {
        entities
            .iter()
            .copied()
            .filter(|entity| cargo_is_landed_on_assignment_island(world, assignment, *entity))
            .collect::<Vec<_>>()
    };
    match assignment.decision {
        IslandCampaignDecision::Defend => {
            // 島が脅威下でも未所有施設の占領は止めない。占領可能Entityだけを
            // Capture責務へ固定し、残りを局地敵の排除または拠点防衛へ分割する。
            let capture = local_entities(&assignment.capture_entities);
            let capture_responsibilities = campaign_assignment_capture_responsibilities(
                world,
                manager,
                player_id,
                assignment,
                &capture,
                connectivity,
            );
            let assigned_capture: HashSet<_> = capture_responsibilities
                .iter()
                .flat_map(|responsibility| responsibility.members.iter().copied())
                .collect();
            assign_campaign_responsibilities(
                world,
                manager,
                player_id,
                assignment.island_id,
                capture_responsibilities,
                &[MissionType::Capture],
            );

            let mut combat: Vec<_> = assignment
                .combat_entities
                .iter()
                .chain(assignment.capture_entities.iter())
                .copied()
                .filter(|entity| !assigned_capture.contains(entity))
                .filter(|entity| {
                    world
                        .get::<crate::components::Transporting>(*entity)
                        .is_none()
                        && (cargo_is_landed_on_assignment_island(world, assignment, *entity)
                            || entity_can_self_deploy_to_assignment_with_connectivity(
                                world,
                                assignment,
                                *entity,
                                connectivity,
                            ))
                })
                .collect();
            combat.sort_by_key(|entity| entity.to_bits());
            combat.dedup();
            let mut combat_responsibilities = campaign_combat_responsibilities(
                world,
                player_id,
                assignment,
                &combat,
                connectivity,
            );
            if !combat.is_empty()
                && combat_responsibilities
                    .iter()
                    .all(|responsibility| responsibility.mission_type == MissionType::Defense)
                && crate::ai::v4::same_land_route_fronts(world, player_id, assignment.island_id)
                    .len()
                    < 2
            {
                // 接近中でまだ島内に敵がいない場合は、防衛対象をばらけさせず
                // assignmentの主要施設へ集結させる。
                combat_responsibilities = vec![CampaignResponsibility {
                    mission_type: MissionType::Defense,
                    target: assignment.target_position,
                    members: combat,
                }];
            }
            assign_campaign_responsibilities(
                world,
                manager,
                player_id,
                assignment.island_id,
                combat_responsibilities,
                &[MissionType::Attack, MissionType::Defense],
            );
        }
        IslandCampaignDecision::Secure => {
            let capture = local_entities(&assignment.capture_entities);
            let responsibilities = campaign_assignment_capture_responsibilities(
                world,
                manager,
                player_id,
                assignment,
                &capture,
                connectivity,
            );
            assign_campaign_responsibilities(
                world,
                manager,
                player_id,
                assignment.island_id,
                responsibilities,
                &[MissionType::Capture],
            );
            let combat = local_entities(&assignment.combat_entities);
            let combat_responsibilities = campaign_combat_responsibilities(
                world,
                player_id,
                assignment,
                &combat,
                connectivity,
            );
            assign_campaign_responsibilities(
                world,
                manager,
                player_id,
                assignment.island_id,
                combat_responsibilities,
                &[MissionType::Attack, MissionType::Defense],
            );
        }
        IslandCampaignDecision::Expand
        | IslandCampaignDecision::Assault
        | IslandCampaignDecision::Contest
        | IslandCampaignDecision::Reinforce => {
            let capture = local_entities(&assignment.capture_entities);
            let capture_responsibilities = campaign_assignment_capture_responsibilities(
                world,
                manager,
                player_id,
                assignment,
                &capture,
                connectivity,
            );
            assign_campaign_responsibilities(
                world,
                manager,
                player_id,
                assignment.island_id,
                capture_responsibilities,
                &[MissionType::Capture],
            );
            // 航空戦力など作戦島へ自力展開できる戦力は、島外にいても輸送cargoへ
            // 入れず、直接Attack/Defense責務を与える。
            let combat = assignment
                .combat_entities
                .iter()
                .copied()
                .filter(|entity| {
                    cargo_is_landed_on_assignment_island(world, assignment, *entity)
                        || entity_can_self_deploy_to_assignment_with_connectivity(
                            world,
                            assignment,
                            *entity,
                            connectivity,
                        )
                })
                .collect::<Vec<_>>();
            let responsibilities = campaign_combat_responsibilities(
                world,
                player_id,
                assignment,
                &combat,
                connectivity,
            );
            assign_campaign_responsibilities(
                world,
                manager,
                player_id,
                assignment.island_id,
                responsibilities,
                &[MissionType::Attack, MissionType::Defense],
            );
        }
        _ => {}
    }
    synchronize_local_capture_departure(world, manager, player_id, assignment);
}

/// ゲームの戦略状況に基づいて、自動的に部隊の構築と新規メンバーの割り当てを行います。
pub(crate) fn plan_squads(world: &mut World, perspective_player: PlayerId) {
    if !crate::ai::code_path::ext_enabled("squad.rs::plan_squads") {
        return super::plan_squads(world, perspective_player);
    }
    // 1. まず既存部隊のクリーンアップと SoloFallback 判定を実行
    update_squads(world, perspective_player);

    // V3 の戦略拡張 (#53: 敵拠点の奪取目標化) を有効にするかどうか
    let is_v3 = crate::ai::resolve_player_ai_version(world, perspective_player).uses_v3_tactics();
    let is_v4 = crate::ai::resolve_player_ai_version(world, perspective_player)
        .uses_operation_driven_production();
    if is_v4 {
        crate::ai::v4::strategy_pipeline::before_squad_planning(world, perspective_player);
    }
    let mut manager = world.remove_resource::<SquadManager>().unwrap_or_default();
    prepare_squads_for_replanning(world, &mut manager, perspective_player, is_v4);
    let tactical_reserved_entities = HashSet::new();
    let strategy = if is_v3 {
        // 防衛もcampaign portfolioの通常優先度・排他割当で扱う。
        let deployment_entities = if is_v4 {
            world
                .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
                .map(|deployments| deployments.active_entities(perspective_player))
                .unwrap_or_default()
        } else {
            HashSet::new()
        };

        // deploymentだけは生産目的が解消されるまで通常free poolから保護する。
        world.insert_resource(manager);
        let strategy = analyze_strategy_with_reserved_entities(
            world,
            perspective_player,
            &deployment_entities,
        );
        manager = world.remove_resource::<SquadManager>().unwrap_or_default();
        strategy
    } else {
        world.insert_resource(manager);
        let strategy = analyze_strategy(world, perspective_player);
        manager = world.remove_resource::<SquadManager>().unwrap_or_default();
        strategy
    };
    let roadmap_turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let roadmap_portfolio = if is_v4 {
        crate::ai::v4::strategy_pipeline::portfolio_for_squads(
            world,
            perspective_player,
            roadmap_turn,
            strategy.campaign_portfolio.clone(),
            &manager,
        )
    } else {
        strategy.campaign_portfolio.clone()
    };
    let enemy_clusters = detect_enemy_clusters(world, perspective_player);
    if is_v3 {
        let mut cache = world
            .remove_resource::<crate::ai::engine::AiTurnStrategyCache>()
            .unwrap_or_default();
        cache.set_campaign_portfolio(perspective_player, roadmap_portfolio.clone());
        cache.mark_squads_planned(perspective_player);
        world.insert_resource(cache);
        claim_campaign_portfolio_assignments(world, perspective_player, &roadmap_portfolio);
    }
    let paused_campaign_islands = if is_v3 {
        campaign_paused_islands(&roadmap_portfolio)
    } else {
        HashSet::new()
    };
    let secure_reserved_entities = if is_v3 {
        apply_campaign_pauses(
            world,
            &mut manager,
            perspective_player,
            &paused_campaign_islands,
        );
        prepare_secure_local_captures(world, &mut manager, perspective_player, &roadmap_portfolio)
    } else {
        HashSet::new()
    };
    let mut campaign_assignments: Vec<_> = if is_v3 {
        roadmap_portfolio
            .defenses
            .iter()
            .chain(roadmap_portfolio.active_offensives.iter())
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    campaign_assignments.sort_by_key(campaign_assignment_priority);
    let active_campaign_islands: HashSet<_> = campaign_assignments
        .iter()
        .map(|assignment| assignment.island_id)
        .collect();
    remove_abandoned_campaign_placeholders(
        world,
        &mut manager,
        perspective_player,
        &active_campaign_islands,
    );
    let mut all_campaign_reserved_entities: HashSet<_> = campaign_assignments
        .iter()
        .flat_map(|assignment| {
            assignment
                .transport_entities
                .iter()
                .chain(assignment.capture_entities.iter())
                .chain(assignment.combat_entities.iter())
        })
        .copied()
        .collect();
    all_campaign_reserved_entities.extend(secure_reserved_entities);
    let mut blocked_campaign_islands = HashSet::new();
    let mut campaign_connectivity = TerrainConnectivity::default();
    for assignment in &campaign_assignments {
        if prepare_campaign_transport_assignment_with_connectivity(
            world,
            &mut manager,
            perspective_player,
            assignment,
            &mut campaign_connectivity,
        ) {
            blocked_campaign_islands.insert(assignment.island_id);
        }
        prepare_campaign_local_assignment_with_connectivity(
            world,
            &mut manager,
            perspective_player,
            assignment,
            &mut campaign_connectivity,
        );
    }

    // V4のCombat枠で生産した実Entityは、campaign再配分とgeneric free poolより前に
    // 要求元の局地Attack任務へ予約する。
    // 既存campaign assignmentへ残っていても、明示的な生産目的を優先して切り離す。
    if is_v4 {
        let deployment_reserved = crate::ai::v4::deployment::prepare_deployment_squads(
            world,
            &mut manager,
            perspective_player,
            &tactical_reserved_entities,
        );
        all_campaign_reserved_entities.extend(deployment_reserved);
    }

    // #53 (V3): メンバーが全滅した占領部隊を解散する。
    // 残しておくと「そのターゲットには部隊が存在する」と誤判定され続け
    // (dedupe に引っかかる)、失敗した奪取目標へ二度と部隊が送られなくなる
    if is_v3 {
        manager
            .squads
            .retain(|s| s.mission_type != MissionType::Capture || !s.members.is_empty());
    }

    let map = world.resource::<Map>().clone();
    let registry = world
        .get_resource::<MasterDataRegistry>()
        .cloned()
        .unwrap_or_default();
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
        .unwrap_or_else(|| crate::ai::islands::IslandMap::analyze(&map));

    // 他の部隊にすでに割り当て済みの全メンバーを特定
    let mut busy_entities = HashSet::new();
    for squad in &manager.squads {
        for &member in &squad.members {
            busy_entities.insert(member);
        }
        busy_entities.extend(squad.cargo_entities.iter().copied());
        busy_entities.extend(squad.delivered_cargo.iter().copied());
    }

    // 占有情報（経路探索用）
    let mut unit_positions = HashMap::new();
    let mut q_all_units = world.query::<(
        &Faction,
        &GridPosition,
        &UnitStats,
        Option<&crate::components::Transporting>,
        Option<&crate::components::CargoCapacity>,
    )>();
    for (faction, pos, stats, transporting, cargo) in q_all_units.iter(world) {
        if transporting.is_some() {
            continue;
        }
        unit_positions.insert(
            (pos.x, pos.y),
            crate::systems::movement::OccupantInfo {
                player_id: faction.0,
                is_transport: stats.max_cargo > 0,
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots: cargo
                    .map(|capacity| capacity.max.saturating_sub(capacity.loaded.len() as u32))
                    .unwrap_or(0),
            },
        );
    }

    let mut turn_cache = TurnDistanceCache::default();
    // マップ全体のフラッドフィルを毎回やり直さないよう、地形連結判定を使い回す。
    let mut connectivity = TerrainConnectivity::default();

    // フリーの自軍ユニットを収集
    let mut free_combat_units = Vec::new();
    let mut free_infantry = Vec::new();
    let mut free_transports = Vec::new();

    let mut q_my_units = world.query::<(
        Entity,
        &Faction,
        &GridPosition,
        &UnitStats,
        Option<&crate::components::Transporting>,
        Option<&crate::components::Fuel>,
        Option<&crate::components::CargoCapacity>,
    )>();
    for (entity, faction, pos, stats, transporting, fuel, cargo) in q_my_units.iter(world) {
        if faction.0 == perspective_player
            && !busy_entities.contains(&entity)
            && !all_campaign_reserved_entities.contains(&entity)
            && !manager.solo_fallbacks.contains(&entity)
            && transporting.is_none()
            && fuel.is_none_or(|fuel| fuel.current > 0)
        {
            let is_transport = stats.unit_type == UnitType::TransportHelicopter
                || stats.unit_type == UnitType::Lander;
            let is_infantry =
                stats.unit_type == UnitType::Infantry || stats.unit_type == UnitType::Mech;

            if is_transport {
                if cargo.is_some_and(|capacity| capacity.max > 0) {
                    free_transports.push((entity, *pos, stats.clone()));
                }
            } else if is_infantry {
                free_infantry.push((entity, *pos, stats.clone()));
            } else {
                free_combat_units.push((entity, *pos, stats.clone()));
            }
        }
    }

    // A. 輸送部隊の割り当て（planner.rs からの統合）
    let mut base_islands = HashSet::new();
    let mut target_islands = Vec::new();
    let mut properties_ownership = HashMap::new();
    {
        let mut q_props = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in q_props.iter(world) {
            properties_ownership.insert(*pos, prop.owner_id);
        }
    }
    let (b_islands, t_islands) =
        island_map.classify_islands(perspective_player, &properties_ownership);
    base_islands.extend(b_islands.iter().copied());
    target_islands.extend(t_islands.iter().copied());

    // ---------------------------------------------------------
    // V1からの統合: 島ごとの期待値（IslandScore）の算出
    // ---------------------------------------------------------
    let mut own_production_bases = Vec::new();
    let mut enemy_production_count_map = std::collections::HashMap::new();
    let mut enemy_owned_islands = HashSet::new();
    let mut island_props_map = std::collections::HashMap::new();

    for (pos, owner) in &properties_ownership {
        if let Some(island) = island_map.get_island_at(pos) {
            island_props_map
                .entry(island.id)
                .or_insert_with(Vec::new)
                .push(*pos);

            let terrain = map
                .get_terrain(pos.x, pos.y)
                .unwrap_or(crate::resources::Terrain::City);
            if *owner == Some(perspective_player) {
                if terrain == crate::resources::Terrain::Factory
                    || terrain == crate::resources::Terrain::Capital
                {
                    own_production_bases.push(*pos);
                }
            } else if owner.is_some() {
                enemy_owned_islands.insert(island.id);
                if terrain == crate::resources::Terrain::Factory
                    || terrain == crate::resources::Terrain::Capital
                {
                    *enemy_production_count_map.entry(island.id).or_insert(0) += 1;
                }
            }
        }
    }

    let mut objectives = Vec::new();
    for &target_id in &target_islands {
        let mut min_distance = i32::MAX;
        let mut props_with_terrain = Vec::new();

        if let Some(positions) = island_props_map.get(&target_id) {
            for tile in positions {
                if properties_ownership.get(tile) == Some(&Some(perspective_player)) {
                    continue; // 自分の拠点はスキップ
                }
                props_with_terrain.push((
                    *tile,
                    map.get_terrain(tile.x, tile.y)
                        .unwrap_or(crate::resources::Terrain::City),
                ));

                let mut local_min_dist = i32::MAX;
                let mut nearest_base_pos = None;
                for base_pos in &own_production_bases {
                    let dist = (tile.x as i32 - base_pos.x as i32).abs()
                        + (tile.y as i32 - base_pos.y as i32).abs();
                    if dist < local_min_dist {
                        local_min_dist = dist;
                        nearest_base_pos = Some(*base_pos);
                    }
                }

                // 徒歩圏内（6マス以下かつ同じ島）は除外
                if nearest_base_pos
                    .and_then(|p| island_map.get_island_at(&p))
                    .filter(|base_island| base_island.id == target_id && local_min_dist <= 6)
                    .is_some()
                {
                    continue;
                }

                if local_min_dist < min_distance {
                    min_distance = local_min_dist;
                }
            }
        }

        let distance_to_nearest_base = if min_distance == i32::MAX {
            10
        } else {
            min_distance
        };
        let enemy_prod = *enemy_production_count_map.get(&target_id).unwrap_or(&0);

        let objective = crate::ai::objectives::Objective::evaluate(
            target_id,
            &props_with_terrain,
            distance_to_nearest_base,
            enemy_prod,
            &registry,
        );
        objectives.push(objective);
    }

    if is_v3 {
        let assigned_islands: HashSet<_> = campaign_assignments
            .iter()
            .map(|assignment| assignment.island_id)
            .collect();
        // portfolio assignmentは上の島別bridgeだけが所有し、旧経路は未予約の島内目標に限定する。
        objectives.retain(|objective| {
            !assigned_islands.contains(&objective.target_island)
                && !blocked_campaign_islands.contains(&objective.target_island)
                && !paused_campaign_islands.contains(&objective.target_island)
                && base_islands.contains(&objective.target_island)
                && !enemy_owned_islands.contains(&objective.target_island)
        });
    }

    // スコア降順ソート（海を渡る必要がある別島を最優先）。
    // 同点時は島IDで決定し、HashSet や ECS の列挙順へ依存しない。
    objectives.sort_by_key(|objective| {
        let is_same_island = base_islands.contains(&objective.target_island);
        let group = if is_same_island { 1u8 } else { 0u8 }; // 別島=0(高優先), 同島=1(低優先)
        (
            group,
            std::cmp::Reverse(objective.priority_score),
            objective.target_island.0,
        )
    });

    // #54 (V3): 自軍の歩兵 (占領要員) が存在する島の集合。
    // 輸送カーゴの選定で「占領要員が未着の島へはまず歩兵を送る」判定に使う
    let mut my_infantry_islands = HashSet::new();
    if is_v3 {
        for ((x, y), occ) in &unit_positions {
            if occ.player_id == perspective_player
                && matches!(occ.unit_type, UnitType::Infantry | UnitType::Mech)
            {
                let pos = GridPosition { x: *x, y: *y };
                if let Some(island) = island_map.get_island_at(&pos) {
                    my_infantry_islands.insert(island.id);
                }
            }
        }
    }

    // 輸送役の選択順を座標と Entity ID で固定する。
    // 逆順にソートし、後続のループで pop() (O(1)) から取り出せるようにする。
    free_transports
        .sort_by_key(|(entity, pos, _)| std::cmp::Reverse((pos.y, pos.x, entity.to_bits())));

    // 優先順位の高い島から輸送機を割り当てる
    for objective in objectives.iter() {
        if free_transports.is_empty() {
            break;
        }

        let mut to_assign = objective.needed_infantry.0;
        let target_position = strategy
            .campaign_portfolio
            .assignment_for(objective.target_island)
            .map(|assignment| assignment.target_position)
            .or_else(|| {
                island_props_map
                    .get(&objective.target_island)
                    .and_then(|positions| {
                        let mut positions = positions.clone();
                        positions.sort_by_key(|position| (position.y, position.x));
                        positions.into_iter().find(|position| {
                            properties_ownership.get(position) != Some(&Some(perspective_player))
                        })
                    })
            });
        let mut enemy_types = Vec::new();
        let mut enemy_query = world.query::<(&GridPosition, &Faction, &UnitStats)>();
        for (position, faction, stats) in enemy_query.iter(world) {
            if faction.0 != perspective_player
                && island_map
                    .get_island_at(position)
                    .is_some_and(|island| island.id == objective.target_island)
            {
                enemy_types.push(stats.unit_type);
            }
        }
        enemy_types.sort_by_key(|unit_type| *unit_type as u8);
        enemy_types.dedup();
        let damage_chart = world
            .get_resource::<crate::resources::DamageChart>()
            .cloned();

        while to_assign > 0 && !free_transports.is_empty() {
            let (transport_entity, transport_position, transport_stats) =
                free_transports.pop().unwrap();
            let capacity = world
                .get::<crate::components::CargoCapacity>(transport_entity)
                .map(|cargo| cargo.max as usize)
                .unwrap_or(0);
            if capacity == 0 {
                continue;
            }

            let mut cargo_entities = world
                .get::<crate::components::CargoCapacity>(transport_entity)
                .map(|cargo| cargo.loaded.clone())
                .unwrap_or_default();
            cargo_entities.sort_by_key(|entity| entity.to_bits());
            cargo_entities.dedup();
            cargo_entities.truncate(capacity);
            let initially_loaded_count = cargo_entities.len();
            let mut assigned_capture_count = cargo_entities
                .iter()
                .filter(|entity| {
                    world
                        .get::<UnitStats>(**entity)
                        .is_some_and(|stats| stats.can_capture)
                })
                .count();
            let mut selected_entries = Vec::new();

            if cargo_entities.len() < capacity {
                // V3 は不足している占領要員を先に補い、残り容量へ有効な戦闘要員を積む。
                // V2 は従来どおり戦闘要員を先に検討する。
                let search_order = if is_v3 && assigned_capture_count == 0 {
                    [false, true]
                } else {
                    [true, false]
                };

                for search_combat in search_order {
                    if cargo_entities.len() >= capacity {
                        break;
                    }
                    let candidates = if search_combat {
                        &free_combat_units
                    } else {
                        &free_infantry
                    };
                    let Some(index) = select_nearest_compatible_cargo(
                        candidates,
                        transport_position,
                        &transport_stats,
                        objective.target_island,
                        target_position,
                        &island_map,
                        &map,
                        &registry,
                        &unit_positions,
                        perspective_player,
                        &mut turn_cache,
                        &mut connectivity,
                        &enemy_types,
                        damage_chart.as_ref(),
                        search_combat,
                    ) else {
                        continue;
                    };
                    let entry = if search_combat {
                        free_combat_units.swap_remove(index)
                    } else {
                        free_infantry.swap_remove(index)
                    };
                    if entry.2.can_capture {
                        assigned_capture_count += 1;
                    }
                    cargo_entities.push(entry.0);
                    selected_entries.push((search_combat, entry));

                    // V2 は単一カーゴ運用を維持し、V3 のみ残り容量へ護衛を追加する。
                    if !is_v3 {
                        break;
                    }
                }

                while is_v3 && cargo_entities.len() < capacity {
                    let Some(index) = select_nearest_compatible_cargo(
                        &free_infantry,
                        transport_position,
                        &transport_stats,
                        objective.target_island,
                        target_position,
                        &island_map,
                        &map,
                        &registry,
                        &unit_positions,
                        perspective_player,
                        &mut turn_cache,
                        &mut connectivity,
                        &enemy_types,
                        damage_chart.as_ref(),
                        false,
                    ) else {
                        break;
                    };
                    let entry = free_infantry.swap_remove(index);
                    if entry.2.can_capture {
                        assigned_capture_count += 1;
                    }
                    cargo_entities.push(entry.0);
                    selected_entries.push((false, entry));
                }
            }

            if cargo_entities.is_empty() {
                continue;
            }
            let requires_pickup = cargo_entities.len() > initially_loaded_count;
            let mut pickup_position = if requires_pickup {
                select_pickup_position(
                    world,
                    perspective_player,
                    transport_position,
                    &transport_stats,
                    &cargo_entities[initially_loaded_count..],
                    &mut connectivity,
                )
            } else {
                Some(transport_position)
            };
            if pickup_position.is_none() {
                // 不成立の組み合わせは free pool へ戻し、搭載済みカーゴだけで継続する。
                for (is_combat, entry) in selected_entries.drain(..) {
                    if entry.2.can_capture {
                        assigned_capture_count = assigned_capture_count.saturating_sub(1);
                    }
                    if is_combat {
                        free_combat_units.push(entry);
                    } else {
                        free_infantry.push(entry);
                    }
                }
                cargo_entities.truncate(initially_loaded_count);
                if cargo_entities.is_empty() {
                    continue;
                }
                pickup_position = Some(transport_position);
            }

            let squad = manager.create_owned_squad(MissionType::Transport, perspective_player);
            squad.members.insert(transport_entity);
            squad.transport_entity = Some(transport_entity);
            squad.cargo_entities = cargo_entities;
            squad.target_island = Some(objective.target_island);
            squad.target = target_position;
            squad.pickup_position = pickup_position;
            squad.allow_partial_departure = true;
            squad.phase =
                MissionPhase::Transport(if requires_pickup && !selected_entries.is_empty() {
                    TransportPhase::Pickup
                } else {
                    TransportPhase::Transit
                });

            to_assign = to_assign.saturating_sub(assigned_capture_count);
        }
    }

    free_transports.sort_by_key(|(entity, position, _)| (position.y, position.x, entity.to_bits()));

    // campaignへ予約されていない搭載済み輸送機は、別assignmentへ転用せず安全に降ろす。
    let mut i = 0;
    while i < free_transports.len() {
        let (transport_entity, transport_position, _) = free_transports[i].clone();
        let mut cargo_entities = world
            .get::<crate::components::CargoCapacity>(transport_entity)
            .map(|cargo| cargo.loaded.clone())
            .unwrap_or_default();
        cargo_entities.sort_by_key(|entity| entity.to_bits());
        cargo_entities.dedup();

        if !cargo_entities.is_empty() {
            let squad = manager.create_owned_squad(MissionType::Transport, perspective_player);
            squad.members.insert(transport_entity);
            squad.transport_entity = Some(transport_entity);
            squad.cargo_entities = cargo_entities;
            squad.pickup_position = Some(transport_position);
            squad.target_island = None;
            squad.target = None;
            squad.phase = MissionPhase::Transport(TransportPhase::Drop);
            free_transports.remove(i);
        } else {
            i += 1;
        }
    }

    // 通常の不足数を満たした後も、未所有施設と敵の反撃余地が残る島には
    // 損耗補充用の予備便を割り当てる。従来はfree poolへ残った歩兵・輸送役を
    // 以後いっさい再照合せず、生産拠点周辺の恒常的な遊兵にしていた。
    if is_v4 && !free_transports.is_empty() && !free_infantry.is_empty() {
        let mut enemy_count_by_island = HashMap::new();
        let mut enemy_query = world.query::<(
            &Faction,
            &GridPosition,
            Option<&crate::components::Transporting>,
        )>();
        for (faction, position, transporting) in enemy_query.iter(world) {
            if faction.0 == perspective_player || transporting.is_some() {
                continue;
            }
            if let Some(island) = island_map.get_island_at(position) {
                enemy_count_by_island
                    .entry(island.id)
                    .and_modify(|count: &mut usize| *count = count.saturating_add(1))
                    .or_insert(1_usize);
            }
        }

        for assignment in &campaign_assignments {
            if free_transports.is_empty() || free_infantry.is_empty() {
                break;
            }
            if matches!(
                assignment.decision,
                crate::ai::island_campaign::IslandCampaignDecision::Observe
                    | crate::ai::island_campaign::IslandCampaignDecision::Withdraw
                    | crate::ai::island_campaign::IslandCampaignDecision::Defend
            ) {
                continue;
            }
            // 占領対象ごとの担当に加え、敵がいる島では最大2体の損耗補充を用意する。
            // 価格や固定倍率ではなく、未所有施設数と観測敵Entity数から導く。
            let desired_capture_depth = assignment
                .capture_target_positions
                .len()
                .max(1)
                .saturating_add(
                    enemy_count_by_island
                        .get(&assignment.island_id)
                        .copied()
                        .unwrap_or(0)
                        .min(2),
                );
            let mut remaining_capture_depth =
                desired_capture_depth.saturating_sub(assignment.capture_entities.len());

            while remaining_capture_depth > 0
                && !free_transports.is_empty()
                && !free_infantry.is_empty()
            {
                let transport_index = free_transports.iter().position(|(_, position, stats)| {
                    island_map
                        .islands
                        .iter()
                        .find(|island| island.id == assignment.island_id)
                        .and_then(|island| {
                            get_target_position_for_island(
                                &map,
                                &registry,
                                island,
                                *position,
                                stats.movement_type,
                            )
                        })
                        .is_some()
                });
                let Some(transport_index) = transport_index else {
                    break;
                };
                let (transport, transport_position, transport_stats) =
                    free_transports.remove(transport_index);
                let capacity = world
                    .get::<crate::components::CargoCapacity>(transport)
                    .map(|cargo| cargo.max as usize)
                    .unwrap_or(0);
                let mut compatible = free_infantry
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, _, stats))| {
                        transport_stats
                            .loadable_unit_types
                            .contains(&stats.unit_type)
                    })
                    .filter_map(|(index, (entity, _, _))| {
                        cargo_pickup_rank(
                            world,
                            perspective_player,
                            transport_position,
                            &transport_stats,
                            *entity,
                            &mut connectivity,
                        )
                        .map(|rank| (rank, index))
                    })
                    .collect::<Vec<_>>();
                compatible.sort_by_key(|(rank, _)| *rank);
                let mut selected_indices = compatible
                    .into_iter()
                    .take(capacity.min(remaining_capture_depth))
                    .map(|(_, index)| index)
                    .collect::<Vec<_>>();
                selected_indices.sort_unstable_by(|left, right| right.cmp(left));
                let mut cargo_entities = Vec::new();
                for index in selected_indices {
                    cargo_entities.push(free_infantry.swap_remove(index).0);
                }
                cargo_entities.sort_by_key(|entity| entity.to_bits());
                if cargo_entities.is_empty() {
                    free_transports.push((transport, transport_position, transport_stats));
                    break;
                }
                let Some(pickup_position) = select_pickup_position(
                    world,
                    perspective_player,
                    transport_position,
                    &transport_stats,
                    &cargo_entities,
                    &mut connectivity,
                ) else {
                    for cargo in cargo_entities {
                        if let (Some(position), Some(stats)) = (
                            world.get::<GridPosition>(cargo).copied(),
                            world.get::<UnitStats>(cargo).cloned(),
                        ) {
                            free_infantry.push((cargo, position, stats));
                        }
                    }
                    free_transports.push((transport, transport_position, transport_stats));
                    break;
                };

                remaining_capture_depth =
                    remaining_capture_depth.saturating_sub(cargo_entities.len());
                let squad = manager.create_owned_squad(MissionType::Transport, perspective_player);
                squad.members.insert(transport);
                squad.transport_entity = Some(transport);
                squad.cargo_entities = cargo_entities;
                squad.pickup_position = Some(pickup_position);
                squad.target_island = Some(assignment.island_id);
                squad.target = Some(assignment.target_position);
                squad.allow_partial_departure = assignment.decision
                    != crate::ai::island_campaign::IslandCampaignDecision::Assault;
                squad.departure_authorized = assignment.operation_ready
                    || assignment.decision
                        != crate::ai::island_campaign::IslandCampaignDecision::Assault;
                squad.phase = MissionPhase::Transport(TransportPhase::Pickup);
            }
        }
    }

    // B. 防衛部隊の立ち上げ（Defenseフェーズ）
    let mut my_capital_pos = None;
    let mut q_props = world.query::<(&GridPosition, &Property)>();
    for (pos, prop) in q_props.iter(world) {
        if prop.terrain == Terrain::Capital && prop.owner_id == Some(perspective_player) {
            my_capital_pos = Some(*pos);
            break;
        }
    }

    // V4では大局戦略（VictoryRoadmap/CampaignPortfolio/CombatPlan）で部隊編成を管理するため、
    // レガシーな「首都から5ターン以内の全敵クラスターに対する防衛部隊作成」は実行しない。
    // 小規模マップ等で全域が首都防衛圏になり、前線戦闘ユニットが誤って防衛に拘束されるのを防ぐ。
    if !is_v4 {
        if let Some(capital) = my_capital_pos {
            for cluster in &enemy_clusters {
                let turn_dist = calculate_turn_distance(
                    &map,
                    &registry,
                    &unit_positions,
                    (capital.x, capital.y),
                    (cluster.center.x, cluster.center.y),
                    crate::resources::MovementType::Infantry,
                    3,
                    1,
                    perspective_player,
                    &mut turn_cache,
                );

                if turn_dist.turns <= 5 {
                    // すでにこのクラスターを防衛目標としている部隊があるか
                    let exists = manager.squads.iter().any(|s| {
                        squad_is_mutable_by_player(world, s, perspective_player)
                            && s.mission_type == MissionType::Defense
                            && s.target == Some(cluster.center)
                    });

                    if !exists {
                        let squad =
                            manager.create_owned_squad(MissionType::Defense, perspective_player);
                        squad.target = Some(cluster.center);
                        squad.phase = MissionPhase::Forming;

                        // 最寄りの戦闘ユニットを最大2基割り当てる
                        free_combat_units.sort_by_key(|(_, pos, stats)| {
                            calculate_turn_distance(
                                &map,
                                &registry,
                                &unit_positions,
                                (pos.x, pos.y),
                                (cluster.center.x, cluster.center.y),
                                stats.movement_type,
                                stats.max_movement,
                                1,
                                perspective_player,
                                &mut turn_cache,
                            )
                        });

                        let assign_count = free_combat_units.len().min(3);
                        for _ in 0..assign_count {
                            let (ent, _, _) = free_combat_units.remove(0);
                            squad.members.insert(ent);
                        }
                    }
                }
            }
        }
    }

    // C. 占領部隊の立ち上げ（Expansionフェーズ）
    // #53 (V3): 中立拠点に加えて敵所有拠点も奪取目標に含める。
    // 従来は中立拠点のみが対象だったため、中立を取り切ると占領部隊が
    // 編成されなくなり、敵領土の奪取が発生しなかった。
    let mut capture_targets: Vec<GridPosition> =
        strategy.unowned_properties.iter().cloned().collect();
    if is_v3 {
        capture_targets.extend(strategy.enemy_properties.iter().cloned());
        capture_targets.retain(|target| {
            island_map
                .get_island_at(target)
                .is_none_or(|island| !paused_campaign_islands.contains(&island.id))
        });
    }
    // 最寄りのフリー歩兵から近い順に割り当てる (HashSet 順の非決定性も排除し、
    // 限られた歩兵を近い目標へ優先的に振り分ける)。
    // #53 (V3): 工場・首都などの生産施設は敵の増援源であり、奪取すれば
    // 前線の消耗戦を根本から崩せるため、多少遠くても優先する
    // (前線都市の取り返し合いに全歩兵が吸われる膠着の解消)
    const PRODUCTION_FACILITY_DIST_BONUS: usize = 6;
    // #53 (V3): 敵生産施設への奪取部隊は同時に1個まで (集中スピアヘッド)。
    // 複数同時に編成すると前線から兵力が抜かれすぎて防衛線が崩壊し、
    // 逐次投入の各個撃破で全部隊が溶けるため
    const MAX_CONCURRENT_FACILITY_CAPTURES: usize = 1;
    let is_enemy_facility = |t: &GridPosition| -> bool {
        properties_ownership
            .get(t)
            .is_some_and(|o| o.is_some() && *o != Some(perspective_player))
            && map
                .get_terrain(t.x, t.y)
                .is_some_and(|tr| registry.is_production_facility(tr.as_str()))
    };
    let mut active_facility_captures = manager
        .squads
        .iter()
        .filter(|s| {
            squad_is_mutable_by_player(world, s, perspective_player)
                && s.mission_type == MissionType::Capture
                && !s.members.is_empty()
                && s.target.as_ref().is_some_and(is_enemy_facility)
        })
        .count();
    if crate::ai::strategy_profile::current_profile(world, perspective_player)
        == crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion
    {
        let enemy_capital = {
            let mut q_props = world.query::<(&GridPosition, &Property)>();
            q_props.iter(world).find_map(|(pos, prop)| {
                (prop.terrain == Terrain::Capital
                    && prop.owner_id != Some(perspective_player)
                    && prop.owner_id.is_some())
                .then_some(*pos)
            })
        };
        if let Some(my_cap) = my_capital_pos {
            capture_targets.sort_unstable_by_key(|t| {
                let d_my = map.distance(my_cap.x, my_cap.y, t.x, t.y);
                let d_enemy = enemy_capital
                    .map(|ec| map.distance(ec.x, ec.y, t.x, t.y))
                    .unwrap_or(0);
                (std::cmp::Reverse(d_my), d_enemy, t.y, t.x)
            });
        }
    } else {
        capture_targets.sort_by_key(|t| {
            let d = free_infantry
                .iter()
                .map(|(_, pos, _)| pos.x.abs_diff(t.x) + pos.y.abs_diff(t.y))
                .min()
                .unwrap_or(usize::MAX);
            let facility_bonus = if is_v3
                && map
                    .get_terrain(t.x, t.y)
                    .is_some_and(|tr| registry.is_production_facility(tr.as_str()))
            {
                PRODUCTION_FACILITY_DIST_BONUS
            } else {
                0
            };
            (d.saturating_sub(facility_bonus), t.x, t.y)
        });
    }

    // #53 (V3): 敵生産施設への突入 (スピアヘッド) は戦力優勢 (Assault フェーズ)
    // のときのみ許可する。拮抗・劣勢時に前線から兵力を抜くと防衛線が崩壊する
    let allow_facility_capture = strategy.phase == crate::ai::strategy::GamePhase::Assault;

    for unowned_pos in &capture_targets {
        // #53 (V3): 敵生産施設への奪取部隊は同時 MAX_CONCURRENT_FACILITY_CAPTURES 個まで
        let target_is_enemy_facility = is_enemy_facility(unowned_pos);
        if target_is_enemy_facility
            && (!allow_facility_capture
                || active_facility_captures >= MAX_CONCURRENT_FACILITY_CAPTURES)
        {
            continue;
        }

        let target_island_opt = island_map.get_island_at(unowned_pos);
        if target_island_opt.is_none() {
            continue;
        }
        let target_island = target_island_opt.unwrap();
        let is_on_base_island = base_islands.contains(&target_island.id);

        // この未占領拠点と同じ島にいるフリーの歩兵を探す（目標に最も近い歩兵を選択）
        let inf_on_same_island_idx = free_infantry
            .iter()
            .enumerate()
            .filter(|(_, (_, pos, stats))| {
                island_map
                    .get_island_at(pos)
                    .is_some_and(|island| island.id == target_island.id)
                    && is_terrain_reachable(
                        &map,
                        &registry,
                        (pos.x, pos.y),
                        (unowned_pos.x, unowned_pos.y),
                        stats.movement_type,
                    )
            })
            .min_by_key(|(_, (ent, pos, _))| {
                (
                    map.distance(pos.x, pos.y, unowned_pos.x, unowned_pos.y),
                    ent.to_bits(),
                )
            })
            .map(|(idx, _)| idx);

        // V3の洋上移動はportfolio transportだけが担当し、generic Captureは到達可能な現地要員に限定する。
        let can_capture = inf_on_same_island_idx.is_some()
            || !is_v3 && is_on_base_island && !free_infantry.is_empty();

        if can_capture {
            let exists = manager.squads.iter().any(|s| {
                squad_is_mutable_by_player(world, s, perspective_player)
                    && s.mission_type == MissionType::Capture
                    && s.target == Some(*unowned_pos)
            });

            if !exists {
                let squad = manager.create_owned_squad(MissionType::Capture, perspective_player);
                squad.target = Some(*unowned_pos);
                if is_v3 {
                    squad.target_island = Some(target_island.id);
                }
                squad.phase = MissionPhase::Forming;

                // 割り当てる歩兵の選択：
                // 同じ島にいる歩兵がいればそれを優先、いなければ free_infantry の最後の歩兵を割り当てる
                let assigned_inf_idx = if let Some(idx) = inf_on_same_island_idx {
                    idx
                } else {
                    free_infantry.len() - 1
                };

                let (inf_ent, _, inf_stats) = free_infantry.remove(assigned_inf_idx);
                let inf_movement = inf_stats.max_movement;
                squad.members.insert(inf_ent);

                // #53 (V3): 敵生産施設 (工場・首都等) への奪取部隊には護衛の
                // 戦闘ユニットを最大2両随伴させる。敵の生産圏は防御が厚く、
                // 単独の歩兵では到達前に撃破されて奪取が成立しないため。
                // 護衛は「歩兵に随伴できる機動力があり戦闘可能」なユニットに限定する
                // (鈍足の砲台や弾切れ・損傷ユニットを組み込むと部隊が機能しないため)
                if is_v3 && target_is_enemy_facility {
                    // 対象拠点から「遠い順」に並べ替え、末尾 (最も近いユニット) から pop() で
                    // 取り出すことで Vec 先頭削除による O(N) シフトを避ける (#57 レビュー対応)。
                    free_combat_units.sort_by_key(|(_, pos, _)| {
                        std::cmp::Reverse(
                            pos.x.abs_diff(unowned_pos.x) + pos.y.abs_diff(unowned_pos.y),
                        )
                    });
                    let mut assigned = 0;
                    // 護衛条件を満たさず不採用としたユニットは後で free pool に戻す
                    let mut rejected = Vec::new();
                    while assigned < 2 {
                        let Some((ent, pos, stats)) = free_combat_units.pop() else {
                            break;
                        };
                        let hp = world
                            .get::<crate::components::Health>(ent)
                            .map(|h| h.current)
                            .unwrap_or(100);
                        let (ammo1, ammo2) = world
                            .get::<crate::components::Ammo>(ent)
                            .map(|a| (a.ammo1, a.ammo2))
                            .unwrap_or((u32::MAX, u32::MAX));
                        if escort_is_eligible(&stats, inf_movement, hp, ammo1, ammo2) {
                            squad.members.insert(ent);
                            assigned += 1;
                        } else {
                            rejected.push((ent, pos, stats));
                        }
                    }
                    // 護衛に採用しなかった候補は他部隊が利用できるよう free pool へ戻す
                    free_combat_units.append(&mut rejected);
                    active_facility_captures += 1;
                }
            }
        }
    }

    // D. 攻撃部隊の立ち上げ
    for cluster in &enemy_clusters {
        if free_combat_units.is_empty() {
            break;
        }

        let exists = manager.squads.iter().any(|s| {
            squad_is_mutable_by_player(world, s, perspective_player)
                && s.mission_type == MissionType::Attack
                && s.target == Some(cluster.center)
        });

        if !exists {
            let squad = manager.create_owned_squad(MissionType::Attack, perspective_player);
            squad.target = Some(cluster.center);
            squad.phase = MissionPhase::Forming;

            // 最寄りの戦闘ユニットを最大2基割り当てる
            free_combat_units.sort_by_key(|(_, pos, stats)| {
                calculate_turn_distance(
                    &map,
                    &registry,
                    &unit_positions,
                    (pos.x, pos.y),
                    (cluster.center.x, cluster.center.y),
                    stats.movement_type,
                    stats.max_movement,
                    1,
                    perspective_player,
                    &mut turn_cache,
                )
            });

            let assign_count = free_combat_units.len().min(3);
            for _ in 0..assign_count {
                let (ent, _, _) = free_combat_units.remove(0);
                squad.members.insert(ent);
            }
        }
    }

    // ---------------------------------------------------------------
    // 定員管理付きの余剰ユニット割り当てロジック（交通渋滞解消）
    // 1部隊の最大人数。これを超えると後続ユニットが前線で渋滞を起こす
    const MAX_SQUAD_SIZE: usize = 3;

    while !free_combat_units.is_empty() {
        let (ent, pos, stats) = free_combat_units.pop().unwrap();

        // ステップ1: 定員未満の既存 Attack/Defense 部隊のうち、最も近いものを探す
        let mut best_squad_idx = None;
        let mut min_dist = crate::ai::turn_distance::TurnDistance {
            turns: u32::MAX,
            used_mp: u32::MAX,
        };

        for (idx, squad) in manager.squads.iter().enumerate() {
            if squad_is_mutable_by_player(world, squad, perspective_player)
                && (squad.mission_type == MissionType::Attack
                    || squad.mission_type == MissionType::Defense)
                && squad.members.len() < MAX_SQUAD_SIZE
            {
                if let Some(target) = squad.target {
                    let dist = calculate_turn_distance(
                        &map,
                        &registry,
                        &unit_positions,
                        (pos.x, pos.y),
                        (target.x, target.y),
                        stats.movement_type,
                        stats.max_movement,
                        1,
                        perspective_player,
                        &mut turn_cache,
                    );
                    if dist < min_dist {
                        min_dist = dist;
                        best_squad_idx = Some(idx);
                    }
                }
            }
        }

        if let Some(idx) = best_squad_idx {
            // 定員未満の部隊に吸収
            manager.squads[idx].members.insert(ent);
        } else {
            // ステップ2: 既存部隊がすべて定員に達している場合、
            // 最も近い敵クラスターを目標とする新規 Attack 部隊（第2波）を新設する
            let mut nearest_cluster_dist = crate::ai::turn_distance::TurnDistance {
                turns: u32::MAX,
                used_mp: u32::MAX,
            };
            let mut nearest_cluster_center = None;

            for cluster in &enemy_clusters {
                let dist = calculate_turn_distance(
                    &map,
                    &registry,
                    &unit_positions,
                    (pos.x, pos.y),
                    (cluster.center.x, cluster.center.y),
                    stats.movement_type,
                    stats.max_movement,
                    1,
                    perspective_player,
                    &mut turn_cache,
                );
                if dist < nearest_cluster_dist {
                    nearest_cluster_dist = dist;
                    nearest_cluster_center = Some(cluster.center);
                }
            }

            // 敵が15ターン以上遠い場合や存在しない場合は、最寄りの拠点（未占領または敵所有）を目標にする
            let final_target = if nearest_cluster_dist.turns <= 15 {
                nearest_cluster_center
            } else {
                let mut nearest_prop_dist = u32::MAX;
                let mut nearest_prop_pos = None;
                for (p_pos, p_owner) in &properties_ownership {
                    if *p_owner != Some(perspective_player) {
                        // 処理時間を削減するためマンハッタン距離による概算を使用
                        let dist = (pos.x as i32 - p_pos.x as i32).unsigned_abs()
                            + (pos.y as i32 - p_pos.y as i32).unsigned_abs();
                        if dist < nearest_prop_dist {
                            nearest_prop_dist = dist;
                            nearest_prop_pos = Some(*p_pos);
                        }
                    }
                }
                nearest_prop_pos.or(nearest_cluster_center)
            };

            if let Some(target) = final_target {
                // 新規部隊を立ち上げて1体目のメンバーとして割り当てる
                let new_squad = manager.create_owned_squad(MissionType::Attack, perspective_player);
                new_squad.target = Some(target);
                new_squad.phase = MissionPhase::Forming;
                new_squad.members.insert(ent);
            }
            // 目標がまったく存在しない場合は放置（SoloFallback として機能）
        }
    }

    if is_v4 {
        // campaign/deployment/generic候補の勝者を先に確定する。正規化前の重複Squadを
        // 「任務あり」と数えると、この直後に敗者参照が落ちてownerだけの遊兵が残る。
        // 手番末の固定点を作る有界線形パスであり、行動候補ごとの走査ではない。
        reconcile_unique_operation_assignments(world, &mut manager, perspective_player);
        reconnect_v4_operation_fixed_point(
            world,
            &mut manager,
            perspective_player,
            &campaign_assignments,
        );
        assign_explicit_reserve_squads(
            world,
            &mut manager,
            perspective_player,
            &campaign_assignments,
        );
    }

    // campaign・deployment・汎用任務をすべて構築した後に一度だけ正規化する。
    // これ以降の行動選択は重複Squadを見ず、所有者照会も平均O(1)になる。
    reconcile_unique_operation_assignments(world, &mut manager, perspective_player);
    if is_v4 {
        crate::ai::v4::strategy_pipeline::after_squad_planning(
            world,
            perspective_player,
            &roadmap_portfolio,
            &mut manager,
        );
    }
    world.insert_resource(manager);
}

/// 全体需要から決めたroute枠へ、memberを一度だけ配属する。
///
/// `route_slots` は戦役全体の資源・戦力・地形ETAから先に算出済みであり、ここでは
/// 個々のunitを仮想投入して再評価しない。需要ゼロのrouteは空でもよく、残った
/// memberは最寄りの実行可能routeへ置いて無所属を作らない。
pub(super) fn distribute_members_across_route_fronts(
    world: &World,
    map: &Map,
    members: &[Entity],
    route_fronts: &[GridPosition],
    route_slots: &[usize],
) -> Vec<Vec<Entity>> {
    let mut remaining = members.to_vec();
    remaining.sort_by_key(|entity| entity.to_bits());
    remaining.dedup();
    let mut distributed = vec![Vec::new(); route_fronts.len()];

    // 先に全体需要から決めた枠だけを埋める。配属時には距離だけを使うので、既に
    // 盤面評価へ含めた戦力差や収益をunitごとに二重計上しない。
    let mut unassigned = Vec::new();
    for member in remaining {
        let route = world.get::<GridPosition>(member).and_then(|position| {
            route_fronts
                .iter()
                .enumerate()
                .filter(|(route, _)| {
                    distributed[*route].len() < route_slots.get(*route).copied().unwrap_or(0)
                })
                .min_by_key(|(route, front)| {
                    (
                        map.distance(position.x, position.y, front.x, front.y),
                        *route,
                    )
                })
                .map(|(route, _)| route)
        });
        if let Some(route) = route {
            distributed[route].push(member);
        } else {
            unassigned.push(member);
        }
    }

    // 需要枠を超えるmemberも、同じcampaign内の最寄りrouteへ結び直す。これにより
    // squadを失った遊休戦力や、汎用Reserveへの退避を発生させない。
    for member in unassigned {
        let route = world
            .get::<GridPosition>(member)
            .map(|position| {
                route_fronts
                    .iter()
                    .enumerate()
                    .min_by_key(|(index, front)| {
                        (
                            map.distance(position.x, position.y, front.x, front.y),
                            distributed[*index].len(),
                            *index,
                        )
                    })
                    .map_or(0, |(index, _)| index)
            })
            .unwrap_or(0);
        distributed[route].push(member);
    }
    distributed
}

pub(super) fn campaign_combat_responsibilities_for_enemies(
    world: &World,
    members: &[Entity],
    mut enemies: Vec<(Entity, GridPosition)>,
    connectivity: &mut TerrainConnectivity,
) -> Vec<CampaignResponsibility> {
    enemies.sort_by_key(|(entity, target)| (target.y, target.x, entity.to_bits()));

    let mut remaining = members.to_vec();
    remaining.sort_by_key(|entity| entity.to_bits());
    remaining.dedup();
    let mut responsibilities = Vec::new();

    while !remaining.is_empty() {
        let mut candidates: Vec<_> = enemies
            .iter()
            .filter_map(|(enemy, target)| {
                let reachable: Vec<_> = remaining
                    .iter()
                    .filter_map(|member| {
                        campaign_member_attack_distance_with_connectivity(
                            world,
                            *member,
                            *enemy,
                            *target,
                            connectivity,
                        )
                        .map(|distance| (*member, distance))
                    })
                    .collect();
                if reachable.is_empty() {
                    return None;
                }
                let total_distance = reachable.iter().fold(0usize, |total, (_, distance)| {
                    total.saturating_add(*distance)
                });
                Some((*enemy, *target, reachable, total_distance))
            })
            .collect();
        if candidates.is_empty() {
            break;
        }
        // 全員が到達できる共通敵を最優先し、無ければ最多memberを受け持つ局所目標を選ぶ。
        candidates.sort_by_key(|(enemy, target, reachable, total_distance)| {
            (
                usize::MAX - reachable.len(),
                *total_distance,
                target.y,
                target.x,
                enemy.to_bits(),
            )
        });
        let (_, target, reachable, _) = candidates.remove(0);
        let assigned: Vec<_> = reachable.iter().map(|(member, _)| *member).collect();
        let assigned_set: HashSet<_> = assigned.iter().copied().collect();
        remaining.retain(|member| !assigned_set.contains(member));
        responsibilities.push(CampaignResponsibility {
            mission_type: MissionType::Attack,
            target,
            members: assigned,
        });
    }

    // 敵へ到達できないmemberは、全員が到達できるmember位置ごとに決定的に現地防御へまとめる。
    while !remaining.is_empty() {
        let mut hold_candidates: Vec<_> = remaining
            .iter()
            .filter_map(|source| {
                let target = world.get::<GridPosition>(*source).copied()?;
                let reachable: Vec<_> = remaining
                    .iter()
                    .filter_map(|member| {
                        campaign_member_distance_to_position(world, *member, target, connectivity)
                            .map(|distance| (*member, distance))
                    })
                    .collect();
                let total_distance = reachable.iter().fold(0usize, |total, (_, distance)| {
                    total.saturating_add(*distance)
                });
                Some((*source, target, reachable, total_distance))
            })
            .collect();
        hold_candidates.sort_by_key(|(source, target, reachable, total_distance)| {
            (
                usize::MAX - reachable.len(),
                *total_distance,
                target.y,
                target.x,
                source.to_bits(),
            )
        });
        let Some((_, target, reachable, _)) = hold_candidates.into_iter().next() else {
            break;
        };
        let assigned: Vec<_> = reachable.iter().map(|(member, _)| *member).collect();
        let assigned_set: HashSet<_> = assigned.iter().copied().collect();
        remaining.retain(|member| !assigned_set.contains(member));
        responsibilities.push(CampaignResponsibility {
            mission_type: MissionType::Defense,
            target,
            members: assigned,
        });
    }

    responsibilities.sort_by_key(|responsibility| {
        let mission_rank = match responsibility.mission_type {
            MissionType::Attack => 0,
            MissionType::Defense => 1,
            MissionType::Capture => 2,
            MissionType::Reserve => 3,
            MissionType::Transport => 4,
        };
        (
            mission_rank,
            responsibility.target.y,
            responsibility.target.x,
            responsibility
                .members
                .iter()
                .map(|entity| entity.to_bits())
                .min()
                .unwrap_or(u64::MAX),
        )
    });
    responsibilities
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::*;
    use crate::resources::master_data::*;
    use crate::resources::*;

    fn setup_test_world() -> World {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }

        let map = Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: GridTopology::Square,
        };

        // Setup a small map
        world.insert_resource(map.clone());
        world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
        world.insert_resource(SquadManager::new());
        world
    }

    #[test]
    fn map26_combat_routes_target_bridgeheads_until_ground_crossing() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_26",
            GridTopology::Square,
        )
        .expect("map_26 initialization should succeed");
        let player = PlayerId(1);
        let own_capital = world
            .iter_entities()
            .find_map(|entity| {
                let position = entity.get::<GridPosition>()?;
                let property = entity.get::<Property>()?;
                (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                    .then_some(*position)
            })
            .expect("own capital");
        let island_id = world
            .resource::<crate::ai::islands::IslandMap>()
            .get_island_at(&own_capital)
            .expect("capital island")
            .id;
        let members = (0..2)
            .map(|_| {
                world
                    .spawn((
                        Faction(player),
                        own_capital,
                        UnitStats {
                            unit_type: UnitType::Tank,
                            movement_type: MovementType::Tank,
                            max_movement: 6,
                            ..UnitStats::mock()
                        },
                    ))
                    .id()
            })
            .collect::<Vec<_>>();
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 2,
            combat_units: 0,
            total_budget: 14_000,
        };
        let assignment = IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Secure,
            target_position: GridPosition { x: 13, y: 9 },
            capture_target_positions: vec![GridPosition { x: 13, y: 9 }],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: requirement,
            allocated_budget: 14_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: members.clone(),
            operation_ready: true,
            continued_from_existing_squad: false,
        };

        let responsibilities = campaign_combat_responsibilities(
            &world,
            player,
            &assignment,
            &members,
            &mut TerrainConnectivity::default(),
        );
        let targets = responsibilities
            .iter()
            .map(|responsibility| responsibility.target)
            .collect::<HashSet<_>>();

        assert_eq!(responsibilities.len(), 2);
        assert!(
            responsibilities
                .iter()
                .all(|responsibility| responsibility.mission_type == MissionType::Defense)
        );
        assert_eq!(
            targets,
            HashSet::from([GridPosition { x: 14, y: 4 }, GridPosition { x: 11, y: 17 },])
        );

        // Secureの占領要員を固定的にDefenseへ抜き取らない。CaptureとCombatは同じ
        // campaignの別責務として保ち、橋頭堡を担うのは需要配分済みの装甲Combatだけにする。
    }

    #[test]
    fn combat_members_follow_precomputed_route_slots_without_duplicate_ownership() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let mut world = setup_test_world();
        let player = PlayerId(1);
        let first = world
            .spawn((Faction(player), GridPosition { x: 2, y: 5 }))
            .id();
        let second = world
            .spawn((Faction(player), GridPosition { x: 2, y: 5 }))
            .id();
        let map = world.resource::<Map>().clone();
        let fronts = vec![GridPosition { x: 7, y: 1 }, GridPosition { x: 7, y: 9 }];

        let distributed = distribute_members_across_route_fronts(
            &world,
            &map,
            &[first, second],
            &fronts,
            &[1, 1],
        );

        assert_eq!(distributed.len(), 2);
        assert!(
            distributed.iter().all(|members| members.len() == 1),
            "戦役需要から既に決まった二枠へ、同じEntityを重複させず配属する"
        );
        let assigned = distributed.into_iter().flatten().collect::<HashSet<_>>();
        assert_eq!(assigned, HashSet::from([first, second]));
    }

    #[test]
    fn active_campaign_releases_entity_removed_from_current_portfolio() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let mut world = setup_test_world();
        let player = PlayerId(1);
        let stale = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let island = crate::ai::islands::IslandId(3);
        let mut registry = UnitOperationRegistry::default();
        registry.assign(
            stale,
            UnitOperationAssignment {
                owner: OperationOwner::Campaign {
                    player_id: player,
                    island_id: island,
                },
                squad_id: None,
                role: OperationUnitRole::Member,
                assigned_turn: 1,
            },
        );
        world.insert_resource(registry);
        let requirement = crate::ai::island_campaign::IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 0,
        };
        let portfolio = crate::ai::island_campaign::IslandCampaignPortfolio {
            active_offensives: vec![crate::ai::island_campaign::IslandCampaignAssignment {
                island_id: island,
                decision: crate::ai::island_campaign::IslandCampaignDecision::Contest,
                target_position: GridPosition { x: 3, y: 3 },
                capture_target_positions: Vec::new(),
                priority_enemy_types: Vec::new(),
                requirement: requirement.clone(),
                purchase_shortfall: requirement,
                allocated_budget: 0,
                transport_entities: Vec::new(),
                capture_entities: Vec::new(),
                combat_entities: Vec::new(),
                operation_ready: true,
                continued_from_existing_squad: true,
            }],
            ..Default::default()
        };

        claim_campaign_portfolio_assignments(&mut world, player, &portfolio);

        assert!(
            world
                .resource::<UnitOperationRegistry>()
                .assignment(stale)
                .is_none(),
            "同じ島の作戦継続だけを理由に旧Entityを作戦なしで固定しない"
        );
    }

    #[test]
    fn surplus_campaign_capturers_receive_distinct_property_responsibilities() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let mut world = World::new();
        let mut map = Map::new(6, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(5, 0, Terrain::Capital).unwrap();
        for x in 2..=4 {
            map.set_terrain(x, 0, Terrain::City).unwrap();
        }
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map
            .get_island_at(&GridPosition { x: 2, y: 0 })
            .unwrap()
            .id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let player = PlayerId(1);
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 5, y: 0 },
            Property::new(Terrain::Capital, Some(PlayerId(2)), 100),
        ));
        for x in 2..=4 {
            world.spawn((
                GridPosition { x, y: 0 },
                Property::new(Terrain::City, None, 100),
            ));
        }
        let capturers = (0..3)
            .map(|x| {
                world
                    .spawn((
                        Faction(player),
                        GridPosition { x, y: 0 },
                        UnitStats {
                            can_capture: true,
                            movement_type: MovementType::Infantry,
                            max_movement: 3,
                            ..UnitStats::mock()
                        },
                    ))
                    .id()
            })
            .collect::<Vec<_>>();
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 3,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 3_000,
        };
        let assignment = IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Secure,
            target_position: GridPosition { x: 2, y: 0 },
            // 明示Milestoneが1件でも、余剰2体を同じ施設へ重複させてはならない。
            capture_target_positions: vec![GridPosition { x: 2, y: 0 }],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: requirement,
            allocated_budget: 3_000,
            transport_entities: Vec::new(),
            capture_entities: capturers.clone(),
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let mut manager = SquadManager::new();
        let pinned_target = GridPosition { x: 4, y: 0 };
        let pinned_member = capturers[2];
        let pinned = manager.create_owned_squad(MissionType::Capture, player);
        pinned.members.insert(pinned_member);
        pinned.target_island = Some(island_id);
        pinned.target = Some(pinned_target);

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);

        let capture_squads = manager
            .squads
            .iter()
            .filter(|squad| squad.mission_type == MissionType::Capture)
            .collect::<Vec<_>>();
        assert_eq!(capture_squads.len(), 3);
        assert_eq!(
            capture_squads
                .iter()
                .filter_map(|squad| squad.target)
                .collect::<HashSet<_>>()
                .len(),
            3,
            "占領可能施設が残る限り、1施設1Squadへ排他的に振り分ける"
        );
        assert_eq!(
            capture_squads
                .iter()
                .find(|squad| squad.members.contains(&pinned_member))
                .and_then(|squad| squad.target),
            Some(pinned_target),
            "生産時に具体化した未取得・到達可能な物件任務は最寄り物件へ戻さない"
        );
    }
}
