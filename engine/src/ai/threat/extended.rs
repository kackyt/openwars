//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

/// 指定タイルにおいて敵から受ける期待総被ダメージ（HP換算）を計算します。
/// 間接砲火および直接攻撃到達圏の敵からの推定ダメージを合算し、
/// 突入時に次ターンで即死（被撃破）するかどうかの生存性を判定するために用います。
pub fn expected_incoming_damage(
    map: &Map,
    tile: (usize, usize),
    my_unit_type: UnitType,
    tile_def_bonus: u32,
    enemy_units: &[EnemyThreatInfo],
    damage_chart: &DamageChart,
) -> u32 {
    let mut total_damage = indirect_fire_expected_damage(
        map,
        tile,
        my_unit_type,
        tile_def_bonus,
        enemy_units,
        damage_chart,
    );

    for (enemy_position, enemy_type, _, _, min_range, max_range, max_movement) in enemy_units {
        if *min_range > 1 {
            continue;
        }
        let distance = map.distance(enemy_position.x, enemy_position.y, tile.0, tile.1);
        if distance > max_movement + max_range {
            continue;
        }
        let direct_damage = damage_chart
            .get_base_damage(*enemy_type, my_unit_type)
            .or_else(|| damage_chart.get_base_damage_secondary(*enemy_type, my_unit_type))
            .unwrap_or(0)
            * (100 - tile_def_bonus.min(100))
            / 100;
        total_damage += direct_damage;
    }

    total_damage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_incoming_damage_sums_threats() {
        let map = Map::new(
            5,
            1,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Artillery, UnitType::Infantry, 50);
        chart.insert_damage(UnitType::Tank, UnitType::Infantry, 60);
        let enemy = [
            (
                GridPosition { x: 0, y: 0 },
                UnitType::Artillery,
                6000,
                100,
                2,
                3,
                5,
            ),
            (
                GridPosition { x: 4, y: 0 },
                UnitType::Tank,
                7000,
                100,
                1,
                1,
                3, // movement 3 + range 1 = can reach (1, 0)
            ),
        ];

        // tile (2, 0): Artillery range [2, 3] from 0 is at dist 2 (in range, 50 dmg).
        // Tank at 4 with move 3 + range 1 = dist 2 <= 4 (in reach, 60 dmg).
        let total_dmg =
            expected_incoming_damage(&map, (2, 0), UnitType::Infantry, 0, &enemy, &chart);
        assert_eq!(total_dmg, 110);
    }
}
