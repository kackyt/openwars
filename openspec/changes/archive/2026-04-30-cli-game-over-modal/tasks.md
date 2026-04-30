## 1. 調査と再現確認

- [x] 1.1 `main.rs` において、`GameOverPopup` が `EventPopup` (戦闘結果など) によって上書きされる競合を特定
- [x] 1.2 `map_debug.csv` を使用した CLI デバッグ実行によるバグの再現確認

## 2. CLI層のバグ修正とUI/UX改善

- [x] 2.1 `cli/src/main.rs` で `GameOverPopup` 状態の時は `EventPopup` による上書きをガードするように修正
- [x] 2.2 `cli/src/ui.rs` の `GameOverPopup` 描画処理を改善（中央配置、勝敗に応じた色分け、背景クリア）
- [x] 2.3 `cli/src/app.rs` の `handle_in_game_key` 等を修正し、`GameOverPopup` 状態での Enter/Esc 入力でマップ選択へ戻るようにする

## 3. 動作確認

- [x] 3.1 デバッグマップを使用して、勝利時にモーダルが消えずに表示されることを確認
- [x] 3.2 モーダル表示中に Enter を押下し、マップ選択画面へ正常に戻れることを確認
- [x] 3.3 マップ選択画面から再開した際、エンジン状態がクリーンアップされていることを確認
