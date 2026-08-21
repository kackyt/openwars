//! Gameboy Wars Turboの生産走査を模擬するV100/V200専用実装。

use super::compatibility_profile::{ProductionRole, preferred_production_role, production_role};
use crate::ai::engine::AiProductionCooldown;
use crate::components::{Faction, GridPosition, PlayerId, Property};
use crate::events::ProduceUnitCommand;
use crate::resources::{
    Map, MasterDataRegistry, MovementType, Players, Terrain, UnitRegistry, UnitType,
};
use bevy_ecs::prelude::*;
use std::cmp::Reverse;
use std::collections::HashSet;

/// V100/V200専用の単発生産判断。既存V1〜V4の生産計画を使用しない。
pub(crate) fn decide_production(
    world: &mut World,
    player_id: PlayerId,
) -> Option<ProduceUnitCommand> {
    let funds = world
        .get_resource::<Players>()?
        .0
        .iter()
        .find(|player| player.id == player_id)?
        .funds;
    let master_data = world.get_resource::<MasterDataRegistry>()?.clone();
    let registry = world.get_resource::<UnitRegistry>()?.clone();
    let used = world
        .get_resource::<AiProductionCooldown>()
        .map(|value| value.0.clone())
        .unwrap_or_default();
    let (occupied, friendly_positions, friendly_unit_count, air_unit_count, ship_unit_count): (
        HashSet<_>,
        HashSet<_>,
        usize,
        usize,
        usize,
    ) = {
        let mut query = world.query::<(&GridPosition, &Faction, &crate::components::UnitStats)>();
        let mut positions = HashSet::new();
        let mut friendly = HashSet::new();
        let mut air_units = 0;
        let mut ship_units = 0;
        let mut unit_count = 0;
        for (position, faction, stats) in query.iter(world) {
            positions.insert((position.x, position.y));
            if faction.0 == player_id {
                unit_count += 1;
                friendly.insert((position.x, position.y));
                if stats.movement_type == MovementType::Air {
                    air_units += 1;
                } else if stats.movement_type == MovementType::Ship {
                    ship_units += 1;
                }
            }
        }
        (positions, friendly, unit_count, air_units, ship_units)
    };
    let all_properties: Vec<_> = {
        let mut query = world.query::<(&GridPosition, &Property)>();
        query
            .iter(world)
            .map(|(position, property)| (*position, *property))
            .collect()
    };
    let all_owned_facilities: Vec<_> = all_properties
        .iter()
        .filter_map(|(position, property)| {
            (property.owner_id == Some(player_id)
                && matches!(
                    property.terrain,
                    Terrain::Capital | Terrain::Factory | Terrain::Airport | Terrain::Port
                ))
            .then_some((*position, property.terrain))
        })
        .collect();
    let map = world.get_resource::<Map>()?.clone();

    // GB版では都市は補給地点だが生産地点ではない。
    let mut facilities: Vec<_> = all_owned_facilities
        .iter()
        .copied()
        .filter(|(position, _)| {
            !occupied.contains(&(position.x, position.y))
                && !used.contains(&(position.x, position.y))
        })
        .collect();
    let owned_capital = all_owned_facilities
        .iter()
        .find_map(|(position, terrain)| (*terrain == Terrain::Capital).then_some(*position));
    let enemy_capital = all_properties.iter().find_map(|(position, property)| {
        (property.terrain == Terrain::Capital && property.owner_id != Some(player_id))
            .then_some(*position)
    });
    if let Some(capital) = owned_capital {
        let forward_y = enemy_capital.map_or(1, |enemy| if enemy.y >= capital.y { 1 } else { -1 });
        // ROMの施設テーブルは首都を先頭に、敵首都方向の列を右から、同じ行を
        // 右から左へ並べる。特定マップの座標ではなく、両首都の相対方向から順序を決める。
        facilities.sort_by_key(|(position, terrain)| {
            production_facility_priority(*position, *terrain, capital, forward_y)
        });
    } else {
        facilities.sort_by_key(|(position, _)| (position.y, position.x));
    }
    // 初回手番だけ共通フェーズイベントがcooldownを消す場合があるため、すでに
    // 部隊で埋まった生産施設も発行済みスロットとして数える。
    let occupied_facility_count = all_owned_facilities
        .iter()
        .filter(|(position, _)| friendly_positions.contains(&(position.x, position.y)))
        .count();
    let production_slot = used.len().max(occupied_facility_count);
    if map_requires_mobile_transport(&map, &all_properties, player_id) {
        if let Some(role) = island_opening_role(friendly_unit_count) {
            let mut island_facilities = facilities.clone();
            if let Some(capital) = owned_capital {
                let advance_right = enemy_capital.is_none_or(|enemy| enemy.x >= capital.x);
                island_facilities.sort_by_key(|(position, terrain)| {
                    island_facility_priority(*position, *terrain, capital, advance_right)
                });
            }
            return choose_island_opening_production(IslandOpeningContext {
                player_id,
                funds,
                role,
                master_data: &master_data,
                registry: &registry,
                facilities: &island_facilities,
            });
        }
        if !used.is_empty() {
            // 初期編成表の末尾へ到達した手番ではそこで生産走査を終える。
            // 次の手番（cooldownが空）からは盤面編成による通常判断へ移行する。
            return None;
        }
    }
    // GBの最初の生産手番は5施設すべて歩兵系。全初期部隊が施設上にいる間だけ
    // 初期配備とみなし、次の手番から観測済みの3兵種周期へ移る。
    let initial_deployment = friendly_positions.len() == occupied_facility_count;
    let preferred_role = if initial_deployment {
        ProductionRole::Capturer
    } else {
        preferred_production_role(production_slot)
    };

    for (position, terrain) in facilities {
        if matches!(terrain, Terrain::Airport | Terrain::Port) {
            if let Some(unit_type) = preferred_transport_facility_unit(
                &registry,
                &master_data,
                terrain,
                funds,
                if terrain == Terrain::Airport {
                    air_unit_count
                } else {
                    ship_unit_count
                },
            ) {
                return Some(ProduceUnitCommand {
                    player_id,
                    target_x: position.x,
                    target_y: position.y,
                    unit_type,
                });
            }
            continue;
        }
        let mut preferred = None;
        let mut fallback = None;
        for (unit_type, stats) in &registry.0 {
            if stats.cost > funds || !master_data.can_produce_unit(terrain.as_str(), *unit_type) {
                continue;
            }
            let key = (stats.cost, *unit_type as u8);
            if fallback.as_ref().is_none_or(|(current, _)| key < *current) {
                fallback = Some((key, *unit_type));
            }
            if production_role(stats) == Some(preferred_role)
                && preferred.as_ref().is_none_or(|(current, _)| key < *current)
            {
                preferred = Some((key, *unit_type));
            }
        }
        if let Some((_, unit_type)) = preferred.or(fallback) {
            return Some(ProduceUnitCommand {
                player_id,
                target_x: position.x,
                target_y: position.y,
                unit_type,
            });
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IslandOpeningRole {
    Capturer,
    MobileCombat,
    MobileTransport,
    DirectGround,
    FastGround,
}

struct IslandOpeningContext<'a> {
    player_id: PlayerId,
    funds: u32,
    role: IslandOpeningRole,
    master_data: &'a MasterDataRegistry,
    registry: &'a UnitRegistry,
    facilities: &'a [(GridPosition, Terrain)],
}

/// ROMの初期部隊レコード数に応じた島嶼マップ用の編成表。
/// 座標やマップ名ではなく、現在生存している編成から次に不足する能力を決める。
fn island_opening_role(unit_count: usize) -> Option<IslandOpeningRole> {
    use IslandOpeningRole::{
        Capturer as I, DirectGround as A, FastGround as R, MobileCombat as B, MobileTransport as T,
    };
    const OPENING: [IslandOpeningRole; 20] =
        [I, I, I, B, I, I, I, B, I, T, I, I, T, A, I, I, B, T, T, R];
    OPENING.get(unit_count).copied()
}

fn choose_island_opening_production(
    context: IslandOpeningContext<'_>,
) -> Option<ProduceUnitCommand> {
    for (position, terrain) in context.facilities {
        let unit_type = context
            .registry
            .0
            .iter()
            .filter(|(unit_type, stats)| {
                stats.cost <= context.funds
                    && context
                        .master_data
                        .can_produce_unit(terrain.as_str(), **unit_type)
                    && island_role_matches(stats, context.role)
            })
            .min_by_key(|(unit_type, stats)| (stats.cost, **unit_type as u8))
            .map(|(unit_type, _)| *unit_type);
        if let Some(unit_type) = unit_type {
            return Some(ProduceUnitCommand {
                player_id: context.player_id,
                target_x: position.x,
                target_y: position.y,
                unit_type,
            });
        }
    }
    None
}

fn island_role_matches(stats: &crate::components::UnitStats, role: IslandOpeningRole) -> bool {
    match role {
        IslandOpeningRole::Capturer => stats.can_capture,
        IslandOpeningRole::MobileCombat => {
            matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
                && !super::compatibility_profile::is_gbw_transport(stats)
                && (stats.max_ammo1 > 0 || stats.max_ammo2 > 0)
        }
        IslandOpeningRole::MobileTransport => super::compatibility_profile::is_gbw_transport(stats),
        IslandOpeningRole::DirectGround => {
            stats.movement_type == MovementType::Tank
                && stats.max_movement >= 5
                && stats.min_range <= 1
                && (stats.max_ammo1 > 0 || stats.max_ammo2 > 0)
        }
        IslandOpeningRole::FastGround => production_role(stats) == Some(ProductionRole::FastGround),
    }
}

fn map_requires_mobile_transport(
    map: &Map,
    properties: &[(GridPosition, Property)],
    player_id: PlayerId,
) -> bool {
    let island_map = crate::ai::islands::IslandMap::analyze(map);
    let base_island = properties
        .iter()
        .find(|(_, property)| {
            property.owner_id == Some(player_id) && property.terrain == Terrain::Capital
        })
        .and_then(|(position, _)| island_map.get_island_at(position))
        .map(|island| island.id);
    properties.iter().any(|(position, property)| {
        property.owner_id != Some(player_id)
            && property.max_capture_points > 0
            && island_map
                .get_island_at(position)
                .is_some_and(|island| Some(island.id) != base_island)
    })
}

fn island_facility_priority(
    position: GridPosition,
    terrain: Terrain,
    capital: GridPosition,
    advance_right: bool,
) -> (u8, usize, Reverse<usize>, u8) {
    let capital_rank = u8::from(position != capital || terrain != Terrain::Capital);
    let horizontal_rank = if advance_right {
        usize::MAX - position.x
    } else {
        position.x
    };
    (
        capital_rank,
        horizontal_rank,
        Reverse(position.y),
        terrain as u8,
    )
}

/// 空港・港では、ROMで観測した「戦闘輸送輸送」の巡回を能力で対応付ける。
/// 最初の1機だけ戦闘部隊を先行させ、その後は3機周期で護衛1・輸送2とする。
fn preferred_transport_facility_unit(
    registry: &UnitRegistry,
    master_data: &MasterDataRegistry,
    terrain: Terrain,
    funds: u32,
    produced_mobile_count: usize,
) -> Option<UnitType> {
    let prefer_combat = prefers_combat_mobile(produced_mobile_count);
    registry
        .0
        .iter()
        .filter(|(unit_type, stats)| {
            stats.cost <= funds
                && master_data.can_produce_unit(terrain.as_str(), **unit_type)
                && if prefer_combat {
                    !super::compatibility_profile::is_gbw_transport(stats)
                        && (stats.max_ammo1 > 0 || stats.max_ammo2 > 0)
                } else {
                    super::compatibility_profile::is_gbw_transport(stats)
                }
        })
        .min_by_key(|(unit_type, stats)| (stats.cost, **unit_type as u8))
        .map(|(unit_type, _)| *unit_type)
}

fn prefers_combat_mobile(produced_mobile_count: usize) -> bool {
    produced_mobile_count == 0 || (produced_mobile_count - 1).is_multiple_of(3)
}

fn production_facility_priority(
    position: GridPosition,
    terrain: Terrain,
    capital: GridPosition,
    forward_y: i32,
) -> (u8, u8, Reverse<i32>, Reverse<usize>, usize) {
    if terrain == Terrain::Capital {
        return (0, 0, Reverse(0), Reverse(position.x), position.y);
    }
    let progress = (position.y as i32 - capital.y as i32) * forward_y;
    let band = if progress > 0 {
        0
    } else if progress == 0 {
        1
    } else {
        2
    };
    (1, band, Reverse(progress), Reverse(position.x), position.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map1_red_facilities_follow_observed_rom_order() {
        let capital = GridPosition { x: 6, y: 3 };
        let mut facilities = [
            (GridPosition { x: 5, y: 3 }, Terrain::Factory),
            (capital, Terrain::Capital),
            (GridPosition { x: 7, y: 3 }, Terrain::Factory),
            (GridPosition { x: 6, y: 4 }, Terrain::Factory),
            (GridPosition { x: 7, y: 4 }, Terrain::Factory),
        ];
        facilities.sort_by_key(|(position, terrain)| {
            production_facility_priority(*position, *terrain, capital, 1)
        });

        assert_eq!(
            facilities.map(|(position, _)| position),
            [
                GridPosition { x: 6, y: 3 },
                GridPosition { x: 7, y: 4 },
                GridPosition { x: 6, y: 4 },
                GridPosition { x: 7, y: 3 },
                GridPosition { x: 5, y: 3 },
            ]
        );
    }

    #[test]
    fn map1_white_facilities_mirror_the_same_rom_order() {
        let capital = GridPosition { x: 3, y: 11 };
        let mut facilities = [
            (GridPosition { x: 3, y: 10 }, Terrain::Factory),
            (GridPosition { x: 4, y: 10 }, Terrain::Factory),
            (GridPosition { x: 2, y: 11 }, Terrain::Factory),
            (capital, Terrain::Capital),
            (GridPosition { x: 4, y: 11 }, Terrain::Factory),
        ];
        facilities.sort_by_key(|(position, terrain)| {
            production_facility_priority(*position, *terrain, capital, -1)
        });

        assert_eq!(
            facilities.map(|(position, _)| position),
            [
                GridPosition { x: 3, y: 11 },
                GridPosition { x: 4, y: 10 },
                GridPosition { x: 3, y: 10 },
                GridPosition { x: 4, y: 11 },
                GridPosition { x: 2, y: 11 },
            ]
        );
    }

    #[test]
    fn mobile_mix_is_derived_from_composition_instead_of_map_or_turn() {
        let sequence: Vec<_> = (0..7).map(prefers_combat_mobile).collect();

        // 観測した B,B,T,T,B,T,T は、マップ座標ではなく現在編成から得られる。
        assert_eq!(sequence, vec![true, true, false, false, true, false, false]);
    }
}
