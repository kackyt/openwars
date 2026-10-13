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
    synchronize_local_capture_departure(world, manager, player_id, assignment, connectivity);
}

/// 拡張の再編差分はこの型へ閉じ込め、再編本体は通常側と共有する。
pub(super) struct ExtendedPlanningHooks;

impl SquadPlanningHooks for ExtendedPlanningHooks {
    fn before_manager_removal(world: &mut World, player: PlayerId, is_v4: bool) {
        if is_v4 {
            crate::ai::v4::strategy_pipeline::before_squad_planning(world, player);
        }
    }

    fn portfolio_for_planning<'a>(
        world: &mut World,
        player: PlayerId,
        manager: &SquadManager,
        portfolio: &'a crate::ai::island_campaign::IslandCampaignPortfolio,
        is_v4: bool,
    ) -> std::borrow::Cow<'a, crate::ai::island_campaign::IslandCampaignPortfolio> {
        let turn = world
            .get_resource::<crate::resources::MatchState>()
            .map_or(0, |state| state.current_turn_number.0);
        std::borrow::Cow::Owned(if is_v4 {
            crate::ai::v4::strategy_pipeline::portfolio_for_squads(
                world,
                player,
                turn,
                portfolio.clone(),
                manager,
            )
        } else {
            portfolio.clone()
        })
    }

    fn prepare_local_assignment(
        world: &World,
        manager: &mut SquadManager,
        player: PlayerId,
        assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
        connectivity: &mut TerrainConnectivity,
    ) {
        // 親の同名関数へ単純に置換しない。ここからは子の独立ID付き実装を直接呼ぶ。
        self::prepare_campaign_local_assignment_with_connectivity(
            world,
            manager,
            player,
            assignment,
            connectivity,
        );
    }

    fn uses_legacy_capital_defense(is_v4: bool) -> bool {
        !is_v4
    }

    fn sort_capture_targets(
        world: &mut World,
        player: PlayerId,
        targets: &mut [GridPosition],
        context: &CapturePlanningContext<'_>,
    ) {
        if crate::ai::strategy_profile::current_profile(world, player)
            == crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion
        {
            let enemy_capital = {
                let mut properties = world.query::<(&GridPosition, &Property)>();
                properties.iter(world).find_map(|(position, property)| {
                    (property.terrain == Terrain::Capital
                        && property.owner_id != Some(player)
                        && property.owner_id.is_some())
                    .then_some(*position)
                })
            };
            // 小規模かつ自首都なしでは従来どおり入力順を保つ。通常sortへ戻さない。
            if let Some(capital) = context.capital {
                targets.sort_unstable_by_key(|target| {
                    let distance = context
                        .map
                        .distance(capital.x, capital.y, target.x, target.y);
                    let enemy_distance = enemy_capital
                        .map(|enemy| context.map.distance(enemy.x, enemy.y, target.x, target.y))
                        .unwrap_or(0);
                    (
                        std::cmp::Reverse(distance),
                        enemy_distance,
                        target.y,
                        target.x,
                    )
                });
            }
        } else {
            MainPlanningHooks::sort_capture_targets(world, player, targets, context);
        }
    }

    fn select_capture_infantry(
        context: &CapturePlanningContext<'_>,
        island: crate::ai::islands::IslandId,
        target: &GridPosition,
    ) -> Option<usize> {
        context
            .infantry
            .iter()
            .enumerate()
            .filter(|(_, (_, position, stats))| {
                context
                    .island_map
                    .get_island_at(position)
                    .is_some_and(|candidate| candidate.id == island)
                    && is_terrain_reachable(
                        context.map,
                        context.registry,
                        (position.x, position.y),
                        (target.x, target.y),
                        stats.movement_type,
                    )
            })
            .min_by_key(|(_, (entity, position, _))| {
                (
                    context
                        .map
                        .distance(position.x, position.y, target.x, target.y),
                    entity.to_bits(),
                )
            })
            .map(|(index, _)| index)
    }

    fn before_final_reconcile(
        _world: &mut World,
        _player: PlayerId,
        _portfolio: &crate::ai::island_campaign::IslandCampaignPortfolio,
        _manager: &SquadManager,
        _is_v4: bool,
    ) {
    }

    fn after_final_reconcile(
        world: &mut World,
        player: PlayerId,
        portfolio: &crate::ai::island_campaign::IslandCampaignPortfolio,
        manager: &mut SquadManager,
        is_v4: bool,
    ) {
        if is_v4 {
            crate::ai::v4::strategy_pipeline::after_squad_planning(
                world, player, portfolio, manager,
            );
        }
    }
}

/// ゲームの戦略状況に基づいて、自動的に部隊の構築と新規メンバーの割り当てを行います。
pub(crate) fn plan_squads(world: &mut World, perspective_player: PlayerId) {
    if !crate::ai::code_path::ext_enabled("squad.rs::plan_squads") {
        return super::plan_squads(world, perspective_player);
    }
    super::plan_squads_with_hooks::<ExtendedPlanningHooks>(world, perspective_player);
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
            HashSet::from_iter([GridPosition { x: 14, y: 4 }, GridPosition { x: 11, y: 17 },])
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
        assert_eq!(assigned, HashSet::from_iter([first, second]));
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
