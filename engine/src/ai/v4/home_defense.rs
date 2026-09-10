//! 所有拠点へ向かう敵占領役に対し、占領期限から地上戦力を排他的に割り当てる。
//! 進軍DAGのセル順ではなく、拠点を射程に収める配置と実経路の到着時刻を使う。

use crate::ai::engine::AiCommand;
use crate::ai::islands::IslandMap;
use crate::ai::turn_distance::{
    ActionTurnDistance, ActionTurnDistanceCache, calculate_action_distance_to_range,
    calculate_action_distance_to_range_after_leaving_start,
};
use crate::components::{
    ActionCompleted, Ammo, CargoCapacity, Faction, Fuel, GridPosition, HasMoved, Health, PlayerId,
    Property, Transporting, UnitStats,
};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Map, MatchState, MovementType, Terrain};
use crate::systems::combat::get_detailed_expected_damage;
use crate::systems::movement::{OccupantInfo, calculate_reachable_tiles};
use bevy_ecs::prelude::*;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy)]
struct DefenseOrder {
    unit: Entity,
    enemy: Entity,
    property: GridPosition,
    firing_position: GridPosition,
    departure_due: bool,
}

#[derive(Debug, Default)]
struct DefensePlan {
    turn: u32,
    orders: Vec<DefenseOrder>,
}

#[derive(Resource, Default)]
struct HomeDefenseRegistry {
    plans: HashMap<PlayerId, DefensePlan>,
}

struct UnitView<'a> {
    entity: Entity,
    position: GridPosition,
    owner: PlayerId,
    stats: &'a UnitStats,
    hp: u32,
    fuel: u32,
    ammo: (u32, u32),
    available: bool,
    carrying: bool,
}

#[derive(Debug, Clone, Copy)]
struct Threat {
    enemy: Entity,
    property: GridPosition,
    arrival: u32,
    deadline: u32,
}

struct Response {
    order: DefenseOrder,
    ready: u32,
    damage: u32,
    loss: u32,
}

fn units<'a>(world: &'a World, skip: &HashSet<Entity>) -> Vec<UnitView<'a>> {
    let mut result = world
        .iter_entities()
        .filter_map(|entity| {
            if entity.contains::<Transporting>() {
                return None;
            }
            let stats = entity.get::<UnitStats>()?;
            let hp = entity.get::<Health>()?.current;
            let position = *entity.get::<GridPosition>()?;
            let owner = entity.get::<Faction>()?.0;
            (hp > 0).then(|| UnitView {
                entity: entity.id(),
                position,
                owner,
                stats,
                hp,
                fuel: entity
                    .get::<Fuel>()
                    .map_or(stats.max_fuel, |fuel| fuel.current),
                ammo: entity
                    .get::<Ammo>()
                    .map_or((0, 0), |ammo| (ammo.ammo1, ammo.ammo2)),
                available: !skip.contains(&entity.id())
                    && !entity.get::<HasMoved>().is_some_and(|moved| moved.0)
                    && !entity
                        .get::<ActionCompleted>()
                        .is_some_and(|completed| completed.0),
                carrying: entity
                    .get::<CargoCapacity>()
                    .is_some_and(|cargo| !cargo.loaded.is_empty()),
            })
        })
        .collect::<Vec<_>>();
    result.sort_unstable_by_key(|unit| unit.entity.to_bits());
    result
}

fn occupancy(units: &[UnitView<'_>]) -> HashMap<(usize, usize), OccupantInfo> {
    units
        .iter()
        .map(|unit| {
            (
                (unit.position.x, unit.position.y),
                OccupantInfo {
                    player_id: unit.owner,
                    is_transport: false,
                    unit_type: unit.stats.unit_type,
                    loadable_types: Vec::new(),
                    free_slots: 0,
                },
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn distance(
    map: &Map,
    data: &MasterDataRegistry,
    occupied: &HashMap<(usize, usize), OccupantInfo>,
    unit: &UnitView<'_>,
    target: GridPosition,
    range: (u32, u32),
    cache: &mut ActionTurnDistanceCache,
) -> Option<ActionTurnDistance> {
    calculate_action_distance_to_range(
        map,
        data,
        occupied,
        (unit.position.x, unit.position.y),
        (target.x, target.y),
        unit.stats.movement_type,
        unit.stats.max_movement,
        unit.fuel,
        range.0,
        range.1,
        unit.owner,
        cache,
    )
}

fn threats(
    world: &World,
    player: PlayerId,
    units: &[UnitView<'_>],
    cache: &mut ActionTurnDistanceCache,
) -> Option<Vec<Threat>> {
    let map = world.get_resource::<Map>()?;
    let data = world.get_resource::<MasterDataRegistry>()?;
    let islands = world.get_resource::<IslandMap>()?;
    let mut properties = world
        .iter_entities()
        .filter_map(|entity| Some((*entity.get::<GridPosition>()?, *entity.get::<Property>()?)))
        .collect::<Vec<_>>();
    properties.sort_unstable_by_key(|(position, _)| (position.y, position.x));
    let home = properties
        .iter()
        .find(|(_, property)| {
            property.owner_id == Some(player) && property.terrain == Terrain::Capital
        })?
        .0;
    let home_island = islands.get_island_at(&home)?.id;
    let empty = HashMap::new();
    let mut result = Vec::new();
    for enemy in units.iter().filter(|unit| {
        unit.owner != player
            && unit.stats.can_capture
            && islands
                .get_island_at(&unit.position)
                .is_some_and(|island| island.id == home_island)
    }) {
        // 一体の占領役を複数拠点へ同時に進ませない。まず敵から見た最早の占領先を選ぶ。
        // 手近な中立を取れる敵を、遠い自軍拠点への即時脅威として数えない。
        let target = properties
            .iter()
            .filter(|(_, property)| {
                property.owner_id != Some(enemy.owner) && property.max_capture_points > 0
            })
            .filter_map(|(position, property)| {
                let arrival = distance(map, data, &empty, enemy, *position, (0, 0), cache)?
                    .turns
                    .max(1);
                let actions = super::property_control::capture_actions_needed(
                    property.capture_points,
                    super::property_control::capture_power_from_hp(enemy.hp),
                );
                Some((
                    arrival.saturating_add(actions),
                    arrival,
                    *position,
                    *property,
                ))
            })
            .min_by_key(|(completion, arrival, position, _)| {
                (*completion, *arrival, position.y, position.x)
            });
        let Some((completion, arrival, position, property)) = target else {
            continue;
        };
        if property.owner_id == Some(player) {
            // 現在の自軍行動をoffset 0とし、次の敵行動の後が自軍offset 1。
            // 到着時にも占領できるため、完了直前に残る自軍行動は合計から2を引く。
            result.push(Threat {
                enemy: enemy.entity,
                property: position,
                arrival,
                deadline: completion.saturating_sub(2),
            });
        }
    }
    result.sort_unstable_by_key(|threat| (threat.deadline, threat.enemy.to_bits()));
    Some(result)
}

#[allow(clippy::too_many_arguments)]
fn response(
    world: &World,
    unit: &UnitView<'_>,
    enemy: &UnitView<'_>,
    threat: Threat,
    occupied: &HashMap<(usize, usize), OccupantInfo>,
    cache: &mut ActionTurnDistanceCache,
) -> Option<Response> {
    let map = world.get_resource::<Map>()?;
    let data = world.get_resource::<MasterDataRegistry>()?;
    let chart = world.get_resource::<DamageChart>()?;
    let range = (unit.stats.min_range, unit.stats.max_range);
    let availability = u32::from(!unit.available);
    let current = distance(map, data, occupied, unit, enemy.position, range, cache)
        .filter(|distance| {
            unit.available
                && distance.turns <= 1
                && map.distance(
                    distance.firing_position.x,
                    distance.firing_position.y,
                    threat.property.x,
                    threat.property.y,
                ) <= unit.stats.max_range
        })
        .filter(|distance| {
            let range = map.distance(
                distance.firing_position.x,
                distance.firing_position.y,
                enemy.position.x,
                enemy.position.y,
            );
            !distance.requires_movement || range <= 1
        });
    let (projection, target, ready, travel_ready) = if let Some(current) = current {
        (current, enemy.position, 0, 0)
    } else {
        let on_factory = world.iter_entities().any(|entity| {
            entity.get::<GridPosition>() == Some(&unit.position)
                && entity.get::<Property>().is_some_and(|property| {
                    property.owner_id == Some(unit.owner)
                        && data.is_production_facility(property.terrain.as_str())
                })
        });
        let future = if on_factory {
            calculate_action_distance_to_range_after_leaving_start(
                map,
                data,
                occupied,
                (unit.position.x, unit.position.y),
                (threat.property.x, threat.property.y),
                unit.stats.movement_type,
                unit.stats.max_movement,
                unit.fuel,
                range.0,
                range.1,
                unit.owner,
                cache,
            )?
        } else {
            distance(map, data, occupied, unit, threat.property, range, cache)?
        };
        let travel_ready = availability.saturating_add(future.turns.saturating_sub(1));
        let ready = travel_ready.max(threat.arrival);
        (future, threat.property, ready, travel_ready)
    };
    if ready > threat.deadline {
        return None;
    }
    let target_bonus = data.get_terrain_defense_bonus(map.get_terrain(target.x, target.y)?);
    let range = map.distance(
        projection.firing_position.x,
        projection.firing_position.y,
        target.x,
        target.y,
    );
    let (damage, (_, _, indirect)) = get_detailed_expected_damage(
        unit.stats,
        unit.hp,
        unit.ammo,
        enemy.stats,
        target_bonus,
        range,
        data,
        chart,
        false,
    )?;
    if damage == 0 {
        return None;
    }
    let my_bonus = data.get_terrain_defense_bonus(
        map.get_terrain(projection.firing_position.x, projection.firing_position.y)?,
    );
    let counter = if indirect {
        0
    } else {
        get_detailed_expected_damage(
            enemy.stats,
            enemy.hp,
            enemy.ammo,
            unit.stats,
            my_bonus,
            range,
            data,
            chart,
            true,
        )
        .map_or(0, |(damage, _)| damage)
    };
    Some(Response {
        order: DefenseOrder {
            unit: unit.entity,
            enemy: enemy.entity,
            property: threat.property,
            firing_position: projection.firing_position,
            // 占領阻止にまだ間に合う間は通常の戦闘を止めない。
            // 一手遅れると期限に届かなくなる出発時刻だけを防衛契約で拘束する。
            departure_due: travel_ready >= threat.deadline,
        },
        ready,
        damage,
        loss: counter.min(unit.hp).saturating_mul(unit.stats.cost) / 100,
    })
}

fn build_plan(world: &World, player: PlayerId, skip: &HashSet<Entity>) -> Option<DefensePlan> {
    let units = units(world, skip);
    let mut cache = ActionTurnDistanceCache::default();
    let threats = threats(world, player, &units, &mut cache)?;
    let mut occupied = occupancy(&units);
    let mut assigned = HashSet::new();
    let mut orders = Vec::new();
    for threat in threats {
        let enemy = units.iter().find(|unit| unit.entity == threat.enemy)?;
        let mut remaining = enemy.hp;
        while remaining > 0 {
            let candidate = units
                .iter()
                .filter(|unit| {
                    unit.owner == player
                        && !assigned.contains(&unit.entity)
                        && !unit.stats.can_capture
                        && !unit.carrying
                        && !skip.contains(&unit.entity)
                        && !matches!(
                            unit.stats.movement_type,
                            MovementType::Air | MovementType::Ship
                        )
                })
                .filter_map(|unit| response(world, unit, enemy, threat, &occupied, &mut cache))
                .min_by_key(|response| {
                    (
                        response.ready,
                        Reverse(response.damage.min(remaining)),
                        response.loss,
                        response.order.unit.to_bits(),
                    )
                });
            let Some(candidate) = candidate else {
                break;
            };
            remaining = remaining.saturating_sub(candidate.damage);
            assigned.insert(candidate.order.unit);
            let unit = units
                .iter()
                .find(|unit| unit.entity == candidate.order.unit)?;
            // 予測射点は排他的に予約する。複数部隊が同じ一マスから同時射撃したことにしない。
            occupied.insert(
                (
                    candidate.order.firing_position.x,
                    candidate.order.firing_position.y,
                ),
                OccupantInfo {
                    player_id: player,
                    is_transport: false,
                    unit_type: unit.stats.unit_type,
                    loadable_types: Vec::new(),
                    free_slots: 0,
                },
            );
            orders.push(candidate.order);
        }
    }
    Some(DefensePlan {
        turn: world
            .get_resource::<MatchState>()
            .map_or(0, |state| state.current_turn_number.0),
        orders,
    })
}

fn command_for_order(
    world: &World,
    order: DefenseOrder,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    if !order.departure_due {
        return None;
    }
    let units = units(world, skip);
    let unit = units
        .iter()
        .find(|unit| unit.entity == order.unit && unit.available)?;
    let enemy = units.iter().find(|unit| unit.entity == order.enemy)?;
    let map = world.get_resource::<Map>()?;
    let data = world.get_resource::<MasterDataRegistry>()?;
    let chart = world.get_resource::<DamageChart>()?;
    if !world.iter_entities().any(|entity| {
        entity.get::<GridPosition>() == Some(&order.property)
            && entity
                .get::<Property>()
                .is_some_and(|property| property.owner_id == Some(unit.owner))
    }) {
        return None;
    }
    let occupied = occupancy(&units);
    let reachable = calculate_reachable_tiles(
        map,
        &occupied,
        (unit.position.x, unit.position.y),
        unit.stats.movement_type,
        unit.stats.max_movement,
        unit.fuel,
        unit.owner,
        unit.stats.unit_type,
        data,
    );
    let mut cache = ActionTurnDistanceCache::default();
    let target = reachable
        .iter()
        .copied()
        .filter(|position| {
            *position == (unit.position.x, unit.position.y) || !occupied.contains_key(position)
        })
        .filter_map(|position| {
            let route = calculate_action_distance_to_range(
                map,
                data,
                &occupied,
                position,
                (order.firing_position.x, order.firing_position.y),
                unit.stats.movement_type,
                unit.stats.max_movement,
                unit.fuel,
                0,
                0,
                unit.owner,
                &mut cache,
            )?;
            Some((route.turns, route.used_mp, position.1, position.0))
        })
        .min()?;
    let position = GridPosition {
        x: target.3,
        y: target.2,
    };
    let range = map.distance(position.x, position.y, enemy.position.x, enemy.position.y);
    let bonus =
        data.get_terrain_defense_bonus(map.get_terrain(enemy.position.x, enemy.position.y)?);
    let attack = get_detailed_expected_damage(
        unit.stats,
        unit.hp,
        unit.ammo,
        enemy.stats,
        bonus,
        range,
        data,
        chart,
        false,
    )
    .is_some_and(|(damage, (_, _, indirect))| {
        damage > 0 && (!indirect || position == unit.position)
    });
    let command = if attack {
        AiCommand::Attack {
            target_pos: position,
            target_entity: enemy.entity,
        }
    } else {
        AiCommand::Wait {
            target_pos: position,
        }
    };
    Some((unit.entity, command))
}

/// 既存の戦役所属を保ち、防衛担当の行動だけを通常進軍より先に実行する。
pub(crate) fn decide_action(
    world: &mut World,
    player: PlayerId,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let turn = world
        .get_resource::<MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let cached = world
        .get_resource::<HomeDefenseRegistry>()
        .and_then(|registry| registry.plans.get(&player))
        .is_some_and(|plan| plan.turn == turn);
    if !cached {
        let plan = build_plan(world, player, skip)?;
        world
            .get_resource_or_insert_with(HomeDefenseRegistry::default)
            .plans
            .insert(player, plan);
    }
    world
        .get_resource::<HomeDefenseRegistry>()?
        .plans
        .get(&player)?
        .orders
        .iter()
        .find_map(|order| command_for_order(world, *order, skip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{GridTopology, UnitType};

    fn position(x: usize, y: usize) -> GridPosition {
        GridPosition { x, y }
    }

    fn board() -> World {
        let mut world = World::new();
        let mut map = Map::new(9, 5, Terrain::Plains, GridTopology::Square);
        for (position, terrain, owner) in [
            (position(0, 2), Terrain::Capital, PlayerId(1)),
            (position(8, 2), Terrain::Capital, PlayerId(2)),
            (position(2, 2), Terrain::City, PlayerId(1)),
        ] {
            map.set_terrain(position.x, position.y, terrain).unwrap();
            world.spawn((position, Property::new(terrain, Some(owner), 200)));
        }
        world.insert_resource(IslandMap::analyze(&map));
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let mut chart = DamageChart::new();
        for kind in [UnitType::Tank, UnitType::Artillery] {
            chart.insert_damage(kind, UnitType::Infantry, 60);
            chart.insert_secondary_damage(kind, UnitType::Infantry, 60);
        }
        world.insert_resource(chart);
        world
    }

    fn unit(world: &mut World, owner: PlayerId, kind: UnitType, at: GridPosition) -> Entity {
        let indirect = kind == UnitType::Artillery;
        world
            .spawn((
                at,
                Faction(owner),
                Health {
                    current: 100,
                    max: 100,
                },
                HasMoved(false),
                ActionCompleted(false),
                Fuel {
                    current: 99,
                    max: 99,
                },
                Ammo {
                    ammo1: 10,
                    max_ammo1: 10,
                    ammo2: 10,
                    max_ammo2: 10,
                },
                UnitStats {
                    unit_type: kind,
                    can_capture: kind == UnitType::Infantry,
                    min_range: if indirect { 2 } else { 1 },
                    max_range: if indirect { 3 } else { 1 },
                    max_movement: 3,
                    movement_type: if indirect {
                        MovementType::Artillery
                    } else {
                        MovementType::Tank
                    },
                    max_fuel: 99,
                    max_ammo1: 10,
                    max_ammo2: 10,
                    ..UnitStats::mock()
                },
            ))
            .id()
    }

    #[test]
    fn assigns_distinct_units_and_firing_cells_until_required_damage_is_covered() {
        let mut world = board();
        let first = unit(&mut world, PlayerId(1), UnitType::Tank, position(0, 1));
        let second = unit(&mut world, PlayerId(1), UnitType::Tank, position(0, 3));
        unit(&mut world, PlayerId(1), UnitType::Tank, position(0, 4));
        let enemy = unit(&mut world, PlayerId(2), UnitType::Infantry, position(4, 2));
        let plan = build_plan(&world, PlayerId(1), &HashSet::new()).unwrap();
        assert_eq!(
            plan.orders.len(),
            2,
            "一体分のHPを二体で削り、残りの戦力は進軍へ残す"
        );
        let assigned = plan
            .orders
            .iter()
            .map(|order| order.unit)
            .collect::<HashSet<_>>();
        assert_eq!(assigned, HashSet::from([first, second]));
        assert_ne!(
            plan.orders[0].firing_position,
            plan.orders[1].firing_position
        );
        assert!(
            plan.orders
                .iter()
                .all(|order| order.enemy == enemy && order.property == position(2, 2))
        );
    }

    #[test]
    fn enemy_heading_to_a_closer_neutral_property_does_not_reserve_defenders() {
        let mut world = board();
        world
            .resource_mut::<Map>()
            .set_terrain(4, 1, Terrain::City)
            .unwrap();
        world.spawn((position(4, 1), Property::new(Terrain::City, None, 200)));
        unit(&mut world, PlayerId(1), UnitType::Tank, position(0, 1));
        unit(&mut world, PlayerId(2), UnitType::Infantry, position(4, 2));
        assert!(
            build_plan(&world, PlayerId(1), &HashSet::new())
                .unwrap()
                .orders
                .is_empty()
        );
    }

    #[test]
    fn an_indirect_unit_that_must_move_cannot_meet_a_current_turn_capture_deadline() {
        let mut world = board();
        let city = world
            .iter_entities()
            .find(|entity| entity.get::<GridPosition>() == Some(&position(2, 2)))
            .unwrap()
            .id();
        world.get_mut::<Property>(city).unwrap().capture_points = 100;
        unit(&mut world, PlayerId(1), UnitType::Artillery, position(0, 4));
        unit(&mut world, PlayerId(2), UnitType::Infantry, position(2, 2));
        assert!(
            build_plan(&world, PlayerId(1), &HashSet::new())
                .unwrap()
                .orders
                .is_empty()
        );
    }

    #[test]
    fn keeps_fighting_until_the_latest_defense_departure_turn() {
        let mut world = board();
        unit(&mut world, PlayerId(1), UnitType::Tank, position(0, 1));
        unit(&mut world, PlayerId(1), UnitType::Tank, position(0, 3));
        let enemy = unit(&mut world, PlayerId(2), UnitType::Infantry, position(4, 2));
        let early = build_plan(&world, PlayerId(1), &HashSet::new()).unwrap();
        assert!(!early.orders.is_empty());
        assert!(early.orders.iter().all(|order| !order.departure_due));

        // 敵が都市上で最後の占領行動を残した局面では、今手番の迎撃を拘束する。
        *world.get_mut::<GridPosition>(enemy).unwrap() = position(2, 2);
        let city = world
            .iter_entities()
            .find(|entity| {
                entity.get::<Property>().is_some()
                    && entity.get::<GridPosition>() == Some(&position(2, 2))
            })
            .unwrap()
            .id();
        world.get_mut::<Property>(city).unwrap().capture_points = 100;
        let urgent = build_plan(&world, PlayerId(1), &HashSet::new()).unwrap();
        assert!(!urgent.orders.is_empty());
        assert!(urgent.orders.iter().all(|order| order.departure_due));
        let (_, command) = command_for_order(&world, urgent.orders[0], &HashSet::new()).unwrap();
        assert!(
            matches!(command, AiCommand::Attack { target_entity, .. } if target_entity == enemy)
        );
    }

    #[test]
    fn capture_units_keep_their_existing_capture_contracts() {
        let mut world = board();
        unit(&mut world, PlayerId(1), UnitType::Infantry, position(1, 2));
        unit(&mut world, PlayerId(2), UnitType::Infantry, position(4, 2));
        assert!(
            build_plan(&world, PlayerId(1), &HashSet::new())
                .unwrap()
                .orders
                .is_empty()
        );
    }
}
