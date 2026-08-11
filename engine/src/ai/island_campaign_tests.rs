use std::collections::{BTreeSet, HashMap, HashSet};

use bevy_ecs::prelude::*;

use crate::ai::island_campaign::IslandCampaignAssignment;
use crate::ai::squad::{MissionPhase, MissionType, SquadManager, TransportPhase, plan_squads};
use crate::ai::{AiVersion, PlayerAiSettings};
use crate::components::{
    CargoCapacity, Faction, GridPosition, Health, PlayerId, Property, Transporting, UnitStats,
};
use crate::resources::master_data::{MasterDataRegistry, UnitName};
use crate::resources::{GridTopology, Map, MovementType, Players, Terrain, UnitType};

fn empty_v3_world() -> (World, MasterDataRegistry, PlayerId) {
    let master_data = MasterDataRegistry::load().expect("master data should load");
    let (mut world, _) = crate::setup::initialize_world_from_master_data(&master_data, "map_1")
        .expect("test world should initialize");
    let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
    for entity in entities {
        world.despawn(entity);
    }

    let player = PlayerId(1);
    let mut settings = PlayerAiSettings::default();
    settings.set_version(player, AiVersion::V3);
    world.insert_resource(settings);
    for entry in &mut world.resource_mut::<Players>().0 {
        entry.funds = 0;
    }
    world.insert_resource(SquadManager::new());
    (world, master_data, player)
}

fn sorted_assignment_entities(assignment: &IslandCampaignAssignment) -> Vec<Entity> {
    let mut entities: Vec<_> = assignment
        .transport_entities
        .iter()
        .chain(assignment.capture_entities.iter())
        .chain(assignment.combat_entities.iter())
        .copied()
        .collect();
    entities.sort_by_key(|entity| entity.to_bits());
    entities.dedup();
    entities
}

#[test]
fn four_expand_candidates_execute_exactly_three_complete_island_operations() {
    let (mut world, master_data, player) = empty_v3_world();
    let mut map = Map::new(11, 3, Terrain::Sea, GridTopology::Square);
    for x in 0..=2 {
        map.set_terrain(x, 1, Terrain::Plains).unwrap();
    }
    map.set_terrain(1, 1, Terrain::Airport).unwrap();
    let target_positions = [4, 6, 8, 10].map(|x| GridPosition { x, y: 1 });
    for target in target_positions {
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
    }
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let candidate_islands: HashSet<_> = target_positions
        .iter()
        .map(|position| island_map.get_island_at(position).unwrap().id)
        .collect();
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((
        GridPosition { x: 1, y: 1 },
        Property::new(Terrain::Airport, Some(player), 100),
    ));
    for target in target_positions {
        world.spawn((target, Property::new(Terrain::City, None, 100)));
    }

    let helicopter_stats = master_data
        .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
        .unwrap();
    for index in 0..4 {
        world.spawn((
            player,
            Faction(player),
            GridPosition { x: index % 3, y: 1 },
            helicopter_stats.clone(),
            CargoCapacity {
                max: helicopter_stats.max_cargo,
                loaded: Vec::new(),
            },
            Health {
                current: 100,
                max: 100,
            },
        ));
    }
    let infantry_stats = master_data
        .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
        .unwrap();
    for index in 0..8 {
        world.spawn((
            player,
            Faction(player),
            GridPosition { x: index % 3, y: 1 },
            infantry_stats.clone(),
            Health {
                current: 100,
                max: 100,
            },
        ));
    }

    let portfolio = crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
    assert_eq!(portfolio.active_offensives.len(), 3);
    assert!(
        portfolio
            .active_offensives
            .iter()
            .all(|assignment| assignment.operation_ready)
    );
    let selected_islands: HashSet<_> = portfolio
        .active_offensives
        .iter()
        .map(|assignment| assignment.island_id)
        .collect();
    assert_eq!(selected_islands.len(), 3);
    assert_eq!(candidate_islands.difference(&selected_islands).count(), 1);

    plan_squads(&mut world, player);

    let manager = world.resource::<SquadManager>();
    let offensive_squads: Vec<_> = manager
        .squads
        .iter()
        .filter(|squad| {
            squad.mission_type == MissionType::Transport
                && squad
                    .target_island
                    .is_some_and(|island| selected_islands.contains(&island))
        })
        .collect();
    let executed_islands: HashSet<_> = offensive_squads
        .iter()
        .filter_map(|squad| squad.target_island)
        .collect();
    assert_eq!(executed_islands, selected_islands);
    assert!(manager.squads.iter().all(|squad| {
        squad.target_island.is_none_or(|island| {
            !candidate_islands.contains(&island) || selected_islands.contains(&island)
        })
    }));

    let mut entity_owners: HashMap<Entity, crate::ai::islands::IslandId> = HashMap::new();
    for assignment in &portfolio.active_offensives {
        let operation_squads: Vec<_> = offensive_squads
            .iter()
            .filter(|squad| squad.target_island == Some(assignment.island_id))
            .collect();
        assert!(!operation_squads.is_empty());
        let mut actual_entities = Vec::new();
        for squad in operation_squads {
            actual_entities.extend(squad.members.iter().copied());
            actual_entities.extend(squad.cargo_entities.iter().copied());
        }
        actual_entities.sort_by_key(|entity| entity.to_bits());
        let unique: HashSet<_> = actual_entities.iter().copied().collect();
        assert_eq!(actual_entities.len(), unique.len());
        assert_eq!(actual_entities, sorted_assignment_entities(assignment));
        assert_eq!(assignment.transport_entities.len(), 1);
        assert_eq!(assignment.capture_entities.len(), 2);
        for entity in unique {
            assert_eq!(entity_owners.insert(entity, assignment.island_id), None);
        }
    }
    let mut snapshot: Vec<_> = offensive_squads
        .iter()
        .map(|squad| {
            (
                squad.id,
                squad.transport_entity,
                squad.members.clone(),
                squad.cargo_entities.clone(),
                squad.target_island,
                squad.target,
                squad.phase.clone(),
            )
        })
        .collect();
    snapshot.sort_by_key(|entry| entry.0.0);

    plan_squads(&mut world, player);
    let manager = world.resource::<SquadManager>();
    let mut repeated: Vec<_> = manager
        .squads
        .iter()
        .filter(|squad| {
            squad.mission_type == MissionType::Transport
                && squad
                    .target_island
                    .is_some_and(|island| selected_islands.contains(&island))
        })
        .map(|squad| {
            (
                squad.id,
                squad.transport_entity,
                squad.members.clone(),
                squad.cargo_entities.clone(),
                squad.target_island,
                squad.target,
                squad.phase.clone(),
            )
        })
        .collect();
    repeated.sort_by_key(|entry| entry.0.0);
    assert_eq!(repeated, snapshot);
}

#[test]
fn purchase_only_assignment_keeps_owner_placeholder_until_assignment_is_removed() {
    let (mut world, _, player_a) = empty_v3_world();
    let player_b = PlayerId(2);
    world
        .resource_mut::<PlayerAiSettings>()
        .set_version(player_b, AiVersion::V3);
    for player in &mut world.resource_mut::<Players>().0 {
        player.funds = if [player_a, player_b].contains(&player.id) {
            6_000
        } else {
            0
        };
    }

    let base_a = GridPosition { x: 0, y: 0 };
    let base_b = GridPosition { x: 2, y: 0 };
    let target = GridPosition { x: 4, y: 0 };
    let stale_target = GridPosition { x: 6, y: 0 };
    let mut map = Map::new(7, 1, Terrain::Sea, GridTopology::Square);
    map.set_terrain(base_a.x, base_a.y, Terrain::Airport)
        .unwrap();
    map.set_terrain(base_b.x, base_b.y, Terrain::Airport)
        .unwrap();
    map.set_terrain(target.x, target.y, Terrain::City).unwrap();
    map.set_terrain(stale_target.x, stale_target.y, Terrain::City)
        .unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let target_island = island_map.get_island_at(&target).unwrap().id;
    let stale_island = island_map.get_island_at(&stale_target).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((base_a, Property::new(Terrain::Airport, Some(player_a), 100)));
    world.spawn((base_b, Property::new(Terrain::Airport, Some(player_b), 100)));
    world.spawn((target, Property::new(Terrain::City, None, 100)));
    world.spawn((
        stale_target,
        Property::new(Terrain::City, Some(player_a), 100),
    ));

    let stale_id = {
        let mut manager = world.remove_resource::<SquadManager>().unwrap();
        let stale = manager.create_squad(MissionType::Transport);
        stale.target_island = Some(stale_island);
        stale.target = Some(stale_target);
        stale.phase = MissionPhase::Forming;
        let id = stale.id;
        world.insert_resource(manager);
        id
    };

    let assignment_a = crate::ai::strategy::analyze_strategy(&mut world, player_a)
        .campaign_portfolio
        .assignment_for(target_island)
        .expect("player A must fund a purchase-only Expand assignment")
        .clone();
    assert!(!assignment_a.operation_ready);
    assert!(assignment_a.transport_entities.is_empty());
    assert!(assignment_a.capture_entities.is_empty());

    plan_squads(&mut world, player_a);
    let snapshot_a = {
        let manager = world.resource::<SquadManager>();
        assert!(
            manager.squads.iter().all(|squad| squad.id != stale_id),
            "ownerless empty Forming Squad must not be preserved as a campaign placeholder"
        );
        let placeholder = manager
            .squads
            .iter()
            .find(|squad| {
                squad.owner_id == Some(player_a)
                    && squad.target_island == Some(target_island)
                    && squad.mission_type == MissionType::Transport
            })
            .expect("player A purchase-only assignment must create a placeholder");
        assert_eq!(placeholder.phase, MissionPhase::Forming);
        assert_eq!(placeholder.target, Some(assignment_a.target_position));
        assert!(placeholder.members.is_empty());
        assert!(placeholder.transport_entity.is_none());
        assert!(placeholder.cargo_entities.is_empty());
        assert!(placeholder.delivered_cargo.is_empty());
        (
            placeholder.id,
            placeholder.owner_id,
            placeholder.mission_type.clone(),
            placeholder.target_island,
            placeholder.target,
            placeholder.phase.clone(),
            placeholder.members.clone(),
            placeholder.transport_entity,
            placeholder.cargo_entities.clone(),
            placeholder.delivered_cargo.clone(),
        )
    };

    plan_squads(&mut world, player_b);
    let player_b_id = {
        let manager = world.resource::<SquadManager>();
        let placeholder = manager
            .squads
            .iter()
            .find(|squad| {
                squad.owner_id == Some(player_b)
                    && squad.target_island == Some(target_island)
                    && squad.mission_type == MissionType::Transport
            })
            .expect("player B must keep a separate purchase-only placeholder");
        assert_ne!(placeholder.id, snapshot_a.0);
        placeholder.id
    };

    for _ in 0..2 {
        plan_squads(&mut world, player_a);
        let manager = world.resource::<SquadManager>();
        let placeholder = manager
            .squads
            .iter()
            .find(|squad| {
                squad.owner_id == Some(player_a) && squad.target_island == Some(target_island)
            })
            .expect("repeated full planning must retain player A placeholder");
        assert_eq!(
            (
                placeholder.id,
                placeholder.owner_id,
                placeholder.mission_type.clone(),
                placeholder.target_island,
                placeholder.target,
                placeholder.phase.clone(),
                placeholder.members.clone(),
                placeholder.transport_entity,
                placeholder.cargo_entities.clone(),
                placeholder.delivered_cargo.clone(),
            ),
            snapshot_a
        );
        assert!(manager.squads.iter().any(|squad| squad.id == player_b_id));
    }

    for player in &mut world.resource_mut::<Players>().0 {
        if player.id == player_a {
            player.funds = 0;
        }
    }
    assert!(
        crate::ai::strategy::analyze_strategy(&mut world, player_a)
            .campaign_portfolio
            .assignment_for(target_island)
            .is_none()
    );
    plan_squads(&mut world, player_a);
    let manager = world.resource::<SquadManager>();
    assert!(manager.squads.iter().all(|squad| squad.id != snapshot_a.0));
    assert!(manager.squads.iter().any(|squad| squad.id == player_b_id));
}

#[test]
fn solo_fallback_stays_out_of_campaign_until_normal_recovery() {
    let (mut world, master_data, player) = empty_v3_world();
    let origin = GridPosition { x: 0, y: 0 };
    let target = GridPosition { x: 2, y: 0 };
    let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
    map.set_terrain(origin.x, origin.y, Terrain::Airport)
        .unwrap();
    map.set_terrain(target.x, target.y, Terrain::City).unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let target_island = island_map.get_island_at(&target).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((origin, Property::new(Terrain::Airport, Some(player), 100)));
    world.spawn((target, Property::new(Terrain::City, None, 100)));

    let fallback = spawn_master_unit(&mut world, &master_data, player, origin, UnitType::Infantry);
    world.get_mut::<Health>(fallback).unwrap().current = 50;
    let other_capture =
        spawn_master_unit(&mut world, &master_data, player, origin, UnitType::Infantry);
    let transport_stats = master_data
        .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
        .unwrap();
    let transport = world
        .spawn((
            player,
            Faction(player),
            origin,
            transport_stats.clone(),
            crate::components::Fuel {
                current: transport_stats.max_fuel,
                max: transport_stats.max_fuel,
            },
            CargoCapacity {
                max: transport_stats.max_cargo,
                loaded: Vec::new(),
            },
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();

    plan_squads(&mut world, player);
    {
        let portfolio =
            crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
        assert!(portfolio.assignment_for(target_island).is_none());
        let manager = world.resource::<SquadManager>();
        assert!(manager.solo_fallbacks.contains(&fallback));
        assert!(manager.squads.iter().all(|squad| {
            squad.transport_entity != Some(fallback)
                && !squad.members.contains(&fallback)
                && !squad.cargo_entities.contains(&fallback)
                && !squad.delivered_cargo.contains(&fallback)
        }));
    }

    world.get_mut::<Health>(fallback).unwrap().current = 100;
    plan_squads(&mut world, player);
    let manager = world.resource::<SquadManager>();
    assert!(!manager.solo_fallbacks.contains(&fallback));
    let campaign = manager
        .squads
        .iter()
        .find(|squad| squad.transport_entity == Some(transport))
        .expect("recovered capture must re-enter the complete campaign package");
    assert!(campaign.cargo_entities.contains(&fallback));
    assert!(campaign.cargo_entities.contains(&other_capture));
}

fn spawn_master_unit(
    world: &mut World,
    master_data: &MasterDataRegistry,
    player: PlayerId,
    position: GridPosition,
    unit_type: UnitType,
) -> Entity {
    let stats = master_data
        .create_unit_stats(&UnitName(unit_type.as_str().to_owned()))
        .unwrap();
    let ammo = crate::components::Ammo {
        ammo1: stats.max_ammo1,
        max_ammo1: stats.max_ammo1,
        ammo2: stats.max_ammo2,
        max_ammo2: stats.max_ammo2,
    };
    world
        .spawn((
            player,
            Faction(player),
            position,
            stats,
            ammo,
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id()
}

#[test]
fn secure_keeps_local_capture_unit_on_nearest_unowned_property() {
    let (mut world, master_data, player) = empty_v3_world();
    let friendly = GridPosition { x: 1, y: 1 };
    let neutral = GridPosition { x: 2, y: 1 };
    let mut map = Map::new(4, 3, Terrain::Sea, GridTopology::Square);
    map.set_terrain(friendly.x, friendly.y, Terrain::City)
        .unwrap();
    map.set_terrain(neutral.x, neutral.y, Terrain::City)
        .unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let island = island_map.get_island_at(&friendly).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((friendly, Property::new(Terrain::City, Some(player), 100)));
    world.spawn((neutral, Property::new(Terrain::City, None, 100)));
    let infantry = spawn_master_unit(
        &mut world,
        &master_data,
        player,
        friendly,
        UnitType::Infantry,
    );

    let portfolio = crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
    let assessment = portfolio
        .islands
        .iter()
        .find(|assessment| assessment.island_id == island)
        .unwrap();
    assert_eq!(
        assessment.decision,
        crate::ai::island_campaign::IslandCampaignDecision::Secure
    );

    plan_squads(&mut world, player);

    let manager = world.resource::<SquadManager>();
    let capture = manager
        .squads
        .iter()
        .find(|squad| squad.members.contains(&infantry))
        .expect("Secure must retain a local capture responsibility");
    assert_eq!(capture.mission_type, MissionType::Capture);
    assert_eq!(capture.target_island, Some(island));
    assert_eq!(capture.target, Some(neutral));
    assert!(matches!(
        capture.phase,
        MissionPhase::Forming | MissionPhase::MovingToTarget | MissionPhase::Executing
    ));
}

#[test]
fn secure_protects_its_sole_local_capture_from_an_earlier_other_island_target() {
    let (mut world, master_data, player) = empty_v3_world();
    let other_owned = GridPosition { x: 9, y: 1 };
    let other_unowned = GridPosition { x: 10, y: 1 };
    let secure_owned = GridPosition { x: 12, y: 1 };
    let secure_bridge = GridPosition { x: 13, y: 1 };
    let secure_unowned = GridPosition { x: 14, y: 1 };
    let mut map = Map::new(15, 3, Terrain::Sea, GridTopology::Square);
    map.set_terrain(other_owned.x, other_owned.y, Terrain::Capital)
        .unwrap();
    map.set_terrain(other_unowned.x, other_unowned.y, Terrain::City)
        .unwrap();
    map.set_terrain(secure_owned.x, secure_owned.y, Terrain::City)
        .unwrap();
    map.set_terrain(secure_bridge.x, secure_bridge.y, Terrain::Plains)
        .unwrap();
    map.set_terrain(secure_unowned.x, secure_unowned.y, Terrain::City)
        .unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let secure_island = island_map.get_island_at(&secure_owned).unwrap().id;
    let other_island = island_map.get_island_at(&other_owned).unwrap().id;
    assert_ne!(secure_island, other_island);
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((
        other_owned,
        Property::new(Terrain::Capital, Some(player), 100),
    ));
    world.spawn((other_unowned, Property::new(Terrain::City, None, 100)));
    world.spawn((
        secure_owned,
        Property::new(Terrain::City, Some(player), 100),
    ));
    world.spawn((secure_unowned, Property::new(Terrain::City, None, 100)));
    let capture = spawn_master_unit(
        &mut world,
        &master_data,
        player,
        secure_owned,
        UnitType::Infantry,
    );

    let portfolio = crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
    assert!(portfolio.islands.iter().any(|assessment| {
        assessment.island_id == secure_island
            && assessment.decision == crate::ai::island_campaign::IslandCampaignDecision::Secure
    }));

    plan_squads(&mut world, player);
    let snapshot = {
        let manager = world.resource::<SquadManager>();
        let squad = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&capture))
            .expect("Secure local capture Entity must remain assigned");
        assert_eq!(squad.mission_type, MissionType::Capture);
        assert_eq!(squad.target_island, Some(secure_island));
        assert_eq!(squad.target, Some(secure_unowned));
        (
            squad.id,
            squad.target_island,
            squad.target,
            squad.members.clone(),
        )
    };

    plan_squads(&mut world, player);
    let manager = world.resource::<SquadManager>();
    let repeated = manager
        .squads
        .iter()
        .find(|squad| squad.members.contains(&capture))
        .map(|squad| {
            (
                squad.id,
                squad.target_island,
                squad.target,
                squad.members.clone(),
            )
        })
        .unwrap();
    assert_eq!(repeated, snapshot);
}

#[test]
fn secure_does_not_assign_a_disconnected_capture_unit() {
    let (mut world, _, player) = empty_v3_world();
    let owned_port = GridPosition { x: 1, y: 1 };
    let neutral_city = GridPosition { x: 2, y: 1 };
    let mut map = Map::new(4, 3, Terrain::Sea, GridTopology::Square);
    map.set_terrain(owned_port.x, owned_port.y, Terrain::Port)
        .unwrap();
    map.set_terrain(neutral_city.x, neutral_city.y, Terrain::City)
        .unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let island = island_map.get_island_at(&owned_port).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((owned_port, Property::new(Terrain::Port, Some(player), 100)));
    world.spawn((neutral_city, Property::new(Terrain::City, None, 100)));
    let disconnected = world
        .spawn((
            player,
            Faction(player),
            owned_port,
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Ship,
                max_movement: 5,
                can_capture: true,
                cost: 1_000,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();

    let portfolio = crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
    assert!(portfolio.islands.iter().any(|assessment| {
        assessment.island_id == island
            && assessment.decision == crate::ai::island_campaign::IslandCampaignDecision::Secure
    }));

    plan_squads(&mut world, player);
    let manager = world.resource::<SquadManager>();
    assert!(manager.squads.iter().all(|squad| {
        !squad.members.contains(&disconnected) || squad.mission_type != MissionType::Capture
    }));
}

#[test]
fn contest_preserves_capture_while_combat_intercepts_capture_threat() {
    let (mut world, master_data, player) = empty_v3_world();
    let opponent = PlayerId(2);
    let home = GridPosition { x: 0, y: 0 };
    let neutral = GridPosition { x: 1, y: 1 };
    let staging = GridPosition { x: 2, y: 1 };
    let mut map = Map::new(4, 3, Terrain::Sea, GridTopology::Square);
    // Contest責務は複数の占領対象陸塊がある島嶼マップでのみ有効であることを明示する。
    map.set_terrain(home.x, home.y, Terrain::City).unwrap();
    map.set_terrain(neutral.x, neutral.y, Terrain::City)
        .unwrap();
    map.set_terrain(staging.x, staging.y, Terrain::Plains)
        .unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let island = island_map.get_island_at(&neutral).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((home, Property::new(Terrain::City, Some(player), 100)));
    world.spawn((neutral, Property::new(Terrain::City, None, 100)));
    let capture = spawn_master_unit(
        &mut world,
        &master_data,
        player,
        staging,
        UnitType::Infantry,
    );
    let combat = spawn_master_unit(&mut world, &master_data, player, staging, UnitType::Tank);
    let enemy = spawn_master_unit(
        &mut world,
        &master_data,
        opponent,
        neutral,
        UnitType::Infantry,
    );

    let portfolio = crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
    let assignment = portfolio.assignment_for(island).unwrap();
    assert_eq!(
        assignment.decision,
        crate::ai::island_campaign::IslandCampaignDecision::Contest
    );
    assert!(assignment.capture_entities.contains(&capture));
    assert!(assignment.combat_entities.contains(&combat));

    plan_squads(&mut world, player);

    let manager = world.resource::<SquadManager>();
    let capture_squad = manager
        .squads
        .iter()
        .find(|squad| squad.members.contains(&capture))
        .expect("Contest capture Entity must keep a capture duty");
    assert_eq!(capture_squad.mission_type, MissionType::Capture);
    assert_eq!(capture_squad.target_island, Some(island));
    assert_eq!(capture_squad.target, Some(neutral));

    let interception_squad = manager
        .squads
        .iter()
        .find(|squad| squad.members.contains(&combat))
        .expect("Contest combat Entity must intercept the immediate capture threat");
    assert!(matches!(
        interception_squad.mission_type,
        MissionType::Interception(_)
    ));
    assert_eq!(interception_squad.target_island, None);
    assert_eq!(
        interception_squad.target,
        world.get::<GridPosition>(enemy).copied()
    );
    assert!(!interception_squad.members.contains(&capture));
    assert!(!capture_squad.members.contains(&combat));
}

#[test]
fn defend_uses_reserved_combat_entities_and_assignment_target() {
    let (mut world, master_data, player) = empty_v3_world();
    let opponent = PlayerId(2);
    let defended = GridPosition { x: 1, y: 1 };
    let enemy_position = GridPosition { x: 3, y: 1 };
    let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
    map.set_terrain(defended.x, defended.y, Terrain::City)
        .unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let island = island_map.get_island_at(&defended).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((defended, Property::new(Terrain::City, Some(player), 100)));
    let tank_a = spawn_master_unit(&mut world, &master_data, player, defended, UnitType::Tank);
    let tank_b = spawn_master_unit(&mut world, &master_data, player, defended, UnitType::Tank);
    spawn_master_unit(
        &mut world,
        &master_data,
        opponent,
        enemy_position,
        UnitType::Bcopters,
    );
    {
        let mut manager = world.remove_resource::<SquadManager>().unwrap();
        let squad = manager.create_squad(MissionType::Defense);
        squad.members.insert(tank_a);
        squad.target_island = Some(island);
        squad.target = Some(defended);
        squad.phase = MissionPhase::MovingToTarget;
        world.insert_resource(manager);
    }

    let portfolio = crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
    let defense = portfolio
        .defenses
        .iter()
        .find(|assignment| assignment.island_id == island)
        .expect("threatened island must receive a defense assignment");
    assert_eq!(
        defense.decision,
        crate::ai::island_campaign::IslandCampaignDecision::Defend
    );
    assert!(defense.operation_ready);
    assert!(!defense.combat_entities.is_empty());
    assert!(
        defense
            .combat_entities
            .iter()
            .all(|entity| [tank_a, tank_b].contains(entity))
    );
    let expected_target = defense.target_position;
    let expected_members: BTreeSet<_> = defense.combat_entities.iter().copied().collect();

    plan_squads(&mut world, player);

    let manager = world.resource::<SquadManager>();
    let squad = manager
        .squads
        .iter()
        .find(|squad| {
            squad.mission_type == MissionType::Defense && squad.target_island == Some(island)
        })
        .expect("Defend assignment must create a Defense Squad");
    assert_eq!(squad.target, Some(expected_target));
    assert_eq!(squad.members, expected_members);
}

#[test]
fn reinforce_keeps_other_operations_and_stranded_capture_continuity() {
    let (mut world, master_data, player) = empty_v3_world();
    let opponent = PlayerId(2);
    let base = GridPosition { x: 0, y: 1 };
    let expand_positions = [GridPosition { x: 3, y: 1 }, GridPosition { x: 5, y: 1 }];
    let contested_property = GridPosition { x: 7, y: 1 };
    let contested_staging = GridPosition { x: 8, y: 1 };
    let mut map = Map::new(9, 3, Terrain::Sea, GridTopology::Square);
    map.set_terrain(base.x, base.y, Terrain::Airport).unwrap();
    for target in expand_positions {
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
    }
    map.set_terrain(contested_property.x, contested_property.y, Terrain::City)
        .unwrap();
    map.set_terrain(contested_staging.x, contested_staging.y, Terrain::City)
        .unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let expand_islands =
        expand_positions.map(|target| island_map.get_island_at(&target).unwrap().id);
    let contested_island = island_map.get_island_at(&contested_property).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((base, Property::new(Terrain::Airport, Some(player), 100)));
    for target in expand_positions {
        world.spawn((target, Property::new(Terrain::City, None, 100)));
    }
    world.spawn((contested_property, Property::new(Terrain::City, None, 100)));
    world.spawn((
        contested_staging,
        Property::new(Terrain::City, Some(player), 100),
    ));

    let helicopter_stats = master_data
        .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
        .unwrap();
    let mut manager = world.remove_resource::<SquadManager>().unwrap();
    let mut operation_snapshots = Vec::new();
    for (index, island) in expand_islands.into_iter().enumerate() {
        let transport = world
            .spawn((
                player,
                Faction(player),
                base,
                helicopter_stats.clone(),
                CargoCapacity {
                    max: helicopter_stats.max_cargo,
                    loaded: Vec::new(),
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let cargo = [
            spawn_master_unit(
                &mut world,
                &master_data,
                player,
                GridPosition { x: 9_999, y: 9_999 },
                UnitType::Infantry,
            ),
            spawn_master_unit(
                &mut world,
                &master_data,
                player,
                GridPosition { x: 9_999, y: 9_999 },
                UnitType::Infantry,
            ),
        ];
        world.get_mut::<CargoCapacity>(transport).unwrap().loaded = cargo.to_vec();
        for entity in cargo {
            world.entity_mut(entity).insert(Transporting(transport));
        }
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities = cargo.to_vec();
        squad.target_island = Some(island);
        squad.target = Some(expand_positions[index]);
        squad.phase = MissionPhase::Transport(TransportPhase::Transit);
        operation_snapshots.push((
            squad.id,
            transport,
            squad.cargo_entities.clone(),
            island,
            squad.target,
            squad.phase.clone(),
        ));
    }

    let recoverable_transport = world
        .spawn((
            player,
            Faction(player),
            base,
            helicopter_stats.clone(),
            CargoCapacity {
                max: helicopter_stats.max_cargo,
                loaded: Vec::new(),
            },
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    let recoverable = manager.create_squad(MissionType::Transport);
    recoverable.members.insert(recoverable_transport);
    recoverable.transport_entity = Some(recoverable_transport);
    recoverable.target_island = Some(contested_island);
    recoverable.target = Some(contested_property);
    recoverable.phase = MissionPhase::Forming;
    let _recoverable_id = recoverable.id;
    let remote_free_capture =
        spawn_master_unit(&mut world, &master_data, player, base, UnitType::Infantry);

    let stranded_capture = spawn_master_unit(
        &mut world,
        &master_data,
        player,
        contested_staging,
        UnitType::Infantry,
    );
    spawn_master_unit(
        &mut world,
        &master_data,
        opponent,
        contested_property,
        UnitType::Tank,
    );
    let local = manager.create_squad(MissionType::Capture);
    local.members.insert(stranded_capture);
    local.target_island = Some(contested_island);
    local.target = Some(contested_property);
    local.phase = MissionPhase::MovingToTarget;
    let local_id = local.id;
    world.insert_resource(manager);

    let portfolio = crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
    let withdrawn = portfolio
        .islands
        .iter()
        .find(|assessment| assessment.island_id == contested_island)
        .unwrap();
    assert_eq!(
        withdrawn.decision,
        crate::ai::island_campaign::IslandCampaignDecision::Reinforce
    );
    assert!(portfolio.assignment_for(contested_island).is_some());
    assert_eq!(portfolio.active_offensives.len(), 3);

    plan_squads(&mut world, player);

    let manager = world.resource::<SquadManager>();
    for (id, transport, cargo, island, target, phase) in operation_snapshots {
        let squad = manager
            .squads
            .iter()
            .find(|squad| squad.id == id)
            .expect("unaffected operation must keep its Squad ID");
        assert_eq!(squad.transport_entity, Some(transport));
        assert_eq!(squad.cargo_entities, cargo);
        assert_eq!(squad.target_island, Some(island));
        assert_eq!(squad.target, target);
        assert_eq!(squad.phase, phase);
    }
    let local = manager
        .squads
        .iter()
        .find(|squad| squad.id == local_id)
        .expect("stranded capture duty must continue locally");
    assert_eq!(local.mission_type, MissionType::Capture);
    assert_eq!(local.target_island, Some(contested_island));
    assert_eq!(local.target, Some(contested_property));
    assert!(local.members.contains(&stranded_capture));
    assert!(manager.squads.iter().any(|squad| {
        squad.mission_type == MissionType::Transport
            && squad.target_island == Some(contested_island)
            && (squad.cargo_entities.contains(&stranded_capture)
                || squad.cargo_entities.contains(&remote_free_capture))
    }));
}

fn setup_remote_reinforce_world(
    with_transport: bool,
) -> (
    World,
    PlayerId,
    crate::ai::islands::IslandId,
    Entity,
    Option<Entity>,
) {
    let (mut world, master_data, player) = empty_v3_world();
    let opponent = PlayerId(2);
    let port = GridPosition { x: 0, y: 1 };
    let base = GridPosition { x: 1, y: 1 };
    let target = GridPosition { x: 4, y: 1 };
    let target_staging = GridPosition { x: 5, y: 1 };
    let mut map = Map::new(6, 3, Terrain::Sea, GridTopology::Square);
    map.set_terrain(port.x, port.y, Terrain::Port).unwrap();
    map.set_terrain(base.x, base.y, Terrain::Capital).unwrap();
    map.set_terrain(target.x, target.y, Terrain::City).unwrap();
    map.set_terrain(target_staging.x, target_staging.y, Terrain::Plains)
        .unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let target_island = island_map.get_island_at(&target).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((base, Property::new(Terrain::Capital, Some(player), 100)));
    world.spawn((target, Property::new(Terrain::City, None, 100)));
    spawn_master_unit(
        &mut world,
        &master_data,
        player,
        target_staging,
        UnitType::Infantry,
    );
    spawn_master_unit(&mut world, &master_data, opponent, target, UnitType::Tank);
    let remote_combat = spawn_master_unit(&mut world, &master_data, player, base, UnitType::MdTank);
    let transport = with_transport.then(|| {
        let stats = master_data
            .create_unit_stats(&UnitName(UnitType::Lander.as_str().to_owned()))
            .unwrap();
        world
            .spawn((
                player,
                Faction(player),
                port,
                stats.clone(),
                CargoCapacity {
                    max: stats.max_cargo,
                    loaded: Vec::new(),
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id()
    });
    (world, player, target_island, remote_combat, transport)
}

#[test]
fn offshore_reinforce_without_owned_transport_is_not_ready_and_idle() {
    let (mut world, player, target_island, remote_combat, _) = setup_remote_reinforce_world(false);

    let portfolio = crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
    let invalid_ready = portfolio
        .assignment_for(target_island)
        .is_some_and(|assignment| {
            assignment.decision == crate::ai::island_campaign::IslandCampaignDecision::Reinforce
                && assignment.operation_ready
                && assignment.transport_entities.is_empty()
                && assignment.combat_entities.contains(&remote_combat)
        });
    assert!(
        !invalid_ready,
        "remote Reinforce must not reserve ready cargo without an owned transport"
    );

    plan_squads(&mut world, player);
    let manager = world.resource::<SquadManager>();
    assert!(manager.squads.iter().all(|squad| {
        !squad.cargo_entities.contains(&remote_combat)
            || squad.mission_type == MissionType::Transport
    }));
}

#[test]
fn offshore_reinforce_with_complete_owned_transport_executes_its_assignment() {
    let (mut world, player, target_island, remote_combat, transport) =
        setup_remote_reinforce_world(true);
    let transport = transport.unwrap();

    let assignment = crate::ai::strategy::analyze_strategy(&mut world, player)
        .campaign_portfolio
        .assignment_for(target_island)
        .expect("complete offshore Reinforce must be allocated")
        .clone();
    assert_eq!(
        assignment.decision,
        crate::ai::island_campaign::IslandCampaignDecision::Reinforce
    );
    assert!(assignment.operation_ready);
    assert!(assignment.transport_entities.contains(&transport));
    assert!(assignment.combat_entities.contains(&remote_combat));

    plan_squads(&mut world, player);
    let manager = world.resource::<SquadManager>();
    let squad = manager
        .squads
        .iter()
        .find(|squad| squad.transport_entity == Some(transport))
        .expect("reserved Reinforce transport must receive a Transport Squad");
    assert_eq!(squad.target_island, Some(target_island));
    assert_eq!(squad.target, Some(assignment.target_position));
    assert!(squad.cargo_entities.contains(&remote_combat));
    assert!(matches!(
        squad.phase,
        MissionPhase::Transport(TransportPhase::Pickup | TransportPhase::Transit)
    ));
}

#[test]
fn partially_loaded_assignment_owned_reinforce_transport_remains_available_for_its_package() {
    let (mut world, player, target_island, remote_combat, transport) =
        setup_remote_reinforce_world(true);
    let transport = transport.unwrap();
    world.get_mut::<CargoCapacity>(transport).unwrap().loaded = vec![remote_combat];
    world
        .entity_mut(remote_combat)
        .insert((GridPosition { x: 9_999, y: 9_999 }, Transporting(transport)));
    let target = world
        .resource::<crate::ai::islands::IslandMap>()
        .islands
        .iter()
        .find(|island| island.id == target_island)
        .and_then(|island| {
            island
                .tiles
                .iter()
                .min_by_key(|position| (position.y, position.x))
        })
        .copied()
        .unwrap();
    let mut manager = world.remove_resource::<SquadManager>().unwrap();
    let transit = manager.create_squad(MissionType::Transport);
    transit.members.insert(transport);
    transit.transport_entity = Some(transport);
    transit.cargo_entities = vec![remote_combat];
    transit.target_island = Some(target_island);
    transit.target = Some(target);
    transit.phase = MissionPhase::Transport(TransportPhase::Transit);
    world.insert_resource(manager);

    let first = crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
    let second = crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
    let assignment = first
        .assignment_for(target_island)
        .expect("associated partial load must continue its Reinforce package");
    assert_eq!(
        assignment.decision,
        crate::ai::island_campaign::IslandCampaignDecision::Reinforce
    );
    assert!(assignment.operation_ready);
    assert!(assignment.transport_entities.contains(&transport));
    assert!(assignment.combat_entities.contains(&remote_combat));
    assert_eq!(second, first);

    plan_squads(&mut world, player);
    let manager = world.resource::<SquadManager>();
    let continued = manager
        .squads
        .iter()
        .find(|squad| squad.transport_entity == Some(transport))
        .expect("associated loaded transport must remain in its original operation");
    assert_eq!(continued.target_island, Some(target_island));
    assert!(continued.cargo_entities.contains(&remote_combat));
    assert!(matches!(
        continued.phase,
        MissionPhase::Transport(TransportPhase::Transit | TransportPhase::Drop)
    ));
}

#[test]
fn full_planning_keeps_targetless_drop_deliveries_local_across_repeated_calls() {
    let (mut world, master_data, player) = empty_v3_world();
    let landing = GridPosition { x: 0, y: 0 };
    let neutral = GridPosition { x: 2, y: 0 };
    let enemy_position = GridPosition { x: 4, y: 0 };
    let mut map = Map::new(5, 1, Terrain::Sea, GridTopology::Square);
    map.set_terrain(landing.x, landing.y, Terrain::City)
        .unwrap();
    map.set_terrain(neutral.x, neutral.y, Terrain::City)
        .unwrap();
    map.set_terrain(enemy_position.x, enemy_position.y, Terrain::City)
        .unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let landing_island = island_map.get_island_at(&landing).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((landing, Property::new(Terrain::City, Some(player), 100)));
    world.spawn((neutral, Property::new(Terrain::City, None, 100)));
    world.spawn((
        enemy_position,
        Property::new(Terrain::City, Some(PlayerId(2)), 100),
    ));
    spawn_master_unit(
        &mut world,
        &master_data,
        PlayerId(2),
        enemy_position,
        UnitType::Tank,
    );

    let capture_a = spawn_master_unit(
        &mut world,
        &master_data,
        player,
        landing,
        UnitType::Infantry,
    );
    let capture_b = spawn_master_unit(
        &mut world,
        &master_data,
        player,
        landing,
        UnitType::Infantry,
    );
    let combat = spawn_master_unit(&mut world, &master_data, player, landing, UnitType::Tank);
    let lander_stats = master_data
        .create_unit_stats(&UnitName(UnitType::Lander.as_str().to_owned()))
        .unwrap();
    let safe_transport = world
        .spawn((
            player,
            Faction(player),
            landing,
            lander_stats.clone(),
            CargoCapacity {
                max: 3,
                loaded: Vec::new(),
            },
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    let helicopter_stats = master_data
        .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
        .unwrap();
    world.spawn((
        player,
        Faction(player),
        landing,
        helicopter_stats.clone(),
        CargoCapacity {
            max: helicopter_stats.max_cargo,
            loaded: Vec::new(),
        },
        Health {
            current: 100,
            max: 100,
        },
    ));
    let delivered = HashSet::from([capture_a, capture_b, combat]);
    let mut manager = world.remove_resource::<SquadManager>().unwrap();
    let safe_drop = manager.create_squad(MissionType::Transport);
    safe_drop.members.insert(safe_transport);
    safe_drop.transport_entity = Some(safe_transport);
    safe_drop.cargo_entities = vec![capture_a, capture_b, combat];
    safe_drop.target_island = None;
    safe_drop.target = None;
    safe_drop.phase = MissionPhase::Transport(TransportPhase::Drop);
    world.insert_resource(manager);

    for _ in 0..2 {
        plan_squads(&mut world, player);
        let manager = world.resource::<SquadManager>();
        for entity in &delivered {
            assert_eq!(world.get::<GridPosition>(*entity), Some(&landing));
            let holder = manager
                .squads
                .iter()
                .find(|squad| squad.delivered_cargo.contains(entity))
                .expect("targetless delivery must remain protected by Squad-derived local state");
            assert_eq!(holder.target_island, None);
            assert_eq!(holder.target, None);
            assert!(manager.squads.iter().all(|squad| {
                if squad.id == holder.id {
                    return true;
                }
                !squad.members.contains(entity)
                    && !squad.cargo_entities.contains(entity)
                    && !squad.delivered_cargo.contains(entity)
            }));
            assert!(manager.squads.iter().all(|squad| {
                !squad.members.contains(entity) || squad.target_island == Some(landing_island)
            }));
        }
    }
}

#[test]
fn missing_source_transport_keeps_targetless_delivery_in_a_local_hold() {
    let (mut world, master_data, player) = empty_v3_world();
    let landing = GridPosition { x: 0, y: 0 };
    let local_target = GridPosition { x: 1, y: 0 };
    let mut map = Map::new(2, 1, Terrain::Plains, GridTopology::Square);
    map.set_terrain(landing.x, landing.y, Terrain::City)
        .unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let landing_island = island_map.get_island_at(&landing).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((landing, Property::new(Terrain::City, Some(player), 100)));
    let held_cargo = spawn_master_unit(
        &mut world,
        &master_data,
        player,
        landing,
        UnitType::Infantry,
    );
    let missing_transport = world.spawn_empty().id();
    let source_id = {
        let mut manager = world.remove_resource::<SquadManager>().unwrap();
        let source = manager.create_owned_squad(MissionType::Transport, player);
        source.transport_entity = Some(missing_transport);
        source.members.insert(missing_transport);
        source.delivered_cargo = vec![held_cargo];
        source.phase = MissionPhase::Transport(TransportPhase::Return);
        let id = source.id;
        world.insert_resource(manager);
        id
    };
    world.despawn(missing_transport);

    let mut hold_id = None;
    for _ in 0..2 {
        plan_squads(&mut world, player);
        let manager = world.resource::<SquadManager>();
        let hold = manager
            .squads
            .iter()
            .find(|squad| squad.delivered_cargo.contains(&held_cargo))
            .expect("missing source transport must leave cargo in a transport-independent hold");
        assert_eq!(hold.owner_id, Some(player));
        assert_eq!(hold.mission_type, MissionType::Transport);
        assert_eq!(hold.phase, MissionPhase::Forming);
        assert_eq!(hold.transport_entity, None);
        assert!(hold.members.is_empty());
        assert_eq!(hold.target_island, None);
        assert_eq!(hold.target, None);
        assert_eq!(world.get::<GridPosition>(held_cargo), Some(&landing));
        assert_eq!(hold_id.get_or_insert(hold.id), &hold.id);
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|squad| {
                    squad.members.contains(&held_cargo)
                        || squad.cargo_entities.contains(&held_cargo)
                        || squad.delivered_cargo.contains(&held_cargo)
                })
                .count(),
            1
        );
    }
    assert!(hold_id.is_some());
    assert!(source_id.0 <= hold_id.unwrap().0);

    world.spawn((local_target, Property::new(Terrain::City, None, 100)));
    plan_squads(&mut world, player);
    let manager = world.resource::<SquadManager>();
    let local = manager
        .squads
        .iter()
        .find(|squad| squad.members.contains(&held_cargo))
        .expect("held cargo must accept a later reachable local duty");
    assert_eq!(local.mission_type, MissionType::Capture);
    assert_eq!(local.target_island, Some(landing_island));
    assert_eq!(local.target, Some(local_target));
    assert!(manager.squads.iter().all(|squad| {
        squad
            .delivered_cargo
            .iter()
            .all(|entity| *entity != held_cargo)
    }));
}

#[test]
fn targetless_delivery_releases_returned_transport_and_waits_for_local_duty() {
    let (mut world, master_data, player) = empty_v3_world();
    let base = GridPosition { x: 0, y: 0 };
    let landing = GridPosition { x: 2, y: 0 };
    let local_target = GridPosition { x: 3, y: 0 };
    let remote_target = GridPosition { x: 5, y: 0 };
    let mut map = Map::new(6, 1, Terrain::Sea, GridTopology::Square);
    map.set_terrain(base.x, base.y, Terrain::Airport).unwrap();
    map.set_terrain(landing.x, landing.y, Terrain::City)
        .unwrap();
    map.set_terrain(local_target.x, local_target.y, Terrain::Plains)
        .unwrap();
    map.set_terrain(remote_target.x, remote_target.y, Terrain::City)
        .unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let landing_island = island_map.get_island_at(&landing).unwrap().id;
    let remote_island = island_map.get_island_at(&remote_target).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((base, Property::new(Terrain::Airport, Some(player), 100)));
    world.spawn((landing, Property::new(Terrain::City, Some(player), 100)));
    world.spawn((remote_target, Property::new(Terrain::City, None, 100)));

    let held_cargo = spawn_master_unit(
        &mut world,
        &master_data,
        player,
        landing,
        UnitType::Infantry,
    );
    let remote_cargo = [
        spawn_master_unit(&mut world, &master_data, player, base, UnitType::Infantry),
        spawn_master_unit(&mut world, &master_data, player, base, UnitType::Infantry),
    ];
    let transport_stats = master_data
        .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
        .unwrap();
    let transport = world
        .spawn((
            player,
            Faction(player),
            landing,
            transport_stats.clone(),
            CargoCapacity {
                max: transport_stats.max_cargo,
                loaded: Vec::new(),
            },
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    let source_id = {
        let mut manager = world.remove_resource::<SquadManager>().unwrap();
        let source = manager.create_squad(MissionType::Transport);
        source.members.insert(transport);
        source.transport_entity = Some(transport);
        source.cargo_entities = vec![held_cargo];
        source.phase = MissionPhase::Transport(TransportPhase::Drop);
        let id = source.id;
        world.insert_resource(manager);
        id
    };

    // 目標無しDropでは現地責務が無いcargoを保護し、空輸送はReturnへ進める。
    plan_squads(&mut world, player);
    {
        let manager = world.resource::<SquadManager>();
        let source = manager
            .squads
            .iter()
            .find(|squad| squad.id == source_id)
            .expect("returning source transport must remain until reaching a base");
        assert_eq!(
            source.phase,
            MissionPhase::Transport(TransportPhase::Return)
        );
        assert_eq!(source.delivered_cargo, vec![held_cargo]);
    }

    // friendly baseへ帰還した物理輸送はholdから分離し、別の島作戦へ再利用できる。
    *world.get_mut::<GridPosition>(transport).unwrap() = base;
    plan_squads(&mut world, player);
    let (hold_id, remote_squad_id) = {
        let manager = world.resource::<SquadManager>();
        assert!(manager.squads.iter().all(|squad| squad.id != source_id));
        let hold = manager
            .squads
            .iter()
            .find(|squad| {
                squad.transport_entity.is_none()
                    && squad.target_island.is_none()
                    && squad.target.is_none()
                    && squad.delivered_cargo.contains(&held_cargo)
            })
            .expect("landed cargo must move to a transport-independent local hold");
        let reused = manager
            .squads
            .iter()
            .find(|squad| {
                squad.transport_entity == Some(transport)
                    && squad.target_island == Some(remote_island)
            })
            .expect("returned transport must be reusable by another campaign");
        assert!(
            remote_cargo
                .iter()
                .all(|cargo| reused.cargo_entities.contains(cargo))
        );
        assert!(!reused.cargo_entities.contains(&held_cargo));
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|squad| {
                    squad.transport_entity == Some(held_cargo)
                        || squad.members.contains(&held_cargo)
                        || squad.cargo_entities.contains(&held_cargo)
                        || squad.delivered_cargo.contains(&held_cargo)
                })
                .count(),
            1
        );
        (hold.id, reused.id)
    };

    // 繰り返し計画でもholdと新輸送作戦は安定し、cargoを遠隔責務へ流さない。
    plan_squads(&mut world, player);
    {
        let portfolio =
            crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
        assert!(
            portfolio
                .defenses
                .iter()
                .chain(portfolio.active_offensives.iter())
                .flat_map(sorted_assignment_entities)
                .all(|entity| entity != held_cargo)
        );
        let manager = world.resource::<SquadManager>();
        let hold = manager
            .squads
            .iter()
            .find(|squad| squad.id == hold_id)
            .expect("local hold identity must be stable across repeated planning");
        assert_eq!(hold.delivered_cargo, vec![held_cargo]);
        assert!(
            manager
                .squads
                .iter()
                .any(|squad| squad.id == remote_squad_id)
        );
        assert!(manager.squads.iter().all(|squad| {
            !squad.members.contains(&held_cargo) || squad.target_island == Some(landing_island)
        }));
    }

    // 後から到達可能な現地占領責務が生じたら、holdを一度だけ引き渡す。
    world.spawn((local_target, Property::new(Terrain::City, None, 100)));
    plan_squads(&mut world, player);
    let manager = world.resource::<SquadManager>();
    let local = manager
        .squads
        .iter()
        .find(|squad| squad.members.contains(&held_cargo))
        .expect("held cargo must take over a later reachable local duty");
    assert_eq!(local.mission_type, MissionType::Capture);
    assert_eq!(local.target_island, Some(landing_island));
    assert_eq!(local.target, Some(local_target));
    assert!(
        manager
            .squads
            .iter()
            .all(|squad| { squad.id != hold_id || !squad.delivered_cargo.contains(&held_cargo) })
    );
    assert_eq!(
        manager
            .squads
            .iter()
            .filter(|squad| {
                squad.transport_entity == Some(held_cargo)
                    || squad.members.contains(&held_cargo)
                    || squad.cargo_entities.contains(&held_cargo)
                    || squad.delivered_cargo.contains(&held_cargo)
            })
            .count(),
        1
    );
}

#[test]
fn explicit_squad_owner_conflicts_are_quarantined_for_both_players() {
    let (mut world, master_data, player_a) = empty_v3_world();
    let player_b = PlayerId(2);
    world
        .resource_mut::<PlayerAiSettings>()
        .set_version(player_b, AiVersion::V3);
    for player in &mut world.resource_mut::<Players>().0 {
        player.funds = if [player_a, player_b].contains(&player.id) {
            6_000
        } else {
            0
        };
    }
    let base_a = GridPosition { x: 0, y: 0 };
    let base_b = GridPosition { x: 2, y: 0 };
    let target = GridPosition { x: 4, y: 0 };
    let mut map = Map::new(5, 1, Terrain::Sea, GridTopology::Square);
    map.set_terrain(base_a.x, base_a.y, Terrain::Airport)
        .unwrap();
    map.set_terrain(base_b.x, base_b.y, Terrain::Airport)
        .unwrap();
    map.set_terrain(target.x, target.y, Terrain::City).unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((base_a, Property::new(Terrain::Airport, Some(player_a), 100)));
    world.spawn((base_b, Property::new(Terrain::Airport, Some(player_b), 100)));
    world.spawn((target, Property::new(Terrain::City, None, 100)));

    let conflict_a = spawn_master_unit(
        &mut world,
        &master_data,
        player_a,
        base_a,
        UnitType::Infantry,
    );
    let conflict_b = spawn_master_unit(
        &mut world,
        &master_data,
        player_b,
        base_b,
        UnitType::Infantry,
    );
    for (player, base) in [(player_a, base_a), (player_b, base_b)] {
        spawn_master_unit(&mut world, &master_data, player, base, UnitType::Infantry);
        spawn_master_unit(&mut world, &master_data, player, base, UnitType::Infantry);
        let transport_stats = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        world.spawn((
            player,
            Faction(player),
            base,
            transport_stats.clone(),
            CargoCapacity {
                max: transport_stats.max_cargo,
                loaded: Vec::new(),
            },
            Health {
                current: 100,
                max: 100,
            },
        ));
    }

    let snapshots = {
        let mut manager = world.remove_resource::<SquadManager>().unwrap();
        let squad_a = manager.create_owned_squad(MissionType::Capture, player_b);
        squad_a.members.insert(conflict_a);
        squad_a.target_island = None;
        squad_a.target = None;
        squad_a.phase = MissionPhase::MovingToTarget;
        let id_a = squad_a.id;
        let squad_b = manager.create_owned_squad(MissionType::Capture, player_a);
        squad_b.members.insert(conflict_b);
        squad_b.target_island = None;
        squad_b.target = None;
        squad_b.phase = MissionPhase::MovingToTarget;
        let id_b = squad_b.id;
        let snapshots: Vec<_> = manager
            .squads
            .iter()
            .map(|squad| {
                (
                    squad.id,
                    squad.owner_id,
                    squad.mission_type.clone(),
                    squad.members.clone(),
                    squad.target_island,
                    squad.target,
                    squad.phase.clone(),
                )
            })
            .collect();
        world.insert_resource(manager);
        assert_ne!(id_a, id_b);
        snapshots
    };

    for (player, conflict) in [(player_a, conflict_a), (player_b, conflict_b)] {
        let portfolio =
            crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
        assert!(
            portfolio
                .defenses
                .iter()
                .chain(portfolio.active_offensives.iter())
                .flat_map(sorted_assignment_entities)
                .all(|entity| entity != conflict_a && entity != conflict_b),
            "explicit owner/Faction conflicts must not enter any portfolio"
        );

        plan_squads(&mut world, player);
        let manager = world.resource::<SquadManager>();
        let current: Vec<_> = manager
            .squads
            .iter()
            .filter(|squad| snapshots.iter().any(|snapshot| snapshot.0 == squad.id))
            .map(|squad| {
                (
                    squad.id,
                    squad.owner_id,
                    squad.mission_type.clone(),
                    squad.members.clone(),
                    squad.target_island,
                    squad.target,
                    squad.phase.clone(),
                )
            })
            .collect();
        assert_eq!(current, snapshots);
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|squad| {
                    squad.transport_entity == Some(conflict)
                        || squad.members.contains(&conflict)
                        || squad.cargo_entities.contains(&conflict)
                        || squad.delivered_cargo.contains(&conflict)
                })
                .count(),
            1
        );
    }
}

#[test]
fn mixed_owner_squad_entities_are_unavailable_to_both_players_campaign_planning() {
    let (mut world, master_data, player_a) = empty_v3_world();
    let player_b = PlayerId(2);
    world
        .resource_mut::<PlayerAiSettings>()
        .set_version(player_b, AiVersion::V3);
    let origin_a = GridPosition { x: 0, y: 0 };
    let bridge = GridPosition { x: 1, y: 0 };
    let origin_b = GridPosition { x: 2, y: 0 };
    let target = GridPosition { x: 4, y: 0 };
    let mut map = Map::new(5, 1, Terrain::Sea, GridTopology::Square);
    map.set_terrain(origin_a.x, origin_a.y, Terrain::Airport)
        .unwrap();
    map.set_terrain(bridge.x, bridge.y, Terrain::Plains)
        .unwrap();
    map.set_terrain(origin_b.x, origin_b.y, Terrain::Airport)
        .unwrap();
    map.set_terrain(target.x, target.y, Terrain::City).unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let target_island = island_map.get_island_at(&target).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((
        origin_a,
        Property::new(Terrain::Airport, Some(player_a), 100),
    ));
    world.spawn((
        origin_b,
        Property::new(Terrain::Airport, Some(player_b), 100),
    ));
    world.spawn((target, Property::new(Terrain::City, None, 100)));

    let mut mixed_entities = Vec::new();
    for (player, position) in [(player_a, origin_a), (player_b, origin_b)] {
        mixed_entities.push(spawn_master_unit(
            &mut world,
            &master_data,
            player,
            position,
            UnitType::Infantry,
        ));
        mixed_entities.push(spawn_master_unit(
            &mut world,
            &master_data,
            player,
            position,
            UnitType::Infantry,
        ));
        mixed_entities.push(spawn_master_unit(
            &mut world,
            &master_data,
            player,
            position,
            UnitType::Tank,
        ));
        let transport_stats = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        world.spawn((
            player,
            Faction(player),
            position,
            transport_stats.clone(),
            CargoCapacity {
                max: transport_stats.max_cargo,
                loaded: Vec::new(),
            },
            Health {
                current: 100,
                max: 100,
            },
        ));
    }
    let mixed_set: BTreeSet<_> = mixed_entities.iter().copied().collect();
    let (mixed_id, snapshot) = {
        let mut manager = world.remove_resource::<SquadManager>().unwrap();
        let mixed = manager.create_squad(MissionType::Capture);
        mixed.members = mixed_set.clone();
        mixed.target_island = Some(target_island);
        mixed.target = Some(target);
        mixed.phase = MissionPhase::MovingToTarget;
        let snapshot = (
            mixed.mission_type.clone(),
            mixed.members.clone(),
            mixed.transport_entity,
            mixed.cargo_entities.clone(),
            mixed.delivered_cargo.clone(),
            mixed.target_island,
            mixed.target,
            mixed.phase.clone(),
            mixed.pickup_position,
            mixed.drop_position,
        );
        let id = mixed.id;
        world.insert_resource(manager);
        (id, snapshot)
    };

    for player in [player_a, player_b] {
        let portfolio =
            crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
        assert!(
            portfolio
                .defenses
                .iter()
                .chain(portfolio.active_offensives.iter())
                .flat_map(sorted_assignment_entities)
                .all(|entity| !mixed_set.contains(&entity))
        );
        plan_squads(&mut world, player);
        let manager = world.resource::<SquadManager>();
        let mixed = manager
            .squads
            .iter()
            .find(|squad| squad.id == mixed_id)
            .expect("mixed Squad must remain present");
        assert_eq!(mixed.owner_id, None);
        assert_eq!(
            (
                mixed.mission_type.clone(),
                mixed.members.clone(),
                mixed.transport_entity,
                mixed.cargo_entities.clone(),
                mixed.delivered_cargo.clone(),
                mixed.target_island,
                mixed.target,
                mixed.phase.clone(),
                mixed.pickup_position,
                mixed.drop_position,
            ),
            snapshot
        );
        for entity in &mixed_set {
            assert_eq!(
                manager
                    .squads
                    .iter()
                    .filter(|squad| {
                        squad.members.contains(entity)
                            || squad.cargo_entities.contains(entity)
                            || squad.delivered_cargo.contains(entity)
                    })
                    .count(),
                1,
                "mixed Entity must not gain a second Squad owner"
            );
        }
    }
}

#[test]
fn faction_only_cargo_marks_every_mixed_squad_reference_unavailable() {
    let (mut world, master_data, player_a) = empty_v3_world();
    let player_b = PlayerId(2);
    world
        .resource_mut::<PlayerAiSettings>()
        .set_version(player_b, AiVersion::V3);
    let base = GridPosition { x: 0, y: 0 };
    let target = GridPosition { x: 2, y: 0 };
    let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
    map.set_terrain(base.x, base.y, Terrain::Airport).unwrap();
    map.set_terrain(target.x, target.y, Terrain::City).unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let target_island = island_map.get_island_at(&target).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((base, Property::new(Terrain::Airport, Some(player_a), 100)));
    world.spawn((target, Property::new(Terrain::City, None, 100)));

    let mixed_player_a =
        spawn_master_unit(&mut world, &master_data, player_a, base, UnitType::Infantry);
    spawn_master_unit(&mut world, &master_data, player_a, base, UnitType::Infantry);
    let transport_stats = master_data
        .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
        .unwrap();
    world.spawn((
        player_a,
        Faction(player_a),
        base,
        transport_stats.clone(),
        CargoCapacity {
            max: transport_stats.max_cargo,
            loaded: Vec::new(),
        },
        Health {
            current: 100,
            max: 100,
        },
    ));

    // Factionだけを持つ参照とstale参照も、完全なUnitSnapshotとは独立に所有者判定へ含める。
    let faction_only_player_b = world.spawn(Faction(player_b)).id();
    let factionless_transport = world.spawn_empty().id();
    let factionless_delivered = world.spawn_empty().id();
    let referenced = HashSet::from([
        mixed_player_a,
        faction_only_player_b,
        factionless_transport,
        factionless_delivered,
    ]);
    let (mixed_id, snapshot) = {
        let mut manager = world.remove_resource::<SquadManager>().unwrap();
        let mixed = manager.create_squad(MissionType::Capture);
        mixed.members.insert(mixed_player_a);
        mixed.transport_entity = Some(factionless_transport);
        mixed.cargo_entities = vec![faction_only_player_b];
        mixed.delivered_cargo = vec![factionless_delivered];
        mixed.target_island = Some(target_island);
        mixed.target = Some(target);
        mixed.phase = MissionPhase::MovingToTarget;
        let snapshot = (
            mixed.mission_type.clone(),
            mixed.members.clone(),
            mixed.transport_entity,
            mixed.cargo_entities.clone(),
            mixed.delivered_cargo.clone(),
            mixed.target_island,
            mixed.target,
            mixed.phase.clone(),
            mixed.pickup_position,
            mixed.drop_position,
        );
        let id = mixed.id;
        world.insert_resource(manager);
        (id, snapshot)
    };

    for player in [player_a, player_b] {
        let portfolio =
            crate::ai::strategy::analyze_strategy(&mut world, player).campaign_portfolio;
        assert!(
            portfolio
                .defenses
                .iter()
                .chain(portfolio.active_offensives.iter())
                .flat_map(sorted_assignment_entities)
                .all(|entity| !referenced.contains(&entity)),
            "mixed Squad reference must not enter a campaign assignment"
        );

        plan_squads(&mut world, player);
        let manager = world.resource::<SquadManager>();
        let mixed = manager
            .squads
            .iter()
            .find(|squad| squad.id == mixed_id)
            .expect("mixed Squad must remain present");
        assert_eq!(mixed.owner_id, None);
        assert_eq!(
            (
                mixed.mission_type.clone(),
                mixed.members.clone(),
                mixed.transport_entity,
                mixed.cargo_entities.clone(),
                mixed.delivered_cargo.clone(),
                mixed.target_island,
                mixed.target,
                mixed.phase.clone(),
                mixed.pickup_position,
                mixed.drop_position,
            ),
            snapshot
        );
        for entity in &referenced {
            assert_eq!(
                manager
                    .squads
                    .iter()
                    .filter(|squad| {
                        squad.transport_entity == Some(*entity)
                            || squad.members.contains(entity)
                            || squad.cargo_entities.contains(entity)
                            || squad.delivered_cargo.contains(entity)
                    })
                    .count(),
                1,
                "mixed Squad reference must not gain another owner"
            );
        }
    }
}

#[test]
fn generic_planning_never_appends_units_to_a_foreign_same_target_squad() {
    let (mut world, master_data, player_a) = empty_v3_world();
    let player_b = PlayerId(2);
    world
        .resource_mut::<PlayerAiSettings>()
        .set_version(player_b, AiVersion::V1);
    let base = GridPosition { x: 0, y: 0 };
    let staging = GridPosition { x: 1, y: 0 };
    let enemy_position = GridPosition { x: 3, y: 0 };
    let mut map = Map::new(4, 1, Terrain::Plains, GridTopology::Square);
    map.set_terrain(base.x, base.y, Terrain::Capital).unwrap();
    map.set_terrain(enemy_position.x, enemy_position.y, Terrain::City)
        .unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((base, Property::new(Terrain::Capital, Some(player_b), 100)));
    world.spawn((
        enemy_position,
        Property::new(Terrain::City, Some(player_a), 100),
    ));

    let foreign_member = world.spawn((Faction(player_a),)).id();
    let own_member = spawn_master_unit(&mut world, &master_data, player_b, staging, UnitType::Tank);
    let (foreign_id, snapshot) = {
        let mut manager = world.remove_resource::<SquadManager>().unwrap();
        let squad = manager.create_squad(MissionType::Attack);
        squad.members.insert(foreign_member);
        squad.target = Some(enemy_position);
        squad.phase = MissionPhase::MovingToTarget;
        let snapshot = (
            squad.mission_type.clone(),
            squad.members.clone(),
            squad.transport_entity,
            squad.cargo_entities.clone(),
            squad.delivered_cargo.clone(),
            squad.target_island,
            squad.target,
            squad.phase.clone(),
            squad.pickup_position,
            squad.drop_position,
        );
        let id = squad.id;
        world.insert_resource(manager);
        (id, snapshot)
    };

    plan_squads(&mut world, player_b);
    {
        let manager = world.resource::<SquadManager>();
        let foreign = manager
            .squads
            .iter()
            .find(|squad| squad.id == foreign_id)
            .expect("foreign Squad must remain present");
        assert_eq!(
            (
                foreign.mission_type.clone(),
                foreign.members.clone(),
                foreign.transport_entity,
                foreign.cargo_entities.clone(),
                foreign.delivered_cargo.clone(),
                foreign.target_island,
                foreign.target,
                foreign.phase.clone(),
                foreign.pickup_position,
                foreign.drop_position,
            ),
            snapshot
        );
        assert!(!foreign.members.contains(&own_member));
        assert!(manager.squads.iter().any(|squad| {
            squad.id != foreign_id
                && squad.members.contains(&own_member)
                && !squad.members.contains(&foreign_member)
        }));
    }

    plan_squads(&mut world, player_a);
    let manager = world.resource::<SquadManager>();
    let foreign = manager
        .squads
        .iter()
        .find(|squad| squad.id == foreign_id)
        .expect("player A must continue its original Squad");
    assert_eq!(foreign.members, BTreeSet::from([foreign_member]));
}

#[test]
fn planning_another_player_preserves_foreign_transport_squad_continuity() {
    let (mut world, master_data, player_a) = empty_v3_world();
    let player_b = PlayerId(2);
    world
        .resource_mut::<PlayerAiSettings>()
        .set_version(player_b, AiVersion::V3);
    let origin = GridPosition { x: 0, y: 1 };
    let target = GridPosition { x: 3, y: 1 };
    let mut map = Map::new(4, 3, Terrain::Sea, GridTopology::Square);
    map.set_terrain(origin.x, origin.y, Terrain::Airport)
        .unwrap();
    map.set_terrain(target.x, target.y, Terrain::City).unwrap();
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let target_island = island_map.get_island_at(&target).unwrap().id;
    world.insert_resource(map);
    world.insert_resource(island_map);
    world.spawn((origin, Property::new(Terrain::Airport, Some(player_a), 100)));
    world.spawn((target, Property::new(Terrain::City, None, 100)));

    let cargo = spawn_master_unit(
        &mut world,
        &master_data,
        player_a,
        GridPosition { x: 9_999, y: 9_999 },
        UnitType::Infantry,
    );
    let transport_stats = master_data
        .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
        .unwrap();
    let transport = world
        .spawn((
            player_a,
            Faction(player_a),
            origin,
            transport_stats.clone(),
            CargoCapacity {
                max: transport_stats.max_cargo,
                loaded: vec![cargo],
            },
            Health {
                current: 100,
                max: 100,
            },
        ))
        .id();
    world.entity_mut(cargo).insert(Transporting(transport));
    spawn_master_unit(&mut world, &master_data, player_b, target, UnitType::Tank);

    let (squad_id, snapshot) = {
        let mut manager = world.remove_resource::<SquadManager>().unwrap();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities = vec![cargo];
        squad.target_island = Some(target_island);
        squad.target = Some(target);
        squad.phase = MissionPhase::Transport(TransportPhase::Transit);
        let snapshot = (
            squad.phase.clone(),
            squad.transport_entity,
            squad.cargo_entities.clone(),
            squad.target_island,
            squad.target,
            squad.members.clone(),
        );
        let id = squad.id;
        world.insert_resource(manager);
        (id, snapshot)
    };

    plan_squads(&mut world, player_b);
    {
        let manager = world.resource::<SquadManager>();
        let squad = manager
            .squads
            .iter()
            .find(|squad| squad.id == squad_id)
            .expect("foreign planning must not delete player A Squad");
        assert_eq!(
            (
                squad.phase.clone(),
                squad.transport_entity,
                squad.cargo_entities.clone(),
                squad.target_island,
                squad.target,
                squad.members.clone(),
            ),
            snapshot
        );
    }

    plan_squads(&mut world, player_a);
    let manager = world.resource::<SquadManager>();
    let squad = manager
        .squads
        .iter()
        .find(|squad| squad.id == squad_id)
        .expect("player A planning must continue its live transport operation");
    assert_eq!(squad.transport_entity, Some(transport));
    assert_eq!(squad.cargo_entities, vec![cargo]);
    assert_eq!(squad.target_island, Some(target_island));
    assert!(matches!(
        squad.phase,
        MissionPhase::Transport(TransportPhase::Transit | TransportPhase::Drop)
    ));
}
