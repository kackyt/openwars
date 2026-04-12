use crate::components::*;
use crate::events::*;
use crate::resources::master_data::UnitName;
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
    let mut q_attacker = world.query::<(
        &GridPosition,
        &UnitStats,
        Option<&HasMoved>,
        &Faction,
        Option<&Ammo>,
    )>();
    let mut q_target = world.query::<(&GridPosition, &UnitStats, &Faction)>();

    let (a_pos, a_stats, a_has_moved, a_fac, a_ammo) = q_attacker
        .get(world, attacker_entity)
        .map_err(|_| AttackError::InvalidEntity)?;

    // a_statsの所有権問題を回避するため、必要な情報をクローン/コピーしておく
    let a_pos_val = *a_pos;
    let a_fac_val = a_fac.0;
    let a_type_name = a_stats.unit_type.as_str();
    let a_ammo1 = a_ammo.map(|a| a.ammo1).unwrap_or(0);
    let a_ammo2 = a_ammo.map(|a| a.ammo2).unwrap_or(0);

    let has_moved_val = if let Some(pm) = world.get_resource::<crate::resources::PendingMove>() {
        if pm.unit_entity == attacker_entity {
            a_pos_val.x != pm.original_pos.x || a_pos_val.y != pm.original_pos.y
        } else {
            a_has_moved.is_some_and(|m| m.0)
        }
    } else {
        a_has_moved.is_some_and(|m| m.0)
    };

    let (d_pos, d_stats, d_fac) = q_target
        .get(world, defender_entity)
        .map_err(|_| AttackError::InvalidEntity)?;

    if a_fac_val == d_fac.0 {
        return Err(AttackError::FriendlyFire);
    }

    let dist = (a_pos_val.x as i64 - d_pos.x as i64).unsigned_abs() as u32
        + (a_pos_val.y as i64 - d_pos.y as i64).unsigned_abs() as u32;

    let target_type_name = d_stats.unit_type.as_str();

    let Some(master_data) = world.get_resource::<MasterDataRegistry>() else {
        return Err(AttackError::InvalidEntity);
    };
    let unit_record = master_data.get_unit(&UnitName(a_type_name.to_string()));

    let mut indirect_after_move = false;

    if let Some(rec) = unit_record {
        // Weapon1のチェック
        if let Some(w1) = rec
            .weapon1
            .as_ref()
            .and_then(|name| master_data.weapons.get(&UnitName(name.clone())))
            && a_ammo1 > 0
            && w1.damages.get(target_type_name).copied().unwrap_or(0) > 0
            && dist >= w1.range_min
            && dist <= w1.range_max
        {
            let is_indirect = w1.range_min > 1;
            if is_indirect && has_moved_val {
                indirect_after_move = true;
            } else {
                return Ok(()); // 攻撃可能な武器が見つかった
            }
        }
        // Weapon2のチェック
        if let Some(w2) = rec
            .weapon2
            .as_ref()
            .and_then(|name| master_data.weapons.get(&UnitName(name.clone())))
            && a_ammo2 > 0
            && w2.damages.get(target_type_name).copied().unwrap_or(0) > 0
            && dist >= w2.range_min
            && dist <= w2.range_max
        {
            let is_indirect = w2.range_min > 1;
            if is_indirect && has_moved_val {
                indirect_after_move = true;
            } else {
                return Ok(());
            }
        }
    }

    // 距離は合っていたが間接攻撃の移動後制限に引っかかった場合
    if indirect_after_move {
        Err(AttackError::IndirectAfterMove)
    } else {
        // 有効な武器がない、または射程外
        Err(AttackError::OutOfRange)
    }
}

/// 指定されたユニットが現在攻撃可能な対象エンティティのリストを返します。
/// マスターデータ上の武器情報（射程、ダメージ設定）を参照し、有効な対象のみを抽出します。
/// allow_indirect が false の場合、間接攻撃武器（射程1超）を持つユニットは「移動後」とみなされ、
/// その武器での攻撃対象を返しません（近接武器があればそちらの射程で判定されます）。
pub fn get_attackable_targets(
    world: &mut World,
    attacker: Entity,
    allow_indirect: bool,
) -> Vec<Entity> {
    let mut targets = vec![];

    let (a_pos, a_stats, unit_faction, a_ammo1, a_ammo2) = {
        let mut q_attacker = world.query::<(&GridPosition, &UnitStats, &Faction, Option<&Ammo>)>();
        let Ok((a_pos, a_stats, a_faction, a_ammo)) = q_attacker.get(world, attacker) else {
            return targets;
        };
        let ammo1 = a_ammo.map(|a| a.ammo1).unwrap_or(0);
        let ammo2 = a_ammo.map(|a| a.ammo2).unwrap_or(0);
        (*a_pos, a_stats.clone(), a_faction.0, ammo1, ammo2)
    };

    let (weapon1_rec, weapon2_rec) = {
        let Some(master_data) = world.get_resource::<MasterDataRegistry>() else {
            return targets;
        };
        let unit_type_name = a_stats.unit_type.as_str();
        let unit_record = master_data.get_unit(&UnitName(unit_type_name.to_string()));
        if let Some(rec) = unit_record {
            let w1 = rec
                .weapon1
                .as_ref()
                .and_then(|w| master_data.weapons.get(&UnitName(w.clone())))
                .cloned();
            let w2 = rec
                .weapon2
                .as_ref()
                .and_then(|w| master_data.weapons.get(&UnitName(w.clone())))
                .cloned();
            (w1, w2)
        } else {
            (None, None)
        }
    };

    let mut q_targets =
        world.query_filtered::<(Entity, &GridPosition, &Faction, &UnitStats), With<Faction>>();
    for (t_ent, t_pos, t_faction, t_stats) in q_targets.iter(world) {
        if t_ent == attacker || t_faction.0 == unit_faction {
            continue;
        }

        let dist = (a_pos.x as i64 - t_pos.x as i64).unsigned_abs() as u32
            + (a_pos.y as i64 - t_pos.y as i64).unsigned_abs() as u32;

        let target_type_name = t_stats.unit_type.as_str();
        let mut can_attack = false;

        // 武器1（主武器）の判定
        if let Some(w1) = &weapon1_rec {
            // ダメージが定義されているか
            if let Some(&dmg) = w1.damages.get(target_type_name)
                && dmg > 0
                && a_ammo1 > 0
            {
                let is_indirect = w1.range_min > 1;
                // 移動制限にかからず、かつ射程内であれば攻撃可能
                if (!is_indirect || allow_indirect) && dist >= w1.range_min && dist <= w1.range_max
                {
                    can_attack = true;
                }
            }
        }

        // 武器2（副武器）の判定（主武器で攻撃不可な場合のみチェック）
        if !can_attack
            && let Some(w2) = &weapon2_rec
            && w2.damages.get(target_type_name).copied().unwrap_or(0) > 0
            && a_ammo2 > 0
        {
            let is_indirect = w2.range_min > 1;
            if (!is_indirect || allow_indirect) && dist >= w2.range_min && dist <= w2.range_max {
                can_attack = true;
            }
        }

        if can_attack {
            targets.push(t_ent);
        }
    }

    targets
}

/// 攻撃者と防衛者のユニット名、距離に基づき、MasterDataRegistry の武器情報を参照して
/// 最適な武器（主武器 または 副武器）を選択します。
///
/// 戻り値: (使用する武器のスロット番号(1 or 2), 基礎ダメージ値, は間接攻撃か) または None
fn select_weapon(
    ammo1: u32,
    ammo2: u32,
    attacker_name: &str,
    defender_name: &str,
    dist: u32,
    master_data: &MasterDataRegistry,
) -> Option<(u32, u32, bool)> {
    let unit_record = master_data.get_unit(&UnitName(attacker_name.to_string()))?;

    // Try weapon 1
    if let Some(w1) = unit_record
        .weapon1
        .as_ref()
        .and_then(|name| master_data.weapons.get(&UnitName(name.clone())))
        && ammo1 > 0
        && dist >= w1.range_min
        && dist <= w1.range_max
        && let Some(&dmg) = w1.damages.get(defender_name)
        && dmg > 0
    {
        return Some((1, dmg, w1.range_min > 1));
    }

    // Try weapon 2
    if let Some(w2) = unit_record
        .weapon2
        .as_ref()
        .and_then(|name| master_data.weapons.get(&UnitName(name.clone())))
        && dist >= w2.range_min
        && dist <= w2.range_max
    {
        // Note: secondary weapons (e.g. machine guns) usually don't consume primary ammo.
        // However, openwars seems to use ammo1 and ammo2. Most secondary weapons have infinite ammo?
        // Let's assume ammo2 > 0 is required if max_ammo2 > 0, but for now we'll just check if it's usable.
        // In advance wars, secondary weapons have infinite ammo. So we will skip ammo2 check here, or assume
        // openwars models it such that ammo2 is handled elsewhere.
        if ammo2 > 0
            && let Some(&dmg) = w2.damages.get(defender_name)
            && dmg > 0
        {
            return Some((2, dmg, w2.range_min > 1));
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
    master_data: Res<MasterDataRegistry>,
    map: Res<Map>,
    mut rng: ResMut<GameRng>,
    mut commands: Commands,
    pending_move: Option<Res<PendingMove>>,
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
            attacker_has_moved_comp,
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

        let attacker_has_moved = if let Some(pm) = pending_move.as_ref() {
            if pm.unit_entity == event.attacker_entity {
                attacker_pos.x != pm.original_pos.x || attacker_pos.y != pm.original_pos.y
            } else {
                attacker_has_moved_comp
            }
        } else {
            attacker_has_moved_comp
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
            attacker_stats.unit_type.as_str(),
            defender_stats.unit_type.as_str(),
            dist,
            &master_data,
        );
        let (a_weapon_slot, a_base_damage, is_indirect) = match attacker_weapon {
            Some(w) => w,
            None => continue,
        };

        if is_indirect && attacker_has_moved {
            continue;
        }

        let def_terrain = map
            .get_terrain(defender_pos.x, defender_pos.y)
            .unwrap_or(Terrain::Plains);
        let def_bonus = master_data.get_terrain_defense_bonus(def_terrain);

        let a_damage =
            (a_base_damage * attacker_hp.current + 105) / (100 + def_bonus) + rng.next_bonus();

        let do_counter = !is_indirect;
        let mut d_damage_opt = None;
        let mut counter_info = None;

        let mut def_hp_post = defender_hp;
        def_hp_post.damage(a_damage);

        if do_counter {
            let (def_ammo1, def_ammo2) = def_ammo_opt.unwrap_or((0, 0));
            counter_info = select_weapon(
                def_ammo1,
                def_ammo2,
                defender_stats.unit_type.as_str(),
                attacker_stats.unit_type.as_str(),
                dist,
                &master_data,
            );
            if let Some((_, d_base_damage, _)) = counter_info {
                let att_terrain = map
                    .get_terrain(attacker_pos.x, attacker_pos.y)
                    .unwrap_or(Terrain::Plains);
                let att_bonus = master_data.get_terrain_defense_bonus(att_terrain);

                let d_damage = (d_base_damage * defender_hp.current + 100) / (100 + att_bonus)
                    + rng.next_bonus();

                d_damage_opt = Some(d_damage);
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

            if let (Some((d_slot, _, _)), Some(def_ammo)) =
                (counter_info, defender.2.as_deref_mut())
            {
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

        // 攻撃確定時に移動履歴を削除
        commands.remove_resource::<PendingMove>();
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
        // Calculation: (45 * 100 + 105) / (100 + 5) + 1 = 4605 / 105 + 1 = 43 + 1 = 44.
        // 100 - 44 = 56.
        assert_eq!(hp2.current, 56);

        let hp1 = world.get::<Health>(entity_1).unwrap();
        // Counter calculation: (45 * 100 + 105) / (100 + 5) + 7 = 43 + 7 = 50.
        // 100 - 50 = 50.
        assert_eq!(hp1.current, 50); // Counter attacked

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
                    max_ammo1: 9,
                    max_ammo2: 0,
                    ..Default::default()
                },
                ActionCompleted(false),
                HasMoved(false),
            ))
            .id();

        // 拠点とユニットを同じ座標 (0, 1) に配置
        // Map資源も更新する必要がある（戦闘システムはMap資源から地形を取得するため）
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 1, Terrain::Factory).unwrap();
        world.insert_resource(map);

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
        // (45 * 100 + 105) / (100 + 20) + 1 = 4605 / 120 + 1 = 38 + 1 = 39.
        // 100 - 39 = 61.
        // Note: Factory has 20 bonus in landscape.csv
        assert_eq!(hp_def.current, 61);
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
        assert!(hp_mt > hp_plains);
        // Case 1 (Plains: 5): (38 * 100 + 105) / 105 + 1 = 3905 / 105 + 1 = 37 + 1 = 38. 100 - 38 = 62.
        assert_eq!(hp_plains, 62);
        // Case 2 (Mountain: 40): (38 * 100 + 105) / 140 + 1 = 3905 / 140 + 1 = 27 + 1 = 28. 100 - 28 = 72.
        assert_eq!(hp_mt, 72);
    }
}
