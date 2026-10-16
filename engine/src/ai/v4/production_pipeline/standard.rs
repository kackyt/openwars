//! 生産判断の標準経路。段分割後に方策へ統合する。

use crate::ai::v4::*;

/// 作戦一覧と資金から、この生産フェーズで発行する生産命令を組み立てる。
#[cfg(test)]
pub(in crate::ai::v4) fn plan_production(
    scan: &BoardScan,
    player_id: PlayerId,
    allow_structural_slots: bool,
    committed_combat_assignments: &HashMap<Entity, deployment::ActiveTargetAssignment>,
) -> (Vec<PlannedProduction>, ProductionPlanTrace) {
    let mut registry = V4RollingPlanRegistry::default();
    plan_production_with_registry(
        scan,
        player_id,
        allow_structural_slots,
        committed_combat_assignments,
        0,
        &mut registry,
    )
}

#[derive(Clone, Copy)]
struct ProductionInputs<'a> {
    scan: &'a BoardScan,
    player_id: PlayerId,
    allow_structural_slots: bool,
    committed_combat_assignments: &'a HashMap<Entity, deployment::ActiveTargetAssignment>,
    turn: u32,
}
/// 今回の配分だけで共有する作業状態。永続的な戦略状態はRegistryが所有する。
struct ProductionState {
    ctx: ReachCtx,
    operations: Vec<Operation>,
    ledger: SlotLedger,
    plan_trace: ProductionPlanTrace,
    committed_by_operation: HashMap<usize, HashMap<Entity, Option<plan_revision::PlanId>>>,
    allocation: AllocationState,
    rolling_plans: HashMap<usize, SelectedPlan>,
    seen_plan_ids: HashSet<plan_revision::PlanId>,
    empty_committed_entities: HashSet<Entity>,
    commands: Vec<PlannedProduction>,
}
pub(in crate::ai::v4) fn plan_production_with_registry(
    scan: &BoardScan,
    player_id: PlayerId,
    allow_structural_slots: bool,
    committed_combat_assignments: &HashMap<Entity, deployment::ActiveTargetAssignment>,
    turn: u32,
    plan_registry: &mut V4RollingPlanRegistry,
) -> (Vec<PlannedProduction>, ProductionPlanTrace) {
    let input = ProductionInputs {
        scan,
        player_id,
        allow_structural_slots,
        committed_combat_assignments,
        turn,
    };
    let mut state = prepare_operations(&input, plan_registry);
    if state.operations.is_empty() {
        return (state.commands, state.plan_trace);
    }
    assign_committed_combat(&input, &mut state);
    allocate_operation_slots(&input, &mut state, plan_registry);
    finalize_turn_production(&input, state, plan_registry)
}

fn prepare_operations(
    input: &ProductionInputs<'_>,
    plan_registry: &V4RollingPlanRegistry,
) -> ProductionState {
    let ProductionInputs {
        scan,
        player_id,
        allow_structural_slots,
        ..
    } = *input;

    let mut ctx = ReachCtx::default();
    let active_objectives = plan_registry.active_objectives(player_id);
    let mut operations = build_operations(scan, &mut ctx, &active_objectives);
    let mut plan_trace =
        ProductionPlanTrace::new(player_id, scan.funds, scan.free_facilities.len());

    if operations.is_empty() {
        plan_trace.fallback = true;
        // 作戦に接続されない汎用戦闘生産は行わない。戦力が必要なら、兵站確保・
        // 防衛・首都攻略のいずれかを先にOperationとして成立させる。
    }

    // campaignと切り離した汎用clusterが、固定兵站工程のCombat予算を先取りしない。
    // 防衛を最上位に保ちつつ、Capture同士では兵站工程順、その後に敵接触ETAで並べる。
    operations.sort_by_key(|op| {
        let logistics_rank = scan
            .campaign_objectives
            .iter()
            .find(|objective| objective.anchor == op.anchor)
            .and_then(|objective| objective.logistics_rank)
            .unwrap_or(u32::MAX);
        (
            operation_priority_rank(op),
            logistics_rank,
            op.facts.enemy_contact_eta,
        )
    });
    // 要求（op.slots）は配分中に書き換えず、未充足の枠は理由付きで台帳へ記録する。
    let mut ledger = SlotLedger::new();
    if !allow_structural_slots {
        // 占領要員と輸送役は島嶼キャンペーン側で予約済み。余剰予算を同じ役割へ
        // 二重投入せず、観測済みの敵に対する迎撃・護衛・撃破だけへ使う。
        // ただし陸続きの前線はcampaign専用生産の対象外なので、別の渡洋作戦が
        // 同時に予約中でも構造枠を見送ってはならない。
        for (index, operation) in operations.iter().enumerate() {
            if operation.facts.requires_transport {
                ledger.close(index, SlotKind::Capture, UnfilledReason::CoveredByCampaign);
                ledger.close(
                    index,
                    SlotKind::Transport,
                    UnfilledReason::CoveredByCampaign,
                );
            }
        }
    }

    plan_trace.operations = operations
        .iter()
        .enumerate()
        .map(|(index, op)| ProductionOperationTrace {
            capture_replacement_pending: false,
            enemy_stress_reinforcement_funds: 0,
            kind: op.kind,
            anchor: op.anchor,
            slots: effective_slots(op, index, &ledger),
            requires_transport: op.facts.requires_transport,
            enemy_combat_units: op.facts.enemy_combat_units,
            enemy_reinforcement_funds: op.facts.enemy_reinforcement_funds,
            contingency_reserve_funds: op.contingency_reserve_funds,
            reinforcement_contingencies: op
                .reinforcement_contingencies
                .iter()
                .map(|contingency| ReinforcementContingencyTrace {
                    enemy_type: contingency.enemy_type,
                    enemy_contact_turn: contingency.enemy_contact_turn,
                    counter_type: contingency.counter_type,
                    counter_facility: contingency.counter_facility,
                    counter_build_turn: contingency.counter_build_turn,
                    counter_contact_turn: contingency.counter_contact_turn,
                    attacks_required: contingency.attacks_required,
                    reserve_cost: contingency.reserve_cost,
                })
                .collect(),
            deploy_lead_time: op.facts.deploy_lead_time,
        })
        .collect();

    ProductionState {
        ctx,
        operations,
        ledger,
        plan_trace,
        committed_by_operation: HashMap::default(),
        allocation: AllocationState::new(scan.funds),
        rolling_plans: HashMap::default(),
        seen_plan_ids: HashSet::default(),
        empty_committed_entities: HashSet::default(),
        commands: Vec::new(),
    }
}

fn assign_committed_combat(input: &ProductionInputs<'_>, state: &mut ProductionState) {
    let ProductionInputs {
        scan,
        committed_combat_assignments,
        ..
    } = *input;
    let ProductionState {
        operations,
        committed_by_operation,
        ..
    } = state;
    for (&entity, assignment) in committed_combat_assignments {
        let Some(unit) = scan
            .my_units
            .iter()
            .find(|unit| unit.entity == Some(entity))
        else {
            continue;
        };
        let assigned_operation = operations
            .iter()
            .enumerate()
            .filter(|(_, operation)| {
                operation.reachable_threats.iter().any(|threat| {
                    threat
                        .entity
                        .is_some_and(|enemy| assignment.targets.contains(&enemy))
                })
            })
            .min_by_key(|(_, operation)| {
                scan.map.distance(
                    unit.pos.x,
                    unit.pos.y,
                    operation.anchor.x,
                    operation.anchor.y,
                )
            })
            .map(|(index, _)| index);
        if let Some(index) = assigned_operation {
            committed_by_operation
                .entry(index)
                .or_default()
                .insert(entity, assignment.plan_id);
        }
    }
}

#[derive(Clone, Copy)]
struct SlotRequest {
    op_index: usize,
    slot_kind: SlotKind,
    spendable_funds: u32,
    per_slot_budget: u32,
    operation_kind: OperationKind,
    operation_anchor: GridPosition,
    deficit_before: f32,
    remaining_funds_before: u32,
}
fn allocate_operation_slots(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
    plan_registry: &mut V4RollingPlanRegistry,
) {
    loop {
        let ProductionInputs { scan, .. } = *input;
        let ProductionState {
            operations,
            ledger,
            allocation,
            ..
        } = state;
        let free_slots =
            allocation.free_facility_count(scan.free_facilities.iter().map(|(pos, _)| pos));
        if free_slots == 0 {
            break;
        }
        // 最も不足している枠を持つ作戦から順に見ていく
        let Some((op_index, slot_kind)) = most_starved_slot(operations, ledger) else {
            break;
        };

        // 先に並ぶ作戦の観測後counterを発動できる現金は、下位作戦へ流さない。
        // 自作戦の予約は現在の確定脅威を処理するために使ってよい。
        let higher_priority_contingency_reserve = operations[..op_index]
            .iter()
            .map(|operation| operation.contingency_reserve_funds)
            .fold(0_u32, u32::saturating_add);
        let spendable_funds = allocation.spendable_after(higher_priority_contingency_reserve);
        // 1 枠あたり予算。高価なユニットで枠を食い潰さないためのソフト上限。
        let per_slot_budget = spendable_funds / free_slots as u32;

        // トレース用に、選定前の未充足率と作戦の識別情報を控えておく。
        let operation_kind = operations[op_index].kind;
        let operation_anchor = operations[op_index].anchor;
        let deficit_before = operations[op_index]
            .slots
            .deficit_ratio(slot_kind, &operations[op_index].filled);
        let remaining_funds_before = allocation.remaining_funds();

        let request = SlotRequest {
            op_index,
            slot_kind,
            spendable_funds,
            per_slot_budget,
            operation_kind,
            operation_anchor,
            deficit_before,
            remaining_funds_before,
        };
        if evaluate_rolling_plan(input, state, plan_registry, request) {
            continue;
        }
        if !purchase_operation_slot(input, state, plan_registry, request) {
            break;
        }
    }
}
fn evaluate_rolling_plan(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
    plan_registry: &mut V4RollingPlanRegistry,
    request: SlotRequest,
) -> bool {
    let ProductionInputs {
        scan,
        player_id,
        allow_structural_slots,
        turn,
        ..
    } = *input;
    let ProductionState {
        ctx,
        operations,
        ledger,
        plan_trace,
        committed_by_operation,
        allocation,
        rolling_plans,
        seen_plan_ids,
        empty_committed_entities,
        ..
    } = state;
    let SlotRequest {
        op_index,
        slot_kind,
        spendable_funds,
        operation_kind,
        operation_anchor,
        ..
    } = request;
    if slot_kind == SlotKind::Combat && !rolling_plans.contains_key(&op_index) {
        let target_enemies = operations[op_index]
            .reachable_threats
            .iter()
            .filter_map(|threat| threat.entity)
            .collect::<HashSet<_>>();
        let continuation = plan_registry.continuation_for_operation(
            player_id,
            turn,
            operation_kind,
            operations[op_index].island_id,
            &operations[op_index].objective_properties,
            &target_enemies,
        );
        // 別PlanのEntityを「既存戦力」として見積もっても任務は移管されない。
        // 継続する同一Planの配属Entityだけを固定パッケージへ入力する。
        let continuation_plan_id = continuation.as_ref().map(|plan| plan.plan_id);
        let committed_for_plan = committed_entities_for_plan(
            committed_by_operation.get(&op_index),
            continuation_plan_id,
        );
        let rolling_input = combat_plan_input(
            scan,
            ctx,
            &operations[op_index],
            allocation.used_facilities(),
            if committed_for_plan.is_empty() {
                empty_committed_entities
            } else {
                &committed_for_plan
            },
            spendable_funds,
            !allow_structural_slots,
        );
        if let Some(input) = rolling_input {
            // 首都攻略の編成中は、固定購入列が今も実行可能ならその再評価だけを行う。
            // 継続すると分かっている案の全beam searchを先に実行して捨てない。
            let evaluated_continuation = continuation.map(|previous| {
                let evaluated = evaluate_fixed_package(&input, &previous.purchases);
                (previous, evaluated)
            });
            let reusable_candidate =
                evaluated_continuation
                    .as_ref()
                    .and_then(|(previous, evaluated)| {
                        previous
                            .can_reuse_without_search(
                                operation_kind,
                                operations[op_index].execution_authorized,
                            )
                            .then(|| evaluated.as_ref().ok().cloned())
                            .flatten()
                    });
            let Some(candidate_plan) = reusable_candidate.or_else(|| plan_force_package(&input))
            else {
                ledger.close(op_index, SlotKind::Combat, UnfilledReason::NoForcePackage);
                return true;
            };
            let conflicted_facilities = evaluated_continuation
                .as_ref()
                .map(|(previous, evaluated)| {
                    if evaluated.is_ok() {
                        return HashSet::default();
                    }
                    let mut due = previous
                        .purchases
                        .iter()
                        .filter(|purchase| purchase.build_turn == 0)
                        .copied()
                        .collect::<Vec<_>>();
                    due.sort_unstable_by_key(|purchase| {
                        (purchase.facility.y, purchase.facility.x, purchase.cost)
                    });
                    let mut affordable_funds = input.current_funds;
                    let mut conflicts = HashSet::default();
                    for purchase in due {
                        let claimed_by_higher_priority = allocation
                            .facility_owner(&purchase.facility)
                            .is_some_and(|owner| {
                                owner < operation_priority_rank(&operations[op_index])
                            });
                        let claimed_by_campaign = scan
                            .production_facilities
                            .iter()
                            .any(|(facility, _)| *facility == purchase.facility)
                            && !scan
                                .free_facilities
                                .iter()
                                .any(|(facility, _)| *facility == purchase.facility);
                        if claimed_by_higher_priority || claimed_by_campaign {
                            conflicts.insert(purchase.facility);
                        } else if operation_kind == OperationKind::AssaultCapital {
                            if purchase.cost <= affordable_funds {
                                affordable_funds = affordable_funds.saturating_sub(purchase.cost);
                            } else {
                                // 上位作戦が当手番の現金を使った場合も、失敗ではなく
                                // 当該施設の首都編成列を次手番以降へ繰り下げる。
                                conflicts.insert(purchase.facility);
                            }
                        }
                    }
                    conflicts
                })
                .unwrap_or_default();
            let selected = plan_registry.select_for_operation(
                player_id,
                turn,
                operation_kind,
                operations[op_index].island_id,
                operation_anchor,
                operations[op_index].objective_properties.clone(),
                target_enemies,
                operations[op_index].execution_authorized,
                evaluated_continuation,
                candidate_plan,
                input.hard_deadline,
                conflicted_facilities,
            );
            if let Some(plan_id) = selected.plan_id {
                seen_plan_ids.insert(plan_id);
            }
            let plan = &selected.plan;
            plan_trace.rolling_combat_plans.retain(|current| {
                current.operation_kind != operation_kind || current.anchor != operation_anchor
            });
            plan_trace
                .rolling_combat_plans
                .push(RollingCombatPlanTrace {
                    surviving_combat_value: 0,
                    required_overmatch_value: 0,
                    overmatch_ready: false,
                    candidates_pruned: 0,
                    plan_id: selected.plan_id,
                    revision: selected.revision,
                    disposition: selected.disposition,
                    replan_reason: selected.reason,
                    operation_kind,
                    anchor: operation_anchor,
                    feasible: plan.feasible,
                    purchases: plan
                        .purchases
                        .iter()
                        .copied()
                        .map(|purchase| RollingPurchaseTrace {
                            unit_type: purchase.unit_type,
                            facility: purchase.facility,
                            build_turn: purchase.build_turn,
                            cost: purchase.cost,
                        })
                        .collect(),
                    targets: plan
                        .target_forecasts
                        .iter()
                        .map(|target| RollingTargetTrace {
                            entity: target.entity,
                            unit_type: target.unit_type,
                            available_turn: target.available_turn,
                            initial_hp: target.initial_hp,
                            remaining_hp: target.remaining_hp,
                            destroyed_turn: target.destroyed_turn,
                        })
                        .collect(),
                    turn_forecasts: plan
                        .turn_forecasts
                        .iter()
                        .map(|forecast| CampaignTurnForecastTrace {
                            turn: forecast.turn,
                            enemy_arrival_hp: forecast.enemy_arrival_hp,
                            enemy_hp_removed: forecast.enemy_hp_removed,
                            friendly_hp_lost: forecast.friendly_hp_lost,
                            attack_count: forecast.attack_count,
                        })
                        .collect(),
                    first_attack_turn: plan.first_attack_turn,
                    elimination_turn: plan.elimination_turn,
                    occupation_turn: plan.occupation_turn,
                    production_cost: plan.production_cost,
                    expected_loss: plan.expected_loss,
                    protected_unit_count: plan.protected_unit_count,
                    protected_survivor_count: plan.protected_survivor_count,
                    required_capture_survivor_count: plan.required_capture_survivor_count,
                    candidates_considered: plan.candidates_considered,
                    search_truncated: plan.search_truncated,
                });
            if selected.plan_id.is_none() {
                // `NoFeasibleReplacement`は診断候補であって実行計画ではない。
                // 先頭の購入だけをPlanIdなしで発注すると、次手番に継続も撤回も
                // できず汎用任務へ流れるため、この作戦のCombat枠は実行しない。
                ledger.close(op_index, SlotKind::Combat, UnfilledReason::NoExecutablePlan);
                return true;
            }
            rolling_plans.insert(op_index, selected);
        }
    }
    false
}

fn purchase_operation_slot(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
    plan_registry: &mut V4RollingPlanRegistry,
    request: SlotRequest,
) -> bool {
    let ProductionInputs {
        scan,
        player_id,
        turn,
        ..
    } = *input;
    let ProductionState {
        ctx,
        operations,
        ledger,
        plan_trace,
        allocation,
        rolling_plans,
        commands,
        ..
    } = state;
    let SlotRequest {
        op_index,
        slot_kind,
        spendable_funds,
        per_slot_budget,
        operation_kind,
        operation_anchor,
        deficit_before,
        remaining_funds_before,
    } = request;
    let rolling_plan = rolling_plans.get(&op_index);
    let mut planned_purchase = None;
    let candidate = if slot_kind == SlotKind::Combat {
        rolling_plan.and_then(|plan| {
            plan.plan
                .current_purchases()
                .find(|purchase| {
                    !allocation.is_facility_used(&purchase.facility)
                        && purchase.cost <= spendable_funds
                })
                .map(|purchase| {
                    planned_purchase = Some(purchase);
                    SlotCandidate {
                        unit_type: purchase.unit_type,
                        cost: purchase.cost,
                        facility: purchase.facility,
                        fitness: 1.0,
                    }
                })
        })
    } else {
        select_candidate(
            scan,
            ctx,
            &operations[op_index],
            slot_kind,
            allocation.used_facilities(),
            CandidateConstraints {
                remaining_funds: spendable_funds,
                per_slot_budget,
            },
        )
    };

    let Some(candidate) = candidate else {
        // この枠を満たせる候補が無い場合は、要求を残したまま当手番は見送り、次を探す
        ledger.close(op_index, slot_kind, UnfilledReason::NoCandidate);
        let reserved = (slot_kind == SlotKind::Combat)
            .then_some(rolling_plan)
            .flatten()
            .filter(|plan| plan.plan_id.is_some())
            .and_then(|plan| {
                plan.plan
                    .purchases
                    .iter()
                    .filter(|purchase| purchase.build_turn > 0)
                    .min_by_key(|purchase| purchase.build_turn)
            });
        plan_trace.steps.push(ProductionStepTrace {
            operation_kind,
            operation_anchor,
            slot_kind,
            deficit_before,
            deficit_after: deficit_before,
            remaining_funds_before,
            decision: reserved.map_or(
                ProductionDecision::Unfilled {
                    reason: UnfilledReason::NoCandidate,
                },
                |purchase| ProductionDecision::Reserved {
                    unit_type: purchase.unit_type,
                    cost: purchase.cost,
                    build_turn: purchase.build_turn,
                },
            ),
        });
        return true;
    };

    // 見送り購入: 一括編成が必要な作戦で、今買える範囲に適合候補が無く、
    // 数ターン待てばより適合する候補が買えるなら、資金を貯める。
    if slot_kind != SlotKind::Combat
        && should_defer_purchase(
            scan,
            ctx,
            &operations[op_index],
            slot_kind,
            spendable_funds,
            candidate.cost,
        )
    {
        plan_trace.steps.push(ProductionStepTrace {
            operation_kind,
            operation_anchor,
            slot_kind,
            deficit_before,
            deficit_after: deficit_before,
            remaining_funds_before,
            decision: ProductionDecision::Deferred {
                unit_type: candidate.unit_type,
                cost: candidate.cost,
            },
        });
        return false;
    }

    if allocation
        .try_claim(
            candidate.facility,
            candidate.cost,
            operation_priority_rank(&operations[op_index]),
        )
        .is_err()
    {
        // 候補評価後に競合した予約は、後続の意図や工程へ反映せず配分を止める。
        return false;
    }
    let mut deployment = planned_deployment(
        scan,
        ctx,
        &operations[op_index],
        slot_kind,
        &candidate,
        None,
    );
    if slot_kind == SlotKind::Combat
        && let (Some(deployment), Some(plan)) = (deployment.as_mut(), rolling_plan)
    {
        deployment.forecast = deployment::DeploymentForecast {
            first_attack_turn: plan.plan.first_attack_turn,
            elimination_turn: plan.plan.elimination_turn,
            occupation_turn: plan.plan.occupation_turn,
            package_cost: plan.plan.production_cost,
            package_size: u32::try_from(plan.plan.purchases.len()).unwrap_or(u32::MAX),
        };
        deployment.plan_step = plan
            .plan_id
            .zip(plan.revision)
            .zip(planned_purchase)
            .and_then(|((plan_id, revision), purchase)| {
                plan_registry.current_step_ref(plan_id, revision, turn, purchase)
            });
    }
    // Combatは同じパッケージの未使用current purchaseを次の反復で選ぶ。
    // 全て消費した後は候補なしとなり、この手番のCombat枠を完了する。
    if slot_kind != SlotKind::Combat {
        record_fill(scan, ctx, &mut operations[op_index], slot_kind, &candidate);
    }
    plan_trace.steps.push(ProductionStepTrace {
        operation_kind,
        operation_anchor,
        slot_kind,
        deficit_before,
        // 購入を反映した後の未充足率。ここが下がらない枠が同一ユニットを買い続ける。
        deficit_after: operations[op_index]
            .slots
            .deficit_ratio(slot_kind, &operations[op_index].filled),
        remaining_funds_before,
        decision: ProductionDecision::Produced {
            unit_type: candidate.unit_type,
            cost: candidate.cost,
            facility: candidate.facility,
        },
    });
    let purpose = plan_step_purpose(deployment.as_ref(), PurchasePurpose::OperationSlot);
    commands.push(PlannedProduction {
        capture_intent: None,
        command: ProduceUnitCommand {
            player_id,
            target_x: candidate.facility.x,
            target_y: candidate.facility.y,
            unit_type: candidate.unit_type,
        },
        role: slot_kind,
        cost: candidate.cost,
        purpose,
        deployment,
    });
    true
}

fn finalize_turn_production(
    input: &ProductionInputs<'_>,
    state: ProductionState,
    plan_registry: &mut V4RollingPlanRegistry,
) -> (Vec<PlannedProduction>, ProductionPlanTrace) {
    let ProductionInputs {
        scan,
        player_id,
        turn,
        ..
    } = *input;
    let ProductionState {
        operations,
        ledger,
        mut plan_trace,
        allocation,
        seen_plan_ids,
        mut commands,
        ..
    } = state;
    // 通常経路は施設を交換しない。同じ確定口で工程・意図・診断の投影だけを揃える。
    production_finalize::finalize(scan, &mut commands, &mut plan_trace, plan_registry, false);
    plan_registry.reconcile_unseen_plans(player_id, turn, &seen_plan_ids);
    let remaining_funds = allocation.remaining_funds();
    plan_trace.leftover_funds = remaining_funds;
    plan_trace.unfilled_slots = unfilled_slot_traces(&operations, &ledger);
    plan_trace.demands = production_demand_traces(&operations, &ledger);
    let contingency_reserve = operations
        .iter()
        .map(|operation| operation.contingency_reserve_funds)
        .fold(0_u32, u32::saturating_add);
    plan_trace.reserved_funds = remaining_funds.min(
        plan_registry
            .reserved_purchase_cost(player_id)
            .saturating_add(contingency_reserve),
    );
    plan_trace.uncommitted_funds = remaining_funds.saturating_sub(plan_trace.reserved_funds);
    (commands, plan_trace)
}
