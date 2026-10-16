# AI再設計 v3・段階4 実装・検証記録

2026-10-16。区分R（構造変更）。作業ブランチは `refactor/ai-redesign-v3-stage4`、比較基準は `a7d13a8`。移動と段分割を別コミットに分けた。

## 実装結果

`v4/extended.rs` を13,710行から3,580行へ縮小し、生産本文と補助関数だけを残した。首都ルートDAG、敵生産予測、物件支配の実装とテストを責務別に分離した。戦略変更、保存形式変更、golden更新は含めない。

以下のパスは `engine/src/ai/v4/` 配下。

| 所有先 | 責務 |
|---|---|
| `capital_route/model.rs` | 静的DAG、Node作戦、Squad指令、陸路需要の型と台帳 |
| `capital_route/topology.rs` | 地形連結・橋Gate・前向きDAGの構築とcache |
| `capital_route/node_operation.rs` | Node作戦の構築・盤面観測・出口条件 |
| `capital_route/force_demand.rs` | 枝別の兵力需要・突破履歴・戦略Milestone |
| `capital_route/commitment.rs` | Squad候補、枝配分、前衛・待機枠、出発許可、台帳確定 |
| `capital_route/tactical.rs` | 確定済み指令の戦術位置・占領予約・回復状態への投影 |
| `capital_route/mod.rs` | 宣言と静的DAG→Node作戦の準備順の組立 |
| `enemy_forecast.rs` | 新規敵Entityの生産観測・次手番予測・増援envelope |
| `property_control.rs` | 既存ETA関数とDirectCapture / Interdict / Recapture計画 |
| `board_scan.rs` | 通常・小規模の読み取り専用盤面収集 |
| `strategy_pipeline.rs` | 生産snapshot前のprofile固定・戦略準備と観測入力 |

親の `pub use self::extended::*` は撤去し、利用側は担当moduleを明示する。公開診断APIだけは明示reexportで互換性を維持する。静的トポロジーから動的Node作戦への逆依存は入口の組立へ移し、診断スイッチは `trace.rs` に置いてDAGから盤面収集への依存もなくした。

## 移動と段分割

移動版と分割版のソースsnapshot・バイナリ・検証ログを `work/stage4/` に分けて保存した。移動版では174関数すべての本文が、空白・末尾カンマ・担当moduleへの参照path変更を除いて一致することを確認した。

元の66テストはDAG 30件・敵予測1件・生産35件へ欠落なく分離し、fixtureを `test_support.rs` に置いた。生産テストは `production_tests.rs` に分離済みで、段階5のパイプライン統合時にも再利用できる。

DAGの割当は、候補抽出→枝配分→Node・役割の照合→前衛/Stage枠→出発許可→指令・索引・診断の反映へ分割した。一時的な `AssignmentContext`、`RouteAllocation`、`IslandAssignment`、`CommitmentDraft` で読み取りと枠の消費・確定前の結果を分け、永続的な正本は増やしていない。前進度と安定IDの順、前回作戦IDによる枝・Node継承、Capture/Control別の容量、Node出口条件、Squad単位の指令、他playerの台帳保持を維持した。

| 計測対象 | 変更前 | 変更後 |
|---|---:|---:|
| `refresh_capital_route_path_commitments` 行数 | 675 | 47 |
| 同・直接CC | 56 | 2 |
| 同・直接認知的複雑度 | 119 | 2 |
| 分割した21関数の最大直接CC | — | 10 |
| 同・最大直接認知的複雑度 | — | 7 |
| 同・最大行数 | — | 97 |

## 盤面収集と更新境界

`BoardScan::collect` / `collect_extended` は `&World` と準備入力だけを受け取る。profile固定・採用済みportfolioの取得・未採用時の解析とDAG準備は `strategy_pipeline::prepare_production_scan` へ移した。

旧処理は解析前に兵站計画と再占領契約を読んでいた。解析が兵站台帳を更新しても今回の判断を解析後の値へ変えないよう、その観測を準備入力に保持する。これは同じ呼出し内だけの入力で、永続cacheは追加していない。

走査順は入口で用意した `BoardScanQueries` の読み取り専用 `QueryState::iter_manual` で保つ。Bevyはcomponent indexからQuery順を決めるため、テーブルIDの昇順とは一致しない。16種類のarchetypeとsparse componentを含む入力で、旧Queryのunit・物件列挙順との一致を検証した。Queryの準備は入口に置き、snapshot内ではWorldを変更しない。もう1件の新規テストで、戦略入力なしのsnapshotが戦略を初期化しないこと、準備前の兵站観測が保持されること、収集が盤面とDAGを変更しないことを確認した。

| 更新対象 | 反映先 |
|---|---|
| 今回のSquad指令・作戦ID索引・診断 | `commit_route_assignments`（旧retain/extend順を維持） |
| Capture Squadの出発許可・Formingからの進行 | `authorize_capture_squad`（旧2field更新を維持） |
| Node作戦の観測 | 割当前後の `refresh_capital_route_node_operations` を維持 |
| profile・戦略/DAGの準備 | 生産の入口に明示し、snapshotからのWorld更新を撤去 |

## 検証結果

| 検証 | 結果 |
|---|---|
| 移動版・全Rust releaseテスト | 976成功、2 ignored |
| 分割版・全Rust releaseテスト | 978成功、2 ignored |
| 全target・全feature Clippy（`-D warnings`） | 成功 |
| `wasm32-unknown-unknown` のengine check | 成功 |
| `cargo fmt --all -- --check` | 成功 |
| 計画書のPython 3スイート | 41成功 |
| 移動版 vs 基準・28条件50T | 全行動列・勝敗・終了turn **28/28一致** |
| 分割版 vs 基準・28条件50T | 全行動列・勝敗・終了turn **28/28一致** |
| V1〜V4固定入力・各版4条件（map_1 / map_10、seed 1、先後、4T） | 移動版・分割版とも各 **4/4一致** |

50T対戦の条件はV4対V200、hex、map_1 / map_2 / map_6 / map_25 / map_32 / map_42 / map_10、seed 1,2、先後両方、各1局、`--jobs 8`。基準→移動版→分割版を順に実行し、各局JSONの `action_history` 全本文をPythonの深い等値比較で確認した。走査順の差が出たmap_6 / map_25では、seed 1,2・先後の8条件について6Tまでの全行動列prefix一致を先に確認した。その後に50Tを再実行しており、短いハッシュや勝敗だけの一致ではなく、`result`・`turns` と実行エラーなしも確認した。

V1〜V4の4T固定入力比較は短い回帰確認であり、中終盤の網羅を主張しない。V4の28条件50Tと既存の複数ターンgolden・save/load・決定性テストを併用し、goldenは更新していない。

計画書にあった `scripts/test_matchup_runner.py` は指定ブランチに含まれていなかったため、リモートのバックアップ `de19006` から既存テストをそのまま復元した。runner本体がその版と現行で同一であることを確認した上で、指定の3スイートを実行している。

主なコマンド:

```sh
cargo test --release --locked --no-fail-fast
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo check --locked -p engine --target wasm32-unknown-unknown
cargo fmt --all -- --check
python -m unittest scripts.test_measure_ai_complexity scripts.test_eval_matchup scripts.test_matchup_runner
python scripts/measure_ai_complexity.py --root engine/src/ai --output work/stage4/final-complexity.json
git diff --check
```

比較用バイナリは `baseline-mcp-server`、`move-verified-mcp-server`、`candidate-mcp-server`、生データは `sweep/games/`、全文比較結果は `sweep/full-history-comparison.json` に保存した（いずれも `work/stage4/` 配下）。

## 全AIの計測と限界

RCA 0.0.25と既存の校正済み `measure_ai_complexity.py` を同じ定義で実行した。直接値と子込み値を混同せず、以下は直接値。RCAの関数spaceにはクロージャも含む。

| 全AI | 基準 | 分割版 |
|---|---:|---:|
| 関数space数 | 5005 | 5033 |
| 直接CC 最大 / P95 / 合計 | 108 / 8 / 11,671 | 108 / 8 / 11,712 |
| 直接認知 最大 / P95 / 合計 | 391 / 9 / 9,597 | 391 / 8 / 9,551 |

関数・クロージャspaceの増減も合計値に反映される。DAG入口とその段の改善を、全AIのCC削減とは扱わない。最大値を持つ未変更の探索核なども残る。ABCと解決済み呼出深さはツール未対応のため、今回も算出していない。

## 段階5への引継ぎ

残る `extended.rs` の生産本文と標準側のコピーを6段へ分解し、`ProductionPolicy` で統合する。親 `mod.rs` に残る生産の実装本文、`combat_plan_input`、`build_operations` の段分割、残りの `*/extended.rs` と一時スイッチの撤去・S項目は、計画書の段階5・6で扱う。
