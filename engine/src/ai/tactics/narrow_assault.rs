//! 狭い装甲経路に限定したGO中の継続前進。
use crate::ai::AiCommand;
use crate::ai::turn_distance::{TurnDistanceCache, calculate_turn_distance};
use crate::collections::{HashMap, HashSet};
use crate::components::*;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Map, MovementType, Terrain};
use crate::systems::movement::{OccupantInfo, calculate_reachable_tiles, get_valid_movement_cost};
use bevy_ecs::prelude::*;
use std::collections::VecDeque;

/// 橋への依存、または一点を塞ぐと通れない回廊があるか。兵種比率・マップ名は使わない。
fn narrow_armor_corridor(
    map: &Map,
    registry: &MasterDataRegistry,
    home: GridPosition,
    target: GridPosition,
) -> bool {
    let passable = map
        .tiles
        .iter()
        .map(|&terrain| get_valid_movement_cost(registry, MovementType::Tank, terrain).is_some())
        .collect::<Vec<_>>();
    let Some(path) = armor_path(map, &passable, home, target, &HashSet::default()) else {
        return false;
    };
    let bridges = map
        .tiles
        .iter()
        .enumerate()
        .filter_map(|(i, &terrain)| {
            (terrain == Terrain::Bridge).then_some(GridPosition {
                x: i % map.width,
                y: i / map.width,
            })
        })
        .collect::<HashSet<_>>();
    if !bridges.is_empty() && armor_path(map, &passable, home, target, &bridges).is_none() {
        return true;
    }
    path.into_iter()
        .filter(|p| *p != home && *p != target)
        .any(|p| armor_path(map, &passable, home, target, &HashSet::from_iter([p])).is_none())
}

/// 占有を無視した装甲経路。橋を外した連結性と、一点で切れる回廊の有無にだけ使う。
fn armor_path(
    map: &Map,
    passable: &[bool],
    home: GridPosition,
    target: GridPosition,
    blocked: &HashSet<GridPosition>,
) -> Option<Vec<GridPosition>> {
    if home.x >= map.width
        || home.y >= map.height
        || target.x >= map.width
        || target.y >= map.height
        || !passable[home.y * map.width + home.x]
        || !passable[target.y * map.width + target.x]
    {
        return None;
    }
    let mut queue = VecDeque::from([home]);
    let mut parent = HashMap::from_iter([(home, None)]);
    while let Some(p) = queue.pop_front() {
        if p == target {
            let mut path = vec![p];
            let mut cursor = p;
            while let Some(Some(previous)) = parent.get(&cursor) {
                path.push(*previous);
                cursor = *previous;
            }
            return Some(path);
        }
        for (x, y) in map.get_adjacent(p.x, p.y) {
            let next = GridPosition { x, y };
            if passable[y * map.width + x]
                && !blocked.contains(&next)
                && !parent.contains_key(&next)
            {
                parent.insert(next, Some(p));
                queue.push_back(next);
            }
        }
    }
    None
}

#[derive(Resource, Default)]
struct CorridorCache {
    map: Option<Map>,
    by_capitals: HashMap<(GridPosition, GridPosition), bool>,
}

fn enabled_on_board(world: &mut World, player: PlayerId, target: GridPosition) -> bool {
    let home = world
        .iter_entities()
        .filter_map(|e| {
            let property = e.get::<Property>()?;
            (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                .then(|| e.get::<GridPosition>().copied())
                .flatten()
        })
        .min_by_key(|p| (p.y, p.x));
    let Some(home) = home else {
        return false;
    };
    let map = world.resource::<Map>();
    if let Some(cache) = world.get_resource::<CorridorCache>()
        && cache.map.as_ref() == Some(map)
        && let Some(result) = cache.by_capitals.get(&(home, target))
    {
        return *result;
    }
    let narrow = narrow_armor_corridor(map, world.resource::<MasterDataRegistry>(), home, target);
    let snapshot = map.clone();
    let mut cache = world.get_resource_or_insert_with(CorridorCache::default);
    if cache.map.as_ref() != Some(&snapshot) {
        cache.by_capitals.clear();
        cache.map = Some(snapshot);
    }
    cache.by_capitals.insert((home, target), narrow);
    narrow
}

/// GO中、狭い装甲経路に限って車列の前方から合法な前進を選ぶ。
/// 歩兵・輸送役・固定砲は従来の占領・輸送処理へ残す。
pub(crate) fn decide_action(
    world: &mut World,
    player: PlayerId,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let target = crate::ai::assault_decision::assault_objective(world, player)?;
    if !enabled_on_board(world, player, target) {
        return None;
    }
    let units = world
        .iter_entities()
        .filter_map(|e| {
            let stats = e.get::<UnitStats>()?;
            let pos = *e.get::<GridPosition>()?;
            if e.get::<Faction>()?.0 != player
                || e.get::<Transporting>().is_some()
                || skip.contains(&e.id())
                || stats.can_capture
                || stats.max_range == 0
                || stats.max_movement <= 1
                || stats.max_cargo > 0
                || matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
                || !crate::systems::action::is_unit_selectable(world, e.id())
                || e.get::<HasMoved>()?.0
                || e.get::<Ammo>()
                    .is_some_and(|a| a.ammo1 == 0 && a.ammo2 == 0)
            {
                return None;
            }
            Some((e.id(), pos, stats.clone(), e.get::<Fuel>()?.current))
        })
        .collect::<Vec<_>>();
    let occupied = world
        .iter_entities()
        .filter_map(|e| {
            if e.get::<Transporting>().is_some() {
                return None;
            }
            let p = e.get::<GridPosition>()?;
            let f = e.get::<Faction>()?;
            let s = e.get::<UnitStats>()?;
            Some((
                (p.x, p.y),
                OccupantInfo {
                    player_id: f.0,
                    is_transport: s.max_cargo > 0,
                    unit_type: s.unit_type,
                    loadable_types: s.loadable_unit_types.clone(),
                    free_slots: 0,
                },
            ))
        })
        .collect::<HashMap<_, _>>();
    let empty = HashMap::default();
    let mut cache = TurnDistanceCache::default();
    let mut best = None;
    for (actor, from, stats, fuel) in units {
        let current_targets =
            crate::systems::combat::get_attackable_targets_at(world, actor, from, true);
        let has_shot = current_targets.iter().any(|&enemy| {
            crate::ai::pruning::evaluate_attack_exchange(
                world,
                actor,
                enemy,
                from,
                world.resource::<DamageChart>(),
            )
            .is_some_and(|e| e.expected_damage > 0)
        });
        let range = if stats.min_range > 1 {
            stats.min_range
        } else {
            0
        };
        let before = calculate_turn_distance(
            world.resource::<Map>(),
            world.resource::<MasterDataRegistry>(),
            &empty,
            (from.x, from.y),
            (target.x, target.y),
            stats.movement_type,
            stats.max_movement,
            range,
            player,
            &mut cache,
        );
        if before.used_mp == u32::MAX {
            continue;
        }
        let reachable = calculate_reachable_tiles(
            world.resource::<Map>(),
            &occupied,
            (from.x, from.y),
            stats.movement_type,
            stats.max_movement,
            fuel,
            player,
            stats.unit_type,
            world.resource::<MasterDataRegistry>(),
        );
        for (x, y) in reachable {
            let to = GridPosition { x, y };
            if to == from || occupied.contains_key(&(x, y)) {
                continue;
            }
            let after = calculate_turn_distance(
                world.resource::<Map>(),
                world.resource::<MasterDataRegistry>(),
                &empty,
                (x, y),
                (target.x, target.y),
                stats.movement_type,
                stats.max_movement,
                range,
                player,
                &mut cache,
            );
            if after.used_mp >= before.used_mp {
                continue;
            }
            let targets = crate::systems::combat::get_attackable_targets_at(
                world,
                actor,
                to,
                stats.min_range > 1,
            );
            let mut attack = None;
            let mut future_fire = false;
            for enemy in targets {
                let Some(exchange) = crate::ai::pruning::evaluate_attack_exchange(
                    world,
                    actor,
                    enemy,
                    to,
                    world.resource::<DamageChart>(),
                ) else {
                    continue;
                };
                if exchange.expected_damage == 0 {
                    continue;
                }
                future_fire = true;
                let position = *world.get::<GridPosition>(enemy).unwrap();
                if stats.min_range <= 1
                    && exchange.expected_counter_damage
                        < world.get::<Health>(actor).unwrap().current
                {
                    let rank = (
                        exchange.expected_damage,
                        std::cmp::Reverse(
                            world
                                .resource::<Map>()
                                .distance(position.x, position.y, target.x, target.y),
                        ),
                    );
                    if attack.as_ref().is_none_or(|(old, _)| rank > *old) {
                        attack = Some((rank, enemy));
                    }
                }
            }
            // 現在撃てる間接部隊は、新位置でも敵を射程に収められる場合だけ射点を更新する。
            if has_shot && stats.min_range > 1 && !future_fire {
                continue;
            }
            let rank = (before.used_mp, after.used_mp, y, x, actor.to_bits());
            if best.as_ref().is_none_or(|(old, _, _)| rank < *old) {
                let command = attack.map_or(AiCommand::Wait { target_pos: to }, |(_, enemy)| {
                    AiCommand::Attack {
                        target_pos: to,
                        target_entity: enemy,
                    }
                });
                best = Some((rank, actor, command));
            }
        }
    }
    best.map(|(_, actor, command)| (actor, command))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::GridTopology;

    #[test]
    fn open_ground_does_not_enable_forced_advance() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = Map::new(8, 5, Terrain::Plains, GridTopology::Hex);
        assert!(!narrow_armor_corridor(
            &map,
            &registry,
            GridPosition { x: 1, y: 2 },
            GridPosition { x: 6, y: 2 }
        ));
    }

    #[test]
    fn required_bridge_or_mountain_corridor_enables_advance() {
        let registry = MasterDataRegistry::load().unwrap();
        let mut bridge = Map::new(8, 5, Terrain::Plains, GridTopology::Square);
        for y in 0..5 {
            bridge.set_terrain(4, y, Terrain::River).unwrap();
        }
        bridge.set_terrain(4, 2, Terrain::Bridge).unwrap();
        assert!(narrow_armor_corridor(
            &bridge,
            &registry,
            GridPosition { x: 1, y: 2 },
            GridPosition { x: 6, y: 2 }
        ));
        let mut corridor = Map::new(8, 5, Terrain::Mountain, GridTopology::Square);
        for x in 0..8 {
            corridor.set_terrain(x, 2, Terrain::Road).unwrap();
        }
        assert!(narrow_armor_corridor(
            &corridor,
            &registry,
            GridPosition { x: 1, y: 2 },
            GridPosition { x: 6, y: 2 }
        ));
    }

    #[test]
    fn real_map_corridors_match_the_intended_scope() {
        let registry = MasterDataRegistry::load().unwrap();
        // マップ名は回帰試験の入力だけに使い、実際の判定は地形と首都位置から行う。
        for (name, expected) in [
            ("map_20", true),
            ("map_47", true),
            ("map_25", false),
            ("map_6", false),
            ("map_40", true),
        ] {
            let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
                &registry,
                name,
                GridTopology::Hex,
            )
            .unwrap();
            let capital = |player| {
                world
                    .iter_entities()
                    .find_map(|entity| {
                        let property = entity.get::<Property>()?;
                        (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                            .then(|| entity.get::<GridPosition>().copied())
                            .flatten()
                    })
                    .expect("試験マップに首都が必要")
            };
            let first = capital(PlayerId(1));
            let second = capital(PlayerId(2));
            for (home, target) in [(first, second), (second, first)] {
                assert_eq!(
                    narrow_armor_corridor(world.resource::<Map>(), &registry, home, target),
                    expected,
                    "{name}: {home:?} -> {target:?}"
                );
            }
        }
    }

    #[test]
    fn disconnected_capitals_do_not_enable_land_push() {
        let registry = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(8, 5, Terrain::Plains, GridTopology::Square);
        for y in 0..5 {
            map.set_terrain(4, y, Terrain::Sea).unwrap();
        }
        assert!(!narrow_armor_corridor(
            &map,
            &registry,
            GridPosition { x: 1, y: 2 },
            GridPosition { x: 6, y: 2 }
        ));
    }
}
