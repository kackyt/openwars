//! V4 作戦計画契約（Plan Contract）モジュール。
//!
//! 対象物件ごとの `StrategicOperation` と局地 `RollingPlan` を親子結合し、
//! 同じ作戦IDを計画→生産→配備→戦闘→占領まで失わずに運ぶ。
//! 下流には軽量な `OperationBinding` のみを渡し、中央レジストリが正本契約を所有する。

use super::plan_revision::{PlanId, PlanRevision, PlanStepRef, ReplanReason};
use super::victory_roadmap::{OperationEntityRole, OperationPhase, StrategicOperationId};
use crate::collections::HashMap;
use crate::components::GridPosition;
use crate::resources::TurnNumber;
use bevy_ecs::prelude::*;
/// 作戦のアプローチ分類。
/// 物件レースの行動フェーズETAに基づき、成立可能な行動形態を決定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum OperationApproach {
    /// 敵より先に占領を完了できる場合の直取り。
    DirectCapture,
    /// 敵の占領完了前に合法な初撃を入れて阻止・遅延させる。
    Interdict,
    /// 直取りも妨害も不能、または敵所有の拠点を撃破後に奪回する。
    Recapture,
}

/// 作戦対象の物件（安定IDと現在座標）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TargetProperty {
    /// 物件エンティティ（安定ID）
    pub entity: Entity,
    /// 現在の盤面座標
    pub position: GridPosition,
}

/// 下流コンポーネント（PendingDeployment, Squad, 行動実行等）に運ぶ軽量作戦バインディング。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct OperationBinding {
    pub operation_id: StrategicOperationId,
    pub plan_step: Option<PlanStepRef>,
    pub role: OperationEntityRole,
    pub revision: PlanRevision,
}

/// V4における対象物件ごとの作戦計画契約。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationPlanContract {
    pub operation_id: StrategicOperationId,
    pub revision: PlanRevision,
    pub target: TargetProperty,
    pub approach: OperationApproach,
    pub phase: OperationPhase,
    pub deadline_turn: Option<TurnNumber>,
    pub plan_id: Option<PlanId>,
    pub combat_entity: Option<Entity>,
    pub capture_entity: Option<Entity>,
    pub production_source: Option<GridPosition>,
    pub deployment_target: Option<GridPosition>,
    pub is_active: bool,
}

/// ロール割当時のエラー。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum RoleAssignmentError {
    #[error(
        "Entity {entity:?} is already assigned to operation {existing_operation:?} as role {existing_role:?}"
    )]
    AlreadyAssigned {
        entity: Entity,
        existing_operation: StrategicOperationId,
        existing_role: OperationEntityRole,
    },
    #[error("Operation {0:?} not found")]
    OperationNotFound(StrategicOperationId),
}

/// 作戦計画契約の中央台帳。
#[derive(Resource, Debug, Default, Clone)]
pub(crate) struct PlanContractRegistry {
    /// 作戦IDごとの契約正本
    contracts: HashMap<StrategicOperationId, OperationPlanContract>,
    /// PlanId -> StrategicOperationId の親子マッピング
    plan_to_operation: HashMap<PlanId, StrategicOperationId>,
    /// Entity -> (StrategicOperationId, OperationEntityRole) の排他割当台帳
    entity_assignments: HashMap<Entity, (StrategicOperationId, OperationEntityRole)>,
    /// 物件Entity -> StrategicOperationId のマッピング（前線固定用）
    property_to_operation: HashMap<Entity, StrategicOperationId>,
    /// 生産済みEntity -> (OperationBinding, deployment target) の保持台帳
    entity_bindings: HashMap<Entity, (OperationBinding, GridPosition)>,
    /// 次に発番する作戦IDカウンター
    next_operation_id: u64,
}

impl PlanContractRegistry {
    /// 作戦契約のリビジョンを取得する。
    pub(crate) fn contract_revision(
        &self,
        operation_id: StrategicOperationId,
    ) -> Option<PlanRevision> {
        self.contracts.get(&operation_id).map(|c| c.revision)
    }

    /// 作戦契約のアプローチおよび配備目標を安全に更新する。
    pub(crate) fn update_approach_and_target(
        &mut self,
        operation_id: StrategicOperationId,
        approach: Option<OperationApproach>,
        deployment_target: Option<GridPosition>,
    ) {
        if let Some(contract) = self.contracts.get_mut(&operation_id) {
            if let Some(approach) = approach {
                contract.approach = approach;
            }
            if let Some(target) = deployment_target {
                contract.deployment_target = Some(target);
            }
        }
    }

    /// 契約を登録する。
    pub(crate) fn register_contract(&mut self, contract: OperationPlanContract) {
        if let Some(plan_id) = contract.plan_id {
            self.plan_to_operation
                .insert(plan_id, contract.operation_id);
        }
        self.property_to_operation
            .insert(contract.target.entity, contract.operation_id);
        self.contracts.insert(contract.operation_id, contract);
    }

    /// PlanId から親の StrategicOperationId を紐付ける。
    pub(crate) fn link_plan_to_operation(
        &mut self,
        plan_id: PlanId,
        operation_id: StrategicOperationId,
    ) {
        self.plan_to_operation.insert(plan_id, operation_id);
    }

    /// Entity にロールを割り当てる。
    /// 同一Entityが既に別の作戦や別のロールに割り当てられている場合は二重割当として拒否する。
    pub(crate) fn assign_role(
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
    pub(crate) fn maintain_or_replan(
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
        self.next_operation_id = self
            .next_operation_id
            .checked_add(1)
            .expect("StrategicOperationId overflow");
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

    /// 生産済みEntityへ作戦bindingとdeployment targetを関連付ける。
    pub(crate) fn reconcile_produced_binding(
        &mut self,
        entity: Entity,
        binding: OperationBinding,
        target: GridPosition,
    ) -> Result<(), RoleAssignmentError> {
        self.assign_role(binding.operation_id, entity, binding.role)?;
        self.entity_bindings.insert(entity, (binding, target));
        Ok(())
    }
}
