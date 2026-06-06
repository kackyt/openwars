# INTEGRATIONS.md - 統合・連携ガイド

## 1. AI開発ツール統合

本プロジェクトは AI仕様駆動開発 (OpenSpec) の枠組みや、独自のAIエージェント・スキル（例： `ai-cli-debugger`, `code-review`）を多用して開発を進めています。

### AIスキルの活用
プロジェクト直下の `.agent/skills` フォルダなどに各種スキルが定義されています。AIはこれらのスキルを利用して特定のタスクを自律的に遂行します。

- **`code-review`**: GitHub Flowやローカルのコミット履歴において、`rules/project.md` 等の制約に照らし合わせた敵対的レビューを実施します。
- **`git-smart-merge`**: AIエージェントにブランチの統合を指示する際に用いるスキルです。
- **`ai-cli-debugger`**: ratatui等を用いたTUIアプリのデバッグを自動化・支援するためのスキルです。描画崩れやデッドロックなどの特有の問題解決に利用します。

### GitHub Copilot / Cursor 等への対応
必要に応じて、これらツールのプロンプトルール（`.cursorrules` など）に `docs/` 配下のアーキテクチャやドメインルールを読み込ませることで、精度の高いコード生成を実現します。

## 2. 外部サービス/フレームワーク統合

### 2.1 Tauri連携 (GUIフロントエンド) - 将来構想
現在はCLI（`ratatui`）でのUI構築が中心ですが、将来的にTauriを用いたデスクトップアプリの提供を予定しています。

#### Tauriコマンドによる呼び出し
Tauriフロントエンドから `engine` クレートの機能を呼び出す際は、`tauri::command` を中継点として使用します。

```rust
// 例: gui/src-tauri/src/cmd.rs
#[tauri::command]
pub fn move_unit(unit_id: String, x: i32, y: i32, state: tauri::State<GameState>) -> Result<(), String> {
    // stateからEngineへのコマンドキューにPushする等の処理
    Ok(())
}
```

#### イベントによるフロントエンドへの通知
`engine` で発生したECSイベント（例: `UnitMovedEvent`）をTauriのイベント機構（`Window::emit`）に変換してフロントエンド（React/Vue等）に通知します。

### 2.2 その他の統合
現時点では、Web API（外部サーバ通信）やデータベース管理システム（PostgreSQL等）、サードパーティの決済・認証システムなどの統合予定はありません。ローカル実行型のスタンドアロンアプリとしての完結を前提としています。
