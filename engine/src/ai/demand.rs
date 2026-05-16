use crate::components::{GridPosition, UnitStats};
use crate::resources::{DamageChart, MovementType, Terrain, UnitRegistry, UnitType};

/// ユニットの戦闘カテゴリ。
/// `MovementType` をもとに Ground / Air / Sea に分類します。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitCategory {
    Ground,
    Air,
    Sea,
}

impl UnitCategory {
    pub fn from_movement_type(mt: MovementType) -> Self {
        match mt {
            MovementType::Air => Self::Air,
            MovementType::Ship => Self::Sea,
            _ => Self::Ground,
        }
    }
}

/// ユニットタイプの各カテゴリへの「攻撃適性」（0.0〜1.0）。
/// `DamageChart` から自動算出されます。
#[derive(Debug, Clone, Default)]
pub struct UnitAffinity {
    /// 地上ユニットへの平均攻撃適性
    pub anti_ground: f32,
    /// 航空ユニットへの平均攻撃適性
    pub anti_air: f32,
    /// 海上ユニットへの平均攻撃適性
    pub anti_sea: f32,
}

/// 自軍が直面している「需要の欠け」（0.0〜1.0 に正規化）。
/// 値が 1.0 に近いほど、そのカテゴリへの対応が緊急であることを示します。
#[derive(Debug, Clone, Default)]
pub struct DemandMatrix {
    /// 敵地上部隊に対する反撃能力の不足度
    pub anti_ground: f32,
    /// 敵航空部隊に対する反撃能力の不足度（対空需要）
    pub anti_air: f32,
    /// 敵海上部隊に対する反撃能力の不足度
    pub anti_sea: f32,
    /// 占領可能ユニットの不足度（致死的脅威ベース）
    pub capture: f32,
    /// 輸送力の不足度
    pub logistics: f32,
}

impl DemandMatrix {
    /// 需要マトリクスと適性のドット積を計算します（戦闘カテゴリのみ）。
    /// 結果は [0.0, 3.0] の範囲となります。
    pub fn dot(&self, affinity: &UnitAffinity) -> f32 {
        affinity.anti_ground * self.anti_ground
            + affinity.anti_air * self.anti_air
            + affinity.anti_sea * self.anti_sea
    }
}

/// `DamageChart` を走査し、全ユニット×全カテゴリの平均攻撃期待値を算出します。
/// これを正規化スケールとして使用することで、ユニット追加・変更に自動対応します。
pub fn average_attack_expectation(damage_chart: &DamageChart, unit_registry: &UnitRegistry) -> f32 {
    let mut total = 0.0f32;
    let mut count = 0u32;

    for attacker_type in unit_registry.0.keys() {
        for defender_type in unit_registry.0.keys() {
            // 主武器
            if let Some(dmg) = damage_chart.get_base_damage(*attacker_type, *defender_type) {
                // 攻撃力を持たない組み合わせは除外（分母に入れない）
                if dmg > 0 {
                    total += dmg as f32;
                    count += 1;
                }
            }
        }
    }

    if count == 0 {
        100.0 // フォールバック
    } else {
        (total / count as f32).max(1.0)
    }
}

/// `DamageChart` からユニットタイプの各カテゴリへの攻撃適性を自動算出します。
/// 適性は 0.0〜1.0 に正規化されます。
pub fn compute_unit_affinity(
    unit_type: UnitType,
    damage_chart: &DamageChart,
    unit_registry: &UnitRegistry,
    normalization_scale: f32,
) -> UnitAffinity {
    let mut ground_sum = 0.0f32;
    let mut ground_count = 0u32;
    let mut air_sum = 0.0f32;
    let mut air_count = 0u32;
    let mut sea_sum = 0.0f32;
    let mut sea_count = 0u32;

    for (defender_type, defender_stats) in &unit_registry.0 {
        let category = UnitCategory::from_movement_type(defender_stats.movement_type);

        // 主武器ダメージ
        let primary = damage_chart
            .get_base_damage(unit_type, *defender_type)
            .unwrap_or(0) as f32;
        // 副武器ダメージ
        let secondary = damage_chart
            .get_base_damage_secondary(unit_type, *defender_type)
            .unwrap_or(0) as f32;
        // 高いほうを採用
        let dmg = primary.max(secondary);

        match category {
            UnitCategory::Ground => {
                ground_sum += dmg;
                ground_count += 1;
            }
            UnitCategory::Air => {
                air_sum += dmg;
                air_count += 1;
            }
            UnitCategory::Sea => {
                sea_sum += dmg;
                sea_count += 1;
            }
        }
    }

    let normalize = |sum: f32, count: u32| -> f32 {
        if count == 0 {
            0.0
        } else {
            ((sum / count as f32) / normalization_scale).clamp(0.0, 1.0)
        }
    };

    UnitAffinity {
        anti_ground: normalize(ground_sum, ground_count),
        anti_air: normalize(air_sum, air_count),
        anti_sea: normalize(sea_sum, sea_count),
    }
}

/// 自軍・敵軍の状況から需要マトリクスを計算します。
///
/// # 計算方針
///
/// ## 消耗ギャップ（Attrition Gap）
/// カテゴリ別に「敵の攻撃期待値 - 自軍の反撃期待値」を算出します。
/// 対空が0体で敵ヘリが1体いれば `anti_air` は最大値になります。
///
/// ## 占領脅威（Capture Threat）
/// 敵の占領可能ユニットが重要拠点（首都・工場・空港）にどれだけ近いかを評価します。
/// 拠点の重要度 × 到達しやすさ（1/ETA）の積和です。
pub fn compute_demand(
    my_units: &[(GridPosition, UnitStats)],
    enemy_units: &[(GridPosition, UnitStats)],
    my_properties: &[(GridPosition, Terrain)],
    damage_chart: &DamageChart,
    unit_registry: &UnitRegistry,
) -> DemandMatrix {
    let normalization_scale = average_attack_expectation(damage_chart, unit_registry);

    // --- 消耗ギャップの計算 ---
    // 「敵が持つカテゴリ別の攻撃期待値」と「自軍がそのカテゴリに対して持つ反撃能力」の差を計算する。
    //
    // キー思想：
    //   - 敵に航空ユニット（Bcopters）がいる → 自軍に「対空能力」が必要
    //   - 「対空能力の不足」= 敵航空の攻撃力 - 自軍の anti_air 適性の合計
    //
    // 敵ユニットの脅威 = そのユニットが与えうるダメージ（全カテゴリへの平均）
    // 自軍の反撃能力  = 敵のカテゴリ（Air/Ground/Sea）に有効なユニットの適性合計

    // 敵の各カテゴリ（Air/Ground/Sea）が持つ攻撃の総量
    // （どれだけ強力な脅威が存在するか）
    let mut enemy_air_threat = 0.0f32; // 敵航空ユニットの総攻撃力（地上への攻撃能力）
    let mut enemy_ground_threat = 0.0f32;
    let mut enemy_sea_threat = 0.0f32;

    for (_, enemy_stats) in enemy_units {
        // 非戦闘ユニットはスキップ
        if enemy_stats.max_ammo1 == 0 && enemy_stats.max_ammo2 == 0 {
            continue;
        }
        // ユニット自体の適性（このユニットが何に強いか）を使って脅威を分類
        let affinity = compute_unit_affinity(
            enemy_stats.unit_type,
            damage_chart,
            unit_registry,
            normalization_scale,
        );
        let category = UnitCategory::from_movement_type(enemy_stats.movement_type);
        // 敵ユニットのカテゴリに応じて脅威を記録する
        // 「敵が Air カテゴリ」= 自軍は anti_air 能力が必要
        match category {
            UnitCategory::Air => {
                // 航空ユニットの「地上への攻撃適性」を航空脅威として積算
                enemy_air_threat += affinity.anti_ground.max(affinity.anti_sea);
            }
            UnitCategory::Ground => {
                enemy_ground_threat += affinity.anti_ground;
            }
            UnitCategory::Sea => {
                enemy_sea_threat += affinity.anti_sea.max(affinity.anti_ground);
            }
        }
    }

    // 自軍の各カテゴリへの反撃能力を集計
    let mut my_power_vs_air = 0.0f32;
    let mut my_power_vs_ground = 0.0f32;
    let mut my_power_vs_sea = 0.0f32;

    for (_, my_stats) in my_units {
        if my_stats.max_ammo1 == 0 && my_stats.max_ammo2 == 0 {
            continue;
        }
        let affinity = compute_unit_affinity(
            my_stats.unit_type,
            damage_chart,
            unit_registry,
            normalization_scale,
        );
        my_power_vs_air += affinity.anti_air;
        my_power_vs_ground += affinity.anti_ground;
        my_power_vs_sea += affinity.anti_sea;
    }

    // ギャップ = 敵の脅威 - 自軍の反撃力（負にはならない）
    let gap_air = (enemy_air_threat - my_power_vs_air).max(0.0);
    let gap_ground = (enemy_ground_threat - my_power_vs_ground).max(0.0);
    let gap_sea = (enemy_sea_threat - my_power_vs_sea).max(0.0);

    // 正規化スケール：「1体分の適性値（≒1.0）」を基準とする
    let unit_scale = 1.0f32.max(normalization_scale / 100.0);

    let anti_air = (gap_air / unit_scale).clamp(0.0, 1.0);
    let anti_ground = (gap_ground / unit_scale).clamp(0.0, 1.0);
    let anti_sea = (gap_sea / unit_scale).clamp(0.0, 1.0);

    // --- 占領脅威の計算 ---
    // 敵の占領可能ユニットが重要拠点に与えるリスクを算出
    let mut capture_threat = 0.0f32;

    // 拠点の重要度テーブル
    let importance = |terrain: Terrain| -> f32 {
        match terrain {
            Terrain::Capital => 3.0,
            Terrain::Factory | Terrain::Airport | Terrain::Port => 2.0,
            Terrain::City => 1.0,
            _ => 0.0,
        }
    };

    for (enemy_pos, enemy_stats) in enemy_units {
        if !enemy_stats.can_capture {
            continue;
        }
        // 最も近い重要拠点への脅威を評価
        for (prop_pos, terrain) in my_properties {
            let dist = (enemy_pos.x as i32 - prop_pos.x as i32).unsigned_abs()
                + (enemy_pos.y as i32 - prop_pos.y as i32).unsigned_abs();
            // 移動力を考慮した ETA の簡易見積もり（最低1ターン）
            let eta = (dist / enemy_stats.max_movement.max(1)).max(1);
            // 重要度 × 到達しやすさ（ETAが短いほど高い）
            capture_threat += importance(*terrain) / eta as f32;
        }
    }

    // 占領脅威を正規化（「重要拠点に1ターンで到達できる歩兵1体」が1.0相当）
    let capture_scale = 3.0f32; // Capital の importance が 3.0
    let capture = (capture_threat / capture_scale).clamp(0.0, 1.0);

    // --- 輸送需要（既存ロジックと同等、ここでは簡易計算） ---
    let total_targets = my_properties.len() as u32;
    let current_capacity: u32 = my_units.iter().map(|(_, s)| s.max_cargo).sum();
    let logistics = if total_targets > current_capacity {
        ((total_targets - current_capacity) as f32 / total_targets as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };

    DemandMatrix {
        anti_ground,
        anti_air,
        anti_sea,
        capture,
        logistics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::UnitStats;
    use crate::resources::{DamageChart, UnitRegistry};
    use std::collections::HashMap;

    fn make_registry_with(types: Vec<(UnitType, MovementType, u32, u32)>) -> UnitRegistry {
        let mut map = HashMap::new();
        for (ut, mt, ammo1, ammo2) in types {
            map.insert(
                ut,
                UnitStats {
                    unit_type: ut,
                    movement_type: mt,
                    max_ammo1: ammo1,
                    max_ammo2: ammo2,
                    max_movement: 3,
                    can_capture: matches!(ut, UnitType::Infantry | UnitType::Mech),
                    ..UnitStats::mock()
                },
            );
        }
        UnitRegistry(map)
    }

    /// 敵に航空機のみが存在し、自軍に対空ユニットがない場合、anti_air が高くなること
    #[test]
    fn test_anti_air_demand_rises_with_air_enemy() {
        let mut chart = DamageChart::new();
        // 対空戦車 → 戦闘ヘリへの高ダメージ
        chart.insert_damage(UnitType::AntiAir, UnitType::Bcopters, 120);
        // 装甲車 → 戦闘ヘリへの低ダメージ
        chart.insert_damage(UnitType::Recon, UnitType::Bcopters, 10);
        // 戦闘ヘリ → 地上ユニットへの攻撃
        chart.insert_damage(UnitType::Bcopters, UnitType::Recon, 70);

        let registry = make_registry_with(vec![
            (UnitType::AntiAir, MovementType::Tank, 9, 0),
            (UnitType::Recon, MovementType::ArmoredCar, 6, 0),
            (UnitType::Bcopters, MovementType::Air, 6, 0),
        ]);

        // 自軍：装甲車のみ（対空なし）
        let my_units = vec![(
            GridPosition { x: 3, y: 3 },
            UnitStats {
                unit_type: UnitType::Recon,
                movement_type: MovementType::ArmoredCar,
                max_ammo1: 6,
                max_ammo2: 0,
                ..UnitStats::mock()
            },
        )];
        // 敵：戦闘ヘリ
        let enemy_units = vec![(
            GridPosition { x: 5, y: 5 },
            UnitStats {
                unit_type: UnitType::Bcopters,
                movement_type: MovementType::Air,
                max_ammo1: 6,
                max_ammo2: 0,
                ..UnitStats::mock()
            },
        )];

        let demand = compute_demand(&my_units, &enemy_units, &[], &chart, &registry);

        assert!(
            demand.anti_air > 0.0,
            "敵ヘリがいて対空なし → anti_air > 0 のはずだが {}",
            demand.anti_air
        );
        assert!(
            demand.anti_air > demand.anti_ground,
            "航空脅威 > 地上脅威 のはずだが anti_air={} anti_ground={}",
            demand.anti_air,
            demand.anti_ground
        );
    }

    /// 占領可能ユニットが重要拠点（首都）の近くにいる場合、capture 需要が高くなること
    #[test]
    fn test_capture_threat_near_capital() {
        let chart = DamageChart::new();
        let registry = make_registry_with(vec![
            (UnitType::Infantry, MovementType::Infantry, 0, 0),
            (UnitType::Recon, MovementType::ArmoredCar, 6, 0),
        ]);

        // 敵歩兵が首都の1マス隣（ETA=1）
        let enemy_units = vec![(
            GridPosition { x: 4, y: 3 },
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                can_capture: true,
                max_ammo1: 0,
                max_ammo2: 0,
                ..UnitStats::mock()
            },
        )];

        // 自軍：首都を所有
        let my_properties = vec![(GridPosition { x: 3, y: 3 }, Terrain::Capital)];

        let demand_near = compute_demand(&[], &enemy_units, &my_properties, &chart, &registry);

        // 敵歩兵が遠い場合（ETA=4）
        let enemy_far = vec![(
            GridPosition { x: 10, y: 10 },
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                can_capture: true,
                max_ammo1: 0,
                max_ammo2: 0,
                ..UnitStats::mock()
            },
        )];
        let demand_far = compute_demand(&[], &enemy_far, &my_properties, &chart, &registry);

        assert!(
            demand_near.capture > demand_far.capture,
            "首都近くの敵歩兵の方が遠い敵より capture 需要が高いはずだが near={} far={}",
            demand_near.capture,
            demand_far.capture
        );
        assert!(
            demand_near.capture > 0.0,
            "capture 需要 > 0 のはずだが {}",
            demand_near.capture
        );
    }

    /// 対空戦車の anti_air 適性が装甲車より高いこと
    #[test]
    fn test_unit_affinity_antiair_vs_armor() {
        let master_data = crate::resources::MasterDataRegistry::load().unwrap();
        let (world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_2").unwrap();

        let damage_chart = world.get_resource::<DamageChart>().unwrap();
        let unit_registry = world.get_resource::<UnitRegistry>().unwrap();
        let scale = average_attack_expectation(damage_chart, unit_registry);

        let antiair_affinity =
            compute_unit_affinity(UnitType::AntiAir, damage_chart, unit_registry, scale);
        let recon_affinity =
            compute_unit_affinity(UnitType::Recon, damage_chart, unit_registry, scale);

        assert!(
            antiair_affinity.anti_air > recon_affinity.anti_air,
            "対空戦車の anti_air 適性({}) > 装甲車({}) のはずだが",
            antiair_affinity.anti_air,
            recon_affinity.anti_air
        );
    }
}
