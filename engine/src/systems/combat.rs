use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;

#[derive(Debug, PartialEq, Eq)]
pub enum AttackError {
    InvalidEntity,
    FriendlyFire,
    OutOfRange,
    IndirectAfterMove,
}

impl std::fmt::Display for AttackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidEntity => write!(f, "Invalid target entity format."),
            Self::FriendlyFire => write!(f, "Cannot attack own units."),
            Self::OutOfRange => write!(f, "Target is out of range."),
            Self::IndirectAfterMove => write!(f, "Cannot use indirect weapons after moving."),
        }
    }
}
impl std::error::Error for AttackError {}

pub fn can_attack(
    attacker_entity: Entity,
    defender_entity: Entity,
    world: &mut World,
) -> Result<(), AttackError> {
    let mut q_attacker = world.query::<(&GridPosition, &UnitStats, Option<&HasMoved>, &Faction)>();
    let mut q_target = world.query::<(&GridPosition, &Faction)>();

    let (a_pos, a_stats, a_has_moved, a_fac) = q_attacker
        .get(world, attacker_entity)
        .map_err(|_| AttackError::InvalidEntity)?;
    let (d_pos, d_fac) = q_target
        .get(world, defender_entity)
        .map_err(|_| AttackError::InvalidEntity)?;

    if a_fac.0 == d_fac.0 {
        return Err(AttackError::FriendlyFire);
    }

    let dist = (a_pos.x as i64 - d_pos.x as i64).unsigned_abs() as u32
        + (a_pos.y as i64 - d_pos.y as i64).unsigned_abs() as u32;

    if dist < a_stats.min_range || dist > a_stats.max_range {
        return Err(AttackError::OutOfRange);
    }

    let is_indirect = a_stats.min_range > 1;
    if is_indirect && a_has_moved.map(|m| m.0).unwrap_or(false) {
        return Err(AttackError::IndirectAfterMove);
    }

    Ok(())
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
    let dmg1 = damage_chart
        .get_base_damage(attacker_type, defender_type)
        .unwrap_or(0);
    if ammo1 > 0 && dmg1 > 0 {
        return Some((1, dmg1));
    }

    let dmg2 = damage_chart
        .get_base_damage_secondary(attacker_type, defender_type)
        .unwrap_or(0);
    if ammo2 > 0 && dmg2 > 0 {
        return Some((2, dmg2));
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
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub fn attack_unit_system(
    mut attack_events: EventReader<AttackUnitCommand>,
    mut attacked_events: EventWriter<UnitAttackedEvent>,
    mut q_units: Query<(
        Entity,
        &mut Health,
        Option<&mut Ammo>,
        &GridPosition,
        &Faction,
        &UnitStats,
        Option<&mut ActionCompleted>,
        Option<&HasMoved>,
    )>,
    match_state: Res<MatchState>,
    players: Res<Players>,
    damage_chart: Res<DamageChart>,
    master_data: Res<MasterDataRegistry>,
    map: Res<Map>,
    mut rng: ResMut<GameRng>,
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Main {
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
            Ok((_, hp, Some(ammo), pos, fac, stats, Some(act), Some(mov))) => (
                *pos,
                fac.0,
                stats.clone(),
                act.0,
                mov.0,
                ammo.ammo1,
                ammo.ammo2,
                *hp,
            ),
            _ => continue,
        };

        if attacker_faction != active_player || attacker_action || attacker_hp.is_destroyed() {
            continue;
        }

        let (defender_pos, defender_faction, defender_stats, defender_hp, def_ammo_opt) =
            match q_units.get(event.defender_entity) {
                Ok((_, hp, ammo, pos, fac, stats, _, _)) => {
                    let ammo_vals = ammo.map(|a| (a.ammo1, a.ammo2));
                    (*pos, fac.0, stats.clone(), *hp, ammo_vals)
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

        let def_terrain = map
            .get_terrain(defender_pos.x, defender_pos.y)
            .unwrap_or(Terrain::Plains);
        let def_bonus = master_data.get_terrain_defense_bonus(def_terrain);

        let a_advantage_damage = (a_base_damage as f64 * 1.05) as u32;
        let a_damage_base = a_advantage_damage * attacker_hp.get_display_hp() / 10;
        let a_defense_reduction = def_bonus * defender_hp.get_display_hp() / 10; // ★1につき10、HP1につき1/10
        let a_damage_reduced = a_damage_base.saturating_sub(a_defense_reduction);
        let a_damage = a_damage_reduced + rng.next_bonus();

        let do_counter = !is_indirect;
        let mut d_damage_opt = None;
        let mut counter_info = None;

        let mut def_hp_post = defender_hp;
        def_hp_post.damage(a_damage);

        if do_counter && !def_hp_post.is_destroyed() {
            let (def_ammo1, def_ammo2) = def_ammo_opt.unwrap_or((0, 0));
            counter_info = select_weapon(
                def_ammo1,
                def_ammo2,
                defender_stats.unit_type,
                attacker_stats.unit_type,
                &damage_chart,
            );
            if let Some((_, d_base)) = counter_info {
                let d_advantage_damage = (d_base as f64 * 1.05) as u32;
                let d_damage_base = d_advantage_damage * def_hp_post.get_display_hp() / 10;

                let att_terrain = map
                    .get_terrain(attacker_pos.x, attacker_pos.y)
                    .unwrap_or(Terrain::Plains);
                let att_bonus = master_data.get_terrain_defense_bonus(att_terrain);
                let d_defense_reduction = att_bonus * attacker_hp.get_display_hp() / 10;

                let d_damage_reduced = d_damage_base.saturating_sub(d_defense_reduction);
                d_damage_opt = Some(d_damage_reduced + rng.next_bonus());
            }
        }

        let a_hp_before = attacker_hp.current;
        let d_hp_before = defender_hp.current;

        let mut a_hp_after = a_hp_before;
        let mut d_hp_after = d_hp_before;

        // Apply ammo consumption and damage using disjoint mutable borrows via get_many_mut
        if let Ok([mut attacker, mut defender]) =
            q_units.get_many_mut([event.attacker_entity, event.defender_entity])
        {
            if let Some(ref mut ammo) = attacker.2 {
                if a_weapon_slot == 1 {
                    ammo.ammo1 = ammo.ammo1.saturating_sub(1);
                } else {
                    ammo.ammo2 = ammo.ammo2.saturating_sub(1);
                }
            }
            if let Some(d_dmg) = d_damage_opt {
                attacker.1.damage(d_dmg);
                a_hp_after = attacker.1.current;
            }
            if let Some(ref mut act) = attacker.6 {
                act.0 = true; // Set action completed to true
            }

            defender.1.damage(a_damage);
            d_hp_after = defender.1.current;

            if let (Some((d_slot, _)), Some(def_ammo)) = (counter_info, defender.2.as_deref_mut()) {
                if d_slot == 1 {
                    def_ammo.ammo1 = def_ammo.ammo1.saturating_sub(1);
                } else {
                    def_ammo.ammo2 = def_ammo.ammo2.saturating_sub(1);
                }
            }
        }

        attacked_events.send(UnitAttackedEvent {
            attacker: event.attacker_entity,
            defender: event.defender_entity,
            damage_dealt: a_damage,
            counter_damage_dealt: d_damage_opt,
            attacker_hp_before: a_hp_before,
            attacker_hp_after: a_hp_after,
            defender_hp_before: d_hp_before,
            defender_hp_after: d_hp_after,
        });
    }
}

/// HPが0になったユニットを削除するシステム。
///
/// 【処理の流れ】
/// 1. 全ユニットの `Health` コンポーネントを確認します。
/// 2. `is_destroyed()` が true のユニットをデスポーンします。
/// 3. `UnitDestroyedEvent` を発行して他システムに通知します。
pub fn remove_destroyed_units_system(
    mut commands: Commands,
    q_units: Query<(Entity, &Health)>,
    mut destroyed_events: EventWriter<UnitDestroyedEvent>,
) {
    for (entity, health) in q_units.iter() {
        if health.is_destroyed() {
            commands.entity(entity).despawn();
            destroyed_events.send(UnitDestroyedEvent { entity });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attack_unit_system() {
        let mut world = World::new();

        let match_state = MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        };
        world.insert_resource(match_state);
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));

        world.insert_resource(Map::new(5, 5, Terrain::Plains, GridTopology::Square));
        world.insert_resource(GameRng::new(42));

        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Infantry, 55);
        world.insert_resource(damage_chart);
        world.insert_resource(MasterDataRegistry::load().unwrap());

        world.insert_resource(Events::<AttackUnitCommand>::default());
        world.insert_resource(Events::<UnitAttackedEvent>::default());
        world.insert_resource(Events::<UnitDestroyedEvent>::default());

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
                    movement_type: MovementType::Infantry,
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
                    movement_type: MovementType::Infantry,
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
        schedule.add_systems((attack_unit_system, remove_destroyed_units_system));
        schedule.run(&mut world);

        let hp2 = world.get::<Health>(entity_2).unwrap();
        assert_eq!(hp2.current, 47);

        let hp1 = world.get::<Health>(entity_1).unwrap();
        assert_eq!(hp1.current, 70); // Counter attacked

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

    #[test]
    fn test_attack_unit_on_property() {
        let mut world = World::new();

        world.insert_resource(MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        });
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));
        world.insert_resource(Map::new(5, 5, Terrain::Plains, GridTopology::Square));
        world.insert_resource(GameRng::new(42));
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Infantry, 50);
        world.insert_resource(damage_chart);
        world.insert_resource(MasterDataRegistry::load().unwrap());

        world.insert_resource(Events::<AttackUnitCommand>::default());
        world.insert_resource(Events::<UnitAttackedEvent>::default());

        let attacker = world
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
                    min_range: 1,
                    max_range: 1,
                    ..Default::default()
                },
                ActionCompleted(false),
                HasMoved(false),
            ))
            .id();

        // 拠点とユニットを同じ座標 (0, 1) に配置
        world.spawn((
            GridPosition { x: 0, y: 1 },
            Property::new(Terrain::Factory, Some(PlayerId(2))),
        ));

        let defender = world
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
                    min_range: 1,
                    max_range: 1,
                    ..Default::default()
                },
            ))
            .id();

        // can_attack が成功することを確認
        assert!(can_attack(attacker, defender, &mut world).is_ok());

        world.send_event(AttackUnitCommand {
            attacker_entity: attacker,
            defender_entity: defender,
        });

        let mut schedule = Schedule::default();
        schedule.add_systems(attack_unit_system);
        schedule.run(&mut world);

        // 防衛者のHPが減っていることを確認
        let hp_def = world.get::<Health>(defender).unwrap();
        assert!(hp_def.current < 100);
    }

    #[test]
    fn test_terrain_defense_scaling() {
        let mut world = World::new();
        world.insert_resource(MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        });
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));
        world.insert_resource(GameRng::new(42));
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Tank, UnitType::Infantry, 70);
        world.insert_resource(damage_chart);
        world.insert_resource(MasterDataRegistry::load().unwrap());

        world.insert_resource(Events::<AttackUnitCommand>::default());
        world.insert_resource(Events::<UnitAttackedEvent>::default());

        // Case 1: Defender on Plains (5 bonus)
        world.insert_resource(Map::new(5, 5, Terrain::Plains, GridTopology::Square));

        let attacker = world
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
                    unit_type: UnitType::Tank,
                    min_range: 1,
                    max_range: 1,
                    ..Default::default()
                },
                ActionCompleted(false),
                HasMoved(false),
            ))
            .id();

        let defender_plains = world
            .spawn((
                Health {
                    current: 100,
                    max: 100,
                },
                GridPosition { x: 0, y: 1 },
                Faction(PlayerId(2)),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    ..Default::default()
                },
            ))
            .id();

        let mut schedule = Schedule::default();
        schedule.add_systems(attack_unit_system);

        world.send_event(AttackUnitCommand {
            attacker_entity: attacker,
            defender_entity: defender_plains,
        });
        schedule.run(&mut world);

        let hp_plains = world.get::<Health>(defender_plains).unwrap().current;

        // Case 2: Defender on Mountain (40 bonus)
        world.insert_resource(GameRng::new(42)); // Reset RNG seed
        let mut map_mt = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map_mt.set_terrain(0, 1, Terrain::Mountain).unwrap();
        world.insert_resource(map_mt);

        let attacker2 = world
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
                    unit_type: UnitType::Tank,
                    min_range: 1,
                    max_range: 1,
                    ..Default::default()
                },
                ActionCompleted(false),
                HasMoved(false),
            ))
            .id();
        let defender_mt = world
            .spawn((
                Health {
                    current: 100,
                    max: 100,
                },
                GridPosition { x: 0, y: 1 },
                Faction(PlayerId(2)),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    ..Default::default()
                },
            ))
            .id();

        world.send_event(AttackUnitCommand {
            attacker_entity: attacker2,
            defender_entity: defender_mt,
        });
        schedule.run(&mut world);

        let hp_mt = world.get::<Health>(defender_mt).unwrap().current;

        // Mountain should provide MORE defense (higher HP remaining)
        assert!(
            hp_mt > hp_plains,
            "Mountain (HP={}) should provide more defense than Plains (HP={})",
            hp_mt,
            hp_plains
        );
    }
}
