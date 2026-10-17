//! AIの手番進行（段取り・命令発行・結果反映）。個別戦略の判断本文は持たない。

pub(crate) mod turn;

pub(crate) mod issue;
pub(crate) mod production_queue;
pub(crate) mod state;
