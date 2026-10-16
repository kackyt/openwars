//! 敵と首都ルート前線への戦闘責務。

use super::{
    CampaignResponsibility, Entity, Faction, GridPosition, HashSet, Map, MissionType, PlayerId,
    TerrainConnectivity, World, campaign_member_attack_distance_with_connectivity,
    campaign_member_distance_to_position,
};
#[cfg(test)]
use super::{
    OperationOwner, OperationUnitRole, SquadManager, UnitOperationAssignment,
    UnitOperationRegistry, claim_campaign_portfolio_assignments, prepare_campaign_local_assignment,
};
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

    let route_fronts =
        crate::ai::v4::capital_route::topology::same_land_route_fronts(world, player_id, island_id);
    if route_fronts.len() >= 2 {
        let Some(map) = world.get_resource::<Map>() else {
            return Vec::new();
        };
        let pending_bridgeheads =
            crate::ai::v4::capital_route::force_demand::same_land_pending_bridgeheads(
                world, player_id, island_id,
            );
        let breakthrough_positions =
            crate::ai::v4::capital_route::force_demand::same_land_route_breakthrough_positions(
                world, player_id, island_id,
            );
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
            crate::ai::v4::capital_route::force_demand::same_land_route_force_demands(
                world, player_id, island_id,
            );
        let route_slots = crate::ai::v4::capital_route::force_demand::apportion_route_force_slots(
            members.len(),
            &route_demands,
        );
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
        let mut pinned = manager.create_owned_squad(MissionType::Capture, player);
        pinned.members.insert(pinned_member);
        pinned.plan.target_island = Some(island_id);
        pinned.plan.target = Some(pinned_target);

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);

        let capture_squads = manager
            .squads
            .iter()
            .filter(|squad| squad.plan.mission_type == MissionType::Capture)
            .collect::<Vec<_>>();
        assert_eq!(capture_squads.len(), 3);
        assert_eq!(
            capture_squads
                .iter()
                .filter_map(|squad| squad.plan.target)
                .collect::<HashSet<_>>()
                .len(),
            3,
            "占領可能施設が残る限り、1施設1Squadへ排他的に振り分ける"
        );
        assert_eq!(
            capture_squads
                .iter()
                .find(|squad| squad.members.contains(&pinned_member))
                .and_then(|squad| squad.plan.target),
            Some(pinned_target),
            "生産時に具体化した未取得・到達可能な物件任務は最寄り物件へ戻さない"
        );
    }
}
