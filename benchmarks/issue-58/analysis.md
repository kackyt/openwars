# Issue #58 Baseline Analysis

## Reproducibility

- commit SHA: `e6e462ecac727016b3133f61f72533fa52c3eb8d`
- working tree: dirty（評価基盤・診断テレメトリの未コミット変更を含む）
- 試合実行時の evaluator SHA-256: `214c3303e3bf14a2996ed38354ccd01b509738e05bd5dddb485b5a8355b3e01c`
- 分析再計算時の evaluator SHA-256: `benchmarks/issue-58/baseline-e6e462e.json` の `metadata.analysis_evaluator_sha256`
- MCP SHA-256: `59ca1df8e4d92bd7e4800088b7973b32fb3be15c86a6b5d765b63f1b44353acd`
- seeds: 58001, 58002, 58003, 58004
- 1マップ1手番あたり4試合、合計24試合（3マップ × 4 seed × 2手番）
- 実行コマンド: `scripts/eval_matchup.py --mode batch --map map_1,map_2,map_3 --p1 V3 --p2 V2 --criteria issue58 --seeds 58001,58002,58003,58004 --max-turns 30 --output benchmarks/issue-58/baseline-e6e462e.md --json-output benchmarks/issue-58/baseline-e6e462e.json`
- 試合エラー: 0件、手番バケット欠損: 0件、メトリクス欠損: 0件

### 決定的再現性（未達・既知制約）

同一 seed・同一コミットで map_3 / seed 58001 / V3=P1 を再実行したところ、T1 の生産が「重歩兵」と「軽歩兵」に分岐し、以降の盤面が一致しなかった。

- 差分発生箇所: 最初のラウンド末メトリクス（`p1_units` 14000 vs 13000）と `unit_produced`（`unit_id` 4294967353 の種別）
- 原因: `GameRng` の seed は `mcp-server/src/main.rs:107-108` で設定されており乱数側は固定される。一方 `engine/src/ai/production.rs:152-193` と `engine/src/ai/production.rs:231-303` は `UnitRegistry` の `HashMap` 反復順をそのまま走査し、同スコア候補を「先に現れた方」で採用する。Rust の `HashMap` 反復順はプロセスごとに変わるため、seed とは独立に生産選択が分岐する。
- 本 baseline の扱い: ユーザー判断により、この既知制約を明記したうえで現行 24 試合を分析対象として採用する。数値は「同一プロセス内で取得した 4 seed × 両手番の実測値」であり、プロセス間再実行の完全一致は保証しない。
- 影響: 完了条件のうち「固定 seed 試合の再現一致」は未達。後続計画で決定的 tie-break を導入した時点で再取得が必要。

## Acceptance Baseline

| Map | Order | Games | Seeds | ZOC (V3 / V2) | Income (V3 / V2) | Properties (V3 / V2) | Trend | External property | Complete | Result |
| --- | --- | ---: | ---: | --- | --- | --- | --- | --- | --- | --- |
| map_1 | 先攻 | 4 | 4 | 28.5 / 40.25 | 8750 / 15250 | 5.75 / 12.25 | FAIL | PASS | yes | **FAIL** |
| map_1 | 後攻 | 4 | 4 | 39.5 / 28.5 | 11750 / 12250 | 8.75 / 9.25 | FAIL | PASS | yes | **FAIL** |
| map_2 | 先攻 | 4 | 4 | 62.5 / 13.75 | 20000 / 9000 | 14.25 / 4.75 | PASS | PASS | yes | **PASS** |
| map_2 | 後攻 | 4 | 4 | 55.5 / 20.75 | 16750 / 12250 | 11.75 / 7.25 | PASS | PASS | yes | **PASS** |
| map_3 | 先攻 | 4 | 4 | 55.5 / 80.75 | 23250 / 26500 | 14.25 / 16.75 | FAIL | PASS(対象外) | yes | **FAIL** |
| map_3 | 後攻 | 4 | 4 | 68.75 / 106.75 | 24000 / 36750 | 15.0 / 24.25 | FAIL | FAIL | yes | **FAIL** |

- map_3 勝率: 0/8（0.0%、ガードレール40%未達）
- map_3 平均思考時間: 2200.0 ms、中央値 2220.4 ms、95パーセンタイル 3712.1 ms
- 全体判定: **FAIL**

map_1 の PASS バケットは 0 個であり、後続の result 実行では「map_1 の 2 バケットを FAIL から悪化させない」ことのみが回帰条件となる。map_2 は両手番 PASS であり、これは維持必須。

## Hypothesis Decisions

| 仮説 | 判定 | 数値証拠（map_3、seed / 手番） |
| --- | --- | --- |
| 占領要員が不足している | rejected | 上陸占領要員は 8 試合すべてで 1-3 体存在し、占領要員への投資は 98,000-124,000G。輸送容量も 3-15 で毎試合 Drop に成功している。 |
| 上陸後に部隊が分散する（占領へ移行しない） | confirmed | 8 試合すべてで敵島の占領完了 0 件。占領開始は seed 58001先攻（T13）と seed 58003後攻（T14）の 2 件のみで、Drop（T8 / T12）から 5-2 ターン遅れて 1 回だけ発生し完了しない。 |
| Battleship が過剰投資である | rejected | 戦艦の生産は 8 試合中 1 試合のみ（58001後攻、30,000G）。その試合の与ダメージ価値は 8,824 で ROI 0.294 と全ユニット中最大。過剰投資の証拠はない。 |
| 護衛戦力が不足している（上陸要員が生存しない） | confirmed | 上陸占領要員の生存率は 8 試合中 7 試合で 0.0、最大でも 0.333（58004先攻）。初回交戦は Drop と同一ターンまたは翌ターン（先攻 T8、後攻 T13-T19）に発生している。 |
| 敵首都を狙わない | inconclusive | 上陸要員から敵首都への最小マンハッタン距離は 6-10 で縮小が観測されない。ただし要員が早期に全滅するため、「狙わない」のか「到達前に排除される」のかを本テレメトリでは分離できない。目標選択（選択された目標拠点）の記録が欠落している。 |
| 後攻の立ち上がりが遅い | confirmed | 先攻は侵攻用輸送ユニット生産 T1 / first_load T3 / first_drop T8。後攻は装甲車を輸送需要として誤充足し、実際の輸送ヘリ・輸送船生産が T5-T7、first_load T7-T9、first_drop T12-T18 まで遅れる。 |

分類基準: `confirmed` は map_3 の固定 seed 試合 2 件以上で該当挙動が観測され、かつ不合格バケットまたは侵攻停滞と同時に発生していること。`rejected` は関連する全試合が反対の挙動を示すこと。`inconclusive` はいずれも満たさないか、必要テレメトリが欠落していること。

## Confirmed Root Causes

### C1. 上陸後に占領へ移行しない

- 対象: map_3、先攻・後攻の全 8 試合（seed 58001, 58002, 58003, 58004）
- 実測値: 敵島の占領完了 0 件 / 8 試合。占領開始は 58001先攻 1 件（T13）、58003後攻 1 件（T14）のみ。
- 関連トレース: first_drop は先攻 T8、後攻 T12-T18。上陸から占領開始までの遅延は観測できた 2 件とも 2 ターン以上で、いずれも完了に至らない。
- 失敗している受入基準: map_3 後攻の外部拠点獲得（`external_properties_gained` 8試合すべて 0）、後攻の平均拠点 15.0 < 24.25。

### C2. 上陸した占領要員が護衛されず生存しない

- 対象: map_3、先攻・後攻の全 8 試合
- 実測値: `capture_unit_survival_rate` は 58004先攻の 0.333 を除き全て 0.0。
- 関連トレース: first_combat は先攻 T8（Drop と同一ターン）、後攻 T13-T19（Drop の 1 ターン後）。上陸直後に交戦が発生し要員が失われている。
- 失敗している受入基準: 外部拠点獲得 0、map_3 両手番の ZOC・収入・拠点いずれも V2 未満。

### C3. 侵攻立ち上がりが後攻で大きく遅れる

- 対象: map_3 後攻の全 4 試合（seed 58001, 58002, 58003, 58004）
- 実測値: first_load 7, 7, 7, 9（先攻は全て 3）。first_drop 12, 12, 12, 18（先攻は全て 8）。
- 関連トレース: 先攻は輸送ヘリを全 seed T1 に生産し T3 に Load。後攻は T1 に装甲車（`max_cargo=1` だが海上侵攻不可）を生産し、実際の輸送ヘリ・輸送船は T5, T5, T5, T7、その2ターン後の T7, T7, T7, T9 に Load。輸送需要が装甲車で誤充足される生産構成が手番差の主因。
- 失敗している受入基準: map_3 後攻の平均収入 24000 < 36750、平均拠点 15.0 < 24.25、外部拠点獲得 0。

## Rejected Hypotheses

### R1. 占領要員が不足している

- 8 試合すべてで占領可能ユニットの上陸を確認（`landed_capture_units` 1-3）。
- 占領要員への投資は 98,000-124,000G で、全試合で最低 98,000G を確保している。
- 侵攻用輸送（輸送ヘリ・輸送船）の容量生産は 2-12 で、Drop は全試合成立している。要員が届いていないのではなく、届いた後に機能していない。

### R2. Battleship が過剰投資である

- 戦艦生産は 8 試合中 1 試合（58001後攻）のみ、30,000G。
- その試合の戦艦与ダメージ価値は 8,824 で ROI 0.294。同試合の重歩兵 1,922 を上回り、全ユニット中最大の貢献。
- 高コスト投資の中心は戦艦ではなく砲台・ロケットランチャーで、総投資 638,400-656,200G のうち 501,200-529,400G（約 78-81%）を占める。ただしこれらの投資が敗因かどうかは C1/C2 と独立には判定できないため、本仮説としては戦艦過剰投資を否定するにとどめる。

## Inconclusive Hypotheses

### I1. 敵首都を狙わない

- 上陸要員から敵首都への最小距離は 6, 9, 7, 8（先攻）/ 9, 9, 7, 10（後攻）で、接近の傾向は見られない。
- ただし要員生存率が 0 のため、距離が縮まないのは「目標に選ばない」からか「接近前に排除される」からかを区別できない。
- 欠落しているテレメトリ: AI が各ターンに選択した目標拠点（squad ごとの target property / target island）の履歴。現在の `strategic_history` は盤面のみで、意思決定の目標を記録していない。

## Required AI-Fix Plan Inputs

### 対象モジュール（トレースから推定）

- C1: `engine/src/ai/squad.rs`（降車要員の Capture 部隊への引き渡し）、`engine/src/ai/objectives.rs`（占領対象の決定的優先順位）、`engine/src/ai/beam_search.rs`（上陸後の対象島維持）
- C2: `engine/src/ai/squad.rs`（占領地点周辺の護衛・敵排除割り当て）
- C3: `engine/src/ai/production.rs` および `engine/src/ai/strategy.rs`（海上侵攻需要を装甲車の cargo 容量で誤充足し、輸送ヘリ・輸送船の生産が T5-T7 まで遅れる）
- I1 の判定に必要な追加テレメトリ: `mcp-server/src/invasion_trace.rs`（squad 目標の記録）

### 決定的回帰シナリオ（1 原因につき1つ）

- C1: 敵島に占領可能ユニットが降車済みで、隣接に占領対象拠点があり、敵ユニットが射程外にある盤面。次ターンに占領コマンドが選択されることを検証する。
- C2: 敵島に降車済みの占領要員と、その要員を攻撃可能な敵ユニット1体、および同島の自軍戦闘ユニット1体がある盤面。戦闘ユニットが当該敵の排除または遮蔽に割り当てられることを検証する。
- C3: 海上侵攻目標があり、工場で装甲車、空港または港で侵攻用輸送を生産可能な盤面。装甲車の `max_cargo` で海上輸送需要を減衰させず、輸送ヘリ・輸送船を先に選ぶことを検証する。

### result 実行が改善すべき baseline 値

| 指標 | baseline | 目標 |
| --- | --- | --- |
| map_3 敵島の占領完了数（8試合合計） | 0 | 1以上 |
| map_3 後攻の外部拠点獲得（4試合） | 0 | 1以上 |
| map_3 上陸占領要員の生存率 | 0.0-0.333 | 改善（0.333超の試合を増やす） |
| map_3 後攻 first_drop | T12-T18 | 短縮 |
| map_3 後攻 平均拠点数 | 15.0（V2 24.25） | V2 以上 |
| map_3 両手番 平均収入 | 23250 / 24000（V2 26500 / 36750） | V2 以上 |
| map_3 勝率 | 0.0% | 40%以上 |
| map_3 平均思考時間 | 2200.0 ms | 3300.0 ms 以下（baseline × 1.50） |
| map_2 両手番判定 | PASS | PASS 維持 |
| map_1 両手番判定 | FAIL | 悪化させない |

## Task 1 Portfolio Protocol Baselines

### Immutable identifiers

- commit SHA: `e6e462ecac727016b3133f61f72533fa52c3eb8d`（short: `e6e462e`）
- working tree: dirty（Issue #58 Phase 1 評価基盤・診断テレメトリの未コミット変更を含む）
- evaluator SHA-256: `f8ce986afcfbbe5ea7f412cdbe1f222f4ff27ac3f78b6ffdcc8077513bcba16b`
- MCP SHA-256: `59ca1df8e4d92bd7e4800088b7973b32fb3be15c86a6b5d765b63f1b44353acd`
- V3 vs V1 JSON: `benchmarks/issue-58/baseline-v3-v1-e6e462e.json`
- V3 vs V1 Markdown: `benchmarks/issue-58/baseline-v3-v1-e6e462e.md`
- V3 self-play JSON: `benchmarks/issue-58/baseline-v3-selfplay-e6e462e.json`
- V3 self-play Markdown: `benchmarks/issue-58/baseline-v3-selfplay-e6e462e.md`

### Commands and runtime verification

```text
python scripts/eval_matchup.py --mode batch --map map_1,map_2,map_3 --p1 V3 --p2 V1 --criteria issue58 --issue58-protocol v3-v1 --artifact-stage baseline --seeds 58001,58002,58003,58004 --max-turns 30 --json-output benchmarks/issue-58/baseline-v3-v1-e6e462e.json --output benchmarks/issue-58/baseline-v3-v1-e6e462e.md
```

- exit code: 0
- protocol / stage: `v3-v1` / `baseline`
- expected games / actual results / analyses: 24 / 24 / 24
- games per seed: 6
- result errors: 0

```text
python scripts/eval_matchup.py --mode batch --map map_3 --p1 V3 --p2 V3 --criteria issue58 --issue58-protocol v3-selfplay --artifact-stage baseline --seeds 58001,58002,58003,58004 --max-turns 30 --json-output benchmarks/issue-58/baseline-v3-selfplay-e6e462e.json --output benchmarks/issue-58/baseline-v3-selfplay-e6e462e.md
```

- exit code: 0
- protocol / stage: `v3-selfplay` / `baseline`
- expected games / actual results / analyses: 4 / 4 / 4
- games per seed: 1
- result errors: 0

### Observed pre-change V3 results

#### V3 vs V1

| Map | V3 order | Games | Unique seeds | ZOC (V3 / V1) | Income (V3 / V1) | Properties (V3 / V1) | Result errors | Missing metrics |
| --- | --- | ---: | ---: | --- | --- | --- | ---: | ---: |
| map_1 | 先攻 | 4 | 4 | 44.0 / 17.5 | 15500 / 8500 | 12.5 / 5.5 | 0 | 0 |
| map_1 | 後攻 | 4 | 4 | 33.25 / 25.5 | 13250 / 10750 | 10.25 / 7.75 | 0 | 0 |
| map_2 | 先攻 | 4 | 4 | 55.25 / 24.0 | 18250 / 10750 | 12.5 / 6.5 | 0 | 0 |
| map_2 | 後攻 | 4 | 4 | 54.75 / 23.75 | 16500 / 12500 | 11.5 / 7.5 | 0 | 0 |
| map_3 | 先攻 | 4 | 4 | 68.5 / 63.25 | 24000 / 24000 | 15.0 / 15.0 | 0 | 0 |
| map_3 | 後攻 | 4 | 4 | 75.75 / 88.5 | 24250 / 31000 | 15.0 / 20.75 | 0 | 0 |

- runtime completeness: 24 / 24 results、24 analyses、result errors 0、missing metrics 0
- physical outcomes: P1 win 13、P2 win 11、draw 0
- map_3 V3 wins: 0 / 8、win rate 0.0%
- map_3 後攻の差分: ZOC は V3 が 12.75 小さく、income は 6750 小さく、properties は 5.75 少ない
- map_3 thinking time: mean 3141.8 ms、median 3209.2 ms、p95 5747.0 ms
- map_3 の観測値: 外部拠点獲得 1 件（1 / 8 試合）、占領開始 16 件、占領完了 2 件、上陸占領要員 38 体
- map_3 first drop: `7, 22, 7, -, 7, -, 7, 17`（seed・手番順。`-` は未観測）

#### V3 self-play

- runtime completeness: 4 / 4 results、4 analyses、result errors 0、missing metrics 0
- physical game results: P1 win 1、P2 win 2、draw 1
- 現行 analyzer が出力する V3=P1 perspective: 4 analyses、先攻 bucket のみ、P1-perspective wins 1 / 4（25.0%）
- observed P1-perspective row: ZOC 81.75 / 80.75、income 24000 / 24000、properties 15.0 / 15.0
- thinking time: mean 2323.6 ms、median 2398.5 ms、p95 4059.3 ms
- map_3 の観測値: 外部拠点獲得 0 件、占領開始 0 件、占領完了 0 件、上陸占領要員 10 体、要員生存率は全4分析で 0.0
- first transport production / first load / first drop: `1/3/8, 1/4/9, 1/3/8, 1/4/9`
