# 対戦スイープ比較

変種: baseline, opening ／ 繰返し: 1

判定は各変種で出現した action_history ハッシュの集合を比べたもの。
通常経路のマップは同じバイナリでも実行ごとに行動列が分かれることがあるため、
1回ずつの比較ではなく分布（集合と出現回数）で判断する。

- ✅ 集合が一致：この繰返し回数では挙動差なし。
- 🟡 共通あり：集合は違うが共通の行動列がある。実行ごとの揺れを拾いきれていない可能性が
  高いので、--repeats を増やして再確認する。これだけで退行と判断しない。
- ❌ 共通なし：どの回も別の行動列。挙動差の疑いが強いので、分岐した手を調べる。

| マップ | seed | P1 | P2 | baseline | opening | 判定 |
|---|---|---|---|---|---|---|
| map_20 | 1 | V200 | V4 | P2_Win_MaxTurns 30T×1<br>`1ef95f6e×1` | P1_Win_MaxTurns 30T×1<br>`5bf2ab53×1` | ❌ |
| map_20 | 2 | V200 | V4 | P2_Win_MaxTurns 30T×1<br>`ccdb9f82×1` | P1_Win_MaxTurns 30T×1<br>`df43e269×1` | ❌ |
| map_20 | 3 | V200 | V4 | P1_Win_MaxTurns 30T×1<br>`943efc70×1` | P1_Win_MaxTurns 30T×1<br>`957da71d×1` | ❌ |
| map_35 | 1 | V200 | V4 | P2_Win_MaxTurns 30T×1<br>`fa46fb24×1` | P2_Win_MaxTurns 30T×1<br>`fa46fb24×1` | ✅ |
| map_35 | 2 | V200 | V4 | P2_Win_MaxTurns 30T×1<br>`d1da99a8×1` | P2_Win_MaxTurns 30T×1<br>`d1da99a8×1` | ✅ |
| map_35 | 3 | V200 | V4 | P2_Win_MaxTurns 30T×1<br>`a27db55b×1` | P2_Win_MaxTurns 30T×1<br>`a27db55b×1` | ✅ |
| map_8 | 1 | V200 | V4 | P2_Win_MaxTurns 30T×1<br>`c5bb20dd×1` | P2_Win_MaxTurns 30T×1<br>`c5bb20dd×1` | ✅ |
| map_8 | 2 | V200 | V4 | P1_Win_MaxTurns 30T×1<br>`aac70381×1` | P1_Win_MaxTurns 30T×1<br>`aac70381×1` | ✅ |
| map_8 | 3 | V200 | V4 | P1_Win_MaxTurns 30T×1<br>`a801c4a2×1` | P1_Win_MaxTurns 30T×1<br>`a801c4a2×1` | ✅ |
