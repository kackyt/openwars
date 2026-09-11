---
name: openwars-ai-improvement-cycle
description: >-
  Subagent群（事前調査・分析・実装・スクリーニング・ベンチマーク）とModel Tieringを駆使し、
  OpenWarsのAI計画と実行の食い違い特定、再現、並列仮説検証、段階的対戦評価までを高コスト効率で回す。
---
# OpenWars AI改善サイクル (Subagent & Multi-Tier Orchestration)

目的は、ゲーム上の成果を改善し、その理由を計画から実行まで説明できる変更を残すこと。
最先端のモデル推論（Opus）、高速バランスモデル（Sonnet）、超軽量高速モデル（Haiku）を組み合わせた**Subagent構成（Model Tiering）**と**4段階検証ゲート**により、高コスト効率かつ並列で仮説検証を回す。

---

## 1. 専門 Subagent & Model Tiering マトリクス

本サイクルでは以下の5つの専門 Subagent を呼び出して作業を委任する。

| Subagent 名 | Model | 役割・定型作業 | 主なツール |
|:---|:---|:---|:---|
| **`openwars-pre-investigator`** | `haiku` | **事前調査**: 巨大対戦ログ(JSONL)から食い違い箇所（計画/配備/DAG/戦術/実行）のみを抽出・要約 | Read, Grep, Glob, Bash |
| **`openwars-analyst`** | `opus` | **深層分析 & 審査**: 根本原因の診断、直交仮説（2〜3案）の立案、評価値ハック排除監査 | Read, Grep, Glob |
| **`openwars-implementer`** | `sonnet` | **仮説実装**: 孤立 Git Worktree (`isolation: worktree`) での再現テスト(fixture)作成と Rust コード修正 | Edit, Write, Bash |
| **`openwars-screener`** | `haiku` | **Gate 0/1 スクリーニング**: `cargo test` と代表 seed 短縮トレースによる Fail-Fast 判定 | Bash, Read, Grep |
| **`openwars-benchmark-runner`** | `haiku` | **ベンチマーク定型実行**: 12 seedバッチ対戦の実行と `compare_benchmarks.py` レポート集計 | Bash, Read |

---

## 2. トークン節約の徹底原則 (Strict Context Isolation)

メインセッションおよび最上位モデル (`openwars-analyst` / Opus) のトークン消費を最小化するため、以下のコンテキスト分離を徹底する：

1. **生ログの直接読み込み禁止**:
   - 数千〜数万行に及ぶ対戦ログ (JSONL) はメイン/Analyst に直接読ませない。
   - 必ず `openwars-pre-investigator` (`haiku`) に読み込ませ、数行〜数十行の「食い違い遷移テーブル（サマリー）」に要約させてから受け取る。
2. **コード探索のオフロード**:
   - 広範囲のコード検索やログ差分チェックは `haiku` / `sonnet` Subagent に委任し、分析・意思決定のみに注力する。

---

## 3. 4段階スクリーニングゲート (Fail-Fast プロトコル)

無駄な長時間の対局シミュレーションとトークン消費を防ぐため、以下のゲート順に検証を通過させる。

```text
[仮説案] ──► Gate 0 (cargo test) ──► Gate 1 (行動変化確認) ──► Gate 2 (3-seed) ──► Gate 3 (12-seed) ──► 採用
               │                      │                       │
               └──── 失敗: 即却下 ────┴──── 変化なし: 即却下 ─┴── 退行: 即却下
```

1. **Gate 0: 単体・局面テスト (Subagent: `openwars-screener` / `haiku`)**
   - `cargo test` および最小再現局面テスト（fixture）を実行。コンパイルエラーや既存テスト違反があれば **即座却下**。

2. **Gate 1: 代表seed短縮トレース (Subagent: `openwars-screener` / `haiku`)**
   - 代表 seed の該当ターンまで（例: `--max-turns 10`）短縮実行。
   - **狙った行動（射撃・配備標的保持など）が実際に変わっていない場合**、下流で上書きされているか無効な仮説として **即座却下**。

3. **Gate 2: 小規模スクリーニング (Subagent: `openwars-screener` / `haiku`)**
   - 3 seed（例: seed 1..3）での短縮対戦。明らかな退行やフリーズがないか確認。

4. **Gate 3: フルベンチマーク (Subagent: `openwars-benchmark-runner` / `haiku`)**
   - Gate 0〜2 を突破した最良の 1〜2 案のみに対して、12 seed フルベンチマーク (`eval_matchup.py`) と `compare_benchmarks.py` 集計を実行。

---

## 4. オーケストレーション手順 & 再仮説ループ (Re-Hypothesis Loop)

### Step 1: 事前調査の自動委任 (`openwars-pre-investigator`)
- ログ分析の定型作業を `openwars-pre-investigator` に委任。コンパクトな「食い違い遷移テーブル」のみを受け取る。

### Step 2: 根本原因診断と直交仮説の立案 (`openwars-analyst`)
- `openwars-analyst` を起動し、[評価値ハック排除原則](references/no-hack-principles.md)に沿って根本原因を特定。
- 互いに干渉しない独立した仮説（例: 仮説 A: 配備標的の保持, 仮説 B: 工場退去前の射撃判定）を 2〜3 個立案。

### Step 3: 並列仮説実装 & スクリーニング (Parallel Subagents)
- 各仮説に対して Subagent を同時に呼び出し、並列実行する：
  - **実装**: `Agent({ subagent_type: "openwars-implementer", prompt: "仮説Aの実装..." })` (Worktree 隔離)
  - **判定**: 実装完了後、`openwars-screener` が Gate 0 / Gate 1 を実行。
- 行動が変わらなかった仮説やテスト失敗案は即座にロールバック・却下する。

### Step 4: 全仮説却下時の再仮説ループ (Re-Hypothesis Loop)
- **すべての仮説が Gate 0〜2 で却下された場合**:
  1. 失敗した仮説と具体的な反証理由（例: 「仮説Aは配備層で直してもDAG層で再ソートされていた」など）を記録する。
  2. 失敗履歴を `openwars-analyst` にフィードバックし、**別の処理層や別の根本原因に着目した新たな直交仮説**を再立案する（最大 3 ループまで）。

### Step 5: 採用候補の統合とベンチマーク (`openwars-benchmark-runner`)
- Gate 0〜2 をクリアした最良案（または直交する変更の組み合わせ）をマージ。
- `openwars-benchmark-runner` を起動して 12 seed ベンチマークを 1 回実行し、比較レポートを生成。
- 最終状態で `cargo test`、`cargo fmt`、`cargo clippy` の通過を確認。

---

## 5. 完了判定と打ち切り条件 (Exit Criteria)

無制限なループやトークン無駄遣いを防ぐため、明確な判定基準に従って終了する。

### 成功終了条件 (Success Exit Criteria)
1. **目的要件の達成**: ユーザーの指定要件（特定マップの勝率改善、目標ターンの到達、特定敗因の解消）を満たした。
2. **ベンチマーク確認 (Gate 3)**: 12 seed フルベンチマークにおいて基準版に対する退行がなく、客観指標を通過した。
3. **品質チェック合格**: `cargo test`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --all -- --check` が全てエラーなしで通過した。

### 打ち切り条件 (Abortion Exit / Safeguard)
1. **最大再試行数の上限**: 再仮説ループを **3 回**（合計 6〜9 個の仮説検証）実施しても成果が改善しない場合。
2. **打ち切り時のアクション**: 無闇にループを続けず、以下をまとめた中間報告を出力して一旦終了し、ユーザーの判断を仰ぐ：
   - 試行した全仮説と却下理由
   - 判明したアーキテクチャ上のボトルネック
   - 今後の推奨アプローチ
