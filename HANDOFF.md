# Handoff: V4の複数ルート早期制圧改善

**Generated**: 2026-10-29 12:29 +09:00

**Branch**: `claude/dazzling-lovelace-if6j7f`、基準HEAD `d67f31b`

**Status**: In Progress — 改善未達。実験版には失敗テストが残る

## Goal

map_20後攻のように2ルートの早期制圧・維持が重要な盤面で、V4のV200相手の勝率と形勢を改善する。ユーザーは前回の「候補不採用・改善未達」の報告を却下し、設計を壊すことを恐れず仮説に沿って修正し、V200との対戦結果を報告するよう求めた。特定マップや生産列の決め打ちは避ける。

## Completed

- [x] 既存の集中作戦WIPと、工場退避・前線経路・作戦継続予算の候補を隔離して検証した。後述の退行や効果不足から本体には採用していない。
- [x] 作戦の進路切替、拠点防衛と占領期限、初手生産を局面テストおよびV200実対戦で調査した。
- [x] 事前防衛の初期候補をmap_20後攻・12シード・60ターンで基準と比較。序盤拠点は増えたが上限判定勝利は8/12→2/12、通常勝利は0/12→0/12。
- [x] 引き継ぎ用に実験差分・集計・分析ツールと60T全24対局の圧縮JSONを `handoff/map20-20261029/` に保存した。ユーザーには改善未達・本体未適用を報告済み。

## Not Yet Done

- [ ] 実際の作戦・部隊所属・生産予算・実行目標の一貫性と失効条件を設計し、2ルートの早期制圧・保持に結びつける。
- [ ] 最新の事前防衛実験の行動済みユニット誤計上を修正し、再現テストを通す。防衛戦力を使う機会損失も考慮し、実行された命令まで検証する。
- [ ] 防衛初期候補の序盤改善が終盤悪化へ転じる具体的局面を同一seedで特定する。原因の断定は未了。
- [ ] 採用候補について標準版・V200の同条件比較、影響マップの回帰確認、最終ソースの `cargo test`・fmt・clippy を実施する。

## Failed Approaches (Don't Repeat These)

数値はV4後攻対V200先攻。**上限判定勝利を通常勝利と混同しない。** 同一seedでも行動列に実行時の揺れがあるため、各1回の結果は確率的な確証ではない。

| 案 | 結果と判断 |
|---|---|
| 元の集中・進軍路作戦WIP | map_20の60T/12seedは上限判定8勝→9勝、通常勝利0→0。ただしmap_35のT15拠点が3seedとも減少、map_8も悪化。一括採用しない。`handoff/map20-20261029/reports/route-full12.md` |
| 工場早期退避の単独変更 | map_20の60T/12seedは上限判定8勝→10勝、通常勝利0→0。ただしmap_35の前線が退行。`handoff/map20-20261029/reports/relief-full12.md` |
| 前線選定を実移動Dijkstra化 | map_20後攻3seedの上限判定2勝→1勝。距離の精緻化だけでは解決しない。`handoff/map20-20261029/reports/front-study-sweep.md` |
| 自軍拠点防衛の実行順を早める、最後の手段で占領兵も使う | `handoff/map20-20261029/reports/defense-first-screen.md`、`defense-last-resort-screen.md` の短縮試験で目的の根本改善は確認できなかった。 |
| 初手の戦闘生産制限を外す | map_20 seed1 T8拠点・収入は基準と同じ21・24000。候補を増やすだけでは狙いが実購入・勝利に届くとは限らない。`handoff/map20-20261029/reports/opening-timing-screen.md` |
| 経路所属を固定 | seed1で観測上の経路切替40/132→1/97だが、経路所属の観測数も170→126に減り、T15拠点19→19。登録から外れただけの可能性があり、数字だけで実移動の改善とは言えない。`handoff/map20-20261029/reports/stable-lanes-v2-screen.md` |
| 占領期限前の事前防衛責務、初期版 | `handoff/map20-20261029/reports/defense-objective-full12.md`。T15平均拠点19.0→約20.1、収入22000→約23083だが60T上限判定8勝4敗→2勝10敗、通常勝利は両方0。序盤拠点増だけを成功と扱わない。 |
| 実経路・ZOCを加味して防衛に必要最小限の戦力を使う後続案 | 30T/3seedの短縮スクリーニングではmap_20の上限判定2勝→0勝。`handoff/map20-20261029/reports/defense-minimal-sweep.md`。60T/12seedの結果ではなく、初期版とは別バイナリ。 |

## Key Decisions

| Decision | Rationale |
|---|---|
| 退行・効果不足の候補を本体へ移植しない | 序盤の指標改善だけでは目標未達。他マップでの劣化も確認した。 |
| 単純な距離・重みの調整ではなく、作戦目標・所属・生産・実行の正本と失効条件を検討する | 目標が下流で上書きされる、進路を反転する、攻守と防衛予算が不整合になる例がある。 |
| 試験は再現fixture→短縮実戦→同条件12seedへ進める | 低コストで仮説と実行到達性を反証し、短縮試験の判定勝敗を本来の60Tと混同しない。 |
| 中間成果は保存し、既存作業やユーザーファイルを削除しない | 実験ごとに版が異なる。引き継ぎには `handoff/map20-20261029/` のパッチ・集計・圧縮ログを使う。 |

## Current State

**Working**: 本体の追跡ソースは基準 `d67f31b` のまま。作業開始時の基準版では `cargo test --workspace` 1145成功・2無視、fmt・clippy成功。これは**基準版の過去の結果**であり実験版の検証ではない。

**Broken**: `handoff/map20-20261029/patches/defense-objective.patch` を適用した最新実験版で `defense_does_not_reuse_a_unit_that_already_acted` が失敗。出力: `行動済みの部隊は次の敵占領より先に攻撃できない`、`test result: FAILED. 0 passed; 1 failed`。このテストは前進中に追加したもので、最新実験は未完成。最新ソースに対する全体テスト・fmt・clippyは未確認。

**Uncommitted Changes**: `HANDOFF.md` と `handoff/map20-20261029/` の引き継ぎ資材。ユーザー由来の多数の未追跡ファイルもある。実験パッチは基準 `d67f31b` に対する独立の差分であり、互いに重ねて適用しない。

**重要**: 12seedで測った事前防衛の初期版と、保存した最新の `defense-objective.patch` は異なる。初期版の実行ファイルや正確なソース差分は保存しておらず、対戦ログと集計のみ。初期版の結果を最新パッチの結果と呼ばない。

## Files to Know

| File | Why It Matters |
|---|---|
| `handoff/v4-route-operations-wip.patch` | 前セッションの大規模WIP。実験フラグ・デバッグを含み、一括採用不可。元資料は `handoff/map20-20261029/previous-handoff.md`。 |
| `engine/src/ai/squad/objective_allocation.rs` | 本体側の攻勢戦闘責務の正本。 |
| `engine/src/ai/operation_assignment.rs`, `engine/src/ai/squad.rs`, `engine/src/ai/runtime/turn.rs` | 作戦割当→部隊→実行の流れを追う入口。 |
| `engine/src/ai/v4/capital_route/` | 進軍路DAG、戦力・予算・作戦。 |
| `handoff/map20-20261029/patches/defense-objective.patch` | 最新事前防衛の実験ソース3ファイルと失敗fixture。基準への独立差分。 |
| `handoff/map20-20261029/patches/` | 他の試案の独立差分。`route-study.patch` は未追跡だった追加ソースも含む。すべて計測後に変化した可能性があり、計測時点の再現と見なさない。 |
| `handoff/map20-20261029/logs/defense-full12.zip` | 初期事前防衛案と基準版の対戦JSON、全12seed×2。展開方法は下記。 |
| `handoff/map20-20261029/reports/` | 主要な対戦集計、計測プロトコル、no-hack原則。 |
| `handoff/map20-20261029/tools/route-contract-analysis-20261029.py` | 同一局面で経路所属・反転・西側拠点を集計。隣の `test_route_contract_analysis_20261029.py` で検証可能。 |
| `handoff/map20-20261029/tools/compare_benchmarks.py` | seed別結果の比較。12seedの再集計に使える。 |
| `scripts/run_matchup_sweep.py` | releaseの比較バイナリを用いる並列対戦。 |

## Code Context

本体の攻勢責務入口（`engine/src/ai/squad/objective_allocation.rs`）:

```rust
pub(super) fn objective_combat_responsibilities(
    world: &World,
    player: PlayerId,
    island: IslandId,
    members: &[Entity],
    capture_members: &[Entity],
    objectives: &[GridPosition],
    connectivity: &mut TerrainConnectivity,
) -> Vec<CampaignResponsibility>
```

最新の実験版では `defense_threats` が自軍施設へ向かう敵占領役と期限を求め、`Allocation::assign_defenses` が期限内の射点・攻撃必要数を用いて仮予約する。既知の欠落は `ActionCompleted` の考慮。攻撃可能時刻は手番順・移動・敵ZOC・間接攻撃・射点競合を含めて計算すること。防衛用距離は実験版の以下を使う:

```rust
pub(crate) fn calculate_action_distance_respecting_zoc(
    map: &Map, registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    start: (usize, usize), target: (usize, usize),
    movement_type: MovementType, max_mp: u32, max_fuel: u32,
    interaction_min_range: u32, interaction_max_range: u32,
    player_id: PlayerId, cache: &mut ActionTurnDistanceCache,
) -> Option<ActionTurnDistance>
```

この実験版の追加API・ロジックの妥当性や本体採用は未確定。局面fixtureが通っても、実際のSquad再編→命令→位置まで確認する。

## Resume Instructions

1. `git status --short`、`git diff --stat`、`git worktree list` を確認する。旧資料は `handoff/map20-20261029/previous-handoff.md`。保存パッチ・ログを含む `handoff/map20-20261029/` を一緒に引き継ぐ。
2. 以下で新しい展開先に12seed×2のJSONを取り出し、基準と初期防衛候補の同一ターンの部隊・戦闘資産・生産・拠点・命令を追う。T15の優位がどこで失われるかを特定し、観察と仮説を分ける。勝敗反転seedは1, 4, 8, 9, 10, 12。出力先が既存なら別名にする。

   ```powershell
   python -m zipfile -e handoff/map20-20261029/logs/defense-full12.zip .tmp/handoff-defense-games
   python handoff/map20-20261029/tools/compare_benchmarks.py .tmp/handoff-defense-games/baseline/map_20_V200-V4_r0 .tmp/handoff-defense-games/defense/map_20_V200-V4_r0 --subject V4 --seeds 12 --turn 15
   ```

   期待結果: 通常勝利0→0、60T上限判定勝利8→2。展開後約1.4GBが必要。ファイルの上書きを避けるため展開先が存在しないことを事前に確認する。
3. 最新実験の独立worktreeを作り、保存パッチを適用して失敗を再現する（PowerShell、既存の作業先は使わない）:

   ```powershell
   $work = '.tmp/handoff-defense-worktree'
   if (Test-Path -LiteralPath $work) { throw '既存作業を保護するため停止' }
   $patch = (Resolve-Path 'handoff/map20-20261029/patches/defense-objective.patch').Path
   git worktree add --detach $work d67f31b
   if ($LASTEXITCODE -ne 0) { throw 'worktree作成失敗' }
   git -C $work apply --check $patch
   if ($LASTEXITCODE -ne 0) { throw 'パッチ検査失敗' }
   git -C $work apply $patch
   if ($LASTEXITCODE -ne 0) { throw 'パッチ適用失敗' }
   $env:CARGO_TARGET_DIR = "$PWD/.tmp/handoff-defense-target"
   cargo test --manifest-path "$work/Cargo.toml" -p engine defense_does_not_reuse_a_unit_that_already_acted --lib
   ```

   現状の期待結果: 上記アサーションで失敗。修正後は成功し、未行動・次手番・先後・敵ZOC・射点重複・間接攻撃も回帰しないこと。`cargo test --manifest-path "$work/Cargo.toml" -p engine objective_allocation --lib` で関連テストを実行する。
4. 局面テスト後、変更をreleaseビルドし、基準と候補を**別名で固定**して新しい出力先へ短縮実戦を実行する。計画上の割当だけでなく実命令・実位置・拠点所有を確認。既存exeや出力先は上書きしない。
5. 有望なら `scripts/run_matchup_sweep.py` で `--map map_20 --p1 V200 --p2 V4 --player-order as-given --seeds 1,2,3,4,5,6,7,8,9,10,11,12 --max-turns 60 --grid-type hex --jobs 8` を基準と候補に同一指定。通常勝利、上限判定、T15/T20以降の指標を別々に示す。map_35・map_8など影響範囲も確認。対局揺れの調査には `--repeats 3` 以上。
6. 採用後にのみ本体へ統合し、最終状態で `cargo test`、`cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings` を実行。成功・失敗・未実施を区別して日本語で報告。

## Warnings

- 原則とプロジェクト規約は `CLAUDE.md` を読む。Webのパッケージ管理はpnpm限定。ユーザーへの報告とコードコメントは日本語。
- `.tmp/` は引き継ぎ対象外。保存したパッチ・集計・圧縮ログは `handoff/map20-20261029/` にあり、引き継ぎ時にこの未追跡フォルダを必ず含める。実行ファイル・ビルド生成物・60Tの初期版以外の詳細ログは保存していない。
- 行動済み・途中移動・行動不能、敵の占領完了手番と自軍の行動順、射点の重複、進路が到達不能・地域変化・戦力喪失で失効する条件を扱う。存在しない防衛役や同一部隊を複数戦線に計上しない。
- 間接兵器は直接攻撃ユニットの前衛があって初めて機能する。分岐や任意閾値で特定マップをハックしない。V1〜V4の共通化で関数内 `is_extended` 分岐を増やさない。
- 既存の旧引き継ぎにある `jobs 4` や作業後の削除手順は今回の運用に優先しない。対戦は `jobs 8` を標準とし、他人や他実験のプロセスは止めない。
