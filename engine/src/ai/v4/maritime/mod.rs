//! V4の海上・航空戦域。
//!
//! 島で隔てられた盤面では、陸上部隊の作戦だけでは戦力が前線へ届かない。
//! このモジュールは次の3つを担う。
//!
//! - [`naval_combat`] — 戦艦の射撃・射撃位置取り・補給帰還（移動後に撃てない間接射撃）。
//! - [`sealift`] — 輸送船・輸送ヘリで、仕事の無い陸塊に取り残された地上部隊を
//!   未所有の拠点や敵の残る陸塊へ運ぶ。
//! - [`threat`] — 次の敵手番に受ける期待損害（敵が実際に隣接できるかで直接攻撃を数える）。
//! - [`production`] — 運べない地上部隊の買い足しを止め、余った資金で輸送部隊・戦艦・
//!   航空機を補う。
//!
//! 艦船を使うかどうかは [`theater::MaritimeTheater`] が盤面から判定する。港があっても
//! 陸続きで敵が艦船を持たない盤面では艦船を買わない。

pub(crate) mod force_balance;
pub(crate) mod landmass;
pub(crate) mod naval_combat;
pub(crate) mod production;
pub(crate) mod sealift;
pub(crate) mod theater;
pub(crate) mod threat;

#[cfg(test)]
pub(crate) mod test_support;

pub(crate) use naval_combat::decide_naval_action;
pub(crate) use production::{decide_supplementary_production, rejects_ground_purchase};
pub(crate) use sealift::{decide_port_relief, decide_sealift_action};
