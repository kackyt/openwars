## 概要

#54 および #56 により敵島への侵攻と交戦が成立した後も、V3 が map_3 で拠点数・収入・勝敗の優位へ結び付けられない問題を分析し、改善する。

本 Issue は基本的な輸送・上陸機能を実装する Issue ではない。侵攻成立後の戦力運用、生産構成、占領スループット、島嶼支配の拡大、終盤の決着力を扱う。

### 現在のフェーズ

- Phase 1（評価基盤・baseline・真因分析）: 完了
- Phase 2（AI 実装・result 取得）: 未着手

Phase 1 の結果、当初の「敵本島への単一侵攻目標を改善する」方針を破棄し、全島を毎ターン評価する島嶼キャンペーン・ポートフォリオ方式へ設計を変更した。設計書は `docs/superpowers/specs/2026-07-28-issue-58-island-campaign-portfolio-design.md`。

## 前提条件

本 Issue の改善実装へ着手する前に、以下を満たすこと。

- [x] #54 が完了している。
- [x] #56 が完了している。
- [x] 敵島への上陸と交戦が決定的な統合テストで確認できる。
- [x] 評価スクリプトで固定 seed を指定できる（`scripts/eval_matchup.py --seeds`）。
- [x] T30 の測定時点が両陣営で公平になるよう定義されている（P1・P2 の両方が当該ターンの行動を完了した後に測定）。

## Phase 1 診断結果

### baseline

- 対象コミット: `e6e462ecac727016b3133f61f72533fa52c3eb8d`（working tree dirty: 評価基盤・診断テレメトリの未コミット変更を含む）
- 構成: 3マップ × 4 seed（58001-58004）× 2手番 = 24試合、`--max-turns 30`
- 実行コマンド:

```text
scripts/eval_matchup.py --mode batch --map map_1,map_2,map_3 --p1 V3 --p2 V2 \
  --criteria issue58 --seeds 58001,58002,58003,58004 --max-turns 30 \
  --output benchmarks/issue-58/baseline-e6e462e.md \
  --json-output benchmarks/issue-58/baseline-e6e462e.json
```

- 成果物: `benchmarks/issue-58/baseline-e6e462e.{json,md}`、`benchmarks/issue-58/seeds.txt`、`benchmarks/issue-58/analysis.md`
- 試合エラー 0 件、手番バケット欠損 0 件、メトリクス欠損 0 件

| Map | 手番 | ZOC (V3 / V2) | 収入 (V3 / V2) | 拠点 (V3 / V2) | 判定 |
| --- | --- | --- | --- | --- | --- |
| map_1 | 先攻 | 28.5 / 40.25 | 8750 / 15250 | 5.75 / 12.25 | FAIL |
| map_1 | 後攻 | 39.5 / 28.5 | 11750 / 12250 | 8.75 / 9.25 | FAIL |
| map_2 | 先攻 | 62.5 / 13.75 | 20000 / 9000 | 14.25 / 4.75 | PASS |
| map_2 | 後攻 | 55.5 / 20.75 | 16750 / 12250 | 11.75 / 7.25 | PASS |
| map_3 | 先攻 | 55.5 / 80.75 | 23250 / 26500 | 14.25 / 16.75 | FAIL |
| map_3 | 後攻 | 68.75 / 106.75 | 24000 / 36750 | 15.0 / 24.25 | FAIL |

map_3 勝率 0/8（0.0%）、平均思考時間 2200.0 ms（95パーセンタイル 3712.1 ms）。

### 既知制約: プロセス間の決定的再現性は未達

`GameRng` の seed は固定されるが、`engine/src/ai/production.rs:152-193` と `engine/src/ai/production.rs:231-303` が `UnitRegistry` の `HashMap` 反復順をそのまま走査し、同スコア候補を「先に現れた方」で採用する。Rust の `HashMap` 反復順はプロセスごとに変わるため、同一 seed・同一コミットでも生産選択が分岐しうる（map_3 / seed 58001 / V3=P1 の再実行で T1 に「重歩兵」と「軽歩兵」へ分岐することを確認）。

本 baseline の数値は「同一プロセス内で取得した 4 seed × 両手番の実測値」であり、プロセス間再実行の完全一致は保証しない。決定的 tie-break の導入は Phase 2 の作業に含める。

### 仮説の判定

| 仮説 | 判定 | 根拠 |
| --- | --- | --- |
| 占領要員が不足している | rejected | 上陸占領要員は8試合すべてで1-3体存在。占領要員投資は 98,000-124,000G、輸送容量 3-15 で全試合 Drop 成立。 |
| 上陸後に部隊が分散する（占領へ移行しない） | confirmed | 8試合すべてで敵島の占領完了 0 件。占領開始は 58001先攻（T13）と 58003後攻（T14）の 2 件のみで、いずれも完了しない。 |
| Battleship が過剰投資である | rejected | 戦艦生産は8試合中1試合のみ（58001後攻、30,000G）。同試合の与ダメージ価値 8,824、ROI 0.294 で全ユニット中最大。高コスト投資の中心は戦艦ではなく砲台・ロケットランチャー（総投資の約78-81%）。 |
| 護衛戦力が不足している（上陸要員が生存しない） | confirmed | 上陸占領要員の生存率は8試合中7試合で 0.0、最大 0.333。初回交戦は Drop と同一ターンまたは翌ターン。 |
| 敵首都を狙わない | inconclusive | 上陸要員から敵首都への最小距離 6-10 で縮小が観測されないが、要員が早期に全滅するため「狙わない」のか「到達前に排除される」のかを分離できない。squad ごとの目標拠点テレメトリが欠落。 |
| 後攻の立ち上がりが遅い | confirmed | 後攻は T1 に装甲車（`max_cargo=1` だが海上侵攻不可）を生産して輸送需要を誤充足し、実際の輸送ヘリ・輸送船生産が T5-T7、first_load T7-T9、first_drop T12-T18 まで遅れる（先攻は T1 / T3 / T8）。 |

### 設計方針の変更

診断で確認された 3 原因に個別対処するのではなく、戦略層そのものを組み替える。理由は以下のとおり。

- 島嶼マップで序盤から敵本島へ攻め込むこと自体が不利であり、敵のいない中立島を先に取るべきである。
- 収入の少ない序盤に装甲車を輸送船で運ぶのはコストが見合わない。輸送ヘリ（4,000G）＋軽歩兵2体（2,000G）＝6,000G の低コスト編成で収入基盤を確保するほうが有利である。
- 単一 `invasion_target` では中立島の取り合い、増援、撤退、確保済み島の再防衛を表現できない。

そのため、全島を毎ターン評価して最大3島の攻勢作戦を管理する島嶼キャンペーン・ポートフォリオを導入する。

## 目的

以下の段階を改善する。

```text
島嶼ポートフォリオ評価
  → 中立島の低コスト確保（輸送ヘリ＋占領要員）
  → 収入増加
  → 侵攻編成（最低32,700G、敵戦力×1.2で増額）の充足
  → 敵島侵攻
  → 敵戦力・敵首都への圧力
  → 勝利
```

#56 は上陸・交戦までを担当し、本 Issue は戦略層（島の選択・優先順位・予算判断）と交戦以降を担当する。

## 設計要旨

詳細は設計書 `docs/superpowers/specs/2026-07-28-issue-58-island-campaign-portfolio-design.md` を正とする。要点のみ以下に示す。

### 島状態（各島は常に1つだけ保持する）

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

- `Ignored`: 占領可能拠点がない、またはどの輸送方式でも到達不能。
- `OpenNeutral`: `neutral_properties > 0` かつ自軍・敵の拠点も units も 0。
- `Secured`: `enemy_units == 0` かつ自軍の足場あり かつ `enemy_arrival_eta > 2` または `None`。全拠点占領は不要。
- `Threatened`: `enemy_units == 0` かつ自軍の足場あり かつ `enemy_arrival_eta <= 2`。
- `Contested`: `friendly_units > 0` かつ `enemy_units > 0`。
- `EnemyHeld`: `friendly_units == 0` かつ（`enemy_units > 0` または `enemy_properties > 0`）。

判定順序は `Ignored → Contested → Threatened → OpenNeutral → Secured → EnemyHeld`。状態変数を複数組み合わせない（`Secured + Threatened` のような複合状態を作らない）。

### 作戦判断

```rust
pub enum IslandCampaignDecision {
    Observe, Expand, Secure, Defend, Contest, Reinforce, Withdraw, Assault,
}
```

状態ではなく毎ターン導出する行動方針であり、盤面から再構築する。新しい永続ライフサイクル状態は追加しない。

### 中立島の投資回収評価

最小拡張編成は輸送ヘリ1体（4,000G）＋占領可能ユニット2体（軽歩兵 2,000G）＝ 6,000G。

```text
payback_turns = transport_eta + capture_turns
              + ceil(missing_package_cost / island_income_per_turn)
```

値が小さい島を優先する。`island_income_per_turn == 0` の島は候補から除外する。

### 敵島侵攻の予算

```text
固定輸送・占領費 = 輸送船 16,500G + 輸送ヘリ 4,000G + 軽歩兵2体 2,000G = 22,500G
required_combat_budget = max(10,200G, ceil(target_island_enemy_combat_value * 1.2))
required_assault_budget = 22,500G + required_combat_budget
```

最低 32,700G。敵戦闘資産が 8,500G を超えると最低戦闘費より敵資産×1.2 が大きくなる。予算を満たす島だけ `Assault` とし、複数島へ同じユニット・資金を二重計上しない。

### ポートフォリオ制約

- 攻勢作戦（`Expand` / `Contest` / `Reinforce` / `Assault`）は最大3島。
- 1島ごとに最低1つの完全編成を保証する。断片的な割当（歩兵だけ、輸送だけ）はしない。
- `Secured` 島は攻勢上限に数えないが、毎ターン脅威評価を継続する。
- `Threatened` が発生したら攻勢優先度最下位の作戦を一時停止して `Defend` を優先する。
- 装甲車の `max_cargo` は海を越える輸送需要へ計上しない。

## 評価プロトコル

V2 は島嶼侵攻を積極的に行わないため、中立島の取り合い・撤退・増援・確保済み島の再防衛を評価できない。比較対象を V1 へ変更し、行動評価として V3 自己対戦を併用する。

### 比較評価: V3 対 V1

```text
maps:   map_1, map_2, map_3
subject: V3
opponent: V1
seeds:  58001, 58002, 58003, 58004
orders: V3=P1 / V3=P2
max turns: 30
total: 24 games
```

map_3 の合否条件:

- V3 平均収入が V1 以上。
- V3 平均拠点数が V1 以上。
- V3 平均 ZOC が V1 より大きい。
- 初期島外の拠点を取得。
- 侵攻予算未達で新規 `EnemyHeld` 侵攻を開始しない。
- 最初の島嶼拡張が原則として輸送ヘリ＋占領要員による `OpenNeutral` 攻略である。
- `EnemyHeld` 侵攻開始時に必要予算を満たす。
- 勝率 40% 以上。
- 平均思考時間が比較 baseline の 150% 以内。

map_1・map_2 は回帰評価に使用する。

### 行動評価: V3 自己対戦

```text
map: map_3
P1: V3 / P2: V3
seeds: 58001, 58002, 58003, 58004（1 game per seed）
max turns: 30
total: 4 games
```

自己対戦では勝敗を主判定にせず、両プレイヤーの行動を個別に判定する。

- 全島に毎ターン1つの状態が記録される。
- 初期中立島が `OpenNeutral` になる。
- 敵本島より先に ROI 上位の `OpenNeutral` を候補にする。
- 同時攻勢が3島を超えない。
- 資金・輸送・ユニットを二重割当しない。
- `Contested` 島ごとに判断理由を記録する。
- 敵排除後は中立拠点が残っていても `Secured` になる。
- `Secured` 島の残存拠点占領を続ける。
- 敵到着 ETA が2以下になると `Threatened` へ遷移する。
- `Threatened` 島の防衛を新規攻勢より優先する。
- `EnemyHeld` 侵攻は必要予算を満たした後だけ開始する。
- 両プレイヤーが初期島外拠点を1つ以上取得する。
- 試合エラー、状態欠損、二重割当があれば FAIL。

### 成果物

既定の `matchup_report.md` に上書きしない。

```text
benchmarks/issue-58/
  seeds.txt
  baseline-e6e462e.{json,md}          # V3 対 V2 診断用（保存のみ、新設計の比較基準には使わない）
  analysis.md
  baseline-v3-v1-<SHA>.{json,md}      # 24 games
  baseline-v3-selfplay-<SHA>.{json,md} #  4 games
  result-v3-v1-<SHA>.{json,md}
  result-v3-selfplay-<SHA>.{json,md}
```

各成果物には対象コミット SHA、実行コマンド、seed 一覧、各手番の試行数、評価スクリプトの SHA-256 を記録する。

比較評価と自己対戦評価の両方が PASS した場合だけ Issue 全体を PASS とする。

## 指標定義

### 基準1: ZOC

V3 の平均 ZOC 支配面積が比較相手を上回ること。

### 基準2: 収入

V3 の平均ターン収入が比較相手以上であること（`V3 >= 相手` を PASS として扱う）。

### 基準3: ジリ貧防止

T15 以降の5ターン移動平均について、V3 のユニット資産価値とターン収入が基準点から低下し続けないこと。

### 基準4: 拠点獲得

V3 が自軍初期島の拠点数を超えて拠点を獲得すること。単に自軍初期島の拠点を確保した状態を「停滞」と呼ぶのではなく、中立島または敵島への上陸と占領が行われたかで判定する。

### 基準5: 島嶼行動の妥当性

自己対戦で、状態分類・優先順位・同時攻勢数・予算充足・二重割当防止が設計どおりであること。

## スコープ

- 島嶼キャンペーン・ポートフォリオの導入（全島評価、状態分類、作戦判断）。
- 中立島優先の低コスト拡張（輸送ヘリ＋占領要員）。
- `Contested` 島の継続・増援・撤退判断。
- `Secured` 島の継続管理と `Threatened` 時の防衛復帰。
- 敵島侵攻の予算ゲート（最低 32,700G、敵戦力×1.2）。
- 生産優先順位のポートフォリオ連動。
- 装甲車が海上輸送需要を誤充足する問題の解消。
- 生産選択の決定的 tie-break（`HashMap` 反復順への依存排除）。
- 島単位の診断テレメトリ追加。
- map_1・map_2 の回帰確認。

## スコープ外

- V1 輸送 AI の変更。
- 基本的な Load・Transit・Drop の実装（#56）。
- `Squad.transport_cargo` の複数対応（#56）。
- 上陸地点選定の基本機能（#56）。
- `is_engaged` の島分離（#56）。
- map_3 の座標、手番、固定 seed に依存する分岐。
- 敵首都への固定突撃。
- 無制限の同時攻略。
- GUI 変更。

## 依存・関連

- 前提 Issue: #54、#56
- 関連 Issue: #48、#51、#53、#55、#72、#80

## 参照文書

- `docs/superpowers/specs/2026-07-28-issue-58-island-campaign-portfolio-design.md`（本 Issue の設計の正）
- `docs/MASTER.md`
- `docs/02-design/ARCHITECTURE.md`
- `docs/02-design/DOMAIN.md`
- `docs/architecture/ai_design.md`
- `docs/03-implementation/PATTERNS.md`
- `docs/04-quality/TESTING.md`

## 受け入れ条件

### Phase 1: 評価基盤・診断

- [x] 固定 seed を指定できる評価経路が存在すること。
- [x] 比較対象コミット、実行コマンド、seed、試行数を記録した baseline が保存されていること。
- [x] T30 が両陣営の30ターン目完了後として測定されること。
- [x] Battleship 過投資仮説について、投資額・与ダメージ価値・勝敗との関係を記録すること。
- [x] 真因と確認されなかった仮説を明記すること。
- [x] 分析結果を `benchmarks/issue-58/analysis.md` に記録すること。

### Phase 2: 島嶼キャンペーン実装

- [ ] 全島が毎ターン1つの `IslandCampaignState` へ分類されること。複合状態を持たないこと。
- [ ] 状態判定が `Ignored → Contested → Threatened → OpenNeutral → Secured → EnemyHeld` の順で排他的に行われること。
- [ ] `OpenNeutral` 島を投資回収ターンの小さい順に優先し、敵本島より先に候補とすること。
- [ ] 最初の島嶼拡張が輸送ヘリ＋占領可能ユニット2体（6,000G）の編成で開始されること。
- [ ] 攻勢作戦が最大3島に制限され、完全編成を割り当てられない候補は `Observe` となること。
- [ ] `Contested` 島ごとに `Contest` / `Reinforce` / `Withdraw` を独立して判断すること。
- [ ] `Secured` 島を毎ターン脅威評価し、`enemy_arrival_eta <= 2` で `Threatened` へ遷移すること。
- [ ] `Threatened` 島の防衛が攻勢優先度最下位の作戦より優先されること。
- [ ] `EnemyHeld` 侵攻が `required_assault_budget`（最低 32,700G、`max(10,200G, ceil(敵戦闘資産 * 1.2))` で増額）を満たすまで開始されないこと。
- [ ] 資金・輸送・ユニットを複数島へ二重計上しないこと。
- [ ] 装甲車の `max_cargo` が海上侵攻の輸送需要を減衰させないこと。
- [ ] 生産選択が `HashMap` 反復順に依存せず、同一 seed・同一コミットで再現すること。
- [ ] map_3 の座標、手番、固定 seed に依存する分岐を production AI ロジックへ追加しないこと。

### Phase 2: 評価

- [ ] V3 対 V1 の 24 試合 baseline と result が同じ seed セットで取得されていること。
- [ ] V3 自己対戦 4 試合の baseline と result が取得されていること。
- [ ] map_3 の先攻・後攻の両方で基準1・基準2・基準3を満たすこと。
- [ ] V3=P2 の固定 seed 試合で、自軍初期島の拠点数を超える拠点獲得が確認できること。
- [ ] 自己対戦で両プレイヤーが初期島外拠点を1つ以上取得すること。
- [ ] 自己対戦で状態欠損・二重割当・試合エラーが 0 件であること。
- [ ] map_1・map_2 を同じ固定 seed 条件で再評価し、既存の PASS 判定を退行させないこと。
- [ ] V3 の総合勝率が40%を下回らないこと。
- [ ] 変更後の map_3 平均思考時間が比較 baseline の150%以内であること。
- [ ] `cargo test`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo fmt --all -- --check` が通ること。
