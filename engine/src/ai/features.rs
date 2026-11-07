//! V4 AIの実験的フィーチャー・環境変数管理。
//! 常時有効化された基本機能（進軍路集中・保持・防衛、砲台運搬、陣地防御、全都市占領、前線共有等）は
//! 本体ロジックへ組み込み済み。このモジュールは現在比較検証中の試作スイッチを管理する。

/// GO判定後の敵首都への攻勢（OPENWARS_ASSAULT_GO）。検証中のため既定では無効。
pub fn assault_go_enabled() -> bool {
    std::env::var("OPENWARS_ASSAULT_GO").is_ok()
}

/// 狭路のある盤面でのGO継続前進。比較検証中は明示的に有効化する。
pub fn narrow_assault_enabled() -> bool {
    std::env::var_os("OPENWARS_NARROW_ASSAULT").is_some()
}

/// 当手番の継続購入を補助輸送車の先取りから保護する比較用スイッチ。
pub fn preserve_due_purchases_enabled() -> bool {
    std::env::var_os("OPENWARS_DUE_BUDGET").is_some()
}

/// 観測敵だけの敵別編成評価。未観測増援・兵種比率・同種上限を使用しない試作。
pub fn observed_composition_enabled() -> bool {
    std::env::var_os("OPENWARS_OBSERVED_COMPOSITION").is_some()
}

/// 購入ごとの予測（射撃回数・与被害価値・死亡手番）を生産計画トレースへ記録する。既定では無効。
/// 観測編成（`OPENWARS_OBSERVED_COMPOSITION`）の交戦シミュレーションでだけ値を持つ。
pub fn prediction_ledger_enabled() -> bool {
    std::env::var_os("OPENWARS_PREDICTION_LEDGER").is_some()
}

/// 武装輸送部隊を戦闘役として比較する第一試作。既定では無効。
pub fn production_role_candidates_enabled() -> bool {
    observed_composition_enabled()
        || std::env::var_os("OPENWARS_PRODUCTION_ROLE_CANDIDATES").is_some()
}

/// 購入期限とは別に、保持判定時点まで交戦を計算する第一試作。既定では無効。
pub fn production_hold_horizon_enabled() -> bool {
    observed_composition_enabled() || std::env::var_os("OPENWARS_PRODUCTION_HOLD_HORIZON").is_some()
}

/// 首都ルートDAGの担当分隊は、毎手番ビーム探索で目標を決め直さず、DAGの区間目標を
/// 行動の目標にする。標準経路の比較検証中は明示的に有効化する。
pub fn dag_squad_targets_enabled() -> bool {
    std::env::var_os("OPENWARS_DAG_SQUAD_TARGETS").is_some()
}

/// 攻勢（GO）の前、占領役でないAttack部隊は、敵を追わず敵首都への集結線まで前進する。
/// 標準経路の比較検証中は明示的に有効化する。
pub fn staging_line_enabled() -> bool {
    std::env::var_os("OPENWARS_STAGING_LINE").is_some()
}
