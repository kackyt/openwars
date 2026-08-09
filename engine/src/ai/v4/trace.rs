//! V4 生産判断の構造化トレース。
//!
//! 「同じユニットばかりが大量に発注される」現象を、勝敗ではなく
//! **どの作戦のどの枠から出たか**で切り分けるための記録。
//!
//! 生産ループ（[`super::plan_production`]）は 1 反復ごとに
//! 「最も飢えた枠を選ぶ → 候補を選定する → 買う／枠を落とす／見送る」
//! を繰り返す。この 1 反復を [`ProductionStepTrace`] として残せば、
//! 同一ユニットの連続発注がどの枠の未充足率に駆動されていたかが確定する。
//!
//! 判定は行わない。値の記録だけを行う純粋なデータ構造である。

use super::operation::{OperationKind, OperationSlots, SlotKind};
use crate::components::{GridPosition, PlayerId};
use crate::resources::UnitType;
use bevy_ecs::prelude::*;
use std::collections::HashMap;

/// 1 枠分の購入判断の結末。
#[derive(Debug, Clone)]
pub enum ProductionDecision {
    /// 生産命令を発行した
    Produced {
        unit_type: UnitType,
        cost: u32,
        facility: GridPosition,
    },
    /// この枠を満たせる候補が無く、枠の要求を落とした
    SlotCleared,
    /// 見送り購入（資金を貯めるためループを打ち切った）
    Deferred { unit_type: UnitType, cost: u32 },
}

/// 生産ループ 1 反復分の記録。
#[derive(Debug, Clone)]
pub struct ProductionStepTrace {
    pub operation_kind: OperationKind,
    pub operation_anchor: GridPosition,
    pub slot_kind: SlotKind,
    /// 選定前の未充足率
    pub deficit_before: f32,
    /// 選定後の未充足率（購入以外は `deficit_before` と同じ）
    pub deficit_after: f32,
    /// この反復に入る時点の残額
    pub remaining_funds_before: u32,
    pub decision: ProductionDecision,
}

/// 作戦 1 件の要求。どの枠に何が要求されていたかを後から突き合わせるための情報。
#[derive(Debug, Clone)]
pub struct ProductionOperationTrace {
    pub kind: OperationKind,
    pub anchor: GridPosition,
    pub slots: OperationSlots,
    pub requires_transport: bool,
    pub enemy_combat_value: u32,
    /// 作戦期限までにこの前線へ到着できる敵生産分だけを数えた増援予算。
    pub enemy_reinforcement_budget: u32,
    /// 固定倍率の代わりに優越余裕として用いた、実在する最安対抗unitのcost。
    pub minimum_combat_unit_cost: u32,
    pub friendly_combat_value_committed: u32,
    pub deploy_lead_time: u32,
}

/// 1 ターン・1 プレイヤー分の生産計画トレース。
#[derive(Debug, Clone)]
pub struct ProductionPlanTrace {
    pub player_id: PlayerId,
    /// 生産フェーズ開始時の所持金
    pub funds: u32,
    /// 発注可能だった施設数
    pub free_facility_count: usize,
    /// 作戦が 1 つも立たず fallback に落ちたか
    pub fallback: bool,
    pub operations: Vec<ProductionOperationTrace>,
    pub steps: Vec<ProductionStepTrace>,
    /// 使い切れずに残った資金（余剰資金の積み上がりを検出する）
    pub leftover_funds: u32,
}

impl ProductionPlanTrace {
    /// 作戦が立たなかったターンの記録を作る。
    pub fn new(player_id: PlayerId, funds: u32, free_facility_count: usize) -> Self {
        Self {
            player_id,
            funds,
            free_facility_count,
            fallback: false,
            operations: Vec::new(),
            steps: Vec::new(),
            leftover_funds: funds,
        }
    }

    /// 実際に発注されたユニット種別ごとの体数。
    /// 「工場数 × ターン数に張り付いていないか」の判定に使う。
    pub fn produced_counts(&self) -> HashMap<UnitType, usize> {
        let mut counts = HashMap::new();
        for step in &self.steps {
            if let ProductionDecision::Produced { unit_type, .. } = &step.decision {
                *counts.entry(*unit_type).or_insert(0) += 1;
            }
        }
        counts
    }
}

/// 直近ターンの生産トレース。プレイヤーごとに手番内の判断を集約する。
#[derive(Resource, Debug, Clone, Default)]
pub struct ProductionTraceDiagnostics {
    pub by_player: HashMap<PlayerId, ProductionPlanTrace>,
    /// 同じプレイヤーの次手番で前回分を破棄するためのターン番号。
    recorded_turns: HashMap<PlayerId, u32>,
}

impl ProductionTraceDiagnostics {
    /// 1手番中の生産判断を集約する。
    ///
    /// AI は施設ごとに1命令ずつ返るため、`decide_production_v4` が同じ手番に複数回
    /// 呼ばれる。最後の呼び出しで上書きすると先に発注した施設の根拠が失われるため、
    /// 同一手番では step を連結し、次手番になった時点で新しい計画へ切り替える。
    pub fn record(&mut self, turn: u32, trace: ProductionPlanTrace) {
        let player_id = trace.player_id;
        let is_same_turn = self.recorded_turns.get(&player_id) == Some(&turn);

        if is_same_turn && let Some(existing) = self.by_player.get_mut(&player_id) {
            // 最初の盤面スナップショットを保持し、実際に辿った全判断だけを追記する。
            existing.fallback |= trace.fallback;
            existing.steps.extend(trace.steps);
            existing.leftover_funds = trace.leftover_funds;
            return;
        }

        self.recorded_turns.insert(player_id, turn);
        self.by_player.insert(player_id, trace);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_accumulates_all_decisions_within_one_turn() {
        let player = PlayerId(1);
        let mut diagnostics = ProductionTraceDiagnostics::default();
        let mut first = ProductionPlanTrace::new(player, 20_000, 2);
        first.leftover_funds = 10_000;
        first.steps.push(ProductionStepTrace {
            operation_kind: OperationKind::Capture,
            operation_anchor: GridPosition { x: 1, y: 1 },
            slot_kind: SlotKind::Capture,
            deficit_before: 1.0,
            deficit_after: 0.0,
            remaining_funds_before: 20_000,
            decision: ProductionDecision::SlotCleared,
        });
        let mut second = ProductionPlanTrace::new(player, 10_000, 1);
        second.leftover_funds = 5_000;
        second.steps.push(ProductionStepTrace {
            operation_kind: OperationKind::Capture,
            operation_anchor: GridPosition { x: 1, y: 1 },
            slot_kind: SlotKind::Combat,
            deficit_before: 1.0,
            deficit_after: 0.0,
            remaining_funds_before: 10_000,
            decision: ProductionDecision::SlotCleared,
        });

        diagnostics.record(3, first);
        diagnostics.record(3, second);

        let trace = diagnostics.by_player.get(&player).unwrap();
        assert_eq!(trace.funds, 20_000);
        assert_eq!(trace.free_facility_count, 2);
        assert_eq!(trace.leftover_funds, 5_000);
        assert_eq!(trace.steps.len(), 2);
    }

    #[test]
    fn record_replaces_the_previous_turn() {
        let player = PlayerId(1);
        let mut diagnostics = ProductionTraceDiagnostics::default();
        let mut first = ProductionPlanTrace::new(player, 20_000, 2);
        first.steps.push(ProductionStepTrace {
            operation_kind: OperationKind::Capture,
            operation_anchor: GridPosition { x: 1, y: 1 },
            slot_kind: SlotKind::Capture,
            deficit_before: 1.0,
            deficit_after: 0.0,
            remaining_funds_before: 20_000,
            decision: ProductionDecision::SlotCleared,
        });
        let second = ProductionPlanTrace::new(player, 8_000, 1);

        diagnostics.record(3, first);
        diagnostics.record(4, second);

        let trace = diagnostics.by_player.get(&player).unwrap();
        assert_eq!(trace.funds, 8_000);
        assert!(trace.steps.is_empty());
    }
}
