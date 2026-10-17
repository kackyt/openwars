//! 生産の準備、既存戦力の帰属、枠配分、即時増援、余剰動員、最終確定。

use crate::ai::v4::capital_route::model::AdvancingCapitalRouteEntities;
use crate::ai::v4::property_control::PlannedCaptureUnit;
use crate::ai::v4::property_control::next_capture_target;
use crate::ai::v4::*;

#[allow(unused_imports)]
use crate::ai::planning::decision::AdoptedTurnPortfolio;

#[derive(Clone, Copy)]
pub(in crate::ai::v4) struct ProductionInputs<'a> {
    pub(in crate::ai::v4) policy: ProductionPolicy,
    pub(in crate::ai::v4) scan: &'a BoardScan,
    pub(in crate::ai::v4) player_id: PlayerId,
    pub(in crate::ai::v4) allow_structural_slots: bool,
    pub(in crate::ai::v4) committed_combat_assignments:
        &'a HashMap<Entity, deployment::ActiveTargetAssignment>,
    pub(in crate::ai::v4) advancing_route_entities: &'a AdvancingCapitalRouteEntities,
    pub(in crate::ai::v4) turn: u32,
}
/// 今回の配分だけで共有する作業状態。永続的な戦略状態はRegistryが所有する。
struct ProductionState {
    ctx: ReachCtx,
    operations: Vec<Operation>,
    ledger: SlotLedger,
    plan_trace: ProductionPlanTrace,
    committed_by_operation: HashMap<usize, HashMap<Entity, deployment::ActiveTargetAssignment>>,
    allocation: AllocationState,
    rolling_plans: HashMap<usize, SelectedPlan>,
    seen_plan_ids: HashSet<plan_revision::PlanId>,
    empty_committed_entities: HashSet<Entity>,
    commands: Vec<PlannedProduction>,
    issued_plan_purchase_cost: u32,
}
pub(in crate::ai::v4) fn plan_production_with_registry(
    input: &ProductionInputs<'_>,
    plan_registry: &mut V4RollingPlanRegistry,
    contract_registry: &mut plan_contract::PlanContractRegistry,
) -> (Vec<PlannedProduction>, ProductionPlanTrace) {
    let mut state = prepare_operations(input, plan_registry, contract_registry);
    if state.operations.is_empty() {
        return (state.commands, state.plan_trace);
    }
    assign_committed_combat(input, &mut state);
    allocate_operation_slots(input, &mut state, plan_registry, contract_registry);
    let surplus = if input.policy.allocates_surplus() {
        let budget = allocate_immediate_reinforcements(input, &mut state, plan_registry);
        allocate_surplus_mobilization(input, &mut state, &budget);
        Some(budget)
    } else {
        None
    };
    finalize_turn_production(input, state, plan_registry, surplus)
}

fn prepare_operations(
    input: &ProductionInputs<'_>,
    plan_registry: &V4RollingPlanRegistry,
    contract_registry: &mut plan_contract::PlanContractRegistry,
) -> ProductionState {
    let ProductionInputs {
        scan,
        player_id,
        allow_structural_slots,
        ..
    } = *input;

    let mut ctx = ReachCtx::default();
    let active_objectives = plan_registry.active_objectives(player_id);
    let mut operations = operation_builder::build_operations(
        scan,
        &mut ctx,
        &active_objectives,
        player_id,
        input.policy,
    );

    input
        .policy
        .synchronize_contracts(scan, &mut operations, contract_registry);
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
            operation_priority_rank(op, input.policy),
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
    input
        .policy
        .reserve_capital_structure(&operations, &mut ledger);

    plan_trace.operations = operations
        .iter()
        .enumerate()
        .map(|(index, op)| production_operation_trace(op, index, &ledger, input.policy))
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
        issued_plan_purchase_cost: 0,
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
                .insert(entity, assignment.clone());
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

struct SurplusBudget {
    funds: u32,
    unissued_plan_reserve: u32,
    contingency_reserve: u32,
    resolved: HashSet<Entity>,
}

fn finalize_turn_production(
    input: &ProductionInputs<'_>,
    state: ProductionState,
    plan_registry: &mut V4RollingPlanRegistry,
    surplus: Option<SurplusBudget>,
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
    production_finalize::finalize(
        scan,
        &mut commands,
        &mut plan_trace,
        plan_registry,
        input.policy.optimizes_facilities(),
    );

    plan_registry.reconcile_unseen_plans(player_id, turn, &seen_plan_ids);
    let remaining_funds = allocation.remaining_funds();
    plan_trace.leftover_funds = remaining_funds;
    plan_trace.unfilled_slots = unfilled_slot_traces(&operations, &ledger);
    plan_trace.demands = production_demand_traces(&operations, &ledger);
    plan_trace.reserved_funds = remaining_funds.min(surplus.map_or_else(
        || {
            let contingency_reserve = operations
                .iter()
                .map(|operation| operation.contingency_reserve_funds)
                .fold(0_u32, u32::saturating_add);
            plan_registry
                .reserved_purchase_cost(player_id)
                .saturating_add(contingency_reserve)
        },
        |budget| {
            budget
                .unissued_plan_reserve
                .saturating_add(budget.contingency_reserve)
        },
    ));
    plan_trace.uncommitted_funds = remaining_funds.saturating_sub(plan_trace.reserved_funds);
    (commands, plan_trace)
}

#[cfg(test)]
pub(in crate::ai::v4) mod test_exports {
    pub(in crate::ai::v4) use super::immediate_combat::{
        ImmediateCombatEngagement, ImmediateCombatOption, immediate_combat_options,
        select_immediate_combat_reinforcement,
    };
    pub(in crate::ai::v4) use super::mobilization::mobilization_role;
    pub(in crate::ai::v4) use crate::ai::v4::candidate::committed_entities_for_plan;
    pub(in crate::ai::v4) use crate::ai::v4::combat_input::combat_plan_input;
    pub(in crate::ai::v4) use crate::ai::v4::operation_builder::{
        build_operation, build_operations, enemy_reinforcement_assessment,
    };
    pub(in crate::ai::v4) use crate::ai::v4::slot_demand::defer_unapproved_capital_structure_for_active_segment;
}
#[cfg(test)]
pub(in crate::ai::v4) fn plan_production(
    scan: &BoardScan,
    player_id: PlayerId,
    allow_structural_slots: bool,
    committed_combat_assignments: &HashMap<Entity, deployment::ActiveTargetAssignment>,
) -> (Vec<PlannedProduction>, ProductionPlanTrace) {
    let route = AdvancingCapitalRouteEntities::default();
    let input = ProductionInputs {
        policy: ProductionPolicy::Standard,
        scan,
        player_id,
        allow_structural_slots,
        committed_combat_assignments,
        advancing_route_entities: &route,
        turn: 0,
    };
    plan_production_with_registry(
        &input,
        &mut V4RollingPlanRegistry::default(),
        &mut plan_contract::PlanContractRegistry::default(),
    )
}

mod slot_allocation;
use slot_allocation::*;

mod immediate_combat;
use immediate_combat::*;

mod mobilization;
use mobilization::*;

fn production_operation_trace(
    op: &Operation,
    index: usize,
    ledger: &SlotLedger,
    policy: ProductionPolicy,
) -> ProductionOperationTrace {
    ProductionOperationTrace {
        kind: op.kind,
        anchor: op.anchor,
        slots: effective_slots(op, index, ledger),
        capture_replacement_pending: policy.reports_property_control()
            && op.capture_replacement_pending,
        requires_transport: op.facts.requires_transport,
        enemy_combat_units: op.facts.enemy_combat_units,
        enemy_reinforcement_funds: op.facts.enemy_reinforcement_funds,
        enemy_stress_reinforcement_funds: policy.stress_funds(op),
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
    }
}

struct SurplusPurchase {
    candidate: SlotCandidate,
    role: SlotKind,
    purpose: PurchasePurpose<PlanStepRef>,
    deployment: Option<PlannedDeployment>,
    remaining_funds_before: u32,
}
fn record_surplus_purchase(
    operation: &Operation,
    player_id: PlayerId,
    plan_trace: &mut ProductionPlanTrace,
    commands: &mut Vec<PlannedProduction>,
    purchase: SurplusPurchase,
) {
    let SurplusPurchase {
        candidate,
        role,
        purpose,
        deployment,
        remaining_funds_before,
    } = purchase;
    let operation_kind = operation.kind;
    let operation_anchor = operation.anchor;
    plan_trace.steps.push(ProductionStepTrace {
        operation_kind,
        operation_anchor,
        slot_kind: role,
        // これはMust枠を埋める購入ではない。残額で前線圧を足すため、枠不足率は
        // 0のまま記録し、rolling planとの混同を避ける。
        deficit_before: 0.0,
        deficit_after: 0.0,
        remaining_funds_before,
        decision: ProductionDecision::ProducedImmediateReinforcement {
            unit_type: candidate.unit_type,
            cost: candidate.cost,
            facility: candidate.facility,
        },
    });
    commands.push(PlannedProduction {
        command: ProduceUnitCommand {
            player_id,
            target_x: candidate.facility.x,
            target_y: candidate.facility.y,
            unit_type: candidate.unit_type,
        },
        role,
        cost: candidate.cost,
        purpose,
        deployment,
        capture_intent: None,
    });
}
