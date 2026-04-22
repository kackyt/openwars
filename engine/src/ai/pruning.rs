use crate::components::{Health, UnitStats};
use crate::resources::DamageChart;
use bevy_ecs::prelude::*;

/// 攻撃行動が無謀（カミカゼアタック）かどうかを判定します。
/// 敵に与える被害価値よりも、反撃で受ける被害価値のほうが大きければ無謀とみなします。
pub fn is_suicidal_attack(
    world: &mut World,
    attacker_entity: Entity,
    defender_entity: Entity,
    damage_chart: &DamageChart,
) -> bool {
    let mut expected_damage_value = 0;
    let mut expected_self_damage_value = 0;

    if let (
        Some((atk_hp, atk_max, atk_cost, atk_type, atk_min_range)),
        Some((def_hp, def_max, def_cost, def_type, _def_min_range)),
    ) = {
        let mut query = world.query::<(&Health, &UnitStats)>();
        let atk = query
            .get(world, attacker_entity)
            .ok()
            .map(|(h, s)| (h.current, h.max, s.cost, s.unit_type, s.min_range));
        let def = query
            .get(world, defender_entity)
            .ok()
            .map(|(h, s)| (h.current, h.max, s.cost, s.unit_type, s.min_range));
        (atk, def)
    } {
        if def_max == 0 || atk_max == 0 {
            return false;
        }

        // 与えるダメージの予測
        let base_damage = damage_chart
            .get_base_damage(atk_type, def_type)
            .unwrap_or(0);
        let effective_base_damage = base_damage * 105 / 100;
        let atk_display = atk_hp.div_ceil(10);
        let expected_damage_to_enemy = (effective_base_damage * atk_display) / 10;
        let actual_damage_to_enemy = std::cmp::min(expected_damage_to_enemy, def_hp);

        // 与える被害価値
        expected_damage_value = (actual_damage_to_enemy as i32 * def_cost as i32) / def_max as i32;

        // 反撃ダメージの予測（戦闘は同時解決のため撃破予定でも反撃する）
        // 間接攻撃 (min_range > 1) の場合は反撃を受けない
        let is_indirect = atk_min_range > 1;
        if !is_indirect {
            let counter_base_damage = damage_chart
                .get_base_damage(def_type, atk_type)
                .unwrap_or(0);
            // 防御側は攻撃を受けた時点のHPで反撃（同時解決）
            let defender_display_hp = def_hp.div_ceil(10);
            let expected_counter_damage = (counter_base_damage * defender_display_hp) / 10;
            let actual_counter_damage = std::cmp::min(expected_counter_damage, atk_hp);

            // 受ける被害価値
            expected_self_damage_value =
                (actual_counter_damage as i32 * atk_cost as i32) / atk_max as i32;
        }
    }

    // 被害価値の比較。攻撃側有利（同時解決だが先制ダメージが反映される）を考慮し、
    // 完全に無謀（受ける被害が与える被害の1.5倍を超える）な場合のみ suicidal と判定。
    expected_self_damage_value > (expected_damage_value * 15 / 10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::UnitType;

    #[test]
    fn test_is_suicidal_attack() {
        let mut world = World::new();
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Tank, 1);
        damage_chart.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        damage_chart.insert_damage(UnitType::Artillery, UnitType::Tank, 50);
        damage_chart.insert_damage(UnitType::Tank, UnitType::Artillery, 50);

        let infantry = world
            .spawn((
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    min_range: 1,
                    ..UnitStats::mock()
                },
            ))
            .id();

        let tank = world
            .spawn((
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    unit_type: UnitType::Tank,
                    cost: 7000,
                    min_range: 1,
                    ..UnitStats::mock()
                },
            ))
            .id();

        let artillery = world
            .spawn((
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    unit_type: UnitType::Artillery,
                    cost: 6000,
                    min_range: 2,
                    ..UnitStats::mock()
                },
            ))
            .id();

        // 1. Infantry attacking Tank is suicidal (does 1% damage, receives 90% counter on its own 1000 cost vs tank 7000 cost)
        // expected damage: 1 dmg * 7000 / 100 = 70 value
        // expected counter: 90 dmg * 1000 / 100 = 900 value
        // 900 > 70 => true
        assert!(is_suicidal_attack(
            &mut world,
            infantry,
            tank,
            &damage_chart
        ));

        // 2. Artillery attacking Tank is NOT suicidal, because indirect attacks receive no counter-attack damage
        assert!(!is_suicidal_attack(
            &mut world,
            artillery,
            tank,
            &damage_chart
        ));

        // 3. Tank attacking Infantry is NOT suicidal (receives minimal counter)
        assert!(!is_suicidal_attack(
            &mut world,
            tank,
            infantry,
            &damage_chart
        ));

        // 4. Missing components -> not suicidal (returns false gracefully)
        let empty_entity = world.spawn_empty().id();
        assert!(!is_suicidal_attack(
            &mut world,
            empty_entity,
            tank,
            &damage_chart
        ));

        // 5. Zero max hp -> safely ignored (returns false)
        let bugged_unit = world
            .spawn((
                Health {
                    current: 100,
                    max: 0,
                },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    min_range: 1,
                    ..UnitStats::mock()
                },
            ))
            .id();
        assert!(!is_suicidal_attack(
            &mut world,
            bugged_unit,
            tank,
            &damage_chart
        ));
    }
}
