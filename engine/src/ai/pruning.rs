use crate::components::{Ammo, GridPosition, Health, UnitStats};
use crate::resources::{DamageChart, Map, master_data::MasterDataRegistry};
use crate::systems::combat::get_expected_damage;
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

    let map = world.resource::<Map>().clone();
    let registry = world.resource::<MasterDataRegistry>().clone();

    if let (
        Some((atk_hp, atk_max, atk_stats, atk_pos, atk_ammo)),
        Some((def_hp, def_max, def_stats, def_pos, def_ammo)),
    ) = {
        let mut query = world.query::<(&Health, &UnitStats, &GridPosition, Option<&Ammo>)>();
        let atk = query.get(world, attacker_entity).ok().map(|(h, s, p, a)| {
            (
                h.current,
                h.max,
                s.clone(),
                *p,
                a.map(|am| (am.ammo1, am.ammo2)).unwrap_or((99, 99)),
            )
        });
        let def = query.get(world, defender_entity).ok().map(|(h, s, p, a)| {
            (
                h.current,
                h.max,
                s.clone(),
                *p,
                a.map(|am| (am.ammo1, am.ammo2)).unwrap_or((99, 99)),
            )
        });
        (atk, def)
    } {
        if def_max == 0 || atk_max == 0 {
            return false;
        }

        // 地形防御ボーナスの取得
        let def_terrain = map
            .get_terrain(def_pos.x, def_pos.y)
            .unwrap_or(crate::resources::Terrain::Plains);
        let def_bonus = registry.get_terrain_defense_bonus(def_terrain);
        let atk_terrain = map
            .get_terrain(atk_pos.x, atk_pos.y)
            .unwrap_or(crate::resources::Terrain::Plains);
        let atk_bonus = registry.get_terrain_defense_bonus(atk_terrain);

        let dist = (atk_pos.x as i64 - def_pos.x as i64).unsigned_abs() as u32
            + (atk_pos.y as i64 - def_pos.y as i64).unsigned_abs() as u32;

        // 与えるダメージの予測 (+5 は乱数期待値)
        let expected_damage_to_enemy = get_expected_damage(
            &atk_stats,
            atk_hp,
            atk_ammo,
            &def_stats,
            def_bonus,
            dist,
            &registry,
            damage_chart,
            false,
        ) + 5;
        let actual_damage_to_enemy = std::cmp::min(expected_damage_to_enemy, def_hp);

        // 与える被害価値
        expected_damage_value =
            (actual_damage_to_enemy as i32 * def_stats.cost as i32) / def_max as i32;

        // 反撃ダメージの予測（戦闘は同時解決のため撃破予定でも反撃する）
        // 反撃判定: 射程1の近接攻撃のみ
        if atk_stats.min_range <= 1 {
            let expected_counter_damage = get_expected_damage(
                &def_stats,
                def_hp,
                def_ammo,
                &atk_stats,
                atk_bonus,
                dist,
                &registry,
                damage_chart,
                true,
            ) + 5;
            let actual_counter_damage = std::cmp::min(expected_counter_damage, atk_hp);

            // 受ける被害価値
            expected_self_damage_value =
                (actual_counter_damage as i32 * atk_stats.cost as i32) / atk_max as i32;
        }
    }

    // 被害価値の比較。
    // 仕様書に基づき、敵へ与える被害価値よりも、反撃で受ける被害価値のほうが大きい場合に無謀と判定します。
    expected_self_damage_value > expected_damage_value
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
        world.insert_resource(damage_chart);

        world.insert_resource(Map {
            width: 5,
            height: 5,
            tiles: vec![crate::resources::Terrain::Plains; 25],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(MasterDataRegistry::load().unwrap());

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
                GridPosition { x: 0, y: 0 },
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
                GridPosition { x: 1, y: 0 },
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
                GridPosition { x: 2, y: 0 },
            ))
            .id();

        let dc = world.resource::<DamageChart>().clone();

        // 1. Infantry attacking Tank is suicidal
        assert!(is_suicidal_attack(&mut world, infantry, tank, &dc));

        // 2. Artillery attacking Tank is NOT suicidal
        assert!(!is_suicidal_attack(&mut world, artillery, tank, &dc));

        // 3. Tank attacking Infantry is NOT suicidal
        assert!(!is_suicidal_attack(&mut world, tank, infantry, &dc));

        // 4. Missing components -> not suicidal (returns false gracefully)
        let empty_entity = world.spawn_empty().id();
        assert!(!is_suicidal_attack(&mut world, empty_entity, tank, &dc));

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
                GridPosition { x: 0, y: 1 },
            ))
            .id();
        assert!(!is_suicidal_attack(&mut world, bugged_unit, tank, &dc));
    }
}
