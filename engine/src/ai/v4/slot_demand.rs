//! 生産判断の枠需要の優先度・配分・診断。

use crate::ai::v4::*;

/// 配備意図が永続Planの工程を持つならその工程、無ければ与えた目的を返す。
pub(in crate::ai::v4) fn plan_step_purpose(
    deployment: Option<&PlannedDeployment>,
    otherwise: PurchasePurpose<PlanStepRef>,
) -> PurchasePurpose<PlanStepRef> {
    deployment
        .and_then(|deployment| deployment.plan_step)
        .map_or(otherwise, PurchasePurpose::PlanStep)
}

/// 未充足で決着した要求枠を、作戦の識別情報つきのtraceへ変換する。
pub(in crate::ai::v4) fn unfilled_slot_traces(
    operations: &[Operation],
    ledger: &SlotLedger,
) -> Vec<UnfilledSlotTrace> {
    ledger
        .outcomes()
        .iter()
        .filter_map(|outcome| {
            let operation = operations.get(outcome.key.operation)?;
            Some(UnfilledSlotTrace {
                operation_kind: operation.kind,
                anchor: operation.anchor,
                slot_kind: outcome.key.slot,
                requirement: operation.slots.requirement(outcome.key.slot),
                reason: outcome.reason,
            })
        })
        .collect()
}

/// 元の要求・配分対象の実効要求・予約済みの充足量を、決定の入力と分離して記録する。
pub(in crate::ai::v4) fn production_demand_traces(
    operations: &[Operation],
    ledger: &SlotLedger,
) -> Vec<ProductionDemandTrace> {
    operations
        .iter()
        .enumerate()
        .map(|(index, operation)| ProductionDemandTrace {
            operation_kind: operation.kind,
            anchor: operation.anchor,
            requested: operation.slots,
            effective: effective_slots(operation, index, ledger),
            filled: operation.filled,
            adjustments: ledger
                .effective_demands()
                .iter()
                .filter(|demand| demand.key.operation == index)
                .copied()
                .collect(),
        })
        .collect()
}

/// 次に埋めるべき枠を返す。
///
/// 2 段階で選ぶ。要求が有限の枠（前提条件）を全作戦ぶん先に満たし、
/// そのうえで残額を要求が青天井の撃破枠へ注ぎ込む。
/// 有限要求と青天井要求を同じ土俵で比べてはならない（`SlotTier` 参照）。
pub(in crate::ai::v4) fn most_starved_slot(
    operations: &[Operation],
    ledger: &SlotLedger,
    policy: ProductionPolicy,
) -> Option<(usize, SlotKind)> {
    most_starved_in_tier(operations, ledger, SlotTier::Prerequisite, policy)
        .or_else(|| most_starved_in_tier(operations, ledger, SlotTier::Residual, policy))
}

/// 当手番の生産配分における要求枠の台帳。作戦番号は配分中の `operations` の添字。
pub(in crate::ai::v4) type SlotLedger = DemandLedger<SlotKind>;

/// 元の要求へ合同パッケージ等の実効要求を反映する。閉じた枠の除外は別に行う。
///
/// 未充足率と配分対象の選択に使う。要求そのものは `Operation::slots` に残す。
pub(in crate::ai::v4) fn allocation_slots(
    op: &Operation,
    index: usize,
    ledger: &SlotLedger,
) -> OperationSlots {
    let mut slots = op.slots;
    slots.capture_units =
        ledger.effective_requirement(index, SlotKind::Capture, slots.capture_units);
    slots.transport_slots =
        ledger.effective_requirement(index, SlotKind::Transport, slots.transport_slots);
    slots.intercept_units =
        ledger.effective_requirement(index, SlotKind::Intercept, slots.intercept_units);
    slots.combat_plan_required =
        ledger.effective_requirement(index, SlotKind::Combat, slots.combat_plan_required);
    slots
}

/// 当手番に未充足で決着した枠を除いた実効要求。元の要求は作戦に残す。
pub(in crate::ai::v4) fn effective_slots(
    op: &Operation,
    index: usize,
    ledger: &SlotLedger,
) -> OperationSlots {
    let mut slots = allocation_slots(op, index, ledger);
    for kind in SLOT_PRIORITY {
        if ledger.is_open(index, kind) {
            continue;
        }
        match kind {
            SlotKind::Intercept => slots.intercept_units = 0,
            SlotKind::Transport => slots.transport_slots = 0,
            SlotKind::Capture => slots.capture_units = 0,
            SlotKind::Combat => slots.combat_plan_required = 0,
        }
    }
    slots
}

// 生産枠の作戦優先度（小さいほど先に枠を取る）。値は同一手番内の相対順だけに使う。
/// 敵接触が展開リードタイム内に迫る防衛。
pub(in crate::ai::v4) const PRIORITY_RANK_URGENT_DEFENSE: u32 = 0;
/// 期限付き物件制御を持つCapture（拡張経路のみ。通常経路では使われない段）。
pub(in crate::ai::v4) const PRIORITY_RANK_DEADLINE_PROPERTY_CAPTURE: u32 = 1;
/// 実行許可済みの首都攻略。
pub(in crate::ai::v4) const PRIORITY_RANK_AUTHORIZED_CAPITAL: u32 = 2;
/// 急を要さない防衛。
pub(in crate::ai::v4) const PRIORITY_RANK_DEFENSE: u32 = 3;
/// 通常の占領作戦。
pub(in crate::ai::v4) const PRIORITY_RANK_CAPTURE: u32 = 4;
/// 実行未許可の首都攻略。
pub(in crate::ai::v4) const PRIORITY_RANK_UNAUTHORIZED_CAPITAL: u32 = 5;

/// 生産枠の実効優先度。すべて同じ作戦集合で比較し、別枠の緊急作戦は作らない。
/// Defenseは敵接触ETAと展開リードタイムから優先度を上げるが、必要枠だけを取得する。
pub(in crate::ai::v4) fn operation_priority_rank(
    operation: &Operation,
    policy: ProductionPolicy,
) -> u32 {
    let defense_cannot_wait = operation.kind == OperationKind::Defense
        && operation.facts.enemy_combat_units > 0
        && operation.facts.enemy_contact_eta
            <= operation.facts.deploy_lead_time.max(1).saturating_add(1);
    // 期限付き物件制御を持つCaptureを緊急扱いにするのは拡張経路だけ。
    // 通常経路では常にfalseとなり、順位1の段は使われない（相対順は従来どおり）。
    let property_control_cannot_wait =
        policy.reports_property_control() && !operation.property_controls.is_empty();
    match (
        operation.kind,
        operation.execution_authorized,
        defense_cannot_wait,
        property_control_cannot_wait,
    ) {
        (OperationKind::Defense, _, true, _) => PRIORITY_RANK_URGENT_DEFENSE,
        // 敵の完了時刻を過ぎると消滅する局地案だけを、首都本隊の編成より先に通す。
        // 通常Captureや成立不能なRecaptureは従来どおり首都作戦を追い越さない。
        (OperationKind::Capture, _, _, true) => PRIORITY_RANK_DEADLINE_PROPERTY_CAPTURE,
        (OperationKind::AssaultCapital, true, _, _) => PRIORITY_RANK_AUTHORIZED_CAPITAL,
        (OperationKind::Defense, _, false, _) => PRIORITY_RANK_DEFENSE,
        (OperationKind::Capture, _, _, false) => PRIORITY_RANK_CAPTURE,
        (OperationKind::AssaultCapital, false, _, _) => PRIORITY_RANK_UNAUTHORIZED_CAPITAL,
    }
}

/// 期限付き物件制御の占領役を、戦闘との共同計画へ回して単独の先買いを控えるか。
///
/// 拡張経路だけの枠適格条件。Combat計画が占領役も同じ組合せとして列挙するため、
/// 初回編成ではCapture枠を単独で埋めない。ただし過去の担当Entityが全滅した場合まで
/// 飛ばすと、戦闘枠不足の間は永久に奪還役が補充されない。担当履歴に未解決の撃破が
/// ある場合だけ1体を先に再契約し、当手番の発注後は共同計画へ戻すことで、最安歩兵の
/// 無制限な先買いを防ぐ。
pub(in crate::ai::v4) fn defers_capture_to_joint_combat_plan(
    op: &Operation,
    index: usize,
    ledger: &SlotLedger,
    kind: SlotKind,
    tier: SlotTier,
    policy: ProductionPolicy,
) -> bool {
    tier == SlotTier::Prerequisite
        && kind == SlotKind::Capture
        && !op.property_controls.is_empty()
        && (!op.capture_replacement_pending || op.filled.capture_units > 0)
        // Combat枠が未充足で決着済みなら、共同計画は成立しないため待たない。
        && ledger.is_open(index, SlotKind::Combat)
        && allocation_slots(op, index, ledger).deficit_ratio(SlotKind::Combat, &op.filled) > 0.0
        // 入口で選んだ方策だけで共同計画への委譲を決める。個別の計測IDや
        // 呼出し時のTLS経路を、配分の途中から再参照しない。
        && policy.uses_joint_capture()
}

/// 指定した段階の中で最も飢えた枠を返す。
pub(in crate::ai::v4) fn most_starved_in_tier(
    operations: &[Operation],
    ledger: &SlotLedger,
    tier: SlotTier,
    policy: ProductionPolicy,
) -> Option<(usize, SlotKind)> {
    let mut best: Option<(usize, SlotKind, u32, f32, usize)> = None;
    for (index, op) in operations.iter().enumerate() {
        let slots = allocation_slots(op, index, ledger);
        for (priority, kind) in SLOT_PRIORITY.iter().enumerate() {
            // 未充足で決着した枠は、要求が残っていても当手番の配分対象にしない。
            if !ledger.is_open(index, *kind) {
                continue;
            }
            if defers_capture_to_joint_combat_plan(op, index, ledger, *kind, tier, policy) {
                continue;
            }
            let deficit = slots.tier_deficit(*kind, &op.filled, tier);
            if deficit <= 0.0 {
                continue;
            }
            let operation_rank = match tier {
                SlotTier::Prerequisite => operation_priority_rank(op, policy),
                SlotTier::Residual => 0,
            };
            // 同じ作戦優先度なら、固定された役割順で全Captureを埋めるのではなく
            // 未充足率が最大の枠を選ぶ。最初の占領兵を確保した後はCombat計画が
            // 先に100%不足となるため、掃討波を形成してから残りの占領前線を広げる。
            // 未充足率が同じ場合だけ輸送→占領→戦闘という依存順を使う。
            let better = best.is_none_or(|(_, _, best_rank, best_deficit, best_priority)| {
                operation_rank < best_rank
                    || (operation_rank == best_rank
                        && (deficit.total_cmp(&best_deficit).is_gt()
                            || (deficit.total_cmp(&best_deficit).is_eq()
                                && priority < best_priority)))
            });
            if better {
                best = Some((index, *kind, operation_rank, deficit, priority));
            }
        }
    }
    best.map(|(index, kind, _, _, _)| (index, kind))
}

/// 購入した 1 体分を充足量へ反映する。
pub(in crate::ai::v4) fn record_fill(
    scan: &BoardScan,
    _ctx: &mut ReachCtx,
    op: &mut Operation,
    kind: SlotKind,
    candidate: &SlotCandidate,
) {
    let cargo = scan
        .available_types
        .iter()
        .find(|(unit_type, _)| *unit_type == candidate.unit_type)
        .map(|(_, stats)| stats.max_cargo)
        .unwrap_or(0);
    match kind {
        SlotKind::Intercept => {
            op.filled.intercept_units = op.filled.intercept_units.saturating_add(1);
        }
        SlotKind::Transport => {
            op.filled.transport_slots = op.filled.transport_slots.saturating_add(cargo.max(1))
        }
        SlotKind::Capture => op.filled.capture_units += 1,
        SlotKind::Combat => unreachable!("Combat購入はRollingPlan経路だけで処理する"),
    }
}

pub(in crate::ai::v4) fn defer_unapproved_capital_structure_for_active_segment(
    operations: &[Operation],
    ledger: &mut SlotLedger,
) {
    let active_segment_under_pressure = operations.iter().any(|operation| {
        operation.kind == OperationKind::Capture && operation.facts.enemy_combat_units > 0
    });
    if !active_segment_under_pressure {
        return;
    }
    for (index, operation) in operations.iter().enumerate() {
        if operation.kind == OperationKind::AssaultCapital && !operation.execution_authorized {
            ledger.close(
                index,
                SlotKind::Capture,
                UnfilledReason::DeferredBehindActiveSegment,
            );
            ledger.close(
                index,
                SlotKind::Transport,
                UnfilledReason::DeferredBehindActiveSegment,
            );
        }
    }
}
