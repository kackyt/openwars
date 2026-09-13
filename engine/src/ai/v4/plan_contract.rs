//! V4 作戦計画契約（Plan Contract）モジュール。
//!
//! 対象物件ごとの `StrategicOperation` と局地 `RollingPlan` を親子結合し、
//! 同じ作戦IDを計画→生産→配備→戦闘→占領まで失わずに運ぶ。
//! 下流には軽量な `OperationBinding` のみを渡し、中央レジストリが正本契約を所有する。

use super::plan_revision::{PlanId, PlanRevision, PlanStepRef, ReplanReason};
use super::property_control::ActionPhase;
use super::victory_roadmap::{OperationEntityRole, OperationPhase, StrategicOperationId};
use crate::components::GridPosition;
use bevy_ecs::prelude::*;
use std::collections::HashMap;

/// 作戦のアプローチ分類。
/// 物件レースの行動フェーズETAに基づき、成立可能な行動形態を決定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationApproach {
    /// 敵より先に占領を完了できる場合の直取り。
    DirectCapture,
    /// 敵の占領完了前に合法な初撃を入れて阻止・遅延させる。
    Interdict,
    /// 直取りも妨害も不能、または敵所有の拠点を撃破後に奪回する。
    Recapture,
}

/// 作戦対象の物件（安定IDと現在座標）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetProperty {
    /// 物件エンティティ（安定ID）
    pub entity: Entity,
    /// 現在の盤面座標
    pub position: GridPosition,
}

/// 下流コンポーネント（PendingDeployment, Squad, 行動実行等）に運ぶ軽量作戦バインディング。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperationBinding {
    pub operation_id: StrategicOperationId,
    pub plan_step: Option<PlanStepRef>,
    pub role: OperationEntityRole,
    pub revision: PlanRevision,
}

/// V4における対象物件ごとの作戦計画契約。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationPlanContract {
    pub operation_id: StrategicOperationId,
    pub revision: PlanRevision,
    pub target: TargetProperty,
    pub approach: OperationApproach,
    pub phase: OperationPhase,
    pub deadline_turn: Option<u32>,
    pub plan_id: Option<PlanId>,
    pub combat_entity: Option<Entity>,
    pub capture_entity: Option<Entity>,
    pub production_source: Option<GridPosition>,
    pub deployment_target: Option<GridPosition>,
    pub is_active: bool,
}

/// ロール割当時のエラー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleAssignmentError {
    AlreadyAssigned {
        entity: Entity,
        existing_operation: StrategicOperationId,
        existing_role: OperationEntityRole,
    },
    OperationNotFound(StrategicOperationId),
}

/// リビジョン検証エラー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionValidationError {
    StaleRevision {
        current: PlanRevision,
        received: PlanRevision,
    },
    OperationNotFound(StrategicOperationId),
}

/// 作戦計画契約の中央台帳。
#[derive(Resource, Debug, Default, Clone)]
pub struct PlanContractRegistry {
    /// 作戦IDごとの契約正本
    pub contracts: HashMap<StrategicOperationId, OperationPlanContract>,
    /// PlanId -> StrategicOperationId の親子マッピング
    pub plan_to_operation: HashMap<PlanId, StrategicOperationId>,
    /// Entity -> (StrategicOperationId, OperationEntityRole) の排他割当台帳
    pub entity_assignments: HashMap<Entity, (StrategicOperationId, OperationEntityRole)>,
    /// 物件Entity -> StrategicOperationId のマッピング（前線固定用）
    pub property_to_operation: HashMap<Entity, StrategicOperationId>,
    /// 生産済みEntity -> (OperationBinding, deployment target) の保持台帳
    pub entity_bindings: HashMap<Entity, (OperationBinding, GridPosition)>,
    /// 次に発番する作戦IDカウンター
    next_operation_id: u64,
}

impl PlanContractRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 行動フェーズETAを比較し、適切な作戦アプローチを分類する。
    ///
    /// 自軍が敵より先に占領完了できる場合のみ DirectCapture とする。
    /// 敵が先着・先完了する場合、敵の占領完了前に合法な攻撃を入れられるなら Interdict、
    /// 攻撃も間に合わない場合は敵排除後の奪還（Recapture）へ分類する。
    pub fn classify_approach(
        my_completion: ActionPhase,
        enemy_completion: ActionPhase,
        attack_ready: Option<ActionPhase>,
    ) -> OperationApproach {
        if my_completion < enemy_completion {
            OperationApproach::DirectCapture
        } else if let Some(attack) = attack_ready
            && attack < enemy_completion
        {
            OperationApproach::Interdict
        } else {
            OperationApproach::Recapture
        }
    }

    /// 契約を登録する。
    pub fn register_contract(&mut self, contract: OperationPlanContract) {
        if let Some(plan_id) = contract.plan_id {
            self.plan_to_operation
                .insert(plan_id, contract.operation_id);
        }
        self.property_to_operation
            .insert(contract.target.entity, contract.operation_id);
        self.contracts.insert(contract.operation_id, contract);
    }

    /// PlanStepRef から親の StrategicOperationId を一意に解決する。
    /// 寿命の異なる PlanId と StrategicOperationId を統合せず、親子関係を通じて安全に解決する。
    pub fn resolve_parent_operation(&self, step_ref: &PlanStepRef) -> Option<StrategicOperationId> {
        self.plan_to_operation.get(&step_ref.plan_id).copied()
    }

    /// PlanId から親の StrategicOperationId を紐付ける。
    pub fn link_plan_to_operation(&mut self, plan_id: PlanId, operation_id: StrategicOperationId) {
        self.plan_to_operation.insert(plan_id, operation_id);
    }

    /// Entity にロールを割り当てる。
    /// 同一Entityが既に別の作戦や別のロールに割り当てられている場合は二重割当として拒否する。
    pub fn assign_role(
        &mut self,
        operation_id: StrategicOperationId,
        entity: Entity,
        role: OperationEntityRole,
    ) -> Result<(), RoleAssignmentError> {
        if let Some(&(existing_op, existing_role)) = self.entity_assignments.get(&entity) {
            return Err(RoleAssignmentError::AlreadyAssigned {
                entity,
                existing_operation: existing_op,
                existing_role,
            });
        }

        let contract = self
            .contracts
            .get_mut(&operation_id)
            .ok_or(RoleAssignmentError::OperationNotFound(operation_id))?;

        match role {
            OperationEntityRole::Combat => {
                contract.combat_entity = Some(entity);
            }
            OperationEntityRole::Capture => {
                contract.capture_entity = Some(entity);
            }
            OperationEntityRole::Transport => {}
        }

        self.entity_assignments.insert(entity, (operation_id, role));
        Ok(())
    }

    /// ReplanReason なしの連続戦略準備で、契約（対象・作戦ID・revision）を維持する。
    /// 明示的な ReplanReason がある場合のみ revision を進めるか新契約を組む。
    pub fn maintain_or_replan(
        &mut self,
        target_entity: Entity,
        target_pos: GridPosition,
        replan_reason: Option<ReplanReason>,
    ) -> StrategicOperationId {
        if let Some(&existing_op) = self.property_to_operation.get(&target_entity) {
            if replan_reason.is_none() {
                // 理由なき前線転進・再計画を防止し、既存の契約IDとrevisionを維持する
                return existing_op;
            }
            // 明示的な理由がある場合は revision を更新する
            if let Some(contract) = self.contracts.get_mut(&existing_op) {
                contract.revision = PlanRevision(contract.revision.0.saturating_add(1));
                contract.target.position = target_pos;
                return existing_op;
            }
        }

        // 新規作成
        self.next_operation_id = self.next_operation_id.saturating_add(1);
        let op_id = StrategicOperationId(self.next_operation_id);
        let contract = OperationPlanContract {
            operation_id: op_id,
            revision: PlanRevision(1),
            target: TargetProperty {
                entity: target_entity,
                position: target_pos,
            },
            approach: OperationApproach::DirectCapture,
            phase: OperationPhase::Forming,
            deadline_turn: None,
            plan_id: None,
            combat_entity: None,
            capture_entity: None,
            production_source: None,
            deployment_target: Some(target_pos),
            is_active: true,
        };
        self.register_contract(contract);
        op_id
    }

    /// 実行コンテキストの PlanRevision を検証する。
    /// stale なリビジョンは拒否する。
    pub fn validate_binding(
        &self,
        binding: &OperationBinding,
    ) -> Result<(), RevisionValidationError> {
        let contract = self.contracts.get(&binding.operation_id).ok_or(
            RevisionValidationError::OperationNotFound(binding.operation_id),
        )?;

        if contract.revision != binding.revision {
            return Err(RevisionValidationError::StaleRevision {
                current: contract.revision,
                received: binding.revision,
            });
        }
        Ok(())
    }

    /// 生産ソース喪失、Combat役消滅、deadline超過を ReplanReason に接続する。
    pub fn check_replan_triggers(
        &self,
        operation_id: StrategicOperationId,
        source_available: bool,
        combat_alive: bool,
        current_turn: u32,
    ) -> Option<ReplanReason> {
        let contract = self.contracts.get(&operation_id)?;

        if !source_available {
            return Some(ReplanReason::ProductionSlotUnavailable);
        }
        if !combat_alive {
            return Some(ReplanReason::ContinuationInfeasible);
        }
        if let Some(deadline) = contract.deadline_turn
            && current_turn > deadline
        {
            return Some(ReplanReason::HardDeadlineMissed);
        }
        None
    }

    /// 生産済みEntityへ作戦bindingとdeployment targetを関連付ける。
    pub fn reconcile_produced_binding(
        &mut self,
        entity: Entity,
        binding: OperationBinding,
        target: GridPosition,
    ) {
        self.entity_bindings.insert(entity, (binding, target));
        let _ = self.assign_role(binding.operation_id, entity, binding.role);
    }

    /// Entityのbindingとdeployment targetを取得する。
    pub fn get_entity_binding(&self, entity: Entity) -> Option<(OperationBinding, GridPosition)> {
        self.entity_bindings.get(&entity).copied()
    }

    /// 監査用にEntityが属するStrategicOperationIdを取得する。
    pub fn audit_operation_for_entity(&self, entity: Entity) -> Option<StrategicOperationId> {
        self.entity_assignments.get(&entity).map(|(op, _)| *op)
    }

    /// Entityの所属作戦の目標位置をread-only解決する。
    pub fn resolve_entity_target(&self, entity: Entity) -> Option<GridPosition> {
        if let Some((_, target)) = self.get_entity_binding(entity) {
            return Some(target);
        }
        let (op_id, _) = self.entity_assignments.get(&entity)?;
        let contract = self.contracts.get(op_id)?;
        contract
            .deployment_target
            .or(Some(contract.target.position))
    }

    /// Entityの所属作戦の最新アプローチをread-only解決する。
    pub fn resolve_entity_approach(&self, entity: Entity) -> Option<OperationApproach> {
        let (op_id, _) = self.entity_assignments.get(&entity)?;
        let contract = self.contracts.get(op_id)?;
        Some(contract.approach)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1. 敵DirectCapture ETAが自軍より早い場合、DirectCaptureのままにせずInterdict/Recaptureへ分類する。
    #[test]
    fn test_1_enemy_direct_capture_faster_classified_as_interdict_or_recapture() {
        // 自軍: round 3, player_order 1 (後手) で占領完了
        let my_completion = ActionPhase::new(3, 1);
        // 敵軍: round 3, player_order 0 (先手) で占領完了 (自軍より早い)
        let enemy_completion = ActionPhase::new(3, 0);

        // 攻撃役が敵完了より前に攻撃可能 (round 2, player_order 1 < (3, 0)) -> Interdict
        let attack_ready = Some(ActionPhase::new(2, 1));
        let approach =
            PlanContractRegistry::classify_approach(my_completion, enemy_completion, attack_ready);
        assert_ne!(
            approach,
            OperationApproach::DirectCapture,
            "敵完了が早い場合はDirectCaptureにしてはならない"
        );
        assert_eq!(
            approach,
            OperationApproach::Interdict,
            "敵完了前に攻撃可能ならInterdictへ分類する"
        );

        // 攻撃役が間に合わない (None) -> Recapture
        let approach_recapture =
            PlanContractRegistry::classify_approach(my_completion, enemy_completion, None);
        assert_ne!(
            approach_recapture,
            OperationApproach::DirectCapture,
            "直取り不能時はDirectCaptureにしてはならない"
        );
        assert_eq!(
            approach_recapture,
            OperationApproach::Recapture,
            "攻撃役が間に合わない場合はRecaptureへ分類する"
        );
    }

    /// 2. Combat/Capture roleが同じ StrategicOperationId に所属し、同一Entityの不正な二重割当を拒否する。
    #[test]
    fn test_2_combat_and_capture_roles_belong_to_same_operation_rejects_duplicate_assignment() {
        let mut registry = PlanContractRegistry::new();
        let op1 = StrategicOperationId(1);
        let op2 = StrategicOperationId(2);
        let entity_combat = Entity::from_raw(10);
        let entity_capture = Entity::from_raw(20);

        // op1, op2 の契約を事前に登録
        for &op in &[op1, op2] {
            registry.register_contract(OperationPlanContract {
                operation_id: op,
                revision: PlanRevision(1),
                target: TargetProperty {
                    entity: Entity::from_raw((100 + op.0) as u32),
                    position: GridPosition { x: 0, y: 0 },
                },
                approach: OperationApproach::DirectCapture,
                phase: OperationPhase::Forming,
                deadline_turn: None,
                plan_id: None,
                combat_entity: None,
                capture_entity: None,
                production_source: None,
                deployment_target: None,
                is_active: true,
            });
        }

        // 同一作戦に別々のEntityを Combat と Capture として割り当てる -> 成功
        assert!(
            registry
                .assign_role(op1, entity_combat, OperationEntityRole::Combat)
                .is_ok()
        );
        assert!(
            registry
                .assign_role(op1, entity_capture, OperationEntityRole::Capture)
                .is_ok()
        );

        // 同一Entity (entity_combat) を別ロール Capture として二重割当しようとする -> 拒否
        let dup_role = registry.assign_role(op1, entity_combat, OperationEntityRole::Capture);
        assert!(
            matches!(dup_role, Err(RoleAssignmentError::AlreadyAssigned { .. })),
            "同一Entityのロール二重割当は拒否されなければならない"
        );

        // 同一Entity (entity_combat) を別作戦 op2 に割り当てようとする -> 拒否
        let dup_op = registry.assign_role(op2, entity_combat, OperationEntityRole::Combat);
        assert!(
            matches!(dup_op, Err(RoleAssignmentError::AlreadyAssigned { .. })),
            "同一Entityの他作戦二重割当は拒否されなければならない"
        );
    }

    /// 3. ReplanReason なしの連続strategy preparationで、対象・operation ID・revisionを維持する。
    #[test]
    fn test_3_consecutive_strategy_preparation_maintains_target_and_revision_without_replan_reason()
    {
        let mut registry = PlanContractRegistry::new();
        let target_property = Entity::from_raw(100);
        let target_pos = GridPosition { x: 5, y: 5 };

        // 1回目の初期作戦作成
        let initial_op = registry.maintain_or_replan(
            target_property,
            target_pos,
            Some(ReplanReason::InitialPlan),
        );

        // 2回目のターン: ReplanReasonなしで更新
        let second_op = registry.maintain_or_replan(target_property, target_pos, None);

        // 同じ物件に対して ReplanReason がなければ作戦IDと対象を維持する
        assert_eq!(
            initial_op, second_op,
            "ReplanReasonがない場合は同じoperation IDを維持しなければならない"
        );
        let contract = registry
            .contracts
            .get(&second_op)
            .expect("契約が存在すること");
        assert_eq!(contract.target.entity, target_property);
        assert_eq!(contract.revision, PlanRevision(1));
    }

    /// 4. PlanStepRef から親operationを一意に解決できる。異なる寿命のID型を統合しない。
    #[test]
    fn test_4_plan_step_ref_resolves_unique_parent_operation() {
        let mut registry = PlanContractRegistry::new();
        let op_id = StrategicOperationId(42);
        let plan_id = PlanId(1001);
        let target = TargetProperty {
            entity: Entity::from_raw(200),
            position: GridPosition { x: 3, y: 3 },
        };

        let contract = OperationPlanContract {
            operation_id: op_id,
            revision: PlanRevision(1),
            target,
            approach: OperationApproach::Interdict,
            phase: OperationPhase::Forming,
            deadline_turn: Some(10),
            plan_id: Some(plan_id),
            combat_entity: None,
            capture_entity: None,
            production_source: Some(GridPosition { x: 1, y: 1 }),
            deployment_target: Some(GridPosition { x: 3, y: 3 }),
            is_active: true,
        };
        registry.register_contract(contract);

        let step_ref = PlanStepRef {
            plan_id,
            revision: PlanRevision(1),
            step_id: super::super::plan_revision::PlanStepId(1),
        };

        let resolved = registry.resolve_parent_operation(&step_ref);
        assert_eq!(
            resolved,
            Some(op_id),
            "PlanStepRefから親のStrategicOperationIdを一意に解決できなければならない"
        );
    }

    /// 5. production intent→PendingDeployment→生産済みEntity reconciliationでoperation/step/role/deployment targetを失わない。
    #[test]
    fn test_5_pending_deployment_reconciliation_preserves_contract_binding() {
        let mut registry = PlanContractRegistry::new();
        let op_id = StrategicOperationId(55);
        let plan_id = PlanId(2002);
        let step_ref = PlanStepRef {
            plan_id,
            revision: PlanRevision(1),
            step_id: super::super::plan_revision::PlanStepId(2),
        };

        let binding = OperationBinding {
            operation_id: op_id,
            plan_step: Some(step_ref),
            role: OperationEntityRole::Combat,
            revision: PlanRevision(1),
        };

        let deployment_target = GridPosition { x: 7, y: 7 };
        let produced_entity = Entity::from_raw(777);

        // 生産意図から実Entityへのreconciliationシミュレーション
        registry.reconcile_produced_binding(produced_entity, binding, deployment_target);

        let retrieved = registry.get_entity_binding(produced_entity);
        assert!(
            retrieved.is_some(),
            "生産済みEntityにbindingが保持されること"
        );
        let (r_binding, r_target) = retrieved.unwrap();
        assert_eq!(r_binding.operation_id, op_id);
        assert_eq!(r_binding.plan_step, Some(step_ref));
        assert_eq!(r_binding.role, OperationEntityRole::Combat);
        assert_eq!(r_target, deployment_target);
    }

    /// 6. stale PlanRevision のexecution contextを拒否または最新契約へ再解決する。
    #[test]
    fn test_6_stale_plan_revision_execution_context_rejected() {
        let mut registry = PlanContractRegistry::new();
        let op_id = StrategicOperationId(66);
        let target = TargetProperty {
            entity: Entity::from_raw(300),
            position: GridPosition { x: 4, y: 4 },
        };

        // 最新契約は revision 2
        let contract = OperationPlanContract {
            operation_id: op_id,
            revision: PlanRevision(2),
            target,
            approach: OperationApproach::Recapture,
            phase: OperationPhase::Forming,
            deadline_turn: Some(15),
            plan_id: None,
            combat_entity: None,
            capture_entity: None,
            production_source: None,
            deployment_target: None,
            is_active: true,
        };
        registry.register_contract(contract);

        // 古い revision 1 を持つ binding
        let stale_binding = OperationBinding {
            operation_id: op_id,
            plan_step: None,
            role: OperationEntityRole::Combat,
            revision: PlanRevision(1),
        };

        let result = registry.validate_binding(&stale_binding);
        assert!(
            matches!(
                result,
                Err(RevisionValidationError::StaleRevision {
                    current: PlanRevision(2),
                    received: PlanRevision(1)
                })
            ),
            "古いPlanRevisionの実行コンテキストは拒否されなければならない"
        );

        // 最新 revision 2 を持つ binding は通過
        let fresh_binding = OperationBinding {
            operation_id: op_id,
            plan_step: None,
            role: OperationEntityRole::Combat,
            revision: PlanRevision(2),
        };
        assert!(registry.validate_binding(&fresh_binding).is_ok());
    }

    /// 7. production source喪失、Combat role消滅、deadline超過をtyped replan reasonへ接続する。
    #[test]
    fn test_7_replan_reasons_connected_for_source_loss_role_death_and_deadline() {
        let mut registry = PlanContractRegistry::new();
        let op_id = StrategicOperationId(77);
        let target = TargetProperty {
            entity: Entity::from_raw(400),
            position: GridPosition { x: 2, y: 2 },
        };

        let contract = OperationPlanContract {
            operation_id: op_id,
            revision: PlanRevision(1),
            target,
            approach: OperationApproach::Interdict,
            phase: OperationPhase::Forming,
            deadline_turn: Some(10),
            plan_id: None,
            combat_entity: None,
            capture_entity: None,
            production_source: Some(GridPosition { x: 0, y: 0 }),
            deployment_target: Some(GridPosition { x: 2, y: 2 }),
            is_active: true,
        };
        registry.register_contract(contract);

        // 正常時: replan reasonなし
        assert_eq!(
            registry.check_replan_triggers(op_id, true, true, 5),
            None,
            "正常時はreplan不要"
        );

        // (a) 生産ソース喪失
        assert_eq!(
            registry.check_replan_triggers(op_id, false, true, 5),
            Some(ReplanReason::ProductionSlotUnavailable),
            "生産ソース喪失はProductionSlotUnavailableへ接続する"
        );

        // (b) Combat役消滅
        assert_eq!(
            registry.check_replan_triggers(op_id, true, false, 5),
            Some(ReplanReason::ContinuationInfeasible),
            "Combat役消滅はContinuationInfeasibleへ接続する"
        );

        // (c) deadline超過 (現在ターン11 > deadline 10)
        assert_eq!(
            registry.check_replan_triggers(op_id, true, true, 11),
            Some(ReplanReason::HardDeadlineMissed),
            "deadline超過はHardDeadlineMissedへ接続する"
        );
    }

    /// 8. 可能ならAttackと後続Captureの監査traceが同じoperation IDを参照する。
    #[test]
    fn test_8_attack_and_subsequent_capture_audit_trace_reference_same_operation_id() {
        let mut registry = PlanContractRegistry::new();
        let op_id = StrategicOperationId(88);
        let target = TargetProperty {
            entity: Entity::from_raw(500),
            position: GridPosition { x: 6, y: 6 },
        };

        let contract = OperationPlanContract {
            operation_id: op_id,
            revision: PlanRevision(1),
            target,
            approach: OperationApproach::Interdict,
            phase: OperationPhase::Forming,
            deadline_turn: Some(12),
            plan_id: None,
            combat_entity: None,
            capture_entity: None,
            production_source: None,
            deployment_target: None,
            is_active: true,
        };
        registry.register_contract(contract);

        let combat_entity = Entity::from_raw(81);
        let capture_entity = Entity::from_raw(82);

        // ロール割当と監査トレースシミュレーション
        registry
            .assign_role(op_id, combat_entity, OperationEntityRole::Combat)
            .unwrap();
        registry
            .assign_role(op_id, capture_entity, OperationEntityRole::Capture)
            .unwrap();

        // 監査トレースレコード
        let attack_trace_op = registry.audit_operation_for_entity(combat_entity);
        let capture_trace_op = registry.audit_operation_for_entity(capture_entity);

        assert_eq!(
            attack_trace_op,
            Some(op_id),
            "Attack監査traceが作戦IDを参照すること"
        );
        assert_eq!(
            capture_trace_op,
            Some(op_id),
            "後続Capture監査traceが作戦IDを参照すること"
        );
        assert_eq!(
            attack_trace_op, capture_trace_op,
            "AttackとCaptureで同じoperation IDを参照すること"
        );
    }
}
