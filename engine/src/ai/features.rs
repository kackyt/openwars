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

/// 小規模マップのV4生産で、模擬戦に頼らない単一統合生産評価を使う。既定では無効。
pub fn unified_production_eval_enabled() -> bool {
    std::env::var_os("OPENWARS_UNIFIED_PRODUCTION").is_some()
}

/// 標準サイズを含む全マップで統合生産評価を試行するか（`OPENWARS_UNIFIED_PRODUCTION=all`）。既定では無効。
pub fn unified_production_all_maps_enabled() -> bool {
    std::env::var("OPENWARS_UNIFIED_PRODUCTION")
        .is_ok_and(|value| value.eq_ignore_ascii_case("all"))
}

/// 物件契約と保護供給の帰属を照合する任意の診断出力。既定では無効。
pub fn campaign_scope_audit_enabled() -> bool {
    std::env::var_os("OPENWARS_CAMPAIGN_SCOPE_AUDIT").is_some()
}

/// 前線帯フィルタ・占領要求の縮小・前線消耗を照合する任意の診断出力。既定では無効。
///
/// engine内ではcampaign目標の絞り込みを、mcp-server内では前線帯(y=5〜6)の
/// 1ステップ単位のユニット増減を記録する。ゲームの判定には一切関与しない。
pub fn campaign_front_audit_enabled() -> bool {
    std::env::var_os("OPENWARS_CAMPAIGN_FRONT_AUDIT").is_some()
}

/// 前線消耗トレースが監視するy座標帯。既定はmap_42の前線(5〜6)。
/// `OPENWARS_CAMPAIGN_FRONT_Y=4-6` のように他マップへ切り替えられる。
pub fn campaign_front_audit_band() -> (usize, usize) {
    // 診断専用の入力なので、不正値は既定帯へ戻して対局を止めない。
    let raw = std::env::var("OPENWARS_CAMPAIGN_FRONT_Y").unwrap_or_default();
    parse_campaign_front_audit_band(&raw)
}

/// `min-max` 形式の前線帯を解釈する。未指定・不正値はmap_42の前線(5〜6)へ戻す。
fn parse_campaign_front_audit_band(raw: &str) -> (usize, usize) {
    const DEFAULT: (usize, usize) = (5, 6);
    let Some((min, max)) = raw.split_once('-') else {
        return DEFAULT;
    };
    match (min.parse::<usize>(), max.parse::<usize>()) {
        (Ok(min), Ok(max)) if min <= max => (min, max),
        _ => DEFAULT,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_campaign_front_audit_band;

    #[test]
    fn 前線帯の未指定と不正値は既定帯へ戻す() {
        assert_eq!(parse_campaign_front_audit_band(""), (5, 6));
        assert_eq!(parse_campaign_front_audit_band("5"), (5, 6));
        assert_eq!(parse_campaign_front_audit_band("6-4"), (5, 6));
        assert_eq!(parse_campaign_front_audit_band("x-y"), (5, 6));
    }

    #[test]
    fn 前線帯は他マップ向けに変更できる() {
        assert_eq!(parse_campaign_front_audit_band("4-6"), (4, 6));
        assert_eq!(parse_campaign_front_audit_band("3-3"), (3, 3));
    }
}
