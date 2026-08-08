# V4 Stage 1 revised: map_3 検証結果

## 結論

Stage 1の生産ループ停止は合格とする。敵が0体の作戦から具体的なCombat兵種を推測するフォールバックを撤去した結果、同一兵種の連続生産と対空偏重は解消し、遊兵AもStage 0を下回った。

Issue #54の上陸判定は両席ともFAILであり、B/Cは悪化した。内訳はInfantryとTransportHelicopter、およびTransport/Capture Squadの停滞へ集中している。これはStage 1のCombat候補選定ではなく、Stage 2の島嶼キャンペーン要求・ミッション接続の未実装を示す。

## 実行条件

- 実行日: 2026-08-08
- コマンド: `python scripts/eval_matchup.py --mode batch --map map_3 --p1 v4 --p2 v3 --criteria issue54 --max-turns 14 --seed 42 --trace-output logs/v4_stage1_revised_final_map3.jsonl --output reports/v4_stage1_revised_final_map3.md`
- 対戦: V4 vs V3、先攻・後攻を各1ゲーム
- V4手番数: 28
- release build: `cargo build --release -p mcp-server` 成功

## Stage 0との比較

| 指標 | Stage 0 | Stage 1 revised | 差分 |
| --- | ---: | ---: | ---: |
| 盤上ユニット数（手番合計） | 487 | 349 | -138 |
| A: 任務なし | 96 | 92 | -4 |
| B: 任務はあるが命令なし | 36 | 54 | +18 |
| C: 行動可能なまま終了 | 12 | 29 | +17 |
| A（1手番平均） | 3.43 | 3.29 | -0.14 |
| A（最大） | 8 | 6 | -2 |
| 終盤3ターンのA（両席合計） | 25 | 12 | -13 |

最終3ターンのAは、先攻が `3 → 2 → 0`、後攻が `4 → 1 → 2` だった。

## 生産内訳

| ユニット | Stage 0 | 旧Stage 1 | Stage 1 revised |
| --- | ---: | ---: | ---: |
| AntiAir | 19 | 26 | 0 |
| Bcopters | 25 | 21 | 5 |
| Infantry | 13 | 10 | 14 |
| TransportHelicopter | 5 | 3 | 7 |
| HeavyFighter | 2 | 0 | 5 |
| Bomber | 0 | 0 | 9 |
| Lander | 0 | 0 | 2 |
| Fighter | 0 | 0 | 1 |

Stage 1 revisedの枠別発注はCombat 20、Capture 14、Transport 9。敵戦力価値が0の作戦からのCombat購入は0件だった。全28手番の残額平均は20,238であり、これは「未観測の増援へ具体的兵種を捏造しない」という修正後の停止条件による。

## B/C悪化の内訳

- B: Infantry 30、TransportHelicopter 23、Bomber 1
- C: Infantry 22、TransportHelicopter 6、Bomber 1
- 行動者0の停滞Squad: Transport/Forming 12、Capture/MovingToTarget 5、Capture/Forming 3、Transport/Pickup 3、Transport/Transit 3、Transport/Drop 1

Bの53/54件が占領要員と輸送役であるため、Stage 2ではV4生産が島嶼キャンペーンshortfallを読み、購入したInfantry/Transportを同じ作戦のSquadへ帰属させるところまでを合格条件とする。

## 成果物

- 最終JSONL: `logs/v4_stage1_revised_final_map3.jsonl`
- Issue #54判定: `reports/v4_stage1_revised_final_map3.md`
- 本比較レポート: `reports/v4_stage1_revised_metrics_map3.md`
