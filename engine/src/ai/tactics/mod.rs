//! 行動選択（戦術）。
//!
//! 1回の行動選択を「盤面の事実 → unitの事実 → 移動先候補 → 評価 → 選択」の順に組み立てる。
//! - `board`: 全unitが共有する盤面の事実（読み取り専用。行動を適用したら作り直す）
//! - `approach`: 目標への接近の測り方（ターン距離と格子距離）
//! - `unit`: 1体のunitの状態（版によらない部分）
//! - `scoring`: 版によらない採点項目と候補の比較
//! - `v2`: V2/V3と通常経路V4の行動選択
//! - `small_map`: 小規模マップV4の行動選択（DAG・占領契約・生産口・占位を加える）

pub(crate) mod approach;
pub(crate) mod blockade;
pub(crate) mod board;
pub(crate) mod scoring;
pub(crate) mod small_map;
pub(crate) mod unit;
pub(crate) mod v2;

pub(crate) mod campaign;
pub(crate) mod candidates;
pub(crate) mod distance_cache;
pub(crate) mod relief;
