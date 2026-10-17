//! 生産要求の台帳。
//!
//! 作戦が必要とする枠（要求）は配分の途中で書き換えない。当手番に充足しない枠は
//! 要求を0にして隠すのではなく、どの枠をなぜ見送ったかを `SlotOutcome` として
//! 別に記録する。配分処理は `is_open` で未決着の枠だけを選び、traceや診断は
//! `outcomes` から未充足の理由を説明できる。合同パッケージが選んだ実効要求は
//! `effective_demands` に分け、元の要求や未充足の理由を上書きしない。

/// 要求枠を当手番で充足しない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnfilledReason {
    /// 予算・施設・到達性の制約を満たす購入候補が無い。
    NoCandidate,
    /// 戦闘パッケージ探索が候補案を返さなかった。
    NoForcePackage,
    /// 候補案が永続計画として採用されず、実行計画にならなかった。
    NoExecutablePlan,
    /// 同じ役割を島嶼campaign専用の生産が予約済み。
    CoveredByCampaign,
    /// 交戦中の前段区間を優先し、未許可首都作戦の構造枠を保留した。
    DeferredBehindActiveSegment,
}

/// 作戦の添字と枠種別で識別する要求枠。
///
/// 添字は1回の配分の中でだけ有効な識別子であり、手番を越えて保存しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DemandKey<S> {
    pub operation: usize,
    pub slot: S,
}

/// 未充足で決着した要求枠と、その理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotOutcome<S> {
    pub key: DemandKey<S>,
    pub reason: UnfilledReason,
}

/// 元の要求と異なる実効要求を採用した理由。未充足の理由とは別に保持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveDemandReason {
    /// 合同占領・戦闘パッケージが実行対象の占領役数を定めた。
    JointPackageSelected,
}

/// 配分中に採用した実効要求。元の要求は作戦側に不変で残す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveDemand<S> {
    pub key: DemandKey<S>,
    pub amount: u32,
    pub reason: EffectiveDemandReason,
}

/// 1回の生産配分における、要求枠ごとの未充足Outcome台帳。
///
/// 決着した枠は記録順を保持する（traceを決定的に出すため）。件数は作戦数×枠種別で
/// 小さいため、線形探索で十分。
#[derive(Debug, Clone)]
pub struct DemandLedger<S> {
    outcomes: Vec<SlotOutcome<S>>,
    effective_demands: Vec<EffectiveDemand<S>>,
}

impl<S> Default for DemandLedger<S> {
    fn default() -> Self {
        Self {
            outcomes: Vec::new(),
            effective_demands: Vec::new(),
        }
    }
}

impl<S: Copy + Eq> DemandLedger<S> {
    /// 空の台帳を作る。
    pub fn new() -> Self {
        Self::default()
    }

    /// 要求枠を未充足として決着させる。既に決着済みなら最初の理由を保持する。
    pub fn close(&mut self, operation: usize, slot: S, reason: UnfilledReason) {
        let key = DemandKey { operation, slot };
        if self.outcomes.iter().any(|outcome| outcome.key == key) {
            return;
        }
        self.outcomes.push(SlotOutcome { key, reason });
    }

    /// まだ配分対象として残っている枠か。
    pub fn is_open(&self, operation: usize, slot: S) -> bool {
        self.reason(operation, slot).is_none()
    }

    /// 決着済みの枠なら、その理由を返す。
    pub fn reason(&self, operation: usize, slot: S) -> Option<UnfilledReason> {
        let key = DemandKey { operation, slot };
        self.outcomes
            .iter()
            .find(|outcome| outcome.key == key)
            .map(|outcome| outcome.reason)
    }

    /// 実効要求を定め直したとき、以前の未充足の決着を取り消す。
    ///
    /// 例: 合同占領・戦闘計画が当手番の占領役数を確定した場合、それ以前の
    /// 「候補なし」は以前の実効要求に対する判断なので取り消す。元の要求は変えない。
    pub fn reopen(&mut self, operation: usize, slot: S) {
        let key = DemandKey { operation, slot };
        self.outcomes.retain(|outcome| outcome.key != key);
    }

    /// 決着した枠を記録順に返す。
    pub fn outcomes(&self) -> &[SlotOutcome<S>] {
        &self.outcomes
    }

    /// 実効要求だけを更新する。未充足の取消は従来と同じ位置で `reopen` を呼ぶ。
    pub fn set_effective(
        &mut self,
        operation: usize,
        slot: S,
        amount: u32,
        reason: EffectiveDemandReason,
    ) {
        let key = DemandKey { operation, slot };
        if let Some(demand) = self
            .effective_demands
            .iter_mut()
            .find(|demand| demand.key == key)
        {
            demand.amount = amount;
            demand.reason = reason;
        } else {
            self.effective_demands.push(EffectiveDemand {
                key,
                amount,
                reason,
            });
        }
    }

    /// 未充足の決着を除く前の実効要求。指定が無ければ元の要求を使う。
    pub fn effective_requirement(&self, operation: usize, slot: S, requested: u32) -> u32 {
        let key = DemandKey { operation, slot };
        self.effective_demands
            .iter()
            .find(|demand| demand.key == key)
            .map_or(requested, |demand| demand.amount)
    }

    /// 実効要求の採用理由を記録順に返す。未充足で閉じてもこの履歴は保持する。
    pub fn effective_demands(&self) -> &[EffectiveDemand<S>] {
        &self.effective_demands
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Slot {
        Capture,
        Combat,
    }

    #[test]
    fn effective_demand_survives_close_and_reopen_without_changing_requested() {
        let requested = 4;
        let mut ledger = DemandLedger::new();
        assert_eq!(ledger.effective_requirement(0, Slot::Capture, requested), 4);
        ledger.set_effective(
            0,
            Slot::Capture,
            2,
            EffectiveDemandReason::JointPackageSelected,
        );
        ledger.close(0, Slot::Capture, UnfilledReason::NoCandidate);
        assert_eq!(requested, 4);
        assert_eq!(ledger.effective_requirement(0, Slot::Capture, requested), 2);
        assert_eq!(
            ledger.reason(0, Slot::Capture),
            Some(UnfilledReason::NoCandidate)
        );
        ledger.reopen(0, Slot::Capture);
        assert!(ledger.is_open(0, Slot::Capture));
        assert_eq!(ledger.effective_requirement(0, Slot::Capture, requested), 2);
        assert_eq!(ledger.effective_requirement(1, Slot::Capture, requested), 4);
        assert_eq!(ledger.effective_requirement(0, Slot::Combat, 1), 1);
    }

    #[test]
    fn effective_demand_can_be_zero_or_exceed_requested_and_keeps_record_order() {
        let mut ledger = DemandLedger::new();
        ledger.set_effective(
            1,
            Slot::Capture,
            0,
            EffectiveDemandReason::JointPackageSelected,
        );
        assert_eq!(ledger.effective_requirement(1, Slot::Capture, 4), 0);
        ledger.set_effective(
            0,
            Slot::Capture,
            2,
            EffectiveDemandReason::JointPackageSelected,
        );
        ledger.set_effective(
            1,
            Slot::Capture,
            6,
            EffectiveDemandReason::JointPackageSelected,
        );
        assert_eq!(ledger.effective_requirement(1, Slot::Capture, 4), 6);
        assert_eq!(ledger.effective_demands().len(), 2);
        assert_eq!(ledger.effective_demands()[0].key.operation, 1);
        assert_eq!(ledger.effective_demands()[0].amount, 6);
        assert_eq!(ledger.effective_demands()[1].key.operation, 0);
    }

    #[test]
    fn closing_keeps_demand_and_records_reason() {
        let mut ledger = DemandLedger::new();
        assert!(ledger.is_open(0, Slot::Capture));
        ledger.close(0, Slot::Capture, UnfilledReason::NoCandidate);
        assert!(!ledger.is_open(0, Slot::Capture));
        // 別の作戦・別の枠には影響しない
        assert!(ledger.is_open(1, Slot::Capture));
        assert!(ledger.is_open(0, Slot::Combat));
        assert_eq!(
            ledger.reason(0, Slot::Capture),
            Some(UnfilledReason::NoCandidate)
        );
    }

    #[test]
    fn first_reason_wins_and_reopen_clears_it() {
        let mut ledger = DemandLedger::new();
        ledger.close(2, Slot::Combat, UnfilledReason::NoForcePackage);
        ledger.close(2, Slot::Combat, UnfilledReason::NoCandidate);
        assert_eq!(ledger.outcomes().len(), 1);
        assert_eq!(
            ledger.reason(2, Slot::Combat),
            Some(UnfilledReason::NoForcePackage)
        );
        ledger.reopen(2, Slot::Combat);
        assert!(ledger.is_open(2, Slot::Combat));
        assert!(ledger.outcomes().is_empty());
    }

    #[test]
    fn outcomes_preserve_record_order() {
        let mut ledger = DemandLedger::new();
        ledger.close(1, Slot::Combat, UnfilledReason::CoveredByCampaign);
        ledger.close(0, Slot::Capture, UnfilledReason::NoCandidate);
        let keys = ledger
            .outcomes()
            .iter()
            .map(|outcome| (outcome.key.operation, outcome.key.slot))
            .collect::<Vec<_>>();
        assert_eq!(keys, vec![(1, Slot::Combat), (0, Slot::Capture)]);
    }
}
