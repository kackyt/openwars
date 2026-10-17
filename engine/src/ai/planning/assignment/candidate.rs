//! 所属候補の優先順位。物理状態・履歴・作戦の順序はここで決める。

use super::squad_model::{MissionType, Squad, SquadId};
use crate::ai::operation_assignment::{OperationOwner, OperationUnitRole, UnitOperationAssignment};
use crate::components::PlayerId;

#[derive(Debug, Clone, Copy)]
pub(crate) struct SquadAssignmentCandidate {
    pub squad_id: SquadId,
    pub owner: OperationOwner,
    pub role: OperationUnitRole,
    pub physically_loaded: bool,
    pub arrived_at_campaign: bool,
    pub mission_rank: u8,
}

pub(crate) fn operation_owner_for_squad(squad: &Squad, player_id: PlayerId) -> OperationOwner {
    if let Some(island_id) = squad.plan.target_island {
        OperationOwner::Campaign {
            player_id,
            island_id,
        }
    } else {
        OperationOwner::TacticalSquad {
            player_id,
            squad_id: squad.id,
        }
    }
}

pub(crate) fn assignment_mission_rank(mission: &MissionType) -> u8 {
    match mission {
        MissionType::Transport => 0,
        MissionType::Capture => 1,
        MissionType::Attack => 2,
        MissionType::Defense => 3,
        MissionType::Reserve => 4,
    }
}

pub(crate) fn assignment_candidate_key(
    candidate: &SquadAssignmentCandidate,
    existing: Option<UnitOperationAssignment>,
) -> Option<(u8, u8, u32)> {
    if candidate.physically_loaded {
        return Some((0, candidate.mission_rank, candidate.squad_id.0));
    }
    if candidate.arrived_at_campaign {
        // 実際に目的島へ到達したEntityの現地Squadは、生産時予約や旧島bindingからの
        // 正式handoffとみなす。座標事実を伴わない重複Squadには適用しない。
        let arrived_role_rank = match candidate.role {
            OperationUnitRole::Member => 0,
            OperationUnitRole::Transport => 1,
            OperationUnitRole::DeliveredCargo => 2,
            OperationUnitRole::Cargo => 3,
        };
        return Some((2, arrived_role_rank, candidate.squad_id.0));
    }
    if existing.is_some_and(|assignment| {
        assignment.owner == candidate.owner && assignment.squad_id == Some(candidate.squad_id)
    }) {
        // 同一作戦に複数の実行Squadがある場合も、正式handoffが無い限り
        // 前手番の具体Squadを維持する。
        return Some((3, candidate.mission_rank, candidate.squad_id.0));
    }
    match existing {
        Some(assignment)
            if matches!(assignment.owner, OperationOwner::Campaign { .. })
                && assignment.owner != candidate.owner =>
        {
            // campaign間の移管は作戦plannerが明示する。Squadの重複だけでは横取りしない。
            None
        }
        Some(assignment) if assignment.owner == candidate.owner => {
            Some((4, candidate.mission_rank, candidate.squad_id.0))
        }
        Some(assignment)
            if matches!(assignment.owner, OperationOwner::TacticalSquad { .. })
                && matches!(candidate.owner, OperationOwner::Campaign { .. }) =>
        {
            Some((5, candidate.mission_rank, candidate.squad_id.0))
        }
        Some(_) => Some((7, candidate.mission_rank, candidate.squad_id.0)),
        None if matches!(candidate.owner, OperationOwner::Campaign { .. }) => {
            Some((5, candidate.mission_rank, candidate.squad_id.0))
        }
        None => Some((7, candidate.mission_rank, candidate.squad_id.0)),
    }
}
