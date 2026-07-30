# Issue #58 島嶼キャンペーン・ポートフォリオ設計

## 1. 背景

既存のV3島嶼戦略は、敵所有拠点を発見すると早期に単一の `invasion_target` を設定し、中立島より敵本島を優先する。このため、収入の少ない序盤から高コストの敵島侵攻を開始し、敵戦力を排除・占領する前に上陸部隊を失っている。

Issue #58 のV3対V2 baselineでは、map_3の8試合すべてで敵島の占領完了が0件、勝率が0%だった。一方でV2は島嶼侵攻を積極的に行わないため、中立島の取り合い、撤退、増援、確保済み島の再防衛を十分に評価できない。

本設計では、単一の敵島侵攻目標を廃止し、全島を継続評価する「島嶼キャンペーン・ポートフォリオ」を導入する。序盤は低コストの輸送ヘリと占領要員で中立島を収益化し、敵戦力に応じた侵攻編成を安定して用意できる段階で敵島へ進む。

## 2. 目標

- 敵のいない中立島を、敵本島より先に低コストで確保する。
- 複数の島を候補として評価し、最大3島の攻勢作戦を同時に管理する。
- 島ごとに進攻継続、増援、撤退、防衛を独立して判断する。
- 確保済み島を管理対象から外さず、敵接近時に防衛へ戻す。
- 敵島侵攻は最低32,700G相当の編成を基準とし、対象島の敵戦力に応じて予算を増やす。
- 状態変数を複数組み合わせず、各島は常に1つの `IslandCampaignState` を持つ。
- V3対V1とV3自己対戦を併用し、勝敗と島嶼行動の両方を評価する。

## 3. 対象外

- V1輸送AIの変更
- Load・Transit・Dropのドメインルール再実装
- map_3の座標、手番、固定seedに依存する分岐
- 敵首都への固定突撃
- 無制限の同時攻略
- GUI変更

## 4. 基本方針

### 4.1 状態と判断の分離

島ごとに保持する状態は `IslandCampaignState` の1つだけとする。拠点数、戦力、ETA、必要予算は状態ではなく、そのターンの判断材料である。

作戦判断は `IslandCampaignDecision` として毎ターン導出する。これは状態変数ではなく、そのターンに実行する行動方針である。

### 4.2 非永続のポートフォリオ

全島の評価結果は盤面から毎ターン再構築する。新しい永続ライフサイクル状態は追加しない。

進行中作戦の継続情報は、既存の `SquadManager` が持つ対象島、対象座標、所属ユニット、輸送フェーズから復元する。

### 4.3 完全編成単位の割当

新規の島嶼作戦へ、歩兵1体だけ、輸送だけ、戦闘ユニットだけを断片的に割り当てない。島の状態と目的に応じた最小編成を満たす場合だけ作戦を開始する。

## 5. 島状態

```rust
pub enum IslandCampaignState {
    Ignored,
    OpenNeutral,
    Secured,
    Threatened,
    Contested,
    EnemyHeld,
}
```

### 5.1 `Ignored`

次のいずれかを満たす島。

- 占領可能拠点がない。
- 自軍が現在保有する、または所有施設で生産可能な全輸送方式のいずれでも到達不能。
- 収入増加がなく、作戦上の生産施設価値もない。

### 5.2 `OpenNeutral`

誰も進出・占領していない純粋な中立島。

```text
neutral_properties > 0
friendly_properties == 0
enemy_properties == 0
friendly_units == 0
enemy_units == 0
```

自軍・敵軍の輸送戦力が接近中でも、上陸や占領が始まるまでは状態は `OpenNeutral` とする。ただし候補評価では双方の到着ETAを考慮する。

### 5.3 `Secured`

自軍の足場があり、島内に敵ユニットがおらず、直近の敵到着もない状態。

```text
enemy_units == 0
かつ
(friendly_units > 0 または friendly_properties > 0)
かつ
enemy_arrival_eta > 2 または enemy_arrival_eta == None
```

全拠点を所有している必要はない。中立拠点や無人の敵所有拠点が残っていても、軍事的に安全なら `Secured` とする。

### 5.4 `Threatened`

自軍の足場があり、島内には敵がいないが、敵戦力が2ターン以内に到着可能な状態。

```text
enemy_units == 0
かつ
(friendly_units > 0 または friendly_properties > 0)
かつ
enemy_arrival_eta <= 2
```

`Secured + Threatened` のような複合状態にはせず、`Secured` から `Threatened` へ遷移する。

### 5.5 `Contested`

両軍が島内に存在する状態。

```text
friendly_units > 0
かつ
enemy_units > 0
```

拠点所有状態にかかわらず、局地戦と占領競争を優先する。

### 5.6 `EnemyHeld`

自軍ユニットがおらず、敵が島内に足場を持つ状態。

```text
friendly_units == 0
かつ
(enemy_units > 0 または enemy_properties > 0)
```

敵本島だけでなく、敵に先取りされた元中立島も含む。

### 5.7 判定順序

```text
1. Ignored
2. Contested
3. Threatened
4. OpenNeutral
5. Secured
6. EnemyHeld
```

各条件は排他的に実装する。状態の初期値を一律に設定せず、マップ読込直後から盤面情報で判定する。

一般的な初期状態は次のとおり。

| 島 | 初期状態 |
| --- | --- |
| 自軍本島 | `Secured` |
| 敵本島 | `EnemyHeld` |
| 無人中立島 | `OpenNeutral` |
| 占領可能拠点がない島 | `Ignored` |

## 6. 島評価データ

```rust
pub struct IslandCampaignAssessment {
    pub island_id: IslandId,
    pub state: IslandCampaignState,
    pub decision: IslandCampaignDecision,

    pub neutral_properties: u32,
    pub friendly_properties: u32,
    pub enemy_properties: u32,

    pub friendly_combat_value: u32,
    pub enemy_combat_value: u32,

    pub friendly_arrival_eta: Option<u32>,
    pub enemy_arrival_eta: Option<u32>,
    pub friendly_capture_eta: Option<u32>,
    pub enemy_capture_eta: Option<u32>,

    pub expansion_payback_turns: Option<u32>,
    pub required_budget: u32,
    pub allocated_budget: u32,
}
```

HPが減少したユニットの戦闘資産価値は、既存の評価と同様にHP割合をコストへ掛けて算出する。輸送中ユニットは、所属する輸送作戦の対象島への到着戦力として扱う。

`friendly_arrival_eta` は、対象島へ割当済みの自軍部隊が島内または接岸可能タイルへ到着する最小ターン数とする。`enemy_arrival_eta` は、可視状態にある敵地上・航空・艦船・輸送ユニットのうち、現在の移動能力で対象島または接岸可能タイルへ到着できる最小ターン数とする。敵の非公開な目標意図は推測せず、到達可能性だけで判定する。

## 7. 作戦判断

```rust
pub enum IslandCampaignDecision {
    Observe,
    Expand,
    Secure,
    Defend,
    Contest,
    Reinforce,
    Withdraw,
    Assault,
}
```

### 7.1 基本対応

| 状態 | 判断 |
| --- | --- |
| `Ignored` | `Observe` |
| `OpenNeutral` | 投資回収効率が上位なら `Expand` |
| `Secured` | 未占領拠点が残るなら `Secure`、なければ `Observe` |
| `Threatened` | `Defend` |
| `Contested` | `Contest` / `Reinforce` / `Withdraw` |
| `EnemyHeld` | 侵攻予算を満たせば `Assault`、不足なら `Observe` |

### 7.2 進行中作戦の維持

既存の作戦は次のいずれかになるまで対象島を維持する。

- 島が `Secured` になった。
- `Withdraw` と判定された。
- 防衛を優先するため一時停止された。
- 輸送・占領要員が全滅し、完全編成を再構築できない。

小さなスコア変動だけでは対象島を変更しない。

## 8. OpenNeutralの投資回収評価

### 8.1 最小拡張編成

原則として次の編成を使用する。

```text
輸送ヘリ1体
占領可能ユニット2体
```

現行マスターデータの最小費用は次のとおり。

```text
輸送ヘリ 4,000G
軽歩兵2体 2,000G
合計 6,000G
```

対象島へ輸送ヘリで到達できない、または輸送ヘリを生産できない場合だけ、対象島へ到達可能な最安輸送へフォールバックする。

### 8.2 回収ターン

```text
payback_turns =
  transport_eta
  + capture_turns
  + ceil(missing_package_cost / island_income_per_turn)
```

値が小さい島ほど優先する。

同値の場合は次で決定する。

1. 工場・港・空港を多く含む。
2. 中立拠点が多い。
3. 輸送ETAが短い。
4. 島IDが小さい。

`island_income_per_turn == 0` の島は拡張候補から除外する。

## 9. Contestedの継続・増援・撤退

```text
friendly_power =
  島内自軍戦闘資産
  + 2ターン以内に到着する割当済み増援

enemy_power =
  島内敵戦闘資産
  + 2ターン以内に到着可能な敵戦力
```

### 9.1 `Contest`

```text
friendly_capture_eta <= enemy_capture_eta + 1
かつ
friendly_power >= enemy_power
```

### 9.2 `Reinforce`

```text
現在はContest条件を満たさない
かつ
完全増援編成後は friendly_power >= enemy_power * 1.2
かつ
上位3作戦の資金・ユニット制約内
```

### 9.3 `Withdraw`

```text
Contest条件を満たさない
かつ
完全増援編成を割り当てられない
かつ
より投資回収効率のよいOpenNeutral候補が存在する
```

撤退経路がない上陸部隊は無理に海へ戻さず、局地防衛または占領を継続する。回収可能な輸送・ユニットだけを次作戦へ再利用する。

## 10. EnemyHeldの侵攻予算

### 10.1 最低編成

```text
固定輸送・占領費:
  輸送船          16,500G
  輸送ヘリ         4,000G
  軽歩兵2体         2,000G
  小計            22,500G

最低戦闘費:
  軽戦車            6,000G
  装甲車            4,200G
  小計            10,200G
```

```text
最低侵攻予算 = 22,500G + 10,200G = 32,700G
```

### 10.2 敵戦力による増額

```text
required_combat_budget =
  max(
    10,200G,
    ceil(target_island_enemy_combat_value * 1.2)
  )

required_assault_budget =
  22,500G + required_combat_budget
```

敵戦闘資産が8,500Gを超えると、最低戦闘費10,200Gより敵資産×1.2が大きくなる。

### 10.3 保有資産の充当

```text
available_assault_budget =
  未割当の侵攻適格ユニット資産
  + 他作戦予約を除いた使用可能資金
```

```text
available_assault_budget >= required_assault_budget
```

を満たす島だけ `Assault` とする。

複数島へ同じユニット・資金を二重計上しない。

## 11. 複数島ポートフォリオ

```rust
pub struct IslandCampaignPortfolio {
    pub islands: Vec<IslandCampaignAssessment>,
    pub active_offensives: Vec<IslandCampaignAssignment>,
}
```

### 11.1 同時作戦数

攻勢作戦は最大3島。

対象判断:

- `Expand`
- `Contest`
- `Reinforce`
- `Assault`

1島ごとに最低1つの完全編成を保証する。完全編成を割り当てられない候補は `Observe` とする。

### 11.2 全島管理

`Secured` 島は攻勢上限に数えないが、全て毎ターン脅威評価する。

`Threatened` が発生した場合は、攻勢優先度最下位の作戦を一時停止して `Defend` を優先する。

安全な `Secured` 島では占領要員だけが残存中立・無人敵拠点を占領し、戦闘部隊は次作戦へ解放する。

## 12. 生産優先順位

`production.rs` は島を直接選択せず、ポートフォリオが算出した不足編成を入力として使用する。

優先順位:

1. `Threatened` 島の防衛不足
2. 実行中作戦の欠損補充
3. `OpenNeutral` 上位候補用の輸送ヘリ＋占領要員
4. `Contested` 島の増援
5. `EnemyHeld` 島の侵攻編成
6. 余剰資金による一般戦力

装甲車の `max_cargo` は海を越える輸送需要へ計上しない。装甲車は地上輸送需要だけを満たす。

## 13. Squad責務

- `Expand`: 輸送ヘリと占領要員をOpenNeutral島へ割り当てる。
- `Secure`: 島内占領要員を最短の未占領拠点へ割り当てる。
- `Contest`: 占領要員を拠点へ、戦闘要員を敵排除へ分離する。
- `Reinforce`: 既存対象島を維持したまま完全編成を追加する。
- `Withdraw`: 回収可能な輸送・生存部隊を次候補へ再利用する。
- `Assault`: 重戦力編成と占領編成を同時投入する。
- `Defend`: 敵到着ETAが短い所有島へ戦闘部隊を優先再配置する。

## 14. データフロー

```text
ECS盤面
  → 全島をIslandCampaignAssessmentへ変換
  → IslandCampaignStateを1つ決定
  → ROI・ETA・局地戦力・必要予算を算出
  → IslandCampaignDecisionを決定
  → Defendを優先割当
  → 残資金・残ユニットで攻勢上位3島を割当
  → SquadManagerへ島別部隊を反映
  → ProductionStrategyへ不足編成を集約
```

## 15. エラー・境界条件

- 占領可能拠点がない島は `Ignored`。
- 収入増加が0の中立島は拡張候補から除外。
- 現在保有する輸送と所有施設で生産可能な輸送の双方を調べ、どの方式でも到達できない島だけを `Ignored` とする。
- 輸送ヘリを生産できない場合は到達可能な最安輸送へフォールバック。
- 敵到着ETAを計算できない場合は直近脅威なしとする。
- staleなSquad entityは既存の `update_squads` で除去。
- 部分編成を新規作戦へ投入しない。
- `Defend` は攻勢優先度最下位の作戦を先に停止する。
- 撤退経路がない部隊は局地作戦を継続する。

## 16. 評価設計

### 16.1 比較評価: V3対V1

```text
maps: map_1, map_2, map_3
subject: V3
opponent: V1
seeds: 58001, 58002, 58003, 58004
orders: V3=P1 / V3=P2
max turns: 30
total: 24 games
```

map_3の合否:

- V3平均収入がV1以上。
- V3平均拠点数がV1以上。
- V3平均ZOCがV1より大きい。
- 初期島外の拠点を取得。
- 侵攻予算未達で新規EnemyHeld侵攻を開始しない。
- 最初の島嶼拡張が原則として輸送ヘリ＋占領要員によるOpenNeutral攻略。
- EnemyHeld侵攻開始時に必要予算を満たす。
- 勝率40%以上。
- 平均思考時間が比較baselineの150%以内。

map_1・map_2は回帰評価に使用する。

### 16.2 行動評価: V3自己対戦

```text
map: map_3
P1: V3
P2: V3
seeds: 58001, 58002, 58003, 58004
1 game per seed
max turns: 30
total: 4 games
```

自己対戦では勝敗を主判定にせず、両プレイヤーの行動を個別に判定する。

- 全島に毎ターン1つの状態が記録される。
- 初期中立島が `OpenNeutral` になる。
- 敵本島より先にROI上位のOpenNeutralを候補にする。
- 同時攻勢が3島を超えない。
- 資金・輸送・ユニットを二重割当しない。
- Contested島ごとに判断理由を記録する。
- 敵排除後は中立拠点が残っていても `Secured` になる。
- Secured島の残存拠点占領を続ける。
- 敵到着ETAが2以下になると `Threatened` へ遷移する。
- Threatened島の防衛を新規攻勢より優先する。
- EnemyHeld侵攻は必要予算を満たした後だけ開始する。
- 両プレイヤーが初期島外拠点を1つ以上取得する。
- 試合エラー、状態欠損、二重割当があればFAIL。

### 16.3 baselineとresult

現在のV3対V2 baselineは診断資料として保存するが、新設計の比較baselineには使用しない。

実装前:

```text
baseline-v3-v1-$SHA.json       24 games
baseline-v3-selfplay-$SHA.json  4 games
```

実装後:

```text
result-v3-v1-$SHA.json
result-v3-selfplay-$SHA.json
```

比較評価と自己対戦評価の両方がPASSした場合だけIssue #58全体をPASSとする。

## 17. 診断テレメトリ

各ターン・各島について次をJSONへ記録する。

- island_id
- state
- decision
- state/decisionの理由
- 中立・自軍・敵拠点数
- 自軍・敵戦闘資産
- 自軍・敵到着ETA
- 自軍・敵占領ETA
- 回収ターン
- 必要予算
- 割当予算
- 割当輸送・占領・戦闘ユニットID
- 作戦継続・増援・撤退の理由

## 18. テスト戦略

### 18.1 純粋関数

- 全 `IslandCampaignState` の境界表。
- 初期状態判定。
- `OpenNeutral → Secured → Threatened → Contested` の遷移。
- 中立島回収ターン。
- 32,700G最低予算。
- 敵戦闘資産×1.2による増額。
- Contest/Reinforce/Withdraw条件。
- 上位3島選択。
- 資金・ユニット二重割当防止。

### 18.2 ECS統合テスト

- 輸送ヘリ＋歩兵で中立島を先に狙う。
- 敵が中立島へ先着した場合に島単位で再評価する。
- 3島を同時攻略し、4島目を待機させる。
- 1島だけ撤退し、他2作戦を継続する。
- Secured島の残存中立拠点を占領する。
- Threatened島の防衛が最下位攻勢より優先される。
- 必要予算未達で敵島へ侵攻しない。
- 敵戦力増加に応じて侵攻予算が増える。
- spawn順・HashMap順に依存しない。

### 18.3 評価テスト

- V3対V1の24試合スケジュール。
- V3自己対戦はseedごとに1試合だけ生成する。
- 自己対戦の両プレイヤーを別々に判定する。
- 状態欠損・二重割当・試合エラーでFAIL。
- baseline/resultのメタデータと比較。

## 19. 完了条件

- 全島が毎ターン1つの状態へ分類される。
- OpenNeutralを敵本島より先に低コストで攻略する。
- 攻勢作戦が最大3島で、完全編成単位で割り当てられる。
- 島ごとにContest/Reinforce/Withdrawを判断する。
- Secured島を継続監視し、Threatened時に防衛へ戻す。
- EnemyHeld侵攻が必要予算未達では始まらない。
- V3対V1比較評価がPASSする。
- V3自己対戦行動評価がPASSする。
- map_1・map_2回帰、勝率、思考時間ガードレールを満たす。
- Python/Rustテスト、clippy、fmtが成功する。
