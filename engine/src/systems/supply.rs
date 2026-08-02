use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;

/// 補給輸送車が補給できるユニット種別かを判定します。
///
/// 地上ユニット全般と軽戦闘機だけを補給対象とし、艦船およびその他の航空ユニットは対象外です。
fn is_suppliable_unit_type(unit_type: UnitType) -> bool {
    matches!(
        unit_type,
        UnitType::Infantry
            | UnitType::Mech
            | UnitType::Recon
            | UnitType::Tank
            | UnitType::MdTank
            | UnitType::TankZ
            | UnitType::Artillery
            | UnitType::LightSpGun
            | UnitType::HeavySpGun
            | UnitType::Rockets
            | UnitType::AntiAir
            | UnitType::Missiles
            | UnitType::SupplyTruck
            | UnitType::Fighter
    )
}

/// 補給対象が補給者の指定位置から合法かを判定します。
///
/// 対象候補の表示とコマンド実行時の検証で同じ条件を使用し、UI と engine の判定を一致させます。
#[allow(clippy::too_many_arguments)]
fn is_valid_supply_target(
    supplier: Entity,
    supplier_faction: PlayerId,
    supplier_position: GridPosition,
    target: Entity,
    target_position: GridPosition,
    target_faction: PlayerId,
    target_unit_type: UnitType,
    target_health: Health,
    target_action_completed: bool,
    target_is_transporting: bool,
    topology: GridTopology,
) -> bool {
    supplier != target
        && supplier_faction == target_faction
        && !target_health.is_destroyed()
        && !target_action_completed
        && !target_is_transporting
        && is_suppliable_unit_type(target_unit_type)
        && topology.distance(
            (supplier_position.x, supplier_position.y),
            (target_position.x, target_position.y),
        ) == 1
}

/// 補給車などによる隣接ユニットへの補給コマンド(`SupplyUnitCommand`)を処理するシステム。
///
/// 【処理の流れ】
/// 1. 補給者(`supplier_entity`)が自軍であり、行動済みでなく、補給能力(`can_supply`)を持つことを確認します。
/// 2. 補給対象(`target_entity`)が未行動の自軍ユニットであり、補給対象種別かつ隣接していることを確認します。
/// 3. 対象の燃料(`Fuel`)と弾薬(`Ammo`)を最大値まで回復させます。
/// 4. 補給者の `ActionCompleted` を true に設定します。対象の HP・行動状態・移動状態は変更しません。
///
pub fn get_suppliable_targets(world: &mut World, supplier: Entity) -> Vec<Entity> {
    let Some(s_pos) = world.get::<GridPosition>(supplier).cloned() else {
        return vec![];
    };
    get_suppliable_targets_at(world, supplier, s_pos)
}

/// 指定された位置で補給可能な対象エンティティのリストを返します。
pub fn get_suppliable_targets_at(
    world: &mut World,
    supplier: Entity,
    supplier_position: GridPosition,
) -> Vec<Entity> {
    let mut targets = vec![];
    let (
        supplier_faction,
        supplier_can_supply,
        supplier_is_alive,
        supplier_is_unacted,
        supplier_is_transporting,
    ) = {
        let mut query = world.query::<(
            &Faction,
            &UnitStats,
            &Health,
            &ActionCompleted,
            Option<&Transporting>,
        )>();
        let Ok((faction, stats, health, action_completed, transporting)) =
            query.get(world, supplier)
        else {
            return targets;
        };
        (
            faction.0,
            stats.can_supply,
            !health.is_destroyed(),
            !action_completed.0,
            transporting.is_some(),
        )
    };

    if !supplier_can_supply
        || !supplier_is_alive
        || !supplier_is_unacted
        || supplier_is_transporting
    {
        return targets;
    }

    // マップのトポロジー（スクエア/ヘックス）に応じた距離で隣接判定する
    let topology = world
        .get_resource::<Map>()
        .map(|map| map.topology)
        .unwrap_or(GridTopology::Square);

    let mut query = world.query::<(
        Entity,
        &GridPosition,
        &Faction,
        &UnitStats,
        &Health,
        &ActionCompleted,
        Option<&Transporting>,
    )>();
    for (
        target,
        target_position,
        target_faction,
        target_stats,
        target_health,
        target_action,
        transporting,
    ) in query.iter(world)
    {
        if is_valid_supply_target(
            supplier,
            supplier_faction,
            supplier_position,
            target,
            *target_position,
            target_faction.0,
            target_stats.unit_type,
            *target_health,
            target_action.0,
            transporting.is_some(),
            topology,
        ) {
            targets.push(target);
        }
    }

    targets
}

#[allow(clippy::type_complexity)]
pub fn supply_unit_system(
    mut supply_events: EventReader<SupplyUnitCommand>,
    mut supplied_writer: EventWriter<UnitSuppliedEvent>,
    mut query_units: Query<(
        Entity,
        &GridPosition,
        &Faction,
        &UnitStats,
        &Health,
        &mut ActionCompleted,
        Option<&mut Fuel>,
        Option<&mut Ammo>,
        Option<&Transporting>,
    )>,
    match_state: Res<MatchState>,
    players: Res<Players>,
    mut commands: Commands,
    map: Option<Res<Map>>,
) {
    if match_state.game_over.is_some() || match_state.current_phase != Phase::Main {
        return;
    }
    let active_player_id = players.0[match_state.active_player_index.0].id;
    let topology = map
        .as_ref()
        .map(|map| map.topology)
        .unwrap_or(GridTopology::Square);

    for event in supply_events.read() {
        let (
            supplier_position,
            supplier_faction,
            supplier_can_supply,
            supplier_health,
            supplier_action,
            supplier_is_transporting,
        ) = match query_units.get_mut(event.supplier_entity) {
            Ok((_, position, faction, stats, health, action, _, _, transporting)) => (
                *position,
                faction.0,
                stats.can_supply,
                *health,
                action.0,
                transporting.is_some(),
            ),
            Err(_) => continue,
        };

        if supplier_faction != active_player_id
            || supplier_action
            || supplier_health.is_destroyed()
            || !supplier_can_supply
            || supplier_is_transporting
        {
            continue;
        }

        let (
            target_position,
            target_faction,
            target_unit_type,
            target_health,
            target_action,
            target_is_transporting,
        ) = match query_units.get(event.target_entity) {
            Ok((_, position, faction, stats, health, action, _, _, transporting)) => (
                *position,
                faction.0,
                stats.unit_type,
                *health,
                action.0,
                transporting.is_some(),
            ),
            Err(_) => continue,
        };

        if !is_valid_supply_target(
            event.supplier_entity,
            supplier_faction,
            supplier_position,
            event.target_entity,
            target_position,
            target_faction,
            target_unit_type,
            target_health,
            target_action,
            target_is_transporting,
            topology,
        ) {
            continue;
        }

        // すべての検証を通過してから、補給者と選択対象だけを更新する
        if let Ok([supplier, target]) =
            query_units.get_many_mut([event.supplier_entity, event.target_entity])
        {
            let (_, _, _, _, _, mut supplier_action, _, _, _) = supplier;
            let (_, _, _, target_stats, _, _, target_fuel, target_ammo, _) = target;

            supplier_action.0 = true; // 補給者は行動完了状態になる

            if let Some(mut fuel) = target_fuel {
                fuel.current = target_stats.max_fuel; // 燃料を最大値まで回復
            }

            if let Some(mut ammo) = target_ammo {
                ammo.ammo1 = target_stats.max_ammo1; // 主武器と副武器の弾薬を最大値まで回復
                ammo.ammo2 = target_stats.max_ammo2;
            }

            // 補給完了イベントを送出
            supplied_writer.send(UnitSuppliedEvent {
                supplier: event.supplier_entity,
                target: event.target_entity,
            });

            // 補給確定時に移動履歴を削除
            commands.remove_resource::<PendingMove>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_world() -> World {
        let mut world = World::new();
        world.insert_resource(MatchState {
            current_phase: Phase::Main,
            ..Default::default()
        });
        world.insert_resource(Players(vec![
            Player::new(1, "P1".to_string()),
            Player::new(2, "P2".to_string()),
        ]));
        world.insert_resource(Events::<SupplyUnitCommand>::default());
        world.insert_resource(Events::<UnitSuppliedEvent>::default());
        world
    }

    fn unit_stats(unit_type: UnitType) -> UnitStats {
        UnitStats {
            unit_type,
            max_fuel: 99,
            max_ammo1: 9,
            max_ammo2: 6,
            ..UnitStats::mock()
        }
    }

    fn spawn_supplier(world: &mut World) -> Entity {
        world
            .spawn((
                GridPosition { x: 2, y: 2 },
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    unit_type: UnitType::SupplyTruck,
                    can_supply: true,
                    ..unit_stats(UnitType::SupplyTruck)
                },
                ActionCompleted(false),
                HasMoved(true),
            ))
            .id()
    }

    fn spawn_target(
        world: &mut World,
        unit_type: UnitType,
        faction: PlayerId,
        position: GridPosition,
        action_completed: bool,
    ) -> Entity {
        world
            .spawn((
                position,
                Faction(faction),
                Health {
                    current: 70,
                    max: 100,
                },
                unit_stats(unit_type),
                ActionCompleted(action_completed),
                HasMoved(true),
                Fuel {
                    current: 10,
                    max: 99,
                },
                Ammo {
                    ammo1: 1,
                    max_ammo1: 9,
                    ammo2: 2,
                    max_ammo2: 6,
                },
            ))
            .id()
    }

    fn run_supply_system(world: &mut World) {
        let mut schedule = Schedule::default();
        schedule.add_systems(supply_unit_system);
        schedule.run(world);
    }

    #[test]
    fn test_supply_unit_system_supplies_only_selected_target_and_preserves_target_state() {
        let mut world = setup_world();
        let supplier = spawn_supplier(&mut world);
        let selected_target = spawn_target(
            &mut world,
            UnitType::Infantry,
            PlayerId(1),
            GridPosition { x: 3, y: 2 },
            false,
        );
        let unselected_target = spawn_target(
            &mut world,
            UnitType::Fighter,
            PlayerId(1),
            GridPosition { x: 2, y: 3 },
            false,
        );
        world.insert_resource(PendingMove {
            unit_entity: supplier,
            original_pos: GridPosition { x: 2, y: 1 },
            original_fuel: Fuel {
                current: 20,
                max: 99,
            },
        });
        world.send_event(SupplyUnitCommand {
            supplier_entity: supplier,
            target_entity: selected_target,
        });

        run_supply_system(&mut world);

        assert!(world.get::<ActionCompleted>(supplier).unwrap().0);
        assert_eq!(world.get::<Fuel>(selected_target).unwrap().current, 99);
        let selected_ammo = world.get::<Ammo>(selected_target).unwrap();
        assert_eq!(selected_ammo.ammo1, 9);
        assert_eq!(selected_ammo.ammo2, 6);
        assert_eq!(world.get::<Health>(selected_target).unwrap().current, 70);
        assert!(!world.get::<ActionCompleted>(selected_target).unwrap().0);
        assert!(world.get::<HasMoved>(selected_target).unwrap().0);
        assert_eq!(world.get::<Fuel>(unselected_target).unwrap().current, 10);
        assert_eq!(world.get::<Ammo>(unselected_target).unwrap().ammo1, 1);
        assert!(world.get_resource::<PendingMove>().is_none());
    }

    #[test]
    fn test_supply_unit_system_rejects_invalid_direct_commands() {
        let mut world = setup_world();
        let supplier = spawn_supplier(&mut world);
        let acted_target = spawn_target(
            &mut world,
            UnitType::Infantry,
            PlayerId(1),
            GridPosition { x: 3, y: 2 },
            true,
        );
        let ineligible_target = spawn_target(
            &mut world,
            UnitType::HeavyFighter,
            PlayerId(1),
            GridPosition { x: 2, y: 3 },
            false,
        );
        world.insert_resource(PendingMove {
            unit_entity: supplier,
            original_pos: GridPosition { x: 2, y: 1 },
            original_fuel: Fuel {
                current: 20,
                max: 99,
            },
        });
        world.send_event(SupplyUnitCommand {
            supplier_entity: supplier,
            target_entity: acted_target,
        });
        world.send_event(SupplyUnitCommand {
            supplier_entity: supplier,
            target_entity: ineligible_target,
        });

        run_supply_system(&mut world);

        assert!(!world.get::<ActionCompleted>(supplier).unwrap().0);
        assert_eq!(world.get::<Fuel>(acted_target).unwrap().current, 10);
        assert_eq!(world.get::<Fuel>(ineligible_target).unwrap().current, 10);
        assert!(world.get_resource::<PendingMove>().is_some());
    }

    #[test]
    fn test_get_suppliable_targets_filters_by_issue_70_eligibility() {
        let mut world = setup_world();
        let supplier = spawn_supplier(&mut world);
        let allowed_types = [
            UnitType::Infantry,
            UnitType::Mech,
            UnitType::Recon,
            UnitType::Tank,
            UnitType::MdTank,
            UnitType::TankZ,
            UnitType::Artillery,
            UnitType::LightSpGun,
            UnitType::HeavySpGun,
            UnitType::Rockets,
            UnitType::AntiAir,
            UnitType::Missiles,
            UnitType::SupplyTruck,
            UnitType::Fighter,
        ];
        let allowed_targets: Vec<_> = allowed_types
            .into_iter()
            .map(|unit_type| {
                spawn_target(
                    &mut world,
                    unit_type,
                    PlayerId(1),
                    GridPosition { x: 3, y: 2 },
                    false,
                )
            })
            .collect();
        for unit_type in [
            UnitType::HeavyFighter,
            UnitType::Bomber,
            UnitType::Bcopters,
            UnitType::TransportHelicopter,
            UnitType::Battleship,
            UnitType::Carrier,
            UnitType::Lander,
        ] {
            spawn_target(
                &mut world,
                unit_type,
                PlayerId(1),
                GridPosition { x: 3, y: 2 },
                false,
            );
        }
        let _acted = spawn_target(
            &mut world,
            UnitType::Infantry,
            PlayerId(1),
            GridPosition { x: 3, y: 2 },
            true,
        );
        let _enemy = spawn_target(
            &mut world,
            UnitType::Infantry,
            PlayerId(2),
            GridPosition { x: 3, y: 2 },
            false,
        );
        let dead = spawn_target(
            &mut world,
            UnitType::Infantry,
            PlayerId(1),
            GridPosition { x: 3, y: 2 },
            false,
        );
        world.get_mut::<Health>(dead).unwrap().current = 0;
        let transported = spawn_target(
            &mut world,
            UnitType::Infantry,
            PlayerId(1),
            GridPosition { x: 3, y: 2 },
            false,
        );
        world.entity_mut(transported).insert(Transporting(supplier));
        let _far = spawn_target(
            &mut world,
            UnitType::Infantry,
            PlayerId(1),
            GridPosition { x: 4, y: 2 },
            false,
        );

        let targets = get_suppliable_targets(&mut world, supplier);

        assert_eq!(targets.len(), allowed_targets.len());
        for target in allowed_targets {
            assert!(targets.contains(&target));
        }
    }

    /// ヘックスモードでは斜め方向の隣接ユニットにも補給できることの確認
    #[test]
    fn test_get_suppliable_targets_hex() {
        let mut world = setup_world();
        world.insert_resource(Map::new(10, 10, Terrain::Plains, GridTopology::Hex));
        let supplier = spawn_supplier(&mut world);
        world.get_mut::<GridPosition>(supplier).unwrap().x = 5;
        world.get_mut::<GridPosition>(supplier).unwrap().y = 5;
        let target_diag = spawn_target(
            &mut world,
            UnitType::Infantry,
            PlayerId(1),
            GridPosition { x: 6, y: 6 },
            false,
        );
        let _target_not_adjacent = spawn_target(
            &mut world,
            UnitType::Infantry,
            PlayerId(1),
            GridPosition { x: 4, y: 4 },
            false,
        );

        let targets = get_suppliable_targets(&mut world, supplier);

        assert_eq!(targets, vec![target_diag]);
    }
}
