//! V4 AIのフィーチャーフラグ管理。
//! Cargo feature または環境変数によって有効化される。
//! WASM環境（ブラウザ）では環境変数が存在しないため、Cargo feature または wasm32 ターゲットで常時有効化する。

/// 進軍路ごとの攻守決定・戦力配分案（OPENWARS_CONCENTRATE）
pub fn route_concentrate_enabled() -> bool {
    cfg!(target_arch = "wasm32")
        || cfg!(feature = "route_concentrate")
        || std::env::var("OPENWARS_CONCENTRATE").is_ok()
}

/// 首都ルート進軍路ごとの目標分割・戦力保持評価（OPENWARS_HOLD）
pub fn route_hold_enabled() -> bool {
    cfg!(target_arch = "wasm32")
        || cfg!(feature = "route_hold")
        || std::env::var("OPENWARS_HOLD").is_ok()
}

/// 戦力劣勢な進軍路の拠点防衛ドクトリン（OPENWARS_ROUTE_DEFENSE）
pub fn route_defense_enabled() -> bool {
    cfg!(target_arch = "wasm32")
        || cfg!(feature = "route_defense")
        || std::env::var("OPENWARS_ROUTE_DEFENSE").is_ok()
}

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

/// 地上の直接戦闘部隊が、移動タイプに関係なく同じ前線マスを取り合うとして数える。
/// 既定で有効。比較のために戻す場合だけ `OPENWARS_SHARED_FRONTAGE=0` を設定する。
pub fn shared_frontage_enabled() -> bool {
    std::env::var("OPENWARS_SHARED_FRONTAGE").map_or(true, |value| value != "0")
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

/// 砲台を積んだ輸送車が、陣地が無いとき要衝のマスではなく受け持ちの範囲に入る降車マスへ向かう。
/// 既定で有効。比較のために戻す場合だけ `OPENWARS_FERRY_ZONE_ENTRY=0` を設定する。
pub fn ferry_zone_entry_enabled() -> bool {
    std::env::var("OPENWARS_FERRY_ZONE_ENTRY").map_or(true, |value| value != "0")
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

/// 補給輸送車による砲台の運搬・陣地展開（OPENWARS_ARTILLERY_FERRY）
pub fn artillery_ferry_enabled() -> bool {
    cfg!(target_arch = "wasm32")
        || cfg!(feature = "artillery_ferry")
        || std::env::var("OPENWARS_ARTILLERY_FERRY").is_ok()
}

/// 陣地防御・布陣型思考（OPENWARS_POSITIONAL）
pub fn positional_enabled() -> bool {
    cfg!(target_arch = "wasm32")
        || cfg!(feature = "positional")
        || std::env::var("OPENWARS_POSITIONAL").is_ok()
}

/// 攻勢進軍路上の未所有都市を占領対象として認可（OPENWARS_ROUTE_CAPTURE_ALL）
pub fn route_capture_all_enabled() -> bool {
    cfg!(target_arch = "wasm32")
        || cfg!(feature = "route_capture_all")
        || std::env::var_os("OPENWARS_ROUTE_CAPTURE_ALL").is_some()
}

/// 進軍路の戦力予算計算における手番数考慮（OPENWARS_ROUTE_BUDGET_TURNS）
pub fn route_budget_turns() -> u32 {
    if let Ok(value) = std::env::var("OPENWARS_ROUTE_BUDGET_TURNS")
        && let Ok(num) = value.parse::<u32>()
    {
        return num;
    }
    if cfg!(target_arch = "wasm32") || cfg!(feature = "route_budget_turns") {
        1
    } else {
        0
    }
}
