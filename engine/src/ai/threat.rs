use crate::components::GridPosition;
use crate::resources::{DamageChart, Map, UnitType};

/// 脅威評価に必要な敵ユニットの最小スナップショット。
pub type EnemyThreatInfo = (GridPosition, UnitType, u32, u32, u32, u32, u32);

const EXPOSURE_RISK_NUM: i32 = 1;
const EXPOSURE_RISK_DEN: i32 = 1;

/// 地形防御補正（百分率）の分母。
const PERCENT_SCALE: u32 = 100;

/// 基本ダメージへ地形防御補正（百分率、上限100）を適用します。
fn apply_terrain_defense(base_damage: u32, tile_def_bonus: u32) -> u32 {
    base_damage * (PERCENT_SCALE - tile_def_bonus.min(PERCENT_SCALE)) / PERCENT_SCALE
}

/// 敵兵種から自兵種への基本ダメージ（主兵装、なければ副兵装）を返します。
fn base_damage(damage_chart: &DamageChart, enemy_type: UnitType, my_unit_type: UnitType) -> u32 {
    damage_chart
        .get_base_damage(enemy_type, my_unit_type)
        .or_else(|| damage_chart.get_base_damage_secondary(enemy_type, my_unit_type))
        .unwrap_or(0)
}

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
        total_damage += apply_terrain_defense(
            base_damage(damage_chart, *enemy_type, my_unit_type),
            tile_def_bonus,
        );
    }
    total_damage
}

/// 移動力＋射程で指定タイルへ届く直接攻撃ユニットからの期待被ダメージ合計を返します。
fn direct_reach_expected_damage(
    map: &Map,
    tile: (usize, usize),
    my_unit_type: UnitType,
    tile_def_bonus: u32,
    enemy_units: &[EnemyThreatInfo],
    damage_chart: &DamageChart,
) -> u32 {
    let mut total_damage = 0;
    for (enemy_position, enemy_type, _, _, min_range, max_range, max_movement) in enemy_units {
        // 間接攻撃ユニットは移動後に射撃できないため、到達圏の計算から除外する。
        if *min_range > 1 {
            continue;
        }
        let distance = map.distance(enemy_position.x, enemy_position.y, tile.0, tile.1);
        if distance > max_movement + max_range {
            continue;
        }
        total_damage += apply_terrain_defense(
            base_damage(damage_chart, *enemy_type, my_unit_type),
            tile_def_bonus,
        );
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
        total_damage += direct_reach_expected_damage(
            map,
            tile,
            my_unit_type,
            tile_def_bonus,
            enemy_units,
            damage_chart,
        );
    }

    expected_loss_value(total_damage, my_cost, my_hp) * EXPOSURE_RISK_NUM / EXPOSURE_RISK_DEN
}

/// 敵の直接攻撃が届くタイルで、地形防御によって避けられる期待損失額を返します。
/// 地形防御0のときと実際の地形との期待損失の差で、直接攻撃圏の外では0になります。
/// 露出そのものは減点しないため、前進の評価は変えずに、前線での停止位置だけを
/// 防御地形へ寄せられます。
#[allow(clippy::too_many_arguments)]
pub fn direct_reach_terrain_relief(
    map: &Map,
    tile: (usize, usize),
    my_unit_type: UnitType,
    my_cost: u32,
    my_hp: u32,
    tile_def_bonus: u32,
    enemy_units: &[EnemyThreatInfo],
    damage_chart: &DamageChart,
) -> i32 {
    if tile_def_bonus == 0 {
        return 0;
    }
    let bare = direct_reach_expected_damage(map, tile, my_unit_type, 0, enemy_units, damage_chart);
    if bare == 0 {
        return 0;
    }
    let defended = direct_reach_expected_damage(
        map,
        tile,
        my_unit_type,
        tile_def_bonus,
        enemy_units,
        damage_chart,
    );
    expected_loss_value(bare, my_cost, my_hp) - expected_loss_value(defended, my_cost, my_hp)
}

/// 指定タイルにおいて敵から受ける期待総被ダメージ（HP換算）を計算します。
/// 間接砲火および直接攻撃到達圏の敵からの推定ダメージを合算し、
/// 突入時に次ターンで即死（被撃破）するかどうかの生存性を判定するために用います。
pub(crate) fn expected_incoming_damage(
    map: &Map,
    tile: (usize, usize),
    my_unit_type: UnitType,
    tile_def_bonus: u32,
    enemy_units: &[EnemyThreatInfo],
    damage_chart: &DamageChart,
) -> u32 {
    indirect_fire_expected_damage(
        map,
        tile,
        my_unit_type,
        tile_def_bonus,
        enemy_units,
        damage_chart,
    ) + direct_reach_expected_damage(
        map,
        tile,
        my_unit_type,
        tile_def_bonus,
        enemy_units,
        damage_chart,
    )
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

    #[test]
    fn direct_reach_terrain_relief_counts_only_avoided_loss_in_reach() {
        let map = Map::new(
            10,
            1,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Tank, UnitType::Tank, 50);
        // 移動力4・射程1の敵戦車は格子距離5まで届く。
        let enemy = [(
            GridPosition { x: 0, y: 0 },
            UnitType::Tank,
            7000,
            100,
            1,
            1,
            4,
        )];
        let relief = |tile, defense| {
            direct_reach_terrain_relief(
                &map,
                tile,
                UnitType::Tank,
                7000,
                100,
                defense,
                &enemy,
                &chart,
            )
        };

        // 期待被害50 → 森(20%)で40。避けられる10HP分 × 7000/100 = 700。
        assert_eq!(relief((3, 0), 20), 700);
        assert_eq!(relief((3, 0), 40), 1400);
        // 地形防御0、または直接攻撃圏の外では0。
        assert_eq!(relief((3, 0), 0), 0);
        assert_eq!(relief((6, 0), 40), 0);
    }

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
                3, // 移動3＋射程1＝到達距離4。(4,0)→(2,0)は距離2なので到達圏内
            ),
        ];

        // タイル(2,0)：砲兵は射程[2,3]で距離2（射程内、50）。戦車は到達圏内（60）。合計110。
        let total_dmg =
            expected_incoming_damage(&map, (2, 0), UnitType::Infantry, 0, &enemy, &chart);
        assert_eq!(total_dmg, 110);
    }
}
