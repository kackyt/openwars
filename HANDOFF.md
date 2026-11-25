# Handoff: map_42 後攻 V4 — 前線都市優先・占領優先・間接火力連携（強制上限実験）

**Generated**: 2026-11-25（夜）
**Branch**: `feature/v4-unified-production-lean`
**Status**: 案 1〜3・占領優先・`Wait` バグ修正・強制購入（`Rockets` 導入）による T30 勝利確認まで完了。次の課題は **通常 V4 生産への間接部隊導入**

## Goal

map_42 後攻で V4 が V200 に通常勝利できない。人間の勝ち筋（前線都市 `y=5` を確保し、森 `y=7` を歩兵で埋め、間接部隊で敵を削って前進する）を試験用スイッチで検証し、製品ロジックへ還元すべき要素を切り分ける。

## Completed

- [x] **案 1（占領順）**: `sort_small_map_capture_targets`（`planning_policy.rs`）で `forced::enabled()` かつ陣地線があるとき、`line.home` 内の最前列都市（`front_cities`）を最寄り歩兵順に優先占領する
- [x] **案 2（兵の解放）**: 所有済み前線都市の上にいる歩兵（占領完了・前線生産）を `contested` の有無によらず陣地線へ解放する
- [x] **案 3 + 占領兵の占領優先**: `forced::can_hold_line_in_forced_mode` により、占領中（捕獲ポイント減少）および占領任務中の歩兵は新規に引き抜かず占領完了を優先させ、所有済み前線都市上の兵・割当済みの兵だけを線へ向ける。行軍中の占領横取りを防ぐ `is_forced_hold` も追加
- [x] **迂回行軍時の `command=None` バグ修正**: `consider_wait`（`small_map.rs`）の `moves_away_from_campaign` を `!unit.is_forced_hold` のときだけ適用し、前線閉塞時の迂回待機が弾かれないようにした
- [x] **通常 V4 生産での T12 比較（seed 2, 3）**: `.tmp/lean/hold-line-capture-prio-t12/`
  - T4=10 / T5=14 拠点と経済ペースを維持し、T6〜T9 で森 `y=7` に最大 6 体を展開（seed 2 の T12 スコア `29,375 → 85,514`、`36 vs 27`）
- [x] **強制購入（`Rockets` 導入）での T12 / T30 検証**: `.tmp/lean/hold-line-battery-t12/`
  - `FORCE_LINE_HOLD=1` + `FORCE_LINE_BATTERY=0` + `Rockets` 強制購入列（T1〜T8）により、T12 で両 seed ともユニット価値が V200 を上回り（seed 2 `P2_Win_MaxTurns`）、T30 では **seed 3 が森 8 体・`y=8` 以降へ 6 体前進（`50 vs 17`）で圧勝（`P2_Win_MaxTurns`）**

## Not Yet Done

- [ ] **通常 V4 生産への間接部隊導入**: 強制購入なし（通常 V4 生産）では T1〜T7 に `Rockets` 等をほとんど買わないため、森の歩兵だけで消耗戦になる。小マップの生産判断で対地間接部隊（`Rockets` 等）を適切に買うロジックを検討・実装する
- [ ] **前線都市の砲座と生産口の両立**: `FORCE_LINE_BATTERY=1` のままだと前線都市（`y=5`）を砲が塞いで歩兵生産を止めてしまう。生産口ではない後方タイルや空き都市から射程 2〜3 で撃つ配置を検討する
- [ ] **map_42 後攻 seed 1..12 の全局比較**（予算は事前にユーザーへ提示する）
- [ ] 計画書 `docs/07-project-management/plans/v4-hold-line-mission.md` へ結果を反映する

## Failed / Reverted Approaches (Don't Repeat These)

1. **敵がいる森マスを `unavailable` から外す案（`reclaim-v2`）**: 敵のいる森の前（防御 5% の平地 `y=6`）で足止め・消耗して T6〜T8 の森展開数が落ちた（seed 2 T12 スコア `85,514 → 36,295`）。敵マスは `unavailable` のまま空いている森（防御 20%）を優先するのが正しい
2. **`OPENWARS_V4_FORCE_LINE_BATTERY=1` と前線歩兵生産の併用**: `Rockets` が `y=5` の前線都市に居座ると、前線都市での歩兵生産を物理的に塞いで総生産数が激減する（`65 → 51` 体）。`Rockets`（射程 2〜3）は `y=4` や非生産マスからでも `y=7` を撃てる
3. **（前版までの失敗案）**: 部分保持（候補 0）、敵追跡のみ候補、前線都市 1 つで全員転用、見極め v1、隣接合図のみ

## Files Modified in This Commit

- `engine/src/ai/v4/hold_line/forced.rs`: `front_cities`, `sort_capture_targets_front_first`, `is_capturing_in_progress`, `can_hold_line_in_forced_mode` と単体テスト
- `engine/src/ai/v4/hold_line/planner.rs`: `InitialHoldSites::line_for`
- `engine/src/ai/squad/planning_policy.rs`: `sort_small_map_capture_targets` の強制時前線都市優先
- `engine/src/ai/tactics/small_map.rs`: `forced_line_targets` の占領優先・前線都市解放、`is_forced_hold`、`consider_wait` の修正

## Verification

- `cargo test -p engine --lib forced`: 9/9 passed
- `cargo fmt --all -- --check`: passed
- `cargo clippy --all-targets --all-features -- -D warnings`: passed
