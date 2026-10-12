//! 生産要求の台帳。
//!
//! 作戦が必要とする枠（要求）は配分の途中で書き換えない。当手番に充足しない枠は
//! 要求を0にして隠すのではなく、どの枠をなぜ見送ったかを `SlotOutcome` として
//! 別に記録する。配分処理は `is_open` で未決着の枠だけを選び、traceや診断は
//! `outcomes` から未充足の理由を説明できる。

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

/// 1回の生産配分における、要求枠ごとの未充足Outcome台帳。
///
/// 決着した枠は記録順を保持する（traceを決定的に出すため）。件数は作戦数×枠種別で
/// 小さいため、線形探索で十分。
#[derive(Debug, Clone)]
pub struct DemandLedger<S> {
    outcomes: Vec<SlotOutcome<S>>,
}

impl<S> Default for DemandLedger<S> {
    fn default() -> Self {
        Self {
            outcomes: Vec::new(),
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

    /// 作戦計画が要求量そのものを定め直したとき、以前の決着を取り消す。
    ///
    /// 例: 合同占領・戦闘計画が当手番の占領役数を確定した場合、それ以前の
    /// 「候補なし」は古い要求に対する判断であり、新しい要求には適用しない。
    pub fn reopen(&mut self, operation: usize, slot: S) {
        let key = DemandKey { operation, slot };
        self.outcomes.retain(|outcome| outcome.key != key);
    }

    /// 決着した枠を記録順に返す。
    pub fn outcomes(&self) -> &[SlotOutcome<S>] {
        &self.outcomes
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
