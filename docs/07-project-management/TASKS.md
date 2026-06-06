# TASKS.md - タスク管理

## 1. 現在のスプリント目標

**目標**: 
ドキュメントの初期セットアップ（`docs/01-context` 〜 `docs/07-project-management`）を完了し、プロジェクトの技術的・アーキテクチャ的な方針（Rust, ECS, CLI分離）を明確にする。

## 2. タスク一覧

### 進行中のタスク (In Progress)

| ID | タスク名 | 担当者 | 期限 |
| --- | --- | --- | --- |
| T001 | `docs/01-context` 〜 `docs/07-project-management` テンプレートの埋め込み・更新 | AI Agent | 直近 |

### 未着手のタスク (Todo)

| ID | タスク名 | 優先度 | 内容 |
| --- | --- | --- | --- |
| T002 | `engine` クレートの初期化とコンポーネント定義 | High | ECSにおける `UnitId`, `HitPoint`, `GridPosition` の定義 |
| T003 | 六角形マップ（HexGrid）のアルゴリズム実装 | High | キューブ座標系の実装と2点間の距離計算処理 |
| T004 | `cli` クレートの初期化 (`ratatui`, `crossterm`) | High | ターミナルのセットアップと初期メニュー画面の描画 |
| T005 | コマンド（Input）とイベント（Output）の定義 | Medium | UIからエンジンへ送る `Command`、エンジンからUIへ送る `Event` の構造体定義 |
| T006 | ユニットの移動システムのテスト作成 | Medium | AP（Action Points）を消費して指定の座標へ移動するロジックのTDD |

### 完了したタスク (Done)

| ID | タスク名 | 完了日 |
| --- | --- | --- |
| T000 | リポジトリとワークスペースの基本作成 | 完了済 |

## 3. 今後の方向性
タスク T001 (ドキュメントの更新) が完了次第、T002 〜 T004 の開発タスクへ順次着手します。実装においては `docs/` に記載された各種規約（CONVENTIONS.md, PATTERNS.md）に準拠します。
