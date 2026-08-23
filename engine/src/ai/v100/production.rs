//! Gameboy Wars Turboの生産状態機械を移植したV100/V200専用実装。

use crate::ai::engine::AiProductionCooldown;
use crate::components::{Faction, GridPosition, Health, PlayerId, Property};
use crate::events::ProduceUnitCommand;
use crate::resources::{
    DamageChart, Map, MasterDataRegistry, MatchState, Players, Terrain, UnitRegistry, UnitType,
};
use bevy_ecs::prelude::*;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

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
    let damage_chart = world.get_resource::<DamageChart>()?.clone();
    let map = world.get_resource::<Map>()?.clone();
    let scenario = super::rom_data::identify_scenario(&map, &master_data)?;
    let turn = world.get_resource::<MatchState>()?.current_turn_number.0;
    let used = world
        .get_resource::<AiProductionCooldown>()
        .map(|value| value.0.clone())
        .unwrap_or_default();
    let (occupied, friendly_counts, enemy_counts, own_force_value, enemy_force_value) = {
        let mut query = world.query::<(
            &GridPosition,
            &Faction,
            &crate::components::UnitStats,
            &Health,
        )>();
        let mut positions = HashSet::new();
        let mut friendly = HashMap::<UnitType, u32>::new();
        let mut enemy = HashMap::<UnitType, u32>::new();
        let mut own_value = 0_u64;
        let mut enemy_value = 0_u64;
        for (position, faction, stats, health) in query.iter(world) {
            positions.insert((position.x, position.y));
            // ROM 6317/635Bは5152のシナリオ別兵種価値へレコード+8のHPを掛ける。
            let force_value = u64::from(scenario.unit_value(stats.unit_type))
                .saturating_mul(u64::from(health.current))
                / u64::from(health.max.max(1));
            if faction.0 == player_id {
                *friendly.entry(stats.unit_type).or_default() += 1;
                own_value = own_value.saturating_add(force_value);
            } else {
                *enemy.entry(stats.unit_type).or_default() += 1;
                enemy_value = enemy_value.saturating_add(force_value);
            }
        }
        (positions, friendly, enemy, own_value, enemy_value)
    };
    // ROM 5EA1は1陣営40部隊で生産を打ち切る。
    //if friendly_counts.values().copied().sum::<u32>() >= 40 {
    //    return None;
    //}
    let all_properties: Vec<_> = {
        let mut query = world.query::<(&GridPosition, &Property)>();
        query
            .iter(world)
            .map(|(position, property)| (*position, *property))
            .collect()
    };
    let owned_capitals: Vec<_> = all_properties
        .iter()
        .filter_map(|(position, property)| {
            (property.owner_id == Some(player_id) && property.terrain == Terrain::Capital)
                .then_some(*position)
        })
        .collect();
    let all_owned_facilities: Vec<_> = all_properties
        .iter()
        .filter_map(|(position, property)| {
            (property.owner_id == Some(player_id)
                && is_rom_production_terrain(property.terrain)
                // ROM 2259/22C1は自軍首都から疑似hex距離3以内だけを許可する。
                && crate::systems::production::is_within_production_range(
                    &owned_capitals,
                    position.x,
                    position.y,
                    map.topology,
                ))
            .then_some((*position, property.terrain))
        })
        .collect();
    let own_property_count = all_properties
        .iter()
        .filter(|(_, property)| property.owner_id == Some(player_id))
        .count() as u64;
    let enemy_property_count = all_properties
        .iter()
        .filter(|(_, property)| {
            property
                .owner_id
                .is_some_and(|owner_id| owner_id != player_id)
        })
        .count() as u64;
    // ROM 5FA8は序盤期限中だけC6AD=0を保持する。期限を過ぎると、既存のC6ADが
    // 非0でも6317/635Bを毎回呼び、戦力比と拠点比から1〜3を引き直す。
    let strategy = if turn <= scenario.opening_limit {
        super::rom_logic::ProductionStrategy::Opening
    } else {
        let own_share_percent = combined_force_and_property_share(
            own_force_value,
            enemy_force_value,
            own_property_count,
            enemy_property_count,
        );
        super::rom_logic::production_strategy(own_share_percent)
    };
    world
        .get_resource_or_insert_with(super::rom_logic::RomAiState::default)
        .set_production_strategy(player_id, strategy);

    // ROM 5F2Cは地上部隊について首都0x30、工場0x32、都市0x3Aを順に試す。
    let mut facilities: Vec<_> = all_owned_facilities
        .iter()
        .copied()
        .filter(|(position, _)| {
            !occupied.contains(&(position.x, position.y))
                && !used.contains(&(position.x, position.y))
        })
        .collect();
    let enemy_capital = all_properties.iter().find_map(|(position, property)| {
        (property.terrain == Terrain::Capital && property.owner_id != Some(player_id))
            .then_some(*position)
    });
    if let Some(enemy_capital) = enemy_capital {
        // ROM 43CC/5F51は敵首都を起点に疑似hexの一様距離フィールドを作り、値が
        // 小さい生産施設を選ぶ。同値ならCEA0の施設レコードを後から走査した側が勝つ。
        facilities.sort_by_key(|(position, terrain)| {
            production_facility_priority(*position, *terrain, enemy_capital, &map)
        });
    } else {
        facilities.sort_by_key(|(position, terrain)| {
            (
                u8::from(*terrain != Terrain::Capital),
                Reverse(position.y),
                Reverse(position.x),
            )
        });
    }
    let (mobility_shortages, pickup_candidates) = world
        .get_resource::<super::rom_logic::RomAiState>()
        .map_or((0, 0), |state| state.production_counters(player_id));
    if strategy != super::rom_logic::ProductionStrategy::Opening {
        let special_mode = world
            .get_resource_or_insert_with(super::rom_logic::RomAiState::default)
            .special_production_mode_for_turn(
                player_id,
                turn,
                mobility_shortages,
                pickup_candidates,
            );
        if let Some(mode) = special_mode {
            for unit_type in special_production_types(mode, scenario.has_radar_transport) {
                if let Some(command) = command_for_unit(
                    player_id,
                    unit_type,
                    funds,
                    strategy,
                    scenario,
                    &friendly_counts,
                    &facilities,
                    &registry,
                    &master_data,
                ) {
                    let mut state =
                        world.get_resource_or_insert_with(super::rom_logic::RomAiState::default);
                    match unit_type {
                        UnitType::TransportHelicopter => {
                            state.consume_pickup_candidates(player_id, 1)
                        }
                        UnitType::Recon => state.consume_pickup_candidates(player_id, 2),
                        UnitType::Lander => state.consume_mobility_shortages(player_id, 2),
                        _ => {}
                    }
                    return Some(command);
                }
            }
            // 特殊生産を選んだ手番は、その候補を作れなくなった時点で終了する。
            return None;
        }
    }

    let mut unit_types: Vec<_> = registry.0.keys().copied().collect();
    unit_types.sort_by_key(|unit_type| super::rom_logic::GbUnitKind::production_order(*unit_type));
    let mut best: Option<(u32, ProduceUnitCommand)> = None;
    for unit_type in unit_types {
        let Some(command) = command_for_unit(
            player_id,
            unit_type,
            funds,
            strategy,
            scenario,
            &friendly_counts,
            &facilities,
            &registry,
            &master_data,
        ) else {
            continue;
        };
        let remaining = scenario
            .production_limit(strategy, unit_type)
            .saturating_sub(friendly_counts.get(&unit_type).copied().unwrap_or_default());
        let score = if strategy == super::rom_logic::ProductionStrategy::Opening {
            // ROM 6367: 5152(kind, 99) × 残り保有枠。
            scenario
                .unit_value(unit_type)
                .saturating_mul(99)
                .saturating_mul(remaining)
        } else {
            // ROM 6190: 候補と敵兵種の各組について、攻撃相性×敵数×残り枠の最大値。
            enemy_counts
                .iter()
                .map(|(enemy_type, count)| {
                    let damage = damage_chart
                        .get_base_damage(unit_type, *enemy_type)
                        .unwrap_or_default()
                        .max(
                            damage_chart
                                .get_base_damage_secondary(unit_type, *enemy_type)
                                .unwrap_or_default(),
                        );
                    damage.saturating_mul(*count).saturating_mul(remaining)
                })
                .max()
                .unwrap_or_default()
        };
        // ROM 6205/63C6は厳密に大きい場合だけ更新し、兵種表で先の候補を保持する。
        if score > best.as_ref().map_or(0, |(best_score, _)| *best_score) {
            best = Some((score, command));
        }
    }
    if let Some((_, command)) = best {
        return Some(command);
    }

    // ROM 640Cはその生産フェーズでまだ1部隊も作っていない場合だけ歩兵を試す。
    used.is_empty().then(|| {
        command_for_unit(
            player_id,
            UnitType::Infantry,
            funds,
            strategy,
            scenario,
            &friendly_counts,
            &facilities,
            &registry,
            &master_data,
        )
    })?
}

fn is_rom_production_terrain(terrain: Terrain) -> bool {
    matches!(
        terrain,
        Terrain::Capital | Terrain::City | Terrain::Factory | Terrain::Airport | Terrain::Port
    )
}

#[allow(clippy::too_many_arguments)]
fn command_for_unit(
    player_id: PlayerId,
    unit_type: UnitType,
    funds: u32,
    strategy: super::rom_logic::ProductionStrategy,
    scenario: super::rom_data::RomScenarioData,
    friendly_counts: &HashMap<UnitType, u32>,
    facilities: &[(GridPosition, Terrain)],
    registry: &UnitRegistry,
    master_data: &MasterDataRegistry,
) -> Option<ProduceUnitCommand> {
    let stats = registry.0.get(&unit_type)?;
    let current = friendly_counts.get(&unit_type).copied().unwrap_or_default();
    if stats.cost > funds || current >= scenario.production_limit(strategy, unit_type) {
        return None;
    }
    facilities
        .iter()
        .find(|(_, terrain)| master_data.can_produce_unit(terrain.as_str(), unit_type))
        .map(|(position, _)| ProduceUnitCommand {
            player_id,
            target_x: position.x,
            target_y: position.y,
            unit_type,
        })
}

fn special_production_types(
    mode: super::rom_logic::SpecialProductionMode,
    has_radar_transport: bool,
) -> Vec<UnitType> {
    match mode {
        super::rom_logic::SpecialProductionMode::Mobility => {
            let mut result = vec![UnitType::Lander];
            if has_radar_transport {
                // 0x20レーダー輸送機はOpenWarsでは最も近い輸送ヘリへ対応させる。
                result.push(UnitType::TransportHelicopter);
            }
            result
        }
        super::rom_logic::SpecialProductionMode::Pickup => {
            vec![UnitType::TransportHelicopter, UnitType::Recon]
        }
    }
}

fn production_facility_priority(
    position: GridPosition,
    terrain: Terrain,
    enemy_capital: GridPosition,
    map: &Map,
) -> (u8, u32, Reverse<usize>, Reverse<usize>) {
    if terrain == Terrain::Capital {
        return (0, 0, Reverse(position.y), Reverse(position.x));
    }
    (
        1,
        map.distance(position.x, position.y, enemy_capital.x, enemy_capital.y),
        Reverse(position.y),
        Reverse(position.x),
    )
}

/// ROM 5FA8〜6026と同じく、戦力比と所有拠点比を百分率にして平均する。
fn combined_force_and_property_share(
    own_force: u64,
    enemy_force: u64,
    own_properties: u64,
    enemy_properties: u64,
) -> u32 {
    fn share(own: u64, enemy: u64) -> u32 {
        let total = own.saturating_add(enemy);
        if total == 0 {
            50
        } else {
            own.saturating_mul(100).saturating_div(total) as u32
        }
    }

    (share(own_force, enemy_force) + share(own_properties, enemy_properties)) / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn city_is_a_ground_production_facility_like_rom_5f2c() {
        let master_data = MasterDataRegistry::load().unwrap();
        let capitals = [GridPosition { x: 0, y: 0 }];

        assert!(is_rom_production_terrain(Terrain::City));
        assert!(master_data.can_produce_unit(Terrain::City.as_str(), UnitType::Rockets));
        assert!(crate::systems::production::is_within_production_range(
            &capitals,
            3,
            0,
            crate::resources::GridTopology::Square,
        ));
        assert!(!crate::systems::production::is_within_production_range(
            &capitals,
            4,
            0,
            crate::resources::GridTopology::Square,
        ));
    }

    #[test]
    fn map1_red_facilities_follow_observed_rom_order() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_1",
            crate::resources::GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>();
        let capital = GridPosition { x: 6, y: 3 };
        let enemy_capital = GridPosition { x: 3, y: 11 };
        let mut facilities = [
            (GridPosition { x: 5, y: 3 }, Terrain::Factory),
            (capital, Terrain::Capital),
            (GridPosition { x: 7, y: 3 }, Terrain::Factory),
            (GridPosition { x: 6, y: 4 }, Terrain::Factory),
            (GridPosition { x: 7, y: 4 }, Terrain::Factory),
        ];
        facilities.sort_by_key(|(position, terrain)| {
            production_facility_priority(*position, *terrain, enemy_capital, map)
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
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_1",
            crate::resources::GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>();
        let capital = GridPosition { x: 3, y: 11 };
        let enemy_capital = GridPosition { x: 6, y: 3 };
        let mut facilities = [
            (GridPosition { x: 3, y: 10 }, Terrain::Factory),
            (GridPosition { x: 4, y: 10 }, Terrain::Factory),
            (GridPosition { x: 2, y: 11 }, Terrain::Factory),
            (capital, Terrain::Capital),
            (GridPosition { x: 4, y: 11 }, Terrain::Factory),
        ];
        facilities.sort_by_key(|(position, terrain)| {
            production_facility_priority(*position, *terrain, enemy_capital, map)
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
    fn map3_red_factories_follow_rom_field_values_and_late_ties() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_3",
            crate::resources::GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>();
        let enemy_capital = GridPosition { x: 23, y: 23 };
        let mut facilities = [
            (GridPosition { x: 4, y: 5 }, Terrain::Factory),
            (GridPosition { x: 6, y: 5 }, Terrain::Factory),
            (GridPosition { x: 5, y: 6 }, Terrain::Factory),
            (GridPosition { x: 6, y: 6 }, Terrain::Factory),
        ];
        facilities.sort_by_key(|(position, terrain)| {
            production_facility_priority(*position, *terrain, enemy_capital, map)
        });

        assert_eq!(
            facilities.map(|(position, _)| position),
            [
                GridPosition { x: 6, y: 6 },
                GridPosition { x: 6, y: 5 },
                GridPosition { x: 5, y: 6 },
                GridPosition { x: 4, y: 5 },
            ]
        );
    }

    #[test]
    fn production_strategy_share_averages_force_and_properties_like_rom() {
        assert_eq!(combined_force_and_property_share(60, 40, 8, 2), 70);
        assert_eq!(combined_force_and_property_share(40, 60, 2, 8), 30);
        assert_eq!(combined_force_and_property_share(0, 0, 0, 0), 50);
    }
}
