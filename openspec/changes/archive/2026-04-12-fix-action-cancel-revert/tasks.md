## 1. Engine: トランザクション管理とドメインロジックの実装

- [x] 1.1 `engine/src/resources/mod.rs` に `PendingMove` リソースを定義
- [x] 1.2 `engine/src/events/mod.rs` に `UndoMoveCommand` イベントを追加
- [x] 1.3 `engine/src/systems/movement.rs` に `UndoMoveCommand` を処理するシステムを実装
- [x] 1.4 `engine/src/systems/movement.rs` の `move_unit_system` で `PendingMove` を記録
- [x] 1.5 確定アクション完了時に `PendingMove` をクリア
- [x] 1.6 UI向けのターゲット検索関数をEngine各システム（combat, supply等）に実装
- [/] 1.7 `get_attackable_targets` に武器マスターデータを参照した検索・射程判定ロジックを実装

## 2. CLI: キャンセル機能の実装とEngineロジックへの統合

- [x] 2.1 `InGameState::TargetSelection` にアクションメニュー復帰用の情報を追加
- [x] 2.2 `cli/src/app.rs` の `handle_in_game_key` に `'x'` キーによるキャンセル処理を追加
- [x] 2.3 ターゲット選択画面等でのキャンセル/エラー時のアクションメニュー復帰の実装
- [x] 2.4 `ActionMenu` 画面でのUndo発行の実装
- [x] 2.5 ターゲット検索をEngineの関数呼び出しにリプレース
- [x] 2.6 操作ヘルプに `[x] Back/Cancel` を追加

## 3. Verification: 検証

- [x] 3.1 `engine/src/systems/movement.rs` に移動取り消し機能の回帰テストを追加
- [x] 3.2 CLIを用いた手動検証
    - ユニット移動 -> キャンセルボタン -> 元の位置に戻り、再度選択可能か確認
    - 間接攻撃ユニット：移動あり（座標変更）時に攻撃不可、その場待機時に攻撃可能か確認
    - 直接攻撃ユニット：移動＋隣接時に攻撃可能か確認
- [x] 3.3 確定アクションの後はキャンセルが効かなくなることを確認
