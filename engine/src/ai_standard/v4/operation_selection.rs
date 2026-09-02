//! 戦術作戦候補の同時実行集合を選ぶ。
//!
//! 島campaignと永続Planが選んだ作戦は上位戦略の決定なので、固定件数で
//! 切り捨てない。追加の局地候補だけを基準容量へ収め、戦力分散を抑える。

use super::operation::OperationKind;
use crate::components::GridPosition;

/// 戦略選択済み作戦が少ない場合に許す、同時戦術作戦の基準容量。
const BASE_OPERATION_CAPACITY: usize = 4;

#[derive(Debug, Clone)]
pub(super) struct OperationCandidate {
    pub continuing: bool,
    pub lead: u32,
    pub kind: OperationKind,
    pub cluster: Vec<GridPosition>,
    /// campaignまたは永続Planが所有しており、局地候補の上限で落としてはならない。
    pub required: bool,
}

/// 優先度順の候補から、全必須作戦と容量内の追加作戦を選ぶ。
pub(super) fn select_operation_candidates(
    candidates: Vec<OperationCandidate>,
) -> Vec<OperationCandidate> {
    let required_count = candidates
        .iter()
        .filter(|candidate| candidate.required)
        .count();
    let capacity = BASE_OPERATION_CAPACITY.max(required_count);
    let optional_capacity = capacity.saturating_sub(required_count);
    let mut selected_optional = 0_usize;

    candidates
        .into_iter()
        .filter(|candidate| {
            if candidate.required {
                true
            } else if selected_optional < optional_capacity {
                selected_optional += 1;
                true
            } else {
                false
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(index: usize, required: bool) -> OperationCandidate {
        OperationCandidate {
            continuing: required,
            lead: u32::try_from(index).unwrap(),
            kind: OperationKind::Capture,
            cluster: vec![GridPosition { x: index, y: 0 }],
            required,
        }
    }

    #[test]
    fn all_strategy_owned_operations_survive_the_base_capacity() {
        let selected =
            select_operation_candidates((0..6).map(|index| candidate(index, true)).collect());

        assert_eq!(selected.len(), 6);
        assert!(selected.iter().all(|operation| operation.required));
    }

    #[test]
    fn optional_operations_are_limited_without_evicting_required_work() {
        let mut candidates = vec![candidate(0, true), candidate(1, true)];
        candidates.extend((2..8).map(|index| candidate(index, false)));

        let selected = select_operation_candidates(candidates);

        assert_eq!(selected.len(), BASE_OPERATION_CAPACITY);
        assert_eq!(
            selected
                .iter()
                .filter(|operation| operation.required)
                .count(),
            2
        );
        assert_eq!(selected[2].cluster[0].x, 2);
        assert_eq!(selected[3].cluster[0].x, 3);
    }
}
