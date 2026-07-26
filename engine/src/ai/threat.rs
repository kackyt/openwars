use crate::components::GridPosition;
use crate::resources::{DamageChart, Map, UnitType};

/// 脅威評価に必要な敵ユニットの最小スナップショット。
pub type EnemyThreatInfo = (GridPosition, UnitType, u32, u32, u32, u32, u32);

const EXPOSURE_RISK_NUM: i32 = 1;
const EXPOSURE_RISK_DEN: i32 = 1;

fn expected_loss_value(total_damage: u32, my_cost: u32, my_hp: u32) -> i32 {
    let effective_damage = total_damage.min(my_hp);
    (effective_damage * my_cost / 100) as i32
}

fn indirect_fire_expected_damage(
    map: &Map,
    tile: (usize, usize),
    my_unit_type: UnitType,
    tile_def_bonus: u32,
    enemy_units: &[EnemyThreatInfo],
    damage_chart: &DamageChart,
) -> u32 {
    let mut total_damage = 0;
    for (enemy_position, enemy_type, _, _, min_range, max_range, _) in enemy_units {
        if *min_range <= 1 {
            continue;
        }
        let distance = map.distance(enemy_position.x, enemy_position.y, tile.0, tile.1);
        if distance < *min_range || distance > *max_range {
            continue;
        }
        total_damage += damage_chart
            .get_base_damage(*enemy_type, my_unit_type)
            .or_else(|| damage_chart.get_base_damage_secondary(*enemy_type, my_unit_type))
            .unwrap_or(0)
            * (100 - tile_def_bonus.min(100))
            / 100;
    }
    total_damage
}

/// 指定タイルが敵間接攻撃ユニットの現在射程内にある場合の期待損失額を返します。
/// 上陸地点選定と通常の移動評価で同じ被害予測を共有するための純粋関数です。
#[allow(clippy::too_many_arguments)]
pub fn indirect_fire_expected_loss(
    map: &Map,
    tile: (usize, usize),
    my_unit_type: UnitType,
    my_cost: u32,
    my_hp: u32,
    tile_def_bonus: u32,
    enemy_units: &[EnemyThreatInfo],
    damage_chart: &DamageChart,
) -> i32 {
    expected_loss_value(
        indirect_fire_expected_damage(
            map,
            tile,
            my_unit_type,
            tile_def_bonus,
            enemy_units,
            damage_chart,
        ),
        my_cost,
        my_hp,
    )
}

/// V3 の通常移動で使用する露出ペナルティを計算します。
/// 間接砲火は全ユニットへ、直接攻撃の踏み込みは反撃不能な間接攻撃ユニットだけへ適用します。
#[allow(clippy::too_many_arguments)]
pub fn exposure_penalty(
    map: &Map,
    tile: (usize, usize),
    my_unit_type: UnitType,
    my_cost: u32,
    my_hp: u32,
    my_min_range: u32,
    tile_def_bonus: u32,
    enemy_units: &[EnemyThreatInfo],
    damage_chart: &DamageChart,
) -> i32 {
    let mut total_damage = indirect_fire_expected_damage(
        map,
        tile,
        my_unit_type,
        tile_def_bonus,
        enemy_units,
        damage_chart,
    );

    if my_min_range > 1 {
        let mut direct_damage = 0;
        for (enemy_position, enemy_type, _, _, min_range, max_range, max_movement) in enemy_units {
            if *min_range > 1 {
                continue;
            }
            let distance = map.distance(enemy_position.x, enemy_position.y, tile.0, tile.1);
            if distance > max_movement + max_range {
                continue;
            }
            direct_damage += damage_chart
                .get_base_damage(*enemy_type, my_unit_type)
                .or_else(|| damage_chart.get_base_damage_secondary(*enemy_type, my_unit_type))
                .unwrap_or(0)
                * (100 - tile_def_bonus.min(100))
                / 100;
        }
        total_damage += direct_damage;
    }

    expected_loss_value(total_damage, my_cost, my_hp) * EXPOSURE_RISK_NUM / EXPOSURE_RISK_DEN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indirect_fire_loss_is_zero_outside_range() {
        let map = Map::new(
            5,
            1,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Artillery, UnitType::Infantry, 50);
        let enemy = [(
            GridPosition { x: 0, y: 0 },
            UnitType::Artillery,
            6000,
            100,
            2,
            3,
            5,
        )];

        assert_eq!(
            indirect_fire_expected_loss(
                &map,
                (4, 0),
                UnitType::Infantry,
                1000,
                100,
                0,
                &enemy,
                &chart,
            ),
            0
        );
        assert!(
            indirect_fire_expected_loss(
                &map,
                (3, 0),
                UnitType::Infantry,
                1000,
                100,
                0,
                &enemy,
                &chart,
            ) > 0
        );
    }
}
