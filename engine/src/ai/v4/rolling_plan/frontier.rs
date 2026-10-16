//! RollingPlanのfrontier責務。
use super::model::MANY_FACTORY_PROPERTY_STATE_LIMIT;
use super::plan_ranking::{
    current_direct_role_counts, exact_property_plan_better, uses_small_map_opening_rules,
    uses_small_map_tactical_rules,
};
use super::role_multiset::{PropertyFrontierKey, RoleMetadata};
use super::{BTreeSet, ForcePackagePlan, MovementType, RollingPlanInput, SearchState, best_damage};

/// 観測量から `PropertyFrontierKey` を共通ロジックで構築する。
#[allow(clippy::too_many_arguments)]
pub(super) fn build_property_frontier_key(
    input: &RollingPlanInput,
    capture_count: usize,
    mut capture_completion_turns: Vec<u32>,
    combat_unit_count: usize,
    total_movement: u32,
    damage_by_enemy: &[u32],
    total_cost: u32,
    identity_indices: Vec<usize>,
) -> PropertyFrontierKey {
    let opening_capture_floor = if uses_small_map_opening_rules(input) {
        2
    } else {
        0
    };
    let required_capturers = input.required_capture_survivors.max(opening_capture_floor);

    let capture_shortfall = required_capturers.saturating_sub(capture_count);
    capture_completion_turns.sort_unstable();
    capture_completion_turns.truncate(required_capturers.max(1));
    capture_completion_turns.resize(required_capturers.max(1), u32::MAX);

    let small_map_capture_completion = if uses_small_map_tactical_rules(input) {
        capture_completion_turns.clone()
    } else {
        Vec::new()
    };

    let (combat_unit_count_desc, total_movement_desc) = if uses_small_map_tactical_rules(input) {
        (
            std::cmp::Reverse(combat_unit_count),
            std::cmp::Reverse(total_movement),
        )
    } else {
        (std::cmp::Reverse(0), std::cmp::Reverse(0))
    };

    let remaining_by_enemy: Vec<u32> = input
        .enemies
        .iter()
        .enumerate()
        .map(|(index, enemy)| enemy.hp.saturating_sub(damage_by_enemy[index]))
        .collect();

    let relevant_enemy_indices: Vec<usize> = if input.interdiction_deadlines.is_empty() {
        (0..input.enemies.len()).collect()
    } else {
        input
            .interdiction_deadlines
            .iter()
            .map(|(index, _)| *index)
            .collect()
    };

    let remaining_relevant_hp: u32 = relevant_enemy_indices
        .into_iter()
        .filter_map(|index| remaining_by_enemy.get(index))
        .copied()
        .sum();
    let remaining_all_hp: u32 = remaining_by_enemy.into_iter().sum();

    PropertyFrontierKey {
        capture_shortfall,
        small_map_capture_completion,
        combat_unit_count_desc,
        total_movement_desc,
        remaining_relevant_hp,
        remaining_all_hp,
        capture_completion_turns,
        excess_capturers: capture_count.saturating_sub(required_capturers),
        cost: total_cost,
        identity_indices,
    }
}

/// 【一時的な診断専用】二つの候補キーが最初に食い違う成分名を返す。
/// 比較順は `PropertyFrontierKey` の宣言順（＝派生Ordの評価順）と同一。
pub(super) fn first_differing_frontier_field(
    left: &PropertyFrontierKey,
    right: &PropertyFrontierKey,
) -> &'static str {
    if left.capture_shortfall != right.capture_shortfall {
        "capture_shortfall"
    } else if left.small_map_capture_completion != right.small_map_capture_completion {
        "small_map_capture_completion"
    } else if left.combat_unit_count_desc != right.combat_unit_count_desc {
        "combat_unit_count_desc"
    } else if left.total_movement_desc != right.total_movement_desc {
        "total_movement_desc"
    } else if left.remaining_relevant_hp != right.remaining_relevant_hp {
        "remaining_relevant_hp"
    } else if left.remaining_all_hp != right.remaining_all_hp {
        "remaining_all_hp"
    } else if left.capture_completion_turns != right.capture_completion_turns {
        "capture_completion_turns"
    } else if left.excess_capturers != right.excess_capturers {
        "excess_capturers"
    } else if left.cost != right.cost {
        "cost"
    } else if left.identity_indices != right.identity_indices {
        "identity_indices"
    } else {
        "none"
    }
}

pub(super) fn multiset_frontier_key(
    input: &RollingPlanInput,
    role_indices: &[usize],
    role_metas: &[RoleMetadata],
) -> PropertyFrontierKey {
    let mut capture_count = 0_usize;
    let mut capture_completion_turns = Vec::new();
    let mut combat_unit_count = 0_usize;
    let mut total_movement = 0_u32;
    let mut total_cost = 0_u32;
    let mut damage_by_enemy = vec![0_u32; input.enemies.len()];

    for &idx in role_indices {
        let meta = &role_metas[idx];
        total_cost = total_cost.saturating_add(meta.cost);
        total_movement = total_movement.saturating_add(meta.max_movement);

        if meta.capture_target.is_some() {
            capture_count += 1;
            if meta.min_capture_completion < u32::MAX {
                capture_completion_turns.push(meta.min_capture_completion);
            }
        } else {
            if meta.is_combat_non_infantry {
                combat_unit_count += 1;
            }
            for (enemy_idx, &dmg) in meta.damage_by_enemy.iter().enumerate() {
                damage_by_enemy[enemy_idx] = damage_by_enemy[enemy_idx].saturating_add(dmg);
            }
        }
    }

    build_property_frontier_key(
        input,
        capture_count,
        capture_completion_turns,
        combat_unit_count,
        total_movement,
        &damage_by_enemy,
        total_cost,
        role_indices.to_vec(),
    )
}

/// 全列挙できない物件制御だけで使う、現在盤面に基づく候補前線。
///
/// この段階は最終評価を代替しない。占領数の不足、各敵へ届く実ダメージ、占領完了ETA
/// が劣る中間案から先に落とし、残った候補だけを従来どおり完全な戦闘シミュレーションで
/// 比較する。従って小規模で候補数が上限未満の盤面（map_1を含む）は一切変わらない。
pub(super) fn retain_property_control_frontier(
    input: &RollingPlanInput,
    states: &mut Vec<SearchState>,
    state_limit: usize,
) {
    states.sort_unstable_by_key(|state| property_control_frontier_key(input, state));
    if states.len() <= state_limit {
        return;
    }

    // 上位256件をそのまま切ると、安価な直接火力だけが残り、物件到着前に敵歩兵を
    // 止められる快速screenのような別作戦が消える。まず盤面から得た到達profileごとに
    // 最良の一案を残し、その後だけ同profile内の順位で予算を埋める。
    let tactical_profiles = states
        .iter()
        .map(|state| property_control_tactical_profile(input, state))
        .collect::<BTreeSet<_>>();
    if tactical_profiles.len() > state_limit {
        // profile自体が予算を超える極端な盤面では、全profileを実simできない。
        // この場合だけ従来の決定順を使う。通常の多工場初手ではprofile数は十分小さい。
        states.truncate(state_limit);
        return;
    }

    let mut retained_profiles = BTreeSet::new();
    let mut retained_states = BTreeSet::new();
    let mut retained = Vec::with_capacity(state_limit);
    for state in states.iter() {
        let profile = property_control_tactical_profile(input, state);
        if retained_profiles.insert(profile) {
            retained_states.insert(state.option_indices.clone());
            retained.push(state.clone());
        }
    }
    for state in states.iter() {
        if retained.len() == state_limit {
            break;
        }
        let profile = property_control_tactical_profile(input, state);
        // 同profileの追加案も最終simに渡すが、代表案を先に確保した後に限る。
        if retained_profiles.contains(&profile)
            && retained_states.insert(state.option_indices.clone())
        {
            retained.push(state.clone());
        }
    }
    *states = retained;
}

/// 物件到達と敵歩兵への初撃を、候補削減前に失わないための戦術profile。
///
/// 固定の兵種評価ではなく、各候補が各観測敵へ何turn目に何ダメージを届けられるかを
/// 用いる。同じprofile内では従来の物件・損耗比較が代表を選ぶので、profileは最終評価を
/// 置き換えない。
pub(super) fn property_control_tactical_profile(
    input: &RollingPlanInput,
    state: &SearchState,
) -> (Vec<u32>, usize, Vec<(u32, u32)>) {
    let mut capture_completion_turns = state
        .option_indices
        .iter()
        .filter_map(|index| input.production_options[*index].capture_completion_turn)
        .collect::<Vec<_>>();
    capture_completion_turns.sort_unstable();
    let mut enemy_profiles = Vec::with_capacity(input.enemies.len());
    for (enemy_index, enemy) in input.enemies.iter().enumerate() {
        let mut earliest_turn = u32::MAX;
        let mut damage_at_earliest = 0_u32;
        for option_index in &state.option_indices {
            let option = &input.production_options[*option_index];
            let ready_turn = input
                .production_attack_projections
                .get(*option_index)
                .and_then(|projections| projections.get(enemy_index))
                .and_then(|projection| *projection)
                .map_or_else(
                    || {
                        option
                            .engageable_enemy_indices
                            .contains(&enemy_index)
                            .then_some(0)
                    },
                    |projection| Some(projection.ready_turn),
                );
            let Some(ready_turn) = ready_turn else {
                continue;
            };
            let damage = best_damage(
                &input.damage_chart,
                option.stats.unit_type,
                enemy.stats.unit_type,
            );
            if ready_turn < earliest_turn {
                earliest_turn = ready_turn;
                damage_at_earliest = damage;
            } else if ready_turn == earliest_turn {
                damage_at_earliest = damage_at_earliest.saturating_add(damage);
            }
        }
        enemy_profiles.push((earliest_turn, damage_at_earliest));
    }
    (
        capture_completion_turns,
        state.used_capture_targets.len(),
        enemy_profiles,
    )
}

/// 多数工場のマルチセット組合せが極端に膨張した場合だけ、候補前線へ縮約する安全弁。
///
/// 施設の順列爆発を排除したため通常の盤面では上限に達しない。特定mapや施設数への依存を排除。
pub(super) fn property_control_state_limit(_facility_count: usize) -> usize {
    MANY_FACTORY_PROPERTY_STATE_LIMIT
}

/// 現在手番の中間生産列を、物件契約と敵編成に対する到達性だけで順序付ける。
///
/// まだ戦闘順・反撃・渋滞を確定しないため、その評価は最終シミュレーションに残す。
/// ここで見るダメージは「どの敵へ何も届かない候補か」を判別する下限であり、
/// 高火力unitを固定的に優遇する評価値ではない。
pub(super) fn property_control_frontier_key(
    input: &RollingPlanInput,
    state: &SearchState,
) -> PropertyFrontierKey {
    let capture_count = state.used_capture_targets.len();
    let capture_completion_turns = state
        .option_indices
        .iter()
        .filter_map(|index| input.production_options[*index].capture_completion_turn)
        .collect::<Vec<_>>();

    let mut damage_by_enemy = vec![0_u32; input.enemies.len()];
    let mut combat_unit_count = 0_usize;
    let mut total_movement = 0_u32;

    for &option_index in &state.option_indices {
        let option = &input.production_options[option_index];
        total_movement = total_movement.saturating_add(option.stats.max_movement);
        if option.capture_target.is_none() && option.stats.movement_type != MovementType::Infantry {
            combat_unit_count += 1;
        }

        for (enemy_index, enemy) in input.enemies.iter().enumerate() {
            let reaches_enemy = input
                .production_attack_projections
                .get(option_index)
                .map_or_else(
                    || option.engageable_enemy_indices.contains(&enemy_index),
                    |projections| projections.get(enemy_index).is_some_and(Option::is_some),
                );
            if reaches_enemy {
                damage_by_enemy[enemy_index] =
                    damage_by_enemy[enemy_index].saturating_add(best_damage(
                        &input.damage_chart,
                        option.stats.unit_type,
                        enemy.stats.unit_type,
                    ));
            }
        }
    }

    build_property_frontier_key(
        input,
        capture_count,
        capture_completion_turns,
        combat_unit_count,
        total_movement,
        &damage_by_enemy,
        state.cost,
        state.option_indices.clone(),
    )
}

/// 既にシミュレーション済みの候補から、物件計画の通常比較で最良の一案を選ぶ。
pub(super) fn select_best_property_plan<'a>(
    input: &RollingPlanInput,
    candidates: impl Iterator<Item = &'a ForcePackagePlan>,
) -> Option<ForcePackagePlan> {
    candidates.cloned().reduce(|selected, candidate| {
        if exact_property_plan_better(input, &candidate, &selected) {
            candidate
        } else {
            selected
        }
    })
}

/// 直接案を壊さない間接案の中から、シミュレーション比較で最良の一案を選ぶ。
/// 同値の場合だけ、実際に直接射点が密な側を優先する。密度だけで候補を捨てない。
pub(super) fn select_best_indirect_upgrade<'a>(
    input: &RollingPlanInput,
    candidates: impl Iterator<Item = &'a ForcePackagePlan>,
) -> Option<ForcePackagePlan> {
    candidates.cloned().reduce(|selected, candidate| {
        if exact_property_plan_better(input, &candidate, &selected) {
            candidate
        } else if exact_property_plan_better(input, &selected, &candidate) {
            selected
        } else if candidate.indirect_fire_has_local_support(input)
            && !selected.indirect_fire_has_local_support(input)
        {
            candidate
        } else {
            selected
        }
    })
}

/// 間接部隊を足した案が、直接部隊だけの基準作戦を崩していないか。
///
/// 物件契約、突破口、生産口、占領役、または敵別のユニークな直接射点を一つでも
/// 悪化させる案は火力が高くても採らない。直接unitの頭数ではなく射点を比較するため、
/// 同じ地点へ向かう余剰unitだけを間接火力へ置き換えられる。
pub(super) fn indirect_upgrade_preserves_direct_plan(
    input: &RollingPlanInput,
    candidate: &ForcePackagePlan,
    direct_plan: &ForcePackagePlan,
) -> bool {
    let (candidate_capture_count, candidate_direct_count) =
        current_direct_role_counts(input, candidate);
    let (direct_capture_count, direct_direct_count) =
        current_direct_role_counts(input, direct_plan);
    let preserves_current_direct_roles = if uses_small_map_tactical_rules(input) {
        // 小規模mapでは、前線の壁役（直接射点）が完全に消滅しない限り、
        // 狭隘地形（橋など）の後方から撃てる間接部隊への置き換えを認める。
        candidate.direct_fire_lane_count > 0 || direct_plan.direct_fire_lane_count == 0
    } else {
        // 標準mapは既存の汎用編成比較をそのまま残す。
        candidate_direct_count >= direct_direct_count
    };
    candidate.cleared_capture_lane_count() >= direct_plan.cleared_capture_lane_count()
        && candidate_capture_count >= direct_capture_count
        && preserves_current_direct_roles
        && candidate.property_contract_completion_profile()
            <= direct_plan.property_contract_completion_profile()
        && candidate.deadline_capture_survivor_count >= direct_plan.deadline_capture_survivor_count
        && candidate.capture_completion_profile() <= direct_plan.capture_completion_profile()
        && candidate.front_breakthrough_rank() <= direct_plan.front_breakthrough_rank()
        && candidate.blocked_next_turn_production_slots
            <= direct_plan.blocked_next_turn_production_slots
        && (!uses_small_map_tactical_rules(input)
            || (candidate.indirect_fire_has_marginal_target()
                || candidate.target_elimination_profile()
                    < direct_plan.target_elimination_profile()))
        && (candidate.remaining_enemy_value(input) < direct_plan.remaining_enemy_value(input)
            || candidate.expected_loss < direct_plan.expected_loss)
}
