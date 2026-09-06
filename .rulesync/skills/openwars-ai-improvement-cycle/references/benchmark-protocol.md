# ベンチマーク実行プロトコル（Benchmark Protocol）

本ドキュメントは、`scripts/eval_matchup.py` を使用して AI の変更前後の性能を定量的・決定論的に検証するための手順規約です。

---

## 1. 実行前提条件

### 1.1 `mcp-server` の release ビルド
ベンチマークスクリプトは `target/release/mcp-server` をサブプロセスとして起動します。コードを変更した後は、評価を実行する前に必ず release ビルドを更新してください。

```bash
cargo build --release -p mcp-server
```

### 1.2 排他実行の順守
`scripts/eval_matchup.py` は起動時に既存の `mcp-server` プロセスを強制終了 (`taskkill` / `pkill`) するため、**複数のベンチマークを並列実行してはなりません**。必ず1プロセスずつシーケンシャルに実行してください。

---

## 2. 標準12seedベンチマークの実行方法

OpenWarsの標準検証は、乱数シード `1..=12` を使用して決定論的に行います。

### 2.1 推奨バッチ実行コマンド
プロンプトContext消費を抑え、安定して結果を収集するため、必ず `--mode batch` を指定し、成果物をマップ・試行別のディレクトリに保存します。

```bash
# 成果物ディレクトリの作成
mkdir -p reports/<run_name>

# 12seedの逐次実行
for s in 1 2 3 4 5 6 7 8 9 10 11 12; do
  python scripts/eval_matchup.py \
    --mode batch \
    --map <map_name> \
    --p1 <subject_ai> \
    --p2 <baseline_ai> \
    --player-order as-given \
    --seed $s \
    --games 1 \
    --max-turns 30 \
    --grid-type hex \
    --output reports/<run_name>/seed$s.md \
    --json-output reports/<run_name>/seed$s.json \
    >/dev/null 2>&1 || echo "FAIL seed=$s"
done
```

### 2.2 引数の意味と標準値
- `--map`: 対象マップ名（例: `map_1`, `map_2`, `map_3`, `map_32`）
- `--p1`: 評価対象の新AI（Subject、例: `V4`, `V200`）
- `--p2`: 比較対象の基準AI（Baseline、例: `V4`, `V1`）
- `--player-order`: 
  - `as-given`: P1/P2の指定通りの手番（先攻P1 vs 後攻P2）
  - `both`: 先後両手番を各シードで対戦
- `--max-turns`: 30ターン（規定ターン）
- `--grid-type`: `hex`（OpenWarsの標準トポロジー）
- `--json-output`: 生のゲーム状態・行動ログ・メトリクスを含むJSON。詳細分析の正本となる。
- `--output`: Markdown形式のサマリーレポート。

---

## 3. 回帰判定と評価指標の読み方

単なる「総合勝率」だけでなく、以下の多面的指標で健全性を判定します。

| 指標 | 意味 | 健全な改善の兆候 | 危険な兆候（局所ハック） |
|:---|:---|:---|:---|
| **勝率 (Win Rate)** | 12戦中の勝利数 | ベースライン以上を維持 | 特定シードのみ勝率急増、他が大幅悪化 |
| **ZOC支配面積** | 自軍影響下のタイル数 | 中盤〜終盤にかけて拡大 | ユニット密集による不自然な低下 |
| **ターン収入** | 確保した物件からの毎ターン収入 | 相手より早期に高水準へ到達 | 占領の遅延、拠点喪失 |
| **生産内訳** | 生産した兵種ごとの総数 | 戦況に応じた自然な混成編成 | 特定兵種（歩兵のみ、等）への極端な偏向 |
| **思考時間** | 1手番あたりの平均計算時間(ms) | 安定して一定範囲内 (100〜300ms) | 順列爆発や無限ループによるタイムアウト |

### 先手優位マップでの注意点
- `map_1` など対称性の強い小規模マップでは先手（P1）が構造的に有利です。
- 勝率50%は「先手優位を相殺した同等の実力」を意味することがあるため、後攻時の粘り（ターン数、ZOC維持面積）を併せて確認してください。
