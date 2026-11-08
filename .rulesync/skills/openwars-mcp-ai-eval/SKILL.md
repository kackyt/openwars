---
name: openwars-mcp-ai-eval
description: >-
  OpenWarsの評価スクリプト `scripts/eval_matchup.py`（単一バイナリの評価）と `scripts/run_matchup_sweep.py`（バイナリ・環境変数の変種を並列で比較）を使用して、開発中のAIの動作・対戦結果・戦術的パフォーマンス（勝率、思考時間、ZOC支配面積、経済指標など）を自動評価し、レポートを作成するスキルです。AIロジックの評価、バージョン比較（例: V3 vs V1）、リファクタ前後の挙動一致確認を行う際に使用します。また `scripts/run_branch_sim.py`（対局ログの途中局面からの分岐シミュレーション）で、修正の効果をフル対局の前に問題の局面だけでA/B比較する際にも使用します。
---
# OpenWars AI Matchup Evaluator

対戦シミュレーションをバッチ実行し、定量的な戦術評価やパフォーマンスレポートを作成するワークフローです。
どちらのスクリプトも `mcp-server` バイナリをサブプロセスとして起動します。

| 目的 | 使うもの |
|---|---|
| 1つのバイナリでAIを評価する（勝率・指標・レポート） | `scripts/eval_matchup.py --jobs N` |
| 2つ以上のバイナリや環境変数（基準版と候補版、`OPENWARS_EXT_DISABLE` の有無など）を同条件で比べる | `scripts/run_matchup_sweep.py` |
| seed別の勝敗比較レポート（`compare_benchmarks.py`） | スイープの出力ディレクトリをそのまま渡す |
| 対局ログの途中局面から数ラウンドだけ両軍のAIで進め、修正の前後を問題の局面だけで比べる（分岐シミュレーション） | `scripts/run_branch_sim.py` + `scripts/compare_branch_sim.py`（2c） |

## 並列実行と後始末（重要）

- 両スクリプトとも `--jobs N` で N 本の `mcp-server` を起動し、対局を並列に処理する。各ワーカーは自分のサーバーを対局間で使い回す。
- 終了時（正常・例外・Ctrl+C）には、**そのスクリプトが起動したサーバーだけ**を終了させる。Windowsではジョブオブジェクトにより、Pythonが強制終了された場合もサーバーは道連れで終了する。他の実行中のベンチやユーザー操作中のサーバーには触れないので、**複数のベンチを同時に走らせてよい**。
- 旧版にあった「起動時に既存の mcp-server を全部終了する」挙動はもう無い。逐次実行やマップ分割で10分以内に収める必要もない（長い実行はバックグラウンドで回し、完了通知を待つ）。
- `--jobs` の目安は CPU コア数の半分〜コア数。メモリはサーバー1本あたり数百MBを見込む。

## 評価ワークフロー

### 1. release ビルドと、比較に使うバイナリの固定

```bash
cargo build --release -p mcp-server
```

- 古いバイナリの結果を新しいソースの結果として報告しない。
- **比較に使うバイナリは別名でコピーして固定する**（例: `.tmp/bench-<日付>/mcp-server-current.exe`）。`target/release/mcp-server.exe` を直接指定すると、実行中に別のビルドで上書きされて結果が混ざる。
- 基準版は `git worktree add .tmp/bench-<commit> <commit>` で別の作業ツリーを作り、`CARGO_TARGET_DIR` を分けてビルドしてからコピーする。

### 2a. 単一バイナリの評価（eval_matchup.py）

生成AIによる実行では必ず `--mode batch` を使う（`--jobs` は batch のみ）。

```bash
python scripts/eval_matchup.py --mode batch --map map_1,map_2 --p1 V4 --p2 V200 \
    --seeds 1,2,3 --max-turns 30 --jobs 6 \
    --mcp-server-bin .tmp/bench-<日付>/mcp-server-current.exe \
    --output .tmp/eval/report.md --json-output .tmp/eval/results.json
```

主要なパラメータ:
- `--map`: カンマ区切りで複数指定可
- `--p1`, `--p2`: AIバージョン（`V1`〜`V4`, `V100`, `V200`）。`--p1` が評価対象（subject）
- `--seeds 1,2,3` / `--seed N`: seed（`--seeds` は各seedを `--games` 局ずつ回す。両方の指定は不可）
- `--player-order`: `both`（先後両方、既定）/ `as-given` / `swapped`
- `--max-turns`, `--criteria`（`objective` / `issue54` / `issue58`）, `--grid-type`（既定 `hex`）
- `--jobs N`: 並列数。2以上では標準出力が進捗1行ずつになり、詳細は `--log-dir`（既定 `<output名>_logs/`）へ出る
- `--output`: Markdownレポート、`--json-output`: 全対局の生データ（対局仕様の順に並ぶ）

`--log-dir` の中身:
- `games/<番号>_<map>_s<seed>_<P1>-<P2>.log` … 対局ごとの `[LOG]` 行とメトリクス
- `results.jsonl` … 1局終わるたびに追記。途中で止めても完了済みの対局は残る
- `servers/server-<n>.stderr.log` … `OPENWARS_TRACE_AI_STEPS` 設定時のサーバーごとのstderr

`--jobs 1` かつ `--log-dir` 未指定のときは従来どおり詳細を標準出力へ出す。

### 2b. 変種の比較（run_matchup_sweep.py）

全変種の対局を1本の作業キューにまとめ、全体の並列数を `--jobs` だけで制御する。

```bash
python scripts/run_matchup_sweep.py \
    --variant baseline=.tmp/bench-<日付>/mcp-server-baseline.exe \
    --variant current=.tmp/bench-<日付>/mcp-server-current.exe \
    --map map_1,map_2,map_6,map_25 --seeds 1,2 --repeats 3 --jobs 8 \
    --out-dir .tmp/sweep-<日付>
```

- 変種ごとの環境変数: `--env NAME:KEY=VALUE`（例: `--env noext:OPENWARS_EXT_DISABLE=v4/mod.rs::collect`）
- `--repeats R`: 同じ条件を R 回繰り返す（実行ごとの揺れの分布を見る）
- その他 `--p1`（既定 V4）, `--p2`（既定 V200）, `--player-order`, `--games`, `--max-turns`, `--grid-type`

出力（`--out-dir` 配下）:
- `summary.md` / `summary.json` … 同じ対局条件ごとの、変種別の勝敗分布と行動列（action_history）ハッシュの集合。判定は ✅ 集合一致 / 🟡 共通あり / ❌ 共通なし
- `games/<変種>/<map>_<P1>-<P2>_r<繰返し>/seed<N>.json` … 1局ごとの生データ。条件ディレクトリ1つが `compare_benchmarks.py` の入力になる
- `logs/<変種>/...` … 対局ごとの詳細ログ、`<変種>.md` … 変種ごとの集計レポート

`compare_benchmarks.py` での比較例:

```bash
python -X utf8 .rulesync/skills/openwars-ai-improvement-cycle/scripts/compare_benchmarks.py \
    .tmp/sweep-<日付>/games/baseline/map_32_V200-V4_r0 \
    .tmp/sweep-<日付>/games/current/map_32_V200-V4_r0 \
    --subject V4 --seed-list 1 2 3 --output .tmp/sweep-<日付>/compare.md
```

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
  - 弾薬・燃料はログに無いので満タンで置く。輸送中の部隊は輸送車に積んだ状態で置く。
  - 相手AI（V200）の記憶は分岐時点で空になる。
  - map_20 で実戦と比べた確認（conc18、T10から8ラウンド）では、拠点数・西側都市数はほぼ一致したが、相手の総戦力が実戦より16〜22%多く出た。絶対値ではなく、同じ条件で回した変更前後の差で判断する。
  - 同じ盤面からでも実行ごとに揺れるので、12局程度の平均で比べる。

### 3. 結果の読み方

- **通常経路の標準マップ（map_6, map_25 など）は、同じバイナリ・同じseedでも実行ごとに行動列が分かれる。** 1局ずつの比較で「挙動が変わった」「劣化した」と判断しない。必ず `--repeats 3` 以上のスイープで分布を比べる。
  - 🟡（集合は違うが共通の行動列あり）は繰返し不足の可能性が高い。`--repeats` を増やして再確認する。
  - ❌（共通なし）が出たら、`action_history` が最初に分かれた手（ラウンド・行動）を調べ、どの判断が変わったかを特定する。
- 小規模マップ（map_1, map_2）の拡張経路は決定的なので、リファクタ（挙動維持）なら行動列ハッシュが完全一致するはず。
- **コミット（リファクタを含む）のたびに比較し直す。** 以前の比較は、それ以降の変更を保証しない。
- 勝敗は通常勝利・ターン上限判定・引き分けを区別する（`P2_Win` と `P2_Win_MaxTurns` は別物）。
- ユーザーへの報告では、総合対戦結果（勝敗・勝率・平均勝利ターン）、思考時間、客観メトリクス（ZOC・収入・拠点数）、変種間の判定（✅🟡❌の件数と❌の詳細）を整理する。

## 注意事項

- パッケージ名は `mcp-server`（`cargo build --release -p mcp-server`）。
- 生成AIによる実行では TUIモード（`--mode tui`）を使わない。
- 計測JSON・ログはローカル成果物（`.tmp/` など）に置き、ユーザーの指示がない限りコミットしない。
