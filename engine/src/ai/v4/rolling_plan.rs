//! 島作戦の残敵排除を、金額ではなく実行可能な行動列として比較する純粋計画器。
//!
//! このモジュールは盤面を直接読まず、呼び出し側が作ったsnapshotだけを受け取る。
//! 既存戦力と複数ターンの生産候補を混ぜ、悲観側ダメージで残敵を排除できる
//! パッケージだけを費用・完了ターン・損耗で比較する。

use crate::components::{GridPosition, UnitStats};
use crate::resources::{DamageChart, Map, Terrain, UnitType};
use crate::systems::combat::calculate_damage_formula;
use bevy_ecs::prelude::Entity;
use std::collections::HashSet;

const SEARCH_BEAM_WIDTH: usize = 24;
const MAX_NEW_UNITS: usize = 5;
const FUTURE_PRODUCTION_TURNS: u32 = 2;
const DEFAULT_SEARCH_TURNS: u32 = 12;
const CAPTURE_COMPLETION_TURNS: u32 = 2;

#[derive(Debug, Clone)]
pub(crate) struct FriendlyPlanUnit {
    pub stats: UnitStats,
    pub position: GridPosition,
    pub hp: u32,
    /// 0は既存unit、1以上は生産完了後に行動可能になる相対ターン。
    pub available_turn: u32,
    /// 地形連結と武装の両方を満たし、実際に交戦できる敵index。
    pub engageable_enemy_indices: Vec<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct EnemyPlanUnit {
    pub entity: Option<Entity>,
    pub stats: UnitStats,
    pub position: GridPosition,
    pub hp: u32,
    pub defense_bonus: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PlannedPurchase {
    pub facility: GridPosition,
    pub unit_type: UnitType,
    /// 0なら今手番、1以上なら将来手番の生産予定。
    pub build_turn: u32,
    pub cost: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct ProductionPlanOption {
    pub purchase: PlannedPurchase,
    pub stats: UnitStats,
    /// 生産地点から実際に交戦できる敵index。
    pub engageable_enemy_indices: Vec<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct RollingPlanInput {
    pub map: Map,
    pub damage_chart: DamageChart,
    pub existing_units: Vec<FriendlyPlanUnit>,
    pub enemies: Vec<EnemyPlanUnit>,
    pub production_options: Vec<ProductionPlanOption>,
    pub current_funds: u32,
    pub income_per_turn: u32,
    /// 観測可能な盤面イベントから導出した硬い期限。なければNone。
    pub hard_deadline: Option<u32>,
    /// 占領が1ターン遅れる機会損失。実行可能案同士の比較にだけ用いる。
    pub delay_cost_per_turn: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetForecast {
    pub entity: Option<Entity>,
    pub unit_type: UnitType,
    pub initial_hp: u32,
    pub remaining_hp: u32,
    pub destroyed_turn: Option<u32>,
}

#[derive(Debug, Clone)]
pub(crate) struct ForcePackagePlan {
    pub purchases: Vec<PlannedPurchase>,
    pub target_forecasts: Vec<TargetForecast>,
    pub feasible: bool,
    pub first_attack_turn: Option<u32>,
    pub elimination_turn: Option<u32>,
    pub occupation_turn: Option<u32>,
    pub production_cost: u32,
    pub expected_loss: u32,
    pub candidates_considered: usize,
    pub search_truncated: bool,
}

impl ForcePackagePlan {
    pub(crate) fn current_purchases(&self) -> impl Iterator<Item = PlannedPurchase> + '_ {
        self.purchases
            .iter()
            .copied()
            .filter(|purchase| purchase.build_turn == 0)
    }

    fn remaining_hp(&self) -> u32 {
        self.target_forecasts
            .iter()
            .map(|target| target.remaining_hp)
            .sum()
    }

    fn completion_for_ordering(&self) -> u32 {
        self.occupation_turn.unwrap_or(u32::MAX)
    }

    fn utility_cost(&self, delay_cost_per_turn: u32) -> u64 {
        u64::from(self.production_cost)
            + u64::from(self.expected_loss)
            + u64::from(self.completion_for_ordering())
                .saturating_mul(u64::from(delay_cost_per_turn))
    }
}

#[derive(Debug, Clone)]
struct SimFriendly {
    stats: UnitStats,
    position: GridPosition,
    hp: u32,
    initial_hp: u32,
    available_turn: u32,
    attacks_left: u32,
    engageable_enemy_indices: Vec<usize>,
}

#[derive(Debug, Clone)]
struct SimEnemy {
    source: EnemyPlanUnit,
    hp: u32,
    destroyed_turn: Option<u32>,
}

#[derive(Debug, Clone, Default)]
struct SearchState {
    option_indices: Vec<usize>,
    used_slots: HashSet<(usize, usize, u32)>,
    cost: u32,
}

/// 現在観測した敵を排除できる混成パッケージを探索する。
pub(crate) fn plan_force_package(input: &RollingPlanInput) -> Option<ForcePackagePlan> {
    if input.enemies.is_empty() {
        return Some(ForcePackagePlan {
            purchases: Vec::new(),
            target_forecasts: Vec::new(),
            feasible: true,
            first_attack_turn: None,
            elimination_turn: Some(0),
            occupation_turn: Some(CAPTURE_COMPLETION_TURNS),
            production_cost: 0,
            expected_loss: 0,
            candidates_considered: 1,
            search_truncated: false,
        });
    }

    let search_turns = input.hard_deadline.unwrap_or(DEFAULT_SEARCH_TURNS).max(1);
    let mut frontier = vec![SearchState::default()];
    let mut evaluated = Vec::new();
    let mut considered = 0_usize;
    let mut truncated = false;

    for depth in 0..=MAX_NEW_UNITS {
        let mut next = Vec::new();
        for state in &frontier {
            let mut plan = simulate_state(input, state, search_turns);
            considered = considered.saturating_add(1);
            plan.candidates_considered = considered;
            evaluated.push(plan);
            if depth == MAX_NEW_UNITS {
                continue;
            }

            let start_index = state.option_indices.last().map_or(0, |index| index + 1);
            for option_index in start_index..input.production_options.len() {
                let option = &input.production_options[option_index];
                let slot = (
                    option.purchase.facility.x,
                    option.purchase.facility.y,
                    option.purchase.build_turn,
                );
                if state.used_slots.contains(&slot) {
                    continue;
                }
                let next_cost = state.cost.saturating_add(option.purchase.cost);
                let available_by_build_turn = input.current_funds.saturating_add(
                    input
                        .income_per_turn
                        .saturating_mul(option.purchase.build_turn),
                );
                if next_cost > available_by_build_turn {
                    continue;
                }
                let mut child = state.clone();
                child.option_indices.push(option_index);
                child.used_slots.insert(slot);
                child.cost = next_cost;
                next.push(child);
            }
        }

        if next.len() > SEARCH_BEAM_WIDTH {
            truncated = true;
            next.sort_by_key(|state| {
                let plan = simulate_state(input, state, search_turns);
                (
                    plan.remaining_hp(),
                    plan.completion_for_ordering(),
                    plan.production_cost,
                )
            });
            next.truncate(SEARCH_BEAM_WIDTH);
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }

    let mut feasible: Vec<_> = evaluated
        .iter()
        .filter(|plan| plan.feasible)
        .cloned()
        .collect();
    if let Some(deadline) = input.hard_deadline {
        let within_deadline: Vec<_> = feasible
            .iter()
            .filter(|plan| plan.elimination_turn.is_some_and(|turn| turn <= deadline))
            .cloned()
            .collect();
        if !within_deadline.is_empty() {
            feasible = within_deadline;
        }
    }
    remove_dominated(&mut feasible);

    let mut selected = feasible
        .into_iter()
        .min_by_key(|plan| {
            (
                plan.utility_cost(input.delay_cost_per_turn),
                plan.completion_for_ordering(),
                plan.production_cost,
            )
        })
        .or_else(|| {
            // 期限内に全滅できる案が無くても、何も作らず停止しない。
            // 探索済み候補のうち残HPを最も減らす案を次revisionへのbest effortとする。
            evaluated.into_iter().min_by_key(|plan| {
                (
                    plan.remaining_hp(),
                    plan.first_attack_turn.unwrap_or(u32::MAX),
                    plan.production_cost,
                )
            })
        })?;
    selected.candidates_considered = considered;
    selected.search_truncated = truncated;
    Some(selected)
}

fn remove_dominated(plans: &mut Vec<ForcePackagePlan>) {
    let snapshot = plans.clone();
    plans.retain(|candidate| {
        !snapshot.iter().any(|other| {
            let no_worse = other.completion_for_ordering() <= candidate.completion_for_ordering()
                && other.production_cost <= candidate.production_cost
                && other.expected_loss <= candidate.expected_loss;
            let strictly_better = other.completion_for_ordering()
                < candidate.completion_for_ordering()
                || other.production_cost < candidate.production_cost
                || other.expected_loss < candidate.expected_loss;
            no_worse && strictly_better
        })
    });
}

fn simulate_state(
    input: &RollingPlanInput,
    state: &SearchState,
    search_turns: u32,
) -> ForcePackagePlan {
    let purchases: Vec<_> = state
        .option_indices
        .iter()
        .map(|index| input.production_options[*index].purchase)
        .collect();
    let mut friendlies: Vec<_> = input
        .existing_units
        .iter()
        .map(sim_friendly)
        .chain(state.option_indices.iter().map(|index| {
            let option = &input.production_options[*index];
            sim_friendly(&FriendlyPlanUnit {
                stats: option.stats.clone(),
                position: option.purchase.facility,
                hp: 100,
                available_turn: option.purchase.build_turn.saturating_add(1),
                engageable_enemy_indices: option.engageable_enemy_indices.clone(),
            })
        }))
        .collect();
    let mut enemies: Vec<_> = input
        .enemies
        .iter()
        .cloned()
        .map(|source| SimEnemy {
            hp: source.hp,
            source,
            destroyed_turn: None,
        })
        .collect();
    let mut first_attack_turn = None;

    for turn in 1..=search_turns {
        for friendly in &mut friendlies {
            if friendly.hp == 0 || friendly.attacks_left == 0 || turn < friendly.available_turn {
                continue;
            }
            let Some(target_index) = select_target(input, friendly, &enemies, turn) else {
                continue;
            };
            let target = &mut enemies[target_index];
            let base_damage = best_damage(
                &input.damage_chart,
                friendly.stats.unit_type,
                target.source.stats.unit_type,
            );
            let damage = calculate_damage_formula(
                base_damage,
                friendly.hp,
                target.source.defense_bonus,
                false,
            );
            if damage == 0 {
                continue;
            }
            first_attack_turn.get_or_insert(turn);
            target.hp = target.hp.saturating_sub(damage);
            friendly.attacks_left = friendly.attacks_left.saturating_sub(1);
            if target.hp == 0 {
                target.destroyed_turn = Some(turn);
                continue;
            }

            // 直接戦闘だけは反撃を受ける。悲観側では乱数ボーナス最大を加える。
            if friendly.stats.max_range <= 1 {
                let counter_base = best_damage(
                    &input.damage_chart,
                    target.source.stats.unit_type,
                    friendly.stats.unit_type,
                );
                if counter_base > 0 {
                    let counter = calculate_damage_formula(counter_base, target.hp, 0, true)
                        .saturating_add(10);
                    friendly.hp = friendly.hp.saturating_sub(counter);
                }
            }
        }
        if enemies.iter().all(|enemy| enemy.hp == 0) {
            break;
        }
    }

    let elimination_turn = enemies
        .iter()
        .map(|enemy| enemy.destroyed_turn)
        .collect::<Option<Vec<_>>>()
        .and_then(|turns| turns.into_iter().max());
    let occupation_turn =
        elimination_turn.map(|turn| turn.saturating_add(CAPTURE_COMPLETION_TURNS));
    let expected_loss = friendlies.iter().fold(0_u32, |total, unit| {
        let lost_hp = unit.initial_hp.saturating_sub(unit.hp);
        total.saturating_add(unit.stats.cost.saturating_mul(lost_hp) / 100)
    });
    ForcePackagePlan {
        purchases,
        target_forecasts: enemies
            .into_iter()
            .map(|enemy| TargetForecast {
                entity: enemy.source.entity,
                unit_type: enemy.source.stats.unit_type,
                initial_hp: enemy.source.hp,
                remaining_hp: enemy.hp,
                destroyed_turn: enemy.destroyed_turn,
            })
            .collect(),
        feasible: elimination_turn.is_some(),
        first_attack_turn,
        elimination_turn,
        occupation_turn,
        production_cost: state.cost,
        expected_loss,
        candidates_considered: 0,
        search_truncated: false,
    }
}

fn sim_friendly(source: &FriendlyPlanUnit) -> SimFriendly {
    let total_ammo = source
        .stats
        .max_ammo1
        .saturating_add(source.stats.max_ammo2)
        .max(1);
    let fuel_turns = source
        .stats
        .max_fuel
        .checked_div(source.stats.daily_fuel_consumption)
        .unwrap_or(u32::MAX);
    SimFriendly {
        stats: source.stats.clone(),
        position: source.position,
        hp: source.hp,
        initial_hp: source.hp,
        available_turn: source.available_turn,
        attacks_left: total_ammo.min(fuel_turns.max(1)),
        engageable_enemy_indices: source.engageable_enemy_indices.clone(),
    }
}

fn select_target(
    input: &RollingPlanInput,
    friendly: &SimFriendly,
    enemies: &[SimEnemy],
    turn: u32,
) -> Option<usize> {
    enemies
        .iter()
        .enumerate()
        .filter(|(_, enemy)| enemy.hp > 0)
        .filter_map(|(index, enemy)| {
            if !friendly.engageable_enemy_indices.contains(&index) {
                return None;
            }
            let base_damage = best_damage(
                &input.damage_chart,
                friendly.stats.unit_type,
                enemy.source.stats.unit_type,
            );
            if base_damage == 0 {
                return None;
            }
            let distance = input.map.distance(
                friendly.position.x,
                friendly.position.y,
                enemy.source.position.x,
                enemy.source.position.y,
            );
            let travel = distance
                .saturating_sub(friendly.stats.max_range.max(1))
                .div_ceil(friendly.stats.max_movement.max(1));
            if turn < friendly.available_turn.saturating_add(travel) {
                return None;
            }
            let strategic_rank = if enemy.source.stats.can_capture {
                0
            } else if enemy.source.stats.max_cargo > 0 {
                1
            } else {
                2
            };
            Some((strategic_rank, enemy.hp, index))
        })
        .min()
        .map(|(_, _, index)| index)
}

fn best_damage(chart: &DamageChart, attacker: UnitType, defender: UnitType) -> u32 {
    chart.get_base_damage(attacker, defender).unwrap_or(0).max(
        chart
            .get_base_damage_secondary(attacker, defender)
            .unwrap_or(0),
    )
}

/// 生産可能施設から、現在手番と将来2手番分の離散的な生産slotを作る。
pub(crate) fn production_options(
    current_facilities: &[(GridPosition, Terrain)],
    future_facilities: &[(GridPosition, Terrain)],
    available_types: &[(UnitType, UnitStats)],
    master_data: &crate::resources::master_data::MasterDataRegistry,
    mut can_reach: impl FnMut(GridPosition, &UnitStats) -> bool,
) -> Vec<ProductionPlanOption> {
    let mut options = Vec::new();
    for build_turn in 0..=FUTURE_PRODUCTION_TURNS {
        let facilities = if build_turn == 0 {
            current_facilities
        } else {
            future_facilities
        };
        for (facility, terrain) in facilities {
            for (unit_type, stats) in available_types {
                if stats.cost == 0
                    || stats.can_capture
                    || stats.max_cargo > 0
                    || !master_data.can_produce_unit(terrain.as_str(), *unit_type)
                    || !can_reach(*facility, stats)
                {
                    continue;
                }
                options.push(ProductionPlanOption {
                    purchase: PlannedPurchase {
                        facility: *facility,
                        unit_type: *unit_type,
                        build_turn,
                        cost: stats.cost,
                    },
                    stats: stats.clone(),
                    engageable_enemy_indices: Vec::new(),
                });
            }
        }
    }
    options.sort_by_key(|option| {
        (
            option.purchase.build_turn,
            option.purchase.facility.y,
            option.purchase.facility.x,
            option.purchase.cost,
            option.purchase.unit_type.as_str(),
        )
    });
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{GridTopology, MovementType};

    fn stats(unit_type: UnitType, cost: u32, movement: u32) -> UnitStats {
        UnitStats {
            unit_type,
            cost,
            max_movement: movement,
            movement_type: MovementType::Air,
            max_fuel: 99,
            max_ammo1: 9,
            min_range: 1,
            max_range: 1,
            ..UnitStats::mock()
        }
    }

    fn input() -> RollingPlanInput {
        let map = Map::new(12, 1, Terrain::Plains, GridTopology::Square);
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 40);
        chart.insert_damage(UnitType::Bomber, UnitType::Infantry, 100);
        chart.insert_damage(UnitType::Infantry, UnitType::Bcopters, 10);
        RollingPlanInput {
            map,
            damage_chart: chart,
            existing_units: Vec::new(),
            enemies: vec![EnemyPlanUnit {
                entity: Some(Entity::from_raw(7)),
                stats: UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1_000,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                position: GridPosition { x: 8, y: 0 },
                hp: 100,
                defense_bonus: 0,
            }],
            production_options: vec![
                ProductionPlanOption {
                    purchase: PlannedPurchase {
                        facility: GridPosition { x: 0, y: 0 },
                        unit_type: UnitType::Bcopters,
                        build_turn: 0,
                        cost: 7_500,
                    },
                    stats: stats(UnitType::Bcopters, 7_500, 6),
                    engageable_enemy_indices: vec![0],
                },
                ProductionPlanOption {
                    purchase: PlannedPurchase {
                        facility: GridPosition { x: 1, y: 0 },
                        unit_type: UnitType::Bomber,
                        build_turn: 0,
                        cost: 20_000,
                    },
                    stats: stats(UnitType::Bomber, 20_000, 6),
                    engageable_enemy_indices: vec![0],
                },
            ],
            current_funds: 30_000,
            income_per_turn: 0,
            hard_deadline: None,
            delay_cost_per_turn: 5_000,
        }
    }

    #[test]
    fn fast_expensive_package_can_beat_slow_cheap_package() {
        let plan = plan_force_package(&input()).unwrap();
        assert!(plan.feasible);
        assert_eq!(plan.purchases.len(), 1);
        assert_eq!(plan.purchases[0].unit_type, UnitType::Bomber);
        assert_eq!(plan.elimination_turn, Some(2));
    }

    #[test]
    fn production_unit_cannot_attack_before_travel_finishes() {
        let mut input = input();
        input.production_options.truncate(1);
        let plan = plan_force_package(&input).unwrap();
        assert!(plan.first_attack_turn.is_some_and(|turn| turn >= 3));
    }

    #[test]
    fn mixed_package_is_selected_when_one_type_cannot_clear_all_targets() {
        let mut input = input();
        input
            .damage_chart
            .insert_damage(UnitType::Bcopters, UnitType::Infantry, 0);
        input
            .damage_chart
            .insert_damage(UnitType::Bomber, UnitType::Fighter, 0);
        input
            .damage_chart
            .insert_damage(UnitType::Bcopters, UnitType::Fighter, 70);
        input.enemies.push(EnemyPlanUnit {
            entity: Some(Entity::from_raw(8)),
            stats: stats(UnitType::Fighter, 9_000, 9),
            position: GridPosition { x: 8, y: 0 },
            hp: 100,
            defense_bonus: 0,
        });
        for option in &mut input.production_options {
            option.engageable_enemy_indices = vec![0, 1];
        }
        let plan = plan_force_package(&input).unwrap();
        assert!(plan.feasible);
        assert!(
            plan.purchases
                .iter()
                .any(|purchase| purchase.unit_type == UnitType::Bomber)
        );
        assert!(
            plan.purchases
                .iter()
                .any(|purchase| purchase.unit_type == UnitType::Bcopters)
        );
    }

    #[test]
    fn occupied_facility_is_available_only_to_future_production_slots() {
        let registry = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let airport = GridPosition { x: 2, y: 0 };
        let options = production_options(
            &[],
            &[(airport, Terrain::Airport)],
            &[(UnitType::Bcopters, stats(UnitType::Bcopters, 7_500, 6))],
            &registry,
            |_, _| true,
        );

        assert!(!options.is_empty());
        assert!(options.iter().all(|option| option.purchase.build_turn > 0));
    }
}
