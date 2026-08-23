//! Gameboy Wars Turboの部隊選択・行動判定を模擬するV100/V200専用実装。
//!
//! ROMは行動を単一の加重点へ変換せず、部隊選択後に占領、攻撃、IQ200固有の
//! 合流、通常移動を順番に試す。この段階構造とWRAM候補盤面の行優先走査を保つ。

use super::candidate_field::{
    CandidateTile, build_candidate_field, build_load_candidate_field, build_merge_candidate_field,
};
use super::route_field::{build_route_field, build_route_field_to_any};
use crate::ai::AiVersion;
use crate::ai::engine::AiCommand;
use crate::components::{
    ActionCompleted, Ammo, Faction, Fuel, GridPosition, HasMoved, Health, PlayerId, Property,
    UnitStats,
};
use crate::resources::{DamageChart, Map, MasterDataRegistry, MatchState, MovementType, Terrain};
use crate::systems::combat::get_expected_damage;
use crate::systems::movement::OccupantInfo;
use bevy_ecs::prelude::*;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

/// GBの部隊レコードに保持される個別目標座標に相当するV100/V200専用状態。
///
/// AIは1手番を複数のstepに分けて実行するため、毎stepで現在位置から目標を
/// 引き直すと、先に動いた部隊の影響で後続部隊の目標が変わってしまう。
#[derive(Resource, Default)]
struct ObjectiveAssignmentState {
    by_player: HashMap<PlayerId, HashMap<Entity, RomUnitObjective>>,
    mission_by_player: HashMap<PlayerId, HashMap<Entity, u8>>,
    cargo_loaded_by_player: HashMap<PlayerId, HashMap<Entity, bool>>,
    turn_by_player: HashMap<PlayerId, u32>,
}

/// ROM部隊レコード+13/+14の個別目標。
/// `0x40,0x40`は座標ではなく、Bank 2 `40E1`へ動的影響目標を選ばせる番兵値である。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RomUnitObjective {
    Position(GridPosition),
    DynamicInfluence,
}

impl RomUnitObjective {
    fn position(self) -> Option<GridPosition> {
        match self {
            Self::Position(position) => Some(position),
            Self::DynamicInfluence => None,
        }
    }
}

impl PartialEq<GridPosition> for RomUnitObjective {
    fn eq(&self, other: &GridPosition) -> bool {
        self.position() == Some(*other)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RomInfluence {
    anti_air: u8,
    anti_surface: u8,
}

impl RomInfluence {
    fn add(&mut self, anti_air: u8, anti_surface: u8) {
        self.anti_air = self.anti_air.saturating_add(anti_air).min(7);
        self.anti_surface = self.anti_surface.saturating_add(anti_surface).min(7);
    }

    fn combined(self) -> u8 {
        self.anti_air + self.anti_surface
    }
}

/// ROM `4299`がAI手番開始時に一度だけ構築するD7E4影響盤面の陣営別スナップショット。
#[derive(Resource, Default)]
struct RomInfluenceState {
    by_player: HashMap<PlayerId, (u32, HashMap<GridPosition, RomInfluence>)>,
}

#[derive(Clone, Copy)]
struct AssignmentUnit {
    entity: Entity,
    position: GridPosition,
    unit_type: crate::resources::UnitType,
    record_order: u32,
    has_cargo: bool,
    is_transported: bool,
}

#[derive(Clone)]
struct UnitView {
    entity: Entity,
    record_order: u32,
    position: GridPosition,
    stats: UnitStats,
    hp: u32,
    max_hp: u32,
    fuel: u32,
    ammo: (u32, u32),
}

#[derive(Clone)]
struct EnemyView {
    entity: Entity,
    position: GridPosition,
    stats: UnitStats,
    hp: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct AttackKey {
    /// ROMの攻撃相性表に対応する代替量。撃破後の残HPが小さい対象を先にする。
    target_remaining_hp: u32,
    /// 攻撃後に残る地形の防御値が高い方を先にする。
    defensive_position: Reverse<u32>,
    /// 同じ残HPなら、元から損耗している対象を先にする。
    target_hp: u32,
}

struct AttackEvaluationContext<'a> {
    map: &'a Map,
    master_data: &'a MasterDataRegistry,
    damage_chart: &'a DamageChart,
    properties: &'a [(GridPosition, Property)],
    player_id: PlayerId,
    version: AiVersion,
}

/// ROMの固定目標選択に必要な盤面情報をまとめる。
struct StrategicObjectiveContext<'a> {
    scenario: Option<super::rom_data::RomScenarioData>,
    properties: &'a [(GridPosition, Property)],
    map: &'a Map,
    master_data: &'a MasterDataRegistry,
    player_id: PlayerId,
    reserved: &'a HashSet<GridPosition>,
}

/// ROMの番兵目標と輸送船の積荷目標を、実際の経路目標へ解決するための参照群。
struct RomMovementObjectiveContext<'a> {
    cargo: Option<&'a [Entity]>,
    assignments: &'a HashMap<Entity, RomUnitObjective>,
    influence: &'a HashMap<GridPosition, RomInfluence>,
    properties: &'a [(GridPosition, Property)],
    map: &'a Map,
    master_data: &'a MasterDataRegistry,
    player_id: PlayerId,
}

/// V100/V200の次の行動を決める。既存V1〜V4の評価器は呼び出さない。
pub(crate) fn decide_action(
    world: &mut World,
    player_id: PlayerId,
    skipped: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let map = world.get_resource::<Map>()?.clone();
    let master_data = world.get_resource::<MasterDataRegistry>()?.clone();
    let damage_chart = world.get_resource::<DamageChart>()?.clone();
    let scenario = super::rom_data::identify_scenario(&map, &master_data);
    let version = crate::ai::resolve_player_ai_version(world, player_id);
    let turn = world.get_resource::<MatchState>()?.current_turn_number.0;
    world
        .get_resource_or_insert_with(super::rom_logic::RomAiState::default)
        .begin_action_turn(player_id, turn);
    let mut units = Vec::new();
    let mut assignment_units = Vec::new();
    let mut occupants = HashMap::new();
    let mut friendly_by_position = HashMap::new();
    let mut cargo_by_transport = HashMap::<Entity, Vec<Entity>>::new();
    {
        let mut query = world.query::<(
            Entity,
            &GridPosition,
            &Faction,
            &UnitStats,
            &Health,
            &Fuel,
            Option<&Ammo>,
            &HasMoved,
            &ActionCompleted,
            Option<&crate::components::CargoCapacity>,
            Option<&crate::components::Transporting>,
        )>();
        for (
            entity,
            position,
            faction,
            stats,
            health,
            fuel,
            ammo,
            moved,
            completed,
            cargo,
            transporting,
        ) in query.iter(world)
        {
            if health.current == 0 {
                continue;
            }
            if faction.0 == player_id {
                assignment_units.push(AssignmentUnit {
                    entity,
                    position: *position,
                    unit_type: stats.unit_type,
                    record_order: 0,
                    has_cargo: cargo.is_some_and(|value| !value.loaded.is_empty()),
                    is_transported: transporting.is_some(),
                });
                if let Some(cargo) = cargo
                    && !cargo.loaded.is_empty()
                {
                    cargo_by_transport.insert(entity, cargo.loaded.clone());
                }
            }
            // ROMの搭載中レコードは個別目標を保持するが、盤上の占有・行動候補には入らない。
            if transporting.is_some() {
                continue;
            }
            occupants.insert(
                (position.x, position.y),
                OccupantInfo {
                    player_id: faction.0,
                    is_transport: super::compatibility_profile::is_gbw_transport(stats),
                    unit_type: stats.unit_type,
                    loadable_types: stats.loadable_unit_types.clone(),
                    free_slots: cargo.map_or(stats.max_cargo, |value| {
                        value.max.saturating_sub(value.loaded.len() as u32)
                    }),
                },
            );
            if faction.0 == player_id {
                friendly_by_position.insert((position.x, position.y), entity);
                if !moved.0 && !completed.0 && !skipped.contains(&entity) {
                    units.push(UnitView {
                        entity,
                        record_order: 0,
                        position: *position,
                        stats: stats.clone(),
                        hp: health.current,
                        max_hp: health.max,
                        fuel: fuel.current,
                        ammo: ammo.map_or((0, 0), |value| (value.ammo1, value.ammo2)),
                    });
                }
            }
        }
    }
    let observed_records: Vec<_> = assignment_units
        .iter()
        .map(|unit| (unit.entity, unit.position, unit.unit_type))
        .collect();
    let record_orders =
        super::unit_record::synchronize_unit_records(world, player_id, &observed_records);
    for unit in &mut assignment_units {
        unit.record_order = record_orders
            .get(&unit.entity)
            .copied()
            .unwrap_or_else(|| unit.entity.index());
    }
    for unit in &mut units {
        unit.record_order = record_orders
            .get(&unit.entity)
            .copied()
            .unwrap_or_else(|| unit.entity.index());
    }
    world
        .get_resource_or_insert_with(super::rom_logic::RomAiState::default)
        .observe_units(player_id, assignment_units.iter().map(|unit| unit.entity));
    let properties: Vec<_> = {
        let mut query = world.query::<(&GridPosition, &Property)>();
        query
            .iter(world)
            .map(|(position, property)| (*position, *property))
            .collect()
    };
    let property_positions: HashSet<_> = properties
        .iter()
        .map(|(position, _)| (position.x, position.y))
        .collect();
    // 生産直後の行動済み部隊へ目標を割り当てると、同じ初期波を増援と誤認する。
    // ROMと同様、実際に行動可能な部隊が現れた時点でその波をまとめて割り当てる。
    let objective_assignments = if units.is_empty() {
        world
            .get_resource::<ObjectiveAssignmentState>()
            .and_then(|state| state.by_player.get(&player_id))
            .cloned()
            .unwrap_or_default()
    } else {
        update_objective_assignments(
            world,
            &assignment_units,
            &properties,
            &map,
            &master_data,
            player_id,
        )
    };
    let mission_assignments = world
        .get_resource::<ObjectiveAssignmentState>()
        .and_then(|state| state.mission_by_player.get(&player_id))
        .cloned()
        .unwrap_or_default();
    let mission_three_transports: HashSet<_> = mission_assignments
        .iter()
        .filter_map(|(entity, mission)| (mission & 0x03 == 3).then_some(*entity))
        .collect();
    let mut enemies: Vec<_> = {
        let mut query = world.query::<(
            Entity,
            &GridPosition,
            &Faction,
            &UnitStats,
            &Health,
            Option<&crate::components::Transporting>,
        )>();
        query
            .iter(world)
            .filter_map(|(entity, position, faction, stats, health, transporting)| {
                (faction.0 != player_id && health.current > 0 && transporting.is_none()).then_some(
                    EnemyView {
                        entity,
                        position: *position,
                        stats: stats.clone(),
                        hp: health.current,
                    },
                )
            })
            .collect()
    };
    // ROM 1F44は敵の固定長レコードを先頭から列挙する。盤面座標順ではない。
    enemies.sort_by_key(|enemy| super::unit_record::record_order(world, enemy.entity));
    let restricted_target = scenario.and_then(|scenario| {
        select_restricted_mode_target(
            &enemies,
            &properties,
            &map,
            player_id,
            scenario.restricted_radius,
        )
    });
    let evaluation_mode = if restricted_target.is_some() {
        super::rom_logic::RomEvaluationMode::Restricted
    } else {
        super::rom_logic::RomEvaluationMode::Normal
    };
    world
        .get_resource_or_insert_with(super::rom_logic::RomAiState::default)
        .set_evaluation_mode(
            player_id,
            evaluation_mode,
            restricted_target.map(|enemy| enemy.stats.unit_type),
        );
    let influence_field = rom_influence_for_turn(world, player_id, turn, &map, &enemies);
    let movement_objectives: HashMap<_, _> = units
        .iter()
        .filter_map(|unit| {
            let assigned = objective_assignments.get(&unit.entity).copied()?;
            resolve_rom_movement_objective(
                unit,
                assigned,
                &RomMovementObjectiveContext {
                    cargo: cargo_by_transport.get(&unit.entity).map(Vec::as_slice),
                    assignments: &objective_assignments,
                    influence: &influence_field,
                    properties: &properties,
                    map: &map,
                    master_data: &master_data,
                    player_id,
                },
            )
            .map(|objective| (unit.entity, objective))
        })
        .collect();

    // ROM 44BC/45B8は都市・工場上の部隊を先に選び、生産地点を空けてから一般部隊へ進む。
    let objective_route_costs: HashMap<_, _> = units
        .iter()
        .filter_map(|unit| {
            let objective = movement_objectives.get(&unit.entity).copied()?;
            let route_field = build_objective_route_field(
                &map,
                &master_data,
                unit.position,
                objective,
                unit.stats.movement_type,
            );
            Some((
                unit.entity,
                route_field.get(&unit.position).copied().unwrap_or(u32::MAX),
            ))
        })
        .collect();
    if evaluation_mode == super::rom_logic::RomEvaluationMode::Restricted {
        let own_capital = own_capital_position(&properties, player_id);
        let scenario = scenario.expect("restricted mode requires ROM scenario data");
        units.sort_by_key(|unit| restricted_actor_priority(unit, own_capital, scenario));
    } else {
        units.sort_by_key(|unit| {
            actor_priority(
                unit,
                &property_positions,
                movement_objectives.get(&unit.entity).copied(),
                objective_route_costs.get(&unit.entity).copied(),
            )
        });
    }

    if let Some(actor) = units.into_iter().next() {
        let actor_objective = restricted_target
            .map(|enemy| enemy.position)
            .or_else(|| movement_objectives.get(&actor.entity).copied());
        let candidates = build_candidate_field(
            &map,
            &occupants,
            actor.position,
            &actor.stats,
            actor.fuel,
            player_id,
            &master_data,
        );

        // 制限モードは4E55へ直接入り、通常側のCapture/Drop/Load/Mergeを迂回する。
        // 通常時だけ4A1E以降の順序を適用する。
        if evaluation_mode == super::rom_logic::RomEvaluationMode::Normal
            && let Some(command) = choose_capture(&actor, &candidates, &properties, player_id)
        {
            return Some((actor.entity, command));
        }
        if evaluation_mode == super::rom_logic::RomEvaluationMode::Normal
            && super::compatibility_profile::is_gbw_transport(&actor.stats)
            && let Some(command) = super::transport::choose_transport_action(
                world,
                actor.entity,
                &candidates,
                actor_objective,
                mission_assignments.get(&actor.entity).copied(),
                player_id,
                version,
            )
        {
            return Some((actor.entity, command));
        }
        if evaluation_mode == super::rom_logic::RomEvaluationMode::Normal {
            // ROM 51DAは個別目標までの距離C69Eを687Cの兵種別閾値と比較する。
            // 到達不能値はROMの0x40へ丸め、近距離部隊を輸送需要へ誤算入しない。
            let pickup_distance = objective_route_costs
                .get(&actor.entity)
                .copied()
                .unwrap_or(0x40)
                .min(0x40);
            let searches_for_transport = pickup_distance
                >= super::rom_logic::GbUnitKind::pickup_distance_threshold(actor.stats.unit_type);
            if searches_for_transport
                && super::rom_logic::GbUnitKind::increments_pickup_counter(actor.stats.unit_type)
            {
                // ROM 51E1〜51EBでは距離ゲートを通過した歩兵系だけがC6A5を増やす。
                world
                    .get_resource_or_insert_with(super::rom_logic::RomAiState::default)
                    .record_pickup_candidate(player_id);
            }
            // ROM 51ECで一度bit 5を立てた後、搭載先がなければ52E1で解除する。
            // 制限モードの51BFは即座に失敗し、このbitには触れない。
            world
                .get_resource_or_insert_with(super::rom_logic::RomAiState::default)
                .set_pickup_eligible(player_id, actor.entity, false);
            if searches_for_transport {
                // 通常候補は自軍占有マスを除くため、搭載候補場だけを51BF用に別生成する。
                let load_candidates = build_load_candidate_field(
                    &map,
                    &occupants,
                    actor.position,
                    &actor.stats,
                    actor.fuel,
                    player_id,
                    &master_data,
                );
                if let Some(command) = super::transport::choose_load(
                    world,
                    actor.entity,
                    &load_candidates,
                    &mission_three_transports,
                    player_id,
                ) {
                    return Some((actor.entity, command));
                }
            }
        }
        if let Some(command) = choose_attack(
            &actor,
            &candidates,
            &enemies,
            &AttackEvaluationContext {
                map: &map,
                master_data: &master_data,
                damage_chart: &damage_chart,
                properties: &properties,
                player_id,
                version,
            },
        ) {
            return Some((actor.entity, command));
        }
        // ROM 5D36はIQ200だけが通る同種部隊合流分岐。
        if evaluation_mode == super::rom_logic::RomEvaluationMode::Normal
            && version == AiVersion::V200
            && let Some(command) = choose_merge(
                &actor,
                &map,
                &occupants,
                &friendly_by_position,
                world,
                player_id,
                &master_data,
            )
        {
            return Some((actor.entity, command));
        }
        let (command, used_fallback) =
            choose_wait(&actor, &candidates, actor_objective, &map, &master_data);
        if evaluation_mode == super::rom_logic::RomEvaluationMode::Normal && used_fallback {
            // ROM 4B91の通常経路移動が失敗し、4D86の特殊移動が成立すると4E4Dで
            // bit 5を再設定する。島や遮断地形に残った歩兵が輸送対象へ戻る分岐である。
            world
                .get_resource_or_insert_with(super::rom_logic::RomAiState::default)
                .set_pickup_eligible(player_id, actor.entity, true);
        }
        if evaluation_mode == super::rom_logic::RomEvaluationMode::Normal
            && used_fallback
            && super::rom_logic::GbUnitKind::increments_mobility_shortage_counter(
                actor.stats.unit_type,
            )
        {
            // ROM 4B91の通常経路が現在地を含まない時だけ、4A4BでC6A4を増やす。
            // ZOCなどにより合法手が現在地だけでも、目標への地形経路があれば輸送不足ではない。
            world
                .get_resource_or_insert_with(super::rom_logic::RomAiState::default)
                .record_mobility_shortage(player_id);
        }
        return Some((actor.entity, command));
    }
    None
}

/// ROM 447B〜461Eの8段走査を、そのまま比較キーへ変換する。
fn actor_priority(
    unit: &UnitView,
    property_positions: &HashSet<(usize, usize)>,
    objective: Option<GridPosition>,
    objective_route_cost: Option<u32>,
) -> (u8, u32, u32) {
    let on_property = property_positions.contains(&(unit.position.x, unit.position.y));
    let pass =
        super::rom_logic::actor_selection_pass(&unit.stats, on_property, objective.is_some());
    if pass == super::rom_logic::ActorSelectionPass::MinimumObjectiveCost {
        // ROM 45E5はレコード+15を最小化し、同値なら後のレコードへ更新する。
        return (
            pass as u8,
            objective_route_cost.unwrap_or(u32::MAX),
            u32::MAX - unit.record_order,
        );
    }
    // それ以前の7本は各々レコード0から昇順に走査し、最初の一致で終了する。
    (pass as u8, 0, unit.record_order)
}

fn own_capital_position(
    properties: &[(GridPosition, Property)],
    player_id: PlayerId,
) -> Option<GridPosition> {
    properties.iter().find_map(|(position, property)| {
        (property.terrain == Terrain::Capital && property.owner_id == Some(player_id))
            .then_some(*position)
    })
}

/// ROM 43CC/43E8と4453〜4475のC6A6切り替え。
///
/// 自軍首都からの一様疑似hex距離がシナリオ+0x10以下の敵を選ぶ。同距離では
/// 43E8の`CP`が等値でも更新するため、敵レコード順で後の部隊を残す。
fn select_restricted_mode_target<'a>(
    enemies: &'a [EnemyView],
    properties: &[(GridPosition, Property)],
    map: &Map,
    player_id: PlayerId,
    defense_radius: u32,
) -> Option<&'a EnemyView> {
    let capital = own_capital_position(properties, player_id)?;
    let mut selected = None;
    let mut selected_distance = u32::MAX;
    for enemy in enemies {
        let distance = map.distance(capital.x, capital.y, enemy.position.x, enemy.position.y);
        if distance <= defense_radius && distance <= selected_distance {
            selected = Some(enemy);
            selected_distance = distance;
        }
    }
    selected
}

/// ROM 50C4〜514Fの制限モード専用部隊選択順。
///
/// 首都上の部隊、間接攻撃部隊をレコード昇順で優先し、それらがなければ
/// UNIT_VALUES_0〜3×現在HPが最大の部隊を選ぶ。最大値の同点は後のレコードが勝つ。
fn restricted_actor_priority(
    unit: &UnitView,
    own_capital: Option<GridPosition>,
    scenario: super::rom_data::RomScenarioData,
) -> (u8, Reverse<u64>, u32) {
    if own_capital == Some(unit.position) {
        return (0, Reverse(0), unit.record_order);
    }
    if unit.stats.min_range > 1 {
        return (1, Reverse(0), unit.record_order);
    }
    let normalized_hp = u64::from(unit.hp)
        .saturating_mul(99)
        .saturating_div(u64::from(unit.max_hp.max(1)));
    let value = u64::from(scenario.unit_value(
        unit.stats.unit_type,
        super::rom_logic::RomEvaluationMode::Restricted,
    ))
    .saturating_mul(normalized_hp);
    (2, Reverse(value), u32::MAX - unit.record_order)
}

/// 部隊レコード順に最寄りの未割当拠点を予約し、GBの個別目標欄に相当する表を作る。
#[cfg(test)]
fn assign_objectives(
    units: &[(Entity, GridPosition)],
    properties: &[(GridPosition, Property)],
    map: &Map,
    player_id: PlayerId,
) -> HashMap<Entity, GridPosition> {
    let mut ordered_units: Vec<_> = units
        .iter()
        .map(|(entity, position)| AssignmentUnit {
            entity: *entity,
            position: *position,
            unit_type: crate::resources::UnitType::Infantry,
            record_order: entity.index(),
            has_cargo: false,
            is_transported: false,
        })
        .collect();
    ordered_units.sort_by_key(|unit| unit.record_order);
    let mut reserved = HashSet::new();
    let master_data = MasterDataRegistry::load().expect("master data");
    ordered_units
        .iter()
        .filter_map(|unit| {
            let objective = select_rom_capture_objective(
                unit,
                properties,
                map,
                &master_data,
                player_id,
                &reserved,
            )?;
            if objective_requires_unique_assignment(objective, properties) {
                reserved.insert(objective);
            }
            Some((unit.entity, objective))
        })
        .collect()
}

/// 生存部隊の既存目標を保ち、未割当部隊だけへ新しい目標を設定する。
fn update_objective_assignments(
    world: &mut World,
    units: &[AssignmentUnit],
    properties: &[(GridPosition, Property)],
    map: &Map,
    master_data: &MasterDataRegistry,
    player_id: PlayerId,
) -> HashMap<Entity, RomUnitObjective> {
    let current_turn = world
        .get_resource::<MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let begins_new_turn = world
        .get_resource::<ObjectiveAssignmentState>()
        .and_then(|state| state.turn_by_player.get(&player_id))
        .copied()
        != Some(current_turn);
    let alive: HashSet<_> = units.iter().map(|unit| unit.entity).collect();
    let mut assignments = world
        .get_resource::<ObjectiveAssignmentState>()
        .and_then(|state| state.by_player.get(&player_id))
        .cloned()
        .unwrap_or_default();
    let mut missions = world
        .get_resource::<ObjectiveAssignmentState>()
        .and_then(|state| state.mission_by_player.get(&player_id))
        .cloned()
        .unwrap_or_default();
    let mut cargo_loaded = world
        .get_resource::<ObjectiveAssignmentState>()
        .and_then(|state| state.cargo_loaded_by_player.get(&player_id))
        .cloned()
        .unwrap_or_default();
    let units_by_entity: HashMap<_, _> = units.iter().map(|unit| (unit.entity, *unit)).collect();
    let owned_properties: HashSet<_> = properties
        .iter()
        .filter_map(|(position, property)| {
            (property.owner_id == Some(player_id)).then_some(*position)
        })
        .collect();
    // 他部隊が先に占領した目標はGBの個別目標欄に残る。一方、map_8の実測では
    // 目標を占領した本人は次手番に別目標へ進むため、現在地の達成済み目標だけを外す。
    assignments.retain(|entity, objective| {
        let Some(unit) = units_by_entity.get(entity) else {
            return false;
        };
        alive.contains(entity)
            && (unit.is_transported
                || !objective.position().is_some_and(|position| {
                    unit.position == position && owned_properties.contains(&position)
                }))
    });
    missions.retain(|entity, _| alive.contains(entity));
    cargo_loaded.retain(|entity, _| alive.contains(entity));
    for unit in units {
        if begins_new_turn || !cargo_loaded.contains_key(&unit.entity) {
            let cargo_state_changed =
                cargo_loaded.get(&unit.entity).copied() != Some(unit.has_cargo);
            if cargo_state_changed
                && unit.unit_type == crate::resources::UnitType::TransportHelicopter
                && missions
                    .get(&unit.entity)
                    .is_some_and(|mission| mission & 0x03 == 3)
            {
                // ROM 483Fは次手番に積荷状態を読み、目標を「自軍首都」と
                // 「搭載歩兵用の未占領施設」の間で引き直す。
                assignments.remove(&unit.entity);
            }
            cargo_loaded.insert(unit.entity, unit.has_cargo);
        }
    }

    // ROM 485Eは都市・空港・港だけ重複目標を拒否し、首都・工場は共有を許す。
    let mut reserved: HashSet<_> = assignments
        .values()
        .filter_map(|objective| objective.position())
        .filter(|position| objective_requires_unique_assignment(*position, properties))
        .collect();
    let mut unassigned: Vec<_> = units
        .iter()
        .filter(|unit| !assignments.contains_key(&unit.entity))
        .copied()
        .collect();
    unassigned.sort_by_key(|unit| unit.record_order);
    let scenario = super::rom_data::identify_scenario(map, master_data);
    // 生産は行動フェーズの後に行われるため、ここには直前の生産で使ったC6ADが残る。
    let production_strategy = world
        .get_resource::<super::rom_logic::RomAiState>()
        .and_then(|state| state.production_strategy_for(player_id))
        .unwrap_or(super::rom_logic::ProductionStrategy::Opening);
    for unit in unassigned {
        let mission = missions.get(&unit.entity).copied().unwrap_or_else(|| {
            initial_rom_mission_state(
                unit.unit_type,
                scenario,
                production_strategy,
                units,
                &missions,
            )
        });
        let objective = if matches!(
            unit.unit_type,
            crate::resources::UnitType::Infantry | crate::resources::UnitType::Mech
        ) {
            select_rom_capture_objective(&unit, properties, map, master_data, player_id, &reserved)
                .map(RomUnitObjective::Position)
        } else {
            select_rom_strategic_objective(
                &unit,
                mission,
                &StrategicObjectiveContext {
                    scenario,
                    properties,
                    map,
                    master_data,
                    player_id,
                    reserved: &reserved,
                },
            )
        };
        missions.insert(unit.entity, mission);
        if let Some(objective) = objective {
            if let Some(position) = objective.position()
                && objective_requires_unique_assignment(position, properties)
            {
                reserved.insert(position);
            }
            assignments.insert(unit.entity, objective);
        }
    }

    let mut state = world.get_resource_or_insert_with(ObjectiveAssignmentState::default);
    state.by_player.insert(player_id, assignments.clone());
    state.mission_by_player.insert(player_id, missions);
    state.cargo_loaded_by_player.insert(player_id, cargo_loaded);
    state.turn_by_player.insert(player_id, current_turn);
    assignments
}

/// ROM 485E〜48DBの歩兵系目標走査。
fn select_rom_capture_objective(
    unit: &AssignmentUnit,
    properties: &[(GridPosition, Property)],
    map: &Map,
    _master_data: &MasterDataRegistry,
    player_id: PlayerId,
    reserved: &HashSet<GridPosition>,
) -> Option<GridPosition> {
    // ROM 4863は仮想兵種0x30の距離場を現在地から一度だけ作る。実機RAMの値は
    // 歩兵の地形移動費ではなく疑似HEXの歩数と一致するため、OpenWars側の盤面
    // トポロジー距離へ写像する。これによりsquareでも同じ目標選択規則を使える。
    properties
        .iter()
        .filter(|(position, property)| {
            property.max_capture_points > 0
                && property.owner_id != Some(player_id)
                // 4899〜48A0は歩兵以外の歩兵系だけ首都を除外する。
                && (unit.unit_type == crate::resources::UnitType::Infantry
                    || property.terrain != crate::resources::Terrain::Capital)
                // 48A2〜48B1は首都・工場以外に限って重複目標を拒否する。
                && (!objective_requires_unique_assignment(*position, properties)
                    || !reserved.contains(position))
        })
        .min_by_key(|(position, _)| {
            (
                if *position == unit.position {
                    // 4870〜4875は現在地の盤面値を0xFFへ戻して目標候補から外す。
                    u32::MAX
                } else {
                    map.distance(unit.position.x, unit.position.y, position.x, position.y)
                },
                Reverse(position.y),
                Reverse(position.x),
            )
        })
        .map(|(position, _)| *position)
}

fn objective_requires_unique_assignment(
    objective: GridPosition,
    properties: &[(GridPosition, Property)],
) -> bool {
    properties.iter().any(|(position, property)| {
        *position == objective
            && !matches!(
                property.terrain,
                crate::resources::Terrain::Capital | crate::resources::Terrain::Factory
            )
    })
}

/// ROM 481B/483Fと0AE9の固定目標分岐。
fn select_rom_strategic_objective(
    unit: &AssignmentUnit,
    mission: u8,
    context: &StrategicObjectiveContext<'_>,
) -> Option<RomUnitObjective> {
    use crate::resources::UnitType;

    if unit.unit_type == UnitType::Artillery {
        return Some(RomUnitObjective::Position(unit.position));
    }
    if mission & 0x03 == 3
        && matches!(
            unit.unit_type,
            UnitType::Recon | UnitType::TransportHelicopter
        )
    {
        if unit.unit_type == UnitType::TransportHelicopter && unit.has_cargo {
            // ROM 485Eは仮想兵種0x30の距離場で積荷が向かう未占領施設を選ぶ。
            // 首都を除く条件は歩兵以外の呼び出し側として残る。
            return select_rom_capture_objective(
                unit,
                context.properties,
                context.map,
                context.master_data,
                context.player_id,
                context.reserved,
            )
            .map(RomUnitObjective::Position);
        }
        return context.properties.iter().find_map(|(position, property)| {
            (property.terrain == crate::resources::Terrain::Capital
                && property.owner_id == Some(context.player_id))
            .then_some(RomUnitObjective::Position(*position))
        });
    }
    if let Some(objective) = context
        .scenario
        .and_then(|value| value.strategic_objective(context.player_id, mission))
    {
        return Some(RomUnitObjective::Position(objective));
    }
    if mission & 0x03 == 3 {
        // ROM 4836の0x40,0x40は盤外座標ではない。Bank 2 40E1/4125はこの値を
        // 検出すると、敵射程の影響盤面から部隊ごとの到達可能目標を作り直す。
        return Some(RomUnitObjective::DynamicInfluence);
    }
    // 状態0は敵首都を目標にする。
    context.properties.iter().find_map(|(position, property)| {
        (property.terrain == crate::resources::Terrain::Capital
            && property.owner_id != Some(context.player_id))
        .then_some(RomUnitObjective::Position(*position))
    })
}

/// ROM Bank 2 `4299`と同じく、敵部隊の射程内へ兵種別の2成分を飽和加算する。
fn build_rom_influence_field(
    map: &Map,
    enemies: &[EnemyView],
) -> HashMap<GridPosition, RomInfluence> {
    let mut influence = HashMap::<GridPosition, RomInfluence>::new();
    for enemy in enemies {
        let (anti_air, anti_surface) =
            super::rom_logic::GbUnitKind::influence_weights(enemy.stats.unit_type);
        if (anti_air == 0 && anti_surface == 0) || enemy.stats.max_range == 0 {
            continue;
        }
        let min_range = enemy.stats.min_range.max(1);
        let max_range = enemy.stats.max_range.max(min_range);
        for y in 0..map.height {
            for x in 0..map.width {
                let position = GridPosition { x, y };
                let distance = map.distance(x, y, enemy.position.x, enemy.position.y);
                if (min_range..=max_range).contains(&distance) {
                    influence
                        .entry(position)
                        .or_default()
                        .add(anti_air, anti_surface);
                }
            }
        }
    }
    influence
}

/// ROMはD7E4/D803の影響盤面をAI手番開始時に一度だけ構築し、途中の撃破後も保持する。
fn rom_influence_for_turn(
    world: &mut World,
    player_id: PlayerId,
    turn: u32,
    map: &Map,
    enemies: &[EnemyView],
) -> HashMap<GridPosition, RomInfluence> {
    if let Some((cached_turn, cached)) = world
        .get_resource::<RomInfluenceState>()
        .and_then(|state| state.by_player.get(&player_id))
        && *cached_turn == turn
    {
        return cached.clone();
    }

    let influence = build_rom_influence_field(map, enemies);
    world
        .get_resource_or_insert_with(RomInfluenceState::default)
        .by_player
        .insert(player_id, (turn, influence.clone()));
    influence
}

/// ROM `49B6`の輸送船積荷目標と`4125`の影響最大マス選択を実座標へ変換する。
fn resolve_rom_movement_objective(
    unit: &UnitView,
    assigned: RomUnitObjective,
    context: &RomMovementObjectiveContext<'_>,
) -> Option<GridPosition> {
    let effective_assigned = if unit.stats.unit_type == crate::resources::UnitType::Lander {
        context
            .cargo
            .and_then(|cargo| {
                // 49B6は積荷スロットを順に上書きするため、最後の積荷の目標状態が残る。
                cargo
                    .iter()
                    .rev()
                    .find_map(|entity| context.assignments.get(entity).copied())
            })
            .unwrap_or(assigned)
    } else {
        assigned
    };

    match effective_assigned {
        RomUnitObjective::Position(position) => Some(position),
        RomUnitObjective::DynamicInfluence => select_rom_dynamic_influence_objective(unit, context),
    }
}

/// ROM `4125`は部隊が到達できるマスだけを行優先で走査し、2成分の合計が最大の
/// 最初のマスを選ぶ。正の影響値がなければ`416B`どおり敵首都へ戻す。
fn select_rom_dynamic_influence_objective(
    unit: &UnitView,
    context: &RomMovementObjectiveContext<'_>,
) -> Option<GridPosition> {
    let reachable = build_route_field(
        context.map,
        context.master_data,
        unit.position,
        unit.stats.movement_type,
    );
    let mut best_score = 0_u8;
    let mut best = None;
    for y in 0..context.map.height {
        for x in 0..context.map.width {
            let position = GridPosition { x, y };
            if !reachable.contains_key(&position) {
                continue;
            }
            let score = context
                .influence
                .get(&position)
                .copied()
                .unwrap_or_default()
                .combined();
            if score > best_score {
                best_score = score;
                best = Some(position);
            }
        }
    }
    best.or_else(|| {
        context.properties.iter().find_map(|(position, property)| {
            (property.terrain == crate::resources::Terrain::Capital
                && property.owner_id != Some(context.player_id))
            .then_some(*position)
        })
    })
}

fn initial_rom_mission_state(
    unit_type: crate::resources::UnitType,
    scenario: Option<super::rom_data::RomScenarioData>,
    strategy: super::rom_logic::ProductionStrategy,
    units: &[AssignmentUnit],
    missions: &HashMap<Entity, u8>,
) -> u8 {
    let Some(scenario) = scenario else {
        return 0;
    };
    let mut same_kind_counts = [0_u32; 4];
    for unit in units.iter().filter(|unit| unit.unit_type == unit_type) {
        if let Some(mission) = missions.get(&unit.entity).copied()
            && let Some(count) = same_kind_counts.get_mut(usize::from(mission & 0x03))
        {
            *count = count.saturating_add(1);
        }
    }
    super::rom_logic::assign_mission_state(
        unit_type,
        strategy,
        scenario.production_limit(strategy, unit_type),
        same_kind_counts,
        scenario.recon_uses_mission_three,
    )
}

fn choose_capture(
    actor: &UnitView,
    candidates: &[CandidateTile],
    properties: &[(GridPosition, Property)],
    player_id: PlayerId,
) -> Option<AiCommand> {
    if !actor.stats.can_capture {
        return None;
    }
    let is_capturable = |position: GridPosition| {
        properties.iter().any(|(property_position, property)| {
            *property_position == position
                && property.owner_id != Some(player_id)
                && property.max_capture_points > 0
        })
    };
    let mut best: Option<(u32, GridPosition)> = None;
    for candidate in candidates {
        if is_capturable(candidate.position)
            && best
                .as_ref()
                .is_none_or(|(cost, _)| candidate.movement_cost <= *cost)
        {
            // ROM 593D〜59B9は個別目標を参照せず、到達可能な未占領施設の
            // 移動費だけを比較する。同一コストでも後から走査したマスへ更新する。
            best = Some((candidate.movement_cost, candidate.position));
        }
    }
    best.map(|(_, target_pos)| AiCommand::Capture { target_pos })
}

fn choose_attack(
    actor: &UnitView,
    candidates: &[CandidateTile],
    enemies: &[EnemyView],
    context: &AttackEvaluationContext<'_>,
) -> Option<AiCommand> {
    let mut best: Option<(AttackKey, AiCommand)> = None;
    let attack_movement = attack_movement_allowance(&actor.stats, context.version);
    for candidate in candidates {
        // ROM 56D4はIQ100の移動後攻撃盤面だけ移動力を1減らし、最低2を保証する。
        // IQ200では減算値が0なので、通常移動と同じ移動力を使う。
        if candidate.movement_cost > attack_movement {
            continue;
        }
        if candidate.position != actor.position && actor.stats.min_range > 1 {
            continue;
        }
        let terrain_defense = attack_position_value(
            candidate.position,
            context.map,
            context.master_data,
            context.properties,
            context.player_id,
            context.version,
        );
        let mut best_target: Option<(u32, &EnemyView)> = None;
        for enemy in enemies {
            let distance = context.map.distance(
                candidate.position.x,
                candidate.position.y,
                enemy.position.x,
                enemy.position.y,
            );
            let enemy_defense = context
                .map
                .get_terrain(enemy.position.x, enemy.position.y)
                .map_or(0, |terrain| {
                    context.master_data.get_terrain_defense_bonus(terrain)
                });
            let damage = get_expected_damage(
                &actor.stats,
                actor.hp,
                actor.ammo,
                &enemy.stats,
                enemy_defense,
                distance,
                context.master_data,
                context.damage_chart,
                false,
            );
            if damage == 0 {
                continue;
            }
            let target_remaining_hp = enemy.hp.saturating_sub(damage);
            if best_target
                .as_ref()
                .is_none_or(|(current, _)| target_remaining_hp < *current)
            {
                // ROM 5822は敵レコードを逆順に調べ、同値でも更新するため、最終的に
                // 最も若い敵レコードが残る。昇順列挙では厳密な改善時だけ更新すれば同値になる。
                best_target = Some((target_remaining_hp, enemy));
            }
        }
        if let Some((target_remaining_hp, enemy)) = best_target {
            let key = AttackKey {
                target_remaining_hp,
                defensive_position: Reverse(terrain_defense),
                target_hp: enemy.hp,
            };
            if best.as_ref().is_none_or(|(current, _)| key <= *current) {
                // ROM 57E0は比較値が同じ場合も後の行優先候補へ更新する。
                best = Some((
                    key,
                    AiCommand::Attack {
                        target_pos: candidate.position,
                        target_entity: enemy.entity,
                    },
                ));
            }
        }
    }
    best.map(|(_, command)| command)
}

fn attack_movement_allowance(stats: &UnitStats, version: AiVersion) -> u32 {
    stats
        .max_movement
        .saturating_sub(super::rom_logic::movement_evaluation_penalty(version))
        .max(2)
}

fn attack_position_value(
    position: GridPosition,
    map: &Map,
    master_data: &MasterDataRegistry,
    properties: &[(GridPosition, Property)],
    player_id: PlayerId,
    version: AiVersion,
) -> u32 {
    let own_property = properties.iter().any(|(property_position, property)| {
        *property_position == position && property.owner_id == Some(player_id)
    });
    if version == AiVersion::V200 && own_property {
        // ROM 4434はIQ200の副評価で自軍施設を0扱いし、通常地形の防御値を優先する。
        return 0;
    }
    map.get_terrain(position.x, position.y)
        .map_or(0, |terrain| master_data.get_terrain_defense_bonus(terrain))
}

#[allow(clippy::too_many_arguments)]
fn choose_merge(
    actor: &UnitView,
    map: &Map,
    occupants: &HashMap<(usize, usize), OccupantInfo>,
    friendly_by_position: &HashMap<(usize, usize), Entity>,
    world: &World,
    player_id: PlayerId,
    master_data: &MasterDataRegistry,
) -> Option<AiCommand> {
    let candidates = build_merge_candidate_field(
        map,
        occupants,
        actor.position,
        &actor.stats,
        actor.fuel,
        player_id,
        master_data,
    );
    for candidate in candidates {
        let target_entity =
            *friendly_by_position.get(&(candidate.position.x, candidate.position.y))?;
        let target_health = world.get::<Health>(target_entity)?;
        // GB版は99を満タンとして合計119未満だけを合流候補にする。
        // OpenWarsの100刻みへ換算し、片方も損耗していない無意味な合流は除く。
        if (actor.hp < actor.max_hp || target_health.current < target_health.max)
            && actor.hp + target_health.current < 120
        {
            return Some(AiCommand::Merge {
                target_pos: candidate.position,
                target_entity,
            });
        }
    }
    None
}

fn choose_wait(
    actor: &UnitView,
    candidates: &[CandidateTile],
    objective: Option<GridPosition>,
    map: &Map,
    master_data: &MasterDataRegistry,
) -> (AiCommand, bool) {
    let normal_route_distances = objective.map(|objective| {
        build_objective_route_field(
            map,
            master_data,
            actor.position,
            objective,
            actor.stats.movement_type,
        )
    });
    let used_fallback = objective.is_some()
        && normal_route_distances
            .as_ref()
            .is_some_and(|distances| !distances.contains_key(&actor.position));
    let movement_objective = if used_fallback {
        objective.and_then(|objective| select_rom_fallback_staging(map, objective))
    } else {
        objective
    };
    let fallback_route_distances = (used_fallback && movement_objective.is_some()).then(|| {
        build_route_field(
            map,
            master_data,
            movement_objective.expect("checked above"),
            actor.stats.movement_type,
        )
    });
    let route_distances = fallback_route_distances
        .as_ref()
        .or(normal_route_distances.as_ref());
    let mut best: Option<(u32, GridPosition)> = None;
    for candidate in candidates {
        let key = movement_objective.map(|objective| {
            route_distances
                .and_then(|distances| distances.get(&candidate.position))
                .copied()
                .unwrap_or_else(|| {
                    map.distance(
                        candidate.position.x,
                        candidate.position.y,
                        objective.x,
                        objective.y,
                    )
                })
        });
        if let Some(key) = key
            && best.as_ref().is_none_or(|(current, _)| key <= *current)
        {
            // ROMの目標距離欄は地形込みの経路長である。到達候補自体にはOpenWarsの
            // ZOC制限を適用したうえで、同値なら4BF6の実測どおり後の行優先候補を残す。
            best = Some((key, candidate.position));
        }
    }
    (
        AiCommand::Wait {
            target_pos: best.map_or(actor.position, |(_, position)| position),
        },
        used_fallback,
    )
}

/// 固定陸上目標へ向かう艦船だけ接岸可能地点へ写像し、それ以外はROMと同じ目標起点場を作る。
fn build_objective_route_field(
    map: &Map,
    master_data: &MasterDataRegistry,
    origin: GridPosition,
    objective: GridPosition,
    movement_type: MovementType,
) -> HashMap<GridPosition, u32> {
    if movement_type == MovementType::Ship {
        build_ship_approach_route_field(map, master_data, origin, objective)
    } else {
        build_route_field(map, master_data, objective, movement_type)
    }
}

/// ROM 4D86〜4E4Dの、通常経路が存在しない部隊向け沿岸中継点走査。
/// 内部地形37〜39/44はOpenWarsでは港・浅瀬に相当し、同値時は後の盤面走査を残す。
fn select_rom_fallback_staging(map: &Map, objective: GridPosition) -> Option<GridPosition> {
    (0..map.height)
        .flat_map(|y| (0..map.width).map(move |x| GridPosition { x, y }))
        .filter(|position| {
            matches!(
                map.get_terrain(position.x, position.y),
                Some(crate::resources::Terrain::Port | crate::resources::Terrain::Shoal)
            )
        })
        .min_by_key(|position| {
            (
                map.distance(position.x, position.y, objective.x, objective.y),
                Reverse(position.y),
                Reverse(position.x),
            )
        })
}

/// 内陸目標を艦船が直接経路場の始点にすると海へ展開できないため、現在の海域から
/// 到達可能なマスのうち目標へ最接近できる海上地点を作戦目標として経路場を作る。
fn build_ship_approach_route_field(
    map: &Map,
    master_data: &MasterDataRegistry,
    origin: GridPosition,
    objective: GridPosition,
) -> HashMap<GridPosition, u32> {
    let reachable = build_route_field(map, master_data, origin, MovementType::Ship);
    let Some(best_distance) = reachable
        .keys()
        .map(|position| map.distance(position.x, position.y, objective.x, objective.y))
        .min()
    else {
        return HashMap::new();
    };
    let approach_positions: Vec<_> = reachable
        .keys()
        .filter(|position| {
            map.distance(position.x, position.y, objective.x, objective.y) == best_distance
        })
        .copied()
        .collect();
    build_route_field_to_any(map, master_data, &approach_positions, MovementType::Ship)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::UnitType;

    fn unit(entity: u32, movement_type: MovementType, position: GridPosition) -> UnitView {
        UnitView {
            entity: Entity::from_raw(entity),
            record_order: entity,
            position,
            stats: UnitStats {
                unit_type: UnitType::Infantry,
                movement_type,
                max_movement: 6,
                ..UnitStats::mock()
            },
            hp: 100,
            max_hp: 100,
            fuel: 99,
            ammo: (9, 0),
        }
    }

    fn assignment_unit(entity: Entity, position: GridPosition) -> AssignmentUnit {
        AssignmentUnit {
            entity,
            position,
            unit_type: UnitType::Infantry,
            record_order: entity.index(),
            has_cargo: false,
            is_transported: false,
        }
    }

    #[test]
    fn actor_priority_uses_bank2_selection_passes() {
        let property_positions = HashSet::from([(2, 2), (3, 2)]);
        let mut recon = unit(2, MovementType::ArmoredCar, GridPosition { x: 2, y: 2 });
        recon.stats.unit_type = UnitType::Recon;
        let unassigned = unit(1, MovementType::Infantry, GridPosition { x: 3, y: 2 });
        let assigned = unit(3, MovementType::Infantry, GridPosition { x: 4, y: 2 });
        let field = unit(0, MovementType::Infantry, GridPosition { x: 1, y: 1 });

        assert_eq!(
            actor_priority(
                &recon,
                &property_positions,
                Some(GridPosition { x: 6, y: 2 }),
                Some(4)
            )
            .0,
            3
        );
        assert_eq!(
            actor_priority(&unassigned, &property_positions, None, None).0,
            4
        );
        assert_eq!(actor_priority(&field, &property_positions, None, None).0, 6);
        assert_eq!(
            actor_priority(
                &assigned,
                &property_positions,
                Some(GridPosition { x: 6, y: 2 }),
                Some(4)
            )
            .0,
            7
        );
    }

    #[test]
    fn actor_priority_uses_route_cost_and_later_record_for_ties() {
        let property_positions = HashSet::new();
        let earlier = unit(0, MovementType::Infantry, GridPosition { x: 7, y: 5 });
        let later = unit(4, MovementType::Infantry, GridPosition { x: 3, y: 5 });
        let earlier_key = actor_priority(
            &earlier,
            &property_positions,
            Some(GridPosition { x: 7, y: 7 }),
            Some(3),
        );
        let later_key = actor_priority(
            &later,
            &property_positions,
            Some(GridPosition { x: 1, y: 7 }),
            Some(3),
        );

        assert!(later_key < earlier_key);
    }

    #[test]
    fn restricted_mode_starts_at_the_rom_capital_radius_and_uses_late_enemy_ties() {
        let map = Map::new(9, 9, Terrain::Plains, crate::resources::GridTopology::Hex);
        let capital = GridPosition { x: 4, y: 4 };
        let properties = [(
            capital,
            Property::new(Terrain::Capital, Some(PlayerId(1)), 200),
        )];
        let enemy = |entity, position| EnemyView {
            entity: Entity::from_raw(entity),
            position,
            stats: UnitStats::mock(),
            hp: 100,
        };
        let enemies = [
            enemy(0, GridPosition { x: 1, y: 4 }),
            enemy(1, GridPosition { x: 7, y: 4 }),
        ];

        let selected =
            select_restricted_mode_target(&enemies, &properties, &map, PlayerId(1), 3).unwrap();
        assert_eq!(selected.entity, Entity::from_raw(1));
        assert!(
            select_restricted_mode_target(&enemies, &properties, &map, PlayerId(1), 2,).is_none()
        );
    }

    #[test]
    fn decide_action_switches_c6a6_at_map1_radius_two() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_1",
            crate::resources::GridTopology::Hex,
        )
        .unwrap();
        let player = PlayerId(1);
        let intruder = world
            .spawn((
                GridPosition { x: 6, y: 5 },
                Faction(PlayerId(2)),
                UnitStats::mock(),
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        let _ = decide_action(&mut world, player, &HashSet::new());
        let state = world.resource::<super::super::rom_logic::RomAiState>();
        assert_eq!(
            state.evaluation_mode_for(player),
            super::super::rom_logic::RomEvaluationMode::Restricted
        );
        assert_eq!(
            state.restricted_target_for(player),
            Some(UnitType::Infantry)
        );

        world
            .entity_mut(intruder)
            .get_mut::<GridPosition>()
            .unwrap()
            .y = 6;
        let _ = decide_action(&mut world, player, &HashSet::new());
        assert_eq!(
            world
                .resource::<super::super::rom_logic::RomAiState>()
                .evaluation_mode_for(player),
            super::super::rom_logic::RomEvaluationMode::Normal
        );
    }

    #[test]
    fn restricted_actor_selection_uses_capital_indirect_then_profile_value() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_1",
            crate::resources::GridTopology::Hex,
        )
        .unwrap();
        let scenario =
            super::super::rom_data::identify_scenario(world.resource::<Map>(), &master_data)
                .unwrap();
        let capital = GridPosition { x: 4, y: 4 };
        let on_capital = unit(4, MovementType::Infantry, capital);
        let mut indirect = unit(3, MovementType::Artillery, GridPosition { x: 3, y: 4 });
        indirect.stats.unit_type = UnitType::Rockets;
        indirect.stats.min_range = 2;
        let mut tank = unit(1, MovementType::Tank, GridPosition { x: 2, y: 4 });
        tank.stats.unit_type = UnitType::TankZ;
        let mut infantry = unit(9, MovementType::Infantry, GridPosition { x: 1, y: 4 });
        infantry.stats.unit_type = UnitType::Infantry;

        assert!(
            restricted_actor_priority(&on_capital, Some(capital), scenario)
                < restricted_actor_priority(&indirect, Some(capital), scenario)
        );
        assert!(
            restricted_actor_priority(&indirect, Some(capital), scenario)
                < restricted_actor_priority(&tank, Some(capital), scenario)
        );
        assert!(
            restricted_actor_priority(&tank, Some(capital), scenario)
                < restricted_actor_priority(&infantry, Some(capital), scenario)
        );
    }

    #[test]
    fn unit_that_captured_its_objective_advances_to_a_new_target() {
        let player_id = PlayerId(1);
        let entity = Entity::from_raw(0);
        let captured_port = GridPosition { x: 1, y: 1 };
        let enemy_factory = GridPosition { x: 6, y: 1 };
        let map = Map::new(
            8,
            4,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Hex,
        );
        let properties = vec![
            (
                captured_port,
                Property::new(crate::resources::Terrain::Port, Some(player_id), 300),
            ),
            (
                enemy_factory,
                Property::new(crate::resources::Terrain::Factory, Some(PlayerId(2)), 200),
            ),
        ];
        let mut world = World::new();
        let master_data = MasterDataRegistry::load().unwrap();
        world.insert_resource(ObjectiveAssignmentState {
            by_player: HashMap::from([(
                player_id,
                HashMap::from([(entity, RomUnitObjective::Position(captured_port))]),
            )]),
            ..Default::default()
        });

        let assignments = update_objective_assignments(
            &mut world,
            &[assignment_unit(entity, captured_port)],
            &properties,
            &map,
            &master_data,
            player_id,
        );

        assert_eq!(assignments[&entity], enemy_factory);
    }

    #[test]
    fn objective_captured_by_another_unit_remains_assigned() {
        let player_id = PlayerId(1);
        let entity = Entity::from_raw(0);
        let captured_port = GridPosition { x: 1, y: 1 };
        let unit_position = GridPosition { x: 2, y: 1 };
        let map = Map::new(
            8,
            4,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Hex,
        );
        let properties = vec![
            (
                captured_port,
                Property::new(crate::resources::Terrain::Port, Some(player_id), 300),
            ),
            (
                GridPosition { x: 6, y: 1 },
                Property::new(crate::resources::Terrain::Factory, Some(PlayerId(2)), 200),
            ),
        ];
        let mut world = World::new();
        let master_data = MasterDataRegistry::load().unwrap();
        world.insert_resource(ObjectiveAssignmentState {
            by_player: HashMap::from([(
                player_id,
                HashMap::from([(entity, RomUnitObjective::Position(captured_port))]),
            )]),
            ..Default::default()
        });

        let assignments = update_objective_assignments(
            &mut world,
            &[assignment_unit(entity, unit_position)],
            &properties,
            &map,
            &master_data,
            player_id,
        );

        assert_eq!(assignments[&entity], captured_port);
    }

    #[test]
    fn transported_cargo_keeps_its_rom_record_objective() {
        let player_id = PlayerId(1);
        let entity = Entity::from_raw(0);
        let objective = GridPosition { x: 4, y: 1 };
        let map = Map::new(
            8,
            4,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Hex,
        );
        let properties = [(
            objective,
            Property::new(crate::resources::Terrain::City, Some(player_id), 200),
        )];
        let mut world = World::new();
        let master_data = MasterDataRegistry::load().unwrap();
        world.insert_resource(ObjectiveAssignmentState {
            by_player: HashMap::from([(
                player_id,
                HashMap::from([(entity, RomUnitObjective::Position(objective))]),
            )]),
            ..Default::default()
        });
        let mut cargo = assignment_unit(entity, objective);
        cargo.is_transported = true;

        let assignments = update_objective_assignments(
            &mut world,
            &[cargo],
            &properties,
            &map,
            &master_data,
            player_id,
        );

        assert_eq!(assignments[&entity], objective);
    }

    #[test]
    fn initial_infantry_receive_distinct_nearest_objectives() {
        let map = Map::new(
            10,
            14,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Hex,
        );
        let units = [
            (Entity::from_raw(0), GridPosition { x: 6, y: 3 }),
            (Entity::from_raw(1), GridPosition { x: 7, y: 4 }),
            (Entity::from_raw(2), GridPosition { x: 6, y: 4 }),
            (Entity::from_raw(3), GridPosition { x: 7, y: 3 }),
            (Entity::from_raw(4), GridPosition { x: 5, y: 3 }),
        ];
        let mut properties: Vec<_> = [1, 3, 5, 7]
            .map(|x| {
                (
                    GridPosition { x, y: 7 },
                    Property::new(crate::resources::Terrain::City, Some(PlayerId(2)), 200),
                )
            })
            .into();
        properties.push((
            GridPosition { x: 4, y: 10 },
            Property::new(crate::resources::Terrain::Factory, Some(PlayerId(2)), 200),
        ));

        let assignments = assign_objectives(&units, &properties, &map, PlayerId(1));

        assert_eq!(
            assignments[&Entity::from_raw(0)],
            GridPosition { x: 7, y: 7 }
        );
        assert_eq!(
            assignments[&Entity::from_raw(1)],
            GridPosition { x: 5, y: 7 }
        );
        assert_eq!(
            assignments[&Entity::from_raw(2)],
            GridPosition { x: 3, y: 7 }
        );
        assert_eq!(
            assignments[&Entity::from_raw(3)],
            GridPosition { x: 4, y: 10 }
        );
        assert_eq!(
            assignments[&Entity::from_raw(4)],
            GridPosition { x: 1, y: 7 }
        );
    }

    #[test]
    fn objectives_remain_stable_while_units_act_sequentially() {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(
            10,
            14,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Hex,
        );
        let first = Entity::from_raw(0);
        let second = Entity::from_raw(1);
        let mut properties = vec![
            (
                GridPosition { x: 7, y: 7 },
                Property::new(crate::resources::Terrain::City, Some(PlayerId(2)), 200),
            ),
            (
                GridPosition { x: 5, y: 7 },
                Property::new(crate::resources::Terrain::City, Some(PlayerId(2)), 200),
            ),
        ];
        let mut world = World::new();
        let initial = update_objective_assignments(
            &mut world,
            &[
                assignment_unit(first, GridPosition { x: 6, y: 3 }),
                assignment_unit(second, GridPosition { x: 7, y: 4 }),
            ],
            &properties,
            &map,
            &master_data,
            PlayerId(1),
        );
        let after_first_move = update_objective_assignments(
            &mut world,
            &[
                assignment_unit(first, GridPosition { x: 7, y: 5 }),
                assignment_unit(second, GridPosition { x: 7, y: 4 }),
            ],
            &properties,
            &map,
            &master_data,
            PlayerId(1),
        );

        assert_eq!(after_first_move, initial);
        assert_eq!(after_first_move[&second], GridPosition { x: 5, y: 7 });

        properties[0].1.owner_id = Some(PlayerId(1));
        let after_ally_capture = update_objective_assignments(
            &mut world,
            &[
                assignment_unit(first, GridPosition { x: 7, y: 5 }),
                assignment_unit(second, GridPosition { x: 5, y: 7 }),
            ],
            &properties,
            &map,
            &master_data,
            PlayerId(1),
        );
        assert_eq!(after_ally_capture, initial);
    }

    #[test]
    fn map1_second_wave_uses_rom_virtual_distance_and_later_scan_tie() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_1",
            crate::resources::GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>().clone();
        let properties: Vec<_> = {
            let mut query = world.query::<(&GridPosition, &Property)>();
            query
                .iter(&world)
                .map(|(position, property)| (*position, *property))
                .collect()
        };
        let first_wave = [
            assignment_unit(Entity::from_raw(0), GridPosition { x: 6, y: 3 }),
            assignment_unit(Entity::from_raw(1), GridPosition { x: 7, y: 4 }),
            assignment_unit(Entity::from_raw(2), GridPosition { x: 6, y: 4 }),
            assignment_unit(Entity::from_raw(3), GridPosition { x: 7, y: 3 }),
            assignment_unit(Entity::from_raw(4), GridPosition { x: 5, y: 3 }),
        ];
        update_objective_assignments(
            &mut world,
            &first_wave,
            &properties,
            &map,
            &master_data,
            PlayerId(1),
        );
        let mut all_units = first_wave.to_vec();
        all_units.extend([
            AssignmentUnit {
                entity: Entity::from_raw(5),
                position: GridPosition { x: 6, y: 3 },
                unit_type: UnitType::Recon,
                record_order: 5,
                has_cargo: false,
                is_transported: false,
            },
            assignment_unit(Entity::from_raw(6), GridPosition { x: 7, y: 4 }),
            assignment_unit(Entity::from_raw(7), GridPosition { x: 6, y: 4 }),
            AssignmentUnit {
                entity: Entity::from_raw(8),
                position: GridPosition { x: 7, y: 3 },
                unit_type: UnitType::Recon,
                record_order: 8,
                has_cargo: false,
                is_transported: false,
            },
            assignment_unit(Entity::from_raw(9), GridPosition { x: 5, y: 3 }),
        ]);

        let assignments = update_objective_assignments(
            &mut world,
            &all_units,
            &properties,
            &map,
            &master_data,
            PlayerId(1),
        );

        assert_eq!(
            assignments[&Entity::from_raw(9)],
            // 実機のrecord 9は同距離の生産拠点を後勝ちで選び、raw(5,11)となる。
            GridPosition { x: 4, y: 10 }
        );
        assert_eq!(
            assignments[&Entity::from_raw(5)],
            GridPosition { x: 3, y: 11 }
        );
    }

    #[test]
    fn map3_first_bcopters_receives_rom_mission_two_objective() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_3",
            crate::resources::GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>().clone();
        let properties: Vec<_> = {
            let mut query = world.query::<(&GridPosition, &Property)>();
            query
                .iter(&world)
                .map(|(position, property)| (*position, *property))
                .collect()
        };
        let mut units = vec![
            assignment_unit(Entity::from_raw(0), GridPosition { x: 5, y: 5 }),
            assignment_unit(Entity::from_raw(1), GridPosition { x: 6, y: 6 }),
            assignment_unit(Entity::from_raw(2), GridPosition { x: 5, y: 6 }),
        ];
        let mut bcopter = assignment_unit(Entity::from_raw(3), GridPosition { x: 6, y: 4 });
        bcopter.unit_type = UnitType::Bcopters;
        units.push(bcopter);
        units.extend([
            assignment_unit(Entity::from_raw(4), GridPosition { x: 6, y: 5 }),
            assignment_unit(Entity::from_raw(5), GridPosition { x: 4, y: 5 }),
        ]);

        let assignments = update_objective_assignments(
            &mut world,
            &units,
            &properties,
            &map,
            &master_data,
            PlayerId(1),
        );

        // ROMシナリオ+0x94/+0x95の(7,19)を0始まりへ変換した座標。
        assert_eq!(
            assignments[&Entity::from_raw(3)],
            GridPosition { x: 6, y: 18 }
        );
    }

    #[test]
    fn map3_white_first_wave_receives_rom_capture_objectives() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_3",
            crate::resources::GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>().clone();
        let properties: Vec<_> = {
            let mut query = world.query::<(&GridPosition, &Property)>();
            query
                .iter(&world)
                .map(|(position, property)| (*position, *property))
                .collect()
        };
        let units = [
            assignment_unit(Entity::from_raw(0), GridPosition { x: 24, y: 24 }),
            assignment_unit(Entity::from_raw(1), GridPosition { x: 23, y: 24 }),
            assignment_unit(Entity::from_raw(2), GridPosition { x: 23, y: 23 }),
            assignment_unit(Entity::from_raw(4), GridPosition { x: 24, y: 23 }),
            assignment_unit(Entity::from_raw(5), GridPosition { x: 25, y: 24 }),
        ];

        let assignments = update_objective_assignments(
            &mut world,
            &units,
            &properties,
            &map,
            &master_data,
            PlayerId(2),
        );
        let actual: Vec<_> = units.iter().map(|unit| assignments[&unit.entity]).collect();

        assert_eq!(
            actual,
            vec![
                GridPosition { x: 24, y: 27 },
                GridPosition { x: 22, y: 26 },
                GridPosition { x: 22, y: 22 },
                GridPosition { x: 25, y: 19 },
                GridPosition { x: 19, y: 25 },
            ]
        );
    }

    #[test]
    fn map3_transport_helicopter_reassigns_objective_after_loading() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_3",
            crate::resources::GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>().clone();
        let properties: Vec<_> = {
            let mut query = world.query::<(&GridPosition, &Property)>();
            query
                .iter(&world)
                .map(|(position, property)| (*position, *property))
                .collect()
        };
        let entity = Entity::from_raw(9);
        let mut transport = AssignmentUnit {
            entity,
            position: GridPosition { x: 5, y: 4 },
            unit_type: UnitType::TransportHelicopter,
            record_order: 9,
            has_cargo: false,
            is_transported: false,
        };

        let empty_assignment = update_objective_assignments(
            &mut world,
            &[transport],
            &properties,
            &map,
            &master_data,
            PlayerId(1),
        );
        assert_eq!(
            empty_assignment[&entity],
            GridPosition { x: 5, y: 5 },
            "ROM 483Fは空の輸送ヘリを自軍首都へ割り当てる"
        );

        transport.position = GridPosition { x: 7, y: 6 };
        transport.has_cargo = true;
        world.resource_mut::<MatchState>().current_turn_number.0 += 1;
        let loaded_assignment = update_objective_assignments(
            &mut world,
            &[transport],
            &properties,
            &map,
            &master_data,
            PlayerId(1),
        );
        assert_eq!(
            loaded_assignment[&entity],
            GridPosition { x: 6, y: 8 },
            "ROM 485Eの歩兵経路フィールドで搭載後の施設目標を選ぶ"
        );
    }

    #[test]
    fn wait_uses_later_row_major_candidate_when_distance_is_equal() {
        let map = Map::new(
            10,
            14,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Hex,
        );
        let actor = unit(0, MovementType::Infantry, GridPosition { x: 6, y: 3 });
        let candidates = vec![
            CandidateTile {
                position: GridPosition { x: 6, y: 5 },
                movement_cost: 2,
            },
            CandidateTile {
                position: GridPosition { x: 7, y: 5 },
                movement_cost: 2,
            },
        ];

        assert!(matches!(
            choose_wait(
                &actor,
                &candidates,
                Some(GridPosition { x: 7, y: 7 }),
                &map,
                &MasterDataRegistry::load().unwrap(),
            )
            .0,
            AiCommand::Wait {
                target_pos: GridPosition { x: 7, y: 5 }
            }
        ));
    }

    #[test]
    fn capture_uses_reachable_movement_cost_instead_of_strategic_objective() {
        let mut actor = unit(0, MovementType::Infantry, GridPosition { x: 1, y: 1 });
        actor.stats.can_capture = true;
        let near = GridPosition { x: 2, y: 1 };
        let far = GridPosition { x: 4, y: 1 };
        let candidates = [
            CandidateTile {
                position: near,
                movement_cost: 1,
            },
            CandidateTile {
                position: far,
                movement_cost: 3,
            },
        ];
        let properties = [
            (
                near,
                Property::new(crate::resources::Terrain::City, None, 200),
            ),
            (
                far,
                Property::new(crate::resources::Terrain::City, None, 200),
            ),
        ];

        assert!(matches!(
            choose_capture(&actor, &candidates, &properties, PlayerId(1)),
            Some(AiCommand::Capture { target_pos }) if target_pos == near
        ));
    }

    #[test]
    fn fallback_staging_matches_map3_rom_coastal_scan() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_3",
            crate::resources::GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>();

        // ROM実測ではraw(24,11)に対し、同距離候補の後側raw(22,12)を残す。
        assert_eq!(
            select_rom_fallback_staging(map, GridPosition { x: 23, y: 10 }),
            Some(GridPosition { x: 21, y: 11 })
        );
        // 反対岸もraw(6,19)に対してraw(7,19)の港を選ぶ。
        assert_eq!(
            select_rom_fallback_staging(map, GridPosition { x: 5, y: 18 }),
            Some(GridPosition { x: 6, y: 18 })
        );
    }

    #[test]
    fn wait_avoids_mountain_route_like_white_fifth_infantry() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_1",
            crate::resources::GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>().clone();
        let mut actor = unit(0, MovementType::Infantry, GridPosition { x: 2, y: 11 });
        actor.stats.max_movement = 3;
        let candidates = build_candidate_field(
            &map,
            &HashMap::new(),
            actor.position,
            &actor.stats,
            actor.fuel,
            PlayerId(2),
            &master_data,
        );

        let (command, used_fallback) = choose_wait(
            &actor,
            &candidates,
            Some(GridPosition { x: 6, y: 4 }),
            &map,
            &master_data,
        );
        let route_field = build_route_field(
            &map,
            &master_data,
            GridPosition { x: 6, y: 4 },
            MovementType::Infantry,
        );
        let candidate_values: Vec<_> = candidates
            .iter()
            .map(|candidate| route_field[&candidate.position])
            .collect();
        assert_eq!(candidate_values, vec![7, 7, 9, 8, 8, 7, 10, 9, 9, 8, 8]);
        assert!(
            matches!(
                command,
                AiCommand::Wait {
                    target_pos: GridPosition { x: 5, y: 10 }
                }
            ),
            "{command:?}"
        );
        assert!(!used_fallback);
    }

    #[test]
    fn ship_routes_around_land_barrier_toward_inland_objective() {
        let master_data = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(
            7,
            5,
            crate::resources::Terrain::Sea,
            crate::resources::GridTopology::Hex,
        );
        // 内陸目標へ直進すると西岸で止まるが、南側を回れば東岸まで到達できる。
        for y in 0..4 {
            map.set_terrain(3, y, crate::resources::Terrain::Plains)
                .unwrap();
        }
        map.set_terrain(4, 1, crate::resources::Terrain::City)
            .unwrap();
        let actor_position = GridPosition { x: 2, y: 1 };
        let actor = unit(0, MovementType::Ship, actor_position);
        let candidates = vec![
            CandidateTile {
                position: actor_position,
                movement_cost: 0,
            },
            CandidateTile {
                position: GridPosition { x: 2, y: 2 },
                movement_cost: 1,
            },
        ];

        let (command, _) = choose_wait(
            &actor,
            &candidates,
            Some(GridPosition { x: 4, y: 1 }),
            &map,
            &master_data,
        );

        assert!(matches!(
            command,
            AiCommand::Wait {
                target_pos: GridPosition { x: 2, y: 2 }
            }
        ));
    }

    #[test]
    fn rom_influence_field_saturates_each_component_at_seven() {
        let map = Map::new(
            7,
            5,
            crate::resources::Terrain::Sea,
            crate::resources::GridTopology::Hex,
        );
        let stats = UnitStats {
            unit_type: UnitType::Battleship,
            min_range: 1,
            max_range: 1,
            ..UnitStats::mock()
        };
        let enemies: Vec<_> = (0..3)
            .map(|entity| EnemyView {
                entity: Entity::from_raw(entity),
                position: GridPosition { x: 4, y: 2 },
                stats: stats.clone(),
                hp: 100,
            })
            .collect();

        let influence = build_rom_influence_field(&map, &enemies);

        assert_eq!(
            influence[&GridPosition { x: 3, y: 1 }],
            RomInfluence {
                anti_air: 7,
                anti_surface: 7,
            }
        );
        assert!(!influence.contains_key(&GridPosition { x: 4, y: 2 }));
    }

    #[test]
    fn rom_influence_snapshot_is_kept_until_the_next_turn() {
        let map = Map::new(
            7,
            5,
            crate::resources::Terrain::Sea,
            crate::resources::GridTopology::Hex,
        );
        let enemy = EnemyView {
            entity: Entity::from_raw(1),
            position: GridPosition { x: 4, y: 2 },
            stats: UnitStats {
                unit_type: UnitType::Battleship,
                min_range: 1,
                max_range: 1,
                ..UnitStats::mock()
            },
            hp: 100,
        };
        let mut world = World::new();

        let initial = rom_influence_for_turn(
            &mut world,
            PlayerId(1),
            4,
            &map,
            std::slice::from_ref(&enemy),
        );
        let after_enemy_removed = rom_influence_for_turn(&mut world, PlayerId(1), 4, &map, &[]);
        let next_turn = rom_influence_for_turn(&mut world, PlayerId(1), 5, &map, &[]);

        assert_eq!(after_enemy_removed, initial);
        assert!(next_turn.is_empty());
    }

    #[test]
    fn mission_three_selects_first_reachable_influence_maximum_instead_of_map_edge() {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(
            7,
            5,
            crate::resources::Terrain::Sea,
            crate::resources::GridTopology::Hex,
        );
        let mut actor = unit(0, MovementType::Ship, GridPosition { x: 1, y: 2 });
        actor.stats.unit_type = UnitType::Battleship;
        let enemy = EnemyView {
            entity: Entity::from_raw(1),
            position: GridPosition { x: 4, y: 2 },
            stats: UnitStats {
                unit_type: UnitType::Battleship,
                min_range: 1,
                max_range: 1,
                ..UnitStats::mock()
            },
            hp: 100,
        };
        let influence = build_rom_influence_field(&map, &[enemy]);
        let assignments = HashMap::new();
        let properties = [];

        let objective = resolve_rom_movement_objective(
            &actor,
            RomUnitObjective::DynamicInfluence,
            &RomMovementObjectiveContext {
                cargo: None,
                assignments: &assignments,
                influence: &influence,
                properties: &properties,
                map: &map,
                master_data: &master_data,
                player_id: PlayerId(1),
            },
        );

        assert_eq!(objective, Some(GridPosition { x: 3, y: 1 }));
    }

    #[test]
    fn mission_three_without_positive_influence_falls_back_to_enemy_capital() {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(
            7,
            5,
            crate::resources::Terrain::Sea,
            crate::resources::GridTopology::Hex,
        );
        let mut actor = unit(0, MovementType::Ship, GridPosition { x: 1, y: 2 });
        actor.stats.unit_type = UnitType::Battleship;
        let enemy_capital = GridPosition { x: 5, y: 2 };
        let properties = [(
            enemy_capital,
            Property::new(crate::resources::Terrain::Capital, Some(PlayerId(2)), 200),
        )];
        let assignments = HashMap::new();
        let influence = HashMap::new();

        let objective = resolve_rom_movement_objective(
            &actor,
            RomUnitObjective::DynamicInfluence,
            &RomMovementObjectiveContext {
                cargo: None,
                assignments: &assignments,
                influence: &influence,
                properties: &properties,
                map: &map,
                master_data: &master_data,
                player_id: PlayerId(1),
            },
        );

        assert_eq!(objective, Some(enemy_capital));
    }

    #[test]
    fn loaded_lander_uses_the_last_cargo_objective_like_rom_49b6() {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(
            7,
            5,
            crate::resources::Terrain::Sea,
            crate::resources::GridTopology::Hex,
        );
        let mut actor = unit(0, MovementType::Ship, GridPosition { x: 1, y: 2 });
        actor.stats.unit_type = UnitType::Lander;
        let first = Entity::from_raw(1);
        let last = Entity::from_raw(2);
        let last_objective = GridPosition { x: 5, y: 3 };
        let assignments = HashMap::from([
            (
                first,
                RomUnitObjective::Position(GridPosition { x: 4, y: 1 }),
            ),
            (last, RomUnitObjective::Position(last_objective)),
        ]);
        let influence = HashMap::new();
        let properties = [];

        let objective = resolve_rom_movement_objective(
            &actor,
            RomUnitObjective::DynamicInfluence,
            &RomMovementObjectiveContext {
                cargo: Some(&[first, last]),
                assignments: &assignments,
                influence: &influence,
                properties: &properties,
                map: &map,
                master_data: &master_data,
                player_id: PlayerId(1),
            },
        );

        assert_eq!(objective, Some(last_objective));
    }

    #[test]
    fn attack_key_prefers_defended_tile_without_unit_price() {
        let exposed = AttackKey {
            target_remaining_hp: 40,
            defensive_position: Reverse(10),
            target_hp: 70,
        };
        let defended = AttackKey {
            target_remaining_hp: 40,
            defensive_position: Reverse(30),
            target_hp: 70,
        };

        assert!(defended < exposed);
    }

    #[test]
    fn attack_movement_reserves_one_point_like_rom_field() {
        let mut infantry = unit(0, MovementType::Infantry, GridPosition { x: 0, y: 0 });
        let recon = unit(1, MovementType::ArmoredCar, GridPosition { x: 0, y: 0 });

        infantry.stats.max_movement = 3;
        assert_eq!(
            attack_movement_allowance(&infantry.stats, AiVersion::V100),
            2
        );
        assert_eq!(
            attack_movement_allowance(&infantry.stats, AiVersion::V200),
            3
        );
        assert_eq!(attack_movement_allowance(&recon.stats, AiVersion::V100), 5);

        let mut slow = infantry.stats;
        slow.max_movement = 2;
        assert_eq!(attack_movement_allowance(&slow, AiVersion::V100), 2);
    }

    #[test]
    fn iq200_treats_own_property_as_zero_attack_position_value() {
        let master_data = MasterDataRegistry::load().unwrap();
        let position = GridPosition { x: 1, y: 1 };
        let map = Map::new(
            3,
            3,
            crate::resources::Terrain::City,
            crate::resources::GridTopology::Hex,
        );
        let properties = [(
            position,
            Property::new(crate::resources::Terrain::City, Some(PlayerId(1)), 200),
        )];

        assert_eq!(
            attack_position_value(
                position,
                &map,
                &master_data,
                &properties,
                PlayerId(1),
                AiVersion::V200,
            ),
            0
        );
        assert_eq!(
            attack_position_value(
                position,
                &map,
                &master_data,
                &properties,
                PlayerId(1),
                AiVersion::V100,
            ),
            15
        );
    }
}
