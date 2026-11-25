# Handoff: map_42 後攻 V4 — 前線中央都市の先取り→森の歩兵壁（強制上限実験）

**Generated**: 2026-11-25（午後）
**Branch**: `feature/v4-unified-production-lean`（HEAD `0abd884`、全変更は未コミット）
**Status**: In Progress。次の実装案（占領順・解放・引き抜き禁止）を提示済みで、**ユーザーの承認待ち**
**前版**: `HANDOFF-20261125-am.md`（同じ作業の午前版。隣接合図の実装前）

## Goal

map_42 後攻で V4 が V200 に通常勝利できない。人間の勝ち筋（森 y=7 を歩兵で埋め、y=5 の都市から間接部隊で撃つ）を**試験用スイッチで強制**し、その形が成立すれば戦況が改善するかという上限を測る。成果は判断する AI。強制は検証の手段にすぎず、製品コードに座標を決め打ちしない。

## Completed

- [x] 強制購入・森への強制割当・砲を前線都市に置く処理・診断トレース（前版までに実装済み）
- [x] 出発判定を「見極め v1（`dispatch_due`）」から**隣接合図**に置換した。`forced::contested_cells` は、敵地上部隊がそのマスにいるか隣にいるマスを返す。`enemy_arrival_turns` と `dispatch_due` は削除済み
- [x] seed 2・3 の T12 比較: `.tmp/lean/hold-line-adjacent-t12/`。「購入のみ」のハッシュは基準どおり（seed2 `c90c9262`、seed3 `8c085aa5`）
- [x] 人間ログと V4 を比べ、根本原因を分析した（下記「分析結果」）。記録は `.tmp/lean/hold-line-adjacent-t12/findings.md`
- [x] `cargo test` 全件成功、fmt・Clippy（`-D warnings`）・`cargo build --release -p mcp-server` 成功
- [x] メモリに記録: `map42-forest-comes-from-capture-order.md`

## Not Yet Done

- [ ] **（承認後）** 試験スイッチの中だけで次の3点を実装する
  1. **占領順**: 陣地線があるときは、線の自陣側で線に最も近い列の都市（前線都市）から、最寄りの歩兵が近い順に占領する。「自首都から遠い順」は使わない
  2. **兵の解放**: 前線都市の占領を終えた兵は、次の占領先へ行かずに線へ向かう。前線都市で生産した歩兵も線へ向かう
  3. **引き抜きの禁止**: 占領中の兵を森へ引き抜かない
- [ ] seed 2・3 各1局、T12 で「購入のみ」と比べる。見るのは、T3 に中央の前線都市を取れたか、T5 の森の歩兵 6〜7 体、毎手番の補充、撃破数
- [ ] 改善が見えたら、map_42 後攻 seed 1..12 の全局比較（予算は事前にユーザーへ提示する）
- [ ] 計画書 `docs/07-project-management/plans/v4-hold-line-mission.md` へ結果を反映する（未着手）

## 分析結果（今回の核心）

**人間は先読みしていない。** 前線都市を中央から先に取った結果、中央が埋まった。

| | 人間 | V4 |
|---|---|---|
| T2 の占領開始 | (3,5)(4,5)(5,5)(6,5) 中央 | (1,5)(9,5)(2,5)(8,5)(2,3) 端 |
| 中央の都市を取り終えた手番 | T3 | T5〜T6 |
| T4 に森へ入った兵 | T3 に中央都市の占領を終えた3体 | 東端の戦車に反応した兵 |
| T4 の歩兵の生産場所 | (4,5)(5,5)(6,5)。T5 に森へ入る | y=3 の工場（3手番かかる） |

- V4 の占領順は [planning_policy.rs:28-67](engine/src/ai/squad/planning_policy.rs) の `sort_small_map_capture_targets` が決める（SmallMapExpansion では自首都から遠い順）。首都 (5,3) から遠い順に並べると、実際の T2 の占領順と一致する。
- 占領のペースは同じ。人間は T5 開始時に拠点 15、V4「購入のみ」は T4 終了時に 14。違うのは順番だけ。

**森が崩れる理由は損失ではない**（`collapse.py`）:

| 1手番あたり（T5〜T9） | 人間 | V4（隣接合図） |
|---|---|---|
| 敵の手番に失う森の兵 | 1〜3 体 | 1〜3 体 |
| 森へ補充した兵 | 2〜6 体（すべて y=5〜6 から 1 手） | ほぼ 0 |
| 自分の手番の攻撃 / 撃破 | 10〜16 回 / 5〜12 体（そのうち森の兵の攻撃が 5〜7 回） | 4〜5 回 / 0〜3 体 |

- 割当の横取りもあった。seed2 の T6 は5体を森に割り当てたが、2体は (8,5)(9,5) の占領に戻った。この2都市が残っていたのは、T3 の合図（東端の戦車）で占領中の兵を森へ引き抜いたため。
- 未確認: V200 の本隊が y=8 で止まったのは中央が埋まっていたからか。占領順を変えても全体の拠点のペースは落ちないか。

## Failed Approaches (Don't Repeat These)

1. **部分保持（`OPENWARS_V4_PARTIAL_HOLD_LINE=1`）**: 449回の判断すべてで候補0体。資格・砲座・厳密な改善条件の3つの関門が重なる。`.tmp/lean/hold-line-cp-b0/cp-b0-findings.md`
2. **敵追跡担当だけを候補にする強制保持**: 購入強制時は歩兵が全員占領担当になるため候補0体。実験として成立しなかった
3. **前線都市を1つ取ったら全員を転用（③）**: T3 に全員が森へ向かい、T4 の拠点が 14→8 に減った。`.tmp/lean/hold-line-joint-t12/`
4. **購入だけ強制**: 指定都市が使えないと y=3 の工場に振り替わり、砲が前に出ない。→ BATTERY（都市への移動・保持）を追加した
5. **見極め v1（敵の最短到着手番 ≤ 自軍の到着手番で出発）**: 占領中の敵も数える最悪値なので、T2〜T3 に全員が出発した。T12 の自軍は 2〜7 体まで悪化。`.tmp/lean/hold-line-jit-t12/`
6. **隣接合図（今回）**: 「購入のみ」を上回らない。T4 の拠点 10、T5 の森 4 体、T7 に 0 体。T3〜T4 は東端の戦車にだけ反応し、占領中の兵を引き抜いた。T5 開始時には森9マスのうち5マスに敵が入っていた。**出発の合図（時期）を調整する方向は原因を外している**。`.tmp/lean/hold-line-adjacent-t12/`
7. 「収入不足が主因」という説明は誤り（ユーザー指摘で訂正済み）

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| 陣地線は地形の切断面（`InitialHoldSites::line`）を使い、座標を与えない | 決め打ちは禁止。map_42 では y=7 の森9マスが選ばれる |
| 強制は環境変数の試験モードだけにし、既定の動作は変えない | 上限実験。「購入のみ」のハッシュで不変を確認する |
| 比較は同じバイナリ・同じ購入列・同じ seed で「購入のみ」と並べる | 土台が違う過去の結果は対照にしない |
| 調整用の定数を増やさない | ユーザー方針（no-parameter-sprawl）。「前線都市」は線に最も近い列として地形から決める |
| seed 2・3 各1局、T12 で止める | 同一 seed は繰り返さない。反復は予算付きの区切りで止めてユーザーに判断を仰ぐ |
| 次の改善は占領順と解放。時期の合図ではない | 今回の分析結果による |

## Current State

**Working**: 強制購入、森への割当（隣接合図）、前線都市での砲の布陣と射撃、トレース `FORCED_HOLD`（`contested=` を出力）。既定の動作は不変。

**Broken / 未解決**: 森は全条件で T7 に 0 体。中央の前線都市の占領が T5〜T6 まで遅れる。

**Uncommitted Changes**（主なもの）:
- `engine/src/ai/tactics/small_map.rs`: `LineHoldMode`、`forced_line_targets`（隣接合図）、`enemy_ground_positions`、`forced_battery_targets`、トレース
- `engine/src/ai/v4/hold_line/forced.rs`（未追跡）: `contested_cells`、`assign`、`forward_battery_sites` とテスト
- `engine/src/ai/v4/hold_line.rs`・`hold_line/` 一式（未追跡）、`engine/src/ai/v4/forced_production.rs`（未追跡）、`v4/mod.rs`、`v4/production_entry.rs`
- 前セッション分: `code_path.rs`・`context/tactical.rs`・`strategy_profile.rs`・`setup.rs`・`squad_executor.rs`・`mcp-server/src/main.rs`・`scripts/eval_matchup.py`・`scripts/test_eval_matchup.py`

## Files to Know

| File | Why It Matters |
|------|----------------|
| `engine/src/ai/tactics/small_map.rs` | 強制モードの本体。`forced_line_targets` で割当を決める |
| `engine/src/ai/v4/hold_line/forced.rs` | 割当 `assign`、合図 `contested_cells`、砲座の選定 |
| `engine/src/ai/squad/planning_policy.rs:28` | 小マップの占領順（自首都から遠い順）。次の改修対象 |
| `engine/src/ai/squad/formation.rs:187` | `sort_capture_targets_by_nearest_infantry`（通常の最寄り順） |
| `engine/src/ai/v4/hold_line/terrain.rs` | 陣地線 L と自陣側 `home` の選定 |
| `battle_map42_v200_vs_human.jsonl` | 人間の勝ち局（正本） |
| `.tmp/lean/hold-line-adjacent-t12/findings.md` | 今回の結果と分析 |
| `.tmp/lean/hold-line-adjacent-t12/summarize.py` | 手番ごとの森・拠点・兵数・前線砲撃（`.tmp/lean` で実行） |
| `.tmp/lean/hold-line-adjacent-t12/collapse.py` | 森の被害・補充・攻撃の人間との比較（同じディレクトリで実行） |

## Code Context

```rust
// engine/src/ai/v4/hold_line/forced.rs
pub(crate) fn enabled() -> bool;          // OPENWARS_V4_FORCE_LINE_HOLD=1
pub(crate) fn with_batteries() -> bool;   // OPENWARS_V4_FORCE_LINE_BATTERY=1
pub(crate) fn contested_cells(
    cells: &[GridPosition],
    enemies: &HashSet<GridPosition>,
    adjacent: impl Fn(GridPosition) -> Vec<GridPosition>,
) -> HashSet<GridPosition>;
pub(crate) fn assign(
    previous: &BTreeMap<Entity, GridPosition>,
    input: &AssignInput,                       // line / unavailable / pool
    retained: impl Fn(Entity) -> bool,
    cost: impl FnMut(Entity, GridPosition) -> Option<(u32, u32)>, // None=出さない
) -> BTreeMap<Entity, GridPosition>;
```

```rust
// engine/src/ai/squad/planning_policy.rs（現状。SmallMapExpansion のとき）
targets.sort_unstable_by_key(|target| (
    std::cmp::Reverse(distance /*自首都から*/), enemy_distance, target.y, target.x,
));
```

自明でない点:
- 強制割当を受けても、未完了の占領があると兵がそちらへ戻る（T6 の横取り）。割当より占領が優先される箇所の特定は未着手
- 前回の割当は `retained` で残す。いったん出発した兵は戻らない
- 人間ログでは ID が再利用される（`UnitProduced` の兵種名を ID で引くと古い兵種が出る）。初期の兵種判定はスナップショットを使う

## Resume Instructions

1. `cargo test -p engine --lib forced` が成功することを確認する（6件）
2. **ユーザーの承認を得てから**、上の案 1〜3 を試験スイッチ（`forced::enabled()`）の中だけで実装する。純粋関数とテストを先に書く（TDD）
   - 案1: `sort_small_map_capture_targets` で、強制時かつ陣地線があれば、`line.home` の中で線に最も近い列の都市を先に並べ、その中を最寄りの歩兵の順にする
   - 案2: 前線都市を所有済みで、その上にいる歩兵（占領を終えた兵、そこで生産した兵）を `forced_line_targets` の候補にする
   - 案3: 占領中（捕獲ポイントが減っている拠点の上）の兵は候補から外す
3. `cargo fmt --all && cargo build --release -p mcp-server`
4. 比較を実行する（Git Bash。`out` は新しいディレクトリにする）:
   ```bash
   out=.tmp/lean/hold-line-front-capture-t12; mkdir -p "$out"
   cp target/release/mcp-server.exe "$out/"; cp .tmp/lean/hold-line-jit-t12/purchases.json "$out/"
   purchases=$(cygpath -am "$out/purchases.json")
   export OPENWARS_UNIFIED_PRODUCTION=1 OPENWARS_V4_PARTIAL_HOLD_LINE=0 OPENWARS_V4_FORCE_LINE_HOLD=0 OPENWARS_V4_FORCE_LINE_BATTERY=0 OPENWARS_TRACE_HOLD_LINE=1 OPENWARS_TRACE_AI_STEPS=1
   python -X utf8 scripts/run_matchup_sweep.py --variant "purchase=$out/mcp-server.exe" --variant "front=$out/mcp-server.exe" \
     --env front:OPENWARS_V4_FORCE_LINE_HOLD=1 --env front:OPENWARS_V4_FORCE_LINE_BATTERY=1 \
     --env "purchase:OPENWARS_FORCED_PURCHASES=$purchases" --env "front:OPENWARS_FORCED_PURCHASES=$purchases" \
     --env purchase:OPENWARS_FORCED_PLAYER=2 --env front:OPENWARS_FORCED_PLAYER=2 \
     --map map_42 --seeds 2,3 --p1 V4 --p2 V200 --player-order swapped --max-turns 12 --grid-type hex --jobs 8 --out-dir "$out"
   ```
   - 期待: `purchase` のハッシュは seed2 `c90c9262`、seed3 `8c085aa5`（違ったら既定の動作が変わっている）
   - 期待（仮説）: T2 の占領が中央 (3..6,5)、T4 の拠点が約 14、T5 の森の歩兵 6〜7 体、T5 以降の補充 2 体以上、撃破の増加
5. 集計スクリプトをコピーして変種名を書き換え、実行する。`summarize.py` は `('hold-line-adjacent-t12','adj')` の行を追加する。`collapse.py` はパスを変える。前回の adj・人間と並べて報告し、そこで止まる

## Setup Required

- `OPENWARS_FORCED_PURCHASES` は**絶対パス**で渡す（Git Bash では `cygpath -am`）。`OPENWARS_FORCED_PLAYER=2`
- `OPENWARS_TRACE_AI_STEPS=1` がないと `logs/servers/*.stderr.log` にトレースが残らない
- 並列数は `--jobs 8`

## Warnings

- `strategic_history` の盤面は **P2（V4）の手番後**。人間ログの `TurnSnapshot`（player=2）は人間の手番の**開始時**
- `invasion_events` の攻撃には標的の位置がない。V4 の撃破数は「消えたか」からの推定
- action_history の `Attack { target_pos }` は攻撃者の移動先（標的の位置ではない）
- `engine/Cargo.toml` の `unused dependency anyhow` 警告は既存のもの
- 関連メモリ: map42-forest-comes-from-capture-order、v4-map42-falsified-hypotheses、production-eval-attempts-history、indirect-units-need-a-screen、no-parameter-sprawl
- 報告は日本語で、結論を先に書く。改善しない案は保留にし、反復は予算付きの区切りで止めてユーザーに判断を仰ぐ
