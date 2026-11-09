//! V4 AIの実験的フィーチャー・環境変数管理。
//! 常時有効化された基本機能（進軍路集中・保持・防衛、砲台運搬、陣地防御、全都市占領、前線共有等）は
//! 本体ロジックへ組み込み済み。このモジュールは診断・トレース用プローブのうち、
//! 生産計画とcampaign物件契約に関わるスイッチをまとめる。
//!
//! 注意: 他の `OPENWARS_*_DEBUG` / `TRACE` 系は依然として各モジュールに直書きされており、
//! ここへは集約されていない。全診断の一元化は別課題とする。

/// 購入ごとの予測（射撃回数・与被害価値・死亡手番）を生産計画トレースへ記録する。既定では無効。
pub fn prediction_ledger_enabled() -> bool {
    std::env::var_os("OPENWARS_PREDICTION_LEDGER").is_some()
}

/// 物件契約と保護供給の帰属を照合する任意の診断出力。既定では無効。
pub fn campaign_scope_audit_enabled() -> bool {
    std::env::var_os("OPENWARS_CAMPAIGN_SCOPE_AUDIT").is_some()
}

/// 前線帯フィルタと占領要求の縮小を照合する任意の診断出力。既定では無効。
pub fn campaign_front_audit_enabled() -> bool {
    std::env::var_os("OPENWARS_CAMPAIGN_FRONT_AUDIT").is_some()
}
