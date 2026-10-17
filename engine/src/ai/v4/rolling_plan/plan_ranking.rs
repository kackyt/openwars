//! RollingPlanのplan_ranking責務。
use super::model::{OpeningProductionPolicy, PlannedCombatPurchase};
use super::{
    ForcePackagePlan, GridPosition, HashSet, MovementType, ProductionPlanOption, ProductionSlot,
    RollingPlanInput, SearchState, SimEnemy, SimFriendly, best_damage, calculate_damage_formula,
};

impl ForcePackagePlan {
    /// 各敵の撃破予測を、未撃破を無限大として比較可能な形にする。
    pub(super) fn target_elimination_profile(&self) -> Vec<u32> {
        self.target_forecasts
            .iter()
            .map(|target| target.destroyed_turn.unwrap_or(u32::MAX))
            .collect()
    }

    /// 今回買う間接部隊が、既存の間接部隊と直接部隊だけでは倒せない敵へ
    /// 実ダメージを足せているか。射撃対象の重複を「unit数」ではなく敵HPで判定する。
    pub(super) fn indirect_fire_has_marginal_target(&self) -> bool {
        self.target_forecasts
            .iter()
            .enumerate()
            .any(|(index, target)| {
                let produced = self
                    .produced_indirect_damage_by_target
                    .get(index)
                    .copied()
                    .unwrap_or_default();
                let prior_damage = self
                    .direct_damage_by_target
                    .get(index)
                    .copied()
                    .unwrap_or_default()
                    .saturating_add(
                        self.existing_indirect_damage_by_target
                            .get(index)
                            .copied()
                            .unwrap_or_default(),
                    );
                produced > 0 && prior_damage < target.initial_hp
            })
    }
}

/// 今手番に発注する占領役と、占領を担当しない直接戦闘役の本数。
/// 標準mapでは従来通りこの二つを下限にし、小規模mapだけ射点比較へ切り替える。
pub(super) fn current_direct_role_counts(
    input: &RollingPlanInput,
    plan: &ForcePackagePlan,
) -> (usize, usize) {
    plan.current_purchases()
        .fold((0, 0), |(capture, direct), purchase| {
            let Some(option) = input
                .production_options
                .iter()
                .find(|option| option.purchase == purchase)
            else {
                return (capture, direct);
            };
            if option.capture_target.is_some() {
                (capture.saturating_add(1), direct)
            } else if option.stats.min_range <= 1 {
                (capture, direct.saturating_add(1))
            } else {
                (capture, direct)
            }
        })
}

/// 今手番に生産する直接部隊が、実シミュレーションで担当した敵ごとのユニークな射点数。
/// 同じ敵・同じ射点へ何体も向かう案は一つと数える。候補ごとのsimulateの終端で一度だけ
/// 計算し、候補比較中には再計算しない。
pub(super) fn direct_fire_lane_count(
    input: &RollingPlanInput,
    combat_purchases: &[PlannedCombatPurchase],
) -> usize {
    combat_purchases
        .iter()
        .filter_map(|assignment| {
            let option = input
                .production_options
                .iter()
                .find(|option| option.purchase == assignment.purchase)?;
            if assignment.purchase.build_turn != 0 || option.stats.min_range > 1 {
                return None;
            }
            let enemy_index = input
                .enemies
                .iter()
                .position(|enemy| enemy.entity == Some(assignment.target))?;
            let projection = input
                .production_attack_projections
                .get(
                    input
                        .production_options
                        .iter()
                        .position(|candidate| candidate.purchase == assignment.purchase)?,
                )?
                .get(enemy_index)
                .and_then(|projection| *projection)?;
            Some((enemy_index, projection.firing_position))
        })
        .collect::<HashSet<_>>()
        .len()
}

/// 直接部隊だけで敵直接部隊をどこまで駆逐できるかを、実際の相性・到着turn・弾数から
/// 割り当てる。砲を足した候補で直接部隊の攻撃先が変わっても、直接戦線を維持する能力まで
/// 失ったことにはしないため、通常simulationの実行結果とは別に不変の能力として計算する。
pub(super) fn direct_enemy_remaining_hp_after_assignment(
    friendlies: &[SimFriendly],
    enemies: &[SimEnemy],
    search_turns: u32,
) -> Vec<u32> {
    let mut remaining_hp = enemies
        .iter()
        .map(|enemy| {
            if enemy.source.stats.min_range <= 1 && enemy.source.available_turn <= search_turns {
                enemy.source.hp
            } else {
                0
            }
        })
        .collect::<Vec<_>>();
    // unitごとの攻撃回数を、弾数と実際に攻撃可能なturn数の小さい方に制限する。
    // これで、同じ戦車一両を複数の敵へ無限に割り当てる過大評価を避ける。
    let mut remaining_shots = friendlies
        .iter()
        .map(|friendly| {
            if friendly.stats.min_range > 1 || friendly.available_turn > search_turns {
                0
            } else {
                friendly.attacks_left.min(
                    search_turns
                        .saturating_sub(friendly.available_turn)
                        .saturating_add(1),
                )
            }
        })
        .collect::<Vec<_>>();
    let mut target_order = remaining_hp
        .iter()
        .enumerate()
        .filter(|(_, hp)| **hp > 0)
        .map(|(enemy_index, hp)| {
            let eligible = friendlies
                .iter()
                .filter(|friendly| friendly.stats.min_range <= 1)
                .filter(|friendly| {
                    friendly
                        .attack_profiles
                        .get(enemy_index)
                        .copied()
                        .flatten()
                        .is_some_and(|profile| profile.ready_turn <= search_turns)
                })
                .count();
            (eligible, std::cmp::Reverse(*hp), enemy_index)
        })
        .collect::<Vec<_>>();
    // 対応できる直接兵種が少ない敵から先に担当を確保する。これにより歩兵・装甲車・
    // 戦車の相性差を固定値にせず、実damage表が必要とする担当を残せる。
    target_order.sort_unstable();
    for (_, _, enemy_index) in target_order {
        while remaining_hp[enemy_index] > 0 {
            let best = friendlies
                .iter()
                .enumerate()
                .filter(|(friendly_index, friendly)| {
                    remaining_shots[*friendly_index] > 0 && friendly.stats.min_range <= 1
                })
                .filter_map(|(friendly_index, friendly)| {
                    let profile = friendly
                        .attack_profiles
                        .get(enemy_index)
                        .copied()
                        .flatten()?;
                    (profile.ready_turn <= search_turns).then(|| {
                        let damage = calculate_damage_formula(
                            profile.base_damage,
                            friendly.hp,
                            enemies[enemy_index].source.defense_bonus,
                            false,
                        );
                        (damage > 0).then_some((
                            damage,
                            std::cmp::Reverse(profile.ready_turn),
                            std::cmp::Reverse(friendly_index),
                        ))
                    })?
                })
                .max();
            let Some((damage, _, std::cmp::Reverse(friendly_index))) = best else {
                break;
            };
            remaining_shots[friendly_index] = remaining_shots[friendly_index].saturating_sub(1);
            remaining_hp[enemy_index] = remaining_hp[enemy_index].saturating_sub(damage);
        }
    }
    remaining_hp
}

/// 同じ敵へ向かう直接部隊の射点が飽和し、間接部隊が工場外から別の攻撃地点を
/// 足せるかを判定する。
///
/// 戦力額や兵種ごとの固定点は使わない。既存・今回生産の直接部隊が、間接部隊の
/// 初撃までに実際に立てる射点だけを数える。隣接射点をすべて直接部隊で埋める必要は
/// なく、「空きが一つ以下」なら新たな直接部隊より外側から撃つ間接火力の限界効用が
/// 高い、とゲーム上の攻撃地点の数から判断する。
pub(super) fn direct_fire_saturates_target(
    input: &RollingPlanInput,
    plan: &ForcePackagePlan,
    enemy_index: usize,
    indirect_ready_turn: u32,
) -> bool {
    let Some(enemy) = input.enemies.get(enemy_index) else {
        return false;
    };
    let target = enemy.position;
    let adjacent_capacity = input.map.get_adjacent(target.x, target.y).len();
    if adjacent_capacity == 0 {
        return false;
    }

    let mut direct_positions = input
        .existing_units
        .iter()
        .filter(|unit| {
            unit.stats.min_range <= 1
                && unit.available_turn <= indirect_ready_turn
                && unit.engageable_enemy_indices.contains(&enemy_index)
        })
        .map(|unit| unit.position)
        .filter(|position| {
            input
                .map
                .distance(position.x, position.y, target.x, target.y)
                <= 1
        })
        .collect::<HashSet<_>>();

    for purchase in &plan.purchases {
        let Some(option_index) = input
            .production_options
            .iter()
            .position(|option| option.purchase == *purchase)
        else {
            continue;
        };
        let option = &input.production_options[option_index];
        // 占領役も、物件を塞ぐ敵へ攻撃する間は直接射点を一つ使う。占領契約だからと
        // 数から外すと、中央で実際に起きている渋滞を見落として砲撃が遅くなる。
        if option.stats.min_range > 1 {
            continue;
        }
        let Some(projection) = input
            .production_attack_projections
            .get(option_index)
            .and_then(|projections| projections.get(enemy_index))
            .and_then(|projection| *projection)
        else {
            continue;
        };
        if input.map.distance(
            projection.firing_position.x,
            projection.firing_position.y,
            target.x,
            target.y,
        ) <= 1
        {
            direct_positions.insert(projection.firing_position);
        }
    }

    direct_positions.len().saturating_add(1) >= adjacent_capacity
}

/// 小規模開幕専用の制約を適用する条件を一箇所に固定する。
pub(super) fn uses_small_map_opening_rules(input: &RollingPlanInput) -> bool {
    input.current_turn == 1 && uses_small_map_tactical_rules(input)
}

/// 戦略プロファイルは盤面構造から対局中に一度だけ決まる。初手以外にも効かせる
/// 小規模マップ専用の判断はここへ集約し、標準マップのRolling Planを変えない。
pub(super) fn uses_small_map_tactical_rules(input: &RollingPlanInput) -> bool {
    input.opening_policy == OpeningProductionPolicy::SmallMapExpansion
}

/// 初手の最終編成が、残額で合法に追加できる生産枠を放置していないか判定する。
pub(super) fn opening_package_spends_all_usable_funds(
    input: &RollingPlanInput,
    state: &SearchState,
    facilities: &[(GridPosition, Vec<usize>)],
) -> bool {
    let remaining_funds = input.current_funds.saturating_sub(state.cost);
    let has_combat_unit = state.option_indices.iter().any(|&idx| {
        let opt = &input.production_options[idx];
        opt.capture_target.is_none() && opt.stats.movement_type != MovementType::Infantry
    });
    let capture_count = state.used_capture_targets.len();
    let capture_satisfied = capture_count >= input.required_capture_survivors;

    !facilities.iter().any(|(facility, option_indices)| {
        let slot = ProductionSlot {
            facility: *facility,
            build_turn: 0,
        };
        !state.used_slots.contains(&slot)
            && option_indices.iter().any(|option_index| {
                let option = &input.production_options[*option_index];
                if option.purchase.cost > remaining_funds {
                    return false;
                }
                if let Some(target) = option.capture_target {
                    !state.used_capture_targets.contains(&target)
                } else {
                    // 戦闘ユニット・占領役ともに確保済みなら、端数資金での追加歩兵の買い足しを強制しない
                    !(has_combat_unit
                        && capture_satisfied
                        && option.stats.movement_type == MovementType::Infantry)
                }
            })
    })
}

/// 物件契約の辞書順比較。各契約内の評価順と同点規則を維持する。
pub(super) fn exact_property_plan_better(
    input: &RollingPlanInput,
    candidate: &ForcePackagePlan,
    current: &ForcePackagePlan,
) -> bool {
    if input.interdiction_deadlines.is_empty() {
        return compare_without_deadlines(input, candidate, current);
    }
    let candidate_satisfied = candidate.property_contracts_satisfied(
        &input.interdiction_deadlines,
        input.required_capture_survivors,
    );
    let current_satisfied = current.property_contracts_satisfied(
        &input.interdiction_deadlines,
        input.required_capture_survivors,
    );
    if candidate_satisfied != current_satisfied {
        return candidate_satisfied;
    }
    if candidate_satisfied {
        return compare_satisfied_contracts(input, candidate, current);
    }
    compare_unmet_contracts(input, candidate, current)
}

/// 全滅成立性を優先し、未成立なら掃討レーンと前線維持を比較する。
fn compare_without_deadlines(
    input: &RollingPlanInput,
    candidate: &ForcePackagePlan,
    current: &ForcePackagePlan,
) -> bool {
    if candidate.feasible && current.feasible {
        return compare_feasible_without_deadlines(input, candidate, current);
    }
    if candidate.feasible != current.feasible {
        return candidate.feasible;
    }
    // 全滅未達（敵が多数存在し、突破・前線維持が必要な場合）
    // 掃討と占領が対になった成立レーン数を最優先し、その後に全レーンの掃討時刻、
    // 占領時刻、敵残HP、損耗を比較する。一体目だけの突破で後続を見失わない。
    if uses_small_map_tactical_rules(input) {
        return (
            std::cmp::Reverse(candidate.cleared_capture_lane_count()),
            candidate.property_contract_completion_profile(),
            (
                candidate.front_breakthrough_rank(),
                candidate.target_destruction_profile(),
                candidate.remaining_enemy_value(input),
                std::cmp::Reverse(candidate.surviving_combat_value),
                candidate.blocked_next_turn_production_slots,
                candidate.expected_loss,
                std::cmp::Reverse(candidate.deadline_capture_survivor_count),
                candidate.capture_completion_profile(),
                candidate.remaining_hp(),
                candidate.production_cost,
            ),
        ) < (
            std::cmp::Reverse(current.cleared_capture_lane_count()),
            current.property_contract_completion_profile(),
            (
                current.front_breakthrough_rank(),
                current.target_destruction_profile(),
                current.remaining_enemy_value(input),
                std::cmp::Reverse(current.surviving_combat_value),
                current.blocked_next_turn_production_slots,
                current.expected_loss,
                std::cmp::Reverse(current.deadline_capture_survivor_count),
                current.capture_completion_profile(),
                current.remaining_hp(),
                current.production_cost,
            ),
        );
    }
    (
        std::cmp::Reverse(candidate.cleared_capture_lane_count()),
        candidate.property_contract_completion_profile(),
        (
            candidate.front_breakthrough_rank(),
            candidate.remaining_enemy_value(input),
            candidate.blocked_next_turn_production_slots,
            candidate.expected_loss,
            std::cmp::Reverse(candidate.surviving_combat_value),
            candidate.target_destruction_profile(),
            std::cmp::Reverse(candidate.deadline_capture_survivor_count),
            candidate.capture_completion_profile(),
            candidate.remaining_hp(),
            candidate.production_cost,
        ),
    ) < (
        std::cmp::Reverse(current.cleared_capture_lane_count()),
        current.property_contract_completion_profile(),
        (
            current.front_breakthrough_rank(),
            current.remaining_enemy_value(input),
            current.blocked_next_turn_production_slots,
            current.expected_loss,
            std::cmp::Reverse(current.surviving_combat_value),
            current.target_destruction_profile(),
            std::cmp::Reverse(current.deadline_capture_survivor_count),
            current.capture_completion_profile(),
            current.remaining_hp(),
            current.production_cost,
        ),
    )
}

/// 期限なしの成立案では、初撃・物件達成と経済性の既存優先順を守る。
fn compare_feasible_without_deadlines(
    input: &RollingPlanInput,
    candidate: &ForcePackagePlan,
    current: &ForcePackagePlan,
) -> bool {
    if uses_small_map_tactical_rules(input) {
        // 同じ占領契約を満たすなら、敵占領役へ最初に触れられるturnを先に比べる。
        // これは兵種の固定優先ではなく、実際の射撃位置・移動後攻撃可否から得た
        // first_attack_turnであり、初撃が遅い重歩兵を火力合計だけで選ばないための
        // 順序である。
        let candidate_first_attack = candidate.first_attack_turn.unwrap_or(u32::MAX);
        let current_first_attack = current.first_attack_turn.unwrap_or(u32::MAX);
        if candidate_first_attack != current_first_attack {
            return candidate_first_attack < current_first_attack;
        }
    }
    if !uses_small_map_tactical_rules(input) && input.required_capture_survivors <= 1 {
        // 標準mapでは、同じ物件契約を成立させた後の早過ぎる掃討のために
        // 追加unitを買わない。次手番に盤面を再観測できるため、費用を先に比べる。
        return (
            std::cmp::Reverse(candidate.cleared_capture_lane_count()),
            candidate.property_contract_completion_profile(),
            std::cmp::Reverse(candidate.deadline_capture_survivor_count),
            candidate.capture_completion_profile(),
            candidate.expected_loss,
            candidate.production_cost,
            candidate.target_destruction_profile(),
            candidate.occupation_turn,
            std::cmp::Reverse(candidate.surviving_combat_value),
        ) < (
            std::cmp::Reverse(current.cleared_capture_lane_count()),
            current.property_contract_completion_profile(),
            std::cmp::Reverse(current.deadline_capture_survivor_count),
            current.capture_completion_profile(),
            current.expected_loss,
            current.production_cost,
            current.target_destruction_profile(),
            current.occupation_turn,
            std::cmp::Reverse(current.surviving_combat_value),
        );
    }
    if uses_small_map_tactical_rules(input) {
        // 小規模マップでは、同じ占領契約を満たすなら、前線に展開した戦闘戦力価値
        // (surviving_combat_value) を優先する。ただ最安歩兵で費用を抑えるだけの
        // 鈍足編成を選ばず、快速screenや防衛火力を前線へ確保する。
        return (
            std::cmp::Reverse(candidate.cleared_capture_lane_count()),
            candidate.property_contract_completion_profile(),
            std::cmp::Reverse(candidate.deadline_capture_survivor_count),
            candidate.capture_completion_profile(),
            candidate.target_destruction_profile(),
            candidate.occupation_turn,
            candidate.expected_loss,
            std::cmp::Reverse(candidate.surviving_combat_value),
            candidate.production_cost,
        ) < (
            std::cmp::Reverse(current.cleared_capture_lane_count()),
            current.property_contract_completion_profile(),
            std::cmp::Reverse(current.deadline_capture_survivor_count),
            current.capture_completion_profile(),
            current.target_destruction_profile(),
            current.occupation_turn,
            current.expected_loss,
            std::cmp::Reverse(current.surviving_combat_value),
            current.production_cost,
        );
    }
    (
        std::cmp::Reverse(candidate.cleared_capture_lane_count()),
        candidate.property_contract_completion_profile(),
        std::cmp::Reverse(candidate.deadline_capture_survivor_count),
        candidate.capture_completion_profile(),
        candidate.target_destruction_profile(),
        candidate.occupation_turn,
        candidate.expected_loss,
        candidate.production_cost,
        std::cmp::Reverse(candidate.surviving_combat_value),
    ) < (
        std::cmp::Reverse(current.cleared_capture_lane_count()),
        current.property_contract_completion_profile(),
        std::cmp::Reverse(current.deadline_capture_survivor_count),
        current.capture_completion_profile(),
        current.target_destruction_profile(),
        current.occupation_turn,
        current.expected_loss,
        current.production_cost,
        std::cmp::Reverse(current.surviving_combat_value),
    )
}

/// 期限契約を満たす案同士のレーン・完了時刻・戦闘結果を比較する。
fn compare_satisfied_contracts(
    input: &RollingPlanInput,
    candidate: &ForcePackagePlan,
    current: &ForcePackagePlan,
) -> bool {
    if candidate.cleared_capture_lane_count() != current.cleared_capture_lane_count() {
        return candidate.cleared_capture_lane_count() > current.cleared_capture_lane_count();
    }
    let candidate_joint = candidate.property_contract_completion_profile();
    let current_joint = current.property_contract_completion_profile();
    if candidate_joint != current_joint {
        return candidate_joint < current_joint;
    }
    if candidate.feasible && current.feasible {
        return compare_feasible_satisfied_contracts(input, candidate, current);
    }
    if uses_small_map_tactical_rules(input) {
        return (
            candidate.target_destruction_profile(),
            candidate.remaining_enemy_value(input),
            candidate.blocked_next_turn_production_slots,
            candidate.expected_loss,
            std::cmp::Reverse(candidate.surviving_combat_value),
            candidate
                .interdiction_first_attack_turn()
                .unwrap_or(u32::MAX),
            candidate.production_cost,
        ) < (
            current.target_destruction_profile(),
            current.remaining_enemy_value(input),
            current.blocked_next_turn_production_slots,
            current.expected_loss,
            std::cmp::Reverse(current.surviving_combat_value),
            current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
            current.production_cost,
        );
    }
    // 契約を満たした案同士では、敵のマスター価格に残HPを掛けた実残存価値を減らす。
    // raw HPや購入兵種への固定点ではなく、実盤面の敵編成と相性による撃破結果を使う。
    (
        candidate.remaining_enemy_value(input),
        candidate.blocked_next_turn_production_slots,
        candidate.expected_loss,
        std::cmp::Reverse(candidate.surviving_combat_value),
        candidate.target_destruction_profile(),
        candidate
            .interdiction_first_attack_turn()
            .unwrap_or(u32::MAX),
        candidate.production_cost,
    ) < (
        current.remaining_enemy_value(input),
        current.blocked_next_turn_production_slots,
        current.expected_loss,
        std::cmp::Reverse(current.surviving_combat_value),
        current.target_destruction_profile(),
        current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
        current.production_cost,
    )
}

/// 期限と全滅を満たす案で標準側の余剰投資抑制と小規模側の戦力保持を維持する。
fn compare_feasible_satisfied_contracts(
    input: &RollingPlanInput,
    candidate: &ForcePackagePlan,
    current: &ForcePackagePlan,
) -> bool {
    if !uses_small_map_tactical_rules(input) && input.required_capture_survivors <= 1 {
        // 期限契約を同じく満たせるなら、標準mapは余剰の直接・航空戦力を
        // 増やさず、次手番の観測後に必要なcounterへ資金を残す。
        return (
            candidate.remaining_enemy_value(input),
            candidate.blocked_next_turn_production_slots,
            candidate.expected_loss,
            candidate.production_cost,
            candidate.target_destruction_profile(),
            std::cmp::Reverse(candidate.surviving_combat_value),
            candidate
                .interdiction_first_attack_turn()
                .unwrap_or(u32::MAX),
        ) < (
            current.remaining_enemy_value(input),
            current.blocked_next_turn_production_slots,
            current.expected_loss,
            current.production_cost,
            current.target_destruction_profile(),
            std::cmp::Reverse(current.surviving_combat_value),
            current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
        );
    }
    (
        candidate.remaining_enemy_value(input),
        candidate.blocked_next_turn_production_slots,
        candidate.expected_loss,
        std::cmp::Reverse(candidate.surviving_combat_value),
        candidate.target_destruction_profile(),
        candidate.production_cost,
        candidate
            .interdiction_first_attack_turn()
            .unwrap_or(u32::MAX),
    ) < (
        current.remaining_enemy_value(input),
        current.blocked_next_turn_production_slots,
        current.expected_loss,
        std::cmp::Reverse(current.surviving_combat_value),
        current.target_destruction_profile(),
        current.production_cost,
        current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
    )
}

/// 未成立契約は未達数と期限差から比較し、火力合計で不足を隠さない。
fn compare_unmet_contracts(
    input: &RollingPlanInput,
    candidate: &ForcePackagePlan,
    current: &ForcePackagePlan,
) -> bool {
    let required = input.required_capture_survivors;
    if uses_small_map_tactical_rules(input) {
        return (
            std::cmp::Reverse(candidate.cleared_capture_lane_count()),
            candidate.property_contract_completion_profile(),
            required.saturating_sub(
                candidate.completed_property_contracts(&input.interdiction_deadlines, required),
            ),
            candidate.interdiction_deadline_shortfall_profile(&input.interdiction_deadlines),
            candidate.capture_survivor_shortfall(required),
            (
                candidate.front_breakthrough_rank(),
                candidate.target_destruction_profile(),
                candidate.remaining_enemy_value(input),
                std::cmp::Reverse(candidate.surviving_combat_value),
                candidate.blocked_next_turn_production_slots,
                candidate.expected_loss,
            ),
            (
                candidate.interdiction_attack_turn_profile(&input.interdiction_deadlines),
                candidate.remaining_hp(),
                candidate.production_cost,
            ),
        ) < (
            std::cmp::Reverse(current.cleared_capture_lane_count()),
            current.property_contract_completion_profile(),
            required.saturating_sub(
                current.completed_property_contracts(&input.interdiction_deadlines, required),
            ),
            current.interdiction_deadline_shortfall_profile(&input.interdiction_deadlines),
            current.capture_survivor_shortfall(required),
            (
                current.front_breakthrough_rank(),
                current.target_destruction_profile(),
                current.remaining_enemy_value(input),
                std::cmp::Reverse(current.surviving_combat_value),
                current.blocked_next_turn_production_slots,
                current.expected_loss,
            ),
            (
                current.interdiction_attack_turn_profile(&input.interdiction_deadlines),
                current.remaining_hp(),
                current.production_cost,
            ),
        );
    }
    (
        std::cmp::Reverse(candidate.cleared_capture_lane_count()),
        candidate.property_contract_completion_profile(),
        required.saturating_sub(
            candidate.completed_property_contracts(&input.interdiction_deadlines, required),
        ),
        candidate.interdiction_deadline_shortfall_profile(&input.interdiction_deadlines),
        candidate.capture_survivor_shortfall(required),
        (
            candidate.remaining_enemy_value(input),
            candidate.blocked_next_turn_production_slots,
            candidate.expected_loss,
            std::cmp::Reverse(candidate.surviving_combat_value),
            candidate.front_breakthrough_rank(),
            candidate.target_destruction_profile(),
        ),
        (
            candidate.interdiction_attack_turn_profile(&input.interdiction_deadlines),
            candidate.remaining_hp(),
            candidate.production_cost,
        ),
    ) < (
        std::cmp::Reverse(current.cleared_capture_lane_count()),
        current.property_contract_completion_profile(),
        required.saturating_sub(
            current.completed_property_contracts(&input.interdiction_deadlines, required),
        ),
        current.interdiction_deadline_shortfall_profile(&input.interdiction_deadlines),
        current.capture_survivor_shortfall(required),
        (
            current.remaining_enemy_value(input),
            current.blocked_next_turn_production_slots,
            current.expected_loss,
            std::cmp::Reverse(current.surviving_combat_value),
            current.front_breakthrough_rank(),
            current.target_destruction_profile(),
        ),
        (
            current.interdiction_attack_turn_profile(&input.interdiction_deadlines),
            current.remaining_hp(),
            current.production_cost,
        ),
    )
}

/// 同じ工場・同じ手番の候補で、価格・交戦対象・相性の全てで劣る兵種を落とす。
///
/// ある敵への打点だけが高い候補を消さないよう、右辺が届く全敵へ左辺も届き、
/// 与ダメージは同等以上かつ被ダメージは同等以下の場合だけ支配とみなす。
pub(super) fn production_option_dominates(
    input: &RollingPlanInput,
    left: &ProductionPlanOption,
    right: &ProductionPlanOption,
) -> bool {
    if left.capture_target != right.capture_target
        || left.purchase.cost > right.purchase.cost
        || !right
            .engageable_enemy_indices
            .iter()
            .all(|index| left.engageable_enemy_indices.contains(index))
    {
        return false;
    }
    let matchup_not_worse = right.engageable_enemy_indices.iter().all(|index| {
        let enemy = &input.enemies[*index];
        let left_outgoing = best_damage(
            &input.damage_chart,
            left.stats.unit_type,
            enemy.stats.unit_type,
        );
        let right_outgoing = best_damage(
            &input.damage_chart,
            right.stats.unit_type,
            enemy.stats.unit_type,
        );
        let left_incoming = best_damage(
            &input.damage_chart,
            enemy.stats.unit_type,
            left.stats.unit_type,
        );
        let right_incoming = best_damage(
            &input.damage_chart,
            enemy.stats.unit_type,
            right.stats.unit_type,
        );
        left_outgoing >= right_outgoing && left_incoming <= right_incoming
    });
    let strictly_better = left.purchase.cost < right.purchase.cost
        || left.engageable_enemy_indices.len() > right.engageable_enemy_indices.len()
        || right.engageable_enemy_indices.iter().any(|index| {
            let enemy = &input.enemies[*index];
            best_damage(
                &input.damage_chart,
                left.stats.unit_type,
                enemy.stats.unit_type,
            ) > best_damage(
                &input.damage_chart,
                right.stats.unit_type,
                enemy.stats.unit_type,
            ) || best_damage(
                &input.damage_chart,
                enemy.stats.unit_type,
                left.stats.unit_type,
            ) < best_damage(
                &input.damage_chart,
                enemy.stats.unit_type,
                right.stats.unit_type,
            )
        });
    matchup_not_worse && strictly_better
}

/// 期限付き目標の fallback 比較。
///
/// 全滅が期限内に成立しない場合でも、実際の初撃が早く、同じ初撃時刻なら敵HPを
/// 多く残さない編成を選ぶ。兵種名・移動力・施設順の固定値ではなく、シミュレーション済みの
/// 到達・射程・ダメージだけを比較する。
pub(super) fn update_best_effort_interdiction(
    best: &mut Option<ForcePackagePlan>,
    candidate: &ForcePackagePlan,
    target_index: Option<usize>,
) {
    let candidate_key = (
        candidate
            .interdiction_first_attack_turn()
            .unwrap_or(u32::MAX),
        candidate.interdiction_remaining_hp(target_index),
        candidate.expected_loss,
        std::cmp::Reverse(candidate.surviving_combat_value),
        candidate.production_cost,
    );
    if best.as_ref().is_none_or(|current| {
        candidate_key
            < (
                current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
                current.interdiction_remaining_hp(target_index),
                current.expected_loss,
                std::cmp::Reverse(current.surviving_combat_value),
                current.production_cost,
            )
    }) {
        *best = Some(candidate.clone());
    }
}

/// 期限前に合法な初撃を入れられる案同士の比較。
///
/// 単に最小費用を優先すると安価で脆弱な兵種ばかり選ばれ、前線の戦闘優位が失われる。
/// 初撃達成可能案同士では、残存軍事価値（NPV）の最大化、敵残HP最小化、被ダメージ損失の
/// 最小化を優先し、それらが同等の場合にのみ経済効率（最小費用）を比較する。
pub(super) fn update_best_sufficient_interdiction(
    best: &mut Option<ForcePackagePlan>,
    candidate: &ForcePackagePlan,
    target_index: Option<usize>,
) {
    let candidate_key = (
        candidate
            .interdiction_first_attack_turn()
            .unwrap_or(u32::MAX),
        candidate.interdiction_remaining_hp(target_index),
        candidate.expected_loss,
        std::cmp::Reverse(candidate.surviving_combat_value),
        candidate.production_cost,
    );
    if best.as_ref().is_none_or(|current| {
        candidate_key
            < (
                current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
                current.interdiction_remaining_hp(target_index),
                current.expected_loss,
                std::cmp::Reverse(current.surviving_combat_value),
                current.production_cost,
            )
    }) {
        *best = Some(candidate.clone());
    }
}

/// 現在手番に行動可能な戦闘役を一体へ集中したとき、確実に除去できる前線敵を選ぶ。
///
/// 通常の戦闘シミュレーションは脅威度も考慮して標的を選ぶが、物件レースの第一波は
/// 「どの兵種を何体買えば通路を開けられるか」を先に確定する必要がある。そこで実際の
/// 攻撃可能時刻・残HP・地形防御込みのdamageだけを合算し、撃破可能な敵がある場合に限り
/// 同一標的への集中を成立させる。
pub(super) fn select_front_breakthrough_target(
    friendlies: &[SimFriendly],
    enemies: &[SimEnemy],
    turn: u32,
) -> Option<usize> {
    enemies
        .iter()
        .enumerate()
        .filter(|(_, enemy)| enemy.hp > 0 && turn >= enemy.source.available_turn)
        .filter_map(|(enemy_index, enemy)| {
            let concentrated_damage = friendlies
                .iter()
                .filter(|friendly| {
                    friendly.hp > 0 && friendly.attacks_left > 0 && turn >= friendly.available_turn
                })
                .filter_map(|friendly| {
                    let profile = friendly
                        .attack_profiles
                        .get(enemy_index)
                        .copied()
                        .flatten()?;
                    (turn >= profile.ready_turn).then_some(calculate_damage_formula(
                        profile.base_damage,
                        friendly.hp,
                        enemy.source.defense_bonus,
                        false,
                    ))
                })
                .fold(0_u32, u32::saturating_add);
            (concentrated_damage >= enemy.hp).then_some((
                (
                    !enemy.source.stats.can_capture,
                    enemy.hp,
                    enemy.source.available_turn,
                    enemy_index,
                ),
                enemy_index,
            ))
        })
        .min_by_key(|(key, _)| *key)
        .map(|(_, enemy_index)| enemy_index)
}
