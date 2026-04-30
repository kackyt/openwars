## Context

現在の実装では、エンジン側で `GameOverEvent` が発行されると、CLI側で `InGameState::GameOverPopup` に遷移し、勝敗結果を表示する仕組みが既に存在します。しかし、この表示は簡易的なものであり、ユーザー体験（UX）の観点から「モーダル」としての洗練と、マップ選択画面への確実な復帰が求められています。

## Goals / Non-Goals

**Goals:**
- 視認性の高い「ゲーム終了モーダル」のUI実装。
- 勝敗結果に応じたスタイル（色、メッセージ）の適用。
- EnterキーまたはEscキーによる、マップ選択画面へのスムーズな遷移。
- 勝利判定ロジックが確実に機能していることの検証。

**Non-Goals:**
- 勝敗統計の保存やランキング機能の実装。
- 複雑なアニメーション（CLIの制約上）。

## Decisions

- **状態上書きバグの修正**: `main.rs` において、`GameOverPopup` が表示されるべきタイミングで `GamePhaseChangedEvent` 等による `EventPopup` が発生すると、状態が上書きされてしまう問題があります。これを修正し、`GameOverPopup` を最優先するようにします。
- **UIスタイルの強化**: 勝利時は `Color::Yellow` または `Color::Cyan`、敗北時は `Color::Red` を基調としたスタイルを採用し、一目で結果がわかるようにします。
- **入力ハンドリングの一元化**: `GameOverPopup` 状態でのキー入力を `App::handle_key` で確実にトラップし、`return_to_map_selection` を呼び出すようにします。
- **状態のリセット**: マップ選択に戻る際、`World` と `Schedule` を `None` にし、メモリを解放します。

## Risks / Trade-offs

- **イベントの重複処理**: `victory_check_system` が毎フレーム走るため、一度 `game_over` 状態になったら二度目以降の判定をスキップするようにします（既に `MatchState::game_over` チェックが入っているためリスクは低い）。
- **描画のちらつき**: ラタトゥイ（Ratatui）の `Clear` ウィジェットを適切に使用し、背景が透けないようにします。
