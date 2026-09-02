//! Rolling Plan が使用する距離別の反撃判定。
//!
//! 与damageと初撃ETAは編成比較で実績のある既存モデルを維持し、レビューで
//! 指摘された反撃可否だけを本番と同じ主・副兵装の射程と弾薬から判定する。

use crate::components::UnitStats;
use crate::resources::DamageChart;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::master_data::UnitName;
use crate::systems::combat::select_weapon;

/// 現在距離から最初に合法な射撃を行う交戦距離で、敵が返せるdamageを得る。
pub(super) fn planned_counter_damage(
    master_data: &MasterDataRegistry,
    damage_chart: &DamageChart,
    attacker: &UnitStats,
    defender: &UnitStats,
    current_distance: u32,
) -> u32 {
    let attack_distance = preferred_attack_distance(
        master_data,
        damage_chart,
        attacker,
        defender,
        current_distance,
    )
    .unwrap_or(current_distance.max(1));
    let Some((_, is_indirect)) = damage_at_distance(
        master_data,
        damage_chart,
        attacker,
        defender,
        attack_distance,
    ) else {
        return 0;
    };
    // 本番戦闘では間接攻撃だけが反撃対象外で、直接兵装なら距離1超でも
    // 防御側が同じ距離で武器を選べる場合に反撃できる。
    if is_indirect {
        return 0;
    }
    damage_at_distance(
        master_data,
        damage_chart,
        defender,
        attacker,
        attack_distance,
    )
    .map_or(0, |(damage, _)| damage)
}

/// 現在射程内ならその距離、射程外なら接近時に最初に使える最長射程を選ぶ。
/// 最短射程の内側にいる間接砲だけは、離脱後に使える最短距離を選ぶ。
fn preferred_attack_distance(
    master_data: &MasterDataRegistry,
    damage_chart: &DamageChart,
    attacker: &UnitStats,
    defender: &UnitStats,
    current_distance: u32,
) -> Option<u32> {
    if damage_at_distance(
        master_data,
        damage_chart,
        attacker,
        defender,
        current_distance,
    )
    .is_some()
    {
        return Some(current_distance);
    }

    let search_limit = current_distance
        .max(maximum_weapon_range(master_data, attacker))
        .max(1);
    (1..=current_distance)
        .rev()
        .find(|distance| {
            damage_at_distance(master_data, damage_chart, attacker, defender, *distance).is_some()
        })
        .or_else(|| {
            ((current_distance.saturating_add(1))..=search_limit).find(|distance| {
                damage_at_distance(master_data, damage_chart, attacker, defender, *distance)
                    .is_some()
            })
        })
}

/// UnitStatsが保持する主兵装射程だけでなく、副兵装を含む最大射程を得る。
fn maximum_weapon_range(master_data: &MasterDataRegistry, stats: &UnitStats) -> u32 {
    let Some(unit) = master_data.get_unit(&UnitName(stats.unit_type.as_str().to_owned())) else {
        return stats.max_range;
    };
    unit.weapon1
        .iter()
        .chain(unit.weapon2.iter())
        .filter_map(|name| master_data.weapons.get(&UnitName(name.clone())))
        .map(|weapon| weapon.range_max)
        .max()
        .unwrap_or(stats.max_range)
}

/// 実戦闘の武器選択とDamageChart上書きを距離指定で再利用する。
fn damage_at_distance(
    master_data: &MasterDataRegistry,
    damage_chart: &DamageChart,
    attacker: &UnitStats,
    defender: &UnitStats,
    distance: u32,
) -> Option<(u32, bool)> {
    let (slot, registry_damage, is_indirect) = select_weapon(
        attacker.max_ammo1,
        attacker.max_ammo2,
        attacker.unit_type.as_str(),
        defender.unit_type.as_str(),
        distance,
        master_data,
    )?;
    let damage = if slot == 1 {
        damage_chart.get_base_damage(attacker.unit_type, defender.unit_type)
    } else {
        damage_chart.get_base_damage_secondary(attacker.unit_type, defender.unit_type)
    }
    .unwrap_or(registry_damage);
    (damage > 0).then_some((damage, is_indirect))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::master_data::UnitName;

    fn damage_chart(master: &MasterDataRegistry) -> DamageChart {
        let mut chart = DamageChart::new();
        for (unit_name, unit_record) in &master.units {
            let attacker = master.unit_type_for_name(&unit_name.0).unwrap();
            if let Some(weapon_name) = &unit_record.weapon1 {
                let weapon = &master.weapons[&UnitName(weapon_name.clone())];
                for (defender_name, damage) in &weapon.damages {
                    let defender = master.unit_type_for_name(defender_name).unwrap();
                    chart.insert_damage(attacker, defender, *damage);
                }
            }
            if let Some(weapon_name) = &unit_record.weapon2 {
                let weapon = &master.weapons[&UnitName(weapon_name.clone())];
                for (defender_name, damage) in &weapon.damages {
                    let defender = master.unit_type_for_name(defender_name).unwrap();
                    chart.insert_secondary_damage(attacker, defender, *damage);
                }
            }
        }
        chart
    }

    fn stats(master: &MasterDataRegistry, name: &str) -> UnitStats {
        master
            .create_unit_stats(&UnitName(name.to_owned()))
            .expect("テスト対象unit")
    }

    #[test]
    fn direct_attack_on_indirect_unit_has_no_counter() {
        let master = MasterDataRegistry::load().unwrap();
        let chart = damage_chart(&master);
        let attacker = stats(&master, "軽戦車");
        let defender = stats(&master, "ロケットランチャー");

        assert_eq!(
            planned_counter_damage(&master, &chart, &attacker, &defender, 1),
            0
        );
    }

    #[test]
    fn adjacent_long_range_capable_tank_still_receives_counter() {
        let master = MasterDataRegistry::load().unwrap();
        let chart = damage_chart(&master);
        let attacker = stats(&master, "重戦車");
        let defender = stats(&master, "軽戦車");

        assert!(planned_counter_damage(&master, &chart, &attacker, &defender, 1) > 0);
    }

    #[test]
    fn distant_indirect_attack_has_no_counter() {
        let master = MasterDataRegistry::load().unwrap();
        let chart = damage_chart(&master);
        let attacker = stats(&master, "ロケットランチャー");
        let defender = stats(&master, "軽歩兵");

        assert_eq!(
            planned_counter_damage(&master, &chart, &attacker, &defender, 6),
            0
        );
    }
}
