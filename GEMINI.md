# Persistent Instructions for AI Agents (GEMINI.md)

このファイルは、AIエージェントがこのリポジトリで作業する際に遵守すべき継続的な注意事項を記載します。

## MANDATORY: Always Read MASTER.md First
プロジェクトのコンテキスト、技術スタック、およびファイル命名規則等の全体方針については、必ず最初に `docs/MASTER.md` を参照してください。

## Project Overview
- **何を作るか**: ヘックス（将来実装）およびスクエアグリッドベースのターン制戦略シミュレーションゲーム（「ファミコンウォーズ」や「大戦略」ライク）。現在は `ratatui` を用いたCUIアプリとして構築し、将来的に Tauri を用いたGUIクライアント化を計画しています。
- **なぜ作るか**: ECS (`bevy_ecs`) アーキテクチャを活用し、「ゲームルール/ロジック層」と「プレゼンテーション(描画)層」の完全な分離を実証するため。また、LLMや自己学習AIによる自律的なゲームプレイ・テスト・動的司令官機能などの実験プラットフォームとするため。
- **誰のためか**: ターン制戦略ゲームのファン、およびECS設計やAIエージェントによるゲーム開発に関心を持つ開発者・AIリサーチャー。

## Architecture
- **アーキテクチャパターン**: Entity Component System (ECS) ベースのロジック (`bevy_ecs`) + マルチフロントエンド構成。
- **通信方式**: イベント駆動 (Event-Driven) およびコマンドベース。UI層はエンジン側のシステムやコンポーネントを直接操作せず、Eventを発行・購読することで疎結合を保ちます。
- **ドメイン隔離**: Clean Architectureの思想に基づき、ドメインロジックをUI層に漏出させないこと。

## Coding Standards
- **Newtypeパターン（値オブジェクト）**: `i32`や`String`等のプリミティブ型を直接使用せず、タプル構造体（例: `UnitId(uuid::Uuid)`）を用いて型安全性を担保します。
- **依存性の注入 (DI)**: ジェネリクスやTraitを用いてインフラ依存を分離します。
- **エラーハンドリング**: ドメイン層・インフラ層では `thiserror` を用いて型安全なエラーを定義し、アプリケーション層・UI層では `anyhow` でエラーを集約・コンテキスト付与します。
- **マジックナンバー禁止**: 意味のある数値/文字列の直接埋め込みを禁止します。定数化してください。

## Build Commands
- ビルド: `cargo build`
- テスト: `cargo test`
- フォーマット確認: `cargo fmt --all -- --check`
- Linter確認: `cargo clippy --all-targets --all-features -- -D warnings`

## Development Workflow
1. **Issue駆動**: すべての作業はIssueから開始し、専用ブランチを作成します。
2. **実装・セルフレビュー**: コーディング規約やテスト充足度を確認（PR作成前に実施）。
3. **PRとマージ**: レビュー後にマージし、知見をナレッジベース（ACE Playbook / Discussions）に還元します。

## AI Agent Simulation Protocol
AIエージェントが、AIモデル（V1, V2等）同士の対戦シミュレーションを回す場合は、以下のコマンドと手順を利用してください。
- **実行コマンド**: `uv run openwars-eval --mode batch`
- **使用可能な引数**:
  - `--map`: 使用するマップ（例: `map_1`, `map_2`, `map_3`）
  - `--p1`, `--p2`: プレイヤー1/2のAIバージョン（例: `V1`, `V2`）
  - `--games`: 各組み合わせでの対戦数（例: `5`）
  - `--max-turns`: 1ゲームの最大ターン数（例: `50`）
  - `--output`: レポートの出力ファイルパス（例: `matchup_report.md`）
- **利用時の注意点**:
  - TUIモード（`--mode tui`）は人間の確認用です。生成AIが実行する際は必ず `--mode batch` を指定し、出力されたJSONLやMarkdownから定量的な結果を評価してください。

## Information Verification Protocol
AIツールは、ドキュメント生成やコード生成時に**情報が不足している場合、推論で埋めずに必ず確認を求めること**。
（詳細は `docs/MASTER.md` の「情報不足時の必須確認プロトコル」を参照）

## CLI実装・レビュー時の注意点

> [!WARNING]
> CLIのフレーム周り（レンダリング、再描画のタイミング、状態管理など）において不具合が発生しやすいことが確認されています。
> 実装やコードレビューを行う際は、以下の点に特に注意してください：
> - 画面遷移（`InGameState` の切り替え）時の描画更新が正しく行われているか。
> - 予期しないフレームの乱れや、入力待ちのデッドロックが発生していないか。
> - 画面のリサイズや表示範囲の制限が正しく処理されているか。
> - **描画タイミングの1フレーム遅延**: `main.rs` のループ構造上、入力処理の結果が描画に反映されるのは「次のループ」になります。ステートフルなUI（メニュー等）を表示する際は、エンジン側の状態更新（座標等）が完了したことを確認した上で遷移させる必要があります。
> - **イベントの手動クリア**: `main.rs` の `run_app` 内で、エンジン側の `Events` リソースを明示的に `clear()` している箇所を確認してください。新しい `Event`/`Command` を追加した際は、ここに追記しないと連続実行バグの原因になります。
> - **Wait状態の活用**: `MoveUnitCommand` のように時間（エンジンの複数システム処理）を要するコマンドを送った後は、即座にメニューを開わず `WaitActionMenu` 等の待機状態を経由させ、エンジン処理完了後に `reopen_unit_action_menu` を呼ぶパターンを徹底してください。
## ACE (Agentic Context Engineering) 運用ルール

### PLAYBOOK.md の配置場所

- パス: docs/08-knowledge/PLAYBOOK.md

### ACEサイクル手順（PRマージ後に実行）

PRマージ後に以下の3フェーズを実行してください:

#### Phase 1: Generate（知見抽出）

対象PRの diff、Issue内容、レビューコメントを分析し、将来の開発で役立つ知見候補を抽出する。

分析観点:

1. コーディングパターン: 採用した設計判断とその理由
2. テスト戦略: テストの書き方で得た教訓
3. セキュリティ: 脆弱性対策の知見
4. パフォーマンス: 最適化のヒント
5. アーキテクチャ: 構造上の決定事項
6. プロセス: ワークフロー・ツール活用の改善点

#### Phase 2: Reflect（評価・分類）

各知見候補について以下を評価する:

- 再現性が「中」以上か（低ならスキップ）
- 影響度が「中」以上か（低ならスキップ）
- 既存 Playbook エントリと重複しないか（重複なら Helpful +1）
- 既存エントリと矛盾しないか（矛盾なら既存を deprecated にして新規作成）

評価マトリクス:

| 基準   | 判定                      |
| ------ | ------------------------- |
| 汎用性 | 汎用的 / プロジェクト固有 |
| 再現性 | 高 / 中 / 低              |
| 影響度 | 高 / 中 / 低              |
| 新規性 | 新規 / 重複 / 矛盾        |

#### Phase 3: Curate（増分更新）

PLAYBOOK.md のエントリ一覧セクション末尾に新エントリを追記する。

### エントリフォーマット

ACE-XXX の XXX は **PRスコープ式 ID** に置換する: ACE-<PR番号>-<連番>（例 ACE-438-1、非PR由来は ACE-i<Issue番号>-<連番> 例 ACE-i425-1）。採番は対象 PR の既存 ACE-<PR番号>-* の最大連番 +1（既存が無ければ連番 1、すなわち ACE-438-1）で、全体の最新 ID を読む必要がない（並行採番でも衝突しない）。anchor は ID を小文字化した <a id="ace-438-1"></a> を見出し直前に付与する。

`
### ACE-XXX: [タイトル]

| フィールド | 値 |
|-----------|---|
| Category | coding / architecture / testing / security / performance / devops / process / tooling |
| Origin | PR #XXX / Issue #YYY |
| Date | YYYY-MM-DD |
| Helpful | 0 |
| Harmful | 0 |
| Status | active |

**Insight**: [知見の本質を1-2文で記述]

**Context**: [この知見が発見された状況・条件を記述]

**Action**: [推奨する具体的なアクション]
`

### 運用ルール

#### 末尾追記ルール

- エントリは常にファイル末尾（Changelog セクションの直前）に追記する
- 既存エントリの本文（Insight/Context/Action）の書き換えは禁止
- カウンター更新と Status 変更のみ許可

#### カウンター運用ルール

- Helpful/Harmful は **+1（インクリメント）のみ**。減算・リセットはしない
- Harmful >= 3 かつ Helpful < Harmful の場合、deprecated を検討する
- Helpful >= 5 は高品質エントリ（PATTERNS.md への昇格を検討）

#### Frontmatter 更新ルール

エントリ追加時に以下を更新する:

- version: マイナーバージョンをインクリメント
- updated: 今日の日付
- ace_entry_count: 全エントリ数（deprecated 含む）

#### コミットメッセージ規則

- 形式: knowledge: ACE-<PR番号>-<連番> [category] [summary]（例 knowledge: ACE-441-1 [testing] ...）
- 複数エントリ: knowledge: ACE-441-1,ACE-441-2 [category1,category2] [summary]
- カウンター更新のみ: knowledge: ACE-016 [category] helpful+1

### 既存エントリ照合手順

新規知見を追記する前に、PLAYBOOK.md の既存エントリを確認し:

1. 同じテーマのエントリが存在するか検索する
2. 重複する場合は既存エントリの Helpful を +1 する
3. 矛盾する場合は既存エントリの Status を deprecated に変更し、新エントリを作成する
4. 新規の場合のみ末尾に追記する
