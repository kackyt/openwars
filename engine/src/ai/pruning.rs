use crate::components::{Ammo, GridPosition, Health, UnitStats};
use crate::resources::{DamageChart, Map, master_data::MasterDataRegistry};
use crate::systems::combat::get_detailed_expected_damage;
use bevy_ecs::prelude::*;

/// 合流後にHP上限を超え、返金されないHPが発生するかを判定します。
/// 現行の合流処理は余剰HPを資金へ変換しないため、該当候補はAIの評価前に除外します。
pub fn is_overflow_merge_without_refund(source_current: u32, target: Health) -> bool {
    source_current.saturating_add(target.current) > target.max
}

/// 移動後の射撃位置から求めた、1回の攻撃交換予測。
///
/// 金額価値は予実監査用であり、攻撃を一律禁止する条件には使わない。戦術的な相性は
/// 双方の最大HPに対する損耗率で比較し、高価な戦略兵器を必要な攻撃から排除しない。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttackExchange {
    pub expected_damage: u32,
    pub expected_counter_damage: u32,
    pub damage_value_dealt: u32,
    pub counter_value_received: u32,
    pub enemy_loss_permyriad: u32,
    pub friendly_loss_permyriad: u32,
}

impl AttackExchange {
    /// 対象の損耗率が自軍の反撃損耗率以上なら、当該兵種の相性は不利ではない。
    pub fn is_favorable_matchup(self) -> bool {
        self.enemy_loss_permyriad >= self.friendly_loss_permyriad
    }

    /// 行動スコアへ加える相性差。±10,000へ収まるため、戦略価値とは別軸で扱える。
    pub fn matchup_margin(self) -> i32 {
        self.enemy_loss_permyriad as i32 - self.friendly_loss_permyriad as i32
    }

    /// 旧AIが用いる保守的な交換価値判定。V4の作戦Entityには一律禁止として使わない。
    pub fn loses_more_value_than_it_deals(self) -> bool {
        self.counter_value_received > self.damage_value_dealt
    }
}

/// 敵Entityに対する攻撃を、候補となる移動後位置から評価する。
///
/// 攻撃対象は常にEntityであり、`attacker_position` は射程・反撃・攻撃側地形を
/// 正しく求めるためだけに渡す。これによりmove-and-attackを移動前座標で誤判定しない。
pub fn evaluate_attack_exchange(
    world: &World,
    attacker_entity: Entity,
    defender_entity: Entity,
    attacker_position: GridPosition,
    damage_chart: &DamageChart,
) -> Option<AttackExchange> {
    let map = world.get_resource::<Map>()?;
    let registry = world.get_resource::<MasterDataRegistry>()?;

    if let (
        Some((atk_hp, atk_max, atk_stats, atk_ammo)),
        Some((def_hp, def_max, def_stats, def_pos, def_ammo)),
    ) = (
        world
            .get::<Health>(attacker_entity)
            .zip(world.get::<UnitStats>(attacker_entity))
            .map(|(h, s)| {
                (
                    h.current,
                    h.max,
                    s.clone(),
                    world
                        .get::<Ammo>(attacker_entity)
                        .map(|ammo| (ammo.ammo1, ammo.ammo2))
                        .unwrap_or((99, 99)),
                )
            }),
        world
            .get::<Health>(defender_entity)
            .zip(world.get::<UnitStats>(defender_entity))
            .zip(world.get::<GridPosition>(defender_entity))
            .map(|((h, s), p)| {
                (
                    h.current,
                    h.max,
                    s.clone(),
                    *p,
                    world
                        .get::<Ammo>(defender_entity)
                        .map(|ammo| (ammo.ammo1, ammo.ammo2))
                        .unwrap_or((99, 99)),
                )
            }),
    ) {
        if def_max == 0 || atk_max == 0 {
            return None;
        }

        // 地形防御ボーナスの取得
        let def_terrain = map
            .get_terrain(def_pos.x, def_pos.y)
            .unwrap_or(crate::resources::Terrain::Plains);
        let def_bonus = registry.get_terrain_defense_bonus(def_terrain);
        let atk_terrain = map
            .get_terrain(attacker_position.x, attacker_position.y)
            .unwrap_or(crate::resources::Terrain::Plains);
        let atk_bonus = registry.get_terrain_defense_bonus(atk_terrain);

        let dist = map.distance(
            attacker_position.x,
            attacker_position.y,
            def_pos.x,
            def_pos.y,
        );

        // 与えるダメージの予測 (+5 は乱数期待値)
        let expected_damage_to_enemy = get_detailed_expected_damage(
            &atk_stats,
            atk_hp,
            atk_ammo,
            &def_stats,
            def_bonus,
            dist,
            registry,
            damage_chart,
            false,
        )
        .map(|(d, _)| d + 5)
        .unwrap_or(0);
        let actual_damage_to_enemy = std::cmp::min(expected_damage_to_enemy, def_hp);

        // 与える被害価値
        let expected_damage_value = actual_damage_to_enemy.saturating_mul(def_stats.cost) / def_max;

        // 反撃ダメージの予測（戦闘は同時解決のため撃破予定でも反撃する）
        // 反撃判定: 攻撃側が間接攻撃武器を選択していない場合のみ反撃が発生する
        let atk_res = crate::systems::combat::get_detailed_expected_damage(
            &atk_stats,
            atk_hp,
            atk_ammo,
            &def_stats,
            def_bonus,
            dist,
            registry,
            damage_chart,
            false,
        );

        if let Some((_, (_, _, is_indirect))) = atk_res
            && !is_indirect
            && let Some((base_counter_damage, _)) = get_detailed_expected_damage(
                &def_stats,
                def_hp,
                def_ammo,
                &atk_stats,
                atk_bonus,
                dist,
                registry,
                damage_chart,
                true,
            )
        {
            let expected_counter_damage = base_counter_damage + 5;
            let actual_counter_damage = std::cmp::min(expected_counter_damage, atk_hp);

            // 受ける被害価値
            let expected_self_damage_value =
                actual_counter_damage.saturating_mul(atk_stats.cost) / atk_max;
            return Some(AttackExchange {
                expected_damage: actual_damage_to_enemy,
                expected_counter_damage: actual_counter_damage,
                damage_value_dealt: expected_damage_value,
                counter_value_received: expected_self_damage_value,
                enemy_loss_permyriad: actual_damage_to_enemy.saturating_mul(10_000) / def_max,
                friendly_loss_permyriad: actual_counter_damage.saturating_mul(10_000) / atk_max,
            });
        }

        return Some(AttackExchange {
            expected_damage: actual_damage_to_enemy,
            expected_counter_damage: 0,
            damage_value_dealt: expected_damage_value,
            counter_value_received: 0,
            enemy_loss_permyriad: actual_damage_to_enemy.saturating_mul(10_000) / def_max,
            friendly_loss_permyriad: 0,
        });
    }

    None
}

/// 旧AI向けの保守的な枝刈り。移動後位置を必ず明示する。
pub fn is_suicidal_attack_at(
    world: &World,
    attacker_entity: Entity,
    defender_entity: Entity,
    attacker_position: GridPosition,
    damage_chart: &DamageChart,
) -> bool {
    evaluate_attack_exchange(
        world,
        attacker_entity,
        defender_entity,
        attacker_position,
        damage_chart,
    )
    .is_some_and(AttackExchange::loses_more_value_than_it_deals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::UnitType;

    #[test]
    fn issue73_overflow_merge_is_pruned() {
        let full = Health {
            current: 100,
            max: 100,
        };
        let damaged = Health {
            current: 34,
            max: 100,
        };

        assert!(is_overflow_merge_without_refund(full.current, full));
        assert!(is_overflow_merge_without_refund(full.current, damaged));
    }

    #[test]
    fn issue73_lossless_merge_is_not_pruned() {
        let source = Health {
            current: 40,
            max: 100,
        };
        let target = Health {
            current: 50,
            max: 100,
        };

        assert!(!is_overflow_merge_without_refund(source.current, target));
    }

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
        assert!(is_suicidal_attack_at(
            &world,
            infantry,
            tank,
            GridPosition { x: 0, y: 0 },
            &dc
        ));

        // 2. Artillery attacking Tank is NOT suicidal
        assert!(!is_suicidal_attack_at(
            &world,
            artillery,
            tank,
            GridPosition { x: 2, y: 0 },
            &dc
        ));

        // 3. Tank attacking Infantry is NOT suicidal
        assert!(!is_suicidal_attack_at(
            &world,
            tank,
            infantry,
            GridPosition { x: 1, y: 0 },
            &dc
        ));

        // 4. Missing components -> not suicidal (returns false gracefully)
        let empty_entity = world.spawn_empty().id();
        assert!(!is_suicidal_attack_at(
            &world,
            empty_entity,
            tank,
            GridPosition { x: 0, y: 0 },
            &dc
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
                GridPosition { x: 0, y: 1 },
            ))
            .id();
        assert!(!is_suicidal_attack_at(
            &world,
            bugged_unit,
            tank,
            GridPosition { x: 0, y: 1 },
            &dc
        ));
    }

    #[test]
    fn exchange_uses_hex_distance_from_post_move_position() {
        let mut world = World::new();
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Tank, UnitType::Infantry, 50);
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Tank, 0);
        world.insert_resource(Map {
            width: 4,
            height: 4,
            tiles: vec![crate::resources::Terrain::Plains; 16],
            topology: crate::resources::GridTopology::Hex,
        });
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let attacker = world
            .spawn((
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    unit_type: UnitType::Tank,
                    cost: 7_000,
                    min_range: 1,
                    max_range: 1,
                    ..UnitStats::mock()
                },
                // 実Entityはまだ移動前。交換予測はこの座標を参照してはならない。
                GridPosition { x: 0, y: 0 },
            ))
            .id();
        let defender = world
            .spawn((
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1_000,
                    min_range: 1,
                    max_range: 1,
                    ..UnitStats::mock()
                },
                GridPosition { x: 1, y: 2 },
            ))
            .id();

        let exchange = evaluate_attack_exchange(
            &world,
            attacker,
            defender,
            GridPosition { x: 0, y: 1 },
            &damage_chart,
        )
        .expect("hexでは(0,1)と(1,2)が隣接し、移動後攻撃を評価できること");

        assert!(exchange.expected_damage > 0);
        // combat期待値は乱数期待の+5を含むため、基礎ダメージ0の反撃は5に留まる。
        assert_eq!(exchange.expected_counter_damage, 5);
    }
}
