# AI再設計 v3・段階5 実装・検証記録

2026-10-16。区分R（構造変更）。作業ブランチは `refactor/ai-redesign-v3-stage5`、比較基準は段階4の `bd0e360`。分解版と統合版を別コミットに分けて保存した。

## 実装結果

`v4/extended.rs`（3,580行）を削除し、生産の標準・小規模の重複9関数を統合した。`v4/mod.rs` は5,575行から117行へ縮小し、宣言・import・公開入口の再公開だけを置いた。公開生産API、保存形式、golden、Cargo.lockは変更していない。

以下のパスは `engine/src/ai/v4/` 配下。

| 所有先 | 責務 |
|---|---|
| `production_entry.rs` | Worldの観測、campaign生産、入力収集、最終購入案の共通commit |
| `production_model.rs` | snapshot・作戦・購入・配備の型と、手番内の到達性cache |
| `production_policy.rs` | `ProductionPolicy { Standard, SmallMap }`、候補評価、見送り条件、DAG・余剰段の適用条件 |
| `production_pipeline/mod.rs` | 6段の組立、`ProductionInputs` / `ProductionState`、準備・単一帰属・最終確定 |
| `production_pipeline/slot_allocation.rs` | 最不足枠の選択、継続/探索、1枠の候補・施設確保・配備・購入記録 |
| `production_pipeline/immediate_combat.rs` | 実在前線の敵HPと施設を対応付ける即時増援 |
| `production_pipeline/mobilization.rs` | 余剰資金の動員。候補・既存最良候補の比較キーを共有 |
| `operation_builder.rs` | campaign候補、重複排除・優先度、配置、脅威・既存戦力の観測、枠算出 |
| `combat_input.rs` / `combat_input/projection.rs` | 対象・占領期限・既存戦力・生産候補・初撃ETAを順に構築 |
| `candidate.rs` / `slot_demand.rs` | 到達性・適合度の共通骨格と、元要求を保つ配分台帳 |

戦略選択や必要数の規則は変更していない。新しい入力・中間型は今回の呼出し内だけの値であり、永続的な正本や独立した戦略cacheを追加していない。

## 6段と更新境界

入口で方策を一度選び、盤面snapshot、campaign予約、作戦の脅威地平、枠選択まで同じ方策を渡す。通常/小規模を判定するTLS参照を配分関数へ散在させない。`OPENWARS_FORCE_MAIN` と生産入口の経路切替を維持し、生産内の個別委譲点の計測IDは方策へ置換した。

1. `prepare_operations`：作戦構築、必要な中央契約同期、順序確定、初期SlotLedger。
2. `assign_committed_combat`：実Entityを現在位置から最寄りの一作戦へ単一帰属。
3. `allocate_operation_slots`：最不足枠→RollingPlan継続/探索→候補選定→施設確保→配備・購入記録。
4. `allocate_immediate_reinforcements`：未発行Mustとcounterの資金を残し、実在敵の未解決HPへ増援。
5. `allocate_surplus_mobilization`：空き施設と残額で前線へ動員。価格ではなく能力・選定理由から役割を決める。
6. `finalize_turn_production`：必要な施設交換、工程・意図・診断の投影、未観測Plan整理、残額・予約金の確定。

段4・5と施設交換は小規模だけに適用する。標準の予約金計算は従来どおり発注済み費用を控除せず、小規模だけ未発行Mustを控除する。標準はPlanIdなしの案を実行せず、小規模は実敵へ届くbest effortを即時Combatとして扱う。標準のCapture充足・Combat充足の排他性、小規模の武装carrier・Stage戦力、合同Captureの実効要求、候補/見送り条件、traceの既定値を方策別に保つ。

資金・施設の確保が成功してから、工程・中央契約・配備・購入列へ反映する。枠を閉じる理由、strictな候補更新、同値時の走査順、plan継続判定の順を維持した。`BoardScan` の `&World` と事前準備したQueryStateの列挙順はそのまま使う。初撃の占有cacheも敵ごとに独立させ、占領経路で失敗したCaptureを別標的へすり替えない。

## 関数単位の計測

表の値は「行数 / 直接CC / 直接認知的複雑度」。入口から処理を消しただけと誤認しないよう、6段と主要な補助処理も別に計測した。

| 対象 | 基準 | 統合後の共通入口 |
|---|---:|---:|
| 生産・標準 | 554行 / 22 / 35 | 20行 / 3 / 3 |
| 生産・小規模 | 1,061行 / 77 / 144 | 20行 / 3 / 3 |
| 作戦構築・標準 | 219行 / 1 / 1 | 45行 / 2 / 3 |
| 作戦構築・小規模 | 440行 / 1 / 1 | 45行 / 2 / 3 |
| 戦闘入力・標準 | 263行 / 19 / 28 | 31行 / 3 / 1 |
| 戦闘入力・小規模 | 726行 / 46 / 77 | 31行 / 3 / 1 |

| 各段 | 行数 / 直接CC / 直接認知的複雑度 |
|---|---:|
| `prepare_operations` | 91行 / 5 / 7 |
| `assign_committed_combat` | 46行 / 3 / 4 |
| `allocate_operation_slots` | 62行 / 5 / 4 |
| `allocate_immediate_reinforcements` | 129行 / 4 / 7 |
| `allocate_surplus_mobilization` | 103行 / 5 / 9 |
| `finalize_turn_production` | 53行 / 1 / 0 |

RollingPlan評価と1枠発注は別関数にし、候補選定・未充足記録・施設確保・配備生成・購入記録まで分けた。作戦構築は候補源→採点→局地支配→配置→観測→枠算出を分離し、物件制御の投影も32行の組立から専用関数へ渡す。合同Capture候補の対象・施設・兵種の走査は別関数とし、各段で1つの明示loopだけを持つ。長い宣言・診断項目を含む補助関数には100行を少し超えるものもあるため、全関数の一律100行以下は主張しない。

RCA 0.0.25と既存の校正済み計測scriptを同じ定義で使用。closureを含む全AIの関数space数は5,033→4,975。

| 全AI | 基準 | 統合後 |
|---|---:|---:|
| 直接CC 最大 / P95 / 合計 | 108 / 8 / 11,712 | 108 / 8 / 11,605 |
| 直接認知的複雑度 最大 / P95 / 合計 | 391 / 8 / 9,551 | 391 / 9 / 9,360 |

全AIの最大値は段階5外の関数に残る。直接認知的複雑度の合計は減ったが、P95は8→9であり、全体の分布が一様に改善したとはしない。ABCと呼出深さは現行scriptの未対応項目であり、近似値で成功を主張しない。

## 検証結果

| 検証 | 結果 |
|---|---|
| 分解版・全Rust releaseテスト | 978成功、2 ignored |
| 統合版・全Rust releaseテスト | 979成功、2 ignored |
| 全target・全feature Clippy（`-D warnings`） | 成功 |
| engine / `wasm32-unknown-unknown` check | 成功 |
| fmt / diff check | 成功 |
| Python 3スイート | 41成功 |
| 分解版 vs 基準・28条件50T | action_history全文・勝敗・終了turn **28/28一致** |
| 統合版 vs 基準・28条件50T | action_history全文・勝敗・終了turn **28/28一致** |
| 統合版・map_6 / map_25の6T prefix | seed 1,2・先後 **8/8一致** |
| V1〜V4固定入力・各4条件4T | 分解版・統合版とも各version **4/4一致** |

既存AIテストの名前・件数を保存し、入口から明示した方策がTLS経路に引かれず枠選択を決める回帰テストを1件追加した。goldenの更新で不一致を解消していない。

50T条件はV4対V200、hex、map_1 / map_2 / map_6 / map_25 / map_32 / map_42 / map_10、seed 1,2、先後両方、各1局、`--jobs 8`。基準は段階4で検証済みの最終バイナリとJSONを再利用し、バイナリSHA-256とソースを照合した。分解版と統合版はそれぞれ別バイナリで実行し、各局の `action_history` 全本文、`result`、`turns` をJSONの深い等値比較で判定し、実行エラーなしも確認した。短い表示hashや勝敗だけでは採用していない。

V1〜V4固定入力はmap_1 / map_10、seed 1、先後、4Tの短い回帰確認であり、全versionの中終盤を網羅する検証ではない。V4の50Tと既存の決定性・save/load・goldenテストを併用している。

[検証の機械可読記録](ai-redesign-v3-stage5-validation.json)に基準commit、ソースmanifestとバイナリのhash、条件別の識別hash、件数と計測値を保存した。識別hashは全文一致を判定した後の記録用。生の対局JSON・ソースsnapshot・ログは `work/stage5/` に保存している。

主なコマンド：

```sh
cargo test --release --locked --no-fail-fast
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo check --locked -p engine --target wasm32-unknown-unknown
cargo fmt --all -- --check
python -m unittest scripts.test_measure_ai_complexity scripts.test_eval_matchup scripts.test_matchup_runner
python scripts/measure_ai_complexity.py --root engine/src/ai --output work/stage5/integrated-complexity.json
git diff --check
```

## 段階6への引継ぎ

次は6aの `rolling_plan/extended.rs` など残りの責務別分離と巨大関数の分割。6bの戦略変更、Roadmapの再選択、拒否工程の取消、占有モデル統一、兵種バランスは今回へ混ぜていない。段階6でも分解の前後で固定入力と全行動列を比較し、Sの変更は1件ずつ反例と得失を評価する。
