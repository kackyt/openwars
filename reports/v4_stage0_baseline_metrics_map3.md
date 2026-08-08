# V4 Stage 0 baseline: map_3

## 実行条件

- 実行日: 2026-08-08
- コマンド: `python scripts/eval_matchup.py --mode batch --map map_3 --p1 v4 --p2 v3 --criteria issue54 --max-turns 14 --seed 42 --trace-output logs/v4_stage0_baseline_map3_final.jsonl --output reports/v4_stage0_baseline_map3_final.md`
- 対戦: V4 vs V3、先攻・後攻を各1ゲーム
- 結果: Issue #54 の侵攻判定は両席とも FAIL（敵初期島への上陸なし）。これは修正前の baseline として記録する。

`v4` の小文字指定を MCP の `V4` へ正規化した後の結果だけを本 baseline とする。以前の小文字をそのまま渡した計測は MCP が設定を拒否して既定 V3 を実行していたため、比較対象にしない。

## 遊兵 A/B/C

V4 の28手番における合計・平均・最大値は以下のとおり。

| 指標 | 合計 | 1手番平均 | 最大 |
| --- | ---: | ---: | ---: |
| 盤上ユニット数 | 487 | 17.39 | 41 |
| A: 任務なし | 96 | 3.43 | 8 |
| B: 任務はあるが命令なし | 36 | 1.29 | 4 |
| C: 行動可能なまま終了 | 12 | 0.43 | 3 |

最終3ターンでも A は先攻で `0 → 5 → 5`、後攻で `5 → 6 → 4` であり、最終目標の A=0 は未達である。

分類 D は、同じ Squad が連続2自軍手番で同じ phase・target・構成のまま、構成員が誰も行動しなかった場合としてトレースの差分から数えた。後攻ゲームで6件あり、`Forming` の1人 Squad（Squad 7、11、51）が停滞していた。

## 生産トレース

実際に採用された発注だけを集計した。候補として返されたがその呼び出しでは発注されなかった施設分は含めない。

| ユニット | 発注数 | 駆動した枠 |
| --- | ---: | --- |
| Bcopters | 25 | Combat |
| AntiAir | 19 | Combat |
| Infantry | 13 | Capture |
| TransportHelicopter | 5 | Transport |
| HeavyFighter | 2 | Combat |

同一手番に同じ種別が複数施設へ発注される現象を再現した。代表例は先攻1ターン目の `AntiAir x2`（Combat）と `Infantry x3`（Capture）、後攻8〜10ターン目の各 `AntiAir x2`（Combat）である。したがって AntiAir 偏重は Combat 枠の選定に起因することが確定した。

輸送役は `TransportHelicopter` が5件で、任務なし Lander はこの seed・14ターンの final trace には現れなかった。A の改善判定は Lander 固有ではなく、全輸送役を含む A/B/C と D で比較する。

## 成果物

- JSONL: `logs/v4_stage0_baseline_map3_final.jsonl`
- Issue #54 判定: `reports/v4_stage0_baseline_map3_final.md`

