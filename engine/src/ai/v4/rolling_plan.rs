//! 島作戦の残敵排除を、金額ではなく実行可能な行動列として比較する純粋計画器。
//!
//! このモジュールは盤面を直接読まず、呼び出し側が作ったsnapshotだけを受け取る。
//! 既存戦力と複数ターンの生産候補を混ぜ、悲観側ダメージで残敵を排除できる
//! パッケージだけを費用・完了ターン・損耗で比較する。

use crate::components::{GridPosition, UnitStats};
use crate::resources::{DamageChart, Map, Terrain, UnitType};
use crate::systems::combat::calculate_damage_formula;
use bevy_ecs::prelude::Entity;
use std::collections::{HashMap, HashSet};

// 1施設手番ごとの候補を順に展開するため、同手番に複数施設を使う混成案が
// beamから落ちない幅を確保する。候補全体の直積を走査する旧方式には戻さない。
const SEARCH_BEAM_WIDTH: usize = 64;
pub(crate) const DEFAULT_SEARCH_TURNS: u32 = 12;

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
    /// 0は現在盤面の敵。1以上は敵施設から前線へ到着する悲観scenarioの増援。
    pub available_turn: u32,
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
    /// 実在するcampaign cargoと輸送phaseから予測した占領完了turn。未編成ならNone。
    pub capture_completion_turn: Option<u32>,
    /// 占領が1ターン遅れる機会損失。実行可能案同士の比較にだけ用いる。
    pub delay_cost_per_turn: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetForecast {
    pub entity: Option<Entity>,
    pub unit_type: UnitType,
    pub available_turn: u32,
    pub initial_hp: u32,
    pub remaining_hp: u32,
    pub destroyed_turn: Option<u32>,
}

/// 作戦の各手番で、前線へ入る敵HPと実際に除去できるHPを比較する予測。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CampaignTurnForecast {
    pub turn: u32,
    pub enemy_arrival_hp: u32,
    pub enemy_hp_removed: u32,
    pub friendly_hp_lost: u32,
    pub attack_count: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct ForcePackagePlan {
    pub purchases: Vec<PlannedPurchase>,
    pub target_forecasts: Vec<TargetForecast>,
    pub turn_forecasts: Vec<CampaignTurnForecast>,
    pub feasible: bool,
    pub first_attack_turn: Option<u32>,
    pub elimination_turn: Option<u32>,
    pub occupation_turn: Option<u32>,
    pub production_cost: u32,
    pub expected_loss: u32,
    pub candidates_considered: usize,
    pub search_truncated: bool,
}

/// 永続化した生産列を現在盤面へ載せ直せない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixedPackageError {
    /// 施設喪失・兵種制約・到達性変化により、予定していた生産slotが消えた。
    ProductionSlotUnavailable,
    /// 同じ施設・同じ手番を複数の生産へ割り当てている。
    DuplicateProductionSlot,
    /// 予定手番までの所持金と収入では購入列を実行できない。
    FundingUnavailable,
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
        self.occupation_turn
            .or(self.elimination_turn)
            .unwrap_or(u32::MAX)
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
            turn_forecasts: Vec::new(),
            feasible: true,
            first_attack_turn: None,
            elimination_turn: Some(0),
            occupation_turn: input.capture_completion_turn,
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

    // 実在するfacility-turnを1枠ずつ処理する。同じ枠の全兵種を深さごとに
    // 再展開すると、首都攻略のような長い購入列で同じ組合せを大量に作る。
    // 「作らない」または「この枠で1兵種を作る」を一度だけ分岐すれば、探索対象を
    // 減らさずにゲームルールの1施設1生産へ一致させられる。
    let mut options_by_slot: HashMap<(usize, usize, u32), Vec<usize>> = HashMap::new();
    for (index, option) in input.production_options.iter().enumerate() {
        options_by_slot
            .entry((
                option.purchase.facility.x,
                option.purchase.facility.y,
                option.purchase.build_turn,
            ))
            .or_default()
            .push(index);
    }
    let mut slots = options_by_slot.into_iter().collect::<Vec<_>>();
    slots.sort_unstable_by_key(|((x, y, build_turn), _)| (*build_turn, *y, *x));

    for (slot, option_indices) in slots {
        let mut next = Vec::new();
        for state in &frontier {
            // この枠を使わない案も残す。高額兵種のための現金予約や、将来の別施設を
            // 選ぶ案を、安い現在購入で強制的に上書きしないためである。
            next.push(state.clone());
            for option_index in &option_indices {
                let option = &input.production_options[*option_index];
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
                child.option_indices.push(*option_index);
                child.used_slots.insert(slot);
                child.cost = next_cost;
                next.push(child);
            }
        }

        for state in &next {
            let mut plan = simulate_state(input, state, search_turns);
            considered = considered.saturating_add(1);
            plan.candidates_considered = considered;
            evaluated.push(plan);
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
    // 同じ施設・同じ兵種を後の手番に置く理由がなく、資金も足りるなら最早枠へ寄せる。
    // beam探索では将来収入で複数機を買う枝が残りやすいため、編成を変えずに
    // 生産だけを前倒しして「計画はあるのに今手番の施設が遊ぶ」状態を除く。
    let shifted_purchases = left_shift_purchases(input, &selected.purchases);
    if shifted_purchases != selected.purchases
        && let Ok(mut shifted) = evaluate_fixed_package(input, &shifted_purchases)
    {
        shifted.candidates_considered = considered;
        shifted.search_truncated = truncated;
        selected = shifted;
    }
    selected.candidates_considered = considered;
    selected.search_truncated = truncated;
    Some(selected)
}

/// 選ばれた編成を変えず、各購入を最も早い実行可能枠へ移す。
///
/// 候補は作戦地点へ交戦可能な施設・兵種だけに絞り込み済みなので、新たに確保した
/// 生産施設も利用する。仮置きするたび全手番の累積資金を確認し、将来収入の先食いは
/// 許さない。
fn left_shift_purchases(
    input: &RollingPlanInput,
    purchases: &[PlannedPurchase],
) -> Vec<PlannedPurchase> {
    let mut ordered = purchases.to_vec();
    ordered.sort_unstable_by_key(|purchase| {
        (
            purchase.build_turn,
            purchase.facility.y,
            purchase.facility.x,
        )
    });

    let mut shifted = Vec::with_capacity(ordered.len());
    let mut used_slots = HashSet::new();
    // まだ処理していない購入の元予定枠は先に確保する。前倒し先がその枠を奪うと、
    // 後続購入が元へ戻ったときに同一施設・同一手番の重複が発生する。
    let mut reserved_original_slots = ordered
        .iter()
        .map(|purchase| {
            (
                purchase.facility.x,
                purchase.facility.y,
                purchase.build_turn,
            )
        })
        .collect::<HashSet<_>>();
    for purchase in ordered {
        let original_slot = (
            purchase.facility.x,
            purchase.facility.y,
            purchase.build_turn,
        );
        reserved_original_slots.remove(&original_slot);
        let mut candidates = input
            .production_options
            .iter()
            .map(|option| option.purchase)
            .filter(|candidate| {
                candidate.unit_type == purchase.unit_type
                    && candidate.build_turn <= purchase.build_turn
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|candidate| {
            (
                candidate.build_turn,
                candidate.facility != purchase.facility,
                candidate.facility.y,
                candidate.facility.x,
            )
        });

        let replacement = candidates.into_iter().find(|candidate| {
            let slot = (
                candidate.facility.x,
                candidate.facility.y,
                candidate.build_turn,
            );
            if used_slots.contains(&slot) || reserved_original_slots.contains(&slot) {
                return false;
            }
            let mut tentative = shifted.clone();
            tentative.push(*candidate);
            funding_suffices(input, &tentative)
        });
        let selected = replacement.unwrap_or(purchase);
        used_slots.insert((
            selected.facility.x,
            selected.facility.y,
            selected.build_turn,
        ));
        shifted.push(selected);
    }
    shifted.sort_unstable_by_key(|purchase| {
        (
            purchase.build_turn,
            purchase.facility.y,
            purchase.facility.x,
        )
    });
    shifted
}

fn funding_suffices(input: &RollingPlanInput, purchases: &[PlannedPurchase]) -> bool {
    let last_turn = purchases
        .iter()
        .map(|purchase| purchase.build_turn)
        .max()
        .unwrap_or(0);
    (0..=last_turn).all(|turn| {
        let required: u32 = purchases
            .iter()
            .filter(|purchase| purchase.build_turn <= turn)
            .map(|purchase| purchase.cost)
            .sum();
        let available = input
            .current_funds
            .saturating_add(input.income_per_turn.saturating_mul(turn));
        required <= available
    })
}

/// 前revisionの未実行購入列を、現在盤面の生産候補と資金へ載せ直して再評価する。
///
/// 新しい最適案を探す関数とは分離し、現行案を候補集合から消さずに比較できるようにする。
/// ここで失敗した計画だけが「実行不能」として撤回候補になる。
pub(crate) fn evaluate_fixed_package(
    input: &RollingPlanInput,
    purchases: &[PlannedPurchase],
) -> Result<ForcePackagePlan, FixedPackageError> {
    // 編成は固定したまま、現在利用できる新しい施設を含めて最早枠へ載せ直す。
    // 同じ兵種が同じ時点で前線へ参加できるなら、古い施設座標に計画を縛らない。
    let scheduled = left_shift_purchases(input, purchases);
    let mut indexed = scheduled
        .iter()
        .map(|purchase| {
            input
                .production_options
                .iter()
                .position(|option| option.purchase == *purchase)
                .map(|index| (purchase.build_turn, index))
                .ok_or(FixedPackageError::ProductionSlotUnavailable)
        })
        .collect::<Result<Vec<_>, _>>()?;
    indexed.sort_unstable_by_key(|(build_turn, _)| *build_turn);

    let mut state = SearchState::default();
    for (build_turn, option_index) in indexed {
        let option = &input.production_options[option_index];
        let slot = (
            option.purchase.facility.x,
            option.purchase.facility.y,
            build_turn,
        );
        if !state.used_slots.insert(slot) {
            return Err(FixedPackageError::DuplicateProductionSlot);
        }
        state.cost = state.cost.saturating_add(option.purchase.cost);
        let available = input
            .current_funds
            .saturating_add(input.income_per_turn.saturating_mul(build_turn));
        if state.cost > available {
            return Err(FixedPackageError::FundingUnavailable);
        }
        state.option_indices.push(option_index);
    }

    let search_turns = input.hard_deadline.unwrap_or(DEFAULT_SEARCH_TURNS).max(1);
    Ok(simulate_state(input, &state, search_turns))
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
    let mut turn_forecasts = Vec::new();

    for turn in 1..=search_turns {
        let enemy_arrival_hp = enemies
            .iter()
            .filter(|enemy| enemy.source.available_turn == turn)
            .map(|enemy| enemy.source.hp)
            .sum();
        let enemy_hp_before: u32 = enemies
            .iter()
            .filter(|enemy| enemy.source.available_turn <= turn)
            .map(|enemy| enemy.hp)
            .sum();
        let friendly_hp_before: u32 = friendlies.iter().map(|friendly| friendly.hp).sum();
        let mut attack_count = 0_u32;
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
            attack_count = attack_count.saturating_add(1);
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
        let enemy_hp_after: u32 = enemies
            .iter()
            .filter(|enemy| enemy.source.available_turn <= turn)
            .map(|enemy| enemy.hp)
            .sum();
        let friendly_hp_after: u32 = friendlies.iter().map(|friendly| friendly.hp).sum();
        turn_forecasts.push(CampaignTurnForecast {
            turn,
            enemy_arrival_hp,
            enemy_hp_removed: enemy_hp_before.saturating_sub(enemy_hp_after),
            friendly_hp_lost: friendly_hp_before.saturating_sub(friendly_hp_after),
            attack_count,
        });
        if enemies.iter().all(|enemy| enemy.hp == 0) {
            break;
        }
    }

    let elimination_turn = enemies
        .iter()
        .map(|enemy| enemy.destroyed_turn)
        .collect::<Option<Vec<_>>>()
        .and_then(|turns| turns.into_iter().max());
    // 固定2ターンを足さず、実campaignのPickup/Transit/Drop/Capture ETAと残敵排除の
    // 遅い方を採る。輸送編成がまだ存在しない場合は占領完了を予測しない。
    let occupation_turn = elimination_turn
        .zip(input.capture_completion_turn)
        .map(|(elimination, capture)| elimination.max(capture));
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
                available_turn: enemy.source.available_turn,
                initial_hp: enemy.source.hp,
                remaining_hp: enemy.hp,
                destroyed_turn: enemy.destroyed_turn,
            })
            .collect(),
        turn_forecasts,
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
        .filter(|(_, enemy)| turn >= enemy.source.available_turn)
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
    future_turns: u32,
    mut can_reach: impl FnMut(GridPosition, &UnitStats) -> bool,
) -> Vec<ProductionPlanOption> {
    let mut options = Vec::new();
    for build_turn in 0..future_turns {
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
                available_turn: 0,
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
            capture_completion_turn: None,
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
    fn occupation_uses_live_campaign_eta_instead_of_a_fixed_delay() {
        let mut input = input();
        input.capture_completion_turn = Some(7);

        let plan = plan_force_package(&input).unwrap();

        assert!(plan.elimination_turn.is_some_and(|turn| turn <= 7));
        assert_eq!(plan.occupation_turn, Some(7));
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
            available_turn: 0,
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
            3,
            |_, _| true,
        );

        assert!(!options.is_empty());
        assert!(options.iter().all(|option| option.purchase.build_turn > 0));
    }

    #[test]
    fn fixed_formation_uses_a_new_free_facility_without_changing_composition() {
        let mut input = input();
        let old_facility = GridPosition { x: 0, y: 0 };
        let new_facility = GridPosition { x: 2, y: 0 };
        input.production_options = vec![
            ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility: new_facility,
                    unit_type: UnitType::Bcopters,
                    build_turn: 0,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
            },
            ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility: old_facility,
                    unit_type: UnitType::Bcopters,
                    build_turn: 1,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
            },
        ];

        let plan = evaluate_fixed_package(
            &input,
            &[PlannedPurchase {
                facility: old_facility,
                unit_type: UnitType::Bcopters,
                build_turn: 1,
                cost: 7_500,
            }],
        )
        .unwrap();

        assert_eq!(plan.purchases.len(), 1);
        assert_eq!(plan.purchases[0].facility, new_facility);
        assert_eq!(plan.purchases[0].build_turn, 0);
        assert_eq!(plan.purchases[0].unit_type, UnitType::Bcopters);
    }

    #[test]
    fn schedule_compaction_does_not_steal_an_unprocessed_original_slot() {
        let mut input = input();
        let facility = GridPosition { x: 0, y: 0 };
        input.production_options = vec![
            ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Bcopters,
                    build_turn: 1,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
            },
            ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Bomber,
                    build_turn: 1,
                    cost: 20_000,
                },
                stats: stats(UnitType::Bomber, 20_000, 6),
                engageable_enemy_indices: vec![0],
            },
        ];
        let original = vec![
            PlannedPurchase {
                facility,
                unit_type: UnitType::Bcopters,
                build_turn: 0,
                cost: 7_500,
            },
            PlannedPurchase {
                facility,
                unit_type: UnitType::Bomber,
                build_turn: 1,
                cost: 20_000,
            },
        ];

        let shifted = left_shift_purchases(&input, &original);
        let slots = shifted
            .iter()
            .map(|purchase| (purchase.facility, purchase.build_turn))
            .collect::<HashSet<_>>();

        assert_eq!(slots.len(), shifted.len());
        assert!(matches!(
            evaluate_fixed_package(&input, &original),
            Err(FixedPackageError::ProductionSlotUnavailable)
        ));
    }

    #[test]
    fn future_enemy_must_be_removed_before_plan_is_feasible() {
        let mut input = input();
        input.enemies.push(EnemyPlanUnit {
            entity: None,
            stats: UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1_000,
                can_capture: true,
                ..UnitStats::mock()
            },
            position: GridPosition { x: 8, y: 0 },
            hp: 100,
            defense_bonus: 0,
            available_turn: 5,
        });
        for option in &mut input.production_options {
            option.engageable_enemy_indices = vec![0, 1];
        }

        let plan = plan_force_package(&input).unwrap();

        assert!(plan.feasible);
        assert!(plan.elimination_turn.is_some_and(|turn| turn >= 5));
        assert_eq!(plan.target_forecasts.len(), 2);
        assert_eq!(
            plan.turn_forecasts
                .iter()
                .find(|forecast| forecast.turn == 5)
                .map(|forecast| forecast.enemy_arrival_hp),
            Some(100)
        );
    }

    #[test]
    fn search_depth_comes_from_facility_turns_not_a_fixed_unit_cap() {
        let mut input = input();
        input
            .damage_chart
            .insert_damage(UnitType::Bcopters, UnitType::Infantry, 100);
        input.production_options.truncate(1);
        input.production_options[0].stats.max_ammo1 = 1;
        input.production_options[0].engageable_enemy_indices = (0..6).collect();
        for build_turn in 1..6 {
            let mut option = input.production_options[0].clone();
            option.purchase.build_turn = build_turn;
            input.production_options.push(option);
        }
        for raw in 8..13 {
            let mut enemy = input.enemies[0].clone();
            enemy.entity = Some(Entity::from_raw(raw));
            input.enemies.push(enemy);
        }
        input.current_funds = 45_000;

        let plan = plan_force_package(&input).unwrap();

        assert!(plan.feasible);
        assert_eq!(plan.purchases.len(), 6);
    }
}
