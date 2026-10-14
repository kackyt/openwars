# AI再設計v3 段階2: 部隊再編の段分割・方策enum・盤面snapshot・propose/commit

[提案書](ai-redesign-v3-proposal.md) §B と段階2のR実装、およびS4件の逐次実装・対戦評価の記録。
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

# S: 戦略変更の逐次評価（4件完了）

基準はR後半の記録まで含む `d925709`。S1〜S4を1件ずつ実装し、成績を確認してから次の変更を重ねる。
占有モデルの統一は今回の対象外。旧首都防衛はV2も含め削除するが、V2の対戦評価は行わない。

## 縮小評価の条件

504局評価の代わりに、次の **28局/版・50ターン上限** で確認する（2026-10-14改訂）。

- V4対V200、hex、map_1/2/6/25/32/42/10
- seed 1,2 × 両手番 × 1反復（7 × 2 × 2 = 28局）
- 固定した初期baselineを再利用し、各候補は直前採用版と初期baselineの両方へ比較する
- S4だけV3対V200のmap_6/25 × seed1 × 両手番を前後で追加する（4局/版）
- 通常勝敗・上限判定・引分を分離し、map/先後別の成績と経済・戦術指標を確認する
- 判断が難しい条件は別seedを追加する。同seedの反復は非決定性が疑われる場合にだけ行う

当初は旧来の非決定性対策として3反復を指定したが、現在はahashの決定性パッチを適用済みで、
初期baselineの全28条件で3反復の勝敗・ターン・行動hashが一致した。ユーザー合意によりS2以降は1反復へ削減する。
baselineとS1の主比較もr0の28条件に揃え、既に実行した残り反復は再現性確認としてのみ使う。

28局は縮小スクリーニングであり、504局相当の非劣後を証明するものではない。
Sの行動列変化は想定内なので、hash一致を採否条件にはしない。
固定バイナリ・ソース差分・評価条件・ログは `.tmp/stage2-s/` に保存する。

## S1: 編成後Roadmapの再選択をやめる（採用）

`victory_roadmap/extended.rs` の選択・承認と実績照合・公開を分けた。
編成前は従来どおり候補比較と兵站gateによる承認を行い、編成後は採用portfolioと候補評価を変えず、
盤面・生産・編成の実績と最新Squadへの指令参照を更新する。DAGの実行区間更新は維持する。
通常V4の観測位置は変更していない。

- 新契約テスト: 編成後に候補評価が上書きされない、採用portfolioが変わらない、Squadの作り直し後に指令参照が更新される
- 変更前は候補評価の上書きで失敗（795 ≠ 123456）、変更後は成功
- `cargo test --release --locked -p engine --lib`: 880成功・既存ignore 1件・失敗0
- `cargo fmt --all -- --check` / `cargo clippy --all-targets --all-features -- -D warnings`: 成功
- baseline/S1〜S4の50ターン・28条件比較が完了・異常0。Sの4件はすべて採用。占有モデルの統一は今回の対象外

### 対戦結果: baseline → S1（50T・28条件）

各map × seed × 先後につきr0の1局だけを集計する。残りの同seed反復は再現性確認であり、成績の分母に加えない。

| 成績 | baseline | S1 |
|---|---:|---:|
| 総合（勝／負／分） | 20／8／0 | 20／8／0 |
| 通常決着（勝／負） | 7／6 | 7／6 |
| ターン上限判定（勝／負） | 13／2 | 13／2 |
| 先手14条件（勝／負） | 11／3 | 11／3 |
| 後手14条件（勝／負） | 9／5 | 9／5 |
| 総合勝率 | 71.4% | 71.4% |

| マップ（各4条件） | baseline 通常勝／負 | baseline 上限勝／負 | S1 通常勝／負 | S1 上限勝／負 |
|---|---:|---:|---:|---:|
| map_1 | 3／1 | 0／0 | 3／1 | 0／0 |
| map_2 | 1／2 | 1／0 | 1／2 | 1／0 |
| map_6 | 0／0 | 4／0 | 0／0 | 4／0 |
| map_25 | 2／1 | 1／0 | 2／1 | 1／0 |
| map_32 | 1／1 | 1／1 | 1／1 | 1／1 |
| map_42 | 0／1 | 3／0 | 0／1 | 3／0 |
| map_10 | 0／0 | 3／1 | 0／0 | 3／1 |

28条件すべてで、**全行動列本文・勝敗・ターン数が完全一致**。悪化分岐は0件のためS1を採用する。
残りの反復でも各条件の行動列は同一だったが、独立した成績標本とは扱わない。
28条件中15条件が上限判定なので、通常勝利と上限勝利を混同しない。

思考時間のr0加重平均はbaseline 681.8ms → S1 783.8ms（+15%）だった。ただし再現性確認の反復まで含めると
増減の方向が逆になり、baselineとS1の同時実行による負荷差を分離できない。高速化・性能退行は断定しない。

既存評価レポートの客観基準による全体判定は両版FAILだが、これはV4対V200の絶対基準であり、
変更前後の採否判定ではない。結果は `.tmp/stage2-s/baseline-50t/` と `.tmp/stage2-s/s1-50t/`、
28条件の集計・照合は `.tmp/stage2-s/agg/` に保存した。

## S2: 経路精錬・橋頭堡観測の重複解消（採用）

`strategy_pipeline::prepare_strategy` の精錬・観測を残し、`portfolio_for_squads` の同じ提案への再実行を削除した。
「1回」は再編の提案→採用の単位であり、生産単独分析やfallbackの準備まで省略する手番cacheは導入していない。
DAGの初期化・編成後の経路commitment更新は維持する。

既存のMilestoneテストへ、提案準備後に物件の所有を変えても採用時に入力を精錬し直さない契約を追加した。
変更前は次Milestone `(4,1)/(4,5)` へ置換されて失敗し、変更後は準備済みの `(2,1)/(2,5)` を維持して成功。

| 検証 | 結果 |
|---|---|
| `cargo test --release --locked -p engine --lib` | 880成功・既存ignore 1件・失敗0 |
| `cargo fmt --all -- --check` | 成功 |
| `cargo clippy --all-targets --all-features -- -D warnings` | 成功 |
| 50T・28条件の対戦成績 | 完了・異常0、S2を採用 |

### 対戦結果: S1 → S2（50T・28条件）

| 成績 | baseline / S1 | S2 |
|---|---:|---:|
| 総合（勝／負／分） | 20／8／0 | 21／7／0 |
| 通常決着（勝／負） | 7／6 | 8／6 |
| ターン上限判定（勝／負） | 13／2 | 13／1 |
| 先手14条件（勝／負） | 11／3 | 12／2 |
| 後手14条件（勝／負） | 9／5 | 9／5 |
| 総合勝率 | 71.4% | 75.0% |

全行動列・結果が一致したのは26/28条件。変化した2条件はいずれもmap_32先手で、
他のmap・先後の成績と終局指標は同一だった。

| 変更条件 | baseline / S1 | S2 | 最初の行動分岐 |
|---|---|---|---|
| map_32 先手 seed1 | 上限敗北（50T） | 上限勝利（50T） | T9の生産座標 `(2,4)` → `(3,3)` |
| map_32 先手 seed2 | 上限勝利（50T） | 通常勝利（50T） | T10の最初の行動（待機/攻撃 → 合流/待機） |

seed2は決着ラベルが通常勝利になったが、ターン数は両版とも50で、決着の早期化とは扱わない。
終局時のV4平均は拠点24.6→24.8、収入29,821→29,964、ZOC133.1→134.5、NPVは同値。
seed1は拠点12→16、収入15,000→19,000、ZOC49→90と改善。
一方seed2はZOC98→95、被損害価値約40.7万→49.6万と悪化した（与損害価値は約63.0万→68.5万）。
勝敗の悪化はなく、上限敗北を1条件減らしたため今回の縮小セットでは採用する。損害増加も記録し、
28条件だけで全map・seedの非劣後を証明したとはしない。

V4思考時間の加重平均はbaseline 681.8ms / S1 783.8ms / S2 616.9ms。
基準側の同時実行負荷が異なるため参考値であり、高速化は断定しない。
結果は `.tmp/stage2-s/s2-50t/`、集計は `.tmp/stage2-s/agg/s2_r0.py`・`s2_fix.py`・`s2_metrics.py`。

## S3: 汎用輸送を採用portfolioへ揃える（採用）

`formation::assign_generic_transports` へ元の分析案でなく採用portfolioを渡す。
島の順位・カーゴ選択・積載済み輸送の降車・capture-depth輸送は変えず、採用assignmentがない場合は
従来どおり未所有物件を目標にする。契約テストも採用案優先と物件fallbackを確認するものに更新した。

- `cargo test --release --locked -p engine --lib`: 880成功・既存ignore 1件・失敗0
- `cargo fmt --all -- --check` / `cargo clippy --all-targets --all-features -- -D warnings`: 成功
- 50T・28条件の対戦比較が完了・異常0、S3を採用

### 対戦結果: S2 → S3（50T・28条件）

| 成績 | baseline | S2 | S3 |
|---|---:|---:|---:|
| 総合（勝／負／分） | 20／8／0 | 21／7／0 | 21／7／0 |
| 通常決着（勝／負） | 7／6 | 8／6 | 8／6 |
| ターン上限判定（勝／負） | 13／2 | 13／1 | 13／1 |
| 先手14条件（勝／負） | 11／3 | 12／2 | 12／2 |
| 後手14条件（勝／負） | 9／5 | 9／5 | 9／5 |
| 総合勝率 | 71.4% | 75.0% | 75.0% |

S2とS3は**28/28条件で全行動列・結果・終局指標が一致**。S3追加による悪化分岐はないため採用する。
baselineとの26/28一致とmap_32先手2条件の変更はS2と同じで、seed2のZOC減少・被損害増加もS3では増えていない。
V4思考時間の加重平均はS2 616.9ms → S3 601.2msだが、実行負荷差を分離できないため参考値。
結果は `.tmp/stage2-s/s3-50t/`、比較は `.tmp/stage2-s/agg/s3_all.py`。

S4用のV3基準もS3固定バイナリで測定した。map_6/25 × seed1 × 両手番の4条件で、
通常決着0、上限1勝3敗（先手0勝2敗・後手1勝1敗）、異常0。
結果は `.tmp/stage2-s/s3-v3-50t/`。V4の28条件へV3の4条件は合算しない。

## S4: 旧首都防衛の削除（採用）

`uses_legacy_capital_defense`、その呼出し、`form_capital_defense_squads` をV2も含め完全削除した。
campaignのDefend割当とV4の本拠防衛は維持し、Attack/余剰吸収が使う敵クラスタ検出も残す。
追加契約は、首都近傍に敵がいるV2でも旧Defenseを新設せず、戦闘要員をAttack編成へ渡すことを確認する。
変更前はDefense生成で失敗、変更後は成功した。

- 固定バイナリのビルド成功
- V4の50T・28条件と、V3の50T・4条件をS3と比較完了・異常0、S4を採用
- 最初の全テストで旧V2仕様を固定する2件が失敗した。期待値の更新理由と再検証は後述
- S1〜S4の独立レビューは指摘なし（読み取りのみ。対戦成績の代わりにはしない）

### 対戦結果: S3 → S4（V4・50T・28条件）

| 成績 | baseline | S3 | S4 |
|---|---:|---:|---:|
| 総合（勝／負／分） | 20／8／0 | 21／7／0 | 21／7／0 |
| 通常決着（勝／負） | 7／6 | 8／6 | 8／6 |
| ターン上限判定（勝／負） | 13／2 | 13／1 | 13／1 |
| 先手14条件（勝／負） | 11／3 | 12／2 | 12／2 |
| 後手14条件（勝／負） | 9／5 | 9／5 | 9／5 |
| 総合勝率 | 71.4% | 75.0% | 75.0% |

S3との全行動列一致は26/28条件。勝敗は全条件で一致し、変化したのは次の2条件だけ。

| 変更条件 | S3 | S4 | 最初の行動分岐 |
|---|---|---|---|
| map_10 後手 seed2 | 上限勝利（50T） | 上限勝利（50T） | T46のP2行動。終局指標は同一 |
| map_25 先手 seed2 | 通常勝利（43T） | 通常勝利（42T） | T41のP1行動 |

map_25の終局時は拠点54・収入57,000が同一で、NPV19,995→14,580、ZOC250→216、
与損害価値約144.2万→136.0万、被損害価値約88.7万→85.4万になった。
**決着ターンが43Tと42Tで違うため同時点の指標比較ではない**。NPV・ZOCの低下は記録するが、
勝敗の悪化はなく1ターン早く勝利したため、この縮小セットではS4を採用する。
他26条件の指標は同一。28条件平均は拠点24.8・収入29,964が同一、NPV8,950.5→8,757.1、ZOC134.5→133.3。
baselineとの行動列一致は24/28条件（S2のmap_32先手2条件と、今回の2条件が変化）。

V4思考時間の加重平均はS3 601.2ms → S4 670.0ms。実行負荷差を分離できない参考値であり、性能効果は断定しない。
結果は `.tmp/stage2-s/s4-50t/`、比較は `.tmp/stage2-s/agg/s4_all.py`。

### V3回帰: S3 → S4（50T・4条件）

map_6/25 × seed1 × 両手番の4条件で、全行動列・勝敗・終局指標が4/4一致。
両版とも通常決着0、上限1勝3敗（先手0勝2敗・後手1勝1敗）、異常0。
結果は `.tmp/stage2-s/s3-v3-50t/` と `.tmp/stage2-s/s4-v3-50t/`。

### 旧V2契約テストの更新と最終検証

S4の最初のworkspace検証では、次の2件が旧仕様との不一致で失敗した。

- `scenario_tests::test_scenario_1_capital_defense`: V2へ首都 `(5,5)` での旧Defense待機を要求していた。
  「旧Defenseを作らず汎用Attackで近傍の敵へ攻撃する」に期待を更新し、単体で成功を確認した。
- `baseline_v1_v2_multi_turn`: 固定行動列の最初の差はT3 P2（V2）の旧Defense待機→Attack。
  まず実際の行動列を別ファイルへ保存して全差分を確認し、意図したS4の挙動変更と判断した後、
  `v1_vs_v2_map1.txt` だけを再生成した。テストは削除・skipせず、他のgoldenは更新しない。

ユーザー合意は「旧首都防衛はV2も含め完全削除、V2の対戦評価は省く」であり、
テストを通すために旧防衛を復活させない。productionロジックは評価済みS4から変更していない。
期待値更新後の再検証はすべて成功した。最初の失敗ログも残し、期待値を更新してから成功したことを区別する。

| 最終検証 | 結果 |
|---|---|
| `cargo test --release --locked --no-fail-fast`（workspace） | 923成功・既存ignore 2件・失敗0（engine lib 881、CLI 16、engine結合 20、MCP 6） |
| `cargo test --locked -p engine --lib`（debug） | 881成功・既存ignore 1件・失敗0 |
| `cargo fmt --all -- --check` | 成功 |
| `cargo clippy --all-targets --all-features -- -D warnings` | 成功 |
| `cargo check --locked -p engine --target wasm32-unknown-unknown` | 成功 |
| 独立レビュー（旧V2契約の更新確認を含む） | 指摘なし |

V2の期待値1件を更新した以外の7件の固定行動列テストも成功。ログは `.tmp/stage2-s/s4-final-recheck-*`。
変更したのはテストの期待値だけで、対戦評価済みproductionロジックを後から変えてはいない。

### 構造・複雑性（採用したS1〜S4）

`scripts/measure_ai_complexity.py` / `measure_ai_structure.py`、テスト除外。
基準 `d925709` のAIソースと候補を同じ計測器で比較した。

| 指標 | 基準 | 候補 |
|---|---:|---:|
| AI全体の直接CC合計 | 11303 | 11301 |
| AI全体の直接認知的複雑性合計 | 9600 | 9593 |
| AI全体の直接CC最大 / p95 | 108 / 8 | 108 / 8 |
| 拡張Roadmap照合の最大段の直接CC / 認知 | 43 / 88 | 41 / 83（実績照合段） |
| 拡張Roadmap選択入口の行数 / 直接CC | 602 / 43 | 43 / 2 |
| 再編入口の直接CC | 5 | 4 |
| 禁止方向参照 | 0 | 0 |
| 経路判定 / 通常・拡張の同名ペア / 修復呼出 | 42 / 29 / 16 | 43 / 29 / 16 |

選択入口が小さくなったのは、実績照合（508行）と公開（84行）を分離したためで、
実績照合自体が大幅に単純化したとは主張しない。主な効果は採用と観測の責務分離および旧防衛段の削除。
追加の経路判定1件は観測専用入口にも従来の拡張有効化境界を維持した分。
呼出しの深さは計測器が対応していないため未計測。結果JSONは `.tmp/stage2-s/complexity-*.json` と `structure-*.json`。

## S4件の採否まとめ（V4・50T・各28条件）

| 版 | 通常勝／負 | 上限勝／負 | 総合勝／負 | 勝率 | 採否 |
|---|---:|---:|---:|---:|---|
| baseline (`d925709`) | 7／6 | 13／2 | 20／8 | 71.4% | 基準 |
| S1: 編成後Roadmapの観測化 | 7／6 | 13／2 | 20／8 | 71.4% | 採用（28条件の全行動列一致） |
| S2: 精錬・観測の重複解消 | 8／6 | 13／1 | 21／7 | 75.0% | 採用（map_32先手2条件が改善） |
| S3: 汎用輸送の採用案統一 | 8／6 | 13／1 | 21／7 | 75.0% | 採用（S2と28条件一致） |
| S4: 旧首都防衛の削除 | 8／6 | 13／1 | 21／7 | 75.0% | 採用（勝敗維持、map_25で1T早く勝利） |

各版は直前採用版と初期baselineへ比較した。同seedの反復を分母へ加えず、V3回帰4条件もこの表へ合算しない。
追加seedの対戦は行っていない。28条件の結果で全マップ・seedの非劣後は証明できず、
S2の被損害増加とS4の決着時点の指標差も上記に残した。占有モデルの統一は未実施。
