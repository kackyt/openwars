//! RollingPlanのforce_package責務。
use super::combat_capture_simulation::simulate_state_with_catalog;
use super::exact_property_control::plan_current_property_control_exact;
use super::plan_ranking::{
    production_option_dominates, update_best_effort_interdiction,
    update_best_sufficient_interdiction, uses_small_map_tactical_rules,
};
use super::{
    DEFAULT_SEARCH_TURNS, ForcePackagePlan, FriendlyAttackProfile, RollingPlanInput,
    SEARCH_BEAM_WIDTH, SearchState, SimEnemy, SimFriendly, SimulationCatalog,
    calculate_damage_formula, evaluate_fixed_package, fill_best_effort_current_screen,
    group_production_options_by_slot, left_shift_purchases, update_best_effort,
};

/// 現在観測した敵を排除できる混成パッケージを探索する。
pub(crate) fn plan_force_package(input: &RollingPlanInput) -> Option<ForcePackagePlan> {
    let catalog = SimulationCatalog::new(input);
    if input.enemies.is_empty() {
        let search_turns = input
            .hard_deadline
            .or(input.capture_completion_turn)
            .unwrap_or(1)
            .max(1);
        let mut plan =
            simulate_state_with_catalog(input, &catalog, &SearchState::default(), search_turns);
        plan.candidates_considered = 1;
        return Some(plan);
    }

    let search_turns = input.hard_deadline.unwrap_or(DEFAULT_SEARCH_TURNS).max(1);
    if input.production_options.is_empty() {
        // 空き生産枠が無い手番でも、既存編成だけで任務を継続できるかは評価する。
        // ここでNoneにすると永続Planの実行・予実監視まで途切れてしまう。
        let mut plan =
            simulate_state_with_catalog(input, &catalog, &SearchState::default(), search_turns);
        plan.candidates_considered = 1;
        return Some(plan);
    }
    if input.exact_property_control {
        return plan_current_property_control_exact(input, &catalog, search_turns);
    }
    let mut frontier = vec![SearchState::default()];
    // 全中間案の敵別forecastを保持すると長期戦でメモリと比較時間が膨張する。
    // 最終選択に必要な3案だけを逐次更新し、各候補のsimulateも1回に限定する。
    let mut best_within_deadline: Option<ForcePackagePlan> = None;
    let mut best_feasible: Option<ForcePackagePlan> = None;
    let mut best_effort: Option<ForcePackagePlan> = None;
    let mut considered = 0_usize;
    let mut truncated = false;

    // 実在するfacility-turnを1枠ずつ処理する。同じ枠の全兵種を深さごとに
    // 再展開すると、首都攻略のような長い購入列で同じ組合せを大量に作る。
    // 「作らない」または「この枠で1兵種を作る」を一度だけ分岐すれば、探索対象を
    // 減らさずにゲームルールの1施設1生産へ一致させられる。
    let mut options_by_slot = group_production_options_by_slot(input);
    let mut candidates_pruned = 0_usize;
    for option_indices in options_by_slot.values_mut() {
        let original_len = option_indices.len();
        let retained = option_indices
            .iter()
            .copied()
            .filter(|candidate_index| {
                !option_indices.iter().copied().any(|other_index| {
                    other_index != *candidate_index
                        && production_option_dominates(
                            input,
                            &input.production_options[other_index],
                            &input.production_options[*candidate_index],
                        )
                })
            })
            .collect::<Vec<_>>();
        candidates_pruned = candidates_pruned.saturating_add(original_len - retained.len());
        *option_indices = retained;
    }
    let mut slots = options_by_slot.into_iter().collect::<Vec<_>>();
    slots.sort_unstable_by_key(|(slot, _)| (slot.build_turn, slot.facility.y, slot.facility.x));

    for (slot, option_indices) in slots {
        let mut next = Vec::new();
        for state in &frontier {
            // この枠を使わない案も残す。高額兵種のための現金予約や、将来の別施設を
            // 選ぶ案を、安い現在購入で強制的に上書きしないためである。
            next.push(state.clone());
            for option_index in &option_indices {
                let option = &input.production_options[*option_index];
                if option
                    .capture_target
                    .is_some_and(|target| state.used_capture_targets.contains(&target))
                {
                    continue;
                }
                let Some(mut child) = state.with_affordable_purchase(input, *option_index, slot)
                else {
                    continue;
                };
                if let Some(target) = option.capture_target {
                    child.used_capture_targets.insert(target);
                }
                next.push(child);
            }
        }

        // 同じbeam層の各状態はsnapshotだけを読む独立計算である。nativeでは並列評価し、
        // 結果は入力順へ戻してから従来どおり最良案を逐次更新するため、同点時の選択は不変。
        let evaluated = crate::ai::deterministic_parallel::map_ordered(next, |state| {
            let plan = simulate_state_with_catalog(input, &catalog, &state, search_turns);
            (state, plan)
        });
        let mut evaluated_next = Vec::with_capacity(evaluated.len());
        for (state, mut plan) in evaluated {
            considered = considered.saturating_add(1);
            plan.candidates_considered = considered;
            if plan.feasible {
                update_best_feasible(input, &mut best_feasible, &plan, input.delay_cost_per_turn);
                if !input.prioritize_deadline_interdiction
                    && input.hard_deadline.is_some_and(|deadline| {
                        plan.elimination_turn.is_some_and(|turn| turn <= deadline)
                    })
                {
                    update_best_feasible(
                        input,
                        &mut best_within_deadline,
                        &plan,
                        input.delay_cost_per_turn,
                    );
                }
            }
            // Interdictの成立条件は敵全滅ではなく、敵の占領完了より前に合法な
            // 初撃を入れること。短い期限内に全滅まで要求して成立案を捨てない。
            if input.prioritize_deadline_interdiction
                && plan.interdiction_contracts_satisfied(&input.interdiction_deadlines)
            {
                update_best_sufficient_interdiction(
                    &mut best_within_deadline,
                    &plan,
                    input.deadline_target_index,
                );
            }
            if input.prioritize_deadline_interdiction {
                update_best_effort_interdiction(
                    &mut best_effort,
                    &plan,
                    input.deadline_target_index,
                );
            } else {
                update_best_effort(&mut best_effort, &plan);
            }
            evaluated_next.push((state, plan));
        }
        let beam_width = input.search_beam_width.clamp(1, SEARCH_BEAM_WIDTH);
        if evaluated_next.len() > beam_width {
            truncated = true;
            if input.prioritize_deadline_interdiction {
                // 物件レースでは高速・安価な妨害を枝刈りで消さない。総ダメージより
                // 早い最初の一撃と安い編成を優先して beam を残す。
                evaluated_next.sort_by_key(|(_, plan)| {
                    (
                        plan.missed_interdiction_contracts(&input.interdiction_deadlines),
                        plan.interdiction_first_attack_turn().unwrap_or(u32::MAX),
                        plan.production_cost,
                        plan.expected_loss,
                        plan.interdiction_remaining_hp(input.deadline_target_index),
                    )
                });
            } else if uses_small_map_tactical_rules(input) {
                // 小規模マップでは、安価な歩兵で後から囲む案より、敵が前線へ侵入する前に
                // 先制攻撃できる案を枝刈りで残す。
                evaluated_next.sort_by_key(|(_, plan)| {
                    (
                        plan.first_attack_turn.unwrap_or(u32::MAX),
                        plan.remaining_hp(),
                        plan.expected_loss,
                        plan.completion_for_ordering(),
                        plan.production_cost,
                    )
                });
            } else {
                evaluated_next.sort_by_key(|(_, plan)| {
                    (
                        plan.remaining_hp(),
                        plan.expected_loss,
                        plan.completion_for_ordering(),
                        plan.production_cost,
                    )
                });
            }
            evaluated_next.truncate(beam_width);
        }
        frontier = evaluated_next.into_iter().map(|(state, _)| state).collect();
        if frontier.is_empty() {
            break;
        }
    }

    let mut selected = best_within_deadline
        .or(best_feasible)
        // 期限内に全滅できる案が無くても、何も作らず停止しない。
        .or(best_effort)?;
    if !selected.feasible {
        // 実行不能な将来列のために現在の空き施設と現金を寝かせない。今出せる
        // 直接戦闘要員を前線screenとして加え、資金が競合する最も遅い予定から
        // 次revisionへ送り返す。実行可能案の高価な必須counter予約には触れない。
        selected = fill_best_effort_current_screen(input, &catalog, selected, search_turns);
    }
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
    selected.candidates_pruned = candidates_pruned;
    selected.search_truncated = truncated;
    Some(selected)
}

pub(super) fn update_best_feasible(
    input: &RollingPlanInput,
    best: &mut Option<ForcePackagePlan>,
    candidate: &ForcePackagePlan,
    delay_cost_per_turn: u32,
) {
    let small_map_first_attack = if uses_small_map_tactical_rules(input) {
        candidate.first_attack_turn.unwrap_or(u32::MAX)
    } else {
        0
    };
    let candidate_key = (
        u8::from(!candidate.overmatch_ready),
        small_map_first_attack,
        candidate.utility_cost(delay_cost_per_turn),
        candidate.completion_for_ordering(),
        candidate.production_cost,
    );
    if best.as_ref().is_none_or(|current| {
        let current_first_attack = if uses_small_map_tactical_rules(input) {
            current.first_attack_turn.unwrap_or(u32::MAX)
        } else {
            0
        };
        candidate_key
            < (
                u8::from(!current.overmatch_ready),
                current_first_attack,
                current.utility_cost(delay_cost_per_turn),
                current.completion_for_ordering(),
                current.production_cost,
            )
    }) {
        *best = Some(candidate.clone());
    }
}

pub(super) fn select_target(
    friendly: &SimFriendly,
    enemies: &[SimEnemy],
    turn: u32,
    interdiction_deadlines: &[(usize, u32)],
    target_first_attack_turns: &[Option<u32>],
    lethal_attacker_counts: &[usize],
) -> Option<(usize, FriendlyAttackProfile)> {
    enemies
        .iter()
        .enumerate()
        .filter(|(_, enemy)| enemy.hp > 0)
        .filter(|(_, enemy)| turn >= enemy.source.available_turn)
        .filter_map(|(index, enemy)| {
            let profile = friendly.attack_profiles[index]?;
            if turn < profile.ready_turn {
                return None;
            }
            let strategic_rank = if enemy.source.stats.can_capture {
                0
            } else if enemy.source.stats.max_cargo > 0 {
                1
            } else {
                2
            };
            let contract_deadline = interdiction_deadlines
                .iter()
                .find_map(|(target_index, deadline)| (*target_index == index).then_some(*deadline));
            let contract_rank = match contract_deadline {
                Some(_)
                    if target_first_attack_turns
                        .get(index)
                        .is_some_and(Option::is_none) =>
                {
                    0
                }
                Some(_) => 1,
                None => 2,
            };
            let damage = calculate_damage_formula(
                profile.base_damage,
                friendly.hp,
                enemy.source.defense_bonus,
                false,
            );
            let lethal = damage >= enemy.hp;
            let lethal_scarcity = if lethal {
                lethal_attacker_counts
                    .get(index)
                    .copied()
                    .unwrap_or(1)
                    .max(1)
            } else {
                usize::MAX
            };
            // 占領能力だけで標的順を固定すると、後方の砲兵を放置したまま前衛へ
            // 損耗を重ねる。まず今この一撃で除去でき、代替担当が少ない敵を選ぶ。
            // 同程度なら自分へ返せる最大与ダメージと勝利条件への影響で決める。
            Some((
                (
                    contract_rank,
                    contract_deadline.unwrap_or(u32::MAX),
                    !lethal,
                    lethal_scarcity,
                    std::cmp::Reverse(profile.incoming_damage),
                    strategic_rank,
                    enemy.hp,
                    index,
                ),
                profile,
            ))
        })
        .min_by_key(|(key, _)| *key)
        .map(|((_, _, _, _, _, _, _, index), profile)| (index, profile))
}
