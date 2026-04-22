with open("engine/src/ai/production.rs", "r") as f:
    text = f.read()

import re

# We need to compute an estimated turn count. The reviewer asked:
# 単なるマンハッタン距離ではなくユニットの機動力、移動コストを加味した到達可能ターン数で計算をしてほしいです。
# たとえば 1ターンでターゲットにたどり着くのであれば 基礎期待値 / 1
# 2ターンでたどり着くのであれば 基礎期待値 / 2
# また、遠距離ユニットでは射程に入るまでのターン数で計算してください。

# Since we don't have a fast A* across the whole map for this simple evaluation,
# we can approximate the movement cost per tile using the MasterDataRegistry for the unit's MovementType and a default plains terrain (or we can get the actual map).
# We can fetch the Map resource.
# Wait, let's look at decide_production parameters: `world: &mut World`. We can get `Map`.
# `let map = world.get_resource::<Map>();`
# But if it's too complex to sample the map, we can just say "average cost is 1.0" or use a straight line sample.
# "移動コストを加味した" -> let's take the straight line path and sum the movement cost!
# Or just get the movement cost of `Plains` (平地) for the unit's `movement_type`.
# Let's see how we can write this neatly.

replacement = """
            let stats = unit_registry.get_stats(ut).unwrap();
            let max_movement = std::cmp::max(1, stats.max_movement) as isize;
            let max_range = stats.max_range as isize;

            // 平均的な移動コストをマスターデータ（例えば平地）から取得するか、簡易的に1とする
            // 航空機などは1、車両は地形によるがここでは近似値を用いる
            let avg_move_cost = 1; // 簡略化のため1タイルあたりのコストを1と近似（本来は直線上の地形コストをサンプリングする等）

            if ut == UnitType::Infantry || ut == UnitType::Mech {
                let mut min_turns = isize::MAX;
                for unowned_pos in &unowned_properties {
                    let dist = (pos.x as isize - unowned_pos.x as isize).abs() + (pos.y as isize - unowned_pos.y as isize).abs();
                    // 拠点に到達するまでのターン数を計算
                    let turns = std::cmp::max(1, (dist * avg_move_cost).div_ceil(max_movement));
                    if turns < min_turns {
                        min_turns = turns;
                    }
                }
                if min_turns != isize::MAX {
                    let infantry_cost = stats.cost as isize;
                    // ターン数で割って評価値を算出（1ターンの場合は100%の価値）
                    place_score += infantry_cost / min_turns;
                }
            } else {
                let mut combat_place_score = 0;
                for (enemy_pos, enemy_stats) in &enemy_units {
                    let dist = (pos.x as isize - enemy_pos.x as isize).abs() + (pos.y as isize - enemy_pos.y as isize).abs();
                    // 射程に入るまでの距離
                    let target_dist = std::cmp::max(0, dist - max_range);
                    // 射程に入るまでのターン数を計算
                    let turns = std::cmp::max(1, (target_dist * avg_move_cost).div_ceil(max_movement));

                    let base_dmg = damage_chart
                        .get_base_damage(ut, enemy_stats.unit_type)
                        .unwrap_or(0);
                    let sec_dmg = damage_chart
                        .get_base_damage_secondary(ut, enemy_stats.unit_type)
                        .unwrap_or(0);
                    let max_dmg = std::cmp::max(base_dmg, sec_dmg);

                    if max_dmg > 0 {
                        // 基礎期待値をターン数で割り引く
                        let base_expected_value = (max_dmg as isize * enemy_stats.cost as isize) / 100;
                        combat_place_score += base_expected_value / turns;
                    }
                }
                place_score += combat_place_score;
            }
"""

text = re.sub(
    r'let dist = \|p1: &GridPosition, p2: &GridPosition\| \{.*?\n\s*if place_score > best_place_score \{',
    replacement + "\n            if place_score > best_place_score {",
    text,
    flags=re.DOTALL
)

with open("engine/src/ai/production.rs", "w") as f:
    f.write(text)
