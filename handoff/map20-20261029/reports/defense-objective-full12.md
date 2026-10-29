# AI対戦比較
基準: .tmp\defense-objective-20261029-full12\games\baseline\map_20_V200-V4_r0
候補: .tmp\defense-objective-20261029-full12\games\defense\map_20_V200-V4_r0
対象: P2 / V4 → V4 / 12対局

| 結果 | 基準 | 候補 |
|---|---:|---:|
| 通常勝利 | 0 | 0 |
| 通常敗北 | 0 | 0 |
| 上限判定勝利 | 8 | 2 |
| 上限判定敗北 | 4 | 10 |
| 通常引分 | 0 | 0 |
| 上限引分 | 0 | 0 |

通常勝利/全対局: 0/12 → 0/12
総対局ターン: 720 → 720
思考時間の標本平均(ms): 1438.41 → 1308.19
思考時間の標本数: 720 → 720 （未記録対局: 0 → 0）

指標の観測時点: T15の実測値。N/Aは未観測。
終局時の数値だけから改善・悪化を判定しない。同時要件は別途検証する。

| Seed | 基準（結果 / 終局 / 指標） | 候補（結果 / 終局 / 指標） | 通常勝利の変化 |
|---|---|---|---|
| 1 | P2_Win_MaxTurns / T60 / ZOC 148 / 拠点 19 / 収入 22000 | P1_Win_MaxTurns / T60 / ZOC 158 / 拠点 21 / 収入 24000 | — |
| 2 | P2_Win_MaxTurns / T60 / ZOC 151 / 拠点 19 / 収入 22000 | P2_Win_MaxTurns / T60 / ZOC 160 / 拠点 20 / 収入 23000 | — |
| 3 | P1_Win_MaxTurns / T60 / ZOC 144 / 拠点 19 / 収入 22000 | P1_Win_MaxTurns / T60 / ZOC 158 / 拠点 20 / 収入 23000 | — |
| 4 | P2_Win_MaxTurns / T60 / ZOC 129 / 拠点 19 / 収入 22000 | P1_Win_MaxTurns / T60 / ZOC 157 / 拠点 20 / 収入 23000 | — |
| 5 | P1_Win_MaxTurns / T60 / ZOC 169 / 拠点 19 / 収入 22000 | P1_Win_MaxTurns / T60 / ZOC 182 / 拠点 20 / 収入 23000 | — |
| 6 | P1_Win_MaxTurns / T60 / ZOC 157 / 拠点 19 / 収入 22000 | P1_Win_MaxTurns / T60 / ZOC 171 / 拠点 20 / 収入 23000 | — |
| 7 | P2_Win_MaxTurns / T60 / ZOC 168 / 拠点 19 / 収入 22000 | P2_Win_MaxTurns / T60 / ZOC 167 / 拠点 20 / 収入 23000 | — |
| 8 | P2_Win_MaxTurns / T60 / ZOC 135 / 拠点 19 / 収入 22000 | P1_Win_MaxTurns / T60 / ZOC 183 / 拠点 20 / 収入 23000 | — |
| 9 | P2_Win_MaxTurns / T60 / ZOC 167 / 拠点 19 / 収入 22000 | P1_Win_MaxTurns / T60 / ZOC 163 / 拠点 20 / 収入 23000 | — |
| 10 | P2_Win_MaxTurns / T60 / ZOC 168 / 拠点 19 / 収入 22000 | P1_Win_MaxTurns / T60 / ZOC 165 / 拠点 20 / 収入 23000 | — |
| 11 | P1_Win_MaxTurns / T60 / ZOC 149 / 拠点 19 / 収入 22000 | P1_Win_MaxTurns / T60 / ZOC 162 / 拠点 20 / 収入 23000 | — |
| 12 | P2_Win_MaxTurns / T60 / ZOC 172 / 拠点 19 / 収入 22000 | P1_Win_MaxTurns / T60 / ZOC 148 / 拠点 20 / 収入 23000 | — |

生産総数は対局長と合わせて読む。

| 兵種 | 基準 | 候補 |
|---|---:|---:|
| Artillery | 45 | 44 |
| HeavySpGun | 62 | 37 |
| Infantry | 2009 | 1771 |
| LightSpGun | 181 | 162 |
| MdTank | 233 | 281 |
| Mech | 827 | 785 |
| Rockets | 345 | 381 |
| Tank | 383 | 428 |
| TankZ | 53 | 30 |
