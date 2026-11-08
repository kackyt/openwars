use crate::components::GridPosition;
use crate::resources::{DamageChart, Map, UnitType};

/// 脅威評価に必要な敵ユニットの最小スナップショット。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnemyThreatInfo {
    pub position: GridPosition,
    pub unit_type: UnitType,
    pub strategic_value: u32,
    pub hp: u32,
    pub min_range: u32,
    pub max_range: u32,
    pub max_movement: u32,
    pub ammo: (u32, u32),
}

impl EnemyThreatInfo {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        position: GridPosition,
        unit_type: UnitType,
        strategic_value: u32,
        hp: u32,
        min_range: u32,
        max_range: u32,
        max_movement: u32,
        ammo: (u32, u32),
    ) -> Self {
        Self {
            position,
            unit_type,
            strategic_value,
            hp,
            min_range,
            max_range,
            max_movement,
            ammo,
        }
    }
}

const EXPOSURE_RISK_NUM: i32 = 1;
const EXPOSURE_RISK_DEN: i32 = 1;

/// 本番と同じ武器選択・HP・地形防御式で、敵一体の射撃を評価する。
fn shot_damage(
    enemy: &EnemyThreatInfo,
    target: UnitType,
    distance: u32,
    defense: u32,
    chart: &DamageChart,
    registry: &crate::resources::master_data::MasterDataRegistry,
) -> u32 {
    if enemy.hp == 0 {
        return 0;
    }
    let Some((slot, fallback, _)) = crate::systems::combat::select_weapon(
        enemy.ammo.0,
        enemy.ammo.1,
        enemy.unit_type.as_str(),
        target.as_str(),
        distance,
        registry,
    ) else {
        return 0;
    };
    let base = if slot == 1 {
        chart.get_base_damage(enemy.unit_type, target)
    } else {
        chart.get_base_damage_secondary(enemy.unit_type, target)
    }
    .unwrap_or(fallback);
    crate::systems::combat::calculate_damage_formula(base, enemy.hp, defense, false)
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
    registry: &crate::resources::master_data::MasterDataRegistry,
) -> u32 {
    let mut total_damage = 0;
    for enemy in enemy_units {
        if enemy.min_range <= 1 {
            continue;
        }
        let distance = map.distance(enemy.position.x, enemy.position.y, tile.0, tile.1);
        if distance < enemy.min_range || distance > enemy.max_range {
            continue;
        }
        total_damage += shot_damage(
            enemy,
            my_unit_type,
            distance,
            tile_def_bonus,
            damage_chart,
            registry,
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
    registry: &crate::resources::master_data::MasterDataRegistry,
) -> u32 {
    let mut total_damage = 0;
    for enemy in enemy_units {
        // 間接攻撃ユニットは移動後に射撃できないため、到達圏の計算から除外する。
        if enemy.min_range > 1 {
            continue;
        }
        let distance = map.distance(enemy.position.x, enemy.position.y, tile.0, tile.1);
        if distance > enemy.max_movement + enemy.max_range {
            continue;
        }
        // 現在の直接攻撃到達圏の概算は維持し、移動で接近した後の距離で武器を選ぶ。
        let firing_distance = distance.saturating_sub(enemy.max_movement).max(1);
        total_damage += shot_damage(
            enemy,
            my_unit_type,
            firing_distance,
            tile_def_bonus,
            damage_chart,
            registry,
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
    registry: &crate::resources::master_data::MasterDataRegistry,
) -> i32 {
    expected_loss_value(
        indirect_fire_expected_damage(
            map,
            tile,
            my_unit_type,
            tile_def_bonus,
            enemy_units,
            damage_chart,
            registry,
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
    registry: &crate::resources::master_data::MasterDataRegistry,
) -> i32 {
    let mut total_damage = indirect_fire_expected_damage(
        map,
        tile,
        my_unit_type,
        tile_def_bonus,
        enemy_units,
        damage_chart,
        registry,
    );

    if my_min_range > 1 {
        total_damage += direct_reach_expected_damage(
            map,
            tile,
            my_unit_type,
            tile_def_bonus,
            enemy_units,
            damage_chart,
            registry,
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
    registry: &crate::resources::master_data::MasterDataRegistry,
) -> i32 {
    if tile_def_bonus == 0 {
        return 0;
    }
    let bare = direct_reach_expected_damage(
        map,
        tile,
        my_unit_type,
        0,
        enemy_units,
        damage_chart,
        registry,
    );
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
        registry,
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
    registry: &crate::resources::master_data::MasterDataRegistry,
) -> u32 {
    indirect_fire_expected_damage(
        map,
        tile,
        my_unit_type,
        tile_def_bonus,
        enemy_units,
        damage_chart,
        registry,
    ) + direct_reach_expected_damage(
        map,
        tile,
        my_unit_type,
        tile_def_bonus,
        enemy_units,
        damage_chart,
        registry,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wounded_indirect_threat_uses_current_hp() {
        let map = Map::new(
            6,
            1,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::HeavySpGun, UnitType::Tank, 92);
        let enemy = [EnemyThreatInfo::new(
            GridPosition { x: 0, y: 0 },
            UnitType::HeavySpGun,
            16500,
            60,
            3,
            5,
            5,
            (6, 0),
        )];
        // 損傷砲を満HP扱いせず、本番の乱数なし期待ダメージ式と一致させる。
        let expected = crate::systems::combat::calculate_damage_formula(92, 60, 0, false);
        assert_eq!(
            indirect_fire_expected_damage(
                &map,
                (5, 0),
                UnitType::Tank,
                0,
                &enemy,
                &chart,
                &registry
            ),
            expected
        );
    }

    #[test]
    fn indirect_threat_with_no_ammo_is_zero() {
        let registry = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let map = Map::new(
            6,
            1,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::HeavySpGun, UnitType::Tank, 92);
        let enemy = EnemyThreatInfo::new(
            GridPosition { x: 0, y: 0 },
            UnitType::HeavySpGun,
            16500,
            100,
            3,
            5,
            5,
            (0, 0),
        );
        assert_eq!(
            shot_damage(&enemy, UnitType::Tank, 5, 0, &chart, &registry),
            0
        );
        assert_eq!(
            indirect_fire_expected_loss(
                &map,
                (5, 0),
                UnitType::Tank,
                6000,
                100,
                0,
                &[enemy],
                &chart,
                &registry
            ),
            0
        );
    }

    #[test]
    fn threat_selects_secondary_only_when_legal_and_uses_current_hp_and_defense() {
        let registry = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Tank, UnitType::Infantry, 80);
        chart.insert_secondary_damage(UnitType::Tank, UnitType::Infantry, 30);
        let mut enemy = EnemyThreatInfo::new(
            GridPosition { x: 0, y: 0 },
            UnitType::Tank,
            6000,
            60,
            1,
            1,
            4,
            (0, 9),
        );
        // 主兵装が弾切れでも、合法な副兵装が残れば本番と同じ副兵装のダメージになる。
        let expected = crate::systems::combat::calculate_damage_formula(30, 60, 20, false);
        assert_eq!(
            shot_damage(&enemy, UnitType::Infantry, 1, 20, &chart, &registry),
            expected
        );
        assert_eq!(
            shot_damage(&enemy, UnitType::Infantry, 2, 20, &chart, &registry),
            0
        );
        enemy.ammo = (0, 0);
        assert_eq!(
            shot_damage(&enemy, UnitType::Infantry, 1, 20, &chart, &registry),
            0
        );
        enemy.ammo = (6, 9);
        assert_eq!(
            shot_damage(&enemy, UnitType::Infantry, 1, 20, &chart, &registry),
            crate::systems::combat::calculate_damage_formula(80, 60, 20, false)
        );
        // 武器が対応しない航空機は、チャートに値があっても射撃不能。
        enemy.unit_type = UnitType::HeavySpGun;
        chart.insert_damage(UnitType::HeavySpGun, UnitType::Bomber, 99);
        assert_eq!(
            shot_damage(&enemy, UnitType::Bomber, 4, 0, &chart, &registry),
            0
        );
    }

    #[test]
    fn indirect_fire_loss_is_zero_outside_range() {
        let map = Map::new(
            5,
            1,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Artillery, UnitType::Infantry, 50);
        let enemy = [EnemyThreatInfo::new(
            GridPosition { x: 0, y: 0 },
            UnitType::Artillery,
            6000,
            100,
            2,
            3,
            5,
            (6, 9),
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
                &registry,
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
                &registry,
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
        let registry = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Tank, UnitType::Tank, 50);
        // 移動力4・射程1の敵戦車は格子距離5まで届く。
        let enemy = [EnemyThreatInfo::new(
            GridPosition { x: 0, y: 0 },
            UnitType::Tank,
            7000,
            100,
            1,
            1,
            4,
            (6, 9),
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
                &registry,
            )
        };

        // 期待被害50 → 森(20%)で40。避けられる10HP分 × 7000/100 = 700。
        // 本番式: 地形補正は(100+防御)を分母に適用する。
        let damage =
            |defense| crate::systems::combat::calculate_damage_formula(50, 100, defense, false);
        assert_eq!(relief((3, 0), 20), ((damage(0) - damage(20)) * 70) as i32);
        assert_eq!(relief((3, 0), 40), ((damage(0) - damage(40)) * 70) as i32);
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
        let registry = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Artillery, UnitType::Infantry, 50);
        chart.insert_damage(UnitType::Tank, UnitType::Infantry, 60);
        let enemy = [
            EnemyThreatInfo::new(
                GridPosition { x: 0, y: 0 },
                UnitType::Artillery,
                6000,
                100,
                2,
                3,
                5,
                (6, 9),
            ),
            EnemyThreatInfo::new(
                GridPosition { x: 4, y: 0 },
                UnitType::Tank,
                7000,
                100,
                1,
                1,
                3, // 移動3＋射程1＝到達距離4。(4,0)→(2,0)は距離2なので到達圏内
                (6, 9),
            ),
        ];

        // タイル(2,0)：砲兵は射程[2,3]で距離2（射程内、50）。戦車は到達圏内（60）。合計110。
        let total_dmg = expected_incoming_damage(
            &map,
            (3, 0),
            UnitType::Infantry,
            0,
            &enemy,
            &chart,
            &registry,
        );
        assert_eq!(total_dmg, 112);
    }
}
