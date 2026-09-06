---
name: openwars-ai-improvement-cycle
description: OpenWarsのAI改善サイクルを標準化・自動化するスキル。対戦ログ（JSON/JSONL）の時系列分析、現行AIとの差分分析、根拠なき評価値ハックを排除した本質的改修、12seedベンチマーク実行とログ収集、客観的検証（勝率・ZOC・収入・生産内訳・思考時間）、コードレビューによる品質チェックまでの一連の改善プロセスを実行・統制する際に使用します。「AIの勝率が落ちた」「対戦ログを分析して改善して」「ベンチマークを比較検証して」「評価値ハックを排除して改修して」などの要求で起動します。
---

# OpenWars AI 改善サイクル標準スキル

本スキルは、OpenWarsのゲームAI開発において、場当たり的なスコア調整（評価値ハック）による回帰を防ぎ、**「ログ分析（事実の観察）」→「差分分析と本質的設計」→「最小実装と単体テスト」→「ベンチマーク計測とログ収集」→「客観的検証とコードレビュー」** という規律ある改善サイクルを実行・標準化します。

---

## 5段階の標準改善ワークフロー

```text
[Step 1: ログ分析・事実特定] ──> [Step 2: 本質的設計(ハック排除)] ──> [Step 3: 最小実装・単体テスト]
            │                                                                      │
            └─────────────── [Step 5: 検証・コードレビュー] <── [Step 4: ベンチマーク実行]
                                      │
                                      ▼
                             (合否判定・次のサイクルへ)
```

---

### Step 1: 現状把握とログ分析（事実ベースの観察）

推測や印象論でコードを変更する前に、必ず**確定済みのコミット状態**と**対戦ログの生データ**を確認します。

1. **コミット境界の確認**:
   - `git log --oneline -5` や `git status` で、現在の HEAD がどのコミットか、未コミットの変更が存在するかを明確に区別します。
2. **ログの定量観察**:
   - 既存のベンチマーク成果物（`reports/<run_name>/seed*.json`）や対戦ログ（`battle.jsonl`）を読み込み、敗北シードにおける具体的な数値を抽出します。
   - 抽出指標: 決着ターン数、各ターンのZOC支配面積、ターン収入、自陣拠点の占領完了ターン、兵種別生産内訳。
   - 詳細な読み解き手順は [ログ分析ガイド](./references/log-analysis-guide.md) を参照してください。

---

### Step 2: 差分分析と本質的設計（ハック排除原則）

抽出した課題に対して、小手先のスコア加減算ではなく、ゲームのルール・物理尺度に根ざした修正方針を設計します。

1. **評価値ハックの厳禁**:
   - 根拠のない生リテラル（マジックナンバー）、特定マップ名・初期工場数への特化分岐、異種指標（HPと距離と資金）の雑多な合算を排除します。
   - 詳細は [評価値ハック排除原則](./references/no-hack-principles.md) を参照してください。
2. **物理尺度による一元比較**:
   - 時間（所要ターン数/ETA）、経済（マスターデータ再調達価格・損失額・ターン収入）、交戦実効性（実ダメージ・残敵価値）で評価を統一します。
3. **1サイクル1仮説の徹底**:
   - 一度に複数のロジック（生産列挙、割当コスト、戦闘シミュレーション、戦術移動）を同時に変更してはなりません。影響範囲を単一に絞り込みます。

---

### Step 3: 最小変更の実装と単体テスト

1. **最小差分の適用**:
   - 設計した仮説に従い、コードの変更量を必要最小限に抑えます。
2. **単体テストの義務化**:
   - 評価関数やソート順を変更した場合、境界条件や順序関係を検証する Unit Test を `tests` モジュールに必ず追加します。
   - テスト実行: `cargo test -p engine <test_name>`
3. **静的品質チェックの通過**:
   - フォーマット確認: `cargo fmt --all -- --check`
   - Linter確認: `cargo clippy --all-targets --all-features -- -D warnings`

---

### Step 4: ベンチマーク実行とログ収集

改修したコードの性能を、固定シードを用いた決定論的シミュレーションで測定します。

1. **`mcp-server` の release ビルド**:
   ```bash
   cargo build --release -p mcp-server
   ```
2. **12seedバッチ実行と成果物保存**:
   - 成果物は必ず新規ディレクトリ `reports/<new_run_name>/` に保存し、過去のログを上書きしません。
   ```bash
   mkdir -p reports/<new_run_name>
   for s in 1 2 3 4 5 6 7 8 9 10 11 12; do
     python scripts/eval_matchup.py \
       --mode batch \
       --map <map_name> \
       --p1 <subject> \
       --p2 <baseline> \
       --player-order as-given \
       --seed $s \
       --games 1 \
       --max-turns 30 \
       --grid-type hex \
       --output reports/<new_run_name>/seed$s.md \
       --json-output reports/<new_run_name>/seed$s.json \
       >/dev/null 2>&1 || echo "FAIL seed=$s"
   done
   ```
   - 実行時の詳細規約（排他実行、引数設定）は [ベンチマーク実行プロトコル](./references/benchmark-protocol.md) を参照してください。

---

### Step 5: 客観的検証とコードレビュー

新旧のベンチマーク成果物を自動突合し、品質を多面的に判定します。

1. **差分集計スクリプトの実行**:
   付属の差分比較スクリプトを実行し、総合勝率、各シードの勝敗、ZOC面積、ターン収入、生産内訳の差分サマリーを出力します。
   ```bash
   python -X utf8 .rulesync/skills/openwars-ai-improvement-cycle/scripts/compare_benchmarks.py \
     reports/<baseline_dir> \
     reports/<new_run_dir> \
     --subject <subject_version>
   ```
2. **コードレビュー観点のセルフチェック**:
   - 追加した定数に較正根拠の日本語コメントがあるか？
   - DRY原則（同一の重み・定数が別ファイルに複製されていないか）を満たしているか？
   - 単体テストが網羅されているか？
3. **ロールバック判断**:
   - 目標指標が悪化した場合、推測でさらにパッチを重ねるのではなく、**即座に直前の安定状態へロールバック**して原因を再分析します。

---

## 付属リソース

- [評価値ハック排除原則](./references/no-hack-principles.md) - マジックナンバー排除と物理尺度一元化の設計基準
- [ベンチマーク実行プロトコル](./references/benchmark-protocol.md) - `eval_matchup.py` の安全な実行手順と環境前提
- [ログ分析ガイド](./references/log-analysis-guide.md) - JSON/MDログの読み解き方と典型的な敗因パターン
- [差分集計スクリプト](./scripts/compare_benchmarks.py) - 2つのベンチマーク結果を自動比較するツール
