//! 標準V4と小規模V4の生産判断の差を持つ方策。
use crate::ai::v4::property_control::{
    PropertyControlApproach, committed_capture_arrival, next_capture_target,
    select_property_capture_candidate,
};
use crate::ai::v4::*;

#[allow(unused_imports)]
use crate::ai::planning::decision::AdoptedTurnPortfolio;

/// 生産入口で一度だけ選び、配分・入力構築まで同じ値を引き継ぐ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ai::v4) enum ProductionPolicy {
    Standard,
    SmallMap,
}
impl ProductionPolicy {
    pub(in crate::ai::v4) fn capture_evaluation(self, player_id: PlayerId) -> CaptureEvaluation {
        CaptureEvaluation {
            policy: self,
            player_id,
        }
    }
    pub(in crate::ai::v4) fn for_current_path() -> Self {
        if crate::ai::code_path::is_extended() {
            Self::SmallMap
        } else {
            Self::Standard
        }
    }
    pub(in crate::ai::v4) fn allocates_surplus(self) -> bool {
        self == Self::SmallMap
    }
    pub(in crate::ai::v4) fn uses_route_commitments(self) -> bool {
        self == Self::SmallMap
    }
    pub(in crate::ai::v4) fn reports_property_control(self) -> bool {
        self == Self::SmallMap
    }
    pub(in crate::ai::v4) fn issues_best_effort(self) -> bool {
        self == Self::SmallMap
    }
    pub(in crate::ai::v4) fn uses_joint_capture(self) -> bool {
        self == Self::SmallMap
    }
    pub(in crate::ai::v4) fn optimizes_facilities(self) -> bool {
        self == Self::SmallMap
    }
    pub(in crate::ai::v4) fn counts_armed_carriers(self) -> bool {
        self == Self::SmallMap
    }
    pub(in crate::ai::v4) fn stress_funds(self, op: &Operation) -> u32 {
        if self == Self::SmallMap {
            op.stress_reinforcement_funds
        } else {
            0
        }
    }
    pub(in crate::ai::v4) fn allocation_slots(
        self,
        op: &Operation,
        index: usize,
        ledger: &SlotLedger,
    ) -> OperationSlots {
        match self {
            Self::Standard => op.slots,
            Self::SmallMap => allocation_slots(op, index, ledger),
        }
    }
    pub(in crate::ai::v4) fn allows_purchase_deferral(self, op: &Operation) -> bool {
        self == Self::Standard || op.property_controls.is_empty()
    }
    pub(in crate::ai::v4) fn reserve_capital_structure(
        self,
        operations: &[Operation],
        ledger: &mut SlotLedger,
    ) {
        if self == Self::SmallMap {
            defer_unapproved_capital_structure_for_active_segment(operations, ledger);
        }
    }
    pub(in crate::ai::v4) fn synchronize_contracts(
        self,
        scan: &BoardScan,
        operations: &mut [Operation],
        contract_registry: &mut plan_contract::PlanContractRegistry,
    ) {
        if self == Self::Standard {
            return;
        }
        // V4中央作戦契約との同期（build_operationsの純粋性を保つため、呼び出し元で実行）
        for operation in operations {
            if let Some(control) = operation.property_control
                && let Some(&target_entity) = scan.property_entities.get(&control.property)
            {
                let op_id =
                    contract_registry.maintain_or_replan(target_entity, control.property, None);
                let approach = match control.approach {
                    PropertyControlApproach::DirectCapture => {
                        plan_contract::OperationApproach::DirectCapture
                    }
                    PropertyControlApproach::Interdict => {
                        plan_contract::OperationApproach::Interdict
                    }
                    PropertyControlApproach::Recapture => {
                        plan_contract::OperationApproach::Recapture
                    }
                };
                contract_registry.update_approach_and_target(
                    op_id,
                    Some(approach),
                    Some(control.property),
                );
                operation.operation_id = Some(op_id);
            } else if let Some(objective) = scan
                .campaign_objectives
                .iter()
                .find(|obj| operation.objective_properties.contains(&obj.anchor))
                && let Some(&target_entity) = scan.property_entities.get(&objective.anchor)
            {
                let op_id =
                    contract_registry.maintain_or_replan(target_entity, objective.anchor, None);
                contract_registry.update_approach_and_target(op_id, None, Some(objective.anchor));
                operation.operation_id = Some(op_id);
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(in crate::ai::v4) fn select_candidate(
        self,
        scan: &BoardScan,
        ctx: &mut ReachCtx,
        op: &Operation,
        kind: SlotKind,
        used_facilities: &HashSet<GridPosition>,
        constraints: CandidateConstraints,
        player_id: PlayerId,
    ) -> Option<SlotCandidate> {
        if self == Self::SmallMap && kind == SlotKind::Capture && !op.property_controls.is_empty() {
            return select_property_capture_candidate(
                scan,
                ctx,
                op,
                used_facilities,
                constraints.remaining_funds,
                player_id,
            );
        }
        select_candidate_by(
            scan,
            ctx,
            kind,
            used_facilities,
            constraints,
            |ctx, facility, stats| {
                slot_fitness(
                    scan,
                    ctx,
                    op,
                    kind,
                    facility,
                    stats,
                    &self.capture_evaluation(player_id),
                )
            },
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(in crate::ai::v4) fn should_defer_purchase(
        self,
        scan: &BoardScan,
        ctx: &mut ReachCtx,
        op: &Operation,
        kind: SlotKind,
        remaining_funds: u32,
        affordable_cost: u32,
        player_id: PlayerId,
    ) -> bool {
        should_defer_purchase_by(
            scan,
            ctx,
            op,
            remaining_funds,
            affordable_cost,
            |ctx, facility, stats| {
                slot_fitness(
                    scan,
                    ctx,
                    op,
                    kind,
                    facility,
                    stats,
                    &self.capture_evaluation(player_id),
                )
            },
        )
    }
}

pub(in crate::ai::v4) struct CaptureEvaluation {
    policy: ProductionPolicy,
    player_id: PlayerId,
}
impl CaptureSlotPolicy for CaptureEvaluation {
    fn anchor(&self, op: &Operation) -> GridPosition {
        match self.policy {
            ProductionPolicy::Standard => op.anchor,
            ProductionPolicy::SmallMap => next_capture_target(op),
        }
    }
    fn fitness(
        &self,
        scan: &BoardScan,
        ctx: &mut ReachCtx,
        anchor: GridPosition,
        facility: &GridPosition,
        stats: &UnitStats,
    ) -> Option<f32> {
        match self.policy {
            ProductionPolicy::Standard => Some(1.0),
            ProductionPolicy::SmallMap => CaptureArrivalPolicy {
                player_id: self.player_id,
            }
            .fitness(scan, ctx, anchor, facility, stats),
        }
    }
}
pub(in crate::ai::v4) struct CaptureArrivalPolicy {
    pub(in crate::ai::v4) player_id: PlayerId,
}

impl CaptureSlotPolicy for CaptureArrivalPolicy {
    fn anchor(&self, op: &Operation) -> GridPosition {
        next_capture_target(op)
    }

    fn fitness(
        &self,
        scan: &BoardScan,
        ctx: &mut ReachCtx,
        anchor: GridPosition,
        facility: &GridPosition,
        stats: &UnitStats,
    ) -> Option<f32> {
        // 兵種名ではなく「物件へ何フェーズで到着し、既存の占領役より速いか」で比較する。
        // 速い兵種でも、既に割り当て済みの占領役より到着が遅い・同等なら直取り目的では
        // 買い増さない（`docs/07-project-management/plans/v4-v200-small-map-expansion-analysis.md`）。
        let candidate_arrival = property_control::capture_arrival_turns(
            &scan.map,
            &scan.master_data,
            (facility.x, facility.y),
            (anchor.x, anchor.y),
            stats.movement_type,
            stats.max_movement,
            stats.max_fuel,
            self.player_id,
            &mut ctx.action_turns,
        )
        .map(|turns| property_control::produced_action_offset(0, turns))
        .unwrap_or(u32::MAX);
        let committed_best = committed_capture_arrival(scan, ctx, anchor, self.player_id);
        property_control::capture_speed_fitness(candidate_arrival, committed_best)
    }
}
