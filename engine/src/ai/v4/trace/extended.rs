//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

/// 敵の実生産観測から作る、次手番1回分の予測と既済予測の誤差。
///
/// 予測は現在の必須戦力や資金予約には使わない。まず対戦ログで誤差を測り、
/// 予測器が追随できていることを確認するための診断値である。
#[derive(Debug, Clone, Copy, Default)]
pub struct EnemyProductionForecastTrace {
    pub expected_units_next_turn: u32,
    pub expected_cost_next_turn: u32,
    pub dominant_unit_type: Option<UnitType>,
    pub evaluated_samples: u32,
    pub mean_absolute_unit_error: u32,
    pub mean_absolute_cost_error: u32,
}

impl ProductionPlanTrace {
    /// 作戦が立たなかったターンの記録を作る。
    pub fn new_extended(player_id: PlayerId, funds: u32, free_facility_count: usize) -> Self {
        Self {
            player_id,
            funds,
            free_facility_count,
            fallback: false,
            operations: Vec::new(),
            steps: Vec::new(),
            rolling_combat_plans: Vec::new(),
            leftover_funds: funds,
            reserved_funds: 0,
            uncommitted_funds: funds,
            enemy_production_forecast: EnemyProductionForecastTrace::default(),
        }
    }

    /// 実際に発注されたユニット種別ごとの体数。
    /// 「工場数 × ターン数に張り付いていないか」の判定に使う。
    pub fn produced_counts_extended(&self) -> HashMap<UnitType, usize> {
        let mut counts = HashMap::new();
        for step in &self.steps {
            match &step.decision {
                ProductionDecision::Produced { unit_type, .. }
                | ProductionDecision::ProducedImmediateReinforcement { unit_type, .. } => {
                    *counts.entry(*unit_type).or_insert(0) += 1;
                }
                ProductionDecision::SlotCleared
                | ProductionDecision::Deferred { .. }
                | ProductionDecision::Reserved { .. } => {}
            }
        }
        counts
    }
}
