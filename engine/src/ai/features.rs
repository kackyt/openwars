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
