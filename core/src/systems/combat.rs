use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::SystemTime;

fn random_bonus() -> u32 {
    let mut h = DefaultHasher::new();
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos()
        .hash(&mut h);
    (h.finish() % 11) as u32 // 0..=10
}

/// 攻撃者と防衛者のユニットタイプに基づき、ダメージ計算表（DamageChart）を参照して
/// 最適な武器（主武器 または 副武器）を選択します。
///
/// 戻り値: (使用する武器のスロット番号(1 or 2), 基礎ダメージ値) または None
fn select_weapon(
    ammo1: u32,
    ammo2: u32,
    attacker_type: UnitType,
    defender_type: UnitType,
    damage_chart: &DamageChart,
) -> Option<(u32, u32)> {
    if ammo1 > 0 {
        if let Some(dmg) = damage_chart.get_base_damage(attacker_type, defender_type) {
            if dmg > 0 {
                return Some((1, dmg));
            }
        }
    }
    if ammo2 > 0 {
        if let Some(dmg) = damage_chart.get_base_damage_secondary(attacker_type, defender_type) {
            if dmg > 0 {
                return Some((2, dmg));
            }
        }
    }
    None
}

/// ユニットの攻撃コマンド(`AttackUnitCommand`)を処理するシステム。
///
/// 【処理の流れ】
/// 1. 攻撃者が自軍か、行動済みでないか、HPが0でないかを確認します。
/// 2. 防衛者が敵軍か、HPが0でないかを確認します。
/// 3. `select_weapon` を使って最適な武器とダメージを決定します。
/// 4. 射程と移動状態（間接攻撃は移動後不可）を確認します。
/// 5. 攻撃ダメージを計算し、防衛者のHP(`Health`)を減算します。
/// 6. 防衛者が直接攻撃の範囲内かつ反撃可能な武器を持っていれば、反撃ダメージを計算し攻撃者のHPを減算します。
/// 7. 攻撃者の `ActionCompleted` を true にし、弾薬(`Ammo`)を消費します。
/// 8. 結果を `UnitAttackedEvent` として発行します。
pub fn attack_unit_system(
    mut attack_events: EventReader<AttackUnitCommand>,
    mut attacked_events: EventWriter<UnitAttackedEvent>,
    mut q_units: Query<(
        Entity,
        &mut Health,
        &mut Ammo,
        &GridPosition,
        &Faction,
        &UnitStats,
        &mut ActionCompleted,
        &HasMoved,
    )>,
    match_state: Res<MatchState>,
    players: Res<Players>,
    damage_chart: Res<DamageChart>,
) {
    if match_state.game_over.is_some() {
        return;
    }
    let active_player = players.0[match_state.active_player_index.0].id;

    for event in attack_events.read() {
        // Read stats first, without holding a mutable borrow for the entire block
        let (
            attacker_pos,
            attacker_faction,
            attacker_stats,
            attacker_action,
            attacker_has_moved,
            attacker_ammo_1,
            attacker_ammo_2,
            attacker_hp,
        ) = match q_units.get(event.attacker_entity) {
            Ok((_, hp, ammo, pos, fac, stats, act, mov)) => (
                pos.clone(),
                fac.0,
                stats.clone(),
                act.0,
                mov.0,
                ammo.ammo1,
                ammo.ammo2,
                hp.clone(),
            ),
            _ => continue,
        };

        if attacker_faction != active_player || attacker_action || attacker_hp.is_destroyed() {
            continue;
        }

        let (defender_pos, defender_faction, defender_stats, defender_hp) =
            match q_units.get(event.defender_entity) {
                Ok((_, hp, _, pos, fac, stats, _, _)) => {
                    (pos.clone(), fac.0, stats.clone(), hp.clone())
                }
                _ => continue,
            };

        if defender_faction == active_player || defender_hp.is_destroyed() {
            continue;
        }

        let dist = (attacker_pos.x as i64 - defender_pos.x as i64).unsigned_abs() as u32
            + (attacker_pos.y as i64 - defender_pos.y as i64).unsigned_abs() as u32;

        let attacker_weapon = select_weapon(
            attacker_ammo_1,
            attacker_ammo_2,
            attacker_stats.unit_type,
            defender_stats.unit_type,
            &damage_chart,
        );
        let (a_weapon_slot, a_base_damage) = match attacker_weapon {
            Some(w) => w,
            None => continue,
        };

        let (min_r, max_r, is_indirect) = if a_weapon_slot == 1 {
            (
                attacker_stats.min_range,
                attacker_stats.max_range,
                attacker_stats.min_range > 1,
            )
        } else {
            (1u32, 1u32, false)
        };

        if dist < min_r || dist > max_r || (is_indirect && attacker_has_moved) {
            continue;
        }

        let a_advantage_damage = (a_base_damage as f64 * 1.05) as u32;
        let a_damage = a_advantage_damage * attacker_hp.get_display_hp() / 10 + random_bonus();

        let do_counter = !is_indirect;
        let mut d_damage_opt = None;
        let mut counter_info = None;

        if do_counter {
            if let Ok((_, _, def_ammo, _, _, def_stats, _, _)) = q_units.get(event.defender_entity)
            {
                counter_info = select_weapon(
                    def_ammo.ammo1,
                    def_ammo.ammo2,
                    def_stats.unit_type,
                    attacker_stats.unit_type,
                    &damage_chart,
                );
                if let Some((_, d_base)) = counter_info {
                    d_damage_opt =
                        Some(d_base * defender_hp.get_display_hp() / 10 + random_bonus());
                }
            }
        }

        // Apply ammo consumption and damage using disjoint mutable borrows via get_many_mut
        if let Ok([mut attacker, mut defender]) =
            q_units.get_many_mut([event.attacker_entity, event.defender_entity])
        {
            if a_weapon_slot == 1 {
                attacker.2.ammo1 = attacker.2.ammo1.saturating_sub(1);
            } else {
                attacker.2.ammo2 = attacker.2.ammo2.saturating_sub(1);
            }

            if let Some(d_dmg) = d_damage_opt {
                attacker.1.damage(d_dmg);
            }
            attacker.6.0 = true; // Set action completed to true

            defender.1.damage(a_damage);
            if let Some((d_slot, _)) = counter_info {
                if d_slot == 1 {
                    defender.2.ammo1 = defender.2.ammo1.saturating_sub(1);
                } else {
                    defender.2.ammo2 = defender.2.ammo2.saturating_sub(1);
                }
            }
        }

        attacked_events.send(UnitAttackedEvent {
            attacker: event.attacker_entity,
            defender: event.defender_entity,
            damage_dealt: a_damage,
            counter_damage_dealt: d_damage_opt,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attack_unit_system() {
        let mut world = World::new();

        world.insert_resource(MatchState::default());
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));

        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Infantry, 55);
        world.insert_resource(damage_chart);

        world.insert_resource(Events::<AttackUnitCommand>::default());
        world.insert_resource(Events::<UnitAttackedEvent>::default());

        let entity_1 = world
            .spawn((
                Health {
                    current: 100,
                    max: 100,
                },
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                GridPosition { x: 0, y: 0 },
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: MovementType::Foot,
                    max_fuel: 10,
                    max_ammo1: 9,
                    max_ammo2: 0,
                    min_range: 1,
                    max_range: 1,
                    daily_fuel_consumption: 0,
                    can_capture: true,
                    can_supply: false,
                    max_cargo: 0,
                    loadable_unit_types: vec![],
                },
                ActionCompleted(false),
                HasMoved(false),
            ))
            .id();

        let entity_2 = world
            .spawn((
                Health {
                    current: 100,
                    max: 100,
                },
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                GridPosition { x: 0, y: 1 },
                Faction(PlayerId(2)),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: MovementType::Foot,
                    max_fuel: 10,
                    max_ammo1: 9,
                    max_ammo2: 0,
                    min_range: 1,
                    max_range: 1,
                    daily_fuel_consumption: 0,
                    can_capture: true,
                    can_supply: false,
                    max_cargo: 0,
                    loadable_unit_types: vec![],
                },
                ActionCompleted(false),
                HasMoved(false),
            ))
            .id();

        world.send_event(AttackUnitCommand {
            attacker_entity: entity_1,
            defender_entity: entity_2,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(attack_unit_system);
        schedule.run(&mut world);

        let hp2 = world.get::<Health>(entity_2).unwrap();
        assert!(hp2.current < 100);

        let hp1 = world.get::<Health>(entity_1).unwrap();
        assert!(hp1.current < 100); // Counter attacked

        let ammo1 = world.get::<Ammo>(entity_1).unwrap();
        assert_eq!(ammo1.ammo1, 8); // Used 1 ammo

        let act1 = world.get::<ActionCompleted>(entity_1).unwrap();
        assert!(act1.0); // Action completed

        let attacked_events = world.resource::<Events<UnitAttackedEvent>>();
        let mut cursor = attacked_events.get_cursor();
        let events: Vec<_> = cursor.read(attacked_events).collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].attacker, entity_1);
        assert_eq!(events[0].defender, entity_2);
    }
}
