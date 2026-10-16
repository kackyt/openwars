//! 最不足枠の選択、継続計画の評価、1枠の発注。

use super::*;
pub(super) fn allocate_operation_slots(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
    plan_registry: &mut V4RollingPlanRegistry,
    contract_registry: &mut plan_contract::PlanContractRegistry,
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
        let Some((op_index, slot_kind)) = most_starved_slot(operations, ledger, input.policy)
        else {
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
        let deficit_before = input
            .policy
            .allocation_slots(&operations[op_index], op_index, ledger)
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
        if evaluate_rolling_plan(input, state, plan_registry, contract_registry, request) {
            continue;
        }
        if !purchase_operation_slot(input, state, plan_registry, contract_registry, request) {
            break;
        }
    }
}

pub(super) fn evaluate_rolling_plan(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
    plan_registry: &mut V4RollingPlanRegistry,
    _contract_registry: &mut plan_contract::PlanContractRegistry,
    request: SlotRequest,
) -> bool {
    let ProductionInputs {
        scan,
        player_id,
        allow_structural_slots,
        advancing_route_entities,
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
    if slot_kind != SlotKind::Combat || rolling_plans.contains_key(&op_index) {
        return false;
    }
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
    // 既存戦力は、PlanIdが一致する発注済み部隊だけでなく、親戦役DAGの
    // 未完了区間へ前進中の部隊から導く。ただし首都作戦と「次の区間を確保する」
    // Captureが同じ戦力を同時に数えることはない。前段では区間目標に一致する
    // Captureだけが受け取り、首都戦闘へ移るのは実行許可後である。
    // 別のPlan由来の部隊は借りず、即時Combat増援は実在敵への任務を持つ場合だけ
    // 入れるため、防衛・輸送・回復部隊を見かけの首都戦力にしない。
    let continuation_plan_id = continuation.as_ref().map(|plan| plan.plan_id);
    let mut committed_for_plan = committed_entities_for_plan(
        committed_by_operation.get(&op_index),
        continuation_plan_id,
        input.policy,
    );
    let staged_for_plan = include_route_force(
        input.policy,
        advancing_route_entities,
        &operations[op_index],
        &mut committed_for_plan,
    );
    let rolling_input = combat_input::combat_plan_input(
        scan,
        ctx,
        &operations[op_index],
        allocation.used_facilities(),
        if committed_for_plan.is_empty() {
            empty_committed_entities
        } else {
            &committed_for_plan
        },
        &staged_for_plan,
        spendable_funds,
        !allow_structural_slots,
        player_id,
        input.policy,
    );
    let Some(combat_input) = rolling_input else {
        return false;
    };
    let Some(selected) = choose_rolling_plan(
        input,
        &operations[op_index],
        &combat_input,
        continuation,
        target_enemies,
        allocation,
        plan_registry,
    ) else {
        ledger.close(op_index, SlotKind::Combat, UnfilledReason::NoForcePackage);
        return true;
    };
    if let Some(plan_id) = selected.plan_id {
        seen_plan_ids.insert(plan_id);
    }
    if combat_input.exact_property_control {
        // 物件数から歩兵数を先決めしない。施設・資金・敵相性を同時に列挙して
        // 選ばれた支配パッケージのCapture割当だけを、この手番の必要数とする。
        // 元の要求を保持し、従来の上書き量を実効要求として台帳へ記録する。
        // 以前の実効要求に対する未充足の取消タイミングは変えない。
        ledger.set_effective(
            op_index,
            SlotKind::Capture,
            u32::try_from(selected.plan.capture_purchases.len()).unwrap_or(u32::MAX),
            EffectiveDemandReason::JointPackageSelected,
        );
        ledger.reopen(op_index, SlotKind::Capture);
    }
    record_rolling_plan_trace(
        plan_trace,
        &selected,
        operation_kind,
        operation_anchor,
        input.policy,
    );
    if selected.plan_id.is_none() {
        if input.policy.issues_best_effort() {
            rolling_plans.insert(op_index, selected);
        } else {
            ledger.close(op_index, SlotKind::Combat, UnfilledReason::NoExecutablePlan);
        }
        return true;
    }
    rolling_plans.insert(op_index, selected);
    false
}

pub(super) fn purchase_operation_slot(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
    plan_registry: &mut V4RollingPlanRegistry,
    contract_registry: &mut plan_contract::PlanContractRegistry,
    request: SlotRequest,
) -> bool {
    let ProductionInputs {
        scan, player_id, ..
    } = *input;
    let ProductionState {
        ctx,
        operations,
        ledger,
        plan_trace,
        allocation,
        rolling_plans,
        ..
    } = state;
    let SlotRequest {
        op_index,
        slot_kind,
        spendable_funds,
        operation_kind,
        operation_anchor,
        deficit_before,
        remaining_funds_before,
        ..
    } = request;
    let rolling_plan = rolling_plans.get(&op_index);
    let (candidate, planned_purchase) = select_slot_purchase(
        input,
        ctx,
        &operations[op_index],
        allocation,
        rolling_plan,
        request,
    );
    let Some(candidate) = candidate else {
        record_missing_slot(ledger, plan_trace, request, rolling_plan);
        return true;
    };

    // 見送り購入: 一括編成が必要な作戦で、今買える範囲に適合候補が無く、
    // 数ターン待てばより適合する候補が買えるなら、資金を貯める。
    if slot_kind != SlotKind::Combat
        && input.policy.allows_purchase_deferral(&operations[op_index])
        && input.policy.should_defer_purchase(
            scan,
            ctx,
            &operations[op_index],
            slot_kind,
            spendable_funds,
            candidate.cost,
            player_id,
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

    let joint_capture_target = (input.policy.uses_joint_capture() && slot_kind == SlotKind::Combat)
        .then_some(rolling_plan)
        .flatten()
        .zip(planned_purchase)
        .and_then(|(plan, purchase)| plan.plan.capture_target_for(purchase));
    // 共同計画のCapture割当を優先し、通常Capture枠だけ従来の次レーンを使う。
    let capture_mission_target = joint_capture_target.or_else(|| {
        (input.policy.uses_joint_capture() && slot_kind == SlotKind::Capture)
            .then(|| next_capture_target(&operations[op_index]))
    });
    let effective_slot_kind = if capture_mission_target.is_some() {
        SlotKind::Capture
    } else {
        slot_kind
    };
    let planned_combat_target = (input.policy.uses_joint_capture()
        && effective_slot_kind == SlotKind::Combat)
        .then_some(rolling_plan)
        .flatten()
        .zip(planned_purchase)
        .and_then(|(plan, purchase)| plan.plan.combat_target_for(purchase));
    commit_slot_order(
        input,
        state,
        plan_registry,
        contract_registry,
        SlotOrder {
            request,
            candidate,
            planned_purchase,
            effective_slot_kind,
            capture_mission_target,
            planned_combat_target,
        },
    )
}

fn include_route_force(
    policy: ProductionPolicy,
    advancing_route_entities: &AdvancingCapitalRouteEntities,
    operation: &Operation,
    committed_for_plan: &mut HashSet<Entity>,
) -> HashSet<Entity> {
    let operation_kind = operation.kind;
    // Stageは前衛ではないが、Roadmapが同じDAG区間へ予約した実Entityである。
    // 到着予定付きの既存戦力としてRollingPlanへ渡し、人数だけを根拠に
    // 追加生産を禁止する第二の判定器にはしない。
    let mut staged_for_plan = HashSet::default();
    if policy.uses_route_commitments()
        && let Some(island_id) = operation.island_id
    {
        match operation_kind {
            OperationKind::Capture => {
                for objective in &operation.objective_properties {
                    if let Some(route_entities) = advancing_route_entities
                        .by_target
                        .get(&(island_id, *objective))
                    {
                        committed_for_plan.extend(route_entities.iter().copied());
                    }
                    if let Some(route_entities) = advancing_route_entities
                        .staged_by_target
                        .get(&(island_id, *objective))
                    {
                        staged_for_plan.extend(route_entities.iter().copied());
                    }
                }
            }
            OperationKind::AssaultCapital if operation.execution_authorized => {
                if let Some(route_entities) = advancing_route_entities.by_island.get(&island_id) {
                    committed_for_plan.extend(route_entities.iter().copied());
                }
                for ((staged_island, _), route_entities) in
                    &advancing_route_entities.staged_by_target
                {
                    if *staged_island == island_id {
                        staged_for_plan.extend(route_entities.iter().copied());
                    }
                }
            }
            OperationKind::Defense | OperationKind::AssaultCapital => {}
        }
    }
    staged_for_plan
}

fn record_rolling_plan_trace(
    plan_trace: &mut ProductionPlanTrace,
    selected: &SelectedPlan,
    operation_kind: OperationKind,
    operation_anchor: GridPosition,
    policy: ProductionPolicy,
) {
    let plan = &selected.plan;
    plan_trace.rolling_combat_plans.retain(|current| {
        current.operation_kind != operation_kind || current.anchor != operation_anchor
    });
    plan_trace
        .rolling_combat_plans
        .push(RollingCombatPlanTrace {
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
            surviving_combat_value: if policy.reports_property_control() {
                plan.surviving_combat_value
            } else {
                0
            },
            required_overmatch_value: if policy.reports_property_control() {
                plan.required_overmatch_value
            } else {
                0
            },
            overmatch_ready: policy.reports_property_control() && plan.overmatch_ready,
            protected_unit_count: plan.protected_unit_count,
            protected_survivor_count: plan.protected_survivor_count,
            required_capture_survivor_count: plan.required_capture_survivor_count,
            candidates_considered: plan.candidates_considered,
            candidates_pruned: if policy.reports_property_control() {
                plan.candidates_pruned
            } else {
                0
            },
            search_truncated: plan.search_truncated,
        });
}

fn conflicted_facilities_for_plan(
    scan: &BoardScan,
    allocation: &AllocationState,
    operation: &Operation,
    previous: &plan_revision::PlanContinuation,
    current_funds: u32,
    policy: ProductionPolicy,
) -> HashSet<GridPosition> {
    let operation_kind = operation.kind;
    let mut due = previous
        .purchases
        .iter()
        .filter(|purchase| purchase.build_turn == 0)
        .copied()
        .collect::<Vec<_>>();
    due.sort_unstable_by_key(|purchase| (purchase.facility.y, purchase.facility.x, purchase.cost));
    let mut affordable_funds = current_funds;
    let mut conflicts = HashSet::default();
    for purchase in due {
        let claimed_by_higher_priority = allocation
            .facility_owner(&purchase.facility)
            .is_some_and(|owner| owner < operation_priority_rank(operation, policy));
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
}

/// 選んだ1枠の役割と、同じ購入が参照する工程。Entity化前の一時値。
struct SlotPurchase<'a> {
    kind: SlotKind,
    capture_target: Option<GridPosition>,
    combat_target: Option<Entity>,
    plan: Option<&'a SelectedPlan>,
    purchase: Option<rolling_plan::PlannedPurchase>,
}
#[allow(clippy::too_many_arguments)]
fn deployment_for_slot_purchase(
    input: &ProductionInputs<'_>,
    ctx: &mut ReachCtx,
    operation: &Operation,
    candidate: &SlotCandidate,
    purchase: SlotPurchase<'_>,
    plan_registry: &V4RollingPlanRegistry,
    contract_registry: &mut plan_contract::PlanContractRegistry,
) -> Option<PlannedDeployment> {
    let scan = input.scan;
    let turn = input.turn;
    let SlotPurchase {
        kind: effective_slot_kind,
        capture_target: capture_mission_target,
        combat_target: planned_combat_target,
        plan: rolling_plan,
        purchase: planned_purchase,
    } = purchase;
    let mut deployment =
        if input.policy.uses_joint_capture() && effective_slot_kind == SlotKind::Capture {
            let target = capture_mission_target.unwrap_or(operation.anchor);
            Some(planned_capture_deployment(operation, target))
        } else {
            planned_deployment(
                scan,
                ctx,
                operation,
                effective_slot_kind,
                candidate,
                planned_combat_target,
            )
        };
    if effective_slot_kind == SlotKind::Combat
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
        if let (Some(plan_id), Some(op_id)) = (plan.plan_id, operation.operation_id) {
            contract_registry.link_plan_to_operation(plan_id, op_id);
        }
    }
    if let Some(op_id) = operation.operation_id
        && let Some(deployment) = deployment.as_mut()
    {
        let role = match effective_slot_kind {
            SlotKind::Capture => victory_roadmap::OperationEntityRole::Capture,
            SlotKind::Transport => victory_roadmap::OperationEntityRole::Transport,
            SlotKind::Combat | SlotKind::Intercept => victory_roadmap::OperationEntityRole::Combat,
        };
        let revision = contract_registry
            .contract_revision(op_id)
            .unwrap_or(plan_revision::PlanRevision(1));
        deployment.operation_binding = Some(plan_contract::OperationBinding {
            operation_id: op_id,
            plan_step: deployment.plan_step,
            role,
            revision,
        });
    }
    deployment
}

struct SlotOrder {
    request: SlotRequest,
    candidate: SlotCandidate,
    planned_purchase: Option<rolling_plan::PlannedPurchase>,
    effective_slot_kind: SlotKind,
    capture_mission_target: Option<GridPosition>,
    planned_combat_target: Option<Entity>,
}

fn commit_slot_order(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
    plan_registry: &mut V4RollingPlanRegistry,
    contract_registry: &mut plan_contract::PlanContractRegistry,
    order: SlotOrder,
) -> bool {
    let ProductionState {
        ctx,
        operations,
        allocation,
        rolling_plans,
        issued_plan_purchase_cost,
        ..
    } = state;
    let order = &order;
    let SlotOrder {
        request,
        candidate,
        planned_purchase,
        effective_slot_kind,
        capture_mission_target,
        planned_combat_target,
    } = *order;
    let SlotRequest { op_index, .. } = request;
    let rolling_plan = rolling_plans.get(&op_index);
    if allocation
        .try_claim(
            candidate.facility,
            candidate.cost,
            operation_priority_rank(&operations[op_index], input.policy),
        )
        .is_err()
    {
        // 候補評価後に競合した予約は、後続の意図や工程へ反映せず配分を止める。
        return false;
    }
    if planned_purchase.is_some() {
        *issued_plan_purchase_cost = issued_plan_purchase_cost.saturating_add(candidate.cost);
    }
    let deployment = deployment_for_slot_purchase(
        input,
        ctx,
        &operations[op_index],
        &candidate,
        SlotPurchase {
            kind: effective_slot_kind,
            capture_target: capture_mission_target,
            combat_target: planned_combat_target,
            plan: rolling_plan,
            purchase: planned_purchase,
        },
        plan_registry,
        contract_registry,
    );
    record_slot_order(input, state, order, deployment);
    true
}
fn record_slot_order(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
    order: &SlotOrder,
    deployment: Option<PlannedDeployment>,
) {
    let ProductionInputs {
        scan, player_id, ..
    } = *input;
    let ProductionState {
        ctx,
        operations,
        ledger,
        plan_trace,
        commands,
        ..
    } = state;
    let SlotOrder {
        request,
        candidate,
        effective_slot_kind,
        capture_mission_target,
        ..
    } = *order;
    let SlotRequest {
        op_index,
        operation_kind,
        operation_anchor,
        deficit_before,
        remaining_funds_before,
        ..
    } = request;
    // Combatは同じパッケージの未使用current purchaseを次の反復で選ぶ。
    // 全て消費した後は候補なしとなり、この手番のCombat枠を完了する。
    if input.policy.uses_joint_capture() && effective_slot_kind == SlotKind::Capture {
        let target = capture_mission_target.expect("Capture購入には任務先がある");
        operations[op_index]
            .planned_capture_units
            .push(PlannedCaptureUnit {
                stats: candidate_stats(scan, &candidate).clone(),
                facility: candidate.facility,
                target,
            });
        operations[op_index].filled.capture_units =
            operations[op_index].filled.capture_units.saturating_add(1);
    } else if effective_slot_kind != SlotKind::Combat {
        record_fill(
            scan,
            ctx,
            &mut operations[op_index],
            effective_slot_kind,
            &candidate,
        );
    }
    plan_trace.steps.push(ProductionStepTrace {
        operation_kind,
        operation_anchor,
        slot_kind: effective_slot_kind,
        deficit_before,
        // 購入を反映した後の未充足率。ここが下がらない枠が同一ユニットを買い続ける。
        deficit_after: input
            .policy
            .allocation_slots(&operations[op_index], op_index, ledger)
            .deficit_ratio(effective_slot_kind, &operations[op_index].filled),
        remaining_funds_before,
        decision: ProductionDecision::Produced {
            unit_type: candidate.unit_type,
            cost: candidate.cost,
            facility: candidate.facility,
        },
    });
    let capture_intent = (input.policy.uses_joint_capture()
        && effective_slot_kind == SlotKind::Capture)
        .then(|| {
            operations[op_index].island_id.map(|island_id| {
                campaign_execution::CampaignProductionIntent {
                    command: ProduceUnitCommand {
                        player_id,
                        target_x: candidate.facility.x,
                        target_y: candidate.facility.y,
                        unit_type: candidate.unit_type,
                    },
                    island_id,
                    role: campaign_execution::CampaignProductionRole::Capture,
                    mission_target: capture_mission_target,
                }
            })
        })
        .flatten();
    let purpose = plan_step_purpose(deployment.as_ref(), PurchasePurpose::OperationSlot);
    commands.push(PlannedProduction {
        command: ProduceUnitCommand {
            player_id,
            target_x: candidate.facility.x,
            target_y: candidate.facility.y,
            unit_type: candidate.unit_type,
        },
        role: effective_slot_kind,
        cost: candidate.cost,
        purpose,
        deployment,
        capture_intent,
    });
}

fn select_slot_purchase(
    input: &ProductionInputs<'_>,
    ctx: &mut ReachCtx,
    operation: &Operation,
    allocation: &AllocationState,
    rolling_plan: Option<&SelectedPlan>,
    request: SlotRequest,
) -> (Option<SlotCandidate>, Option<rolling_plan::PlannedPurchase>) {
    let ProductionInputs {
        scan, player_id, ..
    } = *input;
    let SlotRequest {
        slot_kind,
        spendable_funds,
        per_slot_budget,
        ..
    } = request;
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
        input.policy.select_candidate(
            scan,
            ctx,
            operation,
            slot_kind,
            allocation.used_facilities(),
            CandidateConstraints {
                remaining_funds: spendable_funds,
                per_slot_budget,
            },
            player_id,
        )
    };

    (candidate, planned_purchase)
}

fn record_missing_slot(
    ledger: &mut SlotLedger,
    plan_trace: &mut ProductionPlanTrace,
    request: SlotRequest,
    rolling_plan: Option<&SelectedPlan>,
) {
    let SlotRequest {
        op_index,
        slot_kind,
        operation_kind,
        operation_anchor,
        deficit_before,
        remaining_funds_before,
        ..
    } = request;
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
}

fn choose_rolling_plan(
    input: &ProductionInputs<'_>,
    operation: &Operation,
    combat_input: &RollingPlanInput,
    continuation: Option<plan_revision::PlanContinuation>,
    target_enemies: HashSet<Entity>,
    allocation: &AllocationState,
    plan_registry: &mut V4RollingPlanRegistry,
) -> Option<SelectedPlan> {
    let ProductionInputs {
        scan,
        player_id,
        turn,
        ..
    } = *input;
    let operation_kind = operation.kind;
    let operation_anchor = operation.anchor;
    // 首都攻略の編成中は、固定購入列が今も実行可能ならその再評価だけを行う。
    // 継続すると分かっている案の全beam searchを先に実行して捨てない。
    // ただし物件制御の前線作戦では毎ターンの動的最新盤面から合同探索を行う。
    let evaluated_continuation = if combat_input.exact_property_control {
        None
    } else {
        continuation.map(|previous| {
            let evaluated = evaluate_fixed_package(combat_input, &previous.purchases);
            (previous, evaluated)
        })
    };
    let reusable_candidate = evaluated_continuation
        .as_ref()
        .and_then(|(previous, evaluated)| {
            previous
                .can_reuse_without_search(operation_kind, operation.execution_authorized)
                .then(|| evaluated.as_ref().ok().cloned())
                .flatten()
        });
    let candidate_plan = reusable_candidate.or_else(|| plan_force_package(combat_input))?;
    let conflicted_facilities = evaluated_continuation
        .as_ref()
        .map(|(previous, evaluated)| {
            if evaluated.is_ok() {
                return HashSet::default();
            }
            conflicted_facilities_for_plan(
                scan,
                allocation,
                operation,
                previous,
                combat_input.current_funds,
                input.policy,
            )
        })
        .unwrap_or_default();
    let selected = plan_registry.select_for_operation(
        player_id,
        turn,
        operation_kind,
        operation.island_id,
        operation_anchor,
        operation.objective_properties.clone(),
        target_enemies,
        operation.execution_authorized,
        evaluated_continuation,
        candidate_plan,
        combat_input.hard_deadline,
        conflicted_facilities,
    );
    Some(selected)
}
