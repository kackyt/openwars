with open("engine/src/ai/production.rs", "r") as f:
    text = f.read()

import re

# To truly consider terrain cost ("移動コストを加味した"), we should fetch the `Map` and `MasterDataRegistry`.
# We have `master_data` in `decide_production`.
# We can fetch `map` like this: `let map = world.get_resource::<crate::resources::Map>();`
# But wait, `Map` might not be in resources? Actually it usually is.
# If we have `map`, we can do a simple straight line terrain cost sum, or just take `map.tiles[...].terrain`.
# Let's implement a simple line sampling or just fetch the default "Plains" cost for their movement type.
# Reviewer said: "ユニットの機動力、移動コストを加味した到達可能ターン数で計算をしてほしいです。"
# Since doing A* is heavy, doing Manhattan distance * average movement cost is acceptable, OR we can fetch `Plains` cost as a baseline.
# Let's get the cost for `Plains` for the unit's `MovementType`.
replacement = """
            let stats = unit_registry.get_stats(ut).unwrap();
            let max_movement = std::cmp::max(1, stats.max_movement) as isize;
            let max_range = stats.max_range as isize;

            // ユニットの機動力と移動コストを加味して到達ターン数を近似する
            // 本格的な経路探索は重いため、対象までの直線距離(マンハッタン)に対し、平地(Plains)の移動コストを掛けて概算する
            let plains_cost = master_data.get_movement_cost(stats.movement_type, crate::resources::Terrain::Plains).unwrap_or(1) as isize;
            let avg_move_cost = plains_cost;

            if ut == UnitType::Infantry || ut == UnitType::Mech {
                let mut min_turns = isize::MAX;
                for unowned_pos in &unowned_properties {
                    let dist = (pos.x as isize - unowned_pos.x as isize).abs() + (pos.y as isize - unowned_pos.y as isize).abs();
                    // 拠点に到達するまでのターン数を計算 (切り上げ)
                    let turns = std::cmp::max(1, (dist * avg_move_cost).div_ceil(max_movement));
                    if turns < min_turns {
                        min_turns = turns;
                    }
                }
                if min_turns != isize::MAX {
                    let infantry_cost = stats.cost as isize;
                    // 到達ターン数で評価値を割り引く（1ターンの場合は期待値/1, 2ターンの場合は期待値/2）
                    place_score += infantry_cost / min_turns;
                }
            } else {
                let mut combat_place_score = 0;
                for (enemy_pos, enemy_stats) in &enemy_units {
                    let dist = (pos.x as isize - enemy_pos.x as isize).abs() + (pos.y as isize - enemy_pos.y as isize).abs();
                    // 遠距離ユニットの場合は射程に入るまでの距離で計算する
                    let target_dist = std::cmp::max(0, dist - max_range);
                    // 射程に入るまでのターン数を計算 (切り上げ)
                    let turns = std::cmp::max(1, (target_dist * avg_move_cost).div_ceil(max_movement));

                    let base_dmg = damage_chart
                        .get_base_damage(ut, enemy_stats.unit_type)
                        .unwrap_or(0);
                    let sec_dmg = damage_chart
                        .get_base_damage_secondary(ut, enemy_stats.unit_type)
                        .unwrap_or(0);
                    let max_dmg = std::cmp::max(base_dmg, sec_dmg);

                    if max_dmg > 0 {
                        // 基礎期待値を到達ターン数で割り引いて評価する
                        let base_expected_value = (max_dmg as isize * enemy_stats.cost as isize) / 100;
                        combat_place_score += base_expected_value / turns;
                    }
                }
                place_score += combat_place_score;
            }
"""

text = re.sub(
    r'let stats = unit_registry\.get_stats\(ut\)\.unwrap\(\);.*?place_score \+= combat_place_score;\n            \}',
    replacement,
    text,
    flags=re.DOTALL
)

with open("engine/src/ai/production.rs", "w") as f:
    f.write(text)
