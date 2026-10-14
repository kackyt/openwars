# AI再設計v3 段階2: 部隊再編の段分割・方策enum・盤面snapshot・propose/commit

[提案書](ai-redesign-v3-proposal.md) §B と段階2のうち、挙動を変えない（R）部分を実装した記録。
前半（段分割と方策enum）の基準は段階1の仕上げコミット `c6df306`、後半の基準は前半のコミット `ebf3acf`。

# 前半: 部隊再編の段分割と方策enum

## やったこと

### 1. `plan_squads_with_hooks`（1関数・約1190行）を段に分ける

再編本体を `engine/src/ai/squad/formation.rs` に移し、次の段の関数にした。各段は前段の結果だけを受け取る。

| 段 | 関数 | 入力 → 出力 |
|---|---|---|
| 1 準備 | `update_squads`・`prepare_squads_for_replanning` | 前手番のSquad → 再計画前のSquad |
| 2 分析と採用 | `analyze_strategy_for_formation`・`PlanningPolicy::adopt_portfolio` | 盤面 → `ProductionStrategy`・採用portfolio |
| 3 campaign編成 | `form_campaign_squads`（＋V4配備） | 採用portfolio → `CampaignFormation`（優先順assignment・予約Entity・封鎖島・停止島） |
| 4 汎用編成 | `FormationBoard::observe`・`FreeUnits::collect`、`assign_generic_transports`・`unload_unreserved_loaded_transports`・`assign_capture_depth_transports`・`form_capital_defense_squads`・`form_capture_squads`・`form_attack_squads`・`absorb_surplus_combat_units` | 盤面の事実＋free unit → Squad |
| 5 仕上げ | `finalize_formation` | V4所属修復 → 正規化 → Roadmap観測 |

盤面の事実（地図・占有・物件の所有・島の分類・自首都）は `FormationBoard` に1回だけ集め、
汎用編成の各段は読み取り専用で受け取る。距離cacheは `FormationCaches` にまとめた。

### 2. 8個の前後hook（`SquadPlanningHooks`）を方策enumへ

trait と `MainPlanningHooks` / `ExtendedPlanningHooks` を廃止し、
`formation::PlanningPolicy { Standard, SmallMap }` にした。通常/小規模V4で違うのは次の6点だけで、
すべてこのenumのメソッドにある。

- 採用（`adopt_portfolio`）: 通常はそのまま、小規模V4は首都ルートDAGとRoadmapで選ぶ
- 局地割当（`prepare_local_assignment`）
- 旧首都防衛の有無（`uses_legacy_capital_defense`）
- 占領目標の順序（`sort_capture_targets`）と占領要員の選択（`select_capture_infantry`）
- Roadmap観測の位置（`before_final_reconcile` / `after_final_reconcile`）と、小規模V4の事前処理

小規模V4側の本文は `squad/extended.rs` の自由関数（`adopt_portfolio`・`sort_small_map_capture_targets`・
`select_nearest_capture_infantry`）にした。提案書は「2つの方策enum」としていたが、採用と汎用編成の差は
常に同じ経路で切り替わるため、1つのenumにまとめた。

### 3. 使われない束縛の削除

- `my_infantry_islands`（計算するだけで読まれていなかった）
- `tactical_reserved_entities`（常に空。配備Squad準備へは空集合を直接渡す）

## 検証

| 項目 | 結果 |
|---|---|
| 判断fingerprint | V2/V3/V4 × map_1/2/6/25 × seed1,2 × 両手番 × 30T の48条件で、行動列hashと終局状態が `c6df306` と完全一致 |
| `cargo test --release -p engine --lib` | 872成功・1 ignore（既存）・失敗0。再編の契約テスト5件を含む |
| `cargo clippy --all-targets --all-features -- -D warnings` / `cargo fmt --all -- --check` | 成功 |

契約テストのうち、hookへ記録用の実装を注入して呼出順を検査していたテストは、注入点がなくなったため
「共通の段がSquadManagerを戻し、採用portfolioと編成済みを手番cacheへ記録する」ことを
両方策で直接検査するテストに置き換えた。

## 構造・複雑度（`scripts/measure_ai_complexity.py`、テスト除外）

| 指標 | 変更前 | 変更後 |
|---|---:|---:|
| 再編の最大関数の直接CC | 148（`plan_squads_with_hooks`） | 23（`load_generic_transport`） |
| 同・直接認知的複雑度 | 373 | 51 |
| 入口関数の直接CC | 148 | 5（`plan_squads_with_policy`） |
| 同名の通常/extended関数ペア | 37 | 29 |
| 禁止方向の参照行 | 0 | 0 |

## 前半の時点で残した項目

提案書 §B のうち次を後半へ回した（後半で R の4項目を実装。S は未着手、末尾参照）。

- R: 盤面snapshot（`context::PlayerBoard`）を戦略分析と編成で共有する（占有モデル2種を明示して維持）
- R: `analyze_strategy_internal`（685行）を状況判断・campaign提案・生産需要へ分け、Squad編成では生産需要を計算しない
- R: 編成の propose/commit 分離（Squadの作成・追加を提案として返し、1つの更新口で確定する。SquadIdの発番順は保つ）
- R: 地形連結cache（campaign編成用と汎用編成用）の統合
- S: 編成後のRoadmap 2回目の選択を観測だけにする・経路の精錬と観測を1回にする・汎用輸送を採用portfolioへ・旧首都防衛の削除

# 後半: 盤面snapshot・分析の分割・地形連結cache・propose/commit

Rの4項目を1項目1コミットで実装し、各コミットで判断fingerprintを基準 `ebf3acf` と照合した。

| コミット | 内容 |
|---|---|
| R1 | 戦略分析と部隊再編で盤面snapshot（`PlayerBoard`）を共有する |
| R2 | 戦略分析を状況判断・campaign提案・生産需要へ分け、再編では生産需要を計算しない |
| R3 | 地形連結cacheをcampaign編成・配備・汎用編成で1つにする |
| R4 | 汎用編成を提案（propose）と確定（commit）に分ける |

## やったこと

### 1. 盤面snapshot `context::PlayerBoard`（R1）

`engine/src/ai/context/board.rs`。unit（勢力・座標・能力値・搭載中か・輸送枠・HP・弾薬・燃料・行動済み）、
物件（座標・所有者・物件地形）、地図・registry・島をWorldから1回だけ写し取る。Worldは書かない。
unitと物件はクエリの列挙順のまま持ち、占有や所有のmapをこの順に挿入するので、
個別にWorldを走査していたときと内容も反復順も同じになる。

占有モデルは用途の違いを残すため2つのメソッドに分けて明示した（統一はS）。

| メソッド | 使う所 | 搭載中unit | 輸送unitの空き枠 |
|---|---|---|---|
| `strategic_occupancy` | 状況判断（首都脅威）・生産需要（対空・平和物件） | 含める | 最大積載数 |
| `formation_occupancy` | 汎用編成のターン距離 | 除く | 最大積載数 − 実積載 |

部隊再編は再計画の準備の直後に1回観測し、戦略分析と汎用編成（`FormationBoard`）へ同じsnapshotを渡す。
再編の間はunit・物件・地図を変える処理が無い（campaign編成・配備・採用が書くのはAIの台帳だけ）ことを確認した。
`FormationBoard` は地図・registry・島を複製せず借用し、free unit・敵兵種・島ごとの敵数もsnapshotから数える。

registryが無いWorldでは、戦略分析は従来どおりマスターデータを読み込み、汎用編成もこれに揃えた
（従来の汎用編成は空のregistryだった。対局では必ずregistryがあるので判断は変わらない）。

### 2. 戦略分析の分割（R2）

`analyze_strategy_internal`（685行・直接CC 88）を次の3段に分け、本体は順に呼んでまとめるだけにした（26行・直接CC 4）。

| 段 | 場所 | 入力 → 出力 |
|---|---|---|
| campaign提案 | `planning::campaign::propose` | World（SquadManager・予約Entity） → `IslandCampaignPortfolio` |
| 状況判断 | `planning::situation::assess` | `&PlayerBoard` → `StrategicSituation`（フェーズ・未所有/敵物件・優先目標・自軍物件・自島・収入要否・地図上のunit） |
| 生産需要 | `strategy::apply_production_demand` | 盤面＋状況＋portfolio → 構成比・占領/輸送需要・需要マトリクス・対空・campaign不足・平和物件 |

部隊再編は状況判断とcampaign提案だけを使い（`FormationAnalysis`）、生産需要を計算しない。
V3/V4は同じ手番に分析を2回していたが、2回目（生産判断）だけが生産需要を計算する。
V2は再編でcampaign提案をしないため、SquadManagerをWorldへ一時的に戻す処理も省いた。
V2の輸送候補だけが使う侵攻許可（`is_invasion_allowed`）はV2のときだけ島ごとに求める。

campaign提案は現状もWorldへ書く（拡張V4の首都攻略DAGの準備と、診断用 `IslandCampaignDiagnostics`）。
提案を純関数にするのは、所属の正本の一本化（段階3）と合わせて扱う。
首都脅威と平和物件で共有していたターン距離cacheは、両者が同じ占有モデルで引くため別々に持っても結果は同じ。

### 3. 地形連結cacheの統合（R3）

地形連結（`TerrainConnectivity`）は地図と移動種別だけで決まり占有に依存しない。再編の入口で作る
`FormationCaches` の1つを campaign編成・V4配備（`prepare_deployment_squads_with_connectivity`）・汎用編成で共有する。
ターン距離cacheは編成の占有モデルに依存するので、従来どおり汎用編成だけが使う。
状況判断（交戦判定）の連結cacheは生産判断と共有する関数の中にあるため別に残した。

### 4. 汎用編成の propose/commit（R4）

`engine/src/ai/squad/assignment.rs`。汎用編成の7段（汎用輸送・積載輸送の降車・占領深度の予備便・旧首都防衛・
占領・攻撃・余剰吸収）は `SquadManager` を書かず、`AssignmentProposal` へ「Squadの新設（`SquadDraft`）」と
「既存/新設Squadへの追加」を順に積む。最後に `AssignmentProposal::commit` が1回だけ適用する。

- 既存判定（同じ任務・目標のSquadがあるか）・施設奪取数・定員判定は、提案の要約（`SquadView`: 変更可否・任務・目標・人数）を読む。
  確定前でも前段・同じ段の提案が見えるので、直接書き換えていたときと同じ判断になる
- 変更可否は提案を作るときに既存Squadごとに1回求める。汎用編成は自軍のfree unitだけを加えるので、
  提案の間に変わらない。新設Squadは自分が所有し自軍unitだけを持つので常に変更できる
- 新設Squadの `SquadId` は確定時に提案順で発番するため、従来と同じ値になる
- `commit` は提案後に `SquadManager` のSquad数が変わっていればpanicし、同じEntityの重複割当を `debug_assert` で検査する
- 汎用編成の段は `&mut World` を取らない（小規模V4の占領目標の順序付けだけが従来どおりWorldを読む）

campaign編成（`prepare_campaign_*`・安全占領・一時停止）とV4配備は対象外とした。これらは既存Squadの目標・出発許可を
書き換え、`UnitOperationRegistry`・`V4DeploymentRegistry` も更新するため、提案と確定に分けるには
所属の更新口（段階3の `OperationLedger`）が先に必要になる。

## 検証

| 項目 | 結果 |
|---|---|
| 判断fingerprint（各コミット） | V2/V3/V4 × map_1/2/6/25 × seed1,2 × 両手番 × 30T の48条件で、R1〜R4の各コミットとも行動列hash・終局状態・**全行動列の本文**が `ebf3acf` と完全一致 |
| 主比較（段階末） | V4（相手V200）× map_1/2/6/25/32 × seed 1/2/3 × 両手番 × 60T上限の30局で、全局の**全行動列の本文**・行動数・勝敗・ターン数・終局時の物件数とunit価値が `ebf3acf` と完全一致 |
| `cargo test --release --locked`（workspace全体） | 921成功・既存ignore 2件・失敗0。goldenの更新なし |
| `cargo test -p engine --lib`（debug。`debug_assert` を含む） | 880件中879成功・1 ignore（既存）・失敗0 |
| `cargo clippy --all-targets --all-features -- -D warnings` / `cargo fmt --all -- --check` | 成功 |
| `cargo check --locked -p engine --target wasm32-unknown-unknown` | 成功 |

比較はどちらも同じRust 1.97.0・release設定で基準と候補のテストバイナリを作り、`engine/tests/ai_determinism.rs` の
`print_decision_fingerprints` で各局の行動列を書き出して照合した（1局1行動列、別プロセス）。
主比較の経過時間は、基準と候補を同じ4並列のキューで流して map_6 6局が基準736秒・候補713秒、
他4マップ24局が基準603秒・候補589秒（共有環境の参考値。高速化の主張には使わない）。

追加したテスト: 占有モデル2種（`context::board`）、状況判断の首都脅威と生産側分析との一致（`planning::situation`）、
提案の発番順・確定前の参照・他playerのSquad・確定前のSquadManager変更の拒否（`squad::assignment`）。

## 構造・複雑度（`scripts/measure_ai_complexity.py`・`measure_ai_structure.py`、テスト除外、RCA 0.0.25）

| 指標 | 変更前（`ebf3acf`） | 変更後 |
|---|---:|---:|
| `strategy.rs` の最大関数の直接CC | 88（`analyze_strategy_internal`） | 42（`apply_production_demand`） |
| 同・直接認知的複雑度 | 168 | 91 |
| `analyze_strategy_internal` の行数 / 直接CC | 685 / 88 | 26 / 4 |
| 状況判断 `situation::assess` の直接CC / 認知 | ― | 20 / 30 |
| 提案の確定 `AssignmentProposal::commit` の直接CC | ― | 4 |
| AI全体の直接CC 合計 / 直接認知 合計 | 11282 / 9635 | 11303 / 9600 |
| 直接CC 最大 / p95 | 108 / 8 | 108 / 8 |
| 禁止方向の参照行 | 0 | 0 |
| 同名の通常/extended関数ペア・経路判定・修復処理呼出 | 29・42・16 | 29・42・16 |

直接CC合計の微増（+21）は、分けた段の関数境界と提案型のメソッドの分。認知的複雑度の合計は35減った。

## 段階2で残したもの

- S（戦略変更。1件ずつ評価して採否を決める）: 編成後のRoadmap 2回目の選択を観測だけにする、経路の精錬と観測を1回にする、
  汎用輸送を採用portfolioへ、旧首都防衛の削除、占有モデルの統一
- R（段階3と合わせる）: campaign編成・V4配備の propose/commit（所属の更新口が要る）、campaign提案からWorldへの書き込みを除く
