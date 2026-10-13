# AI再設計 v3: 目標構造と最初の段階の提案

`refactor/ai-redesign-v2` を土台に、共通基盤と小規模V4拡張の分離、依存方向、状態の正本を
整理するための調査結果と提案。**この文書の段階では実装していない。**最初の段階の範囲は末尾で提案し、
承認後に着手する。

## 0. 基準

| 項目 | 値 |
|---|---|
| 基準コミット | `15c774c4ae97c5ed386519d570a59c2af9fb4b2a`（`refactor/ai-redesign-v2`）。作業差分なし |
| 基準バイナリ | `mcp-server` release（edd1896のコード＝15c774cと同一。15c774cはdocsのみ）sha256 `d3e689ff03b120de8dd6207d45227dc53f566462c0793d8629645580e199a1e4` |
| 判断fingerprint用 | `engine/tests/ai_determinism.rs` release バイナリ sha256 `ee8da1dbea563ddf210d3f32fc82b4a2daf343d6763a9b6eab4b0ff434010178` |
| 真のbase | fe49bfe＋ahash鍵固定のみ。sha256 `22caf9f4a0e6ce41c06be07e921789b4291cf9afe03127ea79bedb9ecaa46384` |
| ツール | rustc 1.97.0 / Python 3.13 / x86_64 Linux |
| 対象外の枝 | `refactor/v4-small-strategy`（=fe49bfe、全面巻き戻しはしない）、`refactor/v4-production-stage3-wip`（5b351bf、一括取り込みはしない。§D で個別評価） |

### 決定性の確認（基準バイナリ）

- 同一プロセス内: fingerprintハーネスで map_1/2/6/25/32 × seed 1,1,2,2 × 両手番 × 30T を1プロセスで連続実行。
  20条件すべてで2回の行動列ハッシュと終局状態が一致。
- 別プロセス間: `run_matchup_sweep.py --repeats 3`（1局1プロセス）で同じ5マップ × seed 1,2 × 両手番 × 30T。
  20条件 × 3プロセスすべてで全行動履歴のハッシュが一致。

以後の比較では、同一seedの反復は再現性の確認だけに使い、勝率の独立標本には数えない。

## A. engine と runtime／tactics の双方向参照

### 現状の依存（15c774c）

```
engine.rs (4873行) ──execute_ai_turn_v2──▶ runtime::turn
engine.rs ──decide_ai_action_v2──▶ tactics::v2
engine/extended.rs ──decide_ai_action_v2_for_entities──▶ tactics::small_map
runtime::turn ──▶ engine: AiCommand, execute_ai_command, decide_ai_action_v2,
                 decide_squad_action_v4, decide_forming_campaign_site_relief,
                 transport_has_other_actionable_cargo, AiActionCooldown,
                 AiProductionCooldown, AiProductionCommandQueue,
                 take_next_queued_production_command, issue_production_command,
                 AiTurnStrategyCache
tactics::* ──▶ engine: AiCommand, ActionPriority, AMBUSH_*(4), AiTacticalSnapshot,
                 strategic_target_value, CampaignActionContext, campaign_action_context,
                 campaign_mission_allows_capture, campaign_position_is_on_target_island,
                 air_move_preserves_return_fuel, is_unit_stranded,
                 has_capturable_property_at, coordinated_support_destination,
                 AiTurnStrategyCache
```

runtime/tactics が使う engine の項目は27個で、engine はその runtime/tactics を呼ぶ。
engine.rs は「V1の行動選択」「命令の適用」「手番入口」「手番内の一時状態」「戦術の共通部品」
「V4小規模のSquad実行器（engine/extended.rs）」を同居させている。

engine の外（他のAI module・cli・wasm・mcp-server・tests）が使う engine の項目は7個だけ:
`AiCommand`、`execute_ai_command`、`execute_ai_turn`、`AiActionCooldown`、`AiProductionCooldown`、
`AiTurnStrategyCache`、`clear_ai_cooldowns_system`。

`AiTurnStrategyCache` は4つの別の持ち主の状態を1つのResourceに入れている:
採用した campaign portfolio（戦略判断）、生産の一括計画queueと旗（生産）、
`squads_planned`（手番進行）、行動選択の距離cache（戦術）。

### 目標の依存方向

```
            ┌──────────── runtime (手番段取り・命令発行・結果反映) ────────────┐
            │                         │                                         │
            ▼                         ▼                                         ▼
     tactics (行動選択)        production (生産)                       planning (戦略採用・編成)
            │                         │                                         │
            └──────────────┬──────────┴─────────────────────────────────────────┘
                           ▼
              context (読み取り専用の盤面snapshot・評価用の事実)
                           │
                           ▼
              command (AiCommand: 判断の出力型。components/events だけに依存)
```

- 上から下への一方向だけを許す。tactics/context/command は runtime と engine を参照しない。
- `engine.rs` は外部APIの入口（`execute_ai_turn` 等）とV1だけにする。V1は `ai/v1/` へ分け、
  共通の合法候補・命令適用は使うが、Squadへ偽装しない。

| 項目 | 現在 | 移動先 | 理由 |
|---|---|---|---|
| `AiCommand` | engine.rs | `ai/command.rs`（pub） | 判断の出力型。V1・V100・tactics・squad・missions が共有。どの判断層にも属さない葉 |
| `execute_ai_command` | engine.rs | `runtime/issue.rs` | 命令の適用（イベント発行）は runtime の責務 |
| `AiTacticalSnapshot`、`strategic_target_value`、`has_capturable_property_at`、`is_unit_stranded` | engine.rs | `context/tactical.rs` | 盤面から読む事実・価値。戦術以外（生産のシミュレーション等）も使える |
| `ActionPriority`、`AMBUSH_*` | engine.rs | `tactics/scoring.rs` | 戦術の比較軸。外で使われていない |
| `CampaignActionContext` と campaign 3関数 | engine.rs | `tactics/campaign.rs` | campaign所属unitの戦術制約。台帳は読むだけ |
| `air_move_preserves_return_fuel` | engine.rs | `tactics/candidates.rs` | 合法候補の絞込み |
| `decide_squad_action_v4`、`decide_forming_campaign_site_relief`、`coordinated_support_destination`、`shares_attack_operation`、`V4SquadExecutionCursor` | engine/extended.rs・engine.rs | `tactics/small_map/squad_executor.rs` | 小規模V4固有のSquad単位の実行順 |
| 生産queue・cooldown（`AiProductionCommandQueue` 等） | engine.rs | `runtime/production_queue.rs` | 発行待ち命令は runtime の状態 |
| `AiTurnStrategyCache` | engine.rs | 分割: 採用portfolio→`planning`（Bで `TurnDecision`）、生産旗→`production`、`squads_planned`→`runtime`、距離cache→`tactics` | 持ち主ごとに更新入口を1つにする |
| `execute_ai_turn_v1`・`decide_ai_action` | engine.rs | `ai/v1/` | 版固有の選択 |

**完了条件**（再exportで見かけだけ整えたものは不合格）:

1. `runtime`・`tactics`・`context`・`command` から `crate::ai::engine` への参照が0。
2. `tactics`・`context` から `runtime` への参照が0（`scripts/measure_ai_structure.py` に逆依存数を追加して検査）。
3. 外部API（上の7項目）の公開経路を `ai/mod.rs` に明示する。互換のための `pub use` は外部APIだけで、
   内部moduleは移動先を直接参照する。
4. 各項目の可視性は使う範囲の最小（`pub(crate)` か module 内）。
5. `AiTurnStrategyCache` を廃止し、持ち主ごとの型に置き換える（4番目の持ち主ごとに1コミット）。

すべてRで、判断fingerprint（V1〜V4）・対戦の全行動履歴が一致することを確認する。

## B. Squad編成と戦略判断

（調査: `plan_squads_with_hooks` 1190行・関数スコープの束縛約46個・直接CC148、
`analyze_strategy_internal` 685行・約34個）

### 現状の判断の流れ（`plan_squads_with_hooks`）

V2〜V4の手番の最初のstepで1回（V3/V4。V2は行動前のstepごと）。`SquadManager` をWorldから外して
最後まで手で持ち回し、途中で台帳Resourceを書いては後段で読み直す。

| # | 段 | 主に読む | 書く |
|---|---|---|---|
| 1 | 前手番の掃除 `update_squads` | unit状態 | SquadManager |
| 2 | 再計画の準備（Reserve解放・SoloFallback解除・所属正規化） | SquadManager, UnitOperationRegistry, campaign実行台帳 | SquadManager, UnitOperationRegistry |
| 3 | 戦略分析（mgrを一時的にWorldへ戻して分析し、また外す） | 盤面, **2で書いた** UnitOperationRegistry | V4LogisticsPlanRegistry, 診断 |
| 4 | 採用（小規模V4: DAG経路の精錬・観測・Roadmapでの部分集合選択） | 3のportfolio, 編成前のSquad | RouteBreakthrough, Roadmap各台帳, campaign実行台帳。直後に**自分で書いた**RoadmapTurnPlanを読み戻す |
| 5–9 | 採用portfolioのcache・所有権のclaim・一時停止・安全占領・campaign編成・配備Squad | 採用案, 配備台帳, Roadmap | AiTurnStrategyCache, UnitOperationRegistry, SquadManager, V4DeploymentRegistry |
| 10–18 | 汎用編成（島目標・汎用輸送・積載輸送・占領深度の輸送・旧首都防衛・占領・攻撃・余剰吸収） | 盤面（**戦略分析とは別の占有モデル**）, **元の**portfolio（輸送目標）, 採用portfolio | SquadManager |
| 19 | V4所属修復（正規化→最大3周の再接続→Reserve） | SquadManager, UnitOperationRegistry | 両方 |
| 20 | 仕上げ（通常V4: Roadmap観測を最終正規化の**前**、小規模V4: 経路commitmentと2回目のRoadmap選択を**後**） | SquadManager | Roadmap各台帳, 経路台帳 |

`SquadPlanningHooks` の8つのhookは、通常/小規模の差（採用、局地割当、占領順・歩兵選択、旧首都防衛、
Roadmap観測の位置）を入れる細粒度の前後hookになっている。

確認した問題:

- 戦略の採用（4）が、編成の後（20）にもう一度行われる（小規模V4）。通常は同じ結果だが、採用案の中に
  準備済みoffensiveが無い場合は「Hold」が勝ちうる。Squad・cacheは1回目、Roadmap台帳は2回目の結果を持ち、
  次手番の所有権claimが2回目を読む。
- 経路の精錬・観測が分析内と採用で2回走る（間で `RouteBreakthroughRegistry` を書く）。
- 汎用輸送は元のportfolio、campaign編成は採用portfolioを使う（意図的。契約テストあり）。
- 戦略分析の11〜16段（生産需要・平和物件など）はSquad編成では使われず捨てられる。
  V3は同じ手番に分析を2回行う。
- 使われない束縛が2つ（`my_infantry_islands`、`tactical_reserved_entities` は常に空）。

### 目標の流れ

```
context::PlayerBoard::observe(world, player)            … 読み取り専用。2つの占有モデルを明示的に別メソッドで持つ
   │
planning::situation::assess(&board) -> StrategicSituation      … phase・未所有/敵物件・首都脅威・収入要否
planning::campaign::propose(&board, &squads, &ownership, reserved) -> CampaignProposal
                                                        … portfolio・兵站の差分・診断（Worldは書かない）
planning::selection::adopt(&proposal, &roadmap_view, &squads, profile) -> TurnDecision
                                                        … 通常: そのまま / 小規模V4: 精錬→観測→Roadmap選択
                                                          採用戦略の唯一の正本。cacheは TurnDecision を保持するだけ
planning::assignment::propose(&board, &situation, &decision, &free_pool, policy) -> AssignmentProposal
                                                        … campaign編成・配備・汎用編成を「Squadの作成/追加」の提案として返す
planning::assignment::commit(world, proposal) -> AssignmentDelta
                                                        … 競合解消と検証の後、所属を1つの更新口で確定（C）
runtime::observe::formation(&squads, &decision)         … 編成結果の観測（Roadmap実績）。戦略を選び直さない
```

- 通常/小規模の差は、`adopt` の方策と、汎用編成の `GenericPolicy`（enum: Standard/SmallMap。
  占領順・歩兵選択・旧首都防衛の有無）に集める。8個の前後hookは廃止する。
- 文脈型は必要な事実だけを持つ（例: 汎用編成は `&PlayerBoard, &StrategicSituation, source/adopted portfolio,
  blocked/paused islands, clusters, own capital, &mut FreePool`）。

区分:

| 変更 | 区分 |
|---|---|
| 盤面snapshotの共有（占有モデル2種を明示して維持）、分析の段を `situation`/`campaign`/生産需要に分割、Squad編成では生産需要を計算しない、使われない束縛の削除、地形連結cacheの統合 | R |
| `propose`/`commit` の分離（同じ順序でdeltaを適用。SquadIdは発番順に依存するので順序を保つ） | R |
| 編成後のRoadmap 2回目の選択を観測だけにする、精錬・観測を1回にする | S（Roadmap台帳の内容、Holdの採否が変わりうる） |
| 汎用輸送を採用portfolioへ、占有モデルの統一、旧首都防衛の削除 | S |

## C. 所属・計画・配備の正本

調査方法: コード読解に加え、作業ツリー外の複製へ計測用の出力だけを足し、map_1/2/6/25 × seed1 × 両手番 × 30T を
実行した（fingerprintは計測なしと一致）。

### 台帳ごとの正本と派生

| 台帳 | 正本か派生か | 現在の書込み | 問題 |
|---|---|---|---|
| `SquadManager`（Squad.members ほか） | 事実上の書込み元。作戦ownerは `target_island` から派生 | 約25関数（編成・輸送・campaign・配備・修復） | 追記だけの書込み（campaign輸送）と、同じ島のSquadを丸ごと消す書込み（`assign_campaign_responsibilities`）が混在 |
| `UnitOperationRegistry` | 文書上の「作戦割当の正本」。実際は毎回 SquadManager から作り直し、行動選択はこちらを読む | reconcile・claim・輸送波・撃破時 | 勝者が決まらない移管（campaign→別campaign/戦術）で、全Squadから外れたのに古い `squad_id` 付きのCampaign所属が残る |
| `PlanContractRegistry.contracts` | 正本（物件契約のidと版） | 生産計画の維持/再計画 | ― |
| `PlanContractRegistry` の entity台帳（`entity_assignments`/`entity_bindings`） | **書くだけで読まれない複製**（`AlreadyAssigned` の警告出力だけ）。解放もされない | 生産unitの束縛 | 削除できる |
| `VictoryRoadmapRegistry` の割当 | 派生（監査）。ただし `active_capital_entities` が所有権claimの保護に、首都目標が輸送波の目標に戻る | 編成の後（通常は最終正規化の前、小規模は後に2回） | 監査の投影が次の判断の入力になっている |
| `V4DeploymentRegistry` | 生産意図の正本。`squad_id`/`active` は派生 | 生産確定・生産イベント・配備Squad準備 | ― |
| `V4CampaignExecutionRegistry` | 生産命令↔Entityの状態の正本 | 生産確定・発行・生産イベント・撃破 | 配備台帳と**同じ `UnitProducedEvent` を別々にSquadへ解決**し、1体が2つのSquadに入りうる |
| 首都ルートDAGの経路・Node台帳 | 派生。**不安定な SquadId** をキーにし、`commitment_members` がid変更を跨いで指令を引き継ぐ修復cache | 編成の後 | SquadIdの作り直しで指令が切れる |
| campaign portfolio | 派生（毎回 SquadManager・所属・生産記録から作る）。それが所属を書く入力になる | 戦略分析 | ― |
| `AiTurnStrategyCache` のportfolio | **編成前**のsnapshot。手番の残りの間は古い | 編成の途中 | 生産・戦略・手番末の再接続が古い一覧を読む |

ID: `SquadId` の発番は1か所（＋輸送波での手動発番）だが、複製先7台帳は作り直しのたびに古くなる。
`StrategicOperationId` は Roadmap と PlanContract が**別々に1から発番**し、値が衝突する（現状、両者を比べる箇所はない）。
生産slotのid型は配備用と campaign用の2種類がある。

### 修復処理が補っているもの（実測）

| 処理 | 実行時期 | 補っている不整合 | その不整合を生む上流 |
|---|---|---|---|
| `reconcile_unique_operation_assignments` | 編成の入口・固定点の各周に2回・最終・手番末 | 1体が複数Squadに入る、死亡、輸送役が members に居ない、所属のずれ | campaign輸送の追記、生産イベントの二重解決、積荷の引渡しでの新Squad、`update_squads` が非活性の島で `target_island` を消す |
| `reconnect_current_v4_operations` | 固定点の各周の1手目 | 所属はCampaignなのにcache上の割当一覧に居ないunit | 割当一覧（cache）が古い。しかも自分が全campaignを組み直すため、配備Squadを消して別idで作り直し、**自分で新しい孤立unitを作る**（map_1の1周目で63体） |
| `reconnect_unassigned_transport_waves` | 固定点の各周の3手目 | どのSquadにも居ない占領兵・輸送役 | 上の孤立、Return完了で空いた輸送役、通常経路の生産 |
| `reconnect_v4_operation_fixed_point`（最大3周） | 編成の後・手番末 | 1周目の組み直しで生まれた孤立 | ― |
| `assign_explicit_reserve_squads` | 固定点の後 | 残った未所属を明示Reserveにする | ― |
| `reconcile_v4_end_turn_reserves` | V4で通常行動が尽きるたび | 手番途中の撃破・搭乗変更・生産で所属を失ったunit | ― |

固定点の周回数（実測）:

- map_1（小規模）: 52回中23回が2周目に入り、3周目は0回。2周目は未所属を減らしたことがなく、
  1周目の組み直しが作った孤立のためだけに回っていた。23回中21回は**同じSquadを新しいidで作り直しただけ**
  （延べ Attack171・Capture21・Defense18・Transport2）、2回だけ実際に所属が変わった
  （輸送波でCampaignへ付けたunitを、2周目の組み直しが局地Captureへ移す）。
- map_2: 66回中3回が2周目、そのうち2回で所属が変わった。
- map_6・map_25（通常）: 各329回、すべて1周で収束。勝者なしの移管も0件。
- 手番末の再接続: 未所属になったunitはすべて行動済みで、**同じ手番の行動を可能にした例はない**。
  効果は次手番の入力だけ。

結論: 固定点が本当に補っているのは「割当一覧（cache）が古いこと」と「campaignの組み直しが配備Squadを消して
作り直すこと」の2つ。周回を減らすだけでは原因が残る（計画書の「反復上限を下げただけは不合格」と一致）。

### 目標

- 所属の正本は1つ（`planning/assignment.rs` の `OperationLedger`。現 `UnitOperationRegistry` を発展させる）。
  更新は `propose → resolve（競合解消・検証）→ commit(AssignmentDelta)` の1入口だけ。
  `Squad.members` は ledger の投影にし、members から所属を逆算しない。
- campaignの組み直しは「同じ島のSquadを消して作る」のではなく、差分（追加・移動・解放）を提案する。
- 生産イベントの解決は1か所（配備と campaign の生産slotを1つの `on_produced` に）。
- 割当一覧のcacheは、ledgerのcommit**後**に作る（古い一覧を読ませない）。
- DAG台帳は安定した作戦idで引き、`commitment_members` の引継ぎcacheを廃止する。
- 戦略作戦id・物件契約id・PlanId/Step・SquadId を別の型にする。

区分:

| 変更 | 区分 | 根拠 |
|---|---|---|
| PlanContract の書くだけのentity台帳の削除 | R | 読み手がいない |
| PlanContract の作戦idを別の型に（値は変えない） | R | 比較箇所がない |
| 撃破時の解放経路を1本に（`retain_live_entities` とイベント側の重複） | R（fingerprintで確認） | 同じ解放を2回している |
| 生産イベントの解決を1か所に | Rの可能性。現在の勝者規則を厳密に再現できた場合だけ | 正規化が決める勝者と同じ結果が必要 |
| 固定点の2周目以降をなくす／配備Squadの作り直しをやめる | **S** | SquadIdの作り直しは `squad.id` の同点処理・Squad順・DAG指令に効く。map_1で2/23回は所属も変わる |
| 輸送波・明示Reserve・手番末再接続の廃止や変更 | **S** | 次手番の分析・正規化の入力が変わる |
| 割当一覧cacheをcommit後に作る | **S** | 生産・戦略が読む一覧が変わる |
| 正規化の勝者規則、claimの解放・保護、`update_squads` の `target_island` 消去 | **S**（方策そのもの） | ― |


## D. 生産の契約（stage3-wip からの取り込み評価）

v2 には段階3で `DemandLedger`（要求を0にせず理由付きで閉じる）・`AllocationState`・`commit_turn_production`
がある。wip（5b351bf）は同じ段階を別に実装し、`ProductionPlanDraft`・保存前の施設確定・
確定後の再予測を加えたが、map_1 seed2 後攻で第11ターンに行動が分岐し、CPU p95 が約45%悪化して未受入。

| 考え方 | wip の実装 | v2 の現状 | v2 に残る穴 | 取り込む場合の区分 |
|---|---|---|---|---|
| 元の要求を保持 | `AllocationState` が元の `demands` と合同パッケージの上書きを別に持つ | `DemandLedger` の close/reopen、`effective_slots` | 小規模経路が合同パッケージの数で `slots.capture_units` を上書きし（extended.rs:1308付近）、traceも上書き後の値 | R（上書きをledgerのeffective値にし、requestedを残す） |
| 予約失敗で予算・施設を消費しない | `reserve() -> Result`。失敗時は何も変えない | `claim` は失敗しない（`saturating_sub`・施設ownerを上書き）。呼び出し側の事前filterで守っているだけ | 使用済み施設への claim で資金が減る（scratchで再現）。発行時に取り消された命令の配備・占領意図・計画工程が残る | `try_claim -> Result`: R。取消時の解放: 観測した対局ではR、潜在的にS |
| 保存前に購入工程・施設を確定 | Draft上で施設を確定し、変更した計画は再予測してから保存。失敗時は全体を戻す | 小規模経路が**保存後**に施設を交換し（`optimize_production_facility_assignments` → `retarget_planned_step`）、工程を書き換える | 保存済みの予測・配備forecast・traceの施設・占領意図の施設が交換前のまま。継続計画も交換される | 交換規則を変えずに「命令・占領意図・trace・計画工程を1か所で同時に確定」: R。継続計画の保護・交換条件の追加・再予測: S |

wip の性能悪化の原因（コード読解）: 施設確定後の再予測（変更した計画ごとに戦闘計画入力を作り直し、
`evaluate_literal_plan` で再シミュレーション）、配備の再投影で到達cacheを捨てること、毎手番の
台帳全体のclone（Draft）。v2 は既に台帳をWorldから取り出して1回だけ戻しており、Draftのcloneは
分離の効果を足さない。

**取り込むもの（価値/危険度順）**:

1. R: `AllocationState::try_claim -> Result`（FacilityConflict/BudgetUnavailable、失敗時は無変更）。
   契約テスト: wip の `unfilled_demand_survives_and_failed_reservation_is_atomic` を移植（v2 では現状失敗）。
2. R: 発行直前の検証に手番・行動中player・Mainフェーズ・終局を加え、campaign の発行経路にも適用。
   契約テスト: `stale_turn_budget_facility_loss_and_wrong_player_are_rejected`、
   `issue_validation_does_not_spend_or_spawn_and_rechecks_after_application` を移植。
3. R: 施設確定を1つの段にまとめ、命令・占領意図・trace・計画工程・戦闘計画traceを同時に更新（再予測なし）。
   契約テスト: trace・発行命令・`UnitProducedEvent` の施設の完全一致、PlanStep命令の工程施設＝命令施設。
   wip の `snapshot_contract_tests.rs`（3件、v2でそのまま成功）も追加。
4. R: requested/effective 要求の分離。契約テスト: `rejected_joint_package_keeps_requested_demand_and_records_effective_demand` をledger向けに移植。
5. R（観測した対局）/潜在S: 発行拒否時の工程取消と配備・占領意図の解放。
6. S（任意、A/B評価が先）: 継続計画を施設交換から保護（再予測なし）。

**取り込まないもの**: `ProductionPlanDraft`（v2の取り出し済み台帳と重複し毎手番clone）、施設確定後の再予測と配備再投影
（主な性能コストでS）、`can_join_operation` による交換条件の追加（S）。

## S（戦略変更）の扱い

後攻小規模マップの弱点は、Rの段階と切り離して1仮説ずつ扱う。前回の4仮説（H1〜H4）は不採用。
次の調査の起点として、ログで確認済みの事実を記録する（まだ仮説として採否を判断していない）:

- 小規模4マップ・後攻24局の第1〜12ターンの生産（base）: 歩兵779、偵察車225、重歩兵102、
  間接（ロケット80・軽自走砲24・重自走砲5・その他4）、直接の戦車系（戦車38・中戦車4・他2）。
  V200は歩兵1108・戦車85・偵察車80・戦闘ヘリ51。**間接攻撃unitに対し、それを守る前衛の直接戦闘unitが
  少ない**可能性がある。編成ごとに、間接unitの射程内に前衛が居たか（前衛なしで射撃位置に出た回数）を
  ログで確認してから仮説にする。
- map_1 後攻の敗局では Capture作戦が毎手番5体の占領要員を要求し、既存の歩兵8〜16体のうち
  campaign割当は0〜4体だけ。歩兵4体を毎手番作り資金1万を余らせる（H3で一律に数えると他の盤面で悪化）。
- map_32 の敗局は物件数が同じまま、T6〜T10の交戦で戦力差がつく。

## 検証計画と所要時間（実測ベースの見積り）

| 段 | 内容 | 所要（1バイナリ、4並列） |
|---|---|---|
| R screen | 判断fingerprint（V1〜V4各18条件）＋ map_1/2/6/25/32 × seed1,2 × 両手番30Tの全行動履歴比較 | 約15分 |
| 主比較 | map_1/2/6/25/32 × seed1〜6 × 両手番 × 60T（60局） | 約35分（map_6 が約6割） |
| 最終の非劣後 | 上記5マップ＋回帰（map_42・map_10）× seed1〜12 × 両手番 × 3反復（504局） | 約4.5時間。base と候補で約9時間 |

最終評価の3反復は、決定性を確認済みのため再現性の確認にしか使わない（勝率の標本は1反復分の168局）。
基準側の結果は固定バイナリで1回取れば再利用できる。Rの段では最終評価の代わりに「全局の全行動履歴が
基準と一致」を受入条件にし、Sの採否でだけ最終評価を回すことを提案する。

## 最初の段階の提案

### 段階の順序（全体）

| 段階 | 内容 | 区分 | 依存 |
|---|---|---|---|
| **1（最初）** | A: 依存方向の確立と `AiTurnStrategyCache` の分割／D-1・D-2: 失敗しない予約と発行検証の強化／C: PlanContractの書くだけの台帳削除と作戦id型の分離 | すべてR | ― |
| 2 | B: 盤面snapshot（`context`）・状況判断・campaign提案・採用（`TurnDecision`）・編成提案の分離。8個のhookを2つの方策enumへ | R（Roadmap 2回目の選択などは別のS） | 1 |
| 3 | C: `OperationLedger` の1入口化、生産イベントの解決の一本化、D-3（施設確定の1段化）・D-4（requested/effective） | R部分を先に、固定点・cache・Squad作り直しはSとして1件ずつ | 2 |
| 4 | 後攻小規模の戦略改善（ログで診断した仮説を1件ずつ） | S | 診断は並行可、採用判断は3の後の基準で |

1を最初にする理由: BとCが必要とする置き場所（`command`・`context`・`runtime` の状態・`planning` の判断）と
一方向の依存を先に作る。どれも判断を変えないので全行動履歴の一致で受け入れられ、Sの評価と混ざらない。
D-1・D-2は、以後の生産の変更で失敗を隠さないための安全網になる。

### 段階1の作業順（各項目を1〜数コミットに分け、各コミットでRの検証を行う）

1. `ai/command.rs` を作り `AiCommand` を移す。`execute_ai_command` を `runtime/issue.rs` へ。
2. `context/tactical.rs` へ `AiTacticalSnapshot`・`strategic_target_value`・`has_capturable_property_at`・`is_unit_stranded`。
3. tactics側: `ActionPriority`・`AMBUSH_*` を `tactics/scoring.rs`、campaign制約を `tactics/campaign.rs`、
   `air_move_preserves_return_fuel` を `tactics/candidates.rs`、小規模V4のSquad実行器を `tactics/small_map/squad_executor.rs`。
4. runtime側: 生産queue・cooldownを `runtime/production_queue.rs`。
5. `AiTurnStrategyCache` を持ち主ごとに分割（距離cache→tactics、`squads_planned`→runtime、生産旗→production、
   採用portfolio→planning。採用portfolioはBの `TurnDecision` の前身として型だけ分ける）。
6. V1（`decide_ai_action`・`execute_ai_turn_v1`）を `ai/v1/` へ。`engine.rs` は外部APIの入口だけにする。
7. `scripts/measure_ai_structure.py` に逆依存（tactics/context/command → runtime/engine）の計数を追加し、0を確認。
8. D-1: `AllocationState::try_claim -> Result` と移植テスト（v2で現状失敗するものが成功に変わることを確認）。
9. D-2: `validate_issue` に手番・行動中player・Mainフェーズ・終局の検査を加え、campaignの発行経路にも適用。移植テスト2件。
10. C: PlanContract の entity台帳の削除、物件契約の作戦idを専用型に。

### 段階1の受入条件

- 判断: 各コミットで判断fingerprint（V1〜V4、各18条件）が基準と完全一致。段階末に主比較
  （map_1/2/6/25/32 × seed1〜6 × 両手番 × 60T、60局）で全局の全行動履歴が基準と一致。差が出たら最初の分岐と原因を
  特定し、挙動を戻すかSとして切り出す（goldenは更新しない）。
- 構造: 上の完了条件1〜5。変更前後の依存図・型と状態の持ち主・更新入口を文書で示す。
- 性能: 同じビルド・条件（単一スレッド＋既定並列）で map_6 seed1 後攻60T の平均・p95・最悪step・生産判断時間が
  基準からA/Aノイズを超えて悪化しない。
- 複雑度: 同じ計測器・定義・テスト除外で前後比較（変更対象の関数・多引数・重複・依存方向も報告）。
- テスト: `cargo test`、`cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings`、
  `cargo check -p engine --target wasm32-unknown-unknown`（基準で成功を確認済み）。

### 段階1の所要（見積り）

実装と各コミットのfingerprint確認で数時間、段階末の主比較（基準は固定バイナリの結果を再利用）で約35分＋性能計測約10分。
最終の非劣後評価（504局、約4.5時間/バイナリ）はSを採否判断するときに行い、Rだけの段階1では行わない提案とする。

