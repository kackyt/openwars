with open("engine/src/ai/production.rs", "r") as f:
    text = f.read()

import re

# 1. 1000 buffer for repair/supply -> make it a const
text = re.sub(
    r'const INFANTRY_SHORTAGE_BONUS: u32 = 1000;',
    r'let infantry_cost = unit_registry.get_stats(UnitType::Infantry).map(|s| s.cost).unwrap_or(1000);\n                // 歩兵が足りない場合、1体につき歩兵の標準コスト相当のボーナスを与えて生産を促進する\n                let infantry_shortage_bonus = infantry_cost;',
    text
)

text = re.sub(
    r'score \+= \(10 - my_infantry_count\) \* INFANTRY_SHORTAGE_BONUS;',
    r'score += (10 - my_infantry_count) * infantry_shortage_bonus;',
    text
)

text = re.sub(
    r'const MAX_PLACE_SCORE: isize = 1000;',
    r'let infantry_cost = unit_registry.get_stats(UnitType::Infantry).map(|s| s.cost).unwrap_or(1000) as isize;\n                    // 距離が近いほど高い評価を与える（最大コストと同等、1マス離れるごとに10点減点）\n                    let max_place_score = infantry_cost;',
    text
)

text = re.sub(
    r'place_score \+= MAX_PLACE_SCORE - \(min_dist \* DISTANCE_PENALTY\);',
    r'place_score += max_place_score - (min_dist * DISTANCE_PENALTY);',
    text
)


with open("engine/src/ai/production.rs", "w") as f:
    f.write(text)
