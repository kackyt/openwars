---
title: V4 生産AI 修正計画（map_3 渡洋作戦の成立）
version: 1.1.0
status: draft
owner: kackyt
created: 2026-08-07
updated: 2026-08-08
changeImpact: HIGH
---

# V4 生産AI 修正計画（map_3 渡洋作戦の成立）

## 1. Context

V4 は「ハードコードされた理想割合をやめ、戦況に応じて生産を決める」ために作られた。しかし map_3（8島の群島マップ）で `logs/v4_まとめ.md` の4点が実測されている。

- 歩兵を作っても海を渡らない（V3 は #54 で解決済み → **デグレ**）
- 敵を叩くユニットが生産されない／されても渡洋しない
- 輸送船を作っても輸送ミッションが与えられない
- **毎ターン全く同じ生産計画がループする**（先攻 AntiAir×20、後攻 Lander×7）

コードを追った結果、これらは別々の不具合ではなく **2つの構造欠陥** に集約される（副次要因を含め RC-1〜RC-4）。上限値の追加では直らない（上限で塞がない／敵の増援分で頭打ちにしない）。本計画は要求額を絞らず、**同じ予算の使い道（編成）を戦況に追従させる**方向で直す。

> **前版からの訂正**: 旧 RC-2 / Stage 2 は `planner.rs` / `missions.rs` の輸送ミッションを対象にしていた。しかしこの2ファイルは [execute_ai_turn_v1](engine/src/ai/engine.rs#L981) からのみ呼ばれる **V1 専用**であり、V4 は1行も通らない。V3/V4 が共有する戦術層は `squad.rs` + `island_campaign*.rs` + `beam_search.rs` である。したがって「共用層 `planner.rs` を全AI共通で拡張する」という当初の選択肢は前提ごと成立しない（V1 には触れない）。実際の欠陥は下記 RC-2 に差し替える。

---

## 2. 目標指標: 「遊兵ゼロ」（本計画の受け入れ基準）

修正の成否を勝敗だけで測ると、原因の切り分けができない。**「ミッションを持たない／ミッション通りに動けていないユニット・Squad の数を毎ターン数え、0 に近づける」** を全 Stage 共通の一次指標に据える。観測事象「輸送船が何の役割も持たず港の周りをうろついているだけ」は、この指標がそのまま拾う。

### 2.1. 計測点

[engine.rs:1332](engine/src/ai/engine.rs#L1332) — `execute_ai_turn_v2` が全行動を出し切って `NextPhaseCommand` を送る直前。`execute_ai_turn_v2` は1呼び出し1行動のステップ実行で、行動済み Entity は `AiActionCooldown`（[turn_management.rs:142](engine/src/systems/turn_management.rs#L142) でターン境界に破棄される per-turn リソース）に溜まる。**ここが「そのターンに何が動かなかったか」が確定する唯一の点**である。新しい永続状態は増やさない。

### 2.2. 数える対象（4分類）

自軍かつ `Transporting` でない（＝盤上に実体がある）ユニットを母数とする。

| 分類 | 定義 | 判定材料 | 想定される主因 |
|---|---|---|---|
| **A. 任務なし** | どの Squad の `members` / `transport_entity` / `cargo_entities` / `delivered_cargo` にも属さず、`solo_fallbacks` にも入っていない | [SquadManager](engine/src/ai/squad.rs#L78) | RC-4（予約が落ちて Squad が作られない）／生産過多 |
| **B. 任務はあるが命令が出ない** | Squad には属するが、そのターン一度も `AiActionCooldown` に入らなかった | Squad 所属 × cooldown 差集合 | 合流点に到達できない Pickup、経路のない Transit |
| **C. 行動可能なまま終了** | `!HasMoved && !ActionCompleted` かつ cooldown 未登録 | [engine.rs:175-201](engine/src/ai/engine.rs#L175) の既存クエリと同じ条件 | A・B の観測可能な上位集合 |
| **D. 停滞 Squad** | Squad 単位。`phase` と `target` が N ターン変化せず、構成 Entity が誰も行動していない | `SquadManager` のターン間差分 | Pickup 不成立、輸送役喪失 |

**A/B を分けて数えることが本質。** 「任務なしを減らす」だけを目標にすると、無意味なミッションを配って指標を下げられてしまう。B（任務はあるのに動けない）と D（フェーズが進まない）を同時に見ることで、その抜け道を塞ぐ。

### 2.3. 実装方針

- 判定ロジックは **`engine` 内**に置く（CLAUDE.md: ドメインロジックをプレゼン層に出さない）。`engine/src/ai/idle_audit.rs`（新規）に純粋関数 `audit_idle_units(world, player_id, cooldown) -> IdleAudit` を置き、`execute_ai_turn_v2` の終了直前で呼んで per-turn リソースに格納する。
- 出力は mcp-server の既存トレース経路に載せる。[invasion_trace.rs](mcp-server/src/invasion_trace.rs) が既に `snapshot_units` / `snapshot_transport_squads` / `snapshot_island_campaign_for_player` を JSONL へ出しているので、`IdleAudit` のスナップショットを**同じレコードに1フィールド追加するだけ**にする。新しい出力経路を作らない。
- D（停滞 Squad）はターン間差分が要るが、`InvasionTraceCollector`（[invasion_trace.rs:198](mcp-server/src/invasion_trace.rs#L198)）が既にターン列を保持しているため、**engine 側に履歴を持たせずトレース側で差分を取る**。

### 2.4. 既知の「意図的な遊兵」との整合

既存テスト [offshore_reinforce_without_owned_transport_is_not_ready_and_idle](engine/src/ai/island_campaign_tests.rs#L1060) は、輸送役が無い洋上増援で**カーゴを予約せず遊ばせる**ことを正しい挙動として固定している（積みきれない部隊を海に出さないための安全弁）。この指標はそれを分類 A として計上する。**正しい解決は安全弁を緩めることではなく RC-4** — 不足を `purchase_shortfall.transport_slots` に上げて船を作らせ、次ターンに遊兵を解消することである。よってこの既存テストは維持し、指標側で「解消までのターン数」を見る。

---

## 3. 渡洋作戦の仕様（既存コードで確定済み）

新規に作る必要はない。V4 の戦術層（= V3 と同一）に既に実装されている。**生産側がこれを見ていないだけ**である。

### 3.1. (a) 大きな島に上陸した後の行動

[handoff_delivered_cargo](engine/src/ai/squad.rs#L2956)（[update_squads から毎ターン呼ばれる](engine/src/ai/squad.rs#L541)）が、降車したユニットを**上陸した島にスコープした新しい部隊**へ引き渡す。

| 条件 | 生成される部隊 |
|---|---|
| `stats.can_capture` | `MissionType::Capture` / 目標＝島内で最寄りの非自軍 `Property` |
| それ以外 | `MissionType::Attack` / 目標＝島内で最寄りの敵ユニット（無ければ `preferred_target`） |

いずれも `target_island = 上陸した島`、`phase = MovingToTarget`。目標の適格判定は `is_terrain_reachable`（**同一島の連結性**判定であり、そのターンの移動可能範囲ではない）なので、島が大きければ**複数ターンかけて前進する**。既存テスト: [squad.rs:6011](engine/src/ai/squad.rs#L6011), [squad.rs:6070](engine/src/ai/squad.rs#L6070)。

### 3.2. (b) 撃破目標の島の決定

[classify_island](engine/src/ai/island_campaign.rs#L1141) が盤面から島の状態を判定 → [assess_island](engine/src/ai/island_campaign.rs#L1259) が決定に変換する。`EnemyHeld`（自軍ユニット0・敵ユニットか敵拠点あり）→ `Assault`。所要予算は

```
required_assault_budget = 22,500 + max(10,200, 敵戦力 × 1.2)
```

候補の順位は [offensive_priority_key](engine/src/ai/island_campaign.rs#L307):
`決定種別（Expand < Contest < Reinforce < Assault）` → `継続中の作戦を優先` → `Assault は required_budget 昇順 → enemy_combat_value 昇順` → `island_id`。

**敵ユニットの `Entity` は保持しない。** 目標は島（`IslandId`）で持つ。`IslandId` は [IslandMap::analyze](engine/src/ai/islands.rs#L21) が地形のフラッドフィルで決め [setup.rs:207](engine/src/setup.rs#L207) で一度だけ構築されるため、地形が不変である以上**1ゲーム通して安定**する。敵が島内で動いても、撃破されても、目標は崩れない。

### 3.3. (b-2) 「島が1つ」の場合は Assault にならない

[classify_island](engine/src/ai/island_campaign.rs#L1141) は判定順序に意味があり、`Assault` は自動的には選ばれない。

| 盤面 | 判定順序 | 状態 | 決定 |
|---|---|---|---|
| **島が1つ（両軍が同じ島）** | `friendly_units > 0 && enemy_units > 0` が **`EnemyHeld` より先に**マッチ | `Contested` | さらに [analysis.rs:1840](engine/src/ai/island_campaign_analysis.rs#L1840) の `sole_capturable_landmass` 上書きにより **`Observe`（予算0・通常の地上戦略へ委譲）** |
| **多島マップの争奪島** | 同上 | `Contested` | `Contest` / `Reinforce`（3.4 の表） |
| **敵領地が1島・自軍が未上陸** | Ignored ゲートを通過し、`friendly_units == 0` | `EnemyHeld` | `Assault` |

- **`Assault` の前提は `friendly_units == 0`。** 上陸した瞬間に `Contested` へ落ちるが、これは**作戦終了ではなく増援フェーズへの切替**である（詳細は 3.4）。
- `EnemyHeld` も自動ではなく、先に **Ignored ゲート**を通る必要がある: `capturable_properties > 0` かつ `reachable` かつ（`island_income_per_turn > 0` または `strategic_production_sites > 0`）。
- `reachable` は自軍の足がかりが無い島では [transport_options_for_island](engine/src/ai/island_campaign_analysis.rs#L770) が経路を返すことが条件。ただし候補には **`TransportSource::Producible`**（Lander/輸送ヘリを生産できる自軍拠点、ETA に完成1ターンを加算）が含まれるため、「輸送船が無いから到達不能 → 作戦が立たない → 輸送船を買わない」という循環は**発生しない**。
- 単一島マップでは `requires_transport` が立たないので輸送枠需要も出ない（[v4/operation.rs:209](engine/src/ai/v4/operation.rs#L209)）。**占領対象の陸塊が1つしかない盤面では島嶼キャンペーンは要求を出さない**（上表の `Observe` 上書き）ので、Stage 2 の接続が効くのは多島マップ（= map_3、本件の対象）である。単一島での V4 の挙動は Stage 1（RC-1 の限界価値化）で決まる。
- 多島マップの `Contest` / `Reinforce` は [aggregate_missing_requirements](engine/src/ai/island_campaign.rs#L154) の priority_rank 3 として shortfall を出す（Defend 0 / 継続中 1 / Expand 2 / Contest・Reinforce 3 / Assault 4）。

### 3.4. (b-3) 上陸で Assault が外れても、移動中・輸送中のユニットは放置されない

`Assault` → `Contested` の遷移は**上陸フェーズの完了**であって作戦の破棄ではない。継続は3つの機構で担保される。

1. **Squad は毎ターン作り直されない。** [collect_existing_operations](engine/src/ai/island_campaign_analysis.rs#L552) が生存中の Squad から `ExistingCampaignOperation`（輸送船・積荷・戦闘ユニットの Entity＋輸送フェーズ）を復元し、[reserve_candidate](engine/src/ai/island_campaign.rs#L542) が**決定が変わっても同じ島の作戦へそのまま再予約**する（条件は `is_forming || 生存中の輸送フェーズ`）。Transit/Drop 中の Squad は [squad.rs:1050](engine/src/ai/squad.rs#L1050) の `preserve_live_state` によりフェーズ・積載・pickup 位置を保持し、[squad.rs:1095](engine/src/ai/squad.rs#L1095) の `if !preserve_live_state` ガードで巻き戻されない。
2. **継続中の作戦が予算配分で最優先になる。** 輸送船・積荷・戦闘ユニットが1つでも継続していれば [continued_from_existing_squad](engine/src/ai/island_campaign.rs#L584) が立ち、priority_rank が **1**（Defend 0 の次、Expand 2 / Contest・Reinforce 3 / Assault 4 より上）になる（[island_campaign.rs:165](engine/src/ai/island_campaign.rs#L165), [squad.rs:1126](engine/src/ai/squad.rs#L1126)）。上陸を機に予算が別の島へ流れて途中の部隊が干上がることはない。
3. **上陸後も要求は消えない。** 本番の要求生成は [analysis.rs:1448-1472](engine/src/ai/island_campaign_analysis.rs#L1448) で `Contested` を2分岐する。

| 条件 | 決定 | 要求 |
|---|---|---|
| `contested_is_competitive(facts)` | `Contest` | `combat_budget = 0`（現地戦力で足りている判定） |
| それ以外 | **`Reinforce`** | `combat_budget = total_budget = 敵戦力 × 1.2` を要求し続ける |

作戦が本当に終わるのは `Secured` → `Observe`（敵消滅かつ未取得拠点なし）に到達したときだけである。

> **注記（本計画の対象外・別途整理）**: [decide_contested](engine/src/ai/island_campaign.rs#L1215)（Contest / Reinforce / Withdraw の精緻化）は **`#[cfg(test)]` からしか呼ばれていない**（呼び出しは 1550/1559/1563/1567/1571、tests 開始は [1348](engine/src/ai/island_campaign.rs#L1348)）。本番の分岐は上表の analysis.rs 側だけであり、`Withdraw` は本番で生成されない。本件の修正対象ではないが、二重定義として整理対象に残す。

**懸念（Stage 3 で検証する）**: `Contest` 分岐は `combat_budget = 0` なので、`contested_is_competitive` の判定が甘いと第2波が出ず**逐次投入**になる。「なるべく一気に戦力を投入した方が被害が少なく早く撃破できる」という方針に直結するため、敵島上陸を成立させる Stage 3 で Contest / Reinforce の遷移をトレースする。

### 3.5. (b-4) Stage 2 修正前は、増援（Reinforce）の送り先は決まるが、**運ぶ船が要求されなかった**（RC-4）

「Assault が外れた後、増援をどこへ送るか」は3層で決まっており、迷子にはならない。

| 問い | 決定箇所 | 挙動 |
|---|---|---|
| **どの島へ** | — | 変わらない。`Contested` になった同じ島に `Reinforce` が立つ。`IslandId` は地形由来で不変。 |
| **島内のどの座標へ** | [candidate_target_position](engine/src/ai/island_campaign_analysis.rs#L1539) | **継続中の作戦があれば、その `target_position` をそのまま再利用**（1545-1547）。Assault 時に決めた降車・進撃目標が保持され、決定が変わっても目標地点は動かない。無ければ島内の非自軍・占領可能拠点のうち (y,x) 最小、それも無ければ島タイル先頭。 |
| **誰を送るか** | [island_campaign.rs:709-744](engine/src/ai/island_campaign.rs#L709) | `combat_budget = 敵戦力×1.2` を未割当プールから充当。既存ユニットを先に相殺し、埋まらない残りが `purchase_shortfall.combat_budget` として生産要求になる。 |
| **どう運ぶか** | [island_campaign.rs:746-776](engine/src/ai/island_campaign.rs#L746)（**Reinforce 専用ブロック**） | 予約済みユニットのうち `island_id != 目標島` のものを `remote_cargo` として抽出し、`campaign_transport_package_covers` が満たされるまで「同じ出発島にいて、その積荷を積める」輸送役を追加予約する。中途半端に送り出さない設計。 |

**予約された輸送役に与えられるミッションは「生産拠点へ行け」ではなく「合流点で落ち合え」である。**
予約が成立すると [squad.rs:2526-2538](engine/src/ai/squad.rs#L2526) で `MissionType::Transport` の Squad が作られ、`pickup_position` と `phase = Transport(Pickup)` が入る。その `pickup_position` は [select_pickup_position](engine/src/ai/squad.rs#L216) が盤面走査で決める**歩み寄り点**であり、

- 輸送役が進入できる地形（**船は `Port` / `Shoal` のみ** — [squad.rs:251](engine/src/ai/squad.rs#L251)）かつ輸送役から連結
- **全カーゴがそこ（Shoal なら隣接マス）へ到達できる**
- スコア `(輸送役の現在地なら0, 最大距離, 合計距離, y, x)` = **双方の移動距離の最大値を最小化**

を満たす。移動も片側だけでなく、[squad.rs:3444-3600](engine/src/ai/squad.rs#L3444) が「同マスなら `Load` → まず輸送役を寄せる → 輸送役が行動済みならカーゴを寄せる」の順で**双方**を動かす。生産拠点は目標として参照されない（地形が Port/Shoal かは見るが自軍拠点かは見ない）。関係は間接的で、*新造ユニットが工場マスに湧く → カーゴとして予約される → その現在地込みで合流点が決まる* という順序に過ぎない。合流点が成立しないカーゴは [squad.rs:2507-2523](engine/src/ai/squad.rs#L2507) で free pool に戻され、搭載済み分だけで出発する。

**欠陥**: Reinforce の `IslandCampaignRequirement` は [analysis.rs:1465-1471](engine/src/ai/island_campaign_analysis.rs#L1465) で `preferred_transport: None, transport_slots: 0`。`remaining_transport_slots`（[778-782](engine/src/ai/island_campaign.rs#L778)）も `preferred_transport`（[808-820](engine/src/ai/island_campaign.rs#L808)）も **`Assault` のときしか立たない**。

よって **Reinforce は盤上に既にある輸送役を予約できるだけで、足りなくても「輸送船が要る」という shortfall を出さない**。見つからなければ [769行の `?`](engine/src/ai/island_campaign.rs#L769) で作戦の予約ごと `None` になり、その島は増援を受け取れず、生産側にも何も伝わらない。

さらに悪いことに、第1波が降ろし終えた輸送 Squad は Return/Completed に入り、[analysis.rs:1586-1593](engine/src/ai/island_campaign_analysis.rs#L1586) が**その輸送船と積荷を `unavailable_entities` に入れてプールから除外**する（[1812](engine/src/ai/island_campaign_analysis.rs#L1812) → `collect_campaign_resource_pool`）。**上陸が成功した直後こそ、運ぶ船が最も見つからないタイミングになる。**

**症状は「悪いミッションが出る」ではなく「ミッションが1つも出ない」。** 予約が落ちると上記の Squad 生成自体が起きない。空荷で未予約の輸送役は [squad.rs:2547-2570](engine/src/ai/squad.rs#L2547) の「積荷ありなら Drop 部隊にする」ループを素通りし、続く Section B（防衛）は戦闘ユニットのみが対象なので、**任務ゼロのまま盤上に残る**。観測事象「輸送船が何の役割も持たず港の周りをうろついているだけ」はこの経路と一致する。

これは RC-2（V4 が shortfall を読まない）とは独立した**キャンペーン側の欠陥**であり、V3 にも存在する。map_3 で「上陸はできたが第2波が来ない」挙動の直接原因になり得るため、RC-4 として Stage 2 で扱う。

### 3.6. (c) 撃破目標の達成条件

**明示的な完了フラグを持たない（意図的）。** `classify_island` は毎ターン盤面から状態を導出し直すため、状態遷移そのものが達成条件になる。

```
EnemyHeld ──(自軍が上陸)──> Contested ──(敵ユニット消滅)──> Secured
                                       ──(未取得拠点なし)──> Observe（要求を出さなくなる＝作戦終了）
```

作戦の継続性は永続状態ではなく [ExistingCampaignOperation](engine/src/ai/island_campaign.rs#L237)（「永続状態を追加せず、現在も生存している Squad の ID と目標だけから作戦継続情報を復元する」）で再構成される。この無状態設計は既存コードの明示的な方針であり、本計画でも維持する。

---

## 4. 根本原因（コードで確定済み）

### 4.1. RC-1: 候補採点・充足更新・再計画の尺度が一致せず、同じ argmax を再生する

| 箇所 | 事実 |
|---|---|
| 候補採点 | 旧実装は交戦可能な敵すべてへの相性値を合算したため、1体の候補が何円分の脅威を処理できるかという**容量上限が無かった**。一方、購入後の台帳更新は候補コストを上限にしており、採点と更新の尺度が異なっていた。 |
| 脅威量と優先度 | 占領・輸送能力の戦略的重要度を脅威量へ直接2倍で掛けていた。これは「先に倒すべき」を「2倍の対抗戦力が必要」と誤解釈し、残存脅威を過大にした。 |
| 作戦への敵帰属 | 最寄り作戦が同距離の場合に複数作戦が同じ敵を保持できた。また、V4の生産APIは1命令ごとに再度呼ばれるため、呼び出しごとに作戦と残存脅威を再構築すると、直前の購入による減衰が消えた。 |
| 敵未観測時の例外 | `reachable_threats.is_empty()` の場合だけ限界価値を使わず、機動力による汎用点を返していた。敵0体でも増援予測から撃破予算は立つため、具体的根拠のないAntiAirが同じ作戦から毎ターン生産された。 |
| 撃破予算 | `destroy_budget = parity_gap.max(full_commitment)` は投入可能額の上限を残すが、正の限界価値を保証しない。これを「必ず全額使う要求」と解釈すると、対処済み脅威に対する無価値な追加購入を止められない。 |

**帰結（観測値と定量的に一致）**: 同じ敵集合から同じ最大候補が選ばれ、購入後の減衰も次のAPI呼び出しで失われる。よって **全空き施設が同一ユニットを生産 → 工場4〜5基 × 14ターン ≒ 同一ユニット20体**となる。`full_commitment` 自体ではなく、予算を限界価値と切り離して使い切ろうとしたことがループの直接原因である。

### 4.2. RC-2: V4 の生産が、自分の戦術層が立てた島嶼キャンペーンを一切参照しない

V4 の戦術層は V3 と同一なので、**攻勢作戦は既に立っている**。立った要求も既に集計されている。

1. [plan_squads](engine/src/ai/squad.rs#L1974) は `is_v3 = uses_v3_tactics()`（**V4 も true**）で島嶼キャンペーンを走らせ、`IslandCampaignPortfolio` を [AiTurnStrategyCache に格納する](engine/src/ai/squad.rs#L2013)。
2. [aggregate_missing_requirements](engine/src/ai/island_campaign.rs#L154) が不足分を `IslandCampaignShortfall`（**輸送枠・占領ユニット数・戦闘予算・予約予算・優先度**）として島単位で吐く。
3. [strategy.rs:790-845](engine/src/ai/strategy.rs#L790-L845) は `is_v3`（= V4 も true）で `strategy.campaign_shortfalls` を**実際に埋めている**。
4. 生産は部隊計画の**後**に走る（[engine.rs:1280](engine/src/ai/engine.rs#L1280) > [engine.rs:1173](engine/src/ai/engine.rs#L1173)）ので、V4 が読もうと思えば読める。

にもかかわらず [production.rs:497](engine/src/ai/production.rs#L497) が

```rust
if resolve_player_ai_version(world, player_id).uses_operation_driven_production() {
    return crate::ai::v4::decide_production_v4(world, player_id);
}
```

で **`campaign_shortfalls` の消費側（[749](engine/src/ai/production.rs#L749) / [854](engine/src/ai/production.rs#L854) / [902](engine/src/ai/production.rs#L902)）より手前に return する**。そして `engine/src/ai/v4/` 配下を `campaign|island` で grep すると **一致0件**。[BoardScan::collect](engine/src/ai/v4/mod.rs#L150) も `AiTurnStrategyCache` を読まない。

**帰結**: キャンペーンが出した購買要求（輸送船何隻・占領ユニット何体・撃破予算いくら）は誰にも応えられず、V4 は自前の無状態 argmax（RC-1）で AntiAir を買い続ける。作戦は必要ユニットが揃わないので発動条件に到達せず、**輸送船は任務を与えられないまま港の周りに滞留する**。

### 4.3. RC-3（副次）: 作戦枠が近場に食われ、遠方の高価値島が落選する

[mod.rs:396-434](engine/src/ai/v4/mod.rs#L396-L434) は `facility_lead_time`（**直線ETA・歩兵の足**）昇順で `MAX_OPERATIONS = 4` に truncate。map_3 は中立拠点23個が6島に分散し、収入レースはそこで決まる。近い順で4枠を切ると遠方の高価値島に作戦が立たず、`requires_transport` も `transport_slots` も立たない。占領作戦の救済は1枠のみ。

### 4.4. RC-4: 上陸後の増援（Reinforce）が輸送手段を要求できず、第2波が静かに消える

詳細は 3.5。要点だけ再掲する。

1. `Reinforce` の要求は `transport_slots = 0` / `preferred_transport = None`（[analysis.rs:1465](engine/src/ai/island_campaign_analysis.rs#L1465)）。輸送枠を立てるコードは `Assault` 分岐にしかない（[island_campaign.rs:778](engine/src/ai/island_campaign.rs#L778), [808](engine/src/ai/island_campaign.rs#L808)）。
2. 洋上増援は [island_campaign.rs:746-776](engine/src/ai/island_campaign.rs#L746) で**既存の**輸送役を探すのみ。見つからなければ [769](engine/src/ai/island_campaign.rs#L769) の `?` で作戦ごと不成立になり、**shortfall も出ない**（＝生産に伝わらない）。
3. 第1波を降ろした輸送船は Return/Completed 扱いで `unavailable_entities` に入りプールから外れる（[analysis.rs:1586](engine/src/ai/island_campaign_analysis.rs#L1586)）。上陸成功直後が最も船を見つけにくい。

**帰結**: 上陸には成功しても第2波が海を渡れず、少数の上陸部隊が現地で消耗する。「一気に戦力を投入した方が被害が少ない／逐次投入は AI を弱体化させる」という方針に直撃する。RC-2 を直して V4 が shortfall を読むようにしても、**そもそも shortfall が出ていない**ため RC-4 を併せて直さないと渡洋の連鎖は完成しない。V3 にも同じ欠陥があるが、修正は共有層（`island_campaign*.rs`）なので V3 も同時に改善される（V3 側の回帰確認を検証項目に含める）。

---

## 5. 修正方針

### 5.1. Stage 0: 計測基盤（先に事実を固定する）

**0-a. 遊兵カウンタ（第2章「目標指標」の実装）。** `engine/src/ai/idle_audit.rs` に `audit_idle_units` を追加し、`execute_ai_turn_v2` のターン終了直前で A/B/C を集計、`invasion_trace` の既存レコードに載せる。**これを先に作る**——以降の全 Stage の合否をこの数字で判定するため。

**0-b. 生産トレース。** `plan_production` のループに構造化トレース（ターン / 作戦種別・anchor / 枠種別 / 選定ユニット / 選定前後の deficit / 却下理由）を出す。AntiAir×20 と Lander×7 が **どの枠から出たか**を確定させる。既存の `logs/v4trace*.log` 系の出力経路を踏襲する。

**テスト**: 盤上に自軍ユニットを置き Squad を与えない World で `audit_idle_units` が分類 A に計上すること／Transport Squad を持つが cooldown に入らなかったユニットが分類 B に計上されること（指標そのものの単体テスト）。

**合格条件**:
- 「同一ターン内で同一ユニットが全施設に発注される」ことがログ上で再現できること
- map_3 の現状（修正前）の遊兵数がターン別に取れ、**ベースラインとして記録**されること。観測事象の「港でうろつく Lander」が分類 A として実際に計上されることを確認する（指標が事象を捉えられていることの検証）

### 5.2. Stage 1: 残存脅威台帳と同一ターン計画を同じ契約で動かす（ループの停止）

`full_commitment` は**投入可能額の上限**として残す。ただし「正の限界価値が無くても全額使う」という意味にはしない。Stage 1 の契約を以下に固定する。

1. **敵の一意な帰属**: 各敵は、到達ETAが最小の作戦1件だけへ帰属させる。同率時は作戦の安定した並び順で決め、複数作戦へ重複計上しない。
2. **量と優先度の分離**: `remaining_value` は `cost × 残HP率` とし、戦略的重要度を掛けない。占領・輸送能力の重みは `priority_weight` として別に持ち、割当順と採点だけに使う。
3. **到達可能性の統一**: 既存戦力の計上、候補採点、購入後更新の3箇所で同じ到達可能性判定を用いる。施設から自力展開できないが輸送可能な部隊は、作戦anchorを上陸起点として敵への到達性を測る。
4. **容量付き被覆**: 候補1体の容量は購入候補なら `cost`、既存戦力なら `cost × 残HP率` とする。敵 `e` に対する効率を `efficiency = clamp(max(pair_value, 0) / e.cost, 0, 1)`、実被覆を `min(e.remaining_value, remaining_capacity × efficiency)` とする。容量は `実被覆 / efficiency` だけ消費する。
5. **採点と更新の同型化**: `priority_weight × 実被覆` が最大の未対処敵から貪欲に容量を割り当てる単一関数を使う。候補採点は台帳のcloneへ同関数を適用し、購入確定時は実台帳へ適用する。採点だけ総和、更新だけ容量制限という差を許さない。
6. **同一ターンの原子性**: その手番の全空き施設を一度の `plan_production` で計画し、命令キューを順に返す。1命令ごとのAPI呼び出しで盤面から再計画せず、同じ残存脅威台帳を最後の施設まで消費する。
7. **停止条件**: 予算が残っていても、到達可能な全候補の限界価値が0なら当該枠を打ち切る。Stage 1 では未観測の敵増援に具体的な兵種を捏造しないため、この残額は不具合ではなく「正の根拠がない購入を避けた額」としてトレースする。

`pair_value` / `engagement_factor` は相性計算として再利用し、平均相性の `counter_value` はStage 1の Combat/Intercept 採点には使用しない。

**テスト**: 戦略重みを上げても残存戦力そのものは増えないこと／同距離の敵が複数作戦へ重複しないこと／単一の航空脅威を対空で覆った後は未対処の地上脅威に対する候補へ切り替わること／前提枠を含む複数施設の実計画でも対空と地上候補を同一ターンに選ぶこと。

**実測結果（2026-08-08、map_3 / seed 42 / 先後各1局）**:

- 初回の素直な限界価値化は、敵0作戦の汎用点を残していたため AntiAir 49体、A/B/C=`150/65/49` まで悪化した。
- 汎用点を撤去し「具体的な残存脅威が無ければCombat購入を止める」契約へ統一後、敵0作戦からのCombat購入は0件、AntiAirも0体となった。
- 最終内訳は Infantry 14 / Bomber 9 / TransportHelicopter 7 / Bcopters 5 / HeavyFighter 5 / Lander 2 / Fighter 1。単一兵種が工場数×ターン数へ張り付くループは解消した。
- A/B/CはStage 0の `96/36/12` に対して `92/54/29`。Aおよび終盤3ターンのA合計（25→12）は改善した。一方、Bの53/54件は Infantry 30件と TransportHelicopter 23件で、停滞SquadもTransport/Captureへ集中した。これは生産した戦力と島嶼キャンペーンの接続を扱うStage 2の未解決事項として引き継ぐ。

### 5.3. Stage 2: V4 生産を島嶼キャンペーンに接続する（生産と戦術の契約を戻す）

**V1 の `planner.rs` / `missions.rs` には触れない**（V4 は通らない）。V3 が既に持っている接続を V4 でも成立させる。

Stage 1 後の実測から、問題は「V4 の汎用枠へキャンペーン需要を足せばよい」ではなく、**島嶼キャンペーンを要求・生産・割当の唯一の契約として先に完結させること**だと判明した。Stage 2 の契約を以下に固定する。

1. **キャンペーン不足を V4 汎用作戦より先に生産する。** `decide_production_v4` の冒頭で `plan_campaign_shortfall_production` を呼び、優先度順の shortfall を消費する。この純粋関数は V3 と共有し、可視性だけを `pub(crate)` へ広げる。
2. **同一 shortfall を V4 汎用枠へ重ねて変換しない。** キャンペーン要求はキャンペーン生産キューだけが所有する。`BoardScan` や `derive_slots` へ同じ需要を複写すると、予算・枠・帰属の二重管理になるため採用しない。
3. **1手番1計画を守る。** 全キャンペーン生産命令を既存の `AiTurnStrategyCache` に保存し、生産APIの呼び出しごとに1件ずつ返す。高優先パッケージを当該手番で完成できない場合は `campaign_production_blocks_generic` を立て、余った施設・資金を汎用生産へ流さない。
4. **生産した構成要素は次ターンの盤面再分析で同じ作戦へ予約する。** 新しい永続予約台帳は増やさない。`collect_existing_operations` と `reserve_candidate` が Entity 単位の実在戦力を再構築し、不足分だけを翌手番に再要求する。
5. **洋上 Reinforce の輸送不足を作戦消滅ではなく shortfall にする。** 別島にいる実 cargo について、総スロット数だけでなく「同じ出発島」「積載可能兵種」を二部マッチングで検査する。不足時は、生産圏内の実拠点で生産可能な Lander / TransportHelicopter のうち、全 cargo を運べる最小費用の種別・不足枠・費用を `purchase_shortfall` へ返す。目標島にいる、または目標へ自力到達できる戦力には輸送を要求しない。
6. **輸送 cargo と自力展開戦力を分離する。** Bomber など目標島へ自力到達できる戦力は Transport/Forming へ入れず、島外から直接 Attack / Defense Squad へ割り当てる。輸送役が積載できる地上戦力だけを cargo とする。
7. **Pickup 中は行動可能な cargo を順に進める。** 先頭 cargo が行動済みでも後続 cargo の合流移動を止めない。Drop は輸送役と降車 cargo の双方が参加した行動として `AiActionCooldown` に記録し、監査上の偽陽性を除く。

Stage 2 は「作戦を組んだユニットが生産・合流・輸送の責務を持ち、遊兵を減らす」段階である。**どの敵島をいつ優先するかは Stage 3** とし、14ターン以内の敵初期島上陸を Stage 2 の必須条件にはしない。この境界を混ぜると、接続の不具合と作戦順位の不具合を区別できない。

**テスト**（`engine` 単体。既存の [island_campaign_analysis.rs](engine/src/ai/island_campaign_analysis.rs) と [squad.rs のフェーズ遷移テスト](engine/src/ai/squad.rs#L6011) の書式を踏襲する）

- `EnemyHeld` な島がある盤面で、V4 の生産が **輸送船と陸戦ユニットを両方**発注すること（現状は AntiAir に偏る）
- 同一ターンに同じ shortfall へ二重発注しないこと（キャッシュキューの回帰）
- キャンペーン予約分が汎用枠の予算から差し引かれること
- 上陸後の引き渡し（`handoff_delivered_cargo`）が Capture / Attack を島スコープで生成すること — **既存テストの維持確認**（新規実装ではない）
- **RC-4**: 敵と自軍が同居する島（`Contested` → `Reinforce`）に対し、自軍の増援が別島にいて手持ちの輸送船が無い盤面で、`IslandCampaignShortfall.transport_slots > 0` と `preferred_transport = Some(Lander)` が出ること（現状はどちらも 0 / None で、作戦自体が消える）
- **RC-4**: 目標島に自軍の陸路がある増援では輸送枠を要求しないこと（過剰生産の防止）
- **RC-4**: 修正後、洋上増援の輸送役に `MissionType::Transport` の Squad が実際に付き、`pickup_position` が設定されること（＝「任務ゼロで滞留する輸送船」が出ないことの回帰テスト）

**実測結果（2026-08-08、map_3 / seed 42 / 14ターン / V4先後各1局）**:

- Stage 1 の A/B/C=`92/54/29` に対し、Stage 2 は **`61/50/19`**。
- 主要な排他的分類 A+B は `146 → 111`（35件、約24%減）、3指標合計は `175 → 130`（45件、約26%減）。A/B/Cのすべてが悪化せず減少した。
- 関連単体テストは island campaign 36件、squad 37件が通過。進行中 Assault を「追加輸送を生産できない」という理由で消す試案は既存作戦を破壊したため棄却し、動的な輸送不足導出は Reinforce に限定した。
- Issue #54 の敵初期島上陸は両手番とも14ターン内では未達。これは中立島を先に埋める作戦順位・同時攻勢上限の問題であり、Stage 3へ引き継ぐ。

### 5.4. Stage 3: 作戦カバレッジ（遠方の高価値島を落とさない）

- 占領作戦の順位を `facility_lead_time` 単独ではなく **経済価値 ÷ リードタイム** にする。価値算出は既存の [Objective::evaluate](engine/src/ai/objectives.rs#L24)（収入・距離・敵生産拠点ペナルティ）を再利用し、二重実装しない。
- `MAX_OPERATIONS` の定数 4 を、**同時に補給できる作戦数＝生産施設数**から導出する。根拠のないマジックナンバーを残さない。
- Issue #54 の主ゲート（敵初期島の選定 → 上陸 → 同一 cargo による攻撃・被攻撃・占領）をこの Stage で判定する。Stage 2 の A/B/C を悪化させず、敵初期島が同時攻勢上限から恒常的に漏れないことを確認する。

---

## 6. 変更対象ファイル

| ファイル | 内容 |
|---|---|
| `engine/src/ai/idle_audit.rs`（新規） | **遊兵カウンタ**。`audit_idle_units(world, player_id, cooldown) -> IdleAudit`（純粋関数）と分類 A/B/C の定義 |
| `engine/src/ai/engine.rs` | `execute_ai_turn_v2` のターン終了直前（[1332](engine/src/ai/engine.rs#L1332)）で `audit_idle_units` を呼び per-turn リソースへ格納 |
| `mcp-server/src/invasion_trace.rs` | `IdleAudit` のスナップショットを既存レコードに1フィールド追加。D（停滞 Squad）はターン間差分で算出 |
| `engine/src/ai/v4/mod.rs` | `slot_fitness` の限界価値化、残存脅威の減衰、キャンペーン不足分の先行消費と同一手番キュー、未完成時の汎用生産ブロック、トレース |
| `engine/src/ai/v4/operation.rs` | 残存脅威を `Operation` / `OperationFacts` に載せる |
| `engine/src/ai/production.rs` | `plan_campaign_shortfall_production` の可視性のみ（ロジック不変。V1/V2/V3 の経路は一切変更しない） |
| `engine/src/ai/island_campaign_analysis.rs` | **RC-4**: 生産圏内の実拠点から、生産可能な輸送種別・積載能力・出発島を導出する |
| `engine/src/ai/island_campaign.rs` | **RC-4**: Reinforce の実 cargo と輸送役を出発島・積載可能兵種込みで照合し、不足輸送を `purchase_shortfall` へ上げる |
| `engine/src/ai/squad.rs` | 自力展開戦力を輸送 cargo から除外して直接任務へ割り当て、Pickup 中は次の行動可能 cargo を進める |
| `scripts/eval_matchup.py` | 同一 JSONL レコードへ資金と島嶼キャンペーンの要求・不足・割当を出力する |

`planner.rs` / `missions.rs` は **V1 専用のため変更しない**。

**回帰の範囲について**: Stage 0〜3 のうち RC-1／RC-2／RC-3 の修正は `v4/` 配下に閉じるため V1/V2/V3 に影響しない。一方 **RC-4 は共有層（`island_campaign*.rs`）の修正なので V3 の挙動も変わる**（改善方向だが、変わることは事実）。したがって V3 側の回帰確認を検証に含める。

CLAUDE.md 準拠: ゲームルールの判定は `engine` 内に閉じ、`cli` / `gui` には出さない。コメントは日本語。

---

## 7. 検証

各 Stage 完了ごとに実施する。**評価前に必ず release ビルドを通す**。

```
cargo build --release -p mcp-server
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

**一次指標（全 Stage 共通）: 遊兵数**

- Stage 0 で取ったベースラインに対し、各 Stage で A/B/C を必ず再計測する。ただし3分類を無条件に単調減少させるのではなく、変更した責務に対応する指標を合格ゲートとする。Stage 1は過剰生産に直結するA、Stage 2は要求・ミッション接続に直結するB/C、Stage 3は停滞SquadのDを主ゲートとする。
- 主ゲート以外が悪化した場合も合格扱いで無視してはならない。兵種・MissionType・phase別に原因を分解し、次Stageの明示的な未完了タスクへ引き継ぐ。最終StageではA/B/C/DすべてをStage 0以下にする。
- 終盤（最終3ターン）の A が **0** に到達することを最終目標とする。0 にできない残りは、ターン数と理由（例: 輸送待ちで次ターン解消見込み）を明記する。
- **B と D を必ず併記する。** A だけを下げる修正（無意味なミッションの配布）は B・D の増加として現れるので、そこで弾く。

**Stage 1 の合格条件（ループ停止）**

- map_3 のトレースで、1ゲーム内の同一ユニット生産数が工場数×ターン数に張り付かないこと
- 生産内訳が敵編成の変化に追従して変わること
- 同一ターンの各購入で、直前の購入による残存脅威の減少が次の候補採点へ反映されること
- 正の限界価値がある候補を残して停止しないこと。正の限界価値が無い場合は資金が残ってもよく、`SlotCleared` と残額をトレースで説明できること
- 遊兵Aの累計と終盤3ターン合計がStage 0より悪化しないこと。Stage 1は生産内訳の修正であり任務接続はStage 2の責務なので、A=0とB/C改善はStage 1単独の必須条件にしない
- B/Cが悪化した場合は兵種・MissionType・phase別に原因を記録し、Stage 2の未完了ゲートに追加すること

**Stage 2 の合格条件（生産・作戦・輸送の接続）**

- `scripts/eval_matchup.py --mode batch --map map_3 --p1 v4 --p2 v3 --criteria issue54 --max-turns 14`（先攻・後攻の両席、各1ゲームで足りる）
- Stage 1 と同一条件で、A/B/Cをすべて再計測する。合格条件は **A+BがStage 1より減少し、A/B/CのいずれもStage 1より悪化しないこと**。
- キャンペーン shortfall の命令を同一手番に重複発注せず、未完成の最優先パッケージを無視してV4汎用生産へ流れないこと。
- 洋上 Reinforce で輸送役が無い場合も作戦が消えず、実 cargo を積載できる生産可能な輸送種別が `purchase_shortfall` に現れること。陸路・自力展開可能な戦力には不要な輸送要求を出さないこと。
- 既存の上陸・引き渡し・進行中作戦保持テストを維持し、Stage 2の共有層変更がready済みAssaultやTransit/Dropを破壊しないこと。
- **輸送船の遊兵が減ること**（観測事象の直接的な合否）。修正前は分類 A に計上されていた輸送役が Transport Squad を持ち、行動可能な cargo があるのに先頭 cargo の cooldown だけで Pickup 全体が止まらないこと。
- Issue #54 の敵初期島上陸は参考値として併記するが、目標選定・同時攻勢上限を扱う Stage 3 の合格ゲートとする。

**Stage 3 以降**

- map_3 の両手番で Issue #54 の敵初期島上陸と侵攻成立を確認すること
- map_3 で V3・V1 に勝ち越すこと（決定的に）
- 遊兵 A が終盤 0、B・D が Stage 2 時点から悪化していないこと（作戦枠を広げた副作用で「立てたが動かない作戦」が増えていないことの確認）
- map_1 / map_2 の回帰確認（V4 席）
- **V3 の回帰確認**（RC-4 が共有層のため）: map_3 で `--p1 v3 --p2 v1` を先攻・後攻各1ゲーム。V3 が悪化していないこと（改善していれば RC-4 の裏付けになる）

---

## 8. 補足

- 作業ツリーに未コミットの `is_invasion_allowed` 修正（守備隊比較の是正）が残っている。中立島は `enemy_production_count == 0` のためこのゲートを通らず、**map_3 の失敗とは直交する**。単独の是正として切り出してコミットし、本件の成果とは混同しない。
- 検証で生成した使い捨ての `logs/v4_*.txt` / `reports/v4_*.md` 群は、完了時に整理する。

---

## Changelog

### [1.1.0] - 2026-08-08

#### 変更

- Stage 2を「島嶼キャンペーンshortfallの優先生産、洋上Reinforceの輸送不足化、自力展開戦力とcargoの分離、Pickup進行」の実装可能な契約へ再定義。
- Stage 2の実測 A/B/C=`61/50/19` を記録し、Issue #54の敵初期島侵攻を作戦順位を扱うStage 3へ移管。

### [1.0.0] - 2026-08-07

#### 追加

- V4 生産AI の修正計画を新規作成。根本原因 RC-1〜RC-4、Stage 0〜3 の修正方針、受け入れ基準「遊兵ゼロ」、検証手順を定義。
