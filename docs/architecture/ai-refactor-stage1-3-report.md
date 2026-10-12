# AI再設計 段階1〜3 実施記録

`ai-refactor-redesign-plan.html` の段階1〜3（安全網と生産パイロット）を実施した記録。
段階4以降には進んでいない。

## 段階1: 比較基準（manifest）

| 項目 | 値 |
|---|---|
| 基準コミット | `fe49bfebc0fac828cf67de1906ce5274ffb92fa5`（`refactor/v4-small-strategy`） |
| ツール | rustc 1.97.0 (2d8144b78 2026-07-07) / cargo 1.97.0 / Python 3.13.16 / x86_64 Linux |
| 基準バイナリ | 基準コミットを別worktreeで `cargo build --release -p mcp-server`（sha256 `54d5e955…48f7`） |
| マップ | map_1 `e47b545e…`, map_2 `ab943f48…`, map_6 `d7d4d180…`, map_25 `306eef0e…`（`engine/src/resources/master_data/map/*.csv` のsha256先頭） |
| マスターデータ | unit.csv `ad641f1d…`, weapon.csv `41aab8c9…`, movement.csv `55693cbd…`, landscape.csv `bd71c213…` |
| 環境変数 | すべて未設定（`OPENWARS_EXT_DISABLE` / `OPENWARS_FORCE_MAIN` / `OPENWARS_BLESS_AI_BASELINE` 等） |
| 対戦条件 | `run_matchup_sweep.py --p1 V4 --p2 V200 --player-order both --max-turns 30 --grid-type hex` |

### 未追跡資産の扱い

基準コミットだけでは次の資産が欠けており、HEADからは再現できなかった。

| 資産 | 状態 | 対応 |
|---|---|---|
| `engine/src/ai/squad/planning_contract_tests.rs` | `squad.rs` が `mod` 宣言するがファイルが無く、`cargo test -p engine --lib` と `cargo fmt` が失敗 | `build.rs` がファイルの存在時だけcfgを立て、マクロ経由で宣言する。手元にファイルがある環境では従来どおりコンパイルされる |
| `engine/tests/golden/ai_baseline/*.txt` | multi-turn baselineの期待値が無く全シナリオ失敗 | 基準コミットの別worktreeで `OPENWARS_BLESS_AI_BASELINE=1` により生成してコミット。以後は書き換えずに比較する |
| `scripts/test_matchup_runner.py` | 計画書の検証コマンドが参照するが存在しない | 未対応（実行対象から外した） |

手元に未追跡のgoldenがある場合、このブランチを取り込むとファイルが衝突する。内容を比べてから
どちらを正とするか決めること。

### A/A（同一基準バイナリ同士）

map_1/2/6/25 × seed 1,2 × 先後 × 1局、基準バイナリを2変種として比較した。

- 小規模拡張経路（map_1, map_2）: 8条件すべてで行動列ハッシュが一致（決定的）。
- 通常経路（map_6, map_25）: 8条件中3条件で同一バイナリでも行動列が分岐（勝敗・手数は一致）。
  通常経路では1局ずつの行動列比較で挙動差を判定できない。

## 段階2: 判断の比較と計測

- `engine/tests/ai_decision_contracts.rs`: V4の各手番について、購入案が施設を二重使用しない・
  合計費用が配分開始資金を超えない・計画した購入が同じ兵種で適用される・候補不在で見送った枠が
  実在要求（要求量>0）への判断である・発行直前の取消が起きない、を検証する
  （map_1, map_2, map_6, map_25、先後両方）。golden（行動列一致）とは独立した状態契約なので、
  S変更後も同じ契約で検証できる。
- `scripts/measure_ai_complexity.py`: RCAの関数spaceの値から直下の子space（クロージャ等）を
  差し引いた直接値と、子込み値を別列で出す。ABC・呼出深さは未対応と明記する。
  合成fixtureの単体テスト `scripts/test_measure_ai_complexity.py` 付き。

## 段階3: 生産パイロット

新規 `engine/src/ai/production/{demand,allocation,commit}.rs`（版に依存しない共通部品）と、
`v4/mod.rs`・`v4/extended.rs` の生産配分・確定処理の置換。

| 区分 | 変更 | 削除・置換した旧経路 |
|---|---|---|
| R | 要求は `Operation::slots` に残し、未充足の枠を `DemandLedger` に理由付きOutcome（候補なし・戦闘パッケージなし・実行計画なし・campaign予約済み・前段区間優先で保留）として記録 | `clear_slot`（要求を0に書換え）、campaign予約・首都構造枠保留での要求0化 |
| R | 残額・使用施設・施設確保作戦の優先度を `AllocationState` へ集約 | 各ループの `remaining_funds` / `used_facilities` / `facility_owners` の個別更新（4箇所） |
| R | 通常経路と小規模経路で重複していた購入確定（配備意図・trace・発行待ち列）を `commit_turn_production` へ統合 | 2箇所の重複した確定処理 |
| R | 購入案に役割・費用・目的（PlanStep / OperationSlot / ImmediateReinforcement / Mobilization）を持たせ、Issued遷移は目的から行う | 配備意図からのPlanStep逆引き |
| R | traceの `SlotCleared` を `Unfilled { reason }` にし、未充足枠一覧 `unfilled_slots` を追加（mcp-serverの出力ラベルは互換維持） | — |
| B | 発行直前に資金・施設を `can_produce_at` と資金で再検証し、通らない案は発行せず `IssueAudit` に記録して次の案へ（未発行の取消）。stepはPlannedのまま残り次手番に生産失敗として再計画される | 検証なしの発行 |
| B | 施設最適化で命令の施設を入れ替えたとき、保存済みPlanの未発行stepの施設も揃える | 保存後に命令の施設だけを交換する状態 |
| S | 動員購入の役割を価格（=1000なら占領役）ではなく選定理由（実在脅威へ有効打→交戦役、それ以外の占領能力兵→占領役）で決める | 価格による役割推測 |

### 未着手（段階3の範囲で残したもの）

- 施設最適化そのものを配分の確定前へ移し、到着時刻・予測を再評価する変更（今回はstepの施設同期のみ）。
- 複数作戦予約・合同Capture/Combatの予約調停を `AllocationState` の予約として表すこと
  （現状は既存の `contingency_reserve_funds` / `unissued_plan_reserve` の計算をそのまま利用）。
- 一部発注だけ成功した場合の結果ごとの照合を新しい型で表すこと（既存のPlannedPurchase状態と
  deployment registryの照合を利用）。

## 検証結果

### テスト

| コマンド | 結果 |
|---|---|
| `cargo test -p engine --lib` | 860 passed（基準コミットは未追跡ファイル欠落でコンパイル不可だった） |
| `cargo test -p engine --test ai_multi_turn_baseline --test ai_architecture_baseline` | 8 + 4 passed（基準コミット生成のgoldenと一致。R+S後も一致） |
| `cargo test -p engine --test ai_decision_contracts` | 4 passed |
| `cargo fmt --all -- --check` / `cargo clippy --all-targets --all-features -- -D warnings` | 警告なし |
| `python -m unittest scripts.test_measure_ai_complexity scripts.test_eval_matchup` | passed（`scripts.test_matchup_runner` はファイルが存在しない） |
| `cargo check -p engine --target wasm32-unknown-unknown` | 未実行（WASM targetを導入していない） |

### 対戦（V4 対 V200、map_1/2/6/25 × seed 1,2 × 先後 × 1局、30ターン上限、hex）

基準は3標本（A/A の2変種＋候補比較時の基準）。V4視点の勝ち=1・分=0.5・負=0。

| 変種 | V4 score | 局数 | 通常決着 | 上限判定 | 行動列が基準の標本集合に含まれる条件 |
|---|---|---|---|---|---|
| 基準（3標本それぞれ） | 10.0 | 16 | 4勝 | 6勝6負 | — |
| R（段階3のR/B） | 10.0 | 16 | 4勝 | 6勝6負 | 16/16 |
| R+S（動員役割の変更を追加） | 10.0 | 16 | 4勝 | 6勝6負 | 16/16 |

- 小規模経路（map_1, map_2）は8条件すべてで、R・R+Sとも基準と行動列ハッシュが完全一致。
- 通常経路（map_6, map_25）は基準自身が3標本中で行動列を分岐させる（A/Aと同じ揺れ）。
  R・R+Sの行動列はすべて基準のいずれかの標本と同一で、勝敗・決着種別も全条件で一致。
- Sの動員役割変更は、この16条件では一度も判断差を生まなかった（行動列が基準と同一）。
  つまりSの効果は未評価であり、採用判断には該当局面を含む追加の条件が必要。
- V4思考時間（1手番あたり、ms）: 基準の平均 1879〜2743・p95 5060〜9033、
  Rは平均 2689・p95 6885、R+Sは平均 2425・p95 7565。並列数・同時実行ビルドが
  標本ごとに違うため、この差は計測条件のノイズの範囲で、明確な増減は読み取れない。

この短期screenは計画書の最終非劣後認定（12seed以上×両先後×3反復）ではない。

### 複雑度（`measure_ai_complexity.py`、RCA 0.0.25、テストモジュール除外）

| 関数 | 直接CC | 子込みCC | 直接認知 | 子込み認知 | 行数 |
|---|---|---|---|---|---|
| v4/mod.rs `decide_production_v4` | 11 → 10 | 21 → 15 | 9 → 7 | 10 → 8 | 136 → 75 |
| v4/extended.rs `decide_production_v4` | 12 → 10 | 24 → 16 | 10 → 7 | 11 → 8 | 169 → 95 |
| v4/mod.rs `commit_turn_production`（新規・共通） | — → 2 | — → 8 | — → 1 | — → 1 | 96 |
| v4/mod.rs `plan_production_with_registry` | 21 → 21 | 69 → 69 | 34 → 34 | 59 → 59 | 536 → 545 |
| v4/extended.rs `plan_production_with_registry` | 76 → 76 | 154 → 154 | 145 → 142 | 195 → 192 | 1029 → 1052 |
| v4/mod.rs `most_starved_in_tier` | 8 → 9 | 14 → 15 | 15 → 18 | 18 → 21 | 33 → 41 |

AI全体（4499 → 4534関数）: 直接CC 最大254・P95 8→7、合計 11307→11359。

## 段階3の評価（ここで停止）

改善できたこと:
- 要求を0に書き換えて失敗を隠す経路（`clear_slot` と構造枠の0化、計3種）が無くなり、
  未充足の枠ごとに理由がtraceへ残る。
- 資金・施設の状態更新がループごとの個別更新から `AllocationState::claim` の1箇所になった。
- 購入確定処理の重複（通常／小規模）が1つになり、`decide_production_v4` の行数・複雑度が下がった。
- 発行と成功を区別し、発行直前の再検証で取り消した案を記録できるようになった。
- 保存Planと発行命令の施設の食い違いが残らなくなった。

改善できていないこと:
- 配分ループ本体（`plan_production_with_registry`、特に小規模版の直接CC 76）の複雑度は変わらない。
  境界を差し替えただけで、ループの判断本文は手を付けていない。
- 通常版と小規模版の配分ループ本体は依然として2つある（確定処理だけ共通化）。
- 施設最適化の確定前移動、複数作戦予約・合同Capture/Combatの予約調停、
  一部発注成功の照合型は未着手。

計画書の完了条件（「計画と発注が一致し、旧更新経路を削除できた」）のうち、`clear_slot`・
価格による役割推測・重複した確定処理は削除済み、保存後の施設交換は同期で解消、
配分本文の複雑度低減は未達。段階4以降へ同じ設計を広げる前に、配分ループ本体を
`demand`/`allocation` の上で通常・小規模共通にできるかを判断する必要がある。
