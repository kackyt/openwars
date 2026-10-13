//! 1回の生産配分における可変状態（資金・施設占有）。
//!
//! 要求（`demand`）は不変に保ち、配分の進行に伴って変わる残額・使用済み施設・
//! 施設を確保した作戦の優先度だけをここに集める。候補評価の途中で永続Registryを
//! 書き換えず、配分の結果は最終的な購入案として確定処理（`commit`）へ渡す。

use crate::collections::{HashMap, HashSet};
use crate::components::GridPosition;
/// 予約が成立しない理由。失敗時は資金・施設・所有者のすべてを維持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReservationError {
    #[error("生産施設は予約済み")]
    FacilityConflict,
    #[error("予約資金が不足: required={required}, available={available}")]
    BudgetUnavailable { required: u32, available: u32 },
}

/// 生産配分の可変状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocationState {
    remaining_funds: u32,
    used_facilities: HashSet<GridPosition>,
    /// 施設を確保した作戦の優先度（小さいほど上位）。上位作戦による競合と
    /// 同順位作戦同士の競合を区別するために保持する。
    facility_owners: HashMap<GridPosition, u32>,
}

impl AllocationState {
    /// 手番開始時の利用可能資金から配分を始める。
    pub fn new(funds: u32) -> Self {
        Self {
            remaining_funds: funds,
            used_facilities: HashSet::default(),
            facility_owners: HashMap::default(),
        }
    }

    /// 確保済みの購入を差し引いた残額。
    pub fn remaining_funds(&self) -> u32 {
        self.remaining_funds
    }

    /// 上位作戦の予約額を除いて、今の枠が使ってよい額。
    pub fn spendable_after(&self, reserved_for_others: u32) -> u32 {
        self.remaining_funds.saturating_sub(reserved_for_others)
    }

    /// 施設が当手番の購入案に既に使われているか。
    pub fn is_facility_used(&self, facility: &GridPosition) -> bool {
        self.used_facilities.contains(facility)
    }

    /// 使用済み施設の集合。既存の候補選定関数へ読み取り専用で渡す。
    pub fn used_facilities(&self) -> &HashSet<GridPosition> {
        &self.used_facilities
    }

    /// 与えられた施設のうち、まだ使われていない数。
    pub fn free_facility_count<'a>(
        &self,
        facilities: impl IntoIterator<Item = &'a GridPosition>,
    ) -> usize {
        facilities
            .into_iter()
            .filter(|facility| !self.used_facilities.contains(facility))
            .count()
    }

    /// 施設を確保した作戦の優先度。
    pub fn facility_owner(&self, facility: &GridPosition) -> Option<u32> {
        self.facility_owners.get(facility).copied()
    }

    /// 購入案1件を確保する。資金を差し引き、施設を確保作戦の優先度とともに占有する。
    pub fn try_claim(
        &mut self,
        facility: GridPosition,
        cost: u32,
        owner_rank: u32,
    ) -> Result<(), ReservationError> {
        // すべての前提を確認してから、一度だけ状態を確定する。
        if self.is_facility_used(&facility) {
            return Err(ReservationError::FacilityConflict);
        }
        if cost > self.remaining_funds {
            return Err(ReservationError::BudgetUnavailable {
                required: cost,
                available: self.remaining_funds,
            });
        }
        self.remaining_funds -= cost;
        self.used_facilities.insert(facility);
        self.facility_owners.insert(facility, owner_rank);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: usize, y: usize) -> GridPosition {
        GridPosition { x, y }
    }

    #[test]
    fn claim_spends_funds_and_occupies_facility() {
        let mut state = AllocationState::new(5000);
        let facilities = [pos(1, 1), pos(2, 2)];
        assert_eq!(state.free_facility_count(&facilities), 2);
        state.try_claim(pos(1, 1), 3000, 4).unwrap();
        assert_eq!(state.remaining_funds(), 2000);
        assert!(state.is_facility_used(&pos(1, 1)));
        assert_eq!(state.facility_owner(&pos(1, 1)), Some(4));
        assert_eq!(state.facility_owner(&pos(2, 2)), None);
        assert_eq!(state.free_facility_count(&facilities), 1);
    }

    #[test]
    fn spendable_never_underflows() {
        let mut state = AllocationState::new(1000);
        assert_eq!(state.spendable_after(400), 600);
        assert_eq!(state.spendable_after(4000), 0);
        assert!(state.try_claim(pos(0, 0), 5000, 0).is_err());
        assert_eq!(state.remaining_funds(), 1000);
    }

    #[test]
    fn unfilled_demand_survives_and_failed_reservation_is_atomic() {
        use crate::ai::production::demand::{DemandLedger, UnfilledReason};
        let mut ledger = DemandLedger::new();
        let mut state = AllocationState::new(5000);
        state.try_claim(pos(1, 1), 3000, 4).unwrap();
        let before = state.clone();
        assert_eq!(
            state.try_claim(pos(1, 1), 1000, 0),
            Err(ReservationError::FacilityConflict)
        );
        assert_eq!(state, before);
        assert_eq!(
            state.try_claim(pos(2, 2), 3000, 0),
            Err(ReservationError::BudgetUnavailable {
                required: 3000,
                available: 2000
            })
        );
        assert_eq!(state, before);
        ledger.close(0, "capture", UnfilledReason::NoCandidate);
        assert_eq!(
            ledger.reason(0, "capture"),
            Some(UnfilledReason::NoCandidate)
        );
        assert!(ledger.is_open(0, "combat"));
        // 失敗した施設を再利用でき、残額ぴったりの予約も成立する。
        state.try_claim(pos(2, 2), 2000, 1).unwrap();
        assert_eq!(state.remaining_funds(), 0);
        assert_eq!(state.facility_owner(&pos(1, 1)), Some(4));
    }
}
