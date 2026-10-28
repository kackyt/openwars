# HANDOFF: V4 標準マップAIの改善（主戦場の選択・進軍路作戦・砲台輸送）

ブランチ: `claude/dazzling-lovelace-if6j7f`（最終コミット済みコード: `68ca97c`）

## 0. 最初に読むこと

- **作業中のコードはまだコミットしていない。** 全差分を `handoff/v4-route-operations-wip.patch` に保存してある（`68ca97c` に `git apply` で当たることを確認済み）。
  ```sh
  git apply handoff/v4-route-operations-wip.patch
  ```
- 新機能はほぼすべて環境変数で切り替わる実験段階（下の表）。既定の挙動を変えるのは「工場からの早い退避」の変更だけ（4.6）。
- コミット前に必ず消すもの：
  - デバッグ出力（`TACTIC`、`WAITPROBE`、`ROUTE_ORDERS`、`ROUTE_POSTURE*`、`FERRY*`）
  - 実験用の分岐（`OPENWARS_ROUTE_CAPTURE_ALL`、`OPENWARS_ROUTE_ENEMY_BUDGET` など）

  そのうえで `cargo clippy --all-targets --all-features -- -D warnings`、`cargo fmt --all -- --check`、`cargo test` を通すこと。
- 計測結果（`.tmp/sweepNN`）と比較用スクリプトはコンテナ内にしか無く消える。スクリプトだけ `handoff/tools/` に同梱した。

## 1. 目的とユーザーの要求（経緯）

標準マップ（map_20/35/40/47、map_8、小マップ1/2/6/25）でV4 AIを強くする。対戦相手はV200、基準は race7（=`68ca97c`）。ユーザーの主な指示は次のとおり。

- **作戦の目的:** 「全滅」ではなく「拠点を取って保持」を目的に作戦を組む（hold）。
- **map_20後攻のRTA:**
  - 生産:
    - T1に砲台1・歩兵2・補給輸送車1。
    - T3までに砲台3。
    - 砲台は都市から直接輸送車へ乗せる。輸送車は砲台の隣で生産する。
  - 展開: T4に橋の前の都市へ降ろし、輸送車で補給しながら砲撃。
  - 右側: 右上は捨て、砲台と重自走砲で守る。
- **評価ベース:** ロジックは決め打ちせず評価関数で比較する。山を隔てた経路（map_35/40/47）でも有効にする。
- **兵力集中（二兎を追わない）:**
  - 主戦場に集中し、他の経路は最小限で守る。
  - 集中する案と分散する案を立てて評価で選ぶ。評価には戦力差だけでなく、得られる拠点と、首都へのスピード（移動コスト込みのターン数）を入れる。
- **見直しのタイミング:** 主戦場を毎ターン再計算しない。DAGの**地域**（ノード）の状況が変わったときだけ見直す。
- **要衝の確保:**
  - 決めた要衝へ速やかに動く。任務をコロコロ変えない。
  - 戦力が揃うのを待つかどうかは状況次第にする。
  - ターゲットを決めたのに動かない、実現不能、は許されない。

## 2. 環境変数（実験フラグ）

| 変数 | 内容 |
|---|---|
| `OPENWARS_HOLD` | 拠点保持を評価する計画（保持期間内の敵を相手に保持できる案を優先）、進軍路ごとの作戦分割 |
| `OPENWARS_ROUTE_DEFENSE` | 進軍路ごとに攻め/守りを戦力比較で選ぶ（`ROUTE_BUDGET_TURNS=1` と併用） |
| `OPENWARS_ROUTE_BUDGET_TURNS` | 上の比較で自軍に足す収入のターン数 |
| `OPENWARS_ROUTE_ENEMY_BUDGET` | 上の比較に敵収入も足す（map_8が大幅悪化したので不採用） |
| `OPENWARS_ARTILLERY_FERRY` | 補給輸送車で砲台を運ぶ戦術・生産 |
| `OPENWARS_POSITIONAL` | 位置の価値（前衛が砲台を守る、砲台を作戦地点に据える） |
| `OPENWARS_CONCENTRATE` | **主戦場の選択（集中/分散の比較）と進軍路作戦**（今回の中心） |
| `OPENWARS_ROUTE_CAPTURE_ALL` | 進軍路作戦中も占領目標を要衝周りに絞らない（実験。こちらの方が成績が良い） |
| `OPENWARS_GROUND_LIFT` | 装甲車で重歩兵を運ぶ（効果なしのため不採用） |
| `OPENWARS_COMMIT`、`OPENWARS_THEATRE`、`OPENWARS_HOLD_EXCHANGE`、`OPENWARS_ROUTE_ANCHOR` | 過去の実験（いずれも不採用） |
| `*_DEBUG`、`OPENWARS_TACTIC_DEBUG`、`OPENWARS_WAIT_PROBE_PRINT` | デバッグ出力 |

現在の推奨構成（conc13）：
```
OPENWARS_CONCENTRATE=1 OPENWARS_ROUTE_CAPTURE_ALL=1 OPENWARS_HOLD=1 OPENWARS_ROUTE_DEFENSE=1 \
OPENWARS_ROUTE_BUDGET_TURNS=1 OPENWARS_ARTILLERY_FERRY=1 OPENWARS_POSITIONAL=1
```

## 3. 計測結果（V200相手、勝数・拠点差の合計）

標準マップは3シード×先後、map_8は6シード×先後、小マップは2シード×先後。

| マップ | race7 | posA8 (hold+ferry) | conc4 (集中・生産のみ) | **conc13 (進軍路作戦)** |
|---|---|---|---|---|
| map_20 | 5/6 +24 | 5/6 +22 | **6/6** +22 | 5/6 +12 |
| map_35 | 6/6 +48 | 6/6 +88 | 6/6 **+114** | 6/6 +64 |
| map_40 | 6/6 +247 | – | 6/6 +247 | 6/6 **+261** |
| map_47 | 3/6 +12 | – | 3/6 +22 | 3/6 −4 |
| map_8 | **8/12** −30 | 6/12 −52 | 6/12 −76 | 6/12 −76 |
| map_1/2 | 全勝 +50/+34 | – | 同じ | 同じ |
| map_25 | 4/4 +146 | – | 4/4 +156 | 4/4 **+168** |
| map_6 | 4/4 +138 | – | 4/4 **+154** | 4/4 +126 |

- **conc4:** 「主戦場の選択」を生産だけに反映した版。
- **conc13:** 部隊の動きまで作り直した版。部隊の動きは要求どおりになった（下記）が、拠点差はconc4に届かないマップがある。
- **map_20後攻（conc13の挙動）:**
  - T1に左（敵主力側）への集中を選び、右(24,13)は最小限の防衛。
  - 左は攻め(7,10)とし、集結点は橋前の都市(10,18)。
  - 部隊の目標切替は9ターンで48〜54回 → 数回〜十数回（戦闘目標の切替を除く）。
  - 橋前(10,18)はT14まで保持。西の都市(6,17)(2,18)(2,19)は敵の方が近く（到着競争で負け）、T7〜T9に失う。

## 4. 実装の内容（パッチの中身）

### 4.1 拠点保持の計画（`OPENWARS_HOLD`）
- `rolling_plan.rs`:
  - `hold_window` / `hold_contestants`（保持期間内に来る敵）。
  - 保持できる案を優先する beam の並べ替え。
- `combat_input.rs`: CaptureとDefenseの作戦に保持期間を設定する。
- `board_scan.rs` + `force_demand::split_route_front_objectives`: 首都ルートDAGの進軍路ごとに作戦を分割する。

### 4.2 進軍路ごとの攻め/守り（`OPENWARS_ROUTE_DEFENSE`）
- `force_demand::defend_outmatched_routes`: 4ターン以内に届く敵の戦力と、自軍の戦力＋予算の等分を比べる。劣勢な進軍路は、目標に最も近い自軍拠点のDefenseにする。

### 4.3 砲台の輸送（`OPENWARS_ARTILLERY_FERRY`、`OPENWARS_POSITIONAL`）
- `v4/artillery_ferry.rs`:
  - 行動の優先順は 補給 → 射撃 → 降車 → 搭載 → 合流 → 待機。
  - 敵がいなくなったら再搭載して前線へ運ぶ。
- `production_pipeline/artillery_carriers.rs`:
  - 砲台の数に合わせて輸送車を生産し、砲台の隣の施設に置く。
  - 砲台は「作戦地点へ据えるまでの手番」が最短の施設（最前線）で生産する。
  - 施設の付け替えには `AllocationState::move_claim` を使う。
- `rolling_plan.rs`:
  - 砲台を輸送車とセットの値段で評価する。
  - 運搬の手番数を見積もる（`ferried_readiness_turns`）。
  - 補給できる都市数で弾切れを判定する（`artillery_resupply_slots`）。
  - 前衛が生きている間は砲台が攻撃されない（screen）。
- `runtime/turn.rs`: 輸送の戦術を通常行動より先に実行し、砲台と輸送車を工場からの退避対象から外す。

### 4.4 主戦場の選択（`OPENWARS_CONCENTRATE`, `capital_route/force_demand.rs`）
- **`concentrate_route_efforts` / `choose_route_posture` / `posture_score`:**
  - 分散案と、各進軍路への集中案を同じ評価関数で比べる。
  - 戦闘の見積もりはランチェスター二次法則。各部隊は最も早く着ける進軍路1本だけに数える。
  - 集中案では、主戦場以外の経路へまず敵と互角の分を配り、残りを主戦場へ回す。
  - 敵の増援（期間中の敵収入）は、今の敵配置に比例して各経路へ割り振る。
  - 得られる地域・失う地域はDAGノード単位で数え、勝者の残存戦力で頭打ちにする。
  - 首都へのスピードは両方向で見る（攻め込む速さ、攻め込まれる速さ）。
- **`RoutePostureMemory`:**
  - 決めた案、各進軍路の要衝（`RouteOrder`: key_point・攻守・staging・force）、決めたときの地域の保持者を記録する。
  - 地域の保持者（地域の拠点の2/3以上を持つ側）が変わったときだけ見直す。
- **要衝と集結点:**
  - 要衝・集結点は「敵より先に押さえられる最も前の地点」（`seizable_key_point`、移動コスト込みのターン数で敵味方を比較）。
  - 攻め/守りは戦闘の見積もりで決める。
- **戦力節約の作戦（economy of force）:**
  - 主戦場以外の経路は Defense作戦として、保持できる最も安い編成を選ぶ。
  - 使える資金は互角分（`economy_budget`）までに制限する。

### 4.5 進軍路作戦（`squad/route_operations.rs`）
- 所属を `SquadManager::route_operation_members` に記録する。
  - 作戦が続く間は他の割当から引き抜かない。
  - 見直しで要衝が変わっても、同じ進軍路の作戦へ引き継ぐ。
- 手の空いた部隊は、戦力不足の大きい作戦へ加える。
  - 首都の生産圏にいる部隊はどの作戦へも送れる。
  - 前線に出た部隊は遠い戦線へ移さない。
- 進出の判断:
  - 守り: 即座に要衝へ向かう。
  - 攻め: 敵より先に着けるか戦力で上回れば要衝へ進出し、それ以外は集結点で待つ。
- 各作戦の中では、既存の戦闘割当（`objective_allocation::route_operation_responsibilities`）を所属部隊だけで実行する。対象は目標の周り6マス以内の敵に限る。
- 占領目標は要衝周りと中立に限定する。ただしこの限定は成績を大きく落とした（map_35 −28）ため、`OPENWARS_ROUTE_CAPTURE_ALL` で外せるようにした。

### 4.6 工場からの早い退避（既定で有効、V4標準のみ）
- `tactics/relief.rs::decide_early_campaign_site_relief`: 任務目標から遠ざかる退避はせず、後の退避（通常行動の後）に回す。
- 以前は新しく生産した部隊が逆方向へ一歩動き、1ターン損していた。

## 5. これからやること（優先順）

1. **conc13 と conc4 の差の原因を特定する。** 同じシードで対局を並べ、どこで差がつくかを見る（特に map_35、map_20、map_47）。候補は次のとおり。
   - 攻めの進出条件（所属戦力 > 要衝周り2ターン以内の敵）が厳しすぎ、集結点で待ち続けている。
   - 所属を固定した副作用で、戦力の付け替えが遅い。
   - 占領目標の扱い。`CAPTURE_ALL` を既定にするか、「主戦場の地域＋中立＋安全な敵拠点」などに作り直すか。
2. **生産を要衝に合わせる。** 主戦場の守りで、橋前へ砲台を多めに送り集中砲火する（RTAの形）。今は砲台が1〜2基しか出ない。
   - 一因として、保持判定の期間（2ターン）が砲台の展開（生産から射撃まで4〜5ターン）より短い可能性がある。
   - 補給輸送車も砲台に補給できる数として数えるべき。
3. **T1の砲台生産が間に合っていない（RTAはT1、AIはT2）。** 計画が敵の今後の生産を見込んでいないため。生産計画のシミュレーションに、敵の収入ベースの増援を入れる案（`production_model::ReinforcementAssessment` が既にある）。
4. **map_40やmap_8で、2つの「進軍路」の目標が隣接セルになる。** 実質1つの前線なので、比較対象から外す（目標が数マス以内なら同じ前線として扱う）。
5. **map_47後攻のように全案が大差で負けている局面** では、主戦場の選択がノイズになる。劣勢時の扱いを決める（首都に近い経路の防衛に絞る等）。
6. **map_8 はどの版も race7（8/12）に届かない（6/12）。** hold 系の変更全体の影響。
7. **仕上げ。** 採用する機能を既定にするか決め、不要な実験フラグとデバッグ出力を削除する。テストを追加する。主に `route_operations` と `seizable_key_point`。既存は `posture_tests` 7件、`move_claim`、artillery_ferry 11件、ground_lift 6件。clippy、fmt、test を通してコミットする。

## 6. 計測・調査の手順

- **対戦スイープ:** バイナリは `cargo build --release -p mcp-server` で作り、`target/release/mcp-server` をコピーして使う。
  ```sh
  python3 scripts/run_matchup_sweep.py --variant NAME=BIN --env NAME:OPENWARS_X=1 ... \
    --map map_20,map_35,map_40,map_47 --seeds 1,2,3 --jobs 4 --out-dir .tmp/sweepNN
  ```
- **比較:** `.tmp/cmpX/games/<variant>` に各スイープの `games/<variant>` へのsymlinkを並べる。
  ```sh
  python3 handoff/tools/sweep_cmp.py .tmp/cmpX
  ```
- **1局の要約:**
  - `handoff/tools/story.py <game.json>`: 拠点・生産・輸送・重要拠点の推移。
  - `handoff/tools/plans.py <game.json> 2`: 作戦と計画の推移。
- **エンジン直結のprobe:**
  ```sh
  cp handoff/tools/zz_map20_probe_gl.rs engine/tests/
  OPENWARS_...=1 PROBE_HEX=1 PROBE_MAP=map_20 PROBE_TURNS=10 PROBE_ACTIONS=1 \
    cargo test --release -p engine --test zz_map20_probe_gl -- --ignored --nocapture
  ```
  - V4先攻にするときは `PROBE_V4_FIRST=1` を付ける。終わったら `engine/tests/` から消す。
  - `OPENWARS_TACTIC_DEBUG=1` で出したログは `handoff/tools/track.py` で、部隊ごとの目標と位置の推移に集計できる。
  - `OPENWARS_CONCENTRATE_DEBUG=1` で出したログは `handoff/tools/summarize_posture.py <log> <player>` で、配分案の再計算の記録に集計できる。
- **注意:**
  - `pkill -f` は自分のシェルに一致すると自分を殺す。プロセスは `/proc` を走査して止める。
  - ディスクが一杯になるとスイープが壊れる。古い `.tmp/sweepNN` は消す。
  - 待機ループで `pgrep -f` を使うと、自分自身のコマンド行に一致して終わらないことがある。

## 7. 不採用にした実験（コンテナのstashのみ。再現は不要）
- 被害を受けた高価ユニットの修理後退
- 価値重み付けのベストエフォート順位
- 収入超過ユニット数の制限
- 自拠点防衛の段階化
- 山越えの前線
- 突破順位
- 脅威ベースの目標
- 装甲車での重歩兵輸送（`OPENWARS_GROUND_LIFT`、パッチには含まれる）
- コミットメント（`OPENWARS_COMMIT`）
- 戦域の敵追加（`OPENWARS_THEATRE`）
- 攻守判定への敵収入の追加
