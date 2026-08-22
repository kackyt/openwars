use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;
use std::collections::HashSet;

/// 空母がターン開始時に搭載ユニットへ提供できる内部HP回復量です。
const MAX_CARRIER_CARGO_REPAIR_HP: u32 = 20;
const REPAIR_COST_DENOMINATOR: u64 = 100;

/// 指定した内部HP回復量に対応する修理費を計算します。
fn calculate_repair_cost(unit_cost: u32, repaired_hp: u32) -> u32 {
    ((u64::from(unit_cost) * u64::from(repaired_hp)) / REPAIR_COST_DENOMINATOR)
        .min(u64::from(u32::MAX)) as u32
}

/// 残資金で購入可能な最大のHP回復量と費用を返します。
fn calculate_affordable_repair(unit_cost: u32, desired_repair: u32, funds: u32) -> (u32, u32) {
    debug_assert!(unit_cost > 0, "unit_cost must be greater than zero");

    // floor(unit_cost * repaired_hp / 100) <= funds を満たす最大値を直接求めます。
    let maximum_repair_numerator = (u64::from(funds) + 1) * REPAIR_COST_DENOMINATOR - 1;
    let repaired_hp =
        (maximum_repair_numerator / u64::from(unit_cost)).min(u64::from(desired_repair)) as u32;
    let cost = calculate_repair_cost(unit_cost, repaired_hp);

    (repaired_hp, cost)
}

fn apply_daily_updates_for_unit(
    stats: &UnitStats,
    pos: &GridPosition,
    map: &Map,
    fuel: &mut Fuel,
    hp: &mut Health,
) {
    if hp.is_destroyed() {
        return;
    }
    if stats.movement_type == MovementType::Air {
        let terrain = map.get_terrain(pos.x, pos.y);
        if terrain != Some(Terrain::Airport) {
            if fuel.current == 0 {
                hp.current = 0; // Destroyed
            } else {
                fuel.current = fuel.current.saturating_sub(stats.daily_fuel_consumption);
            }
        }
    }

    if stats.movement_type == MovementType::Ship {
        let terrain = map.get_terrain(pos.x, pos.y);
        if terrain != Some(Terrain::Port) {
            if fuel.current == 0 {
                hp.current = 0; // Destroyed
            } else {
                fuel.current = fuel.current.saturating_sub(stats.daily_fuel_consumption);
            }
        }
    }
}

/// 全ユニットに対して日次更新（燃料消費、墜落判定）を適用します。
#[allow(clippy::type_complexity)]
fn run_daily_update_for_all(
    q_units: &mut Query<(
        Entity,
        &mut HasMoved,
        &mut ActionCompleted,
        &Faction,
        &UnitStats,
        &mut Fuel,
        &mut Ammo,
        &mut Health,
        &GridPosition,
        Option<&Transporting>,
    )>,
    map: &Map,
) {
    for (_, _, _, _, stats, mut fuel, _, mut hp, pos, transporting) in q_units.iter_mut() {
        // 搭載中の航空ユニットは空母の格納庫で保護されるため、日次燃料消費と墜落判定を行いません。
        if transporting.is_none() {
            apply_daily_updates_for_unit(stats, pos, map, &mut fuel, &mut hp);
        }
    }
}

/// フェーズの進行、ターンの切り替え、拠点による資金増加と自動補給を管理します。
pub fn next_phase_system(
    mut next_phase_events: EventReader<NextPhaseCommand>,
    mut commands: Commands,
) {
    for _ in next_phase_events.read() {
        commands.queue(|world: &mut World| {
            advance_next_phase(world);
        });
    }
}

/// 次のフェーズ、または次のプレイヤーのターンへ移行させます。
/// この関数はシステム内から、あるいは初期化時などに直接呼び出すことができます。
#[allow(clippy::type_complexity)]
pub fn advance_next_phase(world: &mut World) {
    use bevy_ecs::system::SystemState;

    let mut system_state: SystemState<(
        Commands,
        ResMut<MatchState>,
        Query<(
            Entity,
            &mut HasMoved,
            &mut ActionCompleted,
            &Faction,
            &UnitStats,
            &mut Fuel,
            &mut Ammo,
            &mut Health,
            &GridPosition,
            Option<&Transporting>,
        )>,
        ResMut<Players>,
        Query<(&GridPosition, &Property)>,
        Res<Map>,
        Res<MasterDataRegistry>,
        Option<ResMut<ProductionDiagnostic>>,
        EventWriter<GamePhaseChangedEvent>,
    )> = SystemState::new(world);

    let (
        mut commands,
        mut match_state,
        mut q_units,
        mut players,
        q_properties,
        map,
        registry,
        mut diagnostic,
        mut phase_changed_events,
    ) = system_state.get_mut(world);

    if match_state.game_over.is_some() {
        return;
    }

    // 1. 全ユニットの状態をリセット
    for (_, mut has_moved, mut action_completed, _, _, _, _, _, _, _) in q_units.iter_mut() {
        has_moved.0 = false;
        action_completed.0 = false;
    }

    // 2. 移動履歴・AIクールダウンを強制削除 (ターン終了時にリセット)
    commands.remove_resource::<PendingMove>();
    commands.remove_resource::<crate::ai::engine::AiActionCooldown>();
    commands.remove_resource::<crate::ai::engine::AiProductionCooldown>();
    commands.remove_resource::<crate::ai::engine::AiTurnStrategyCache>();

    // 3. プレイヤーの切り替え
    match_state.active_player_index.0 += 1;

    // 4. プレイヤー一周による日次更新
    if match_state.active_player_index.0 >= players.0.len() {
        match_state.active_player_index.0 = 0;
        match_state.current_turn_number.0 += 1;

        // 全ユニットの日次更新（燃料消費・墜落）を実行
        run_daily_update_for_all(&mut q_units, &map);
    }

    match_state.current_phase = Phase::Main;
    let active_player_id = players.0[match_state.active_player_index.0].id;

    // 5. 資金増加
    apply_income(
        active_player_id,
        &mut players,
        &q_properties,
        &registry,
        diagnostic.as_deref_mut(),
    );

    // 6. ユニット補給
    apply_unit_resupply(active_player_id, &mut players, &q_properties, &mut q_units);

    // 7. UIへ通知 (Mainフェーズ開始のみ通知)
    phase_changed_events.send(GamePhaseChangedEvent {
        new_phase: Phase::Main,
        active_player: active_player_id,
    });

    // 変更をワールドに適用（Commandsの実行など）
    system_state.apply(world);

    // 物件補給の後に、空母を洋上の移動補給拠点として搭載ユニットをサービスします。
    apply_carrier_cargo_service(world, active_player_id);
}

/// プレイヤーの所有物件に基づいて資金を増加させます。
fn apply_income(
    active_player_id: PlayerId,
    players: &mut Players,
    q_properties: &Query<(&GridPosition, &Property)>,
    registry: &MasterDataRegistry,
    mut diagnostic: Option<&mut ProductionDiagnostic>,
) {
    if let Some(ref mut diag) = diagnostic {
        diag.income_log.clear();
    }

    // 1. Map & Reduce: 所有物件から合計金額を算出
    let budget_increase: u32 = q_properties
        .iter()
        .filter(|(_, prop)| prop.owner_id == Some(active_player_id))
        .map(|(_, prop)| registry.landscape_income(prop.terrain.as_str()))
        .sum();

    // 2. Diagnostic への書き込み
    if let Some(ref mut diag) = diagnostic {
        diag.income_log.push(format!("Total: {}G", budget_increase));
    }

    // 3. プレイヤー資金への反映
    if let Some(player) = players.0.iter_mut().find(|p| p.id == active_player_id) {
        player.funds += budget_increase;
    }
}

/// 自軍ターン開始時に空母へ搭載されたユニットを修理・補給します。
fn apply_carrier_cargo_service(world: &mut World, active_player_id: PlayerId) {
    let Some(mut remaining_funds) = world
        .get_resource::<Players>()
        .and_then(|players| {
            players
                .0
                .iter()
                .find(|player| player.id == active_player_id)
        })
        .map(|player| player.funds)
    else {
        return;
    };

    // 資金配分を決定的にするため、空母はEntity ID順、搭載ユニットは搭載順で処理します。
    let mut carriers: Vec<(Entity, Vec<Entity>)> = {
        let mut query = world.query_filtered::<
            (Entity, &Faction, &UnitStats, &Health, &CargoCapacity),
            Without<Transporting>,
        >();
        query
            .iter(world)
            .filter(|(_, faction, stats, health, _)| {
                faction.0 == active_player_id
                    && stats.unit_type == UnitType::Carrier
                    && !health.is_destroyed()
            })
            .map(|(entity, _, _, _, cargo)| (entity, cargo.loaded.clone()))
            .collect()
    };
    carriers.sort_by_key(|(entity, _)| entity.to_bits());

    let mut processed_cargo = HashSet::new();
    for (carrier, cargo_entities) in carriers {
        for cargo in cargo_entities {
            if !processed_cargo.insert(cargo) {
                continue;
            }

            let Some(transporting) = world.get::<Transporting>(cargo) else {
                continue;
            };
            if transporting.0 != carrier {
                continue;
            }

            let (unit_cost, desired_repair) = {
                let (Some(faction), Some(stats), Some(health)) = (
                    world.get::<Faction>(cargo),
                    world.get::<UnitStats>(cargo),
                    world.get::<Health>(cargo),
                ) else {
                    continue;
                };
                if faction.0 != active_player_id || health.is_destroyed() {
                    continue;
                }

                (
                    stats.cost,
                    MAX_CARRIER_CARGO_REPAIR_HP.min(health.max.saturating_sub(health.current)),
                )
            };

            let (repaired_hp, repair_cost) =
                calculate_affordable_repair(unit_cost, desired_repair, remaining_funds);
            remaining_funds -= repair_cost;

            let mut cargo_entity = world.entity_mut(cargo);
            if repaired_hp > 0 {
                let mut health = cargo_entity
                    .get_mut::<Health>()
                    .expect("validated cargo must retain Health");
                health.current = (health.current + repaired_hp).min(health.max);
            }
            // 空母の補給はHP修理だけを課金し、燃料と弾薬は資金に関係なく最大化します。
            if let Some(mut fuel) = cargo_entity.get_mut::<Fuel>() {
                fuel.resupply();
            }
            if let Some(mut ammo) = cargo_entity.get_mut::<Ammo>() {
                ammo.resupply();
            }
        }
    }

    if let Some(player) = world
        .resource_mut::<Players>()
        .0
        .iter_mut()
        .find(|player| player.id == active_player_id)
    {
        player.funds = remaining_funds;
    }
}

/// プレイヤーの所有物件に滞在しているユニットの補給（燃料・弾薬・HP回復）を行います。
#[allow(clippy::type_complexity)]
fn apply_unit_resupply(
    active_player_id: PlayerId,
    players: &mut Players,
    q_properties: &Query<(&GridPosition, &Property)>,
    q_units: &mut Query<(
        Entity,
        &mut HasMoved,
        &mut ActionCompleted,
        &Faction,
        &UnitStats,
        &mut Fuel,
        &mut Ammo,
        &mut Health,
        &GridPosition,
        Option<&Transporting>,
    )>,
) {
    // 補給可能な物件の座標を収集
    let mut resupply_tiles = HashSet::new();
    for (pos, prop) in q_properties.iter() {
        if prop.owner_id == Some(active_player_id)
            && (prop.terrain == Terrain::City
                || prop.terrain == Terrain::Airport
                || prop.terrain == Terrain::Factory
                || prop.terrain == Terrain::Port
                || prop.terrain == Terrain::Capital)
        {
            resupply_tiles.insert((pos.x, pos.y));
        }
    }

    let active_player_idx = players
        .0
        .iter()
        .position(|p| p.id == active_player_id)
        .unwrap();

    // 物件補給の実行
    for (_, _, _, faction, stats, mut fuel, mut ammo, mut hp, pos, _) in q_units.iter_mut() {
        if faction.0 == active_player_id {
            if hp.is_destroyed() {
                continue;
            }
            if resupply_tiles.contains(&(pos.x, pos.y)) {
                // 回復・補充にかかるコストを計算
                // HP回復（最大20回復）
                let hp_to_restore = 20.min(hp.max.saturating_sub(hp.current));
                let repair_cost = stats.cost * hp_to_restore / 100;

                let ammo1_diff = stats.max_ammo1.saturating_sub(ammo.ammo1);
                let ammo2_diff = stats.max_ammo2.saturating_sub(ammo.ammo2);
                let fuel_diff = stats.max_fuel.saturating_sub(fuel.current);
                let resupply_cost =
                    ammo1_diff * stats.ammo1_cost + ammo2_diff * stats.ammo2_cost + fuel_diff * 5;

                let total_cost = repair_cost + resupply_cost;

                if players.0[active_player_idx].funds >= total_cost && total_cost > 0 {
                    players.0[active_player_idx].funds -= total_cost;
                    hp.current = (hp.current + hp_to_restore).min(hp.max);
                    fuel.current = stats.max_fuel;
                    ammo.ammo1 = stats.max_ammo1;
                    ammo.ammo2 = stats.max_ammo2;
                } else if players.0[active_player_idx].funds >= resupply_cost && resupply_cost > 0 {
                    // 資金不足で修理はできないが、補給だけはできる場合
                    players.0[active_player_idx].funds -= resupply_cost;
                    fuel.current = stats.max_fuel;
                    ammo.ammo1 = stats.max_ammo1;
                    ammo.ammo2 = stats.max_ammo2;
                }
            }
        }
    }
}

/// ユニットの待機コマンドを処理します。
#[allow(clippy::type_complexity)]
pub fn wait_unit_system(
    mut wait_events: EventReader<WaitUnitCommand>,
    mut waited_writer: EventWriter<UnitWaitedEvent>,
    mut q_units: ParamSet<(
        Query<(Entity, &GridPosition, &Faction, Option<&Transporting>)>,
        Query<(&Faction, &mut ActionCompleted)>,
        Query<(&mut GridPosition, &mut Fuel, &mut HasMoved)>,
    )>,
    players: Res<Players>,
    match_state: Res<MatchState>,
    pending_move: Option<Res<PendingMove>>,
    mut commands: Commands,
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Main {
        return;
    }
    let active_player = players.0[match_state.active_player_index.0].id;

    for ev in wait_events.read() {
        let occupied_by_other = {
            let positions = q_units.p0();
            let Ok((_, unit_position, faction, _)) = positions.get(ev.unit_entity) else {
                continue;
            };
            if faction.0 != active_player {
                continue;
            }
            positions.iter().any(|(entity, position, _, transporting)| {
                entity != ev.unit_entity && transporting.is_none() && *position == *unit_position
            })
        };
        if occupied_by_other {
            // 移動後の占有マスではWaitはルール上選べない。誤った複合コマンドが
            // 届いても重複配置を確定せず、直前の移動を取り消す。
            if let Some(pending) = pending_move.as_deref()
                && pending.unit_entity == ev.unit_entity
            {
                if let Ok((mut position, mut fuel, mut has_moved)) =
                    q_units.p2().get_mut(ev.unit_entity)
                {
                    *position = pending.original_pos;
                    *fuel = pending.original_fuel;
                    has_moved.0 = false;
                }
                commands.remove_resource::<PendingMove>();
            }
            continue;
        }

        if let Ok((faction, mut action_comp)) = q_units.p1().get_mut(ev.unit_entity) {
            if faction.0 != active_player {
                continue;
            }
            action_comp.0 = true;
            // ユニット待機完了イベントを送出
            waited_writer.send(UnitWaitedEvent {
                entity: ev.unit_entity,
            });
            // アクション確定時に移動履歴を削除
            commands.remove_resource::<PendingMove>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_world() -> (World, Schedule) {
        let mut world = World::new();
        let mut schedule = Schedule::default();

        world.insert_resource(MatchState::default());
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));
        world.insert_resource(Map::new(5, 5, Terrain::Plains, GridTopology::Square));
        world.insert_resource(MasterDataRegistry::load().unwrap());
        world.init_resource::<Events<NextPhaseCommand>>();
        world.init_resource::<Events<GamePhaseChangedEvent>>();

        schedule.add_systems(next_phase_system);

        (world, schedule)
    }

    #[test]
    fn test_affordable_repair_uses_floor_cost_boundary() {
        // 150Gユニットは内部HP 1の修理費が floor(150 / 100) = 1G です。
        let (repaired_hp, cost) = calculate_affordable_repair(150, 20, 1);

        assert_eq!(repaired_hp, 1);
        assert_eq!(cost, 1);
    }

    #[test]
    fn wait_on_another_unit_rolls_back_the_illegal_move() {
        let (mut world, mut schedule) = setup_world();
        world.init_resource::<Events<WaitUnitCommand>>();
        world.init_resource::<Events<UnitWaitedEvent>>();
        schedule.add_systems(wait_unit_system);

        let player = PlayerId(1);
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Faction(player),
            ActionCompleted(false),
        ));
        let cargo = world
            .spawn((
                GridPosition { x: 1, y: 0 },
                Faction(player),
                ActionCompleted(false),
                HasMoved(true),
                Fuel {
                    current: 98,
                    max: 99,
                },
            ))
            .id();
        world.insert_resource(PendingMove {
            unit_entity: cargo,
            original_pos: GridPosition { x: 0, y: 0 },
            original_fuel: Fuel {
                current: 99,
                max: 99,
            },
        });
        world.send_event(WaitUnitCommand { unit_entity: cargo });

        schedule.run(&mut world);

        assert_eq!(
            *world.get::<GridPosition>(cargo).unwrap(),
            GridPosition { x: 0, y: 0 }
        );
        assert_eq!(world.get::<Fuel>(cargo).unwrap().current, 99);
        assert!(!world.get::<HasMoved>(cargo).unwrap().0);
        assert!(!world.get::<ActionCompleted>(cargo).unwrap().0);
        assert!(world.get_resource::<PendingMove>().is_none());
        assert!(world.resource::<Events<UnitWaitedEvent>>().is_empty());
    }

    #[test]
    fn test_turn_progression() {
        let (mut world, mut schedule) = setup_world();

        // Initially Player 1, Turn 1
        {
            let ms = world.resource::<MatchState>();
            assert_eq!(ms.active_player_index.0, 0);
            assert_eq!(ms.current_turn_number.0, 1);
        }

        // P1 -> P2
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);
        {
            let ms = world.resource::<MatchState>();
            assert_eq!(ms.active_player_index.0, 1);
            assert_eq!(ms.current_turn_number.0, 1);
            assert_eq!(ms.current_phase, Phase::Main);
        }

        // P2 -> P1 (New Turn)
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);
        {
            let ms = world.resource::<MatchState>();
            assert_eq!(ms.active_player_index.0, 0);
            assert_eq!(ms.current_turn_number.0, 2);
        }
    }

    #[test]
    fn test_air_unit_fuel_and_crash() {
        let (mut world, mut schedule) = setup_world();

        // Spawn a bomber for P1 (consumes 5 fuel per round)
        let bomber = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Bomber,
                    movement_type: MovementType::Air,
                    daily_fuel_consumption: 5,
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 10,
                    max: 50,
                },
                Health {
                    current: 100,
                    max: 100,
                },
                HasMoved(false),
                ActionCompleted(false),
                Ammo {
                    ammo1: 0,
                    max_ammo1: 0,
                    ammo2: 0,
                    max_ammo2: 0,
                },
            ))
            .id();

        // Round 1: P1 -> P2 (NextPhaseCommand 1)
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);
        {
            let fuel = world.get::<Fuel>(bomber).unwrap();
            assert_eq!(
                fuel.current, 10,
                "Fuel should not decrease on midway phase change"
            );
        }

        // Round 1 ends: P2 -> P1 (NextPhaseCommand 2)
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);
        {
            let fuel = world.get::<Fuel>(bomber).unwrap();
            assert_eq!(
                fuel.current, 5,
                "Fuel should decrease exactly once per full round"
            );
        }

        // Round 2 ends: P2 -> P1 (NextPhaseCommand 4 total)
        world.send_event(NextPhaseCommand); // P1 -> P2
        schedule.run(&mut world);
        world.send_event(NextPhaseCommand); // P2 -> P1
        schedule.run(&mut world);
        {
            let fuel = world.get::<Fuel>(bomber).unwrap();
            assert_eq!(fuel.current, 0, "Fuel should be 0");
        }

        // Round 3 ends: P2 -> P1 (Crash)
        world.send_event(NextPhaseCommand); // P1 -> P2
        schedule.run(&mut world);
        world.send_event(NextPhaseCommand); // P2 -> P1
        schedule.run(&mut world);
        {
            let hp = world.get::<Health>(bomber).unwrap();
            assert_eq!(
                hp.current, 0,
                "Aircraft with 0 fuel not on airport should crash"
            );
        }
    }

    #[test]
    fn test_all_units_reset_on_next_phase() {
        let (mut world, mut schedule) = setup_world();

        // P1 unit (will act)
        let p1_unit = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                Faction(PlayerId(1)),
                UnitStats::mock(),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: 10,
                    max: 10,
                },
                Ammo {
                    ammo1: 0,
                    max_ammo1: 0,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                HasMoved(true),
                ActionCompleted(true),
            ))
            .id();

        // P2 unit (already acted somehow, maybe in previous turn)
        let p2_unit = world
            .spawn((
                GridPosition { x: 1, y: 1 },
                Faction(PlayerId(2)),
                UnitStats::mock(),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: 10,
                    max: 10,
                },
                Ammo {
                    ammo1: 0,
                    max_ammo1: 0,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                HasMoved(true),
                ActionCompleted(true),
            ))
            .id();

        // P1 turn ends -> P2 turn starts
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);

        // BOTH units should be reset immediately
        let p1_moved = world.get::<HasMoved>(p1_unit).unwrap();
        let p1_action = world.get::<ActionCompleted>(p1_unit).unwrap();
        let p2_moved = world.get::<HasMoved>(p2_unit).unwrap();
        let p2_action = world.get::<ActionCompleted>(p2_unit).unwrap();

        assert!(!p1_moved.0, "P1 unit should be reset");
        assert!(!p1_action.0, "P1 unit should be reset");
        assert!(!p2_moved.0, "P2 unit should be reset");
        assert!(!p2_action.0, "P2 unit should be reset");
    }

    #[test]
    fn test_resupply_cost_calculation() {
        let (mut world, mut schedule) = setup_world();

        // P1 starts with 10000 funds
        {
            let mut players = world.resource_mut::<Players>();
            players.0[0].funds = 10000;
        }

        // City for P1 at (0, 0)
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::City, Some(PlayerId(1)), 200),
        ));

        // Heavy Tank for P1 at city (0, 0)
        let tank = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::TankZ,
                    cost: 16000,
                    max_fuel: 50,
                    max_ammo1: 8,
                    ammo1_cost: 50, // 1発 50G
                    max_ammo2: 0,
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 40, // 10 diff * 5G = 50G
                    max: 50,
                },
                Health {
                    current: 80, // 20 diff -> 3200G
                    max: 100,
                },
                Ammo {
                    ammo1: 4, // 4 diff * 50G = 200G
                    max_ammo1: 8,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();

        // Phase 1: P1 Main -> P2 Main
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);

        // Phase 2: P2 Main -> P1 Main (Resupply happens here)
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);

        // Expected costs:
        // Repair: 16000 * 20 / 100 = 3200
        // Fuel: (50-40) * 5 = 50
        // Ammo: (8-4) * 50 = 200
        // Total cost: 3450
        // Income: 1000 (from 1 city)
        // Funds: 10000 + 1000 - 3450 = 7550

        let player1 = &world.resource::<Players>().0[0];
        assert_eq!(player1.funds, 7550);

        // Check if unit is restored
        let hp = world.get::<Health>(tank).unwrap();
        assert_eq!(hp.current, 100);
        let fuel = world.get::<Fuel>(tank).unwrap();
        assert_eq!(fuel.current, 50);
        let ammo = world.get::<Ammo>(tank).unwrap();
        assert_eq!(ammo.ammo1, 8);
    }

    #[test]
    fn test_ai_cooldown_reset_on_next_phase() {
        let (mut world, mut schedule) = setup_world();

        let entity = world.spawn_empty().id();

        // クールダウンをセット
        world.insert_resource(crate::ai::engine::AiActionCooldown(
            [entity].into_iter().collect(),
        ));
        world.insert_resource(crate::ai::engine::AiProductionCooldown(
            [(5, 5)].into_iter().collect(),
        ));
        world.insert_resource(crate::ai::engine::AiTurnStrategyCache::default());

        // ターン終了 -> P2へ
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);

        // クールダウンが削除されていることを確認
        assert!(
            world
                .get_resource::<crate::ai::engine::AiActionCooldown>()
                .is_none()
        );
        assert!(
            world
                .get_resource::<crate::ai::engine::AiProductionCooldown>()
                .is_none()
        );
        assert!(
            world
                .get_resource::<crate::ai::engine::AiTurnStrategyCache>()
                .is_none()
        );
    }

    fn spawn_carrier_with_cargo(
        world: &mut World,
        cargo_health: u32,
        cargo_fuel: u32,
        funds: u32,
    ) -> (Entity, Entity) {
        world.resource_mut::<Players>().0[0].funds = funds;

        let carrier = world
            .spawn((
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Carrier,
                    ..UnitStats::mock()
                },
                Health {
                    current: 40,
                    max: 100,
                },
                CargoCapacity {
                    max: 2,
                    loaded: vec![],
                },
            ))
            .id();
        let cargo = world
            .spawn((
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Fighter,
                    cost: 16_000,
                    movement_type: MovementType::Air,
                    daily_fuel_consumption: 5,
                    ..UnitStats::mock()
                },
                Health {
                    current: cargo_health,
                    max: 100,
                },
                Fuel {
                    current: cargo_fuel,
                    max: 50,
                },
                Ammo {
                    ammo1: 1,
                    max_ammo1: 6,
                    ammo2: 0,
                    max_ammo2: 2,
                },
                Transporting(carrier),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 9999, y: 9999 },
            ))
            .id();
        world
            .get_mut::<CargoCapacity>(carrier)
            .unwrap()
            .loaded
            .push(cargo);

        (carrier, cargo)
    }

    #[test]
    fn test_carrier_cargo_service_repairs_and_resupplies_at_turn_start() {
        let (mut world, mut schedule) = setup_world();
        let (_, cargo) = spawn_carrier_with_cargo(&mut world, 80, 10, 5_000);

        // P1 -> P2 -> P1 と進め、自軍ターン開始時のサービスを確認します。
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);

        assert_eq!(world.get::<Health>(cargo).unwrap().current, 100);
        assert_eq!(world.get::<Fuel>(cargo).unwrap().current, 50);
        let ammo = world.get::<Ammo>(cargo).unwrap();
        assert_eq!((ammo.ammo1, ammo.ammo2), (6, 2));
        assert_eq!(world.resource::<Players>().0[0].funds, 1_800);
    }

    #[test]
    fn test_carrier_cargo_service_repairs_partially_but_resupplies_for_free() {
        let (mut world, _) = setup_world();
        let (_, cargo) = spawn_carrier_with_cargo(&mut world, 80, 0, 1_500);

        apply_carrier_cargo_service(&mut world, PlayerId(1));

        // 16,000Gのユニットは内部HP 9回復で1,440Gとなり、残り60Gでは次の1HPを購入できません。
        assert_eq!(world.get::<Health>(cargo).unwrap().current, 89);
        assert_eq!(world.resource::<Players>().0[0].funds, 60);
        assert_eq!(world.get::<Fuel>(cargo).unwrap().current, 50);
        let ammo = world.get::<Ammo>(cargo).unwrap();
        assert_eq!((ammo.ammo1, ammo.ammo2), (6, 2));
    }

    #[test]
    fn test_carrier_cargo_service_uses_load_order_for_limited_funds() {
        let (mut world, _) = setup_world();
        let (carrier, first_cargo) = spawn_carrier_with_cargo(&mut world, 80, 0, 4_000);
        let second_cargo = world
            .spawn((
                Faction(PlayerId(1)),
                UnitStats {
                    unit_type: UnitType::Fighter,
                    cost: 16_000,
                    ..UnitStats::mock()
                },
                Health {
                    current: 80,
                    max: 100,
                },
                Fuel {
                    current: 0,
                    max: 50,
                },
                Ammo {
                    ammo1: 0,
                    max_ammo1: 6,
                    ammo2: 0,
                    max_ammo2: 2,
                },
                Transporting(carrier),
            ))
            .id();
        world
            .get_mut::<CargoCapacity>(carrier)
            .unwrap()
            .loaded
            .push(second_cargo);

        apply_carrier_cargo_service(&mut world, PlayerId(1));

        // 搭載順の先頭が20HP、残り800Gを次の搭載ユニットが5HP分使用します。
        assert_eq!(world.get::<Health>(first_cargo).unwrap().current, 100);
        assert_eq!(world.get::<Health>(second_cargo).unwrap().current, 85);
        assert_eq!(world.resource::<Players>().0[0].funds, 0);
    }

    #[test]
    fn test_transported_aircraft_is_not_destroyed_before_carrier_service() {
        let (mut world, mut schedule) = setup_world();
        let (_, cargo) = spawn_carrier_with_cargo(&mut world, 100, 0, 0);

        // ラウンド切替時でも搭載中は墜落せず、空母が燃料を補給します。
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);
        world.send_event(NextPhaseCommand);
        schedule.run(&mut world);

        assert_eq!(world.get::<Health>(cargo).unwrap().current, 100);
        assert_eq!(world.get::<Fuel>(cargo).unwrap().current, 50);
    }
}
