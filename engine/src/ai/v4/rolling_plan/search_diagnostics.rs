//! RollingPlanのsearch_diagnostics責務。
use super::combat_capture_simulation::simulate_state_with_catalog;
use super::frontier::first_differing_frontier_field;
use super::frontier::multiset_frontier_key;
use super::frontier::property_control_tactical_profile;
use super::frontier::{indirect_upgrade_preserves_direct_plan, property_control_frontier_key};
use super::plan_ranking::{
    current_direct_role_counts, exact_property_plan_better, uses_small_map_tactical_rules,
};
use super::role_multiset::{
    MULTISET_BEAM_WIDTH, MultisetCandidate, ProductionRole, PropertyFrontierKey, RoleMetadata,
};
use super::{ForcePackagePlan, HashSet, RollingPlanInput, SearchState, SimulationCatalog};
use super::{GridPosition, HashMap, UnitType};

/// 【一時的な診断専用】前線縮約で落ちた間接案を、本採用案と同じcatalog・同じ探索turn数で
/// 実シミュレーションし直し、推定キーではなく実際の結果で優劣を突き合わせる。
///
/// `OPENWARS_INDIRECT_WITNESS_SIM` が設定された時だけ呼ばれる。渡すのは縮約前に控えた
/// 元の `SearchState` そのもので、`evaluate_fixed_package` も購入列の再照合も使わないため
/// 役割（占領先）対応は厳密に保たれる。選択結果は一切変更しない。
pub(super) fn report_frontier_witness_simulation(
    input: &RollingPlanInput,
    catalog: &SimulationCatalog,
    witness_states: &[SearchState],
    selected: &ForcePackagePlan,
    combat_simulation_turns: u32,
) {
    // 実シミュレーション後の物理的な結末だけを並べる。推定値は混ぜない。
    let outcome = |plan: &ForcePackagePlan| -> String {
        let expected_capturers_current = plan
            .capture_purchases
            .iter()
            .filter(|assignment| assignment.purchase.build_turn == 0)
            .count();
        let completed_survived = plan
            .capture_purchases
            .iter()
            .filter(|assignment| {
                assignment.completion_turn.is_some() && assignment.survived_at_completion
            })
            .count();
        format!(
            "feasible={} contracts_sat={} missed_interdiction={} cleared_lanes={} capturers_expected_total={} capturers_expected_now={} capturers_completed_survived={} protected_survivors={}/{} deadline_survivors={}/{} blocked_slots={} actual_remaining_enemy_hp={} remaining_enemy_value={} expected_loss={} surviving_combat_value={} production_cost={} destruction={:?} front={:?} first_attack={:?} occupation={:?} capture_prof={:?} contract_prof={:?}",
            plan.feasible,
            plan.property_contracts_satisfied(
                &input.interdiction_deadlines,
                input.required_capture_survivors
            ),
            plan.missed_interdiction_contracts(&input.interdiction_deadlines),
            plan.cleared_capture_lane_count(),
            plan.capture_purchases.len(),
            expected_capturers_current,
            completed_survived,
            plan.protected_survivor_count,
            plan.protected_unit_count,
            plan.deadline_capture_survivor_count,
            plan.required_capture_survivor_count,
            plan.blocked_next_turn_production_slots,
            plan.remaining_hp(),
            plan.remaining_enemy_value(input),
            plan.expected_loss,
            plan.surviving_combat_value,
            plan.production_cost,
            plan.target_destruction_profile(),
            plan.front_breakthrough_rank(),
            plan.first_attack_turn,
            plan.occupation_turn,
            plan.capture_completion_profile(),
            plan.property_contract_completion_profile(),
        )
    };

    // 実際に効いている比較枝（締切なし・成立可否が同じ・小規模map）の成分順で、
    // 最初に食い違う軸を一つだけ特定する。枝が違う場合はその旨を返す。
    let first_differing_comparator_field = |candidate: &ForcePackagePlan,
                                            current: &ForcePackagePlan|
     -> String {
        if !input.interdiction_deadlines.is_empty() {
            return "branch=interdiction_contracts".to_owned();
        }
        if candidate.feasible != current.feasible {
            return "feasible".to_owned();
        }
        if candidate.feasible {
            return "branch=both_feasible".to_owned();
        }
        if !uses_small_map_tactical_rules(input) {
            return "branch=standard_map_infeasible".to_owned();
        }
        if candidate.cleared_capture_lane_count() != current.cleared_capture_lane_count() {
            "cleared_capture_lane_count".to_owned()
        } else if candidate.property_contract_completion_profile()
            != current.property_contract_completion_profile()
        {
            "property_contract_completion_profile".to_owned()
        } else if candidate.front_breakthrough_rank() != current.front_breakthrough_rank() {
            "front_breakthrough_rank".to_owned()
        } else if candidate.target_destruction_profile() != current.target_destruction_profile() {
            "target_destruction_profile".to_owned()
        } else if candidate.remaining_enemy_value(input) != current.remaining_enemy_value(input) {
            "remaining_enemy_value".to_owned()
        } else if candidate.surviving_combat_value != current.surviving_combat_value {
            "surviving_combat_value".to_owned()
        } else if candidate.blocked_next_turn_production_slots
            != current.blocked_next_turn_production_slots
        {
            "blocked_next_turn_production_slots".to_owned()
        } else if candidate.expected_loss != current.expected_loss {
            "expected_loss".to_owned()
        } else if candidate.deadline_capture_survivor_count
            != current.deadline_capture_survivor_count
        {
            "deadline_capture_survivor_count".to_owned()
        } else if candidate.capture_completion_profile() != current.capture_completion_profile() {
            "capture_completion_profile".to_owned()
        } else if candidate.remaining_hp() != current.remaining_hp() {
            "remaining_hp".to_owned()
        } else if candidate.production_cost != current.production_cost {
            "production_cost".to_owned()
        } else {
            "none".to_owned()
        }
    };

    // 占領必要数(required_capture_survivors)の内訳。既存の被保護unitと、先行計画分の
    // 被保護枠、そして今回新規に賄う必要がある数を、実入力の件数のまま出す。
    let existing_protected = input
        .existing_units
        .iter()
        .filter(|unit| unit.protection_completion_turn.is_some())
        .collect::<Vec<_>>();
    eprintln!(
        "INDIRECT_WITNESS_SIM quota_supply turn={} required_capture_survivors={} existing_units_total={} existing_protected_count={} existing_protected_alive_hp={} existing_protected_hp_list={:?} planned_protected_units_count={} planned_protected_hp={} existing_unprotected_count={} facilities_with_current_options={}",
        input.current_turn,
        input.required_capture_survivors,
        input.existing_units.len(),
        existing_protected.len(),
        existing_protected
            .iter()
            .filter(|unit| unit.hp > 0)
            .map(|unit| unit.hp)
            .sum::<u32>(),
        existing_protected
            .iter()
            .map(|unit| (
                unit.stats.unit_type,
                unit.hp,
                unit.protection_completion_turn
            ))
            .collect::<Vec<_>>(),
        input.protected_units.len(),
        input
            .protected_units
            .iter()
            .map(|unit| unit.hp)
            .sum::<u32>(),
        input.existing_units.len() - existing_protected.len(),
        input
            .production_options
            .iter()
            .filter(|option| option.purchase.build_turn == 0)
            .map(|option| option.purchase.facility)
            .collect::<HashSet<_>>()
            .len(),
    );
    eprintln!(
        "INDIRECT_WITNESS_SIM selected turn={} purchases={:?} {}",
        input.current_turn,
        selected
            .purchases
            .iter()
            .map(|purchase| (
                purchase.unit_type,
                purchase.facility.x,
                purchase.facility.y,
                purchase.cost,
                selected.capture_target_for(*purchase)
            ))
            .collect::<Vec<_>>(),
        outcome(selected),
    );

    for (rank, state) in witness_states.iter().enumerate() {
        let key = property_control_frontier_key(input, state);
        let simulated = simulate_state_with_catalog(input, catalog, state, combat_simulation_turns);
        eprintln!(
            "INDIRECT_WITNESS_SIM witness turn={} rank={} units={:?} state_cost={} frontier_estimate(capture_shortfall={} remaining_relevant_hp={} remaining_all_hp={} cost={}) witness_beats_selected={} selected_beats_witness={} first_differing_comparator_field={} {}",
            input.current_turn,
            rank + 1,
            state
                .option_indices
                .iter()
                .map(|index| {
                    let option = &input.production_options[*index];
                    (
                        option.purchase.unit_type,
                        option.purchase.facility.x,
                        option.purchase.facility.y,
                        option.purchase.cost,
                        option.capture_target,
                    )
                })
                .collect::<Vec<_>>(),
            state.cost,
            key.capture_shortfall,
            key.remaining_relevant_hp,
            key.remaining_all_hp,
            key.cost,
            exact_property_plan_better(input, &simulated, selected),
            exact_property_plan_better(input, selected, &simulated),
            first_differing_comparator_field(&simulated, selected),
            outcome(&simulated),
        );
    }
}

/// 【一時的な診断専用】間接案ゲートが、直接基準案より良い間接案を捨てているかを記録する。
///
/// `OPENWARS_INDIRECT_GATE_AUDIT` が設定された時だけ呼び出される。既にこの手番で
/// シミュレーション済みの候補（`evaluated`）だけを読み、再シミュレーションも
/// option_indexの再構築も行わないため、役割メタデータの取り違えによる誤った
/// 比較を混入させない。選択結果（`selected`）は一切変更しない。
pub(super) fn report_indirect_gate_audit(
    input: &RollingPlanInput,
    evaluated: &[ForcePackagePlan],
    direct_baseline: &ForcePackagePlan,
    final_selected: &ForcePackagePlan,
) {
    // 購入列を「兵種・施設・手番・費用・役割」で表示する。役割はcapture契約の
    // targetまで一致させて引くため、同一purchaseに複数の占領先候補がある場合だけ
    // [amb]を付け、曖昧さを隠さない。
    let describe_purchases = |plan: &ForcePackagePlan| -> String {
        plan.purchases
            .iter()
            .map(|purchase| {
                let target = plan.capture_target_for(*purchase);
                let matches = input
                    .production_options
                    .iter()
                    .filter(|option| {
                        option.purchase == *purchase && option.capture_target == target
                    })
                    .collect::<Vec<_>>();
                let role = match target {
                    Some(target) => format!("capture@({},{})", target.x, target.y),
                    None => match matches.first() {
                        Some(option) if option.stats.min_range > 1 => "indirect".to_owned(),
                        Some(_) => "direct".to_owned(),
                        None => "unknown".to_owned(),
                    },
                };
                format!(
                    "{:?}@({},{})/build{}/cost{}/{}{}",
                    purchase.unit_type,
                    purchase.facility.x,
                    purchase.facility.y,
                    purchase.build_turn,
                    purchase.cost,
                    role,
                    if matches.len() == 1 { "" } else { "[amb]" },
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    };

    // 比較に使われる指標を、ゲートの各条件と同じ形のまま並べる。
    let describe_metrics = |plan: &ForcePackagePlan| -> String {
        let (capture_roles, direct_roles) = current_direct_role_counts(input, plan);
        format!(
            "feasible={} contracts_sat={} missed_interdiction={} lanes={} contract_prof={:?} capture_prof={:?} deadline_surv={}/{} front={:?} blocked_slots={} destruction={:?} rem_enemy_value={} rem_hp={} loss={} surv_value={} cost={} direct_fire_lanes={} roles(capture/direct)={}/{} marginal_indirect={}",
            plan.feasible,
            plan.property_contracts_satisfied(
                &input.interdiction_deadlines,
                input.required_capture_survivors
            ),
            plan.missed_interdiction_contracts(&input.interdiction_deadlines),
            plan.cleared_capture_lane_count(),
            plan.property_contract_completion_profile(),
            plan.capture_completion_profile(),
            plan.deadline_capture_survivor_count,
            plan.required_capture_survivor_count,
            plan.front_breakthrough_rank(),
            plan.blocked_next_turn_production_slots,
            plan.target_destruction_profile(),
            plan.remaining_enemy_value(input),
            plan.remaining_hp(),
            plan.expected_loss,
            plan.surviving_combat_value,
            plan.production_cost,
            plan.direct_fire_lane_count,
            capture_roles,
            direct_roles,
            plan.indirect_fire_has_marginal_target(),
        )
    };

    // ゲートの各連言を、個別に真偽値として出す。どの条件が棄却理由かを一意に特定する。
    let describe_gate = |candidate: &ForcePackagePlan, reference: &ForcePackagePlan| -> String {
        let (candidate_capture, candidate_direct) = current_direct_role_counts(input, candidate);
        let (reference_capture, reference_direct) = current_direct_role_counts(input, reference);
        let small_map = uses_small_map_tactical_rules(input);
        let preserves_roles = if small_map {
            candidate.direct_fire_lane_count > 0 || reference.direct_fire_lane_count == 0
        } else {
            candidate_direct >= reference_direct
        };
        format!(
            "g1_cleared_lanes={} g2_capture_roles={}({}vs{}) g3_direct_roles={}(small_map={} counts={}vs{} lanes={}vs{}) g4_contract_profile={} g5_deadline_survivors={} g6_capture_profile={} g7_front_breakthrough={} g8_blocked_slots={}({}vs{}) g9_small_map_marginal={} g10_value_or_loss={} gate_pass={}",
            candidate.cleared_capture_lane_count() >= reference.cleared_capture_lane_count(),
            candidate_capture >= reference_capture,
            candidate_capture,
            reference_capture,
            preserves_roles,
            small_map,
            candidate_direct,
            reference_direct,
            candidate.direct_fire_lane_count,
            reference.direct_fire_lane_count,
            candidate.property_contract_completion_profile()
                <= reference.property_contract_completion_profile(),
            candidate.deadline_capture_survivor_count >= reference.deadline_capture_survivor_count,
            candidate.capture_completion_profile() <= reference.capture_completion_profile(),
            candidate.front_breakthrough_rank() <= reference.front_breakthrough_rank(),
            candidate.blocked_next_turn_production_slots
                <= reference.blocked_next_turn_production_slots,
            candidate.blocked_next_turn_production_slots,
            reference.blocked_next_turn_production_slots,
            !small_map
                || (candidate.indirect_fire_has_marginal_target()
                    || candidate.target_elimination_profile()
                        < reference.target_elimination_profile()),
            candidate.remaining_enemy_value(input) < reference.remaining_enemy_value(input)
                || candidate.expected_loss < reference.expected_loss,
            indirect_upgrade_preserves_direct_plan(input, candidate, reference),
        )
    };

    let existing_direct = input
        .existing_units
        .iter()
        .filter(|unit| unit.stats.min_range <= 1)
        .collect::<Vec<_>>();
    let existing_indirect = input
        .existing_units
        .iter()
        .filter(|unit| unit.stats.min_range > 1)
        .collect::<Vec<_>>();
    eprintln!(
        "INDIRECT_GATE_AUDIT context turn={} small_map={} policy={:?} funds={} income={} required_capture_survivors={} deadlines={:?} evaluated={} indirect_candidates={} existing_screen_direct={}/hp{} existing_indirect={}/hp{} enemies={:?}",
        input.current_turn,
        uses_small_map_tactical_rules(input),
        input.opening_policy,
        input.current_funds,
        input.income_per_turn,
        input.required_capture_survivors,
        input.interdiction_deadlines,
        evaluated.len(),
        evaluated
            .iter()
            .filter(|plan| plan.has_indirect_purchase(input))
            .count(),
        existing_direct.len(),
        existing_direct.iter().map(|unit| unit.hp).sum::<u32>(),
        existing_indirect.len(),
        existing_indirect.iter().map(|unit| unit.hp).sum::<u32>(),
        input
            .enemies
            .iter()
            .map(|enemy| (
                enemy.stats.unit_type,
                enemy.hp,
                enemy.position.x,
                enemy.position.y
            ))
            .collect::<Vec<_>>(),
    );
    eprintln!(
        "INDIRECT_GATE_AUDIT direct_baseline turn={} purchases=[{}] {}",
        input.current_turn,
        describe_purchases(direct_baseline),
        describe_metrics(direct_baseline),
    );
    eprintln!(
        "INDIRECT_GATE_AUDIT final_selected turn={} upgraded={} purchases=[{}] {}",
        input.current_turn,
        final_selected.purchases != direct_baseline.purchases,
        describe_purchases(final_selected),
        describe_metrics(final_selected),
    );

    let indirect_candidates = evaluated
        .iter()
        .filter(|plan| plan.has_indirect_purchase(input))
        .collect::<Vec<_>>();
    // 全間接案の中で最強の一案を、ゲートを通す前の基準で選ぶ。
    if let Some(strongest) = indirect_candidates
        .iter()
        .copied()
        .reduce(|best, candidate| {
            if exact_property_plan_better(input, candidate, best) {
                candidate
            } else {
                best
            }
        })
    {
        eprintln!(
            "INDIRECT_GATE_AUDIT strongest_indirect turn={} better_than_direct={} better_than_final={} purchases=[{}] {} | {}",
            input.current_turn,
            exact_property_plan_better(input, strongest, direct_baseline),
            exact_property_plan_better(input, strongest, final_selected),
            describe_purchases(strongest),
            describe_metrics(strongest),
            describe_gate(strongest, direct_baseline),
        );
    }

    // 証人＝「直接基準案より良い」と評価されながらゲートで棄却された間接案。
    let mut pool = indirect_candidates
        .into_iter()
        .filter(|candidate| {
            exact_property_plan_better(input, candidate, direct_baseline)
                && !indirect_upgrade_preserves_direct_plan(input, candidate, direct_baseline)
        })
        .collect::<Vec<_>>();
    if pool.is_empty() {
        eprintln!(
            "INDIRECT_GATE_AUDIT no_witness turn={} reason=no_indirect_candidate_beats_direct_baseline_and_is_rejected",
            input.current_turn,
        );
        return;
    }
    // 全順序でない比較関数をsortへ渡さず、都度最強を取り出して上位3件だけ残す。
    let mut rank = 0_usize;
    while rank < 3 && !pool.is_empty() {
        let mut best_index = 0_usize;
        for index in 1..pool.len() {
            if exact_property_plan_better(input, pool[index], pool[best_index]) {
                best_index = index;
            }
        }
        let witness = pool.remove(best_index);
        rank += 1;
        eprintln!(
            "INDIRECT_GATE_AUDIT witness turn={} rank={} better_than_final={} final_better_than_witness={} preserves_final={} purchases=[{}] {} | vs_direct {} | vs_final {}",
            input.current_turn,
            rank,
            exact_property_plan_better(input, witness, final_selected),
            exact_property_plan_better(input, final_selected, witness),
            indirect_upgrade_preserves_direct_plan(input, witness, final_selected),
            describe_purchases(witness),
            describe_metrics(witness),
            describe_gate(witness, direct_baseline),
            describe_gate(witness, final_selected),
        );
    }
    eprintln!(
        "INDIRECT_GATE_AUDIT witness_total turn={} reported={} remaining={}",
        input.current_turn,
        rank,
        pool.len(),
    );
}

/// 探索段ごとの間接候補の有無を診断する。
pub(super) fn state_has_indirect(input: &RollingPlanInput, state: &SearchState) -> bool {
    state
        .option_indices
        .iter()
        .any(|index| input.production_options[*index].stats.min_range > 1)
}

/// 支配剪定前後の候補数と占領quota費用を観測する。
pub(super) fn audit_exact_options(
    input: &RollingPlanInput,
    facilities: &[(GridPosition, Vec<usize>)],
    candidates_pruned: usize,
    funnel_audit: bool,
    funnel_pre_prune: Option<&HashMap<GridPosition, Vec<usize>>>,
) {
    if funnel_audit {
        let indirect_count = |indices: &mut dyn Iterator<Item = usize>| {
            indices
                .filter(|index| input.production_options[*index].stats.min_range > 1)
                .count()
        };
        let pre_total = funnel_pre_prune
            .as_ref()
            .map_or(0, |map| map.values().map(Vec::len).sum::<usize>());
        let pre_indirect = funnel_pre_prune.as_ref().map_or(0, |map| {
            indirect_count(&mut map.values().flatten().copied())
        });
        let post_total = facilities
            .iter()
            .map(|(_, indices)| indices.len())
            .sum::<usize>();
        let post_indirect =
            indirect_count(&mut facilities.iter().flat_map(|(_, i)| i.iter().copied()));
        // 目標ごとの最安占領役を積み上げ、必要数を満たす最小費用を出す。
        let mut cheapest_by_target: HashMap<GridPosition, u32> = HashMap::default();
        for option in input
            .production_options
            .iter()
            .filter(|option| option.purchase.build_turn == 0)
        {
            if let Some(target) = option.capture_target {
                let entry = cheapest_by_target
                    .entry(target)
                    .or_insert(option.purchase.cost);
                *entry = (*entry).min(option.purchase.cost);
            }
        }
        let mut quota_costs = cheapest_by_target.values().copied().collect::<Vec<_>>();
        quota_costs.sort_unstable();
        let quota = input
            .required_capture_survivors
            .min(quota_costs.len())
            .min(facilities.len());
        let cheapest_indirect = input
            .production_options
            .iter()
            .filter(|option| option.purchase.build_turn == 0 && option.stats.min_range > 1)
            .map(|option| option.purchase.cost)
            .min();
        eprintln!(
            "INDIRECT_FUNNEL_AUDIT stage=options turn={} facilities={} funds={} required_capture_survivors={} distinct_capture_targets={} pre_prune_total={} pre_prune_indirect={} post_prune_total={} post_prune_indirect={} pruned={} cheapest_indirect={:?} cheapest_capture={:?} min_cost_for_capture_quota({})={} quota_plus_cheapest_indirect={:?}",
            input.current_turn,
            facilities.len(),
            input.current_funds,
            input.required_capture_survivors,
            cheapest_by_target.len(),
            pre_total,
            pre_indirect,
            post_total,
            post_indirect,
            candidates_pruned,
            cheapest_indirect,
            quota_costs.first().copied(),
            quota,
            quota_costs.iter().take(quota).sum::<u32>(),
            cheapest_indirect.map(|cost| quota_costs
                .iter()
                .take(quota)
                .sum::<u32>()
                .saturating_add(cost)),
        );
    }
}

/// 厳密探索に渡された施設・役割・対象数を記録する。
pub(super) fn audit_exact_input(
    input: &RollingPlanInput,
    facilities: &[(GridPosition, Vec<usize>)],
) {
    if std::env::var_os("OPENWARS_ROLLING_AUDIT").is_some() {
        let capture_option_count = input
            .production_options
            .iter()
            .filter(|option| option.purchase.build_turn == 0 && option.capture_target.is_some())
            .count();
        let combat_option_count = input
            .production_options
            .iter()
            .filter(|option| option.purchase.build_turn == 0 && option.capture_target.is_none())
            .count();
        let capture_target_count = input
            .production_options
            .iter()
            .filter(|option| option.purchase.build_turn == 0)
            .filter_map(|option| option.capture_target)
            .collect::<HashSet<_>>()
            .len();
        eprintln!(
            "ROLLING_AUDIT exact_input facilities={} combat_options={} capture_options={} capture_targets={} funds={} enemies={}",
            facilities.len(),
            combat_option_count,
            capture_option_count,
            capture_target_count,
            input.current_funds,
            input.enemies.len(),
        );
    }
}

/// 間接役が役割集合へ残ったかを記録する。
pub(super) fn audit_exact_roles(
    input: &RollingPlanInput,
    facility_role_options: &HashMap<(GridPosition, ProductionRole), usize>,
    distinct_roles: &[ProductionRole],
    funnel_audit: bool,
) -> HashSet<ProductionRole> {
    let funnel_indirect_role_set: HashSet<ProductionRole> = if funnel_audit {
        facility_role_options
            .iter()
            .filter(|(_, option_index)| {
                input.production_options[**option_index].stats.min_range > 1
            })
            .map(|((_, role), _)| *role)
            .collect()
    } else {
        HashSet::default()
    };
    if funnel_audit {
        eprintln!(
            "INDIRECT_FUNNEL_AUDIT stage=roles turn={} distinct_roles={} indirect_roles={} indirect_role_units={:?}",
            input.current_turn,
            distinct_roles.len(),
            funnel_indirect_role_set.len(),
            funnel_indirect_role_set
                .iter()
                .map(|role| (role.unit_type(), role.cost()))
                .collect::<Vec<_>>(),
        );
    }
    funnel_indirect_role_set
}

/// 役割集合の列挙後に間接役を含む案の数を観測する。
pub(super) fn audit_exact_multisets(
    input: &RollingPlanInput,
    all_multisets: &[Vec<ProductionRole>],
    multiset_truncated: bool,
    funnel_indirect_role_set: &HashSet<ProductionRole>,
    funnel_audit: bool,
) {
    if funnel_audit {
        eprintln!(
            "INDIRECT_FUNNEL_AUDIT stage=multisets turn={} multisets_total={} multisets_with_indirect={} beam_truncated={}",
            input.current_turn,
            all_multisets.len(),
            all_multisets
                .iter()
                .filter(|multiset| multiset
                    .iter()
                    .any(|role| funnel_indirect_role_set.contains(role)))
                .count(),
            multiset_truncated,
        );
    }
}

/// 【一時的な診断専用】役割集合のビーム縮約の前後で、間接役を含む候補が何件残るかを記録する。
/// `OPENWARS_INDIRECT_FUNNEL_AUDIT` が設定された時だけ作られ、探索結果は変更しない。
pub(super) struct MultisetBeamAudit {
    /// 役割indexごとに、間接（min_range>1）の選択肢を代表に持つか。
    indirect_roles: Vec<bool>,
}

/// 縮約前に控えた一段分の候補と、そのうち間接役を含む件数。
pub(super) struct GeneratedMultisets {
    candidates: Vec<MultisetCandidate>,
    indirect: usize,
}

impl MultisetBeamAudit {
    pub(super) fn from_env(
        roles: &[ProductionRole],
        input: &RollingPlanInput,
        facilities: &[(GridPosition, Vec<usize>)],
        facility_role_options: &HashMap<(GridPosition, ProductionRole), usize>,
    ) -> Option<Self> {
        std::env::var_os("OPENWARS_INDIRECT_FUNNEL_AUDIT")
            .is_some()
            .then(|| Self {
                indirect_roles: roles
                    .iter()
                    .map(|role| {
                        facilities
                            .iter()
                            .find_map(|(facility, _)| {
                                facility_role_options.get(&(*facility, *role)).copied()
                            })
                            .is_some_and(|option_index| {
                                input.production_options[option_index].stats.min_range > 1
                            })
                    })
                    .collect(),
            })
    }

    fn has_indirect(&self, candidate: &MultisetCandidate) -> bool {
        candidate
            .role_indices
            .iter()
            .any(|index| self.indirect_roles[*index])
    }

    pub(super) fn observe_generated(&self, level: &[MultisetCandidate]) -> GeneratedMultisets {
        GeneratedMultisets {
            candidates: level.to_vec(),
            indirect: level
                .iter()
                .filter(|candidate| self.has_indirect(candidate))
                .count(),
        }
    }

    pub(super) fn report(
        &self,
        input: &RollingPlanInput,
        roles: &[ProductionRole],
        role_metas: &[RoleMetadata],
        size: usize,
        generated: GeneratedMultisets,
        retained: &[MultisetCandidate],
    ) {
        let after = retained
            .iter()
            .filter(|candidate| self.has_indirect(candidate))
            .count();
        // 切り捨てで全滅した場合だけ、どのキー成分で負けたかを特定する。
        let detail = if generated.indirect > 0 && after == 0 {
            self.rejection_detail(input, roles, role_metas, &generated.candidates, retained)
        } else {
            String::new()
        };
        eprintln!(
            "INDIRECT_FUNNEL_AUDIT stage=multiset_beam turn={} size={} generated={} beam_width={} indirect_before={} indirect_after={} truncated={}{}",
            input.current_turn,
            size,
            generated.candidates.len(),
            MULTISET_BEAM_WIDTH,
            generated.indirect,
            after,
            generated.candidates.len() > MULTISET_BEAM_WIDTH,
            detail,
        );
    }

    /// 最良の間接候補と最下位の残存候補を突き合わせ、最初に食い違うキー成分を示す。
    fn rejection_detail(
        &self,
        input: &RollingPlanInput,
        roles: &[ProductionRole],
        role_metas: &[RoleMetadata],
        generated: &[MultisetCandidate],
        retained: &[MultisetCandidate],
    ) -> String {
        let key = |candidate: &MultisetCandidate| {
            multiset_frontier_key(input, &candidate.role_indices, role_metas)
        };
        let best_indirect = generated
            .iter()
            .filter(|candidate| self.has_indirect(candidate))
            .min_by_key(|candidate| key(candidate));
        let worst_retained = retained.iter().max_by_key(|candidate| key(candidate));
        let (Some(rejected), Some(retained)) = (best_indirect, worst_retained) else {
            return String::new();
        };
        let rejected_key = key(rejected);
        let retained_key = key(retained);
        format!(
            " first_differing_key={} rejected({}) retained({})",
            first_differing_frontier_field(&rejected_key, &retained_key),
            describe_multiset(roles, rejected, &rejected_key),
            describe_multiset(roles, retained, &retained_key),
        )
    }
}

fn describe_multiset(
    roles: &[ProductionRole],
    candidate: &MultisetCandidate,
    key: &PropertyFrontierKey,
) -> String {
    format!(
        "roles={:?} cost={} capture_shortfall={} completion={:?} combat_desc={:?} movement_desc={:?} rel_hp={} all_hp={} excess={}",
        candidate
            .role_indices
            .iter()
            .map(|index| (roles[*index].unit_type(), roles[*index].capture_target()))
            .collect::<Vec<_>>(),
        key.cost,
        key.capture_shortfall,
        key.capture_completion_turns,
        key.combat_unit_count_desc,
        key.total_movement_desc,
        key.remaining_relevant_hp,
        key.remaining_all_hp,
        key.excess_capturers,
    )
}

/// 施設割当まで成立した案数を観測する。
pub(super) fn audit_assigned_states(
    input: &RollingPlanInput,
    states: &[SearchState],
    state_limit: usize,
    funnel_audit: bool,
) {
    if funnel_audit {
        eprintln!(
            "INDIRECT_FUNNEL_AUDIT stage=assigned_states turn={} states={} indirect_states={} state_limit={}",
            input.current_turn,
            states.len(),
            states
                .iter()
                .filter(|s| state_has_indirect(input, s))
                .count(),
            state_limit,
        );
    }
}

/// 前線縮約の落選案と残存案の最初の比較差を診断する。
pub(super) fn audit_frontier_retain(
    input: &RollingPlanInput,
    states: &[SearchState],
    state_limit: usize,
    funnel_audit: bool,
    funnel_pre_frontier: Option<&[SearchState]>,
) {
    if let Some(before) = funnel_pre_frontier.as_ref().filter(|_| funnel_audit) {
        let before_indirect = before
            .iter()
            .filter(|s| state_has_indirect(input, s))
            .count();
        let after_indirect = states
            .iter()
            .filter(|s| state_has_indirect(input, s))
            .count();
        let mut detail = String::new();
        // 前線縮約で間接案が全滅した場合だけ、最良の落選案と最下位の残存案を比較する。
        if before_indirect > 0 && after_indirect == 0 {
            let rejected = before
                .iter()
                .filter(|s| state_has_indirect(input, s))
                .min_by_key(|state| property_control_frontier_key(input, state));
            let retained = states
                .iter()
                .max_by_key(|state| property_control_frontier_key(input, state));
            if let (Some(rejected), Some(retained)) = (rejected, retained) {
                let rejected_key = property_control_frontier_key(input, rejected);
                let retained_key = property_control_frontier_key(input, retained);
                detail = format!(
                    " first_differing_key={} rejected(units={:?} cost={} captures={} shortfall={} completion={:?} combat_desc={:?} movement_desc={:?} rel_hp={} all_hp={} excess={} profile={:?}) retained(units={:?} cost={} captures={} shortfall={} completion={:?} combat_desc={:?} movement_desc={:?} rel_hp={} all_hp={} excess={} profile={:?})",
                    first_differing_frontier_field(&rejected_key, &retained_key),
                    rejected
                        .option_indices
                        .iter()
                        .map(|index| {
                            let option = &input.production_options[*index];
                            (option.purchase.unit_type, option.capture_target)
                        })
                        .collect::<Vec<_>>(),
                    rejected.cost,
                    rejected.used_capture_targets.len(),
                    rejected_key.capture_shortfall,
                    rejected_key.capture_completion_turns,
                    rejected_key.combat_unit_count_desc,
                    rejected_key.total_movement_desc,
                    rejected_key.remaining_relevant_hp,
                    rejected_key.remaining_all_hp,
                    rejected_key.excess_capturers,
                    property_control_tactical_profile(input, rejected).0,
                    retained
                        .option_indices
                        .iter()
                        .map(|index| {
                            let option = &input.production_options[*index];
                            (option.purchase.unit_type, option.capture_target)
                        })
                        .collect::<Vec<_>>(),
                    retained.cost,
                    retained.used_capture_targets.len(),
                    retained_key.capture_shortfall,
                    retained_key.capture_completion_turns,
                    retained_key.combat_unit_count_desc,
                    retained_key.total_movement_desc,
                    retained_key.remaining_relevant_hp,
                    retained_key.remaining_all_hp,
                    retained_key.excess_capturers,
                    property_control_tactical_profile(input, retained).0,
                );
            }
        }
        eprintln!(
            "INDIRECT_FUNNEL_AUDIT stage=frontier_retain turn={} before_total={} before_indirect={} after_total={} after_indirect={} applied={}{}",
            input.current_turn,
            before.len(),
            before_indirect,
            states.len(),
            after_indirect,
            before.len() > state_limit,
            detail,
        );
    }
}

/// 実simulationに到達した候補数を記録する。
pub(super) fn audit_evaluated_plans(
    input: &RollingPlanInput,
    evaluated: &[ForcePackagePlan],
    funnel_audit: bool,
) {
    if funnel_audit {
        eprintln!(
            "INDIRECT_FUNNEL_AUDIT stage=evaluated turn={} evaluated={} indirect_evaluated={}",
            input.current_turn,
            evaluated.len(),
            evaluated
                .iter()
                .filter(|plan| plan.has_indirect_purchase(input))
                .count(),
        );
    }
}

/// 明示監査時だけ評価済み候補を同じ辞書順で表示する。
pub(super) fn audit_evaluated_ranking(input: &RollingPlanInput, evaluated: &[ForcePackagePlan]) {
    if std::env::var_os("OPENWARS_ROLLING_AUDIT").is_none() {
        return;
    }
    let mut audit = evaluated.to_vec();
    audit.sort_by(|left, right| {
        if exact_property_plan_better(input, left, right) {
            std::cmp::Ordering::Less
        } else if exact_property_plan_better(input, right, left) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    audit_ranking_context(input);
    audit_top_plans(&audit);
    audit_best_unit_types(input, &audit);
    audit_ideal_shape(&audit);
    audit_named_shapes(input, &audit);
}

/// 監査の盤面と保護対象数を記録する。
fn audit_ranking_context(input: &RollingPlanInput) {
    eprintln!(
        "ROLLING_AUDIT context policy={:?} funds={} enemies={:?} protected={} options={} capture_options={}",
        input.opening_policy,
        input.current_funds,
        input
            .enemies
            .iter()
            .map(|enemy| (enemy.stats.unit_type, enemy.hp))
            .collect::<Vec<_>>(),
        input
            .existing_units
            .iter()
            .filter(|unit| unit.protection_completion_turn.is_some())
            .count()
            + input.protected_units.len(),
        input.production_options.len(),
        input
            .production_options
            .iter()
            .filter(|option| option.capture_target.is_some())
            .count(),
    );
}

/// 辞書順の上位候補を表示する。
fn audit_top_plans(audit: &[ForcePackagePlan]) {
    for (rank, plan) in audit.iter().take(10).enumerate() {
        eprintln!(
            "ROLLING_AUDIT rank={} purchases={:?} lanes={} joint={:?} breakthrough={:?} blocked={} destruction={:?} survivors={}/{} remaining_hp={} loss={} surviving_value={} cost={}",
            rank + 1,
            plan.purchases
                .iter()
                .map(|purchase| (
                    purchase.unit_type,
                    purchase.cost,
                    purchase.facility,
                    plan.capture_target_for(*purchase)
                ))
                .collect::<Vec<_>>(),
            plan.cleared_capture_lane_count(),
            plan.property_contract_completion_profile(),
            plan.front_breakthrough_rank(),
            plan.blocked_next_turn_production_slots,
            plan.target_destruction_profile(),
            plan.deadline_capture_survivor_count,
            plan.protected_unit_count,
            plan.remaining_hp(),
            plan.expected_loss,
            plan.surviving_combat_value,
            plan.production_cost,
        );
    }
}

/// 兵種ごとの最良候補を診断する。
fn audit_best_unit_types(input: &RollingPlanInput, audit: &[ForcePackagePlan]) {
    let mut seen_unit_types = HashSet::default();
    let unit_types = input
        .production_options
        .iter()
        .map(|option| option.stats.unit_type)
        .filter(|unit_type| seen_unit_types.insert(*unit_type))
        .collect::<Vec<_>>();
    for unit_type in unit_types {
        if let Some((rank, plan)) = audit.iter().enumerate().find(|(_, plan)| {
            plan.purchases
                .iter()
                .any(|purchase| purchase.unit_type == unit_type)
        }) {
            eprintln!(
                "ROLLING_AUDIT type_best={:?} rank={} purchases={:?} lanes={} joint={:?} breakthrough={:?} targets={:?} survivors={}/{} remaining_hp={} loss={} surviving_value={} cost={}",
                unit_type,
                rank + 1,
                plan.purchases
                    .iter()
                    .map(|purchase| purchase.unit_type)
                    .collect::<Vec<_>>(),
                plan.cleared_capture_lane_count(),
                plan.property_contract_completion_profile(),
                plan.front_breakthrough_rank(),
                plan.target_forecasts
                    .iter()
                    .map(|target| (
                        target.unit_type,
                        target.initial_hp,
                        target.remaining_hp,
                        target.destroyed_turn
                    ))
                    .collect::<Vec<_>>(),
                plan.deadline_capture_survivor_count,
                plan.protected_unit_count,
                plan.remaining_hp(),
                plan.expected_loss,
                plan.surviving_combat_value,
                plan.production_cost,
            );
        }
    }
}

/// 既存の診断専用編成形状を表示する。
fn audit_ideal_shape(audit: &[ForcePackagePlan]) {
    if let Some(plan) = audit.iter().find(|plan| {
        plan.purchases.len() == 4
            && plan
                .purchases
                .iter()
                .filter(|purchase| purchase.unit_type == UnitType::Tank)
                .count()
                == 1
            && plan
                .purchases
                .iter()
                .filter(|purchase| purchase.unit_type == UnitType::Recon)
                .count()
                == 1
            && plan
                .purchases
                .iter()
                .filter(|purchase| purchase.unit_type == UnitType::Infantry)
                .count()
                == 2
    }) {
        eprintln!(
            "ROLLING_AUDIT ideal_shape purchases={:?} blocked={} destruction={:?} targets={:?} remaining_hp={} loss={} surviving_value={} cost={}",
            plan.purchases
                .iter()
                .map(|purchase| purchase.unit_type)
                .collect::<Vec<_>>(),
            plan.blocked_next_turn_production_slots,
            plan.target_destruction_profile(),
            plan.target_forecasts
                .iter()
                .map(|target| (
                    target.unit_type,
                    target.initial_hp,
                    target.remaining_hp,
                    target.destroyed_turn,
                ))
                .collect::<Vec<_>>(),
            plan.remaining_hp(),
            plan.expected_loss,
            plan.surviving_combat_value,
            plan.production_cost,
        );
    }
}

/// 既存の診断専用形状に一致した候補を表示する。
fn audit_named_shapes(input: &RollingPlanInput, audit: &[ForcePackagePlan]) {
    for (label, wanted) in [
        (
            "tank_recon_infantry",
            [UnitType::Tank, UnitType::Recon, UnitType::Infantry],
        ),
        (
            "tank_recon_mech",
            [UnitType::Tank, UnitType::Recon, UnitType::Mech],
        ),
    ] {
        if let Some(plan) = audit.iter().find(|plan| {
            plan.purchases.len() == wanted.len()
                && wanted.iter().all(|unit_type| {
                    plan.purchases
                        .iter()
                        .filter(|purchase| purchase.unit_type == *unit_type)
                        .count()
                        == wanted
                            .iter()
                            .filter(|wanted_type| *wanted_type == unit_type)
                            .count()
                })
        }) {
            eprintln!(
                "ROLLING_AUDIT shape={label} purchases={:?} lanes={} joint={:?} breakthrough={:?} blocked={} destruction={:?} remaining_value={} remaining_hp={} loss={} surviving_value={} cost={}",
                plan.purchases
                    .iter()
                    .map(|purchase| purchase.unit_type)
                    .collect::<Vec<_>>(),
                plan.cleared_capture_lane_count(),
                plan.property_contract_completion_profile(),
                plan.front_breakthrough_rank(),
                plan.blocked_next_turn_production_slots,
                plan.target_destruction_profile(),
                plan.remaining_enemy_value(input),
                plan.remaining_hp(),
                plan.expected_loss,
                plan.surviving_combat_value,
                plan.production_cost,
            );
        }
    }
}
