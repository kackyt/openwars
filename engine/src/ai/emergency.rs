use crate::ai::turn_distance::{TurnDistanceCache, calculate_turn_distance};
use crate::components::{
    Ammo, Faction, Fuel, GridPosition, Health, PlayerId, Property, Transporting, UnitStats,
};
use crate::resources::{DamageChart, Map, MovementType, Terrain, master_data::MasterDataRegistry};
use crate::systems::combat::get_expected_damage;
use crate::systems::movement::{OccupantInfo, calculate_reachable_tiles};
use bevy_ecs::prelude::*;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

/// ターン内で緊急ミッションを識別するID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EmergencyMissionId(pub u32);

/// 重要拠点の優先度を比較する値オブジェクト。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SitePriorityScore(pub u64);

/// 緊急ミッションで採用する対応方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmergencyResponse {
    EliminateThreat,
    OccupySite,
    BlockRoute,
}

/// 敵占領ユニットが重要拠点へ到達する脅威。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CriticalSiteThreat {
    pub threat_entity: Entity,
    pub threat_position: GridPosition,
    pub site_position: GridPosition,
    pub site_terrain: Terrain,
    pub site_capture_points: u32,
    pub eta: u32,
    pub priority: SitePriorityScore,
}

/// 1ユニットへ割り当てる迎撃ミッション。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmergencyMission {
    pub id: EmergencyMissionId,
    pub owner_id: PlayerId,
    pub assigned_entity: Entity,
    pub threat: CriticalSiteThreat,
    pub response: EmergencyResponse,
    pub target_position: GridPosition,
}

/// 盤面から毎ターン再構築する緊急ミッション計画。
#[derive(Resource, Debug, Clone, Default)]
pub struct EmergencyMissionPlan {
    pub missions: Vec<EmergencyMission>,
}

impl EmergencyMissionPlan {
    pub fn reserved_entities(&self) -> HashSet<Entity> {
        self.missions
            .iter()
            .map(|mission| mission.assigned_entity)
            .collect()
    }

    pub fn mission_for_entity(&self, entity: Entity) -> Option<&EmergencyMission> {
        self.missions
            .iter()
            .find(|mission| mission.assigned_entity == entity)
    }
}

#[derive(Debug, Clone)]
struct UnitSnapshot {
    entity: Entity,
    faction: PlayerId,
    position: GridPosition,
    stats: UnitStats,
    health: Health,
    ammo: (u32, u32),
    fuel: u32,
}

fn critical_site_rank(terrain: Terrain) -> Option<u64> {
    match terrain {
        Terrain::Capital => Some(4),
        Terrain::Factory | Terrain::Airport | Terrain::Port => Some(3),
        Terrain::City => Some(2),
        _ => None,
    }
}

fn site_priority(
    terrain: Terrain,
    eta: u32,
    registry: &MasterDataRegistry,
) -> Option<SitePriorityScore> {
    let rank = critical_site_rank(terrain)?;
    let income = registry.landscape_income(terrain.as_str()) as u64;
    Some(SitePriorityScore(
        rank * 1_000_000 + income * 1_000 + u64::from(2u32.saturating_sub(eta)),
    ))
}

fn unit_positions_from_snapshots(units: &[UnitSnapshot]) -> HashMap<(usize, usize), OccupantInfo> {
    units
        .iter()
        .map(|unit| {
            (
                (unit.position.x, unit.position.y),
                OccupantInfo {
                    player_id: unit.faction,
                    is_transport: unit.stats.max_cargo > 0,
                    unit_type: unit.stats.unit_type,
                    loadable_types: unit.stats.loadable_unit_types.clone(),
                    free_slots: 0,
                },
            )
        })
        .collect()
}

fn required_capture_turns(capture_points: u32, capture_power: u32) -> u32 {
    if capture_power == 0 {
        return u32::MAX;
    }
    capture_points.div_ceil(capture_power)
}

fn select_eliminator(
    candidates: &[UnitSnapshot],
    enemy: &UnitSnapshot,
    threat: &CriticalSiteThreat,
    map: &Map,
    registry: &MasterDataRegistry,
    damage_chart: &DamageChart,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
) -> Option<Entity> {
    let defense_bonus = map
        .get_terrain(enemy.position.x, enemy.position.y)
        .map(|terrain| registry.get_terrain_defense_bonus(terrain))
        .unwrap_or(0);
    let mut ranked = Vec::new();

    for candidate in candidates {
        let mut cache = TurnDistanceCache::default();
        let arrival = calculate_turn_distance(
            map,
            registry,
            unit_positions,
            (candidate.position.x, candidate.position.y),
            (enemy.position.x, enemy.position.y),
            candidate.stats.movement_type,
            candidate.stats.max_movement,
            candidate.stats.max_range.max(1),
            candidate.faction,
            &mut cache,
        );
        if arrival.turns > threat.eta
            || arrival.used_mp > candidate.fuel
            || (candidate.stats.min_range > 1 && arrival.turns > 0)
        {
            continue;
        }

        let distance = if arrival.turns == 0 {
            map.distance(
                candidate.position.x,
                candidate.position.y,
                enemy.position.x,
                enemy.position.y,
            )
        } else {
            candidate.stats.min_range.max(1)
        };
        let damage = get_expected_damage(
            &candidate.stats,
            candidate.health.current,
            candidate.ammo,
            &enemy.stats,
            defense_bonus,
            distance,
            registry,
            damage_chart,
            false,
        );
        if damage == 0 {
            continue;
        }

        let current_capture_power = enemy.health.current.saturating_add(9) / 10 * 10;
        let remaining_hp = enemy.health.current.saturating_sub(damage);
        let remaining_capture_power = remaining_hp.saturating_add(9) / 10 * 10;
        let lethal = damage >= enemy.health.current;
        let delays_capture =
            required_capture_turns(threat.site_capture_points, remaining_capture_power)
                > required_capture_turns(threat.site_capture_points, current_capture_power);
        if !lethal && !delays_capture {
            continue;
        }

        ranked.push((
            !candidate.stats.can_capture,
            lethal,
            delays_capture,
            Reverse(arrival.turns),
            damage,
            Reverse(candidate.entity.to_bits()),
            candidate.entity,
        ));
    }

    ranked.sort_by_key(|entry| (entry.0, entry.1, entry.2, entry.3, entry.4, entry.5));
    ranked.last().map(|entry| entry.6)
}

fn select_site_occupier(
    candidates: &[UnitSnapshot],
    threat: &CriticalSiteThreat,
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
) -> Option<Entity> {
    if unit_positions.contains_key(&(threat.site_position.x, threat.site_position.y)) {
        return None;
    }

    let mut ranked = Vec::new();
    for candidate in candidates {
        let mut cache = TurnDistanceCache::default();
        let arrival = calculate_turn_distance(
            map,
            registry,
            unit_positions,
            (candidate.position.x, candidate.position.y),
            (threat.site_position.x, threat.site_position.y),
            candidate.stats.movement_type,
            candidate.stats.max_movement,
            0,
            candidate.faction,
            &mut cache,
        );
        if arrival.turns <= threat.eta && arrival.used_mp <= candidate.fuel {
            ranked.push((arrival.turns, candidate.entity.to_bits(), candidate.entity));
        }
    }
    ranked.sort_by_key(|entry| (entry.0, entry.1));
    ranked.first().map(|entry| entry.2)
}

#[allow(clippy::too_many_arguments)]
fn select_route_blocker(
    candidates: &[UnitSnapshot],
    enemy: &UnitSnapshot,
    threat: &CriticalSiteThreat,
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &mut HashMap<(usize, usize), OccupantInfo>,
) -> Option<(Entity, GridPosition)> {
    let mut ranked = Vec::new();

    for candidate in candidates {
        let reachable = calculate_reachable_tiles(
            map,
            unit_positions,
            (candidate.position.x, candidate.position.y),
            candidate.stats.movement_type,
            candidate.stats.max_movement,
            candidate.fuel,
            candidate.faction,
            candidate.stats.unit_type,
            registry,
        );
        let mut tiles: Vec<_> = reachable.into_iter().collect();
        tiles.sort_by_key(|position| {
            (
                map.distance(enemy.position.x, enemy.position.y, position.0, position.1)
                    + map.distance(
                        position.0,
                        position.1,
                        threat.site_position.x,
                        threat.site_position.y,
                    ),
                position.1,
                position.0,
            )
        });
        tiles.truncate(16);

        for position in tiles {
            if position == (candidate.position.x, candidate.position.y)
                || position == (threat.site_position.x, threat.site_position.y)
                || unit_positions.contains_key(&position)
            {
                continue;
            }

            let original_position = (candidate.position.x, candidate.position.y);
            let original_occupant = unit_positions.remove(&original_position);
            let inserted = OccupantInfo {
                player_id: candidate.faction,
                is_transport: candidate.stats.max_cargo > 0,
                unit_type: candidate.stats.unit_type,
                loadable_types: candidate.stats.loadable_unit_types.clone(),
                free_slots: 0,
            };
            let replaced = unit_positions.insert(position, inserted);

            let mut cache = TurnDistanceCache::default();
            let delayed = calculate_turn_distance(
                map,
                registry,
                unit_positions,
                (enemy.position.x, enemy.position.y),
                (threat.site_position.x, threat.site_position.y),
                enemy.stats.movement_type,
                enemy.stats.max_movement,
                0,
                enemy.faction,
                &mut cache,
            );

            unit_positions.remove(&position);
            if let Some(occupant) = replaced {
                unit_positions.insert(position, occupant);
            }
            if let Some(occupant) = original_occupant {
                unit_positions.insert(original_position, occupant);
            }

            if delayed.turns > threat.eta {
                ranked.push((
                    delayed.turns,
                    Reverse(candidate.entity.to_bits()),
                    Reverse(position.1),
                    Reverse(position.0),
                    candidate.entity,
                    GridPosition {
                        x: position.0,
                        y: position.1,
                    },
                ));
            }
        }
    }

    ranked.sort_by_key(|entry| (entry.0, entry.1, entry.2, entry.3));
    ranked.last().map(|entry| (entry.4, entry.5))
}

/// 重要拠点への占領脅威を分析し、担当ユニットを割り当てます。
pub fn analyze_interceptions(
    world: &mut World,
    player_id: PlayerId,
    unavailable_entities: &HashSet<Entity>,
) -> EmergencyMissionPlan {
    let map = world.resource::<Map>().clone();
    let registry = world
        .get_resource::<MasterDataRegistry>()
        .cloned()
        .unwrap_or_default();
    let damage_chart = world
        .get_resource::<DamageChart>()
        .cloned()
        .unwrap_or_default();

    let units = {
        let mut query = world.query::<(
            Entity,
            &Faction,
            &GridPosition,
            &UnitStats,
            &Health,
            Option<&Ammo>,
            Option<&Fuel>,
            Option<&Transporting>,
        )>();
        query
            .iter(world)
            .filter(|(_, _, _, _, health, _, _, transporting)| {
                health.current > 0 && transporting.is_none()
            })
            .map(
                |(entity, faction, position, stats, health, ammo, fuel, _)| UnitSnapshot {
                    entity,
                    faction: faction.0,
                    position: *position,
                    stats: stats.clone(),
                    health: *health,
                    ammo: ammo.map_or((99, 99), |ammo| (ammo.ammo1, ammo.ammo2)),
                    fuel: fuel.map_or(u32::MAX, |fuel| fuel.current),
                },
            )
            .collect::<Vec<_>>()
    };
    let mut unit_positions = unit_positions_from_snapshots(&units);
    let properties = {
        let mut query = world.query::<(&GridPosition, &Property)>();
        query
            .iter(world)
            .filter_map(|(position, property)| {
                critical_site_rank(property.terrain).map(|_| (*position, *property))
            })
            .collect::<Vec<_>>()
    };

    let mut threats = Vec::new();
    for enemy in units.iter().filter(|unit| {
        unit.faction != player_id
            && unit.stats.can_capture
            && unit.stats.movement_type != MovementType::Air
    }) {
        for (site_position, property) in &properties {
            if property.owner_id == Some(enemy.faction)
                || unit_positions
                    .get(&(site_position.x, site_position.y))
                    .is_some_and(|occupant| occupant.player_id == player_id)
            {
                continue;
            }

            let mut cache = TurnDistanceCache::default();
            let distance = calculate_turn_distance(
                &map,
                &registry,
                &unit_positions,
                (enemy.position.x, enemy.position.y),
                (site_position.x, site_position.y),
                enemy.stats.movement_type,
                enemy.stats.max_movement,
                0,
                enemy.faction,
                &mut cache,
            );
            if distance.turns > 2 {
                continue;
            }
            let Some(priority) = site_priority(property.terrain, distance.turns, &registry) else {
                continue;
            };
            threats.push(CriticalSiteThreat {
                threat_entity: enemy.entity,
                threat_position: enemy.position,
                site_position: *site_position,
                site_terrain: property.terrain,
                site_capture_points: property.capture_points,
                eta: distance.turns,
                priority,
            });
        }
    }
    threats.sort_by_key(|threat| {
        (
            Reverse(threat.priority),
            threat.eta,
            threat.site_position.y,
            threat.site_position.x,
            threat.threat_entity.to_bits(),
        )
    });
    // 同じ敵を排除すれば複数拠点への脅威が同時に消えるため、最重要拠点の1件へ集約する。
    let mut planned_threats = HashSet::new();
    threats.retain(|threat| planned_threats.insert(threat.threat_entity));

    let mut reserved = unavailable_entities.clone();
    let mut missions = Vec::new();
    for threat in threats {
        let Some(enemy) = units
            .iter()
            .find(|unit| unit.entity == threat.threat_entity)
        else {
            continue;
        };
        let mut candidates = units
            .iter()
            .filter(|unit| unit.faction == player_id && !reserved.contains(&unit.entity))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| {
            (
                map.distance(
                    candidate.position.x,
                    candidate.position.y,
                    threat.site_position.x,
                    threat.site_position.y,
                ),
                candidate.entity.to_bits(),
            )
        });
        // 緊急分析の経路探索量を上限化し、遠方の戦力まで不要に引き抜かない。
        candidates.truncate(8);

        let assignment = if let Some(entity) = select_eliminator(
            &candidates,
            enemy,
            &threat,
            &map,
            &registry,
            &damage_chart,
            &unit_positions,
        ) {
            Some((
                entity,
                EmergencyResponse::EliminateThreat,
                threat.threat_position,
            ))
        } else if let Some(entity) =
            select_site_occupier(&candidates, &threat, &map, &registry, &unit_positions)
        {
            Some((entity, EmergencyResponse::OccupySite, threat.site_position))
        } else {
            select_route_blocker(
                &candidates,
                enemy,
                &threat,
                &map,
                &registry,
                &mut unit_positions,
            )
            .map(|(entity, position)| (entity, EmergencyResponse::BlockRoute, position))
        };

        let Some((assigned_entity, response, target_position)) = assignment else {
            continue;
        };
        reserved.insert(assigned_entity);
        missions.push(EmergencyMission {
            id: EmergencyMissionId(missions.len() as u32),
            owner_id: player_id,
            assigned_entity,
            threat,
            response,
            target_position,
        });
    }

    EmergencyMissionPlan { missions }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{
        ActionCompleted, Ammo, Faction, Fuel, HasMoved, Health, Property, UnitStats,
    };
    use crate::resources::{GridTopology, Map, MovementType, UnitType};

    fn setup_world(width: usize, height: usize) -> World {
        let master_data = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }
        let map = Map {
            width,
            height,
            tiles: vec![Terrain::Plains; width * height],
            topology: GridTopology::Square,
        };
        world.insert_resource(map.clone());
        world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
        world
    }

    fn spawn_unit(
        world: &mut World,
        player: PlayerId,
        position: GridPosition,
        stats: UnitStats,
        hp: u32,
    ) -> Entity {
        let max_ammo1 = stats.max_ammo1;
        let max_ammo2 = stats.max_ammo2;
        world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                position,
                stats,
                Health {
                    current: hp,
                    max: 100,
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
                Ammo {
                    ammo1: max_ammo1,
                    max_ammo1,
                    ammo2: max_ammo2,
                    max_ammo2,
                },
            ))
            .id()
    }

    fn infantry_stats(max_movement: u32) -> UnitStats {
        UnitStats {
            unit_type: UnitType::Infantry,
            movement_type: MovementType::Infantry,
            max_movement,
            can_capture: true,
            ..UnitStats::mock()
        }
    }

    fn blocker_stats(max_movement: u32) -> UnitStats {
        UnitStats {
            unit_type: UnitType::SupplyTruck,
            movement_type: MovementType::Infantry,
            max_movement,
            ..UnitStats::mock()
        }
    }

    #[test]
    fn issue74_generates_interception_at_eta_two() {
        let mut world = setup_world(10, 1);
        let player = PlayerId(1);
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::City, Some(player), 100),
        ));
        spawn_unit(
            &mut world,
            player.opposite(),
            GridPosition { x: 7, y: 0 },
            infantry_stats(3),
            100,
        );
        spawn_unit(
            &mut world,
            player,
            GridPosition { x: 0, y: 0 },
            blocker_stats(3),
            100,
        );

        let plan = analyze_interceptions(&mut world, player, &HashSet::new());

        assert_eq!(plan.missions.len(), 1);
        assert_eq!(plan.missions[0].threat.eta, 2);
    }

    #[test]
    fn issue74_ignores_capture_threat_beyond_eta_two() {
        let mut world = setup_world(12, 1);
        let player = PlayerId(1);
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::City, Some(player), 100),
        ));
        spawn_unit(
            &mut world,
            player.opposite(),
            GridPosition { x: 9, y: 0 },
            infantry_stats(3),
            100,
        );
        spawn_unit(
            &mut world,
            player,
            GridPosition { x: 1, y: 0 },
            blocker_stats(3),
            100,
        );

        let plan = analyze_interceptions(&mut world, player, &HashSet::new());

        assert!(plan.missions.is_empty());
    }

    #[test]
    fn issue74_prefers_occupying_site_when_attack_is_impossible() {
        let mut world = setup_world(6, 1);
        let player = PlayerId(1);
        let site = GridPosition { x: 2, y: 0 };
        world.spawn((site, Property::new(Terrain::Factory, Some(player), 100)));
        spawn_unit(
            &mut world,
            player.opposite(),
            GridPosition { x: 5, y: 0 },
            infantry_stats(3),
            100,
        );
        let defender = spawn_unit(
            &mut world,
            player,
            GridPosition { x: 0, y: 0 },
            blocker_stats(3),
            100,
        );

        let plan = analyze_interceptions(&mut world, player, &HashSet::new());

        assert_eq!(plan.missions.len(), 1);
        assert_eq!(plan.missions[0].assigned_entity, defender);
        assert_eq!(plan.missions[0].response, EmergencyResponse::OccupySite);
        assert_eq!(plan.missions[0].target_position, site);
    }

    #[test]
    fn issue74_prioritizes_capital_over_city() {
        let mut world = setup_world(10, 2);
        let player = PlayerId(1);
        world.spawn((
            GridPosition { x: 3, y: 0 },
            Property::new(Terrain::City, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        spawn_unit(
            &mut world,
            player.opposite(),
            GridPosition { x: 8, y: 0 },
            infantry_stats(3),
            100,
        );
        spawn_unit(
            &mut world,
            player,
            GridPosition { x: 2, y: 1 },
            blocker_stats(3),
            100,
        );
        spawn_unit(
            &mut world,
            player,
            GridPosition { x: 3, y: 1 },
            blocker_stats(3),
            100,
        );

        let plan = analyze_interceptions(&mut world, player, &HashSet::new());

        assert_eq!(plan.missions.len(), 1);
        assert_eq!(plan.missions[0].threat.site_terrain, Terrain::Capital);
    }

    #[test]
    fn issue74_blocks_route_when_site_cannot_be_reached_in_time() {
        let mut world = setup_world(5, 2);
        let player = PlayerId(1);
        let site = GridPosition { x: 0, y: 0 };
        world.spawn((site, Property::new(Terrain::City, Some(player), 100)));
        spawn_unit(
            &mut world,
            player.opposite(),
            GridPosition { x: 4, y: 0 },
            infantry_stats(2),
            100,
        );
        let blocker = spawn_unit(
            &mut world,
            player,
            GridPosition { x: 2, y: 1 },
            blocker_stats(1),
            100,
        );

        let plan = analyze_interceptions(&mut world, player, &HashSet::new());

        assert_eq!(plan.missions.len(), 1);
        assert_eq!(plan.missions[0].assigned_entity, blocker);
        assert_eq!(plan.missions[0].response, EmergencyResponse::BlockRoute);
        assert_eq!(
            plan.missions[0].target_position,
            GridPosition { x: 2, y: 0 }
        );
    }

    #[test]
    fn issue74_interception_target_survives_beam_search_and_executes() {
        let mut world = setup_world(7, 1);
        let player = PlayerId(1);
        let site = GridPosition { x: 3, y: 0 };
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(player, crate::ai::AiVersion::V3);
        world.insert_resource(settings);
        world.spawn((site, Property::new(Terrain::Factory, Some(player), 100)));
        spawn_unit(
            &mut world,
            player.opposite(),
            GridPosition { x: 6, y: 0 },
            infantry_stats(3),
            100,
        );
        let defender = spawn_unit(
            &mut world,
            player,
            GridPosition { x: 0, y: 0 },
            blocker_stats(3),
            100,
        );

        crate::ai::squad::plan_squads(&mut world, player);
        let mission_id = {
            let manager = world.resource::<crate::ai::squad::SquadManager>();
            let squad = manager
                .squads
                .iter()
                .find(|squad| squad.members.contains(&defender))
                .expect("迎撃担当のSquadが作成されること");
            assert_eq!(squad.target, Some(site));
            match squad.mission_type {
                crate::ai::squad::MissionType::Interception(id) => id,
                ref other => panic!("Interceptionを期待したが {other:?}"),
            }
        };

        crate::ai::beam_search::run_squad_beam_search(&mut world, player);
        let manager = world.resource::<crate::ai::squad::SquadManager>();
        let squad = manager
            .squads
            .iter()
            .find(|squad| {
                squad.mission_type == crate::ai::squad::MissionType::Interception(mission_id)
            })
            .expect("ビーム探索後も迎撃Squadが残ること");
        assert_eq!(squad.target, Some(site));

        let action = crate::ai::engine::decide_ai_action_v2(&mut world, player, &HashSet::new())
            .expect("迎撃行動が選ばれること");
        assert_eq!(action.0, defender);
        match action.1 {
            crate::ai::engine::AiCommand::Wait { target_pos } => assert_eq!(target_pos, site),
            other => panic!("拠点占有Waitを期待したが {other:?}"),
        }
    }

    #[test]
    fn issue74_v2_does_not_create_interception() {
        let mut world = setup_world(7, 1);
        let player = PlayerId(1);
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(player, crate::ai::AiVersion::V2);
        world.insert_resource(settings);
        world.spawn((
            GridPosition { x: 3, y: 0 },
            Property::new(Terrain::Factory, Some(player), 100),
        ));
        spawn_unit(
            &mut world,
            player.opposite(),
            GridPosition { x: 6, y: 0 },
            infantry_stats(3),
            100,
        );
        spawn_unit(
            &mut world,
            player,
            GridPosition { x: 0, y: 0 },
            blocker_stats(3),
            100,
        );

        crate::ai::squad::plan_squads(&mut world, player);

        let manager = world.resource::<crate::ai::squad::SquadManager>();
        assert!(manager.squads.iter().all(|squad| !matches!(
            squad.mission_type,
            crate::ai::squad::MissionType::Interception(_)
        )));
        assert!(world.get_resource::<EmergencyMissionPlan>().is_none());
    }

    #[test]
    fn issue74_missing_target_falls_back_without_stale_squad_bonus() {
        let mut world = setup_world(7, 2);
        let player = PlayerId(1);
        let site = GridPosition { x: 3, y: 0 };
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(player, crate::ai::AiVersion::V3);
        world.insert_resource(settings);
        world.spawn((site, Property::new(Terrain::Factory, Some(player), 100)));
        let threat = spawn_unit(
            &mut world,
            player.opposite(),
            GridPosition { x: 6, y: 0 },
            infantry_stats(3),
            100,
        );
        spawn_unit(
            &mut world,
            player.opposite(),
            GridPosition { x: 1, y: 1 },
            blocker_stats(1),
            100,
        );
        let defender = spawn_unit(
            &mut world,
            player,
            GridPosition { x: 0, y: 0 },
            blocker_stats(3),
            100,
        );
        crate::ai::squad::plan_squads(&mut world, player);
        world.despawn(threat);

        let action = crate::ai::engine::decide_ai_action_v2(&mut world, player, &HashSet::new())
            .expect("通常行動へフォールバックすること");

        assert_eq!(action.0, defender);
        match action.1 {
            crate::ai::engine::AiCommand::Wait { target_pos } => assert_ne!(target_pos, site),
            other => panic!("通常の接近Waitを期待したが {other:?}"),
        }
    }
}
