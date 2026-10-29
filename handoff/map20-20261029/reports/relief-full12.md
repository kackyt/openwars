# AI対戦比較
基準: .tmp\relief-study-20261029-full12\games\baseline\map_20_V200-V4_r0
候補: .tmp\relief-study-20261029-full12\games\relief\map_20_V200-V4_r0
対象: P2 / V4 → V4 / 12対局

| 結果 | 基準 | 候補 |
|---|---:|---:|
| 通常勝利 | 0 | 0 |
| 通常敗北 | 0 | 0 |
| 上限判定勝利 | 8 | 10 |
| 上限判定敗北 | 4 | 2 |
| 通常引分 | 0 | 0 |
| 上限引分 | 0 | 0 |

通常勝利/全対局: 0/12 → 0/12
総対局ターン: 720 → 720
思考時間の標本平均(ms): 2475.32 → 4572.29
思考時間の標本数: 720 → 720 （未記録対局: 0 → 0）

指標の観測時点: T15の実測値。N/Aは未観測。
終局時の数値だけから改善・悪化を判定しない。同時要件は別途検証する。

| Seed | 基準（結果 / 終局 / 指標） | 候補（結果 / 終局 / 指標） | 通常勝利の変化 |
|---|---|---|---|
| 1 | P2_Win_MaxTurns / T60 / ZOC 148 / 拠点 19 / 収入 22000 | P2_Win_MaxTurns / T60 / ZOC 152 / 拠点 19 / 収入 22000 | — |
| 2 | P2_Win_MaxTurns / T60 / ZOC 151 / 拠点 19 / 収入 22000 | P2_Win_MaxTurns / T60 / ZOC 159 / 拠点 19 / 収入 22000 | — |
| 3 | P1_Win_MaxTurns / T60 / ZOC 144 / 拠点 19 / 収入 22000 | P2_Win_MaxTurns / T60 / ZOC 140 / 拠点 19 / 収入 22000 | — |
| 4 | P2_Win_MaxTurns / T60 / ZOC 129 / 拠点 19 / 収入 22000 | P2_Win_MaxTurns / T60 / ZOC 159 / 拠点 19 / 収入 22000 | — |
| 5 | P1_Win_MaxTurns / T60 / ZOC 169 / 拠点 19 / 収入 22000 | P2_Win_MaxTurns / T60 / ZOC 154 / 拠点 19 / 収入 22000 | — |
| 6 | P1_Win_MaxTurns / T60 / ZOC 157 / 拠点 19 / 収入 22000 | P2_Win_MaxTurns / T60 / ZOC 141 / 拠点 20 / 収入 23000 | — |
| 7 | P2_Win_MaxTurns / T60 / ZOC 168 / 拠点 19 / 収入 22000 | P2_Win_MaxTurns / T60 / ZOC 159 / 拠点 19 / 収入 22000 | — |
| 8 | P2_Win_MaxTurns / T60 / ZOC 135 / 拠点 19 / 収入 22000 | P2_Win_MaxTurns / T60 / ZOC 173 / 拠点 19 / 収入 22000 | — |
| 9 | P2_Win_MaxTurns / T60 / ZOC 167 / 拠点 19 / 収入 22000 | P2_Win_MaxTurns / T60 / ZOC 148 / 拠点 19 / 収入 22000 | — |
| 10 | P2_Win_MaxTurns / T60 / ZOC 168 / 拠点 19 / 収入 22000 | P1_Win_MaxTurns / T60 / ZOC 153 / 拠点 19 / 収入 22000 | — |
| 11 | P1_Win_MaxTurns / T60 / ZOC 149 / 拠点 19 / 収入 22000 | P1_Win_MaxTurns / T60 / ZOC 135 / 拠点 19 / 収入 22000 | — |
| 12 | P2_Win_MaxTurns / T60 / ZOC 172 / 拠点 19 / 収入 22000 | P2_Win_MaxTurns / T60 / ZOC 154 / 拠点 19 / 収入 22000 | — |

生産総数は対局長と合わせて読む。

| 兵種 | 基準 | 候補 |
|---|---:|---:|
| Artillery | 45 | 40 |
| HeavySpGun | 62 | 92 |
| Infantry | 2009 | 2147 |
| LightSpGun | 181 | 181 |
| MdTank | 233 | 182 |
| Mech | 827 | 850 |
| Rockets | 345 | 348 |
| Tank | 383 | 367 |
| TankZ | 53 | 105 |
