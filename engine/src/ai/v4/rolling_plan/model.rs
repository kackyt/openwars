//! RollingPlanのmodel責務。
use super::{Entity, GridPosition, PlannedPurchase};

/// 多工場の現手番物件制御で、実シミュレーションへ渡せる候補数上限。
///
/// 工場数と各工場の到達可能物件がともに多い盤面では、現在手番だけの選択でも
/// 直積が指数的に増える。これは戦術上の評価値ではなく、実行時間を有限に保つための
/// 候補数上限である。上限に達するまでは従来どおり全候補を厳密にシミュレーションする。
pub(super) const MANY_FACTORY_PROPERTY_STATE_LIMIT: usize = 256;

/// 生産unitが観測敵へ初撃する際の、地形・手番順・射点を含む実行可能edge。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProductionAttackProjection {
    pub ready_turn: u32,
    pub firing_position: GridPosition,
    pub requires_movement: bool,
}

/// 初手の候補集合に適用する戦略パイプライン固有の制約。
///
/// 標準マップの探索へ小規模マップの役割下限や全額投入を混ぜないため、候補列挙の
/// 分岐ではなく入力時に一度だけ決定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpeningProductionPolicy {
    Standard,
    SmallMapExpansion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlannedCapturePurchase {
    pub purchase: PlannedPurchase,
    pub target: GridPosition,
    pub completion_turn: Option<u32>,
    /// この物件への進入を妨げる敵index。別レーンの容易な敵を倒しても、この契約を
    /// 成立扱いにしないため、物件と実在敵の対応を最後まで保持する。
    pub blocking_enemy_index: Option<usize>,
    /// 占領完了時点にこの購入unitが生存していたか。
    pub survived_at_completion: bool,
}

/// 戦闘シミュレーションで最初に成立した、購入unitと実在敵Entityの組合せ。
///
/// 生産後に標的を選び直すと、施設からの初撃ETAを比較した計画と別の正面へ移動してしまう。
/// 価格や兵種名ではなく、実シミュレーションで成立した最初の交戦を配備へ引き渡す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlannedCombatPurchase {
    pub purchase: PlannedPurchase,
    pub target: Entity,
}

#[derive(Debug, Clone)]
pub(super) struct SimCaptureContract {
    pub(super) target: GridPosition,
    pub(super) arrival_turn: u32,
    pub(super) remaining_durability: u32,
    pub(super) blocking_enemy_index: Option<usize>,
    pub(super) completed_turn: Option<u32>,
    pub(super) survived_at_completion: bool,
}
