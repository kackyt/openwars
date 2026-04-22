with open("engine/src/ai/production.rs", "r") as f:
    text = f.read()

import re

# 1. 1000 buffer for repair/supply -> make it a const
text = re.sub(
    r'combat_score \+= \(max_dmg \* enemy_stats\.cost\) / 100;',
    r'// ダメージはパーセンテージ（0-100）であるため、敵のコストに掛けて100で割ることで、実質的なダメージ金額価値を算出する\n            combat_score += (max_dmg * enemy_stats.cost) / 100;',
    text
)

text = re.sub(
    r'let budget = \(available_funds / 100\) as usize;',
    r'// 計算量を削減するため、予算とコストを100G単位にスケールダウンしてDPテーブルを構築する\n    let budget = (available_funds / 100) as usize;',
    text
)

with open("engine/src/ai/production.rs", "w") as f:
    f.write(text)
