# V4: feature/regional-control-20261101 を土台にした再実装計画

> ステータス: 計画のみ・未着手（2026-11-03 作成）。
> 作業ブランチ: `feature/regional-control-rebuild-20261104`（基点 `9d9853c` = `feature/regional-control-20261101`）。
> 最初のコミット `ceb11ea` で AI 挙動のゴールデン比較テスト（`engine/tests/ai_multi_turn_baseline.rs`, `engine/tests/golden/`）を削除済み。

## 背景

- `claude/dazzling-lovelace-if6j7f`（`460e3d2`）から `feature/cooperative-ordering-20261103`（`91bb374`）までの1日分の改修で、map_20 両手番・map_47 先手の改善を狙ったが、標準サイズマップの戦績が全体に下がった。
- `460e3d2` には CODEX_IMPLEMENTATION_PLAN.md の実装 `8212f4a` が含まれる。履歴は `9d9853c` → `9d689ca`（計画書のみ）→ `8212f4a`（CODEX 実装）→ `460e3d2`（検証記録のみ）→ 1日分の改修。
- ユーザー評価: 砲台の生産・移動・敵ユニットの駆逐は、CODEX 実装前の `9d9853c` の方が良かった。
- `docs/verification/2026-11-01-codex/` の検証は、CODEX の計画外部分（地域ノード化）と停滞原因の診断が対象。CODEX の Task 1〜5 が狙った指標（過剰生産・乗降の往復・待機渋滞）は測られていない。
- 同 `t18-t22-unit-diagnosis.md` が特定した「目標から一時的に離れる移動を候補から外し、現在地待機を繰り返す」判定（`engine/src/ai/tactics/v2.rs` の `wait_move_away_from_campaign`）は、1日分の改修のどれも手を入れていない。

## 方針

- 土台は `9d9853c`。CODEX と1日分の改修からは、効果を確かめたものだけを移植する。元のコミットはすべて既存ブランチに残るので、移植しないものも失われない。
- 不具合修正（A群）は、勝敗ではなく「直したい現象が消えたか」で1件ずつ判定する。対戦はまとめて1回にし、`9d9853c` より明確に悪くならないことだけを見る。
- 勝敗で1件ずつ比べるのは、戦略に効く変更（B群）だけにする。
- フェーズ2の目的は、劣化させずに土台をきれいにすること。成績の伸びは B群とフェーズ3で狙う。
- 禁止事項: 評価値ハック、特定マップ・座標の分岐、根拠のない閾値、関数内の `is_extended` 的な分岐、補正定数でのごまかし。
- 対戦は V4 対 V200・Hex・60T。異なるシードを各1局（基本はシード 1〜4）、`--jobs 8`。同一シードは繰り返さない。
- 比較用のバイナリは、別名のコピーで固定する（`target/release` を直接渡さない）。
- コミットはユーザーの承認後に行う。

## 評価の対象

- 重点: map_20 両手番、map_47 先手（map_47 後手は対象外）
- 回帰: 仮に map_6・map_14・map_25・map_40。CP0 でユーザーが確定する。

## 指標

`scripts/summarize_artillery_logistics.py`（フェーズ1で新設）で、対局ごと・変種ごとに集計する。

- 兵種別の生産数（砲台・補給輸送車・戦車系）
- 砲台の攻撃数と撃破数
- 同じマスでの Load と Drop の往復（2手番以内）の回数
- 自軍手番の Wait の数と、位置を変えなかった直接部隊の数
- T30・T60 の所有拠点
- 敵首都から前線までの距離
- 勝敗と決着ターン

「悪化」の判定: 重点マップでシードの過半数が同じ向きに悪い（所有拠点・決着ターン・勝敗）、かつ砲台の指標か前線の距離が `9d9853c` より明確に悪い。片方だけなら誤差とする。具体的な線は CP0 で、base のシードごとの揺れを見て決める。

---

## フェーズ1: 測る仕組みと基準

1. **比較用バイナリを固定する**
   - `git worktree` で `9d9853c`（base）・`460e3d2`（codex）・`91bb374`（head）を release ビルドし、`.tmp/rebuild/bin/{base,codex,head}/mcp-server.exe` へコピーする。
   - 確認: 3つの exe が存在し、各 worktree の `git rev-parse HEAD` が狙いのコミットと一致する。
2. **対局 JSON の項目を確認する**
   - `action_counts`（生産の内訳）と、手番ごとの行動（兵種・位置・Load/Drop/Attack/Wait・撃破）の項目名を書き出す。
3. **指標の集計スクリプトを作る**
   - `scripts/summarize_artillery_logistics.py` と `scripts/test_summarize_artillery_logistics.py`。
   - 確認: `python -X utf8 -m pytest scripts/test_summarize_artillery_logistics.py`。既存の対局 JSON 1件を手で数えた値と一致する。
4. **基準の対戦**
   - `python -X utf8 scripts/run_matchup_sweep.py --variant base=... --variant codex=... --variant head=... --map <マップ> --p1 V4 --p2 V200 --player-order both --seeds 1,2,3,4 --max-turns 60 --jobs 8 --out-dir .tmp/rebuild/sweep0`（マップごと、バックグラウンド実行。map_47 は V4 先手のみ）。
   - 確認: 全対局が `results.jsonl` に揃い、3変種の比較表が出る。

**→ CP0**

5. **CODEX のどの Task が砲台の成績を下げたかを切り分ける**
   - codex が base より悪い指標について、関係する Task だけを外した変種を作り、重点マップで測る（生産数 → Task 2・4・輸送車の割当、乗降の往復 → Task 3）。
   - 確認: 採用する Task と採用しない Task の一覧を、根拠の数値付きで作る。

**→ CP1**

## フェーズ2: A群（不具合修正）の移植

各項目の検証:
- `cargo test -p engine --lib`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo fmt --all -- --check`
- 直したい現象が消えたことを、単体テストか手番の記録で確認する
- 移植前に、`9d9853c` に同じ関数・構造があるかを `git show 9d9853c:<path>` で確認する。無ければ見送る

| # | 項目 | 出所 | 主なファイル | 現象の確認 |
|---|---|---|---|---|
| A1 | 武器も占領能力も無い車両を、戦闘小隊・前衛枠・局地戦力に数えない | 8212f4a（CODEX Task 1） | `squad/formation.rs`、`capital_route/commitment.rs`（`ground_route_member`）、`capital_route/node_operation.rs` | 補給輸送車が Attack/Defense 小隊に入った回数が0 |
| A2 | 降車済み・待機中・進軍路所属の砲台を既存戦力として数える。再接続で進軍路の所属を保つ（CP1 で採用した場合のみ） | 8212f4a（CODEX Task 2） | `v4/mission_input.rs`、`v4/production_pipeline/mod.rs`、`squad.rs` | 砲台の生産数が base より増えない |
| A3 | 出撃判定を、直線距離÷移動力から地形・実燃料の到達計算にする。`fire_arrival_turns` を追加する。射撃位置の任務を `Defense` 固定から作戦の任務の継承にする | 9be87db、f7abade | `squad/route_operations.rs`、`v4/property_control.rs` | 入り江・海越しの到達誤判定を固定盤面のテストで確認 |
| A4 | `beyond_defended_route` を地形距離にする。地形コストの探索は手番ごとに1回でキャッシュし、最寄りの要衝の選択と半径の判定の距離基準をそろえる | 8212f4a（CODEX Task 5 の修正版） | `squad/objective_allocation.rs` | 入り江越しのテストを追加。思考時間が base から大きく増えない |
| A5 | DAG の区間: 枝の途中の施設が尽きたら首都を終点にする（`.or(Some(goal))`）、担当方面の入口を必ず経由させる（`via_front`）、経路が通る地域を区間に含める | a1206a9 ほか | `capital_route/commitment.rs` | `9d9853c` に地域（`control_area`）が無ければ「地域を含める」は見送る |
| A6 | CODEX Task 4（降車した手番は撃てないので射撃開始を1手番遅らせる、占領を守られる歩兵を盾として二重に数えない）と Task 3（砲台輸送の段の分割）。CP1 で採用した場合のみ、1件ずつ | 8212f4a | `v4/rolling_plan.rs`、`v4/artillery_ferry.rs`、`runtime/turn.rs` | 乗降の往復・砲台の撃破数が base を下回らない |

A群をまとめて入れた版を、重点マップと回帰マップで base と対戦させる。

悪化した場合:
1. A群を独立した5つの塊（A1／A2+A6／A3／A4／A5）に分け、半分ずつ外して原因を絞る。
2. 原因の項目について次のどちらかを判断する。
   - (a) 修正そのものが誤っている → 実装を直して測り直す。
   - (b) 修正は正しいが、他の判断がその不具合を前提に釣り合っていた → 同じシードの base と候補の対局で、行動が最初に分かれた手番を探す（`scripts/compare_action_histories.py`、`scripts/run_branch_sim.py`）。寄りかかっていた側を正しく直せる見込みがあれば直す。無ければその項目を外し、分析を保留の一覧に残す。正しい修正でも、悪化したまま残さない。
3. 悪化しない組み合わせの範囲だけを採用する。それも無ければ土台は `9d9853c` のままにする。

**→ CP2**

## B群（戦略に効く変更）の評価

候補ごとに別のバイナリをビルドし、「A群まで入れた版」と重点マップで比べる。改善した候補だけ、回帰マップでも1回比べる。関数の中に試験用の切り替えは入れない。上から順に評価する。

| 優先 | 候補 | 出所 | 前提・注意 |
|---|---|---|---|
| 1 | 交戦中の部隊を、守りの余剰として別の進軍路へ引き抜かない | 3058aa4 | 当日いちばん成績の良かった版。変更が小さく単独で載せられる。「交戦中」は進軍路を問わず敵との距離6以内という判定 |
| 2 | 待機中の攻撃役の移動候補に、安全に届く敵の生産口を加える（生産の封鎖） | 8212f4a（`tactics/v2.rs`） | 「決着を決めるのは敵生産マスの封鎖数」という過去の分析と一致 |
| 3 | force_demand の到達時間を、地形・移動力・射程・燃料で実測する | 9be87db | 全マップの攻守配分が変わる。回帰マップ全体で測る |
| 4 | 保持地点への後方の脅威に守備役を残し、残りは前進する（`route_operations/guard.rs`） | 9be87db | 先に不具合を直す: 脅威の到達計算に実燃料ではなく `max_fuel` を渡している。対応できない脅威があると全員が保持に回る点も確認する |
| 5 | 拠点の所有が変わったら、同じ手番の中で進軍路を更新する。首都の生産範囲に地上部隊が届いたら、周辺拠点の占領を首都作戦の前提にしない | 8212f4a（`runtime/turn.rs`、`victory_roadmap`） | 首都進軍の段階で効く。重点マップで測る |
| 6 | 方面の継承（`squad/route_partition.rs` と carried_routes） | 9be87db | 約200行の基盤が一式で必要。1〜5 の後に判断する |
| 保留 | 砲台輸送の変更（降車制約の撤去、陣地を射程内の敵数で比べる、輸送の要否の判定、輸送車の割当の順序） | 91bb374、8212f4a | CP0・CP1 で砲台側に具体的な問題が見つかった場合だけ扱う。陣地の比較で「同数なら都市を優先」はユーザーが否定済み |
| 保留 | 地域の塗り分け（重なりなし・半径3） | daa39ff、0fec4c9 | `9d9853c` に地域 Area が無ければ前提が成り立たない。A5 の確認結果で判断する |

**→ 3件評価するごとに CP3**

## フェーズ3: 停滞原因の修正

- `t18-t22-unit-diagnosis.md` の再現（map_20 先手 T18〜T22、Entity 4294967442）を固定盤面にし、`wait_move_away_from_campaign` が前方の占有時に合法な迂回・一時後退を候補から外して待機を繰り返す問題を直す。
- スコア定数の追加や、後退拒否の全解除で済ませない。合法な迂回・味方の退避・射撃の機会を区別する。

**→ CP4**

## 移植しないもの

- 地域ノード化と局地優勢による出口の解放（自軍戦力×4 ≥ 敵戦力×5、所有の過半、2手番の継続という閾値）。`f7abade` の地域制圧の試作一式（`tactics/v2.rs` の `regional_control`、DAG ゲートの緩和、`tactical.rs` の戦術目標の書き換えと地域キャッシュ、砲台の地域標的）
- `v4/fire_follow_order.rs`（撃破→進入の順序層）。対象の局面がまれでほぼ発火せず、通常行動より先に割り込む副作用が未計測。`2026-11-01-codex/fire-follow-diagnosis.md` は、課題が順序ではなく前進後の維持であることを示している
- 橋の改修（`tactics/chokepoint.rs`、半径2、間接火力だけの小隊の待機、橋の前衛枠）
- `squad/route_operations/city_defense.rs`
- `91bb374` の砲台輸送の書き換えと `repair_cities`
- 砲台を一本道に据えない規則（`is_separating_cell`）。ユーザーが否定済み

---

## チェックポイント

各チェックポイントで必ず一度止め、ユーザーの判断を受ける。予算を使い切ったら、途中でも結論を出して先へ進む。

| CP | タイミング | 予算 | 終了の条件 | ユーザーが判断すること |
|---|---|---|---|---|
| CP0 | フェーズ1の手順1〜4の後 | 基準対戦1回（3変種 × 重点マップ＋回帰マップ、4シード） | 比較表が出た時点 | 回帰マップの組、「悪化」の線、base の砲台成績が本当に良いかの確認 |
| CP1 | 手順5の後 | 重点マップの対戦を最大2回 | 採用・不採用の Task が決まるか、2回を使い切った時点。使い切ったら、悪化に関わる Task はすべて不採用 | A群に入れる CODEX の Task |
| CP2 | A群の対戦の後 | 対戦1回＋原因の絞り込み最大3回。原因の分析（(b) のケース）は1項目あたり1日分の作業まで | 悪化しなければそのまま通過。悪化したら、3回以内に絞れた項目を外して通過。分析が予算を超えたら保留の一覧へ | 採用版を土台にするか、`9d9853c` のままにするか |
| CP3 | B群を3件評価するごと | 候補1件につき対戦1回（重点マップ）。採用の候補だけ回帰マップで1回追加 | 3件の採否が決まった時点 | 続けるか、フェーズ3へ移るか |
| CP4 | フェーズ3の後 | 固定盤面での再現と修正、対戦2回まで | 停滞していた部隊が動くことを手番の記録で確認でき、成績が悪化しなければ採用 | 全体の打ち切り、またはマージ |

全体の打ち切り条件:
- B群は優先順で最大6件まで。保留の2件は、CP0・CP1 で砲台側の具体的な問題が見つかった場合だけ扱う。
- 良くならなかったものは保留の一覧へ送って先へ進む。同じ候補を「もう一回だけ」試すのは、ユーザーの判断があった場合に限る。
- 各チェックポイントで、採用したもの・保留にしたもの（理由付き）・使った対戦回数を報告し、HANDOFF.md に追記する。

## ロールバック

- 移植は1項目1コミットにする。悪化したものは `git revert <commit>` で個別に戻せる。
- `9d9853c` と既存のブランチには手を入れない。比較用のバイナリは `.tmp/rebuild/bin/` に固定し、いつでも基準と比べ直せるようにする。

## 参照

以下の文書は土台の `9d9853c` には無い。`git show <ブランチ>:<path>` で読む（CODEX 関連は `claude/dazzling-lovelace-if6j7f`、それ以外は `feature/cooperative-ordering-20261103`）。

- 1日分の改修の経緯: `feature/cooperative-ordering-20261103` の `HANDOFF.md`、`DEEPSEEK_IMPLEMENTATION_PLAN.md`、`docs/verification/2026-11-02-deepseek-plan/fire-follow-order-review.md`
- CODEX の計画と検証: `CODEX_IMPLEMENTATION_PLAN.md`、`docs/verification/2026-11-01-codex/`、`docs/verification/2026-11-02-codex/regional-execution-review.md`
- 評価ツール: `scripts/eval_matchup.py`、`scripts/run_matchup_sweep.py`、`.rulesync/skills/openwars-ai-improvement-cycle/scripts/compare_benchmarks.py`
