//! V4 Combat計画を手番間で保持し、継続・撤回・再計画を判定する台帳。
//!
//! 盤面は毎ターン再評価するが、前revisionの未実行手順を候補集合から消さない。
//! 実行不能、硬い期限逸脱、目的達成、切替損失込みのPareto優越、上位防衛による
//! preemptだけを計画変更の理由として認める。

use super::operation::OperationKind;
use super::rolling_plan::{FixedPackageError, ForcePackagePlan, PlannedPurchase};
use crate::components::{GridPosition, PlayerId};
use crate::resources::UnitType;
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};

/// 作戦計画を手番間で識別するID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlanId(pub u64);

/// 同じ作戦計画を組み直した回数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlanRevision(pub u32);

/// revision内の生産手順ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlanStepId(pub u32);

/// 生産イベントと予定手順を照合するための型安全な参照。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlanStepRef {
    pub plan_id: PlanId,
    pub revision: PlanRevision,
    pub step_id: PlanStepId,
}

/// 計画を変更または終了した根拠。曖昧な「盤面が変わった」は理由にしない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplanReason {
    InitialPlan,
    ObjectiveCompleted,
    ProductionStepFailed,
    ProductionSlotUnavailable,
    FundingUnavailable,
    ContinuationInfeasible,
    HardDeadlineMissed,
    ParetoDominatedAfterSwitchCost,
    EnemyReinforced,
    FirstAttackDelayed,
    EliminationDelayed,
    OccupationDelayed,
    ProductionSlotDeferred,
    NoFeasibleReplacement,
}

/// その手番に行った計画ライフサイクル上の判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanDisposition {
    Created,
    Continued,
    Revised,
    Completed,
    Withdrawn,
    Rejected,
}

/// 比較に使用する現在時点からの残り計画指標。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanMetrics {
    pub completion_turn: Option<u32>,
    pub production_cost: u32,
    pub expected_loss: u32,
}

impl PlanMetrics {
    fn from_plan(plan: &ForcePackagePlan, _kind: OperationKind) -> Self {
        let tranche_completion_turn = plan
            .purchases
            .iter()
            .map(|purchase| purchase.build_turn.saturating_add(1))
            .max();
        Self {
            // Load/Drop/Capture未接続の移行期間は、固定占領ETAを捏造せず残敵排除ETAで
            // Combat revisionの実行可能性を比較する。作戦完了自体は実施設所有で判定する。
            completion_turn: plan
                .occupation_turn
                .or(plan.elimination_turn)
                // 全体完遂案がまだ無い場合も、敵HPを減らす有限の生産列は
                // tranche完了時に必ず再評価する実行単位として扱う。
                .or(tranche_completion_turn),
            production_cost: plan.production_cost,
            expected_loss: plan.expected_loss,
        }
    }

    fn feasible(self) -> bool {
        self.completion_turn.is_some()
    }
}

/// 全体撃破へ未到達でも、有限の購入列が敵HPを実際に減らすなら再評価付きtrancheとして実行する。
fn executable_plan(_kind: OperationKind, plan: &ForcePackagePlan) -> bool {
    if plan.feasible {
        return true;
    }
    !plan.purchases.is_empty()
        && plan
            .target_forecasts
            .iter()
            .any(|target| target.remaining_hp < target.initial_hp)
}

/// 現行計画と新規候補を比較するための、盤面から導出済みの事実。
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReplanAssessment {
    pub objective_complete: bool,
    pub production_failed: bool,
    pub continuation_error: Option<FixedPackageError>,
    pub continuation: Option<PlanMetrics>,
    pub candidate: Option<PlanMetrics>,
    pub hard_deadline: Option<u32>,
    pub production_slot_deferred: bool,
    pub enemy_reinforced: bool,
    pub execution_delay: Option<ReplanReason>,
    /// 既存Entityの再集合や予約解除で失う金額。投入済み費用そのものはsunk costなので含めない。
    pub switch_cost: u32,
    /// 任務変更によって余計に必要となる集合ターン。
    pub switch_delay: u32,
}

/// 純粋な継続・撤回判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleDecision {
    pub disposition: PlanDisposition,
    pub reason: Option<ReplanReason>,
}

/// 現行計画を捨ててよい条件を一か所へ集約する。
pub(crate) fn decide_lifecycle(assessment: ReplanAssessment) -> LifecycleDecision {
    if assessment.objective_complete {
        return LifecycleDecision {
            disposition: PlanDisposition::Completed,
            reason: Some(ReplanReason::ObjectiveCompleted),
        };
    }
    if assessment.production_slot_deferred {
        return replacement_or_withdrawal(assessment, ReplanReason::ProductionSlotDeferred);
    }
    if assessment.production_failed {
        return replacement_or_withdrawal(assessment, ReplanReason::ProductionStepFailed);
    }
    if let Some(error) = assessment.continuation_error {
        let reason = match error {
            FixedPackageError::ProductionSlotUnavailable
            | FixedPackageError::DuplicateProductionSlot => ReplanReason::ProductionSlotUnavailable,
            FixedPackageError::FundingUnavailable => ReplanReason::FundingUnavailable,
        };
        return replacement_or_withdrawal(assessment, reason);
    }
    if assessment.enemy_reinforced {
        return replacement_or_withdrawal(assessment, ReplanReason::EnemyReinforced);
    }
    if let Some(reason) = assessment.execution_delay {
        return replacement_or_withdrawal(assessment, reason);
    }

    if let (Some(current), Some(deadline)) = (assessment.continuation, assessment.hard_deadline)
        && current.completion_turn.is_none_or(|turn| turn > deadline)
    {
        return replacement_or_withdrawal(assessment, ReplanReason::HardDeadlineMissed);
    }
    let Some(current) = assessment.continuation.filter(|metrics| metrics.feasible()) else {
        return replacement_or_withdrawal(assessment, ReplanReason::ContinuationInfeasible);
    };

    if assessment.candidate.is_some_and(|candidate| {
        pareto_dominates_after_switch_cost(
            candidate,
            current,
            assessment.switch_cost,
            assessment.switch_delay,
        )
    }) {
        return LifecycleDecision {
            disposition: PlanDisposition::Revised,
            reason: Some(ReplanReason::ParetoDominatedAfterSwitchCost),
        };
    }

    LifecycleDecision {
        disposition: PlanDisposition::Continued,
        reason: None,
    }
}

fn replacement_or_withdrawal(
    assessment: ReplanAssessment,
    reason: ReplanReason,
) -> LifecycleDecision {
    let replacement_feasible = assessment.candidate.is_some_and(PlanMetrics::feasible);
    LifecycleDecision {
        disposition: if replacement_feasible {
            PlanDisposition::Revised
        } else {
            PlanDisposition::Withdrawn
        },
        reason: Some(reason),
    }
}

fn pareto_dominates_after_switch_cost(
    candidate: PlanMetrics,
    current: PlanMetrics,
    switch_cost: u32,
    switch_delay: u32,
) -> bool {
    let (Some(candidate_turn), Some(current_turn)) =
        (candidate.completion_turn, current.completion_turn)
    else {
        return false;
    };
    let switched_turn = candidate_turn.saturating_add(switch_delay);
    let switched_cost = candidate.production_cost.saturating_add(switch_cost);
    let no_worse = switched_turn <= current_turn
        && switched_cost <= current.production_cost
        && candidate.expected_loss <= current.expected_loss;
    let strictly_better = switched_turn < current_turn
        || switched_cost < current.production_cost
        || candidate.expected_loss < current.expected_loss;
    no_worse && strictly_better
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PurchaseStatus {
    Planned,
    Issued { turn: u32 },
    Produced,
}

#[derive(Debug, Clone)]
struct ScheduledPurchase {
    step: PlanStepRef,
    scheduled_turn: u32,
    facility: GridPosition,
    unit_type: UnitType,
    cost: u32,
    status: PurchaseStatus,
}

/// 上位作戦が現在の生産枠を使ったとき、未生産列の相対間隔を保ったまま繰り下げる。
/// 今手番分だけを次手番へ動かすと、元から次手番だった購入と同じ施設で衝突する。
fn defer_remaining_schedule(
    steps: &mut [ScheduledPurchase],
    turn: u32,
    conflicted_facilities: &HashSet<GridPosition>,
) {
    for facility in conflicted_facilities {
        let Some(first_pending_turn) = steps
            .iter()
            .filter(|step| step.status != PurchaseStatus::Produced && step.facility == *facility)
            .map(|step| step.scheduled_turn)
            .min()
        else {
            continue;
        };
        let delay = turn
            .saturating_add(1)
            .saturating_sub(first_pending_turn)
            .max(1);
        for step in steps
            .iter_mut()
            .filter(|step| step.status != PurchaseStatus::Produced && step.facility == *facility)
        {
            step.scheduled_turn = step.scheduled_turn.saturating_add(delay);
            if matches!(step.status, PurchaseStatus::Issued { .. }) {
                step.status = PurchaseStatus::Planned;
            }
        }
    }
}

/// 再評価で前倒しされた同一編成を、PlanId/StepIdを保ったまま永続予定へ反映する。
fn apply_rescheduled_purchases(
    steps: &mut [ScheduledPurchase],
    turn: u32,
    purchases: &[PlannedPurchase],
) {
    let mut available_steps = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.status == PurchaseStatus::Planned)
        .map(|(index, step)| (index, step.step.step_id.0))
        .collect::<Vec<_>>();
    available_steps.sort_unstable_by_key(|(_, step_id)| *step_id);

    let mut ordered_purchases = purchases.to_vec();
    ordered_purchases.sort_unstable_by_key(|purchase| {
        (
            purchase.build_turn,
            purchase.facility.y,
            purchase.facility.x,
        )
    });
    assert_eq!(
        available_steps.len(),
        ordered_purchases.len(),
        "固定編成の再配置で購入step数が変化してはならない"
    );
    for purchase in ordered_purchases {
        let position = available_steps
            .iter()
            .position(|(index, _)| {
                let step = &steps[*index];
                step.unit_type == purchase.unit_type && step.cost == purchase.cost
            })
            .expect("固定編成の再配置で兵種または費用が変化してはならない");
        let (step_index, _) = available_steps.remove(position);
        let step = &mut steps[step_index];
        step.facility = purchase.facility;
        step.scheduled_turn = turn.saturating_add(purchase.build_turn);
    }
}

/// 生産済みEntityからPlanへ戻す、当該手番の実績snapshot。
#[derive(Debug, Clone)]
pub(crate) struct DeploymentExecutionObservation {
    pub entity: Entity,
    pub plan_id: PlanId,
    pub plan_step: Option<PlanStepRef>,
    pub unit_cost: u32,
    pub alive: bool,
    pub mission_active: bool,
    pub current_loss_value: u32,
    pub first_attack_turn: Option<u32>,
    pub attack_count: u32,
    pub priority_attack_count: u32,
    pub kill_count: u32,
    pub damage_value_dealt: u32,
    pub counter_value_received: u32,
    pub destroyed_value: u32,
}

/// 敵Entityごとの予測と実績。金額ではなくHPと中立化手番を作戦進捗に使う。
#[derive(Debug, Clone)]
pub struct TargetExecutionSnapshot {
    pub entity: Entity,
    pub planned_destroy_turn: Option<u32>,
    pub actual_hp: Option<u32>,
    pub neutralized_turn: Option<u32>,
    pub reinforcement: bool,
}

/// Plan単位の予実台帳。E2E診断と次revisionの実行可能性判定で共有する。
#[derive(Debug, Clone, Default)]
pub struct PlanExecutionSnapshot {
    pub created_turn: u32,
    pub last_observed_turn: u32,
    pub planned_production_cost: u32,
    pub committed_production_cost: u32,
    pub actual_production_cost: u32,
    pub released_production_cost: u32,
    pub produced_step_count: usize,
    pub assigned_entity_count: usize,
    pub active_entity_count: usize,
    pub planned_first_attack_turn: Option<u32>,
    pub actual_first_attack_turn: Option<u32>,
    pub first_attack_delay: Option<i64>,
    pub planned_elimination_turn: Option<u32>,
    pub actual_elimination_turn: Option<u32>,
    pub elimination_delay: Option<i64>,
    pub planned_occupation_turn: Option<u32>,
    pub actual_occupation_turn: Option<u32>,
    pub occupation_delay: Option<i64>,
    pub attack_count: u32,
    pub priority_attack_count: u32,
    pub kill_count: u32,
    pub damage_value_dealt: u32,
    pub counter_value_received: u32,
    pub destroyed_value: u32,
    pub current_force_loss: u32,
    pub initial_target_count: usize,
    pub reinforcement_count: usize,
    pub remaining_target_count: usize,
    pub objective_property_count: usize,
    pub owned_objective_property_count: usize,
    pub targets: Vec<TargetExecutionSnapshot>,
}

#[derive(Debug, Clone, Default)]
struct PlanExecutionLedger {
    snapshot: PlanExecutionSnapshot,
    initial_targets: HashSet<Entity>,
    known_targets: HashSet<Entity>,
    planned_destroy_turns: HashMap<Entity, u32>,
    neutralized_turns: HashMap<Entity, u32>,
    produced_steps: HashSet<PlanStepRef>,
    assigned_entities: HashSet<Entity>,
}

impl PlanExecutionLedger {
    fn new(
        turn: u32,
        target_enemies: &HashSet<Entity>,
        objective_properties: &[GridPosition],
        plan: &ForcePackagePlan,
    ) -> Self {
        let mut ledger = Self::default();
        ledger.snapshot.created_turn = turn;
        ledger.initial_targets = target_enemies.clone();
        ledger.known_targets = target_enemies.clone();
        ledger.snapshot.initial_target_count = target_enemies.len();
        ledger.snapshot.objective_property_count = objective_properties.len();
        ledger.update_forecast(turn, plan);
        ledger
    }

    fn update_forecast(&mut self, turn: u32, plan: &ForcePackagePlan) {
        // revision後のplan.production_costは「今から追加する費用」。既に生産した
        // Entityの費用を失うと予算が実績より小さく見えるため、累計承認額に直す。
        self.snapshot.planned_production_cost = self
            .snapshot
            .actual_production_cost
            .saturating_add(plan.production_cost);
        self.snapshot.committed_production_cost =
            plan.purchases.iter().map(|purchase| purchase.cost).sum();
        self.snapshot.planned_first_attack_turn =
            plan.first_attack_turn.map(|eta| turn.saturating_add(eta));
        self.snapshot.planned_elimination_turn =
            plan.elimination_turn.map(|eta| turn.saturating_add(eta));
        self.snapshot.planned_occupation_turn =
            plan.occupation_turn.map(|eta| turn.saturating_add(eta));
        self.planned_destroy_turns.clear();
        for target in &plan.target_forecasts {
            if let (Some(entity), Some(destroyed_turn)) = (target.entity, target.destroyed_turn) {
                self.planned_destroy_turns
                    .insert(entity, turn.saturating_add(destroyed_turn));
            }
        }
    }

    fn observe_targets(&mut self, targets: &HashSet<Entity>) {
        self.known_targets.extend(targets.iter().copied());
        self.snapshot.reinforcement_count =
            self.known_targets.difference(&self.initial_targets).count();
    }

    fn note_released_cost(&mut self, cost: u32) {
        self.snapshot.released_production_cost =
            self.snapshot.released_production_cost.saturating_add(cost);
    }

    fn observe(
        &mut self,
        turn: u32,
        current_targets: &HashSet<Entity>,
        objective_properties: &[GridPosition],
        owned_properties: &HashSet<GridPosition>,
        enemy_health: &HashMap<Entity, u32>,
        deployments: &[DeploymentExecutionObservation],
    ) {
        self.snapshot.last_observed_turn = turn;
        self.snapshot.objective_property_count = objective_properties.len();
        self.snapshot.owned_objective_property_count = objective_properties
            .iter()
            .filter(|property| owned_properties.contains(property))
            .count();
        self.observe_targets(current_targets);

        self.assigned_entities.clear();
        self.produced_steps.clear();
        self.snapshot.actual_production_cost = 0;
        self.snapshot.active_entity_count = 0;
        self.snapshot.actual_first_attack_turn = None;
        self.snapshot.attack_count = 0;
        self.snapshot.priority_attack_count = 0;
        self.snapshot.kill_count = 0;
        self.snapshot.damage_value_dealt = 0;
        self.snapshot.counter_value_received = 0;
        self.snapshot.destroyed_value = 0;
        self.snapshot.current_force_loss = 0;
        for deployment in deployments {
            if self.assigned_entities.insert(deployment.entity) {
                self.snapshot.actual_production_cost = self
                    .snapshot
                    .actual_production_cost
                    .saturating_add(deployment.unit_cost);
            }
            if let Some(step) = deployment.plan_step {
                self.produced_steps.insert(step);
            }
            if deployment.alive && deployment.mission_active {
                self.snapshot.active_entity_count =
                    self.snapshot.active_entity_count.saturating_add(1);
            }
            self.snapshot.actual_first_attack_turn = match (
                self.snapshot.actual_first_attack_turn,
                deployment.first_attack_turn,
            ) {
                (Some(current), Some(observed)) => Some(current.min(observed)),
                (None, observed) => observed,
                (current, None) => current,
            };
            self.snapshot.attack_count = self
                .snapshot
                .attack_count
                .saturating_add(deployment.attack_count);
            self.snapshot.priority_attack_count = self
                .snapshot
                .priority_attack_count
                .saturating_add(deployment.priority_attack_count);
            self.snapshot.kill_count = self
                .snapshot
                .kill_count
                .saturating_add(deployment.kill_count);
            self.snapshot.damage_value_dealt = self
                .snapshot
                .damage_value_dealt
                .saturating_add(deployment.damage_value_dealt);
            self.snapshot.counter_value_received = self
                .snapshot
                .counter_value_received
                .saturating_add(deployment.counter_value_received);
            self.snapshot.destroyed_value = self
                .snapshot
                .destroyed_value
                .saturating_add(deployment.destroyed_value);
            self.snapshot.current_force_loss = self
                .snapshot
                .current_force_loss
                .saturating_add(deployment.current_loss_value);
        }
        self.snapshot.assigned_entity_count = self.assigned_entities.len();
        self.snapshot.produced_step_count = self.produced_steps.len();
        self.snapshot.committed_production_cost = self
            .snapshot
            .planned_production_cost
            .saturating_sub(self.snapshot.actual_production_cost);

        self.snapshot.first_attack_delay = delay_against_forecast(
            self.snapshot.planned_first_attack_turn,
            self.snapshot.actual_first_attack_turn,
            turn,
        );

        self.snapshot.remaining_target_count = current_targets
            .iter()
            .filter(|target| enemy_health.contains_key(target))
            .count();
        if self.snapshot.remaining_target_count == 0
            && !self.known_targets.is_empty()
            && self.snapshot.actual_elimination_turn.is_none()
        {
            self.snapshot.actual_elimination_turn = Some(turn);
        }
        self.snapshot.elimination_delay = delay_against_forecast(
            self.snapshot.planned_elimination_turn,
            self.snapshot.actual_elimination_turn,
            turn,
        );
        let objectives_secured = !objective_properties.is_empty()
            && self.snapshot.owned_objective_property_count == objective_properties.len();
        if objectives_secured && self.snapshot.actual_occupation_turn.is_none() {
            self.snapshot.actual_occupation_turn = Some(turn);
        }
        self.snapshot.occupation_delay = delay_against_forecast(
            self.snapshot.planned_occupation_turn,
            self.snapshot.actual_occupation_turn,
            turn,
        );

        for target in &self.known_targets {
            if !enemy_health.contains_key(target) {
                self.neutralized_turns.entry(*target).or_insert(turn);
            }
        }
        let mut targets = self
            .known_targets
            .iter()
            .map(|entity| TargetExecutionSnapshot {
                entity: *entity,
                planned_destroy_turn: self.planned_destroy_turns.get(entity).copied(),
                actual_hp: enemy_health.get(entity).copied(),
                neutralized_turn: self.neutralized_turns.get(entity).copied(),
                reinforcement: !self.initial_targets.contains(entity),
            })
            .collect::<Vec<_>>();
        targets.sort_unstable_by_key(|target| target.entity.to_bits());
        self.snapshot.targets = targets;
    }

    fn objectives_secured(&self) -> bool {
        self.snapshot.objective_property_count > 0
            && self.snapshot.owned_objective_property_count
                == self.snapshot.objective_property_count
    }

    fn delay_reason(&self, turn: u32) -> Option<ReplanReason> {
        if self
            .snapshot
            .planned_first_attack_turn
            .is_some_and(|planned| turn > planned)
            && self.snapshot.actual_first_attack_turn.is_none()
        {
            return Some(ReplanReason::FirstAttackDelayed);
        }
        if self
            .snapshot
            .planned_elimination_turn
            .is_some_and(|planned| turn > planned)
            && self.snapshot.remaining_target_count > 0
        {
            return Some(ReplanReason::EliminationDelayed);
        }
        if self
            .snapshot
            .planned_occupation_turn
            .is_some_and(|planned| turn > planned)
            && !self.objectives_secured()
        {
            return Some(ReplanReason::OccupationDelayed);
        }
        None
    }
}

fn delay_against_forecast(
    planned_turn: Option<u32>,
    actual_turn: Option<u32>,
    observed_turn: u32,
) -> Option<i64> {
    planned_turn.map(|planned| i64::from(actual_turn.unwrap_or(observed_turn)) - i64::from(planned))
}

#[derive(Debug, Clone)]
struct StoredPlan {
    player_id: PlayerId,
    plan_id: PlanId,
    revision: PlanRevision,
    kind: OperationKind,
    anchor: GridPosition,
    objective_properties: Vec<GridPosition>,
    target_enemies: HashSet<Entity>,
    steps: Vec<ScheduledPurchase>,
    forecast: PlanMetrics,
    /// 敵増援を含む最終撃破が実行可能で、兵站gate後に攻撃へ移してよいか。
    execution_ready: bool,
    last_evaluated_turn: u32,
    execution: PlanExecutionLedger,
}

/// E2E traceへ公開するrevision判断。
#[derive(Debug, Clone)]
pub struct PlanRevisionAudit {
    pub player_id: PlayerId,
    pub turn: u32,
    pub plan_id: PlanId,
    pub revision: PlanRevision,
    pub kind: OperationKind,
    pub anchor: GridPosition,
    pub disposition: PlanDisposition,
    pub reason: Option<ReplanReason>,
    pub remaining_steps: usize,
    pub forecast: PlanMetrics,
    pub execution: PlanExecutionSnapshot,
}

/// 呼び出し側が現行計画を現在盤面へ再評価するためのsnapshot。
#[derive(Debug, Clone)]
pub(crate) struct PlanContinuation {
    pub index: usize,
    pub plan_id: PlanId,
    pub revision: PlanRevision,
    pub purchases: Vec<PlannedPurchase>,
    pub production_failed: bool,
}

/// 新規作戦の上限で進行中計画を押し出さないため、作戦構築へ返す目的snapshot。
#[derive(Debug, Clone)]
pub(crate) struct ActivePlanObjective {
    pub kind: OperationKind,
    pub properties: Vec<GridPosition>,
    pub target_enemies: HashSet<Entity>,
}

/// 勝利ロードマップが局地Combat Planを島作戦へ接続するための読み取り専用情報。
#[derive(Debug, Clone)]
pub(crate) struct ActiveCombatPlanSummary {
    pub plan_id: PlanId,
    pub anchor: GridPosition,
    pub objective_properties: Vec<GridPosition>,
    pub planned_elimination_turn: Option<u32>,
    pub remaining_target_count: usize,
}

/// 既に生産済みのEntityへ、最新revisionの敵集合と局地範囲を配り直す情報。
#[derive(Debug, Clone)]
pub(crate) struct ActiveDeploymentIntent {
    pub plan_id: PlanId,
    pub anchor: GridPosition,
    pub staging_anchor: GridPosition,
    pub priority_enemies: Vec<Entity>,
    pub threat_horizon: u32,
    pub posture: super::deployment::DeploymentPosture,
}

/// 選択された実行計画と、永続台帳上の識別情報。
#[derive(Debug, Clone)]
pub(crate) struct SelectedPlan {
    pub plan: ForcePackagePlan,
    pub plan_id: Option<PlanId>,
    pub revision: Option<PlanRevision>,
    pub disposition: PlanDisposition,
    pub reason: Option<ReplanReason>,
}

/// プレイヤーごとの有効計画とrevision履歴を保持するResource。
#[derive(Resource, Debug, Default)]
pub struct V4RollingPlanRegistry {
    next_plan_id: u64,
    active: Vec<StoredPlan>,
    audits: Vec<PlanRevisionAudit>,
}

impl V4RollingPlanRegistry {
    /// 進行中計画を、新規候補より先に作戦集合へ復帰させる。
    pub(crate) fn active_objectives(&self, player_id: PlayerId) -> Vec<ActivePlanObjective> {
        self.active
            .iter()
            .filter(|plan| plan.player_id == player_id)
            .map(|plan| ActivePlanObjective {
                kind: plan.kind,
                properties: plan.objective_properties.clone(),
                target_enemies: plan.target_enemies.clone(),
            })
            .collect()
    }

    pub(crate) fn active_combat_plan_summaries(
        &self,
        player_id: PlayerId,
    ) -> Vec<ActiveCombatPlanSummary> {
        self.active
            .iter()
            .filter(|plan| plan.player_id == player_id)
            .map(|plan| ActiveCombatPlanSummary {
                plan_id: plan.plan_id,
                anchor: plan.anchor,
                objective_properties: plan.objective_properties.clone(),
                planned_elimination_turn: plan.execution.snapshot.planned_elimination_turn,
                remaining_target_count: plan.execution.snapshot.remaining_target_count,
            })
            .collect()
    }

    /// 盤面と生産済みEntityの実績を、各Planへ一度だけ線形集約する。
    pub(crate) fn observe_execution(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        owned_properties: &HashSet<GridPosition>,
        enemy_health: &HashMap<Entity, u32>,
        deployments: &[DeploymentExecutionObservation],
    ) {
        for plan in self
            .active
            .iter_mut()
            .filter(|plan| plan.player_id == player_id)
        {
            let plan_deployments = deployments
                .iter()
                .filter(|deployment| deployment.plan_id == plan.plan_id)
                .cloned()
                .collect::<Vec<_>>();
            plan.execution.observe(
                turn,
                &plan.target_enemies,
                &plan.objective_properties,
                owned_properties,
                enemy_health,
                &plan_deployments,
            );
            plan.execution.snapshot.committed_production_cost = plan
                .steps
                .iter()
                .filter(|step| step.status != PurchaseStatus::Produced)
                .map(|step| step.cost)
                .sum();
        }
    }

    /// 最新revisionの敵集合を、既に生産済みの全Entityへ再配布する。
    pub(crate) fn active_deployment_intents(
        &self,
        player_id: PlayerId,
        capital_assault_authorized: bool,
        capital_staging_anchor: Option<GridPosition>,
    ) -> Vec<ActiveDeploymentIntent> {
        self.active
            .iter()
            .filter(|plan| plan.player_id == player_id)
            .map(|plan| {
                let mut priority_enemies = plan.target_enemies.iter().copied().collect::<Vec<_>>();
                priority_enemies.sort_unstable_by_key(|entity| entity.to_bits());
                let formation_complete = plan
                    .steps
                    .iter()
                    .all(|step| step.status == PurchaseStatus::Produced);
                let posture = if plan.kind != OperationKind::AssaultCapital
                    || (capital_assault_authorized && formation_complete && plan.execution_ready)
                {
                    super::deployment::DeploymentPosture::Execute
                } else {
                    super::deployment::DeploymentPosture::Forming
                };
                ActiveDeploymentIntent {
                    plan_id: plan.plan_id,
                    anchor: plan.anchor,
                    staging_anchor: capital_staging_anchor.unwrap_or(plan.anchor),
                    priority_enemies,
                    threat_horizon: plan
                        .execution
                        .snapshot
                        .planned_occupation_turn
                        .unwrap_or(plan.last_evaluated_turn)
                        .saturating_sub(plan.last_evaluated_turn)
                        .max(1),
                    posture,
                }
            })
            .collect()
    }

    /// 現在残高から確保すべき、発行前の永続購入列の総額。
    pub(crate) fn reserved_purchase_cost(&self, player_id: PlayerId) -> u32 {
        self.active
            .iter()
            .filter(|plan| plan.player_id == player_id)
            .flat_map(|plan| plan.steps.iter())
            .filter(|step| step.status == PurchaseStatus::Planned)
            .map(|step| step.cost)
            .fold(0_u32, u32::saturating_add)
    }

    pub fn execution_records(
        &self,
        player_id: PlayerId,
    ) -> Vec<(PlanId, PlanRevision, PlanExecutionSnapshot)> {
        self.active
            .iter()
            .filter(|plan| plan.player_id == player_id)
            .map(|plan| (plan.plan_id, plan.revision, plan.execution.snapshot.clone()))
            .collect()
    }

    /// 生産完了イベントへ照合済みのstepを反映する。
    pub(crate) fn reconcile_produced_steps(
        &mut self,
        player_id: PlayerId,
        produced: &HashSet<PlanStepRef>,
    ) {
        for plan in self
            .active
            .iter_mut()
            .filter(|plan| plan.player_id == player_id)
        {
            for step in &mut plan.steps {
                if produced.contains(&step.step) {
                    step.status = PurchaseStatus::Produced;
                }
            }
        }
    }

    /// 対象拠点または敵Entityが重なる同種作戦を、前revisionの継続候補として取得する。
    pub(crate) fn continuation(
        &self,
        player_id: PlayerId,
        turn: u32,
        kind: OperationKind,
        objective_properties: &[GridPosition],
        target_enemies: &HashSet<Entity>,
    ) -> Option<PlanContinuation> {
        let index = self.matching_index(player_id, kind, objective_properties, target_enemies)?;
        let plan = &self.active[index];
        let production_failed = plan.steps.iter().any(|step| match step.status {
            PurchaseStatus::Planned => step.scheduled_turn < turn,
            PurchaseStatus::Issued { turn: issued_turn } => issued_turn < turn,
            PurchaseStatus::Produced => false,
        });
        let purchases = plan
            .steps
            .iter()
            .filter(|step| step.status != PurchaseStatus::Produced)
            .map(|step| PlannedPurchase {
                facility: step.facility,
                unit_type: step.unit_type,
                // 未照合の発注や期限超過stepも固定編成から落とさず、今手番の
                // 再配置候補として評価する。
                build_turn: step.scheduled_turn.saturating_sub(turn),
                cost: step.cost,
            })
            .collect();
        Some(PlanContinuation {
            index,
            plan_id: plan.plan_id,
            revision: plan.revision,
            purchases,
            production_failed,
        })
    }

    /// 現行案と新案を比較し、実行するrevisionを返す。不可能な案は永続化しない。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn select(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        kind: OperationKind,
        anchor: GridPosition,
        objective_properties: Vec<GridPosition>,
        target_enemies: HashSet<Entity>,
        continuation: Option<(
            PlanContinuation,
            Result<ForcePackagePlan, FixedPackageError>,
        )>,
        candidate: ForcePackagePlan,
        hard_deadline: Option<u32>,
        conflicted_facilities: HashSet<GridPosition>,
    ) -> SelectedPlan {
        let production_slot_deferred = !conflicted_facilities.is_empty();
        let candidate_executable = executable_plan(kind, &candidate);
        let candidate_metrics =
            candidate_executable.then(|| PlanMetrics::from_plan(&candidate, kind));
        let Some((previous, evaluated)) = continuation else {
            if !candidate_executable {
                return SelectedPlan {
                    plan: candidate,
                    plan_id: None,
                    revision: None,
                    disposition: PlanDisposition::Rejected,
                    reason: Some(ReplanReason::NoFeasibleReplacement),
                };
            }
            let (plan_id, revision) = self.create_plan(
                player_id,
                turn,
                kind,
                anchor,
                objective_properties,
                target_enemies,
                &candidate,
            );
            self.record_audit(
                player_id,
                turn,
                plan_id,
                revision,
                PlanDisposition::Created,
                Some(ReplanReason::InitialPlan),
            );
            return SelectedPlan {
                plan: candidate,
                plan_id: Some(plan_id),
                revision: Some(revision),
                disposition: PlanDisposition::Created,
                reason: Some(ReplanReason::InitialPlan),
            };
        };

        let continuation_error = evaluated.as_ref().err().copied();
        let continuation_metrics = evaluated
            .as_ref()
            .ok()
            .map(|plan| PlanMetrics::from_plan(plan, kind));
        let stored = &self.active[previous.index];
        let formation_in_progress = kind == OperationKind::AssaultCapital
            && stored
                .steps
                .iter()
                .any(|step| step.status != PurchaseStatus::Produced);
        let enemy_reinforced = !formation_in_progress
            && target_enemies
                .difference(&stored.target_enemies)
                .next()
                .is_some();
        let execution_delay = stored.execution.delay_reason(turn);

        // 兵站・防衛生産が当手番の施設を先に使っただけなら、首都編成を撤回しない。
        // 実在する同じ施設の購入を次手番へずらし、購入列とPlanIdを維持する。
        if kind == OperationKind::AssaultCapital
            && production_slot_deferred
            && matches!(
                continuation_error,
                Some(
                    FixedPackageError::ProductionSlotUnavailable
                        | FixedPackageError::FundingUnavailable
                )
            )
        {
            let (plan_id, revision, purchases) = {
                let stored = &mut self.active[previous.index];
                defer_remaining_schedule(&mut stored.steps, turn, &conflicted_facilities);
                stored.target_enemies = target_enemies;
                stored.last_evaluated_turn = turn;
                let purchases = stored
                    .steps
                    .iter()
                    .filter(|step| step.status == PurchaseStatus::Planned)
                    .map(|step| PlannedPurchase {
                        facility: step.facility,
                        unit_type: step.unit_type,
                        build_turn: step.scheduled_turn.saturating_sub(turn),
                        cost: step.cost,
                    })
                    .collect::<Vec<_>>();
                (stored.plan_id, stored.revision, purchases)
            };
            self.record_audit(
                player_id,
                turn,
                plan_id,
                revision,
                PlanDisposition::Continued,
                Some(ReplanReason::ProductionSlotDeferred),
            );
            let mut deferred = candidate;
            deferred.production_cost = purchases.iter().map(|purchase| purchase.cost).sum();
            deferred.purchases = purchases;
            return SelectedPlan {
                plan: deferred,
                plan_id: Some(plan_id),
                revision: Some(revision),
                disposition: PlanDisposition::Continued,
                reason: Some(ReplanReason::ProductionSlotDeferred),
            };
        }
        // 発注イベントを実Entityへ照合できなかった1stepだけを理由に、形成済み戦力と
        // PlanIdを全て捨てない。この契約は首都攻略だけでなく、中央島のCapture/Defense
        // にも必要である。残編成を現在の生産枠へ載せ直せる場合は、未完了stepだけを
        // Plannedへ戻して同じ計画内で再発注する。
        if previous.production_failed && continuation_error.is_none() {
            let plan = evaluated.expect("再発注可能な固定編成が評価済み");
            let stored = &mut self.active[previous.index];
            for step in &mut stored.steps {
                let failed = match step.status {
                    PurchaseStatus::Planned => step.scheduled_turn < turn,
                    PurchaseStatus::Issued { turn: issued_turn } => issued_turn < turn,
                    PurchaseStatus::Produced => false,
                };
                if failed {
                    step.status = PurchaseStatus::Planned;
                    step.scheduled_turn = turn;
                }
            }
            apply_rescheduled_purchases(&mut stored.steps, turn, &plan.purchases);
            stored.anchor = anchor;
            stored.objective_properties = objective_properties;
            stored.target_enemies = target_enemies;
            stored.forecast = PlanMetrics::from_plan(&plan, kind);
            stored.execution_ready = plan.feasible;
            stored.last_evaluated_turn = turn;
            stored.execution.observe_targets(&stored.target_enemies);
            self.record_audit(
                player_id,
                turn,
                previous.plan_id,
                previous.revision,
                PlanDisposition::Continued,
                Some(ReplanReason::ProductionStepFailed),
            );
            return SelectedPlan {
                plan,
                plan_id: Some(previous.plan_id),
                revision: Some(previous.revision),
                disposition: PlanDisposition::Continued,
                reason: Some(ReplanReason::ProductionStepFailed),
            };
        }
        let decision = decide_lifecycle(ReplanAssessment {
            objective_complete: target_enemies.is_empty() && stored.execution.objectives_secured(),
            production_failed: previous.production_failed,
            continuation_error,
            continuation: continuation_metrics,
            // 編成trancheの途中は敵が毎ターン増えても購入列を捨てない。生産失敗など
            // 硬い実行不能だけを先に処理し、完了後に次tranche/最終案を比較する。
            candidate: (!formation_in_progress)
                .then_some(candidate_metrics)
                .flatten(),
            hard_deadline,
            production_slot_deferred,
            enemy_reinforced,
            execution_delay,
            // このsliceでは既存Entityを再目標化しないため、未発注列の差替えに
            // 移動損失は発生しない。実Entityを切り替える段階で実測値を入力する。
            switch_cost: 0,
            switch_delay: 0,
        });

        match decision.disposition {
            PlanDisposition::Continued => {
                let plan = evaluated.expect("継続判定には実行可能な現行案がある");
                let stored = &mut self.active[previous.index];
                apply_rescheduled_purchases(&mut stored.steps, turn, &plan.purchases);
                stored.anchor = anchor;
                stored.objective_properties = objective_properties;
                stored.target_enemies = target_enemies;
                stored.forecast = PlanMetrics::from_plan(&plan, kind);
                stored.execution_ready = plan.feasible;
                stored.last_evaluated_turn = turn;
                stored.execution.observe_targets(&stored.target_enemies);
                self.record_audit(
                    player_id,
                    turn,
                    previous.plan_id,
                    previous.revision,
                    decision.disposition,
                    decision.reason,
                );
                SelectedPlan {
                    plan,
                    plan_id: Some(previous.plan_id),
                    revision: Some(previous.revision),
                    disposition: decision.disposition,
                    reason: decision.reason,
                }
            }
            PlanDisposition::Revised if candidate_executable => {
                let revision = PlanRevision(previous.revision.0.saturating_add(1));
                self.replace_plan(
                    previous.index,
                    turn,
                    revision,
                    anchor,
                    objective_properties,
                    target_enemies,
                    &candidate,
                );
                self.record_audit(
                    player_id,
                    turn,
                    previous.plan_id,
                    revision,
                    decision.disposition,
                    decision.reason,
                );
                SelectedPlan {
                    plan: candidate,
                    plan_id: Some(previous.plan_id),
                    revision: Some(revision),
                    disposition: decision.disposition,
                    reason: decision.reason,
                }
            }
            PlanDisposition::Completed | PlanDisposition::Withdrawn => {
                let mut removed = self.active.remove(previous.index);
                let released_cost = remaining_unissued_cost(&removed.steps);
                removed.execution.note_released_cost(released_cost);
                self.audits.push(PlanRevisionAudit {
                    player_id,
                    turn,
                    plan_id: removed.plan_id,
                    revision: removed.revision,
                    kind: removed.kind,
                    anchor,
                    disposition: decision.disposition,
                    reason: decision.reason,
                    remaining_steps: removed
                        .steps
                        .iter()
                        .filter(|step| step.status != PurchaseStatus::Produced)
                        .count(),
                    forecast: removed.forecast,
                    execution: removed.execution.snapshot,
                });
                SelectedPlan {
                    plan: candidate,
                    plan_id: None,
                    revision: None,
                    disposition: decision.disposition,
                    reason: decision.reason,
                }
            }
            _ => SelectedPlan {
                plan: candidate,
                plan_id: None,
                revision: None,
                disposition: PlanDisposition::Rejected,
                reason: Some(ReplanReason::NoFeasibleReplacement),
            },
        }
    }

    /// 生産命令を発行した時点でstepをIssuedへ進める。
    pub(crate) fn mark_issued(&mut self, step_ref: PlanStepRef, turn: u32) {
        for plan in &mut self.active {
            if plan.plan_id != step_ref.plan_id || plan.revision != step_ref.revision {
                continue;
            }
            if let Some(step) = plan.steps.iter_mut().find(|step| step.step == step_ref) {
                step.status = PurchaseStatus::Issued { turn };
                return;
            }
        }
    }

    /// 選択計画の当手番購入をstep参照へ変換する。
    pub(crate) fn current_step_ref(
        &self,
        plan_id: PlanId,
        revision: PlanRevision,
        turn: u32,
        purchase: PlannedPurchase,
    ) -> Option<PlanStepRef> {
        self.active
            .iter()
            .find(|plan| plan.plan_id == plan_id && plan.revision == revision)
            .and_then(|plan| {
                plan.steps.iter().find(|step| {
                    step.scheduled_turn == turn
                        && step.facility == purchase.facility
                        && step.unit_type == purchase.unit_type
                        && step.cost == purchase.cost
                        && step.status == PurchaseStatus::Planned
                })
            })
            .map(|step| step.step)
    }

    pub fn audit_records(&self, player_id: PlayerId) -> Vec<PlanRevisionAudit> {
        self.audits
            .iter()
            .filter(|audit| audit.player_id == player_id)
            .cloned()
            .collect()
    }

    /// 今手番の作戦集合から消えた計画を、実績から確定できる理由だけで閉じる。
    /// 単に作戦候補へ再掲されなかっただけなら保持し、未実行手順の期限超過か
    /// 対象敵の全滅を観測した場合に限って撤回・完了する。
    pub(crate) fn reconcile_unseen_plans(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        seen: &HashSet<PlanId>,
    ) {
        let mut index = 0;
        while index < self.active.len() {
            let plan = &self.active[index];
            if plan.player_id != player_id || seen.contains(&plan.plan_id) {
                index += 1;
                continue;
            }
            let objective_complete = plan.execution.snapshot.remaining_target_count == 0
                && !plan.execution.known_targets.is_empty()
                && plan.execution.objectives_secured();
            let production_failed = plan.steps.iter().any(|step| match step.status {
                PurchaseStatus::Planned => step.scheduled_turn < turn,
                PurchaseStatus::Issued { turn: issued_turn } => issued_turn < turn,
                PurchaseStatus::Produced => false,
            });
            if !(objective_complete || production_failed) {
                index += 1;
                continue;
            }
            let mut removed = self.active.remove(index);
            let released_cost = remaining_unissued_cost(&removed.steps);
            removed.execution.note_released_cost(released_cost);
            self.audits.push(PlanRevisionAudit {
                player_id,
                turn,
                plan_id: removed.plan_id,
                revision: removed.revision,
                kind: removed.kind,
                anchor: removed.anchor,
                disposition: if objective_complete {
                    PlanDisposition::Completed
                } else {
                    PlanDisposition::Withdrawn
                },
                reason: Some(if objective_complete {
                    ReplanReason::ObjectiveCompleted
                } else {
                    ReplanReason::ProductionStepFailed
                }),
                remaining_steps: removed
                    .steps
                    .iter()
                    .filter(|step| step.status != PurchaseStatus::Produced)
                    .count(),
                forecast: removed.forecast,
                execution: removed.execution.snapshot,
            });
        }
    }

    fn matching_index(
        &self,
        player_id: PlayerId,
        kind: OperationKind,
        objective_properties: &[GridPosition],
        _target_enemies: &HashSet<Entity>,
    ) -> Option<usize> {
        self.active
            .iter()
            .enumerate()
            .filter(|(_, plan)| plan.player_id == player_id && plan.kind == kind)
            .find_map(|(index, plan)| {
                // 敵Entityは移動して別の島へ渡るため、敵の重複をPlan identityに使わない。
                // 同じ目的拠点集合だけを同一作戦のrevisionとして扱い、別島への変質を禁止する。
                (plan.objective_properties.len() == objective_properties.len()
                    && plan
                        .objective_properties
                        .iter()
                        .all(|property| objective_properties.contains(property)))
                .then_some(index)
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn create_plan(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        kind: OperationKind,
        anchor: GridPosition,
        objective_properties: Vec<GridPosition>,
        target_enemies: HashSet<Entity>,
        plan: &ForcePackagePlan,
    ) -> (PlanId, PlanRevision) {
        self.next_plan_id = self.next_plan_id.saturating_add(1);
        let plan_id = PlanId(self.next_plan_id);
        let revision = PlanRevision(0);
        let execution =
            PlanExecutionLedger::new(turn, &target_enemies, &objective_properties, plan);
        self.active.push(StoredPlan {
            player_id,
            plan_id,
            revision,
            kind,
            anchor,
            objective_properties,
            target_enemies,
            steps: scheduled_steps(plan_id, revision, turn, plan),
            forecast: PlanMetrics::from_plan(plan, kind),
            execution_ready: plan.feasible,
            last_evaluated_turn: turn,
            execution,
        });
        (plan_id, revision)
    }

    #[allow(clippy::too_many_arguments)]
    fn replace_plan(
        &mut self,
        index: usize,
        turn: u32,
        revision: PlanRevision,
        anchor: GridPosition,
        objective_properties: Vec<GridPosition>,
        target_enemies: HashSet<Entity>,
        plan: &ForcePackagePlan,
    ) {
        let stored = &mut self.active[index];
        let released_cost = remaining_unissued_cost(&stored.steps);
        stored.execution.note_released_cost(released_cost);
        stored.revision = revision;
        stored.anchor = anchor;
        stored.objective_properties = objective_properties;
        stored.target_enemies = target_enemies;
        stored.steps = scheduled_steps(stored.plan_id, revision, turn, plan);
        stored.forecast = PlanMetrics::from_plan(plan, stored.kind);
        stored.execution_ready = plan.feasible;
        stored.last_evaluated_turn = turn;
        stored.execution.observe_targets(&stored.target_enemies);
        stored.execution.update_forecast(turn, plan);
    }

    fn record_audit(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        plan_id: PlanId,
        revision: PlanRevision,
        disposition: PlanDisposition,
        reason: Option<ReplanReason>,
    ) {
        let Some(plan) = self
            .active
            .iter()
            .find(|plan| plan.plan_id == plan_id && plan.revision == revision)
        else {
            return;
        };
        self.audits.push(PlanRevisionAudit {
            player_id,
            turn,
            plan_id,
            revision,
            kind: plan.kind,
            anchor: plan.anchor,
            disposition,
            reason,
            remaining_steps: plan
                .steps
                .iter()
                .filter(|step| step.status != PurchaseStatus::Produced)
                .count(),
            forecast: plan.forecast,
            execution: plan.execution.snapshot.clone(),
        });
    }
}

fn scheduled_steps(
    plan_id: PlanId,
    revision: PlanRevision,
    turn: u32,
    plan: &ForcePackagePlan,
) -> Vec<ScheduledPurchase> {
    plan.purchases
        .iter()
        .enumerate()
        .map(|(index, purchase)| ScheduledPurchase {
            step: PlanStepRef {
                plan_id,
                revision,
                step_id: PlanStepId(u32::try_from(index).unwrap_or(u32::MAX)),
            },
            scheduled_turn: turn.saturating_add(purchase.build_turn),
            facility: purchase.facility,
            unit_type: purchase.unit_type,
            cost: purchase.cost,
            status: PurchaseStatus::Planned,
        })
        .collect()
}

fn remaining_unissued_cost(steps: &[ScheduledPurchase]) -> u32 {
    steps
        .iter()
        .filter(|step| step.status == PurchaseStatus::Planned)
        .map(|step| step.cost)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(turn: u32, cost: u32, loss: u32) -> PlanMetrics {
        PlanMetrics {
            completion_turn: Some(turn),
            production_cost: cost,
            expected_loss: loss,
        }
    }

    fn assessment(current: PlanMetrics, candidate: PlanMetrics) -> ReplanAssessment {
        ReplanAssessment {
            objective_complete: false,
            production_failed: false,
            continuation_error: None,
            continuation: Some(current),
            candidate: Some(candidate),
            hard_deadline: None,
            production_slot_deferred: false,
            enemy_reinforced: false,
            execution_delay: None,
            switch_cost: 0,
            switch_delay: 0,
        }
    }

    #[test]
    fn capital_slot_conflict_defers_only_the_affected_facility_without_collision() {
        let facility = GridPosition { x: 3, y: 4 };
        let mut steps = vec![
            ScheduledPurchase {
                step: PlanStepRef {
                    plan_id: PlanId(1),
                    revision: PlanRevision(0),
                    step_id: PlanStepId(0),
                },
                scheduled_turn: 10,
                facility,
                unit_type: UnitType::Bomber,
                cost: 22_000,
                status: PurchaseStatus::Issued { turn: 9 },
            },
            ScheduledPurchase {
                step: PlanStepRef {
                    plan_id: PlanId(1),
                    revision: PlanRevision(0),
                    step_id: PlanStepId(1),
                },
                scheduled_turn: 11,
                facility,
                unit_type: UnitType::Bcopters,
                cost: 7_500,
                status: PurchaseStatus::Planned,
            },
            ScheduledPurchase {
                step: PlanStepRef {
                    plan_id: PlanId(1),
                    revision: PlanRevision(0),
                    step_id: PlanStepId(2),
                },
                scheduled_turn: 9,
                facility,
                unit_type: UnitType::Fighter,
                cost: 16_000,
                status: PurchaseStatus::Produced,
            },
            ScheduledPurchase {
                step: PlanStepRef {
                    plan_id: PlanId(1),
                    revision: PlanRevision(0),
                    step_id: PlanStepId(3),
                },
                scheduled_turn: 10,
                facility: GridPosition { x: 7, y: 8 },
                unit_type: UnitType::Bomber,
                cost: 22_000,
                status: PurchaseStatus::Planned,
            },
        ];

        defer_remaining_schedule(&mut steps, 10, &HashSet::from([facility]));

        assert_eq!(steps[0].scheduled_turn, 11);
        assert_eq!(steps[0].status, PurchaseStatus::Planned);
        assert_eq!(steps[1].scheduled_turn, 12);
        assert_eq!(steps[2].scheduled_turn, 9);
        assert_eq!(steps[2].status, PurchaseStatus::Produced);
        assert_eq!(steps[3].scheduled_turn, 10);
        assert_eq!(steps[3].status, PurchaseStatus::Planned);
    }

    #[test]
    fn continued_plan_persists_the_rescheduled_facility_and_turn() {
        let old_facility = GridPosition { x: 3, y: 4 };
        let new_facility = GridPosition { x: 7, y: 8 };
        let mut steps = vec![ScheduledPurchase {
            step: PlanStepRef {
                plan_id: PlanId(1),
                revision: PlanRevision(0),
                step_id: PlanStepId(0),
            },
            scheduled_turn: 12,
            facility: old_facility,
            unit_type: UnitType::Bomber,
            cost: 22_000,
            status: PurchaseStatus::Planned,
        }];

        apply_rescheduled_purchases(
            &mut steps,
            10,
            &[PlannedPurchase {
                facility: new_facility,
                unit_type: UnitType::Bomber,
                build_turn: 0,
                cost: 22_000,
            }],
        );

        assert_eq!(steps[0].facility, new_facility);
        assert_eq!(steps[0].scheduled_turn, 10);
        assert_eq!(steps[0].step.step_id, PlanStepId(0));
    }

    #[test]
    fn capital_budget_conflict_defers_instead_of_withdrawing_the_plan() {
        let player = PlayerId(1);
        let enemy = Entity::from_raw(10);
        let anchor = GridPosition { x: 9, y: 9 };
        let enemies = HashSet::from([enemy]);
        let mut registry = V4RollingPlanRegistry::default();
        let created = registry.select(
            player,
            3,
            OperationKind::AssaultCapital,
            anchor,
            vec![anchor],
            enemies.clone(),
            None,
            feasible_plan(0),
            None,
            HashSet::new(),
        );
        let continuation = registry
            .continuation(
                player,
                3,
                OperationKind::AssaultCapital,
                &[anchor],
                &enemies,
            )
            .unwrap();
        let facility = created.plan.purchases[0].facility;

        let selected = registry.select(
            player,
            3,
            OperationKind::AssaultCapital,
            anchor,
            vec![anchor],
            enemies.clone(),
            Some((continuation, Err(FixedPackageError::FundingUnavailable))),
            feasible_plan(0),
            None,
            HashSet::from([facility]),
        );

        assert_eq!(selected.plan_id, created.plan_id);
        assert_eq!(selected.disposition, PlanDisposition::Continued);
        assert_eq!(selected.reason, Some(ReplanReason::ProductionSlotDeferred));
        let deferred = registry
            .continuation(
                player,
                4,
                OperationKind::AssaultCapital,
                &[anchor],
                &enemies,
            )
            .unwrap();
        assert!(!deferred.production_failed);
        assert_eq!(deferred.purchases[0].build_turn, 0);
    }

    #[test]
    fn unmatched_issue_retries_the_step_for_every_operation_kind() {
        for kind in [OperationKind::Capture, OperationKind::AssaultCapital] {
            let player = PlayerId(1);
            let enemy = Entity::from_raw(10);
            let anchor = GridPosition { x: 9, y: 9 };
            let enemies = HashSet::from([enemy]);
            let mut registry = V4RollingPlanRegistry::default();
            let created = registry.select(
                player,
                3,
                kind,
                anchor,
                vec![anchor],
                enemies.clone(),
                None,
                feasible_plan(0),
                None,
                HashSet::new(),
            );
            let purchase = created.plan.purchases[0];
            let step = registry
                .current_step_ref(
                    created.plan_id.unwrap(),
                    created.revision.unwrap(),
                    3,
                    purchase,
                )
                .unwrap();
            registry.mark_issued(step, 3);
            let continuation = registry
                .continuation(player, 4, kind, &[anchor], &enemies)
                .unwrap();
            assert!(continuation.production_failed);
            assert_eq!(continuation.purchases.len(), 1);

            let selected = registry.select(
                player,
                4,
                kind,
                anchor,
                vec![anchor],
                enemies.clone(),
                Some((continuation, Ok(feasible_plan(0)))),
                feasible_plan(0),
                None,
                HashSet::new(),
            );

            assert_eq!(selected.plan_id, created.plan_id);
            assert_eq!(selected.disposition, PlanDisposition::Continued);
            assert_eq!(selected.reason, Some(ReplanReason::ProductionStepFailed));
            let retried = registry
                .continuation(player, 4, kind, &[anchor], &enemies)
                .unwrap();
            assert!(!retried.production_failed);
            assert_eq!(retried.purchases[0].build_turn, 0);
        }
    }

    #[test]
    fn premise_change_alone_does_not_discard_current_plan() {
        let result = decide_lifecycle(assessment(
            metrics(6, 20_000, 3_000),
            metrics(5, 25_000, 2_000),
        ));
        assert_eq!(result.disposition, PlanDisposition::Continued);
        assert_eq!(result.reason, None);
    }

    #[test]
    fn impossible_continuation_is_revised_when_replacement_is_feasible() {
        let mut input = assessment(metrics(6, 20_000, 3_000), metrics(5, 25_000, 2_000));
        input.continuation = None;
        let result = decide_lifecycle(input);
        assert_eq!(result.disposition, PlanDisposition::Revised);
        assert_eq!(result.reason, Some(ReplanReason::ContinuationInfeasible));
    }

    #[test]
    fn impossible_plan_is_withdrawn_when_no_replacement_exists() {
        let mut input = assessment(metrics(6, 20_000, 3_000), metrics(5, 25_000, 2_000));
        input.continuation = None;
        input.candidate = None;
        let result = decide_lifecycle(input);
        assert_eq!(result.disposition, PlanDisposition::Withdrawn);
        assert_eq!(result.reason, Some(ReplanReason::ContinuationInfeasible));
    }

    #[test]
    fn new_plan_must_dominate_after_switch_cost() {
        let mut input = assessment(metrics(6, 20_000, 3_000), metrics(5, 19_000, 3_000));
        input.switch_cost = 2_000;
        input.switch_delay = 1;
        assert_eq!(
            decide_lifecycle(input).disposition,
            PlanDisposition::Continued
        );

        input.switch_cost = 0;
        input.switch_delay = 0;
        assert_eq!(
            decide_lifecycle(input).disposition,
            PlanDisposition::Revised
        );
    }

    #[test]
    fn hard_deadline_miss_forces_revision() {
        let mut input = assessment(metrics(6, 20_000, 3_000), metrics(4, 22_000, 2_000));
        input.hard_deadline = Some(4);
        let result = decide_lifecycle(input);
        assert_eq!(result.disposition, PlanDisposition::Revised);
        assert_eq!(result.reason, Some(ReplanReason::HardDeadlineMissed));
    }

    #[test]
    fn completed_objective_ends_plan_without_replacement() {
        let mut input = assessment(metrics(6, 20_000, 3_000), metrics(4, 22_000, 2_000));
        input.objective_complete = true;
        let result = decide_lifecycle(input);
        assert_eq!(result.disposition, PlanDisposition::Completed);
        assert_eq!(result.reason, Some(ReplanReason::ObjectiveCompleted));
    }

    #[test]
    fn reinforcement_and_execution_delay_are_explicit_revision_reasons() {
        let mut reinforced = assessment(metrics(6, 20_000, 3_000), metrics(7, 25_000, 4_000));
        reinforced.enemy_reinforced = true;
        let result = decide_lifecycle(reinforced);
        assert_eq!(result.disposition, PlanDisposition::Revised);
        assert_eq!(result.reason, Some(ReplanReason::EnemyReinforced));

        let mut delayed = assessment(metrics(6, 20_000, 3_000), metrics(7, 25_000, 4_000));
        delayed.execution_delay = Some(ReplanReason::EliminationDelayed);
        let result = decide_lifecycle(delayed);
        assert_eq!(result.disposition, PlanDisposition::Revised);
        assert_eq!(result.reason, Some(ReplanReason::EliminationDelayed));
    }

    fn feasible_plan(build_turn: u32) -> ForcePackagePlan {
        ForcePackagePlan {
            purchases: vec![PlannedPurchase {
                facility: GridPosition { x: 2, y: 3 },
                unit_type: UnitType::Bcopters,
                build_turn,
                cost: 7_500,
            }],
            target_forecasts: Vec::new(),
            turn_forecasts: Vec::new(),
            feasible: true,
            first_attack_turn: Some(build_turn.saturating_add(2)),
            elimination_turn: Some(build_turn.saturating_add(3)),
            occupation_turn: Some(build_turn.saturating_add(5)),
            production_cost: 7_500,
            expected_loss: 1_000,
            protected_unit_count: 0,
            protected_survivor_count: 0,
            required_capture_survivor_count: 0,
            candidates_considered: 1,
            search_truncated: false,
        }
    }

    #[test]
    fn future_purchase_survives_until_its_scheduled_turn() {
        let player = PlayerId(1);
        let mut registry = V4RollingPlanRegistry::default();
        let selected = registry.select(
            player,
            3,
            OperationKind::Capture,
            GridPosition { x: 5, y: 5 },
            vec![GridPosition { x: 5, y: 5 }],
            HashSet::from([Entity::from_raw(10)]),
            None,
            feasible_plan(1),
            None,
            HashSet::new(),
        );
        assert_eq!(selected.disposition, PlanDisposition::Created);
        let created_audit = registry
            .audit_records(player)
            .into_iter()
            .find(|audit| audit.plan_id == selected.plan_id.expect("永続plan"))
            .expect("作成判断は監査履歴へ残る");
        assert_eq!(created_audit.turn, 3);
        assert_eq!(created_audit.disposition, PlanDisposition::Created);

        let continuation = registry
            .continuation(
                player,
                4,
                OperationKind::Capture,
                &[GridPosition { x: 5, y: 5 }],
                &HashSet::from([Entity::from_raw(10)]),
            )
            .expect("将来購入は次手番まで保持される");
        assert!(!continuation.production_failed);
        assert_eq!(continuation.purchases.len(), 1);
        assert_eq!(continuation.purchases[0].build_turn, 0);
    }

    #[test]
    fn enemy_overlap_does_not_move_a_plan_to_another_objective() {
        let player = PlayerId(1);
        let enemy = Entity::from_raw(10);
        let original = GridPosition { x: 5, y: 5 };
        let another_island = GridPosition { x: 20, y: 20 };
        let enemies = HashSet::from([enemy]);
        let mut registry = V4RollingPlanRegistry::default();
        registry.select(
            player,
            3,
            OperationKind::Capture,
            original,
            vec![original],
            enemies.clone(),
            None,
            feasible_plan(1),
            None,
            HashSet::new(),
        );

        assert!(
            registry
                .continuation(
                    player,
                    4,
                    OperationKind::Capture,
                    &[another_island],
                    &enemies,
                )
                .is_none(),
            "同じ敵を追って別島の作戦へPlanIdを移してはならない"
        );
        assert!(
            registry
                .continuation(
                    player,
                    4,
                    OperationKind::Capture,
                    &[original],
                    &HashSet::from([Entity::from_raw(11)]),
                )
                .is_some(),
            "敵集合が変化しても目的拠点が同じなら同一作戦として再評価する"
        );
    }

    #[test]
    fn unseen_plan_is_kept_until_an_execution_step_actually_misses_its_turn() {
        let player = PlayerId(1);
        let enemy = Entity::from_raw(10);
        let properties = [GridPosition { x: 5, y: 5 }];
        let enemies = HashSet::from([enemy]);
        let mut registry = V4RollingPlanRegistry::default();
        let selected = registry.select(
            player,
            3,
            OperationKind::Capture,
            properties[0],
            properties.to_vec(),
            enemies.clone(),
            None,
            feasible_plan(1),
            None,
            HashSet::new(),
        );
        let plan_id = selected.plan_id.expect("実行可能な計画は永続化される");

        // 作戦候補へ再掲されなかったこと自体は撤回理由ではない。
        registry.reconcile_unseen_plans(player, 4, &HashSet::new());
        assert!(
            registry
                .continuation(player, 4, OperationKind::Capture, &properties, &enemies)
                .is_some()
        );

        // 生産予定手番を過ぎても発注されなかった時点で、初めて実行失敗として撤回する。
        registry.reconcile_unseen_plans(player, 5, &HashSet::new());
        assert!(
            registry
                .continuation(player, 5, OperationKind::Capture, &properties, &enemies)
                .is_none()
        );
        let audit = registry
            .audit_records(player)
            .into_iter()
            .find(|audit| {
                audit.plan_id == plan_id && audit.disposition == PlanDisposition::Withdrawn
            })
            .expect("期限超過の撤回監査を残す");
        assert_eq!(audit.reason, Some(ReplanReason::ProductionStepFailed));
    }

    #[test]
    fn issued_but_unproduced_step_invalidates_continuation() {
        let player = PlayerId(1);
        let mut registry = V4RollingPlanRegistry::default();
        let selected = registry.select(
            player,
            3,
            OperationKind::Capture,
            GridPosition { x: 5, y: 5 },
            vec![GridPosition { x: 5, y: 5 }],
            HashSet::from([Entity::from_raw(10)]),
            None,
            feasible_plan(0),
            None,
            HashSet::new(),
        );
        let purchase = selected.plan.purchases[0];
        let step = registry
            .current_step_ref(
                selected.plan_id.expect("永続plan"),
                selected.revision.expect("revision"),
                3,
                purchase,
            )
            .expect("当手番step");
        registry.mark_issued(step, 3);

        let continuation = registry
            .continuation(
                player,
                4,
                OperationKind::Capture,
                &[GridPosition { x: 5, y: 5 }],
                &HashSet::from([Entity::from_raw(10)]),
            )
            .expect("失敗理由を付けて再評価するため計画自体は取得する");
        assert!(continuation.production_failed);
    }

    #[test]
    fn infeasible_candidate_is_never_persisted() {
        let player = PlayerId(1);
        let mut registry = V4RollingPlanRegistry::default();
        let mut impossible = feasible_plan(0);
        impossible.feasible = false;
        impossible.elimination_turn = None;
        impossible.occupation_turn = None;
        let selected = registry.select(
            player,
            3,
            OperationKind::Capture,
            GridPosition { x: 5, y: 5 },
            vec![GridPosition { x: 5, y: 5 }],
            HashSet::from([Entity::from_raw(10)]),
            None,
            impossible,
            None,
            HashSet::new(),
        );
        assert_eq!(selected.disposition, PlanDisposition::Rejected);
        assert!(selected.plan_id.is_none());
        assert!(
            registry
                .continuation(
                    player,
                    4,
                    OperationKind::Capture,
                    &[GridPosition { x: 5, y: 5 }],
                    &HashSet::from([Entity::from_raw(10)]),
                )
                .is_none()
        );
    }

    #[test]
    fn capital_progress_package_is_persisted_as_formation_not_attack() {
        let player = PlayerId(1);
        let enemy = Entity::from_raw(10);
        let mut registry = V4RollingPlanRegistry::default();
        let mut tranche = feasible_plan(2);
        tranche.feasible = false;
        tranche.elimination_turn = None;
        tranche.occupation_turn = None;
        tranche.target_forecasts = vec![super::super::rolling_plan::TargetForecast {
            entity: Some(enemy),
            unit_type: UnitType::Infantry,
            available_turn: 0,
            initial_hp: 100,
            remaining_hp: 40,
            destroyed_turn: None,
        }];

        let selected = registry.select(
            player,
            3,
            OperationKind::AssaultCapital,
            GridPosition { x: 9, y: 9 },
            vec![GridPosition { x: 9, y: 9 }],
            HashSet::from([enemy]),
            None,
            tranche,
            None,
            HashSet::new(),
        );

        assert_eq!(selected.disposition, PlanDisposition::Created);
        assert!(selected.plan_id.is_some());
        assert_eq!(registry.reserved_purchase_cost(player), 7_500);
        let intent = registry
            .active_deployment_intents(player, true, Some(GridPosition { x: 2, y: 2 }))
            .pop()
            .unwrap();
        assert_eq!(
            intent.posture,
            super::super::deployment::DeploymentPosture::Forming
        );
        assert_eq!(intent.staging_anchor, GridPosition { x: 2, y: 2 });
    }

    #[test]
    fn capture_progress_package_is_persisted_and_executes_immediately() {
        let player = PlayerId(1);
        let enemy = Entity::from_raw(10);
        let anchor = GridPosition { x: 5, y: 5 };
        let mut registry = V4RollingPlanRegistry::default();
        let mut tranche = feasible_plan(0);
        tranche.feasible = false;
        tranche.elimination_turn = None;
        tranche.occupation_turn = None;
        tranche.target_forecasts = vec![super::super::rolling_plan::TargetForecast {
            entity: Some(enemy),
            unit_type: UnitType::Infantry,
            available_turn: 0,
            initial_hp: 100,
            remaining_hp: 40,
            destroyed_turn: None,
        }];

        let selected = registry.select(
            player,
            3,
            OperationKind::Capture,
            anchor,
            vec![anchor],
            HashSet::from([enemy]),
            None,
            tranche,
            None,
            HashSet::new(),
        );

        assert_eq!(selected.disposition, PlanDisposition::Created);
        assert!(selected.plan_id.is_some());
        assert_eq!(registry.reserved_purchase_cost(player), 7_500);
        let intent = registry
            .active_deployment_intents(player, false, None)
            .pop()
            .unwrap();
        assert_eq!(
            intent.posture,
            super::super::deployment::DeploymentPosture::Execute
        );
        assert_eq!(intent.anchor, anchor);
    }

    #[test]
    fn execution_ledger_aggregates_production_attack_loss_and_target_hp() {
        let enemy = Entity::from_raw(10);
        let reinforcement = Entity::from_raw(11);
        let unit = Entity::from_raw(20);
        let objective = GridPosition { x: 5, y: 5 };
        let mut plan = feasible_plan(0);
        plan.target_forecasts = vec![super::super::rolling_plan::TargetForecast {
            entity: Some(enemy),
            unit_type: UnitType::Infantry,
            available_turn: 0,
            initial_hp: 100,
            remaining_hp: 0,
            destroyed_turn: Some(3),
        }];
        let mut ledger = PlanExecutionLedger::new(3, &HashSet::from([enemy]), &[objective], &plan);
        let step = PlanStepRef {
            plan_id: PlanId(1),
            revision: PlanRevision(0),
            step_id: PlanStepId(0),
        };
        ledger.observe(
            5,
            &HashSet::from([enemy, reinforcement]),
            &[objective],
            &HashSet::new(),
            &HashMap::from([(enemy, 40), (reinforcement, 100)]),
            &[DeploymentExecutionObservation {
                entity: unit,
                plan_id: PlanId(1),
                plan_step: Some(step),
                unit_cost: 7_500,
                alive: true,
                mission_active: true,
                current_loss_value: 750,
                first_attack_turn: Some(5),
                attack_count: 2,
                priority_attack_count: 1,
                kill_count: 0,
                damage_value_dealt: 4_000,
                counter_value_received: 500,
                destroyed_value: 0,
            }],
        );

        assert_eq!(ledger.snapshot.actual_production_cost, 7_500);
        assert_eq!(ledger.snapshot.produced_step_count, 1);
        assert_eq!(ledger.snapshot.assigned_entity_count, 1);
        assert_eq!(ledger.snapshot.active_entity_count, 1);
        assert_eq!(ledger.snapshot.actual_first_attack_turn, Some(5));
        assert_eq!(ledger.snapshot.attack_count, 2);
        assert_eq!(ledger.snapshot.damage_value_dealt, 4_000);
        assert_eq!(ledger.snapshot.current_force_loss, 750);
        assert_eq!(ledger.snapshot.remaining_target_count, 2);
        assert_eq!(ledger.snapshot.reinforcement_count, 1);
        assert!(
            ledger
                .snapshot
                .targets
                .iter()
                .any(|target| target.entity == reinforcement && target.reinforcement)
        );

        let mut revised = plan.clone();
        revised.production_cost = 16_000;
        ledger.update_forecast(5, &revised);
        assert_eq!(
            ledger.snapshot.planned_production_cost, 23_500,
            "revision予算は投入済み7,500と追加予定16,000の累計"
        );
    }

    #[test]
    fn enemy_elimination_does_not_complete_plan_until_objective_is_owned() {
        let player = PlayerId(1);
        let enemy = Entity::from_raw(10);
        let objective = GridPosition { x: 5, y: 5 };
        let enemies = HashSet::from([enemy]);
        let mut registry = V4RollingPlanRegistry::default();
        let selected = registry.select(
            player,
            3,
            OperationKind::Capture,
            objective,
            vec![objective],
            enemies.clone(),
            None,
            feasible_plan(1),
            None,
            HashSet::new(),
        );
        let plan_id = selected.plan_id.expect("永続plan");

        registry.observe_execution(player, 4, &HashSet::new(), &HashMap::new(), &[]);
        registry.reconcile_unseen_plans(player, 4, &HashSet::new());
        assert!(
            registry
                .continuation(player, 4, OperationKind::Capture, &[objective], &enemies)
                .is_some(),
            "敵を排除しただけでは占領作戦を完了しない"
        );

        registry.observe_execution(player, 4, &HashSet::from([objective]), &HashMap::new(), &[]);
        registry.reconcile_unseen_plans(player, 4, &HashSet::new());
        assert!(
            registry
                .continuation(player, 4, OperationKind::Capture, &[objective], &enemies)
                .is_none()
        );
        let audit = registry
            .audit_records(player)
            .into_iter()
            .find(|audit| {
                audit.plan_id == plan_id && audit.disposition == PlanDisposition::Completed
            })
            .expect("排除と占領の両方を満たした完了監査");
        assert_eq!(audit.execution.actual_elimination_turn, Some(4));
        assert_eq!(audit.execution.actual_occupation_turn, Some(4));
    }

    #[test]
    fn missed_execution_milestones_trigger_replanning_after_not_before_deadline() {
        let enemy = Entity::from_raw(10);
        let objective = GridPosition { x: 5, y: 5 };
        let plan = feasible_plan(1);
        let mut ledger = PlanExecutionLedger::new(3, &HashSet::from([enemy]), &[objective], &plan);
        ledger.observe(
            6,
            &HashSet::from([enemy]),
            &[objective],
            &HashSet::new(),
            &HashMap::from([(enemy, 100)]),
            &[],
        );
        assert_eq!(ledger.delay_reason(6), None);

        ledger.observe(
            7,
            &HashSet::from([enemy]),
            &[objective],
            &HashSet::new(),
            &HashMap::from([(enemy, 100)]),
            &[],
        );
        assert_eq!(
            ledger.delay_reason(7),
            Some(ReplanReason::FirstAttackDelayed)
        );
    }
}
