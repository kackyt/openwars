# Handoff: map_20後攻 V4 — 西の砲台・右辺の守り・西の都市の早期占領

**Generated**: 2026-10-30 09:25 UTC

**Branch**: `claude/dazzling-lovelace-if6j7f`（基準 HEAD `83ecfb4`）

**Status**: In Progress。エンジンの変更は未コミットで、パッチとして `handoff/west-guns-20261030/patches/` に保存した。T20に西の4都市を押さえる目標は未達（最良 15/36局）。次の一手はユーザーの承認待ち（下記「Next Step」）。

前の担当の資料は `handoff/map20-20261029/HANDOFF-20261029.md` に移した。

## Goal

map_20 の後攻（P2）V4 を、先攻 V200 に対して改善する。特定マップの決め打ちはしない。判断は対局ログと分岐シミュレーションで確かめてから実装する。

## ユーザーの方針（守ること）

- **主戦場の概念をなくす**。配分案どうしを比べて主戦場を選び直さない。進軍路ごとに攻守を決める。
- **右辺は最低限の守り。ゴーサインは出さない**。右辺の部隊（歩兵を含む）と砲台は工場付近まで下がる。
- 右辺の砲台は「工場から射程（6マス）以内・味方より前に出ない」範囲に置く。根拠のない数字（3マスなど）は使わない。
- **西には砲台を3基以上**置く。足りない分は**生産で足す**。右辺の砲台を引き抜かない。
- 西を掃討したら砲台を前進させる。
- **西の4都市（(10,18)(6,17)(2,18)(2,19)）をT20までに全部押さえる**。都市を取れないと戦車・砲台を運べず、敵を殲滅できない。
- 歩兵は川を渡れる（移動コスト2）。橋に殺到しなくてよい。
- 北東の話と西の話を混ぜない。方針に反する動きが出たら報告する。
- 見積もりや判断が間違っている限り実装に進まない。実戦の前にシミュレーションで確かめる。

## Completed

- [x] 分岐シミュレーター `engine/tests/branch_sim.rs` と `scripts/run_branch_sim.py`・`scripts/compare_branch_sim.py` を追加し、ai-eval スキルに使い方を追記（コミット済み）。輸送中の部隊の再現と、占領点の出力も追加済み。
- [x] 砲台の運搬の不具合を直した（パッチ内）。
  - 森・山に囲まれて二度と乗り直せないマス（北東の都市(24,13)など）へ砲台を降ろさない。
  - 補給の付き添いの輸送車は、砲台が乗り込めるマスに居る。付き添いは1両だけ。
  - 撃てる敵の居ない砲台は、自分の戦線の撃てる陣地へ運ぶ（盤面全体で一番敵に近い都市へは運ばない）。
  - 元の対局では砲台・手番の35%が「動けないまま」だった。
- [x] 西の砲台を3基にする（パッチ内）。攻めの進軍路へ、新しく作った砲台を合計3基まで割り当てる。既にいる砲台は引き抜かない。足りなければ生産で買う。
- [x] 主戦場を選び直さない（パッチ内）。攻めの砲台を割り当てた進軍路（西）だけが攻める。右辺の守りの要衝は工場(20,15)。
- [x] 右辺の歩兵の北への占領をやめる（パッチ内）。原因は実行設定 `OPENWARS_ROUTE_CAPTURE_ALL=1` で、守りの進軍路の先の拠点への占領任務も出ていた。
- [x] 取れない敵の拠点へ占領役を送らない（パッチ内）。周り6マスの自軍戦力が周り3マスの敵戦力に届かない敵の拠点は外す。ログでは、この判定で「待つ」になった拠点は1,580件中0%しか取れていなかった。

## 主な数字（分岐シミュレーション、36局 = 3乱数シード × 12局）

T20の西の都市（T10から11ラウンド）:

| 版 | T20の西の都市 | 4都市すべて | (2,18) | (2,19) |
|---|---|---|---|---|
| 基準（conc18） | 2.44 | 7/36 | 7 | 15 |
| v13（`patches/v13-best.patch`） | 2.97 | **15/36** | 17 | 20 |
| v14: 予備の歩兵も西の都市へ | 2.81 | 10/36 | 11 | 18 |
| v15: 近くの歩兵に占領を追加 | 2.97 | 15/36（効果なし） | 17 | 20 |
| v16: 輸送車が橋で止まらない | 2.86 | 14/36 | 14 | 19 |
| v17（`patches/v17-latest.patch`）: 川を渡れる部隊も橋で手番を終えない | 2.67 | 6/36 | 7 | 20 |

v13 の方針違反の集計（各360手番）: 右辺が攻め 0、右辺の守りの要衝が工場以外 0（v11以降）、西が守りに回る 0、右辺の砲台が工場から射程より前 0。右辺の部隊が工場から射程より前にいるのは 0.2〜0.8体/手番（北への占領をやめた後）。

戦況（v12相当、T31）: 敵戦力 160k（基準 175k）。右辺の拠点は 1.25（基準 2.5）。右辺を工場付近まで下げて北の都市を手放したため。

## Failed Approaches (Don't Repeat These)

| 案 | 結果 |
|---|---|
| 撃てる敵がいる砲台を「より良い陣地」へ移す | 帳簿上の射程内の敵は2倍、実際の射撃は 131→63 回に半減（敵が動くたびに移し替え）。T22以降は3シードとも悪化。 |
| 右辺の砲台を西へ引き抜く割り当て | 右辺の都市を失い、敵戦力 +39k。 |
| 資金の取り置き（組の値段に届かない手番に残額を保留） | 不要だった。実際は数え方の誤りで、資金は足りていた。 |
| 攻めの砲台の割り当てを配分案の攻守に連動 | 配分案の揺れ（西→(16,3)など）で割り当てが外れ、追加の砲台を買い続けた。一度割り当てたら外さない形に直した。 |
| 予備の歩兵まで西の都市へ向かわせる（v14） | 渋滞が増えて 15→10/36。 |
| 歩兵が橋で手番を終えない（v17） | 橋は空いた（ふさがり 79%→29%）が、歩兵は川の手前から動かず 15→6/36。 |

## 未解決の原因（いま分かっていること）

wait_probe（`OPENWARS_WAIT_PROBE=1 OPENWARS_WAIT_PROBE_PLAYERS=2 OPENWARS_WAIT_PROBE_UNITS= OPENWARS_WAIT_PROBE_TYPES=Infantry OPENWARS_WAIT_PROBE_TURNS=16-16 OPENWARS_WAIT_PROBE_PRINT=1`）で確認した。

- 川の手前（(10〜14, 14〜19)）の歩兵は、出発してよい状態（departure_authorized=true）のまま待機している。
- 西の攻めの作戦の部隊は20〜57体で、全員が同じ目標(2,18)に向かう。(2,18)は西の端の海沿いの行き止まり。
- 先に着いた部隊がその周りに留まり、そこから橋のたもと、東岸の道路まで列がつながる。例えば(12,17)の歩兵は、行ける12マスのうち止まれるのが3マスだけ（前方は味方で埋まっている）で、待機を選ぶ。
- 経路の選び方（歩兵が橋と道路を選ぶ）や橋のふさがりより、「全員が1か所を目指す」ことが渋滞の元。

## Next Step（ユーザーに承認を求めている提案）

1. 西の攻めの作戦で、戦車などの戦闘部隊の行き先を、占領先の都市ではなく敵のいる前線（いまなら回廊の出口(2〜5, 13〜16)）にする。
2. 占領先の都市へは占領の担当の歩兵だけを向かわせる。
3. 橋を渡った部隊は、橋のたもとに留まらずに前へ進ませる。

1は進軍路作戦の行き先の決め方（`engine/src/ai/squad/route_operations.rs` の `responsibilities` が攻めの作戦に要衝＝目標を渡している）を変える構造的な変更。承認を得てから着手すること。

## Current State

**Working**: HEAD のエンジン・テストは前回のまま。v13 のパッチを当てた状態で `cargo test -p engine --lib`（1124件成功・1無視）、`cargo clippy -p engine --all-targets --all-features -- -D warnings`、`cargo fmt --all -- --check` が通ることを確認済み。

**Uncommitted engine changes**: なし（作業ツリーは HEAD）。変更はすべてパッチで保存。

- `handoff/west-guns-20261030/patches/v13-best.patch` … 一番よかった版。conc18 の土台＋上記の修正。v17 から v15〜v17 の3つの変更を外して作り直し、元の v13 のバイナリと分岐シミュレーションの結果が2局とも完全一致することを確認した。
- `handoff/west-guns-20261030/patches/v17-latest.patch` … 最新の版（v13 ＋ v15〜v17）。どちらもクリーンな HEAD に `git apply` できることを確認済み。

**重要**: パッチの土台は conc18（2026-10-29 の実験版。主戦場の配分案 `concentrate_route_efforts` を含む）。今回、主戦場の選び直しは止めたが、配分案の仕組み自体はまだ残っている。序盤（攻めの砲台がまだ無い間）は最初の砲台を付ける進軍路を配分案の比較で選んでいる。

## Files to Know

| File | Why It Matters |
|---|---|
| `handoff/west-guns-20261030/patches/` | 作業中のエンジンの変更（v13・v17）。 |
| `handoff/west-guns-20261030/logs/conc18-map20-p2-12seeds.zip` | 基準の対局ログ12局（盤面と資金だけに絞った版、1.1MB）。分岐シミュレーションの入力。 |
| `handoff/west-guns-20261030/tools/` | 分析スクリプト。`run_conc18_branch.sh`（conc18 の環境変数で分岐シミュレーション）、`t20.py`（T20の西の都市と橋のふさがり）、`cmpvar.py`（砲台・戦況の比較）、`gunsim.py`、`guntraj.py`（砲台ごとの軌跡）、`common.py`。出力は `.tmp/gun/<名前>` を読む。 |
| `engine/tests/branch_sim.rs`, `scripts/run_branch_sim.py`, `scripts/compare_branch_sim.py` | 分岐シミュレーター（コミット済み）。使い方は `.rulesync/skills/openwars-mcp-ai-eval/SKILL.md` の 2c。 |
| `engine/src/ai/v4/artillery_ferry.rs`（パッチで追加） | 砲台の運搬・割り当て・守り場所。 |
| `engine/src/ai/v4/capital_route/force_demand.rs` | 配分案と進軍路の攻守・要衝。 |
| `engine/src/ai/squad/objective_allocation.rs` | 占領の目標の絞り込み（`capture_target_allowed` など）。 |
| `engine/src/ai/squad/route_operations.rs`（パッチで追加） | 進軍路作戦（Next Step で変える場所）。 |

## Resume Instructions

1. パッチを当てる（v13 から再開する場合）。

   ```bash
   git apply handoff/west-guns-20261030/patches/v13-best.patch
   cargo test -p engine --lib
   ```

2. 基準ログを展開する。

   ```bash
   mkdir -p .tmp/handoff-conc18
   python3 -m zipfile -e handoff/west-guns-20261030/logs/conc18-map20-p2-12seeds.zip .tmp/handoff-conc18
   ```

3. 分岐シミュレーション用のテストバイナリを作って固定する。

   ```bash
   cargo test --release -p engine --test branch_sim --no-run   # 出力の Executable のパスを確認
   mkdir -p .tmp/gun && cp target/release/deps/branch_sim-<hash> .tmp/gun/branch_sim_v13
   ```

4. T20 の比較（T10 から 11 ラウンド、3シード）。基準はパッチを当てる前のバイナリで同じように回す。

   ```bash
   for sd in 1 2 3; do X=$([ $sd = 1 ] && echo "" || echo _s$sd)
     handoff/west-guns-20261030/tools/run_conc18_branch.sh .tmp/gun/branch_sim_v13 t20_v13$X 10 11 --seed $sd
   done
   python3 handoff/west-guns-20261030/tools/t20.py v13          # 版名は .tmp/gun/t20_<版名> に対応
   ```

5. 方針違反の集計には `OPENWARS_CONCENTRATE_DEBUG=1` を付けて回し、ログの `ROUTE_ORDERS P2` 行（各進軍路の要衝と攻守）を数える。

## Warnings

- 分岐シミュレーションは同じ乱数シードなら完全に決定的。ばらつきは `--seed` を変えて見る。
- 分岐シミュレーションの序盤（T4 から始める）の配置は実戦と合わない（西の砲台が1基になる）。T10・T22 からの分岐で評価している。
- 弾薬・燃料は満タンで置かれる。相手（V200）の記憶は分岐時点で空になる。絶対値ではなく、同じ条件での変更前後の差で判断する。
- conc18 の実行設定（`OPENWARS_CONCENTRATE`、`OPENWARS_HOLD`、`OPENWARS_ROUTE_DEFENSE`、`OPENWARS_ARTILLERY_FERRY`、`OPENWARS_POSITIONAL`、`OPENWARS_ROUTE_CAPTURE_ALL`、`OPENWARS_ROUTE_BUDGET_TURNS`）を前提にしている。
- 60ターンの実戦スイープ（`scripts/run_matchup_sweep.py`）はまだ回していない。勝敗への効果は未確認。
- 原則とプロジェクト規約は `CLAUDE.md`。ユーザーへの報告とコードのコメントは日本語。
