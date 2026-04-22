with open("engine/src/ai/production.rs", "r") as f:
    text = f.read()

import re

# 1. 1000 buffer for repair/supply -> make it a const
text = re.sub(
    r'let available_funds = current_funds\.saturating_sub\(1000\);',
    r'// 補充や修理のための予備資金として1000Gを残す\n    const RESERVE_FUNDS: u32 = 1000;\n    let available_funds = current_funds.saturating_sub(RESERVE_FUNDS);',
    text
)

# 2. Score bonus for infantry -> make it a const and explain
text = re.sub(
    r'score \+= \(10 - my_infantry_count\) \* 1000;',
    r'// 歩兵が足りない場合、1体につき1000点（歩兵の標準コスト相当）のボーナスを与えて生産を促進する\n                const INFANTRY_SHORTAGE_BONUS: u32 = 1000;\n                score += (10 - my_infantry_count) * INFANTRY_SHORTAGE_BONUS;',
    text
)

# 3. Infantry place score calculation -> distance max (999), calculate max score 1000
text = re.sub(
    r'let mut min_dist = 999;\n(.*?)if min_dist < 999 \{\n\s*place_score \+= 1000 - min_dist \* 10;\n\s*\}',
    r'let mut min_dist = i32::MAX;\n\g<1>if min_dist != i32::MAX {\n                    // 距離が近いほど高い評価を与える（最大1000点、1マス離れるごとに10点減点）\n                    // 1000点は初期の歩兵ボーナスと同等スケールにするための基準値\n                    const MAX_PLACE_SCORE: i32 = 1000;\n                    const DISTANCE_PENALTY: i32 = 10;\n                    place_score += MAX_PLACE_SCORE - (min_dist as i32 * DISTANCE_PENALTY);\n                }',
    text,
    flags=re.DOTALL
)

with open("engine/src/ai/production.rs", "w") as f:
    f.write(text)
