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
        Some((atk_hp, atk_max, atk_cost, atk_type)),
        Some((def_hp, def_max, def_cost, def_type)),
    ) = {
        let mut query = world.query::<(&Health, &UnitStats)>();
        let atk = query
            .get(world, attacker_entity)
            .ok()
            .map(|(h, s)| (h.current, h.max, s.cost, s.unit_type));
        let def = query
            .get(world, defender_entity)
            .ok()
            .map(|(h, s)| (h.current, h.max, s.cost, s.unit_type));
        (atk, def)
    } {
        // 与えるダメージの予測
        let base_damage = damage_chart
            .get_base_damage(atk_type, def_type)
            .unwrap_or(0);
        let atk_display = atk_hp.div_ceil(10);
        let expected_damage_to_enemy = (base_damage * atk_display) / 10;
        let actual_damage_to_enemy = std::cmp::min(expected_damage_to_enemy, def_hp);

        // 与える被害価値
        expected_damage_value = (actual_damage_to_enemy as i32 * def_cost as i32) / def_max as i32;

        // 反撃ダメージの予測（敵が生き残る場合のみ）
        let remaining_enemy_hp = def_hp.saturating_sub(actual_damage_to_enemy);
        if remaining_enemy_hp > 0 {
            let counter_base_damage = damage_chart
                .get_base_damage(def_type, atk_type)
                .unwrap_or(0);
            let remaining_display_hp = remaining_enemy_hp.div_ceil(10);
            let expected_counter_damage = (counter_base_damage * remaining_display_hp) / 10;
            let actual_counter_damage = std::cmp::min(expected_counter_damage, atk_hp);

            // 受ける被害価値
            expected_self_damage_value =
                (actual_counter_damage as i32 * atk_cost as i32) / atk_max as i32;
        }
    }

    expected_self_damage_value > expected_damage_value
}
