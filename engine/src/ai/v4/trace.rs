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
use super::plan_revision::{PlanDisposition, PlanId, PlanRevision, ReplanReason};
use crate::ai::production::demand::{EffectiveDemand, UnfilledReason};
use crate::collections::HashMap;
use crate::components::{GridPosition, PlayerId};
use crate::resources::UnitType;
use bevy_ecs::prelude::*;
/// 1 枠分の購入判断の結末。
#[derive(Debug, Clone)]
pub enum ProductionDecision {
    /// 生産命令を発行した
    Produced {
        unit_type: UnitType,
        cost: u32,
        facility: GridPosition,
    },
    /// Must を確保した残額で、観測済み前線へ直ちに寄与する増援を発行した。
    ///
    /// 永続Combat計画の購入列ではないため、次手番にそのまま再利用する予約にはしない。
    /// ただし生産Entityには通常のCombat任務を付けるので、無所属戦力にはならない。
    ProducedImmediateReinforcement {
        unit_type: UnitType,
        cost: u32,
        facility: GridPosition,
    },
    /// 当手番はこの枠を充足しない。要求は残し、理由を記録する。
    Unfilled { reason: UnfilledReason },
    /// 見送り購入（資金を貯めるためループを打ち切った）
    Deferred { unit_type: UnitType, cost: u32 },
    /// 永続計画に将来手番の購入を予約し、当手番は実行対象がない。
    Reserved {
        unit_type: UnitType,
        cost: u32,
        build_turn: u32,
    },
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
    /// 永続Roadmap上の占領担当撃破に対する再契約が未完了か。
    pub capture_replacement_pending: bool,
    pub requires_transport: bool,
    pub enemy_combat_units: u32,
    /// 実生産履歴と敵施設の事前分布を基準に、作戦期限までにこの前線へ到着しうる
    /// 通常増援額。RollingPlanへ仮想敵として渡す継続生産の基準である。
    pub enemy_reinforcement_funds: u32,
    /// 敵が施設・収入をより強く使った場合の上側見積り。進撃Goは止めず、
    /// counter候補と次回再計画の診断に使う。
    pub enemy_stress_reinforcement_funds: u32,
    /// 観測後に間に合う具体的counter生産列の現在必要な予約額。
    pub contingency_reserve_funds: u32,
    pub reinforcement_contingencies: Vec<ReinforcementContingencyTrace>,
    pub deploy_lead_time: u32,
}

/// 配分終了時の需要。実効要求と実際に確保した量は同一とは限らない。
#[derive(Debug, Clone)]
pub struct ProductionDemandTrace {
    pub operation_kind: OperationKind,
    pub anchor: GridPosition,
    /// 分析が導出した元の要求。予算不足や合同パッケージで上書きしない。
    pub requested: OperationSlots,
    /// 当手番の配分対象。未充足で閉じた枠を除き、合同パッケージの量を反映する。
    pub effective: OperationSlots,
    /// 当手番に購入案として確保した量。命令発行・生産完了を意味しない。
    pub filled: OperationSlots,
    /// 合同パッケージなどが実効要求を定めた理由。未充足理由とは別に保持する。
    pub adjustments: Vec<EffectiveDemand<SlotKind>>,
}

/// 仮想敵を現在編成へ混ぜず、観測後に発動する条件付きcounter計画。
#[derive(Debug, Clone, Copy)]
pub struct ReinforcementContingencyTrace {
    pub enemy_type: UnitType,
    pub enemy_contact_turn: u32,
    pub counter_type: UnitType,
    pub counter_facility: GridPosition,
    pub counter_build_turn: u32,
    pub counter_contact_turn: u32,
    pub attacks_required: u32,
    pub reserve_cost: u32,
}

/// ローリングCombat計画で選んだ生産1件。
#[derive(Debug, Clone)]
pub struct RollingPurchaseTrace {
    pub unit_type: UnitType,
    pub facility: GridPosition,
    pub build_turn: u32,
    pub cost: u32,
}

/// 敵Entityごとの撃破予測。
#[derive(Debug, Clone)]
pub struct RollingTargetTrace {
    pub entity: Option<Entity>,
    pub unit_type: UnitType,
    /// 0は観測済み。1以上は敵生産・移動scenarioから予測した前線到着turn。
    pub available_turn: u32,
    pub initial_hp: u32,
    pub remaining_hp: u32,
    pub destroyed_turn: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub struct CampaignTurnForecastTrace {
    pub turn: u32,
    pub enemy_arrival_hp: u32,
    pub enemy_hp_removed: u32,
    pub friendly_hp_lost: u32,
    pub attack_count: u32,
}

/// 金額充足ではなく、実行可能な混成編成として選択したCombat計画。
#[derive(Debug, Clone)]
pub struct RollingCombatPlanTrace {
    pub plan_id: Option<PlanId>,
    pub revision: Option<PlanRevision>,
    pub disposition: PlanDisposition,
    pub replan_reason: Option<ReplanReason>,
    pub operation_kind: OperationKind,
    pub anchor: GridPosition,
    pub feasible: bool,
    pub purchases: Vec<RollingPurchaseTrace>,
    pub targets: Vec<RollingTargetTrace>,
    pub turn_forecasts: Vec<CampaignTurnForecastTrace>,
    pub first_attack_turn: Option<u32>,
    pub elimination_turn: Option<u32>,
    pub occupation_turn: Option<u32>,
    pub production_cost: u32,
    pub expected_loss: u32,
    pub surviving_combat_value: u32,
    pub required_overmatch_value: u32,
    pub overmatch_ready: bool,
    pub protected_unit_count: usize,
    pub protected_survivor_count: usize,
    pub required_capture_survivor_count: usize,
    pub candidates_considered: usize,
    pub candidates_pruned: usize,
    pub search_truncated: bool,
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
    /// 配分終了時のrequested/effective/filledと、実効要求を採用した理由。
    pub demands: Vec<ProductionDemandTrace>,
    pub steps: Vec<ProductionStepTrace>,
    /// 島ごとの実行可能な混成Combat計画。anchorごとに最終revisionだけを保持する。
    pub rolling_combat_plans: Vec<RollingCombatPlanTrace>,
    /// 生産後の現金残高。予約済みと無所属を含むため、単独では浪費判定に使わない。
    pub leftover_funds: u32,
    /// 永続作戦の将来購入へ既に帰属している現金。
    pub reserved_funds: u32,
    /// どの作戦・購入列にも帰属していない、真の余剰資金。
    pub uncommitted_funds: u32,
    /// 敵の実生産履歴からの次手番予測。要求量・予約額とは独立して記録する。
    pub enemy_production_forecast: EnemyProductionForecastTrace,
    /// 当手番に充足しなかった要求枠と理由。要求自体は `operations` に残る。
    pub unfilled_slots: Vec<UnfilledSlotTrace>,
}

/// 当手番に充足しなかった要求枠1件。
#[derive(Debug, Clone, Copy)]
pub struct UnfilledSlotTrace {
    pub operation_kind: OperationKind,
    pub anchor: GridPosition,
    pub slot_kind: SlotKind,
    /// 作戦が要求していた量（体数・スロット・計画起動フラグ）。
    pub requirement: u32,
    pub reason: UnfilledReason,
}

impl ProductionPlanTrace {
    /// 作戦が立たなかったターンの記録を作る。
    pub fn new(player_id: PlayerId, funds: u32, free_facility_count: usize) -> Self {
        Self {
            enemy_production_forecast: Default::default(),
            player_id,
            funds,
            free_facility_count,
            fallback: false,
            operations: Vec::new(),
            demands: Vec::new(),
            steps: Vec::new(),
            rolling_combat_plans: Vec::new(),
            leftover_funds: funds,
            reserved_funds: 0,
            uncommitted_funds: funds,
            unfilled_slots: Vec::new(),
        }
    }

    /// 実際に発注されたユニット種別ごとの体数。
    /// 「工場数 × ターン数に張り付いていないか」の判定に使う。
    pub fn produced_counts(&self) -> HashMap<UnitType, usize> {
        let mut counts = HashMap::default();
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
            for rolling in trace.rolling_combat_plans {
                existing.rolling_combat_plans.retain(|current| {
                    current.operation_kind != rolling.operation_kind
                        || current.anchor != rolling.anchor
                });
                existing.rolling_combat_plans.push(rolling);
            }
            existing.leftover_funds = trace.leftover_funds;
            existing.reserved_funds = trace.reserved_funds;
            existing.uncommitted_funds = trace.uncommitted_funds;
            return;
        }

        self.recorded_turns.insert(player_id, turn);
        self.by_player.insert(player_id, trace);
    }

    /// 指定手番の生産計画が将来の購入へ取り置いた資金。その手番の計画が無ければNone。
    pub fn reserved_funds(&self, player_id: PlayerId, turn: u32) -> Option<u32> {
        (self.recorded_turns.get(&player_id) == Some(&turn))
            .then(|| self.by_player.get(&player_id))
            .flatten()
            .map(|trace| trace.reserved_funds)
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
            decision: ProductionDecision::Unfilled {
                reason: UnfilledReason::NoCandidate,
            },
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
            decision: ProductionDecision::Unfilled {
                reason: UnfilledReason::NoCandidate,
            },
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
            decision: ProductionDecision::Unfilled {
                reason: UnfilledReason::NoCandidate,
            },
        });
        let second = ProductionPlanTrace::new(player, 8_000, 1);

        diagnostics.record(3, first);
        diagnostics.record(4, second);

        let trace = diagnostics.by_player.get(&player).unwrap();
        assert_eq!(trace.funds, 8_000);
        assert!(trace.steps.is_empty());
    }
}

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

/// 物件契約と保護供給の帰属を照合する任意の診断出力。
pub(in crate::ai::v4) fn campaign_scope_audit_enabled() -> bool {
    std::env::var_os("OPENWARS_CAMPAIGN_SCOPE_AUDIT").is_some()
}
