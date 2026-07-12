use wasm_bindgen::prelude::*;
use js_sys::Promise;
use wasm_bindgen_futures::future_to_promise;

/// 同期APIの例: 現在のゲームステートを取得
#[wasm_bindgen]
pub fn get_game_state() -> JsValue {
    // TODO: ECSリソースやコンポーネントから盤面データを抽出し、JSONなどにシリアライズして返す
    let mock_data = r#"{"status": "running"}"#;
    JsValue::from_str(mock_data)
}

/// 同期APIの例: ターン情報を取得
#[wasm_bindgen]
pub fn get_turn_info() -> JsValue {
    let mock_data = r#"{"turn": 1, "phase": "P1"}"#;
    JsValue::from_str(mock_data)
}

/// 非同期APIの例: AIの思考ターンを実行する
/// 処理に時間がかかるため Promise を返し、Web Worker 側で await できるようにする
#[wasm_bindgen]
pub fn execute_ai_turn() -> Promise {
    future_to_promise(async {
        // TODO: AIの思考ロジックを呼び出す
        // 実際にはECSのシステムを複数回回すなどの重い処理を行う
        
        let result_json = r#"{"action": "end_turn"}"#;
        Ok(JsValue::from_str(result_json))
    })
}

/// 非同期APIの例: 経路探索
#[wasm_bindgen]
pub fn calculate_move_path(unit_id: &str, dest_x: i32, dest_y: i32) -> Promise {
    // ライフタイムの問題を避けるため、引数はコピー/所有権を取得する
    let unit_id_owned = unit_id.to_string();
    
    future_to_promise(async move {
        // TODO: A* 等の経路探索ロジックを実行
        
        let result_json = format!(r#"{{"unit_id": "{}", "path": [[0,0], [{}, {}]]}}"#, unit_id_owned, dest_x, dest_y);
        Ok(JsValue::from_str(&result_json))
    })
}
