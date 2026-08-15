---
title: "PLAYBOOK"
version: "1.18.0"
status: "approved"
created: "2026-06-06"
updated: "2026-08-15"
owner: "@t_kak"
ace_entry_count: 40
tags: [ace, playbook, knowledge-management]
references:
  - docs/ACE_FRAMEWORK.md
  - docs-template/05-operations/deployment/ace-cycle.md
---

# ACE Playbook

> **Parent**: [BEST_PRACTICES.md](./BEST_PRACTICES.md) | **関連**: [ACE サイクル運用手順](../05-operations/deployment/ace-cycle.md) | [ACE フレームワーク概念](../../docs/ACE_FRAMEWORK.md)

## 概要

### 目的

ACE (Agentic Context Engineering) Playbook は、開発プロセスで得た知見を **AIツールが直接参照できる構造化形式** で蓄積するファイルです。

GitHub Discussions が「人間が読むためのナラティブ（物語的記録）」であるのに対し、Playbook は「AIが参照するための構造化知見（delta方式: 差分のみを末尾追記する更新方式）」として機能します。

### 運用ルール

| ルール                             | 説明                                                                                                                           |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| **末尾追記のみ**                   | エントリは常にファイル末尾に追記。既存エントリの本文（Insight/Context/Action）書き換えは禁止。カウンター更新・Status変更は許可 |
| **カウンターはインクリメントのみ** | Helpful/Harmful は +1 のみ。減算・リセットはしない                                                                             |
| **削除禁止**                       | エントリを物理的に削除しない。不要な場合は `Status: deprecated` に変更                                                         |
| **800行超過時は分割**              | `playbook/` サブディレクトリにカテゴリ別ファイルとして分割                                                                     |
| **Frontmatter更新**                | エントリ追加時に `version`, `updated`, `ace_entry_count` を更新                                                                |
| **コミット規則**                   | `knowledge: ACE-XXX [category] [summary]` 形式で記録                                                                           |

### エントリID規則

ACE エントリ ID は **PRスコープ式** を採用する（このセクションが ID 規則の SSOT）。複数人・複数AIが並行で `/ace-curate` を回しても番号が衝突しないための構造である。

- **形式**: `ACE-<PR番号>-<連番>`（例: `ACE-438-1`, `ACE-438-2`）
- **非PR由来の fallback**: `ACE-i<Issue番号>-<連番>`（例: `ACE-i425-1`）
- **採番**: 同一 PR の既存 `ACE-<PR番号>-*` の最大連番 +1 を連番とする（既存が無ければ連番 `1`、すなわち最初のエントリは `ACE-<PR番号>-1`）。**全体の最新 ID を読む必要がない**ため並行採番でも衝突しない（PR 番号は GitHub が全体一意に採番するため、別 PR = 別 namespace）。
- **連番の範囲**: 1 回の `/ace-curate` で同一 PR から 1〜3 件追記する想定。同一 PR を再 curate する場合は既存の最大連番から継続。
- **既存 ID の扱い**: 旧 `ACE-{連番3桁}` 形式（`ACE-001`〜）のエントリは **改名しない**。旧 3 桁形式と新 PRスコープ式は恒久的に共存する（参照・anchor 互換の維持）。ID にファイル位置の情報は持たせないため、分割後も ID はそのまま維持する。

---

## カテゴリ一覧

| カテゴリ       | 説明                                               | 例                                |
| -------------- | -------------------------------------------------- | --------------------------------- |
| `coding`       | コーディングパターン、言語固有のベストプラクティス | 型安全性、エラーハンドリング      |
| `architecture` | 設計判断、構造上の決定事項                         | レイヤー設計、モジュール分割      |
| `testing`      | テスト戦略、テストパターン                         | モック設計、テストデータ管理      |
| `security`     | セキュリティ対策、脆弱性防止                       | 認証、暗号化、入力検証            |
| `performance`  | パフォーマンス最適化                               | キャッシュ、クエリ最適化          |
| `devops`       | CI/CD、デプロイ、環境構築                          | パイプライン、インフラ設定        |
| `process`      | 開発プロセス、ワークフロー改善                     | レビュー手法、タスク管理          |
| `tooling`      | ツール設定、開発環境                               | IDE設定、リンター、フォーマッター |

---

## ステータス定義

| ステータス   | 説明                                   | 遷移条件                                                |
| ------------ | -------------------------------------- | ------------------------------------------------------- |
| `active`     | 有効な知見                             | 新規作成時のデフォルト                                  |
| `deprecated` | 非推奨（古い情報、矛盾が発見された等） | Harmful >= 3 かつ Helpful < Harmful、または明示的な判断 |

---

## エントリテンプレート

新しいエントリを追記する際は、以下のテンプレートを使用してください：

```markdown
<a id="ace-XXX"></a>

### ACE-XXX: [タイトル（簡潔で検索しやすい表現）]

| フィールド | 値                                                                                    |
| ---------- | ------------------------------------------------------------------------------------- |
| Category   | coding / architecture / testing / security / performance / devops / process / tooling |
| Origin     | PR #XXX / Issue #YYY                                                                  |
| Date       | YYYY-MM-DD                                                                            |
| Helpful    | 0                                                                                     |
| Harmful    | 0                                                                                     |
| Status     | active                                                                                |

**Insight**: [知見の本質を1-2文で記述]

**Context**: [この知見が発見された状況・条件を記述]

**Action**: [推奨する具体的なアクション。可能であればコード例も含める]
```

### 記述ガイドライン

- **anchor**: 各エントリは見出し直前に `<a id="ace-XXX"></a>` を 1 行付与する。`XXX` は **エントリ ID を小文字化したもの**（新規は `ace-438-1` / `ace-i425-1`、旧エントリは `ace-001`。anchor 部分は常に小文字英数字＋ハイフン）。ファイルレベル参照（`PLAYBOOK.md` 単体）は常にファイル先頭に着地するため、anchor がなければ個別エントリへの誘導が成立しない。anchor 付与により他ドキュメントから `[ACE-438-1](path/to/PLAYBOOK.md#ace-438-1)` 形式で**特定エントリに直接ジャンプ可能**になる。
- **参照リンク形式**: 他ドキュメントから ACE エントリを参照する場合は `[ACE-XXX](path/to/PLAYBOOK.md#ace-XXX)` 形式に統一する（`XXX` はエントリ ID の接頭辞 `ACE-` / `ace-` を除いた部分。新規は `438-1`、旧は 3 桁 `040`。label は `ACE-438-1`、anchor は `#ace-438-1`）。`[PLAYBOOK ACE-XXX]` / `[PLAYBOOK.md ACE-XXX]` 等の異なる label は使わない（[ACE-040](#ace-040) 語彙統一 / [ACE-024](#ace-024) 用語衝突防止 の系。Origin: Issue [#425](https://github.com/feel-flow/ai-spec-driven-development/issues/425)）。
- **Insight**: 「何を学んだか」を簡潔に。1-2文。
- **Context**: 「どんな状況で発見したか」を記述。再現条件が明確であるほど価値が高い。
- **Action**: 「次回何をすべきか」を具体的に。コード例があると AIツールが直接適用しやすい。

---

## Helpful / Harmful カウンター運用

### カウンター更新タイミング

| タイミング                                     | 更新内容            |
| ---------------------------------------------- | ------------------- |
| ACE サイクルで既存エントリと重複する知見を発見 | Helpful +1          |
| 既存エントリの知見に従って問題を回避できた     | Helpful +1          |
| 既存エントリの知見に従ったが問題が発生した     | Harmful +1          |
| 既存エントリの内容が古くなっていると判明       | 検討の上 deprecated |

### エントリ品質の目安

| カウンター状態                           | 解釈                                       |
| ---------------------------------------- | ------------------------------------------ |
| `Helpful >= 5`                           | 高品質エントリ。PATTERNS.md への昇格を検討 |
| `Helpful >= 3, Harmful == 0`             | 良質なエントリ                             |
| `Harmful >= 3, Helpful < Harmful`        | deprecated 候補                            |
| `Helpful == 0, Harmful == 0`（90日以上） | 有効性未検証。次回関連タスクで意識的に検証 |

---

## ファイル分割ルール

Playbook が 800 行を超えた場合、以下のように分割する：

```
08-knowledge/
├── PLAYBOOK.md           ← 索引 + 運用ルール（200行程度）
└── playbook/
    ├── coding.md         ← Category: coding のエントリ群
    ├── architecture.md   ← Category: architecture のエントリ群
    ├── testing.md        ← Category: testing のエントリ群
    ├── security.md       ← Category: security のエントリ群
    ├── performance.md    ← Category: performance のエントリ群
    ├── devops.md         ← Category: devops のエントリ群
    ├── process.md        ← Category: process のエントリ群
    └── tooling.md        ← Category: tooling のエントリ群
```

分割時の手順：

1. カテゴリ別にエントリをサブファイルに移動
2. PLAYBOOK.md に索引テーブルを残す（エントリID + タイトル + 参照先）
3. 以降の新規追記は該当カテゴリのサブファイルに行う
4. Frontmatter の `ace_entry_count` は全エントリの合計を維持

---

---

## カテゴリ別サブファイル一覧

800行超過ルールに基づき、エントリ本体は各カテゴリ別サブファイルに分割・管理されています。
新規知見の追記は、該当するカテゴリのサブファイル末尾に行ってください。

| カテゴリ | サブファイル | エントリ数 | 主な内容 |
| -------- | ------------ | ---------- | -------- |
| `architecture` | [Architecture](./playbook/architecture.md) | 20 件 | 設計判断、構造上の決定事項、状態管理 |
| `performance` | [Performance](./playbook/performance.md) | 9 件 | パフォーマンス最適化、キャッシュ、計算量削減 |
| `coding` | [Coding](./playbook/coding.md) | 4 件 | コーディングパターン、言語固有・型安全のベストプラクティス |
| `testing` | [Testing](./playbook/testing.md) | 4 件 | テスト戦略、テストパターン、モック・シミュレーション設計 |
| `tooling` | [Tooling](./playbook/tooling.md) | 3 件 | ツール設定、ログ出力・可視化、開発環境・スキル |
| `security` | [Security](./playbook/security.md) | 0 件 | セキュリティ対策、脆弱性防止、入力検証 |
| `devops` | [Devops](./playbook/devops.md) | 0 件 | CI/CD、デプロイ、ビルドパイプライン |
| `process` | [Process](./playbook/process.md) | 0 件 | 開発プロセス、ワークフロー改善、タスク管理 |

---

## 全エントリ索引（Index）

| エントリID | タイトル | カテゴリ | Origin | Status | 参照先 |
| ---------- | -------- | -------- | ------ | ------ | ------ |
| [ACE-47-1](./playbook/architecture.md#ace-47-1) | AIの部隊管理におけるユニットの解放忘れと再搭乗ループ防止 | `architecture` | PR #47 | `active` | [詳細](./playbook/architecture.md#ace-47-1) |
| [ACE-47-2](./playbook/performance.md#ace-47-2) | ECSクエリのループ内呼び出しによるオーバーヘッド回避 | `performance` | PR #47 | `active` | [詳細](./playbook/performance.md#ace-47-2) |
| [ACE-47-3](./playbook/performance.md#ace-47-3) | 探索アルゴリズム内の Vec::contains によるパフォーマンス低下の回避 | `performance` | PR #47 | `active` | [詳細](./playbook/performance.md#ace-47-3) |
| [ACE-52-1](./playbook/testing.md#ace-52-1) | AI評価における主観メトリクスと客観メトリクスの分離 | `testing` | PR #52 | `active` | [詳細](./playbook/testing.md#ace-52-1) |
| [ACE-52-2](./playbook/architecture.md#ace-52-2) | ROI等の蓄積指標と盤面評価（スコア）の二重計上防止と分離 | `architecture` | PR #52 | `active` | [詳細](./playbook/architecture.md#ace-52-2) |
| [ACE-57-1](./playbook/performance.md#ace-57-1) | O(N) オーバーヘッドを回避する Vec の pop() による要素取り出し | `performance` | PR #57 | `active` | [詳細](./playbook/performance.md#ace-57-1) |
| [ACE-57-2](./playbook/architecture.md#ace-57-2) | 部隊（Squad）全滅時の解散漏れによる目標リソースの永久予約バグの防止 | `architecture` | PR #57 | `active` | [詳細](./playbook/architecture.md#ace-57-2) |
| [ACE-57-3](./playbook/architecture.md#ace-57-3) | 価値交換と交戦成立率に基づく動的なカウンター生産評価 | `architecture` | PR #57 | `active` | [詳細](./playbook/architecture.md#ace-57-3) |
| [ACE-59-1](./playbook/performance.md#ace-59-1) | Hot pathでの動的ディスパッチ（dyn Trait）回避によるパフォーマンス改善 | `performance` | PR #59 | `active` | [詳細](./playbook/performance.md#ace-59-1) |
| [ACE-63-1](./playbook/performance.md#ace-63-1) | Zustandストア設計での不要な再レンダリング防止（Zustand Selectors） | `performance` | PR #63 | `active` | [詳細](./playbook/performance.md#ace-63-1) |
| [ACE-63-2](./playbook/architecture.md#ace-63-2) | WebAssembly / Web Worker での安全なエラー境界 (ErrorBoundary) の適用 | `architecture` | PR #63 | `active` | [詳細](./playbook/architecture.md#ace-63-2) |
| [ACE-63-3](./playbook/performance.md#ace-63-3) | RustのWASMバインディングにおけるヒープアロケーション削減（std::slice::from_ref） | `performance` | PR #63 | `active` | [詳細](./playbook/performance.md#ace-63-3) |
| [ACE-64-1](./playbook/architecture.md#ace-64-1) | セーブ・ロードにおける空の輸送コンポーネント復元漏れバグの防止 | `architecture` | PR #64 | `active` | [詳細](./playbook/architecture.md#ace-64-1) |
| [ACE-64-2](./playbook/architecture.md#ace-64-2) | コアゲームエンジンにおける anyhow 依存排除と thiserror による型安全なエラー境界 | `architecture` | PR #64 | `active` | [詳細](./playbook/architecture.md#ace-64-2) |
| [ACE-64-3](./playbook/tooling.md#ace-64-3) | useCallback を用いた React ライフサイクル関数の安定化とリンター警告解消 | `tooling` | PR #64 | `active` | [詳細](./playbook/tooling.md#ace-64-3) |
| [ACE-65-1](./playbook/architecture.md#ace-65-1) | スナップショットとイベントストリームを分離した軽量対戦ログ設計 | `architecture` | PR #65 | `active` | [詳細](./playbook/architecture.md#ace-65-1) |
| [ACE-65-2](./playbook/tooling.md#ace-65-2) | AI思考評価ログの可視化とLLMエージェントによる自律敗因分析 | `tooling` | PR #65 | `active` | [詳細](./playbook/tooling.md#ace-65-2) |
| [ACE-67-1](./playbook/architecture.md#ace-67-1) | マスターデータの定義順（ロード順）保持によるUI一覧順序の全環境統一 | `architecture` | PR #67 / Issue #66 | `active` | [詳細](./playbook/architecture.md#ace-67-1) |
| [ACE-67-2](./playbook/coding.md#ace-67-2) | 固定UIパネルによるマップ遮蔽を防ぐカメラパディング制御 | `coding` | PR #67 / Issue #66 | `active` | [詳細](./playbook/coding.md#ace-67-2) |
| [ACE-71-1](./playbook/architecture.md#ace-71-1) | プレゼンテーション層の操作可否判定を engine へ集約する | `architecture` | PR #71 | `active` | [詳細](./playbook/architecture.md#ace-71-1) |
| [ACE-71-2](./playbook/architecture.md#ace-71-2) | 条件付きルールの共通 predicate を候補列挙・探索・実行で再利用する | `architecture` | PR #71 | `active` | [詳細](./playbook/architecture.md#ace-71-2) |
| [ACE-71-3](./playbook/coding.md#ace-71-3) | 行動種別は静的能力ではなく実際の実行文脈で判定する | `coding` | PR #71 | `active` | [詳細](./playbook/coding.md#ace-71-3) |
| [ACE-81-1](./playbook/architecture.md#ace-81-1) | 輸送部隊の状態遷移マシン化と複数カーゴの明示的フェーズ管理 | `architecture` | PR #81 | `active` | [詳細](./playbook/architecture.md#ace-81-1) |
| [ACE-81-2](./playbook/architecture.md#ace-81-2) | 間接攻撃脅威評価の独立モジュール化と危険地帯評価の共有 | `architecture` | PR #81 | `active` | [詳細](./playbook/architecture.md#ace-81-2) |
| [ACE-82-1](./playbook/testing.md#ace-82-1) | エンティティID追跡による複合行動パイプラインのシーケンス成立検証 | `testing` | PR #82 | `active` | [詳細](./playbook/testing.md#ace-82-1) |
| [ACE-82-2](./playbook/coding.md#ace-82-2) | Rust テストコードにおける nightly 機能 (let_chains) 回避による stable 互換性維持 | `coding` | PR #82 | `active` | [詳細](./playbook/coding.md#ace-82-2) |
| [ACE-83-1](./playbook/architecture.md#ace-83-1) | 特殊戦略需要による汎用需要の無条件上書きと生産ブロッキングの防止 | `architecture` | PR #83 | `active` | [詳細](./playbook/architecture.md#ace-83-1) |
| [ACE-83-2](./playbook/architecture.md#ace-83-2) | ドメイン概念の判定範囲を本来の前提条件（海上輸送を要する別陸塊）へ限定する境界設計 | `architecture` | PR #83 | `active` | [詳細](./playbook/architecture.md#ace-83-2) |
| [ACE-83-3](./playbook/testing.md#ace-83-3) | ターン限定キャッシュにおける状態更新（mark/set）の呼び出しと実効性検証 | `testing` | PR #83 | `active` | [詳細](./playbook/testing.md#ace-83-3) |
| [ACE-92-1](./playbook/architecture.md#ace-92-1) | 従属エンティティの被弾イベント駆動による状態同期と旧状態への上書き防止 | `architecture` | PR #92 | `active` | [詳細](./playbook/architecture.md#ace-92-1) |
| [ACE-92-2](./playbook/performance.md#ace-92-2) | 段階的ループ処理から O(1) 直接算術計算への最適化と事前バリデーション | `performance` | PR #92 | `active` | [詳細](./playbook/performance.md#ace-92-2) |
| [ACE-93-1](./playbook/architecture.md#ace-93-1) | 候補取得とアクション実行で共有する判定関数による不整合防止と安全拒否 | `architecture` | PR #93 | `active` | [詳細](./playbook/architecture.md#ace-93-1) |
| [ACE-93-2](./playbook/testing.md#ace-93-2) | vi.hoisted と vi.mock を用いた Web Worker / WASM ブリッジ層のモックテスト | `testing` | PR #93 | `active` | [詳細](./playbook/testing.md#ace-93-2) |
| [ACE-94-1](./playbook/coding.md#ace-94-1) | ユニット標的価値算出における搭載物および短期的経済阻害効果の複合評価 | `coding` | PR #94 | `active` | [詳細](./playbook/coding.md#ace-94-1) |
| [ACE-94-2](./playbook/architecture.md#ace-94-2) | 客観的ゲームルール依存評価の共通戦術層への集約と個別意思決定の分離 | `architecture` | PR #94 | `active` | [詳細](./playbook/architecture.md#ace-94-2) |
| [ACE-98-1](./playbook/architecture.md#ace-98-1) | 生産口封鎖（ProductionBlockade）検知と多角的 NPV に基づく全 AI 共通緊急任務解除 | `architecture` | PR #98 / Issue #76 | `active` | [詳細](./playbook/architecture.md#ace-98-1) |
| [ACE-98-2](./playbook/tooling.md#ace-98-2) | 動的緊急ミッション・配備計画の対戦 JSONL トレース記録による AI 検証可能性の確保 | `tooling` | PR #98 | `active` | [詳細](./playbook/tooling.md#ace-98-2) |
| [ACE-99-1](./playbook/performance.md#ace-99-1) | Native/WASM 共通の順序保持並列化基盤（map_ordered）と決定論的再現性の担保 | `performance` | PR #99 | `active` | [詳細](./playbook/performance.md#ace-99-1) |
| [ACE-99-2](./playbook/architecture.md#ace-99-2) | 作戦所有権（OperationOwner）の正本レジストリと双方向 O(1) 逆引きによる遊兵化・二重予約防止 | `architecture` | PR #99 | `active` | [詳細](./playbook/architecture.md#ace-99-2) |
| [ACE-99-3](./playbook/performance.md#ace-99-3) | 長期ターン進行時の戦術スナップショット再利用と再割り当てループの排除 | `performance` | PR #99 | `active` | [詳細](./playbook/performance.md#ace-99-3) |

---

## Changelog

### [1.18.0] - 2026-08-15

#### 変更

- 800行超過ルールに基づき、各エントリを `playbook/` サブディレクトリ（8カテゴリ）へ分割移行
- メインの `PLAYBOOK.md` を運用ルールおよび全エントリ索引テーブル（Index）としてスリム化

### [1.0.0] - 2026-06-06

#### 追加

- 初版作成
