---
name: openwars-mcp-ai-eval
description: >-
  OpenWarsの評価スクリプト `scripts/eval_matchup.py` を使用して、開発中のAIの動作・対戦結果・戦術的パフォーマンス（勝率、思考時間、ZOC支配面積、経済指標など）を自動評価し、レポートを作成するスキルです。AIロジックの評価やバージョン比較（例: V3 vs V1）を行う際に使用します。また `scripts/run_branch_sim.py`（対局ログの途中局面からの分岐シミュレーション）で、修正の効果をフル対局の前に問題の局面だけでA/B比較する際にも使用します。
---
# OpenWars AI Matchup Evaluator

`scripts/eval_matchup.py` を使用して、開発・アップデートしたAIロジックの対戦シミュレーションをバッチ実行し、定量的な戦術評価やパフォーマンスレポートを作成するワークフローです。
内部で `mcp-server` バイナリをサブプロセスとして起動して高速シミュレーションを行うため、評価実行前に必ず `mcp-server` の release ビルドを行います。

## 対象となるケース
- 新しいAIロジックや評価関数を実装した際のE2E評価
- 異なるAIバージョン間（例: V3 vs V1, V4 vs V3）の対戦シミュレーションと品質検証
- 特定マップ（例: `map_1`, `map_2`, `map_3`）におけるZOC支配面積・ターン収入・思考時間・勝率等の比較検証

## 評価ワークフロー

### 1. `mcp-server` クレートの release ビルド
`scripts/eval_matchup.py` は `target/release/mcp-server` (Windowsの場合は `mcp-server.exe`) を使用して対戦を行うため、評価前に必ず最新のコードを release ビルドしてください。

```bash
cargo build --release -p openwars-mcp-server
```

### 2. 評価条件の確認と対戦実行
評価対象のマップ、対戦カード（P1 vs P2のAIバージョン）、ターン数、対戦数などの条件を確認し、`scripts/eval_matchup.py` を実行します。AI評価を行う際は、プロンプトContextの節約と決定的なシミュレーション実行のため、必ず `--mode batch` オプションを指定します。

基本実行コマンド例:
```bash
python scripts/eval_matchup.py --mode batch --map map_3 --p1 V3 --p2 V1 --games 1 --max-turns 30 --output matchup_report.md
```

主要なパラメータ:
- `--mode`: 実行モード (`batch` を使用)
- `--map`: テスト対象マップ（カンマ区切りで複数指定可能: `map_1,map_2,map_3`）
- `--p1`, `--p2`: プレイヤー1/2のAIバージョン（例: `V1`, `V2`, `V3`, `V4`）
- `--games`: 1マッチアップあたりの対戦数
- `--max-turns`: 1ゲームの最大ターン数
- `--criteria`: 合否判定基準 (`objective`, `issue54`, `issue58` など)
- `--output`: 最終レポートの出力先パス (デフォルト: `matchup_report.md`)
- `--seed` / `--seeds`: 乱数シードの固定指定

### 2c. 途中局面からの分岐シミュレーション（run_branch_sim.py / compare_branch_sim.py）

60ターンのスイープを回す前に、**問題の起きている局面だけで修正の効果をA/B比較する**ための道具。
対局ログ（スイープの `games/<変種>/<条件>/seed<N>.json`）の途中の盤面を読み込み、両軍のAIで数ラウンドだけ対局を進める。
中身は ignored テスト `engine/tests/branch_sim.rs` で、`run_branch_sim.py` がそれを release ビルドして seed ごとに並列起動する（`mcp-server` は使わない）。

```bash
# 変更後のコードで、10ラウンド目から8ラウンド分進める（--player 側がV4、相手はV200）
python scripts/run_branch_sim.py \
    --logs .tmp/sweep-<日付>/games/current/map_20_V200-V4_r0 \
    --from 10 --turns 8 --jobs 4 --map map_20 --player 2 \
    --env OPENWARS_CONCENTRATE=1 --out-dir .tmp/branch/after

# 実際の対局・変更前・変更後を、指定ラウンドの終わりの盤面で比べる
python scripts/compare_branch_sim.py --rounds 13,17 \
    actual=.tmp/sweep-<日付>/games/current/map_20_V200-V4_r0 \
    before=.tmp/branch/before after=.tmp/branch/after \
    --region west:0-12,* --cities "west_cities:10,18;6,17;2,18;2,19"
```

- 手順: ログの `from-warmup-1`〜`from-2` ラウンドの盤面で対象AIの判断だけを再現して記憶（作戦の割り当てなど）を温め、`from-1` ラウンドの終わりの盤面と資金を置いて、エンジンの手番開始処理から両軍で `--turns` ラウンド進める。
- 変更前（before）は、変更前のコミットの作業ツリー（`git worktree`）で同じコマンドを回す。テストバイナリを固定したいときは `--bin` で渡す。
- 出力 `<out-dir>/seed<N>.json` の `results[0].strategic_history` は対局ログと同じ形式。`seed<N>.log` はテストの出力。
- `compare_branch_sim.py` の指標: `pN_props`（拠点数）、`pN_value`（戦力 = 価格×HP%の合計）、`pN_<領域>`（`--region 名前:x範囲,y範囲` 内の戦力。範囲は `a-b` / `a-` / `-b` / `*`）、`<名前>`（`--cities` の拠点のうち `--player` 側が持つ数）。
- **忠実度の注意（相対比較にだけ使う）**:
  - 弾薬・燃料はログに無いので満タンで置く。輸送中の部隊は置かない。
  - 相手AI（V200）の記憶は分岐時点で空になる。
  - map_20 で実戦と比べた確認（conc18、T10から8ラウンド）では、拠点数・西側都市数はほぼ一致したが、相手の総戦力が実戦より16〜22%多く出た。絶対値ではなく、同じ条件で回した変更前後の差で判断する。
  - 同じ盤面からでも実行ごとに揺れるので、12局程度の平均で比べる。

### 3. 結果の確認と評価レポートの作成・分析
スクリプトの実行が完了すると、指定した `--output` (標準では `matchup_report.md`) に詳細な評価レポートが生成されます。
生成されたレポートや標準出力の内容を分析し、ユーザーへ以下のポイントを整理して提示します。

- **総合対戦結果**: 勝敗・引き分け数、勝率、平均勝利ターン数
- **パフォーマンス指標**:
  - 平均思考時間（1ターンあたり）
  - 客観メトリクス（ZOC支配面積、ターン収入、拠点数、NPV等の推移）
- **戦術・戦略の評価**:
  - 資源管理および生産バランス
  - 地形利用・ZOC支配の有効性
  - ユニットの移動・戦闘・拠点占領の連携（停滞や不必要な行動が発生していないか）
- **改善提案**: 分析結果に基づいたAIロジックの弱点や今後の修正方針

## 注意事項

- **事前ビルドの徹底**: `cargo build --release -p openwars-mcp-server` を行わずに評価を実行すると、古いビルドのバイナリが実行されたり、バイナリが存在しないためにエラーとなる可能性があります。
- **バッチモードの使用**: 生成AIによる評価実行時は、TUIモード (`--mode tui`) ではなく必ず `--mode batch` を使用してください。
