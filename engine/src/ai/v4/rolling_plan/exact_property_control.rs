//! RollingPlanのexact_property_control責務。
use super::combat_capture_simulation::simulate_state_with_catalog;
use super::facility_assignment::optimize_multiset_facility_assignment;
use super::frontier::{
    indirect_upgrade_preserves_direct_plan, property_control_frontier_key,
    property_control_state_limit, retain_property_control_frontier, select_best_indirect_upgrade,
    select_best_property_plan,
};
use super::plan_ranking::{
    opening_package_spends_all_usable_funds, production_option_dominates,
    uses_small_map_opening_rules,
};
use super::role_multiset::{ProductionRole, generate_role_multisets};
use super::search_diagnostics::{report_frontier_witness_simulation, report_indirect_gate_audit};
use super::{
    DEFAULT_SEARCH_TURNS, ForcePackagePlan, GridPosition, HashMap, HashSet, RollingPlanInput,
    SearchState, SimulationCatalog,
};

use super::search_diagnostics::{
    audit_assigned_states, audit_evaluated_plans, audit_evaluated_ranking, audit_exact_input,
    audit_exact_multisets, audit_exact_options, audit_exact_roles, audit_frontier_retain,
    state_has_indirect,
};

struct PreparedFacilities {
    facilities: Vec<(GridPosition, Vec<usize>)>,
    candidates_pruned: usize,
    state_limit: usize,
    funnel_audit: bool,
    funnel_pre_prune: Option<HashMap<GridPosition, Vec<usize>>>,
}
struct RoleCatalog {
    facility_role_options: HashMap<(GridPosition, ProductionRole), usize>,
    distinct_roles: Vec<ProductionRole>,
}
struct ReducedFrontier {
    states: Vec<SearchState>,
    truncated: bool,
    witnesses: Vec<SearchState>,
}

/// 同じ候補順・枝刈り・比較を、意味のある探索段として適用する。
pub(super) fn plan_current_property_control_exact(
    input: &RollingPlanInput,
    catalog: &SimulationCatalog,
    search_turns: u32,
) -> Option<ForcePackagePlan> {
    let PreparedFacilities {
        facilities,
        candidates_pruned,
        state_limit,
        funnel_audit,
        funnel_pre_prune,
    } = prepare_facilities(input);
    audit_exact_options(
        input,
        &facilities,
        candidates_pruned,
        funnel_audit,
        funnel_pre_prune.as_ref(),
    );
    audit_exact_input(input, &facilities);
    let RoleCatalog {
        facility_role_options,
        distinct_roles,
    } = collect_role_catalog(input, &facilities);
    let funnel_indirect_role_set =
        audit_exact_roles(input, &facility_role_options, &distinct_roles, funnel_audit);
    let (all_multisets, multiset_truncated) = generate_role_multisets(
        &distinct_roles,
        input,
        &facilities,
        &facility_role_options,
        input.current_funds,
        facilities.len(),
    );
    audit_exact_multisets(
        input,
        &all_multisets,
        multiset_truncated,
        &funnel_indirect_role_set,
        funnel_audit,
    );
    let states = assign_multisets(input, all_multisets, &facilities, &facility_role_options);
    let ReducedFrontier {
        mut states,
        truncated,
        witnesses,
    } = reduce_frontier(input, states, state_limit, multiset_truncated, funnel_audit);
    apply_role_floor(input, &mut states, funnel_audit);
    apply_opening_spend(input, &mut states, &facilities, funnel_audit);
    let considered = states.len();
    let combat_simulation_turns = search_turns.max(DEFAULT_SEARCH_TURNS);
    let evaluated = crate::ai::deterministic_parallel::map_ordered(states, |state| {
        simulate_state_with_catalog(input, catalog, &state, combat_simulation_turns)
    });
    audit_evaluated_plans(input, &evaluated, funnel_audit);
    audit_evaluated_ranking(input, &evaluated);
    let mut selected = select_evaluated_plan(input, &evaluated)?;
    if !witnesses.is_empty() {
        report_frontier_witness_simulation(
            input,
            catalog,
            &witnesses,
            &selected,
            combat_simulation_turns,
        );
    }
    selected.candidates_considered = considered;
    selected.candidates_pruned = candidates_pruned;
    selected.search_truncated = truncated;
    Some(selected)
}

/// 現在手番の施設候補を集め、同一施設で支配される選択肢を剪定する。
fn prepare_facilities(input: &RollingPlanInput) -> PreparedFacilities {
    let mut options_by_facility: HashMap<GridPosition, Vec<usize>> = HashMap::default();
    for (index, option) in input.production_options.iter().enumerate() {
        if option.purchase.build_turn == 0 {
            options_by_facility
                .entry(option.purchase.facility)
                .or_default()
                .push(index);
        }
    }
    // 厳密探索でも、同一施設・同一手番で全交戦対象・価格・被弾期待が劣る候補を
    // 列挙する必要はない。兵種名や固定点ではなく、入力盤面の相性表だけで支配関係を
    // 判定するため、残した候補の最適解は変わらない。
    // 【一時的な診断専用】段階ごとの間接候補の生存数を数えるための入口スナップショット。
    let funnel_audit = std::env::var_os("OPENWARS_INDIRECT_FUNNEL_AUDIT").is_some();
    let funnel_pre_prune = funnel_audit.then(|| options_by_facility.clone());

    let mut candidates_pruned = 0_usize;
    prune_facility_options(input, &mut options_by_facility, &mut candidates_pruned);
    let mut facilities = options_by_facility.into_iter().collect::<Vec<_>>();
    facilities.sort_unstable_by_key(|(facility, _)| (facility.y, facility.x));
    let state_limit = property_control_state_limit(facilities.len());
    PreparedFacilities {
        facilities,
        candidates_pruned,
        state_limit,
        funnel_audit,
        funnel_pre_prune,
    }
}

/// 価格・交戦・相性で支配される候補だけを施設内で落とす。
fn prune_facility_options(
    input: &RollingPlanInput,
    options_by_facility: &mut HashMap<GridPosition, Vec<usize>>,
    candidates_pruned: &mut usize,
) {
    for option_indices in options_by_facility.values_mut() {
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
        *candidates_pruned = candidates_pruned.saturating_add(original_len - retained.len());
        *option_indices = retained;
    }
}

/// 施設順に合法役割を収集し、既存の役割比較順で確定する。
fn collect_role_catalog(
    input: &RollingPlanInput,
    facilities: &[(GridPosition, Vec<usize>)],
) -> RoleCatalog {
    let mut facility_role_options: HashMap<(GridPosition, ProductionRole), usize> =
        HashMap::default();
    let mut distinct_roles_set: HashSet<ProductionRole> = HashSet::default();
    for (facility, option_indices) in facilities {
        record_facility_roles(
            input,
            *facility,
            option_indices,
            &mut facility_role_options,
            &mut distinct_roles_set,
        );
    }
    let mut distinct_roles: Vec<ProductionRole> = distinct_roles_set.into_iter().collect();
    distinct_roles.sort_by_key(|role| {
        (
            role.capture_target().is_none(),
            role.cost(),
            role.unit_type().as_str(),
            role.capture_target().map(|target| (target.y, target.x)),
        )
    });
    RoleCatalog {
        facility_role_options,
        distinct_roles,
    }
}

/// 対象物件を持たない占領役を除き、一施設の役割を登録する。
fn record_facility_roles(
    input: &RollingPlanInput,
    facility: GridPosition,
    option_indices: &[usize],
    facility_role_options: &mut HashMap<(GridPosition, ProductionRole), usize>,
    distinct_roles_set: &mut HashSet<ProductionRole>,
) {
    for option_index in option_indices {
        let option = &input.production_options[*option_index];
        if option.stats.can_capture && option.capture_target.is_none() {
            // 物件レースにおいて占領可能ユニット（歩兵・工兵等）は必ず占領対象物件と紐付けて評価する。
            // 目標なき歩兵の乱造は、戦闘部隊（装甲車・戦車等）の配備枠を奪うため除外する。
            continue;
        }
        let role = if let Some(target) = option.capture_target {
            ProductionRole::Capture {
                target,
                unit_type: option.purchase.unit_type,
                cost: option.purchase.cost,
            }
        } else {
            ProductionRole::Combat {
                unit_type: option.purchase.unit_type,
                cost: option.purchase.cost,
            }
        };
        facility_role_options.insert((facility, role), *option_index);
        distinct_roles_set.insert(role);
    }
}

/// 空購入案を先頭に残し、役割集合ごとに最小費用の施設割当を求める。
fn assign_multisets(
    input: &RollingPlanInput,
    all_multisets: Vec<Vec<ProductionRole>>,
    facilities: &[(GridPosition, Vec<usize>)],
    facility_role_options: &HashMap<(GridPosition, ProductionRole), usize>,
) -> Vec<SearchState> {
    let mut states = vec![SearchState::default()];
    for multiset in all_multisets {
        if let Some(state) = optimize_multiset_facility_assignment(
            input,
            &multiset,
            facilities,
            facility_role_options,
        ) {
            states.push(state);
        }
    }
    states
}

/// 状態数上限を適用し、診断時だけ落選状態の控えを残す。
fn reduce_frontier(
    input: &RollingPlanInput,
    mut states: Vec<SearchState>,
    state_limit: usize,
    multiset_truncated: bool,
    funnel_audit: bool,
) -> ReducedFrontier {
    // 前線縮約で落ちた間接案を、後で本物のシミュレーションへ掛け直すための控え。
    let witness_sim_audit = std::env::var_os("OPENWARS_INDIRECT_WITNESS_SIM").is_some();
    let funnel_pre_frontier = (funnel_audit || witness_sim_audit).then(|| states.clone());
    audit_assigned_states(input, &states, state_limit, funnel_audit);
    let mut frontier_truncated = multiset_truncated;
    if states.len() > state_limit {
        retain_property_control_frontier(input, &mut states, state_limit);
        frontier_truncated = true;
    }
    // 【一時的な診断専用】前線縮約で落ちた間接案のうち、キー順で上位3件だけを控える。
    // 元のSearchStateそのものを保持するので、option_indices＝役割の対応は厳密に保たれる。
    let mut witness_states: Vec<SearchState> = Vec::new();
    if witness_sim_audit && let Some(before) = funnel_pre_frontier.as_ref() {
        let retained = states
            .iter()
            .map(|state| state.option_indices.clone())
            .collect::<HashSet<_>>();
        witness_states = before
            .iter()
            .filter(|state| state_has_indirect(input, state))
            .filter(|state| !retained.contains(&state.option_indices))
            .cloned()
            .collect();
        witness_states.sort_by_key(|state| property_control_frontier_key(input, state));
        witness_states.truncate(3);
    }
    audit_frontier_retain(
        input,
        &states,
        state_limit,
        funnel_audit,
        funnel_pre_frontier.as_deref(),
    );
    ReducedFrontier {
        states,
        truncated: frontier_truncated,
        witnesses: witness_states,
    }
}

/// 到達可能な複数物件があれば初手の占領下限を保持する。
fn apply_role_floor(input: &RollingPlanInput, states: &mut Vec<SearchState>, funnel_audit: bool) {
    let capture_targets = states
        .iter()
        .flat_map(|state| state.used_capture_targets.iter().copied())
        .collect::<HashSet<_>>();
    let available_slots = states
        .iter()
        .map(|state| state.used_slots.len())
        .max()
        .unwrap_or_default();
    let role_floor = (uses_small_map_opening_rules(input)
        && capture_targets.len() >= 2
        && available_slots >= 2
        && states
            .iter()
            .any(|state| state.used_capture_targets.len() >= 2))
    .then_some(2_usize);
    let funnel_before_role_floor = funnel_audit.then(|| {
        states
            .iter()
            .filter(|s| state_has_indirect(input, s))
            .count()
    });
    if let Some(required) = role_floor {
        states.retain(|state| state.used_capture_targets.len() >= required);
    }
    if let Some(before_indirect) = funnel_before_role_floor {
        eprintln!(
            "INDIRECT_FUNNEL_AUDIT stage=role_floor turn={} required={:?} before_indirect={} after_indirect={} after_total={}",
            input.current_turn,
            role_floor,
            before_indirect,
            states
                .iter()
                .filter(|s| state_has_indirect(input, s))
                .count(),
            states.len(),
        );
    }
}

/// 初手の最終候補だけ、残額で利用可能な施設の未使用を除く。
fn apply_opening_spend(
    input: &RollingPlanInput,
    states: &mut Vec<SearchState>,
    facilities: &[(GridPosition, Vec<usize>)],
    funnel_audit: bool,
) {
    let funnel_before_spend = funnel_audit.then(|| {
        states
            .iter()
            .filter(|s| state_has_indirect(input, s))
            .count()
    });
    if uses_small_map_opening_rules(input) {
        states.retain(|state| opening_package_spends_all_usable_funds(input, state, facilities));
    }
    if let Some(before_indirect) = funnel_before_spend {
        eprintln!(
            "INDIRECT_FUNNEL_AUDIT stage=opening_spend turn={} applied={} before_indirect={} after_indirect={} after_total={}",
            input.current_turn,
            uses_small_map_opening_rules(input),
            before_indirect,
            states
                .iter()
                .filter(|s| state_has_indirect(input, s))
                .count(),
            states.len(),
        );
    }
}

/// 直接部隊の基準を選び、同じ契約を悪化させない間接案だけへ置き換える。
fn select_evaluated_plan(
    input: &RollingPlanInput,
    evaluated: &[ForcePackagePlan],
) -> Option<ForcePackagePlan> {
    let mut selected = select_best_property_plan(
        input,
        evaluated
            .iter()
            .filter(|plan| !plan.has_indirect_purchase(input)),
    )
    .or_else(|| select_best_property_plan(input, evaluated.iter()))?;
    // 診断専用の控え。環境変数が無い通常対戦ではcloneも監査も一切行わない。
    let gate_audit_baseline = std::env::var_os("OPENWARS_INDIRECT_GATE_AUDIT")
        .is_some()
        .then(|| selected.clone());
    if let Some(indirect_upgrade) = select_best_indirect_upgrade(
        input,
        evaluated.iter().filter(|candidate| {
            candidate.has_indirect_purchase(input)
                && indirect_upgrade_preserves_direct_plan(input, candidate, &selected)
        }),
    ) {
        selected = indirect_upgrade;
    }
    if let Some(direct_baseline) = gate_audit_baseline.as_ref() {
        // 選択結果は変更しない。既にシミュレーション済みの候補だけを読み出して記録する。
        report_indirect_gate_audit(input, evaluated, direct_baseline, &selected);
    }
    Some(selected)
}
