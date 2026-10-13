#![allow(
    clippy::collapsible_if,
    clippy::unnecessary_min_or_max,
    clippy::unnecessary_map_or
)]
mod tests {
    use crate::ai::execute_ai_turn;
    use std::collections::VecDeque;

    use crate::ai::command::AiCommand;
    use crate::ai::context::tactical::*;
    use crate::ai::production::turn_state::ProductionTurnState;
    use crate::ai::runtime::state::AiTurnProgress;
    use crate::ai::runtime::{production_queue::*, state::AiActionCooldown};
    use crate::ai::tactics::campaign::campaign_mission_allows_capture;
    use crate::ai::tactics::distance_cache::ActionDistanceCache;
    use crate::ai::tactics::{
        candidates::air_move_preserves_return_fuel, relief::decide_forming_campaign_site_relief,
        v2::decide_action as decide_ai_action_v2,
    };
    use crate::ai::v1::decide_ai_action;
    use crate::collections::{HashMap, HashSet};
    use crate::components::{
        ActionCompleted, Faction, GridPosition, HasMoved, Health, PlayerId, Property, UnitStats,
    };
    use crate::resources::master_data::MasterDataRegistry;
    use crate::resources::{Map, Terrain, UnitType};
    use bevy_ecs::prelude::*;

    use crate::ai::turn_distance::calculate_turn_distance;
    use crate::resources::DamageChart;

    #[test]
    fn queued_production_keeps_later_commands_for_the_fast_path() {
        let mut world = World::new();
        let player = PlayerId(1);
        let first = crate::events::ProduceUnitCommand {
            player_id: player,
            target_x: 1,
            target_y: 2,
            unit_type: UnitType::Infantry,
        };
        let second = crate::events::ProduceUnitCommand {
            player_id: player,
            target_x: 3,
            target_y: 4,
            unit_type: UnitType::Bcopters,
        };
        world.insert_resource(AiProductionCommandQueue {
            player_id: Some(player),
            turn: 7,
            commands: VecDeque::from([first, second]),
        });

        let selected = take_next_queued_production_command(&mut world, player, 7)
            .expect("先頭の生産命令を取得する");
        assert_eq!((selected.target_x, selected.target_y), (1, 2));
        let remaining = world.resource::<AiProductionCommandQueue>();
        assert_eq!(remaining.commands.len(), 1);
        assert_eq!(
            remaining
                .commands
                .front()
                .map(|command| (command.target_x, command.target_y)),
            Some((3, 4))
        );
    }

    #[test]
    fn action_distance_cache_survives_friendly_actions_but_clears_on_enemy_movement() {
        let player = PlayerId(1);
        let map = Map::new(
            4,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut strategy_cache = ActionDistanceCache::default();
        let blockers = vec![(3, 0)];
        let mut distance_cache = strategy_cache.take_action_distance_cache(player, &blockers);
        let _ = calculate_turn_distance(
            &map,
            &registry,
            &HashMap::default(),
            (0, 0),
            (3, 0),
            crate::resources::MovementType::Infantry,
            3,
            1,
            player,
            &mut distance_cache,
        );
        assert!(!distance_cache.cache.is_empty());
        strategy_cache.put_action_distance_cache(distance_cache);

        let reused = strategy_cache.take_action_distance_cache(player, &blockers);
        assert!(!reused.cache.is_empty());
        strategy_cache.put_action_distance_cache(reused);

        let cleared = strategy_cache.take_action_distance_cache(player, &[(2, 0)]);
        assert!(cleared.cache.is_empty());
    }

    #[test]
    fn drop_keeps_transport_actionable_until_last_ready_cargo() {
        let mut world = World::new();
        let first = world.spawn(crate::components::ActionCompleted(false)).id();
        let second = world.spawn(crate::components::ActionCompleted(false)).id();
        let transport = world
            .spawn(crate::components::CargoCapacity {
                max: 2,
                loaded: vec![first, second],
            })
            .id();

        assert!(transport_has_other_actionable_cargo(
            &world, transport, first
        ));
        world
            .get_mut::<crate::components::ActionCompleted>(second)
            .unwrap()
            .0 = true;
        assert!(!transport_has_other_actionable_cargo(
            &world, transport, first
        ));
    }

    #[test]
    fn strategic_target_value_includes_cargo_and_immediate_capture_risk() {
        let mut world = World::new();
        let first_cargo = world.spawn_empty().id();
        let second_cargo = world.spawn_empty().id();
        let unit_costs = HashMap::from_iter([(first_cargo, 1000), (second_cargo, 3000)]);
        let cargo = crate::components::CargoCapacity {
            max: 2,
            loaded: vec![first_cargo, second_cargo],
        };
        let registry = MasterDataRegistry::load().unwrap();
        let enemy = PlayerId(2);
        let position = GridPosition { x: 2, y: 3 };
        let properties = vec![(position, Terrain::City, Some(PlayerId(1)))];

        let transport = UnitStats {
            cost: 4000,
            ..UnitStats::mock()
        };
        assert_eq!(
            strategic_target_value(
                &transport,
                position,
                enemy,
                Some(&cargo),
                &unit_costs,
                &properties,
                &registry,
            ),
            8000
        );

        let occupier = UnitStats {
            cost: 1000,
            can_capture: true,
            ..UnitStats::mock()
        };
        assert_eq!(
            strategic_target_value(
                &occupier,
                position,
                enemy,
                None,
                &unit_costs,
                &properties,
                &registry,
            ),
            1000 + registry.landscape_income(Terrain::City.as_str())
        );

        let owned_properties = vec![(position, Terrain::City, Some(enemy))];
        assert_eq!(
            strategic_target_value(
                &occupier,
                position,
                enemy,
                None,
                &unit_costs,
                &owned_properties,
                &registry,
            ),
            1000
        );
    }

    #[test]
    fn forming_campaign_transport_vacates_owned_production_site() {
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(3, crate::ai::ai_version::AiVersion::V3);
        world.insert_resource(Map {
            width: 3,
            height: 2,
            tiles: vec![
                Terrain::Capital,
                Terrain::Airport,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Airport, Some(player), 100),
        ));
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::TransportHelicopter.as_str().to_owned(),
            ))
            .unwrap();
        let transport = world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 0 },
                stats.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: stats.max_fuel,
                    max: stats.max_fuel,
                },
                crate::components::CargoCapacity {
                    max: stats.max_cargo,
                    loaded: Vec::new(),
                },
            ))
            .id();
        let mut manager = crate::ai::squad::SquadManager::new();
        let squad = manager.create_owned_squad(crate::ai::squad::MissionType::Transport, player);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.target_island = Some(crate::ai::islands::IslandId(1));
        squad.phase = crate::ai::squad::MissionPhase::Forming;
        world.insert_resource(manager);

        let (entity, command) =
            decide_forming_campaign_site_relief(&mut world, player, &HashSet::default(), false)
                .expect("Forming transport must vacate the airport while waiting");
        assert_eq!(entity, transport);
        let AiCommand::Wait { target_pos } = command else {
            panic!("production site relief must issue a movement wait");
        };
        assert_ne!(target_pos, GridPosition { x: 1, y: 0 });
        assert!(!matches!(
            world
                .resource::<Map>()
                .get_terrain(target_pos.x, target_pos.y),
            Some(Terrain::Capital | Terrain::Airport)
        ));
    }

    #[test]
    fn unassigned_empty_transport_vacates_owned_production_site() {
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(3, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 3,
            height: 2,
            tiles: vec![
                Terrain::Capital,
                Terrain::Airport,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Airport, Some(player), 100),
        ));
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::TransportHelicopter.as_str().to_owned(),
            ))
            .unwrap();
        let transport = world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 0 },
                stats.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: stats.max_fuel,
                    max: stats.max_fuel,
                },
                crate::components::CargoCapacity {
                    max: stats.max_cargo,
                    loaded: Vec::new(),
                },
            ))
            .id();
        world.insert_resource(crate::ai::squad::SquadManager::new());

        let (entity, command) =
            decide_forming_campaign_site_relief(&mut world, player, &HashSet::default(), false)
                .expect("未割当の空輸送役も空港を退避する");

        assert_eq!(entity, transport);
        let AiCommand::Wait { target_pos } = command else {
            panic!("production site relief must issue a movement wait");
        };
        assert_ne!(target_pos, GridPosition { x: 1, y: 0 });
    }

    /// 工場(1,0)上で停滞したAttack任務の歩兵と、任務目標(3,0)を持つ盤面を作る。
    /// `neutral_city_at_target` がtrueなら、目標(3,0)に中立都市を置く。
    fn stalled_attack_world(neutral_city_at_target: bool) -> (World, PlayerId, Entity) {
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(3, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 4,
            height: 2,
            tiles: vec![
                Terrain::Capital,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(player), 100),
        ));
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        let attacker = world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 0 },
                stats.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: stats.max_fuel,
                    max: stats.max_fuel,
                },
            ))
            .id();
        let mut manager = crate::ai::squad::SquadManager::new();
        let squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        squad.members.insert(attacker);
        squad.target = Some(GridPosition { x: 3, y: 0 });
        squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        world.insert_resource(manager);
        if neutral_city_at_target {
            world.spawn((
                GridPosition { x: 3, y: 0 },
                Property::new(Terrain::City, None, 100),
            ));
        }
        (world, player, attacker)
    }

    #[test]
    fn stalled_attack_unit_vacates_factory_toward_its_mission() {
        let (mut world, player, attacker) = stalled_attack_world(false);

        assert!(
            decide_forming_campaign_site_relief(&mut world, player, &HashSet::default(), false)
                .is_none(),
            "通常作戦より前の退避処理はAttack任務を奪わない"
        );
        let (entity, command) =
            decide_forming_campaign_site_relief(&mut world, player, &HashSet::default(), true)
                .expect("通常行動に失敗したAttack unitは工場を退避する");

        assert_eq!(entity, attacker);
        let AiCommand::Wait { target_pos } = command else {
            panic!("生産施設からの退避は移動Waitであること");
        };
        assert_eq!(target_pos, GridPosition { x: 3, y: 0 });
    }

    #[test]
    fn factory_relief_starts_capture_only_on_extended_path() {
        // 退避先(3,0)が中立都市のとき、通常経路は移動Waitのまま、
        // 拡張経路は退避と同時に占領を始める（F01の経路差を固定する）。
        for (path, expects_capture) in [
            (crate::ai::code_path::CodePath::MainCompatible, false),
            (crate::ai::code_path::CodePath::Extended, true),
        ] {
            let _guard = crate::ai::code_path::enter(path);
            let (mut world, player, attacker) = stalled_attack_world(true);
            let (entity, command) =
                decide_forming_campaign_site_relief(&mut world, player, &HashSet::default(), true)
                    .expect("停滞したAttack unitは工場を退避する");

            assert_eq!(entity, attacker);
            let target_pos = GridPosition { x: 3, y: 0 };
            let is_expected = if expects_capture {
                matches!(command, AiCommand::Capture { target_pos: pos } if pos == target_pos)
            } else {
                matches!(command, AiCommand::Wait { target_pos: pos } if pos == target_pos)
            };
            assert!(is_expected, "{path:?}: {command:?}");
        }
    }

    #[test]
    fn campaign_attack_units_may_capture_but_defense_and_transport_may_not() {
        use crate::ai::squad::MissionType;

        assert!(campaign_mission_allows_capture(&MissionType::Capture));
        assert!(campaign_mission_allows_capture(&MissionType::Attack));
        assert!(!campaign_mission_allows_capture(&MissionType::Defense));
        assert!(!campaign_mission_allows_capture(&MissionType::Transport));
    }

    #[test]
    fn ready_combat_aircraft_does_not_wait_on_owned_airport() {
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(3, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 3,
            height: 1,
            tiles: vec![Terrain::Plains, Terrain::Airport, Terrain::Plains],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Airport, Some(player), 100),
        ));
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Bcopters.as_str().to_owned(),
            ))
            .unwrap();
        let aircraft = world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 0 },
                stats.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: stats.max_ammo1,
                    max_ammo1: stats.max_ammo1,
                    ammo2: stats.max_ammo2,
                    max_ammo2: stats.max_ammo2,
                },
                crate::components::Fuel {
                    current: stats.max_fuel,
                    max: stats.max_fuel,
                },
            ))
            .id();

        let (entity, command) =
            decide_ai_action_v2(&mut world, player, &HashSet::default()).expect("行動を選ぶこと");
        assert_eq!(entity, aircraft);
        let AiCommand::Wait { target_pos } = command else {
            panic!("敵がいないため移動Waitを選ぶこと");
        };
        assert_ne!(target_pos, GridPosition { x: 1, y: 0 });
    }

    #[test]
    fn v3_turn_cache_marks_squad_plan_until_cleared() {
        let player = PlayerId(1);
        let mut cache = AiTurnProgress::default();

        assert!(!cache.squads_planned(player));
        cache.mark_squads_planned(player);
        assert!(cache.squads_planned(player));

        cache.clear();
        assert!(!cache.squads_planned(player));
    }

    #[test]
    fn plan_squads_populates_v3_turn_strategy_cache() {
        let master_data = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }
        let player = PlayerId(1);
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(player, crate::ai::AiVersion::V3);
        world.insert_resource(settings);

        crate::ai::squad::plan_squads(&mut world, player);

        assert!(world.resource::<AiTurnProgress>().squads_planned(player));
        assert!(
            world
                .resource::<crate::ai::planning::decision::AdoptedTurnPortfolio>()
                .campaign_portfolio(player)
                .is_some()
        );
    }

    #[test]
    fn v3_campaign_production_plan_consumes_each_command_once() {
        let player = PlayerId(1);
        let first = crate::events::ProduceUnitCommand {
            player_id: player,
            target_x: 1,
            target_y: 2,
            unit_type: UnitType::Infantry,
        };
        let second = crate::events::ProduceUnitCommand {
            player_id: player,
            target_x: 3,
            target_y: 4,
            unit_type: UnitType::Mech,
        };
        let mut cache = ProductionTurnState::default();

        cache.set_campaign_production_plan(player, 0, vec![first, second], true);

        assert!(cache.campaign_production_planned(player));
        let actual_first = cache.take_campaign_production_command(player).unwrap();
        let actual_second = cache.take_campaign_production_command(player).unwrap();
        assert_eq!(
            (
                actual_first.target_x,
                actual_first.target_y,
                actual_first.unit_type,
            ),
            (1, 2, UnitType::Infantry)
        );
        assert_eq!(
            (
                actual_second.target_x,
                actual_second.target_y,
                actual_second.unit_type,
            ),
            (3, 4, UnitType::Mech)
        );
        assert!(cache.take_campaign_production_command(player).is_none());
        assert!(!cache.campaign_production_blocks_generic(player));
    }

    #[test]
    fn incomplete_v3_campaign_production_plan_blocks_generic_fallback() {
        let player = PlayerId(1);
        let mut cache = ProductionTurnState::default();

        cache.set_campaign_production_plan(player, 0, Vec::new(), false);

        assert!(cache.campaign_production_planned(player));
        assert!(cache.campaign_production_blocks_generic(player));
    }

    #[test]
    fn test_decide_ai_action_no_units() {
        let mut world = World::new();
        let skips = crate::collections::HashSet::default();
        assert!(decide_ai_action(&mut world, PlayerId(1), &skips).is_none());
    }

    #[test]
    fn air_movement_reserves_distance_and_daily_fuel_for_return() {
        let map = Map::new(
            20,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let player = PlayerId(1);
        let properties = vec![(GridPosition { x: 0, y: 0 }, Terrain::Airport, Some(player))];
        let stats = UnitStats {
            movement_type: crate::resources::MovementType::Air,
            max_movement: 8,
            daily_fuel_consumption: 5,
            ..UnitStats::mock()
        };

        assert!(air_move_preserves_return_fuel(
            &map,
            &properties,
            player,
            &stats,
            GridPosition { x: 8, y: 0 },
            GridPosition { x: 7, y: 0 },
            20,
        ));
        assert!(!air_move_preserves_return_fuel(
            &map,
            &properties,
            player,
            &stats,
            GridPosition { x: 8, y: 0 },
            GridPosition { x: 10, y: 0 },
            20,
        ));
        assert!(air_move_preserves_return_fuel(
            &map,
            &properties,
            player,
            &stats,
            GridPosition { x: 1, y: 0 },
            GridPosition { x: 0, y: 0 },
            1,
        ));

        // 既に安全圏外でも、帰還不足を増やさず空港へ近づく一歩は拒否しない。
        assert!(air_move_preserves_return_fuel(
            &map,
            &properties,
            player,
            &stats,
            GridPosition { x: 8, y: 0 },
            GridPosition { x: 7, y: 0 },
            10,
        ));
        assert!(!air_move_preserves_return_fuel(
            &map,
            &properties,
            player,
            &stats,
            GridPosition { x: 8, y: 0 },
            GridPosition { x: 9, y: 0 },
            10,
        ));
    }

    #[test]
    fn test_decide_ai_action_wait() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(Map {
            width: 5,
            height: 5,
            tiles: vec![Terrain::Plains; 25],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        world.spawn((
            PlayerId(1),
            Faction(PlayerId(1)),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 0, y: 0 },
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 1000,
                max_movement: 3,
                movement_type: crate::resources::MovementType::Tank,
                max_fuel: 99,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
            crate::components::Fuel {
                current: 99,
                max: 99,
            },
        ));

        // Since there is no enemy to attack and no property to capture, it should return Wait.
        let skips = crate::collections::HashSet::default();
        let action = decide_ai_action(&mut world, PlayerId(1), &skips);
        assert!(action.is_some());
        if let Some((_, AiCommand::Wait { .. })) = action {
        } else {
            panic!("Expected Wait command, got {:?}", action);
        }
    }

    #[test]
    fn test_decide_ai_action_attack() {
        let mut world = World::new();
        let mut dc = DamageChart::new();
        dc.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        dc.insert_damage(UnitType::Infantry, UnitType::Tank, 1); // Ensure not suicidal
        world.insert_resource(dc);
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        let attacker = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    ammo1_cost: 0,
                    ammo2_cost: 0,
                    unit_type: UnitType::Tank,
                    cost: 7000,
                    min_range: 1,
                    max_range: 1,
                    max_ammo1: 10,
                    max_ammo2: 10,
                    movement_type: crate::resources::MovementType::Tank,
                    max_movement: 6,
                    max_fuel: 99,
                    daily_fuel_consumption: 0,
                    can_capture: false,
                    can_supply: false,
                    max_cargo: 0,
                    loadable_unit_types: vec![],
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: 10,
                    max_ammo1: 10,
                    ammo2: 10,
                    max_ammo2: 10,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 1, y: 2 }, // adjacent
            UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let skips = crate::collections::HashSet::default();
        let action = decide_ai_action(&mut world, p1, &skips);
        assert!(action.is_some());
        if let Some((entity, AiCommand::Attack { target_entity, .. })) = action {
            assert_eq!(entity, attacker);
            // target_entity is the spawned defender
            let defender_faction = world.get::<Faction>(target_entity).unwrap();
            assert_eq!(defender_faction.0, p2);
        } else {
            panic!("Expected Attack command, got {:?}", action);
        }
    }

    #[test]
    fn test_decide_ai_action_capture() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);

        let unit = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    can_capture: true,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        // Neutral property on the same tile
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::City, None, 200),
        ));

        let p1 = PlayerId(1);
        let skips = crate::collections::HashSet::default();
        let action = decide_ai_action(&mut world, p1, &skips);
        assert!(action.is_some());
        if let Some((entity, AiCommand::Capture { .. })) = action {
            assert_eq!(entity, unit);
        } else {
            panic!("Expected Capture command, got {:?}", action);
        }
    }

    #[test]
    fn test_decide_ai_action_indirect_range() {
        let mut world = World::new();
        let mut dc = DamageChart::new();
        // Artillery vs Tank
        dc.insert_damage(UnitType::Artillery, UnitType::Tank, 50);
        world.insert_resource(dc);
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // Artillery at (0,0), can move 5 tiles.
        // Max range 3, Min range 2.
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 0, y: 0 },
            UnitStats {
                unit_type: UnitType::Artillery,
                cost: 6000,
                max_movement: 5,
                movement_type: crate::resources::MovementType::Artillery,
                min_range: 2,
                max_range: 3,
                max_fuel: 99,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
            crate::components::Fuel {
                current: 99,
                max: 99,
            },
            crate::components::Ammo {
                ammo1: 10,
                max_ammo1: 10,
                ammo2: 0,
                max_ammo2: 0,
            },
        ));

        // Tank at (7,0). Distance is 7.
        // Artillery can move to (4,0) [dist 3], (5,0) [dist 2].
        // It should prefer (4,0) because it's max_range (3).
        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 7, y: 0 },
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 7000,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let skips = crate::collections::HashSet::default();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        if let Some((_, AiCommand::Wait { target_pos, .. })) = action {
            // Should be at distance 3 from (7,0) -> x=4, y=0
            assert_eq!(target_pos.x, 4);
            assert_eq!(target_pos.y, 0);
        } else {
            panic!("Expected Wait command at distance 3, got {:?}", action);
        }
    }

    #[test]
    fn test_decide_ai_action_indirect_escape() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // Artillery at (4,0), adjacent to Tank at (5,0).
        // Cannot attack from (4,0) because min_range is 2.
        // Should move away to at least distance 2.
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 4, y: 0 },
            UnitStats {
                unit_type: UnitType::Artillery,
                cost: 6000,
                max_movement: 5,
                movement_type: crate::resources::MovementType::Artillery,
                min_range: 2,
                max_range: 3,
                max_fuel: 99,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
            crate::components::Fuel {
                current: 99,
                max: 99,
            },
            crate::components::Ammo {
                ammo1: 10,
                max_ammo1: 10,
                ammo2: 0,
                max_ammo2: 0,
            },
        ));

        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 5, y: 0 },
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 7000,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let skips = crate::collections::HashSet::default();
        let action = decide_ai_action(&mut world, p1, &skips);

        let (_, cmd) = action.expect("some action must be chosen");
        let target_pos = match cmd {
            AiCommand::Wait { target_pos } => target_pos,
            other => panic!("Expected Wait command, got {:?}", other),
        };

        // Distance to (5,0) should be >= 2. (4,0) is dist 1.
        let dist = (target_pos.x as i32 - 5).abs() + (target_pos.y as i32).abs();
        assert!(
            dist >= 2,
            "Artillery should move away from adjacency, got pos {:?} (dist {})",
            target_pos,
            dist
        );
    }

    #[test]
    fn test_decide_ai_action_avoid_kamikaze() {
        let mut world = World::new();
        let mut dc = DamageChart::new();
        // Infantry vs Tank: 1% damage
        dc.insert_damage(UnitType::Infantry, UnitType::Tank, 1);
        // Tank vs Infantry: 90% damage
        dc.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        world.insert_resource(dc);
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // Infantry (P1) at (1,1)
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 1, y: 1 },
            UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                min_range: 1,
                max_range: 1,
                max_movement: 3,
                movement_type: crate::resources::MovementType::Infantry,
                max_fuel: 99,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
            crate::components::Fuel {
                current: 99,
                max: 99,
            },
            crate::components::Ammo {
                ammo1: 10,
                max_ammo1: 10,
                ammo2: 10,
                max_ammo2: 10,
            },
        ));

        // Tank (P2) at (1,2)
        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 1, y: 2 },
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 7000,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let skips = crate::collections::HashSet::default();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        if let Some((_, AiCommand::Attack { .. })) = action {
            panic!("AI should not perform a suicidal attack (Infantry vs Tank)");
        }
    }

    #[test]
    fn test_decide_ai_action_load() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        world.spawn((
            GridPosition { x: 9, y: 9 },
            Property {
                terrain: Terrain::City,
                owner_id: Some(p2),
                capture_points: 20,
                max_capture_points: 20,
            },
        ));

        let _inf = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    max_movement: 3,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        let _transport = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
                crate::components::CargoCapacity {
                    max: 2,
                    loaded: vec![],
                },
            ))
            .id();

        let skips = crate::collections::HashSet::default();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        let (_ent, cmd) = action.unwrap();
        if let AiCommand::Load { .. } = cmd {
            panic!("Expected Load command to be completely removed from normal decision making")
        }
    }

    #[test]
    fn test_decide_ai_action_drop() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        world.spawn((
            GridPosition { x: 1, y: 2 },
            Property {
                terrain: Terrain::City,
                owner_id: Some(p2),
                capture_points: 20,
                max_capture_points: 20,
            },
        ));

        let inf = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(true),
                ActionCompleted(false),
                GridPosition { x: 999, y: 999 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: crate::resources::MovementType::Infantry,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Transporting(Entity::from_raw(0)),
            ))
            .id();

        let transport = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    max_cargo: 2,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
                crate::components::CargoCapacity {
                    max: 2,
                    loaded: vec![inf],
                },
            ))
            .id();

        world
            .entity_mut(inf)
            .insert(crate::components::Transporting(transport));

        let skips = crate::collections::HashSet::default();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        let (_ent, cmd) = action.unwrap();
        if let AiCommand::Drop { .. } = cmd {
            panic!("Expected Drop command to be completely removed from normal decision making")
        }
    }

    #[test]
    fn test_decide_ai_action_retreat_low_hp() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(Map {
            width: 5,
            height: 5,
            tiles: vec![Terrain::Plains; 25],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        // 都市を(1,1)に設置
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::City, Some(p1), 200),
        ));

        // 低HP(30)の戦車を(1,0)に配置
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 1, y: 0 },
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 7000,
                max_movement: 3,
                movement_type: crate::resources::MovementType::Tank,
                max_fuel: 99,
                ..UnitStats::mock()
            },
            Health {
                current: 30,
                max: 100,
            },
            crate::components::Fuel {
                current: 99,
                max: 99,
            },
        ));

        let skips = crate::collections::HashSet::default();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        if let Some((_, AiCommand::Wait { target_pos })) = action {
            // (1,1)の都市へ移動して待機することを確認
            assert_eq!(target_pos.x, 1);
            assert_eq!(target_pos.y, 1);
        } else {
            panic!("Expected Wait at (1,1), got {:?}", action);
        }
    }

    #[test]
    fn test_decide_ai_action_merge() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(Map {
            width: 5,
            height: 5,
            tiles: vec![Terrain::Plains; 25],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);

        // 低HP(50)の歩兵Aを(0,0)に配置
        let unit_a = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
                    ..UnitStats::mock()
                },
                Health {
                    current: 50,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        // 低HP(40)の歩兵Bを(1,0)に配置
        let unit_b = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
                    ..UnitStats::mock()
                },
                Health {
                    current: 40,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        let skips = crate::collections::HashSet::default();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        // 歩兵Aが歩兵Bの位置(1,0)へ移動してMergeすることを確認
        if let Some((
            entity,
            AiCommand::Merge {
                target_pos,
                target_entity,
            },
        )) = action
        {
            assert_eq!(entity, unit_a);
            assert_eq!(target_pos.x, 1);
            assert_eq!(target_pos.y, 0);
            assert_eq!(target_entity, unit_b);
        } else {
            panic!("Expected Merge command, got {:?}", action);
        }
    }

    #[test]
    fn issue73_v1_overflow_merge_is_not_selected() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(Map {
            width: 3,
            height: 1,
            tiles: vec![Terrain::Plains, Terrain::Forest, Terrain::Plains],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|master_data| world.insert_resource(master_data))
            .unwrap();

        let player = PlayerId(1);
        world.spawn((
            player,
            Faction(player),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 0, y: 0 },
            UnitStats {
                unit_type: UnitType::Infantry,
                max_movement: 3,
                movement_type: crate::resources::MovementType::Infantry,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
            crate::components::Fuel {
                current: 99,
                max: 99,
            },
        ));
        let target = world
            .spawn((
                player,
                Faction(player),
                HasMoved(true),
                ActionCompleted(true),
                GridPosition { x: 1, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
                    ..UnitStats::mock()
                },
                Health {
                    current: 34,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        let action = decide_ai_action(&mut world, player, &crate::collections::HashSet::default());

        assert!(
            !matches!(
                action,
                Some((_, AiCommand::Merge { target_entity, .. })) if target_entity == target
            ),
            "HP上限を超えるMergeはV1の候補から除外されること"
        );
    }

    #[test]
    fn issue73_v3_position_score_does_not_revive_overflow_merge() {
        let mut world = setup_v3_test_world(3, crate::ai::AiVersion::V3);
        world.insert_resource(DamageChart::new());
        let player = PlayerId(1);
        let stats = UnitStats {
            unit_type: UnitType::Infantry,
            max_movement: 3,
            movement_type: crate::resources::MovementType::Infantry,
            ..UnitStats::mock()
        };
        let source = spawn_v3_test_unit(&mut world, player, 0, 100, stats.clone());
        let target = world
            .spawn((
                Faction(player),
                HasMoved(true),
                ActionCompleted(true),
                GridPosition { x: 1, y: 0 },
                stats,
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();
        insert_single_unit_squad(&mut world, source, GridPosition { x: 1, y: 0 });

        let action =
            decide_ai_action_v2(&mut world, player, &crate::collections::HashSet::default());

        assert!(
            !matches!(
                action,
                Some((entity, AiCommand::Merge { target_entity, .. }))
                    if entity == source && target_entity == target
            ),
            "部隊目標による大きな位置スコアがあってもHP超過Mergeを復活させないこと"
        );
    }

    #[test]
    fn test_decide_ai_action_retreat_no_ammo() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(Map {
            width: 5,
            height: 5,
            tiles: vec![Terrain::Plains; 25],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        // 都市を(1,1)に設置
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::City, Some(p1), 200),
        ));

        // 弾薬切れ(0)の戦車を(1,0)に配置
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 1, y: 0 },
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 7000,
                max_movement: 3,
                movement_type: crate::resources::MovementType::Tank,
                max_fuel: 99,
                max_ammo1: 5, // 主武装あり
                ..UnitStats::mock()
            },
            Health {
                current: 100, // HPは満タン
                max: 100,
            },
            crate::components::Ammo {
                ammo1: 0, // 弾薬切れ
                max_ammo1: 5,
                ammo2: 99,
                max_ammo2: 99,
            },
            crate::components::Fuel {
                current: 99,
                max: 99,
            },
        ));

        let skips = crate::collections::HashSet::default();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        if let Some((_, AiCommand::Wait { target_pos })) = action {
            // (1,1)の都市へ移動して待機することを確認
            assert_eq!(target_pos.x, 1);
            assert_eq!(target_pos.y, 1);
        } else {
            panic!("Expected Wait at (1,1) due to no ammo, got {:?}", action);
        }
    }

    #[test]
    fn test_ai_action_taxi_back() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let p1 = PlayerId(1);

        // 1. 全ユニットをクリア
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }

        // 2. 首都（生産拠点）を設置 (x=0, y=0)
        let capital_pos = GridPosition { x: 0, y: 0 };
        world.spawn((capital_pos, Property::new(Terrain::Capital, Some(p1), 100)));

        // 3. 空の輸送ヘリを「前線（遠く）」に設置 (x=8, y=0)
        let heli_pos = GridPosition { x: 8, y: 0 };
        let heli_entity = world
            .spawn((
                heli_pos,
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    max_movement: 6,
                    movement_type: crate::resources::MovementType::Air,
                    max_cargo: 1,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
                crate::components::CargoCapacity {
                    loaded: vec![],
                    max: 1,
                },
            ))
            .id();

        // 4. AIに行動を決定させる
        let skips = crate::collections::HashSet::default();
        let action = decide_ai_action(&mut world, p1, &skips);

        // 5. 検証: 輸送ヘリが首都（x=0）の方向に移動しようとしていること
        assert!(action.is_some());
        if let Some((entity, AiCommand::Wait { target_pos })) = action {
            assert_eq!(entity, heli_entity);
            assert!(
                target_pos.x < heli_pos.x,
                "Empty transport should move back towards capital (x=0). Target: {:?}, Current: {:?}",
                target_pos,
                heli_pos
            );
        } else {
            panic!("Expected Wait command for taxi-back, got {:?}", action);
        }
    }

    #[test]
    fn test_is_unit_stranded_coast_attraction() {
        let mut world = World::new();
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Infantry, 55);
        world.insert_resource(damage_chart);

        // 5x5のマップ。左上の3x3が陸地、それ以外は海
        // (0,0) ~ (2,2) は Plains、それ以外は Sea
        let mut tiles = vec![Terrain::Sea; 25];
        for y in 0..3 {
            for x in 0..3 {
                tiles[y * 5 + x] = Terrain::Plains;
            }
        }

        let map = Map {
            width: 5,
            height: 5,
            tiles,
            topology: crate::resources::GridTopology::Square,
        };
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        world.insert_resource(map);
        world.insert_resource(island_map);

        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // 孤立した歩兵を (1,1) に配置。周囲は海に接する Plains
        let infantry = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 1, // 移動力1
                    movement_type: crate::resources::MovementType::Infantry,
                    can_capture: true,
                    min_range: 1,
                    max_range: 1,
                    max_ammo1: 10,
                    max_ammo2: 10,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
                crate::components::Ammo {
                    ammo1: 10,
                    max_ammo1: 10,
                    ammo2: 10,
                    max_ammo2: 10,
                },
            ))
            .id();

        // 1. 敵や建物が他にない場合（孤立状態）
        // (1,1) にいる歩兵は海に隣接するマス（例: (0,1), (1,0), (1,2), (2,1)）のいずれかに移動して待機するはず。
        // なぜなら (1,1) は海に隣接しておらず、海までの距離が2だが、
        // 周囲4マスは海に隣接しており距離1だからである。
        let skips = crate::collections::HashSet::default();
        let action = decide_ai_action(&mut world, p1, &skips);
        assert!(action.is_some());
        if let Some((entity, AiCommand::Wait { target_pos })) = action {
            assert_eq!(entity, infantry);
            // (1,1) のままでなく、海に面した隣接マスのいずれかに移動していることを確認
            let dist = (target_pos.x as i32 - 1).abs() + (target_pos.y as i32 - 1).abs();
            assert_eq!(dist, 1); // 隣接マスへ移動
            assert!(target_pos.x < 3 && target_pos.y < 3); // かつ陸地の中
        } else {
            panic!("Expected Wait at coast tile, got {:?}", action);
        }

        // 2. 同じ島に敵ユニットを配置した場合（孤立していない状態）
        // 敵ユニットを (0,0) に配置
        let enemy = world
            .spawn((
                p2,
                Faction(p2),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
                    max_ammo1: 10,
                    max_ammo2: 10,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
                crate::components::Ammo {
                    ammo1: 10,
                    max_ammo1: 10,
                    ammo2: 10,
                    max_ammo2: 10,
                },
            ))
            .id();

        // これにより、島には敵がいるため、is_unit_stranded は false になるはず。
        // 歩兵は敵を攻撃しようとするはず。
        // (1,1) から (0,1) または (1,0) に移動して (0,0) の敵を攻撃するコマンドになるはず。
        let action2 = decide_ai_action(&mut world, p1, &skips);
        assert!(action2.is_some());
        if let Some((
            entity,
            AiCommand::Attack {
                target_pos,
                target_entity,
            },
        )) = action2
        {
            assert_eq!(entity, infantry);
            assert!(
                (target_pos.x == 0 && target_pos.y == 1)
                    || (target_pos.x == 1 && target_pos.y == 0)
            );
            assert_eq!(target_entity, enemy);
        } else {
            panic!("Expected Attack command on enemy, got {:?}", action2);
        }
    }

    #[test]
    #[allow(deprecated)]
    fn test_ai_mission_priority_and_cooldown() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());

        // 5x5の平地マップ
        let map = Map {
            width: 5,
            height: 5,
            tiles: vec![Terrain::Plains; 25],
            topology: crate::resources::GridTopology::Square,
        };
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        world.insert_resource(map);
        world.insert_resource(island_map);
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        // 必要なイベントリソースを登録
        world.insert_resource(Events::<crate::events::MoveUnitCommand>::default());
        world.insert_resource(Events::<crate::events::WaitUnitCommand>::default());
        world.insert_resource(Events::<crate::events::NextPhaseCommand>::default());

        let p1 = PlayerId(1);
        let mut ai_settings = crate::ai::ai_version::PlayerAiSettings::new();
        ai_settings.set_version(p1, crate::ai::ai_version::AiVersion::V1);
        world.insert_resource(ai_settings);

        // 1. 輸送機(ヘリ)を(0,0)に配置
        let heli = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    max_movement: 6,
                    movement_type: crate::resources::MovementType::Air,
                    max_cargo: 1,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        // 2. 歩兵を(3,0)に配置
        let infantry = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 3, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        // 3. ミッションを登録する
        // phase: Pickup, transport: heli, cargo: infantry
        let mission = crate::ai::missions::TransportMission {
            transport_entity: heli,
            cargo_entity: infantry,
            phase: crate::ai::missions::TransportPhase::Pickup,
            target_island: None,
        };
        let mut manager = crate::ai::missions::TransportMissionManager::default();
        manager.missions.push(mission);
        world.insert_resource(manager);

        // クールダウン用のリソースを登録
        world.insert_resource(AiActionCooldown(crate::collections::HashSet::default()));

        // 4. execute_ai_turn を呼び出す (1回目)
        // ミッション優先実行により、ヘリが歩兵(3,0)へ向かうコマンドが実行され、Someが返るはず。
        let result1 = execute_ai_turn(&mut world, p1);
        assert!(result1.is_some());

        // 5. ヘリが AiActionCooldown に追加されていることを確認
        let cooldown = world.get_resource::<AiActionCooldown>().unwrap();
        assert!(cooldown.0.contains(&heli));

        // イベントが送られていることを確認
        let move_events = world
            .get_resource::<Events<crate::events::MoveUnitCommand>>()
            .unwrap();
        let mut reader = move_events.get_reader();
        let sent_move = reader.read(move_events).next();
        assert!(sent_move.is_some());
        let move_cmd = sent_move.unwrap();
        assert_eq!(move_cmd.unit_entity, heli);
        // ヘリが (0, 0) から右方向 (x > 0) の歩兵 (3, 0) に向けて移動を開始したことを検証する
        assert!(move_cmd.target_x > 0 && move_cmd.target_x < 5);
        assert!(move_cmd.target_y < 5);

        // 6. 同一ターン内での2回目の execute_ai_turn 呼び出し
        // ヘリは cooldown のため無視される。
        let _result2 = execute_ai_turn(&mut world, p1);

        // cooldown リソースを確認し、ヘリがクールダウン中に留まっていること
        let cooldown2 = world.get_resource::<AiActionCooldown>().unwrap();
        assert!(cooldown2.0.contains(&heli));
    }

    /// V3 テスト用の共通ワールドを構築するヘルパー。
    /// 幅 width x 高さ 1 の平原マップと必要リソースを登録する。
    fn setup_v3_test_world(width: usize, version: crate::ai::ai_version::AiVersion) -> World {
        let mut world = World::new();
        world.insert_resource(Map {
            width,
            height: 1,
            tiles: vec![Terrain::Plains; width],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();
        let mut settings = crate::ai::ai_version::PlayerAiSettings::new();
        settings.set_version(PlayerId(1), version);
        settings.set_version(PlayerId(2), version);
        world.insert_resource(settings);
        world
    }

    /// 同条件の輸送ヘリから、搭載兵を持つ高価値目標を選ぶか検証するワールドを作る。
    fn setup_strategic_target_selection_world() -> (World, Entity, Entity, Entity) {
        let mut world = setup_v3_test_world(3, crate::ai::ai_version::AiVersion::V3);
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Fighter, UnitType::TransportHelicopter, 80);
        damage_chart.insert_damage(UnitType::TransportHelicopter, UnitType::Fighter, 0);
        world.insert_resource(damage_chart);

        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let attacker = world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 0 },
                UnitStats {
                    unit_type: UnitType::Fighter,
                    cost: 14000,
                    movement_type: crate::resources::MovementType::Air,
                    max_movement: 0,
                    min_range: 1,
                    max_range: 1,
                    max_ammo1: 10,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: 10,
                    max_ammo1: 10,
                    ammo2: 0,
                    max_ammo2: 0,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        let transport_stats = UnitStats {
            unit_type: UnitType::TransportHelicopter,
            cost: 4000,
            movement_type: crate::resources::MovementType::Air,
            max_cargo: 2,
            ..UnitStats::mock()
        };
        let empty_transport = world
            .spawn((
                Faction(enemy),
                GridPosition { x: 0, y: 0 },
                transport_stats.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
            ))
            .id();

        let cargo = world
            .spawn((
                Faction(enemy),
                GridPosition { x: 99, y: 99 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 12000,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Transporting(Entity::from_raw(0)),
            ))
            .id();
        let loaded_transport = world
            .spawn((
                Faction(enemy),
                GridPosition { x: 2, y: 0 },
                transport_stats,
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::CargoCapacity {
                    max: 2,
                    loaded: vec![cargo],
                },
            ))
            .id();
        world
            .entity_mut(cargo)
            .insert(crate::components::Transporting(loaded_transport));

        (world, attacker, empty_transport, loaded_transport)
    }

    #[test]
    fn v1_prioritizes_transport_with_loaded_combat_value() {
        let (mut world, _, _, loaded_transport) = setup_strategic_target_selection_world();

        let (_, action) = decide_ai_action(&mut world, PlayerId(1), &HashSet::default())
            .expect("V1が攻撃行動を選ぶこと");

        assert!(matches!(
            action,
            AiCommand::Attack { target_entity, .. } if target_entity == loaded_transport
        ));
    }

    #[test]
    fn v3_prioritizes_transport_with_loaded_combat_value() {
        let (mut world, _, _, loaded_transport) = setup_strategic_target_selection_world();

        let (_, action) = decide_ai_action_v2(&mut world, PlayerId(1), &HashSet::default())
            .expect("V3が攻撃行動を選ぶこと");

        assert!(matches!(
            action,
            AiCommand::Attack { target_entity, .. } if target_entity == loaded_transport
        ));
    }

    #[test]
    fn issue95_v4_deployment_target_precedes_generic_high_value_target() {
        let (mut world, attacker, assigned_target, generic_high_value_target) =
            setup_strategic_target_selection_world();
        let mut deployments = crate::ai::v4::deployment::V4DeploymentRegistry::default();
        deployments.assign_target_for_test(PlayerId(1), attacker, assigned_target);
        world.insert_resource(deployments);

        let (_, action) = decide_ai_action_v2(&mut world, PlayerId(1), &HashSet::default())
            .expect("V4の局地任務ユニットが攻撃行動を選ぶこと");

        assert!(matches!(
            action,
            AiCommand::Attack { target_entity, .. }
                if target_entity == assigned_target && target_entity != generic_high_value_target
        ));
    }

    #[test]
    fn v4_switches_from_bad_mission_matchup_to_favorable_local_target() {
        let (mut world, attacker, bad_mission_target, favorable_local_target) =
            setup_strategic_target_selection_world();
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Fighter, UnitType::TransportHelicopter, 10);
        damage_chart.insert_secondary_damage(UnitType::Fighter, UnitType::TransportHelicopter, 10);
        damage_chart.insert_damage(UnitType::TransportHelicopter, UnitType::Fighter, 90);
        damage_chart.insert_secondary_damage(UnitType::TransportHelicopter, UnitType::Fighter, 90);
        damage_chart.insert_damage(UnitType::Fighter, UnitType::Bcopters, 80);
        damage_chart.insert_secondary_damage(UnitType::Fighter, UnitType::Bcopters, 80);
        damage_chart.insert_damage(UnitType::Bcopters, UnitType::Fighter, 0);
        damage_chart.insert_secondary_damage(UnitType::Bcopters, UnitType::Fighter, 0);
        world.insert_resource(damage_chart);
        world
            .get_mut::<UnitStats>(favorable_local_target)
            .expect("局地標的の能力")
            .unit_type = UnitType::Bcopters;

        let mut deployments = crate::ai::v4::deployment::V4DeploymentRegistry::default();
        deployments.assign_target_for_test(PlayerId(1), attacker, bad_mission_target);
        world.insert_resource(deployments);

        let (_, action) = decide_ai_action_v2(&mut world, PlayerId(1), &HashSet::default())
            .expect("V4の局地任務ユニットが攻撃行動を選ぶこと");

        assert!(
            matches!(
                action,
                AiCommand::Attack { target_entity, .. }
                    if target_entity == favorable_local_target
                        && target_entity != bad_mission_target
            ),
            "選択={action:?}, bad={:?}, favorable={:?}",
            crate::ai::pruning::evaluate_attack_exchange(
                &world,
                attacker,
                bad_mission_target,
                GridPosition { x: 1, y: 0 },
                world.resource::<DamageChart>(),
            ),
            crate::ai::pruning::evaluate_attack_exchange(
                &world,
                attacker,
                favorable_local_target,
                GridPosition { x: 1, y: 0 },
                world.resource::<DamageChart>(),
            )
        );
    }

    #[test]
    fn v4_keeps_necessary_bad_exchange_when_no_favorable_target_exists() {
        let (mut world, attacker, bad_mission_target, other_target) =
            setup_strategic_target_selection_world();
        world.despawn(other_target);
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Fighter, UnitType::TransportHelicopter, 10);
        damage_chart.insert_secondary_damage(UnitType::Fighter, UnitType::TransportHelicopter, 10);
        damage_chart.insert_damage(UnitType::TransportHelicopter, UnitType::Fighter, 90);
        damage_chart.insert_secondary_damage(UnitType::TransportHelicopter, UnitType::Fighter, 90);
        world.insert_resource(damage_chart);

        let mut deployments = crate::ai::v4::deployment::V4DeploymentRegistry::default();
        deployments.assign_target_for_test(PlayerId(1), attacker, bad_mission_target);
        world.insert_resource(deployments);

        let (_, action) = decide_ai_action_v2(&mut world, PlayerId(1), &HashSet::default())
            .expect("実行段階の作戦対象には必要攻撃を選ぶこと");

        assert!(matches!(
            action,
            AiCommand::Attack { target_entity, .. } if target_entity == bad_mission_target
        ));
    }

    #[test]
    fn v2_v3_transport_executor_skips_foreign_owned_squads() {
        for version in [
            crate::ai::ai_version::AiVersion::V2,
            crate::ai::ai_version::AiVersion::V3,
        ] {
            let mut world = setup_v3_test_world(5, version);
            let map = world.resource::<Map>().clone();
            world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
            world.insert_resource(Events::<crate::events::WaitUnitCommand>::default());
            world.insert_resource(Events::<crate::events::MoveUnitCommand>::default());
            let player_a = PlayerId(1);
            let player_b = PlayerId(2);
            let property_a = GridPosition { x: 1, y: 0 };
            let property_b = GridPosition { x: 3, y: 0 };
            world.spawn((
                property_a,
                Property::new(Terrain::City, Some(player_a), 100),
            ));
            world.spawn((
                property_b,
                Property::new(Terrain::City, Some(player_b), 100),
            ));
            let transport_stats = UnitStats {
                unit_type: UnitType::TransportHelicopter,
                movement_type: crate::resources::MovementType::Air,
                max_movement: 6,
                max_cargo: 2,
                ..UnitStats::mock()
            };
            let transport_a = world
                .spawn((
                    Faction(player_a),
                    GridPosition { x: 0, y: 0 },
                    transport_stats.clone(),
                    crate::components::Fuel {
                        current: 99,
                        max: 99,
                    },
                    crate::components::CargoCapacity {
                        max: 2,
                        loaded: Vec::new(),
                    },
                ))
                .id();
            let ownerless_transport = world
                .spawn((
                    Faction(player_b),
                    GridPosition { x: 2, y: 0 },
                    transport_stats.clone(),
                    crate::components::Fuel {
                        current: 99,
                        max: 99,
                    },
                    crate::components::CargoCapacity {
                        max: 2,
                        loaded: Vec::new(),
                    },
                ))
                .id();
            let transport_b = world
                .spawn((
                    Faction(player_b),
                    GridPosition { x: 4, y: 0 },
                    transport_stats,
                    crate::components::Fuel {
                        current: 99,
                        max: 99,
                    },
                    crate::components::CargoCapacity {
                        max: 2,
                        loaded: Vec::new(),
                    },
                ))
                .id();
            let (foreign_id, foreign_snapshot) = {
                let mut manager = crate::ai::squad::SquadManager::new();
                let foreign =
                    manager.create_owned_squad(crate::ai::squad::MissionType::Transport, player_a);
                foreign.members.insert(transport_a);
                foreign.transport_entity = Some(transport_a);
                foreign.phase = crate::ai::squad::MissionPhase::Transport(
                    crate::ai::squad::TransportPhase::Return,
                );
                let snapshot = (
                    foreign.owner_id,
                    foreign.members.clone(),
                    foreign.transport_entity,
                    foreign.cargo_entities.clone(),
                    foreign.delivered_cargo.clone(),
                    foreign.target_island,
                    foreign.target,
                    foreign.phase.clone(),
                    foreign.pickup_position,
                    foreign.drop_position,
                );
                let id = foreign.id;
                let ownerless = manager.create_squad(crate::ai::squad::MissionType::Transport);
                ownerless.members.insert(ownerless_transport);
                ownerless.transport_entity = Some(ownerless_transport);
                ownerless.phase = crate::ai::squad::MissionPhase::Transport(
                    crate::ai::squad::TransportPhase::Return,
                );
                let own =
                    manager.create_owned_squad(crate::ai::squad::MissionType::Transport, player_b);
                own.members.insert(transport_b);
                own.transport_entity = Some(transport_b);
                own.phase = crate::ai::squad::MissionPhase::Transport(
                    crate::ai::squad::TransportPhase::Return,
                );
                world.insert_resource(manager);
                (id, snapshot)
            };
            let sentinel = world.spawn_empty().id();
            world.insert_resource(AiActionCooldown(HashSet::from_iter([sentinel])));

            let result_b = execute_ai_turn(&mut world, player_b);
            assert!(result_b.is_some());
            let manager = world.resource::<crate::ai::squad::SquadManager>();
            let foreign = manager
                .squads
                .iter()
                .find(|squad| squad.id == foreign_id)
                .unwrap();
            assert_eq!(
                (
                    foreign.owner_id,
                    foreign.members.clone(),
                    foreign.transport_entity,
                    foreign.cargo_entities.clone(),
                    foreign.delivered_cargo.clone(),
                    foreign.target_island,
                    foreign.target,
                    foreign.phase.clone(),
                    foreign.pickup_position,
                    foreign.drop_position,
                ),
                foreign_snapshot
            );
            let cooldown = world.resource::<AiActionCooldown>();
            assert!(!cooldown.0.contains(&transport_a));
            assert!(!cooldown.0.contains(&ownerless_transport));
            assert!(cooldown.0.contains(&transport_b));

            let result_a = execute_ai_turn(&mut world, player_a);
            assert!(result_a.is_some());
            assert!(
                world
                    .resource::<AiActionCooldown>()
                    .0
                    .contains(&transport_a)
            );
        }
    }

    /// 移動可能な自軍ユニットをスポーンするヘルパー
    fn spawn_v3_test_unit(
        world: &mut World,
        player: PlayerId,
        x: usize,
        hp: u32,
        stats: UnitStats,
    ) -> Entity {
        world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x, y: 0 },
                stats,
                Health {
                    current: hp,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id()
    }

    /// 指定ユニット1体のみからなる部隊 (目標つき) を登録するヘルパー
    fn insert_single_unit_squad(world: &mut World, member: Entity, target: GridPosition) {
        let mut manager = crate::ai::squad::SquadManager::default();
        let mut members = std::collections::BTreeSet::new();
        members.insert(member);
        manager.squads.push(crate::ai::squad::Squad {
            id: crate::ai::squad::SquadId(1),
            owner_id: None,
            members,
            mission_type: crate::ai::squad::MissionType::Attack,
            target: Some(target),
            target_island: None,
            phase: crate::ai::squad::MissionPhase::MovingToTarget,
            transport_entity: None,
            cargo_entities: Vec::new(),
            pickup_position: None,
            drop_position: None,
            delivered_cargo: Vec::new(),
            allow_partial_departure: false,
            departure_authorized: true,
            return_after_combat: false,
        });
        world.insert_resource(manager);
    }

    /// Issue #50: V3 は敵間接攻撃ユニットの射程 (脅威マップ) 内への
    /// 前進を避け、V2 は露出を考慮せず前進することを検証する
    #[test]
    fn test_v3_avoids_indirect_fire_exposure() {
        use crate::ai::ai_version::AiVersion;

        let run = |version: AiVersion| -> (usize, usize) {
            let mut world = setup_v3_test_world(12, version);
            let mut dc = DamageChart::new();
            // 重自走砲 (射程3-5) は軽戦車に大ダメージ、軽戦車の反対方向は中程度
            dc.insert_damage(UnitType::HeavySpGun, UnitType::Tank, 92);
            dc.insert_damage(UnitType::Tank, UnitType::HeavySpGun, 43);
            world.insert_resource(dc);

            // 自軍: 軽戦車 (移動4) at x=0
            let tank = spawn_v3_test_unit(
                &mut world,
                PlayerId(1),
                0,
                100,
                UnitStats {
                    unit_type: UnitType::Tank,
                    cost: 6000,
                    max_movement: 4,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 1,
                    max_range: 1,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
            );
            let _ = tank;

            // 敵軍: 重自走砲 (射程3-5) at x=9 -> 脅威ゾーンは x in [4,6]
            world.spawn((
                Faction(PlayerId(2)),
                HasMoved(true),
                ActionCompleted(true),
                GridPosition { x: 9, y: 0 },
                UnitStats {
                    unit_type: UnitType::HeavySpGun,
                    cost: 16500,
                    max_movement: 5,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 3,
                    max_range: 5,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ));

            let skips = crate::collections::HashSet::default();
            let action =
                decide_ai_action_v2(&mut world, PlayerId(1), &skips).expect("行動が決定されること");
            match action.1 {
                AiCommand::Wait { target_pos } => (target_pos.x, target_pos.y),
                other => panic!("Wait/Move を期待したが {:?}", other),
            }
        };

        // V2: 露出を考慮せず、最も敵に近い x=4 (脅威ゾーン内) へ前進する
        let (v2_x, _) = run(AiVersion::V2);
        assert_eq!(v2_x, 4, "V2 は脅威ゾーン内 (x=4) まで前進するはず");

        // V3: 脅威ゾーン (x in [4,6]) を避けて手前で待機する
        let (v3_x, _) = run(AiVersion::V3);
        assert!(
            v3_x < 4,
            "V3 は敵間接攻撃の射程外 (x<4) で待機するはず (actual: x={})",
            v3_x
        );
    }

    /// Issue #50 (Gemini #5): 間接攻撃ユニット (自走砲) が、敵直接攻撃ユニット
    /// (戦車) の「移動+攻撃」到達圏を避けることを検証する。反撃できない自走砲が
    /// 戦車の踏み込みに轢かれる配置を防ぐ。
    #[test]
    fn test_v3_avoids_direct_attacker_move_reach() {
        use crate::ai::ai_version::AiVersion;

        let run = |version: AiVersion| -> usize {
            let mut world = setup_v3_test_world(14, version);
            let mut dc = DamageChart::new();
            // 戦車 → 自走砲に大ダメージ (踏み込まれると一方的に轢かれる)
            dc.insert_damage(UnitType::Tank, UnitType::LightSpGun, 80);
            world.insert_resource(dc);

            // 自軍: 軽自走砲 (間接 射程2-3, 移動4) at x=0、部隊目標は x=13
            let sp = spawn_v3_test_unit(
                &mut world,
                PlayerId(1),
                0,
                100,
                UnitStats {
                    unit_type: UnitType::LightSpGun,
                    cost: 6200,
                    max_movement: 4,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 2,
                    max_range: 3,
                    max_ammo1: 5,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
            );
            insert_single_unit_squad(&mut world, sp, GridPosition { x: 13, y: 0 });

            // 敵軍: 軽戦車 (直接 射程1, 移動4) at x=9 -> 移動+攻撃到達圏は距離5以内 (x>=4)
            world.spawn((
                Faction(PlayerId(2)),
                HasMoved(true),
                ActionCompleted(true),
                GridPosition { x: 9, y: 0 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    cost: 6000,
                    max_movement: 4,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 1,
                    max_range: 1,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ));

            let skips = crate::collections::HashSet::default();
            let action =
                decide_ai_action_v2(&mut world, PlayerId(1), &skips).expect("行動が決定されること");
            match action.1 {
                AiCommand::Wait { target_pos } | AiCommand::Attack { target_pos, .. } => {
                    target_pos.x
                }
                other => panic!("Wait/Attack を期待したが {:?}", other),
            }
        };

        // V2: 移動+攻撃到達圏を考慮せず、目標へ最接近する x=4 (戦車の踏み込み圏内) へ
        let v2_x = run(AiVersion::V2);
        assert_eq!(v2_x, 4, "V2 は戦車の移動+攻撃圏内 (x=4) まで前進するはず");

        // V3: 戦車の移動+攻撃到達圏 (x>=4) を避けて x<=3 で待機する
        let v3_x = run(AiVersion::V3);
        assert!(
            v3_x <= 3,
            "V3 は戦車の移動+攻撃到達圏外 (x<=3) で待機するはず (actual: x={})",
            v3_x
        );
    }

    /// Issue #45: 間接攻撃ユニットが最小射程より内側へ不要な前進をせず、
    /// 先制攻撃圏 (待ち伏せ位置) で待機することを検証する
    #[test]
    fn test_v3_indirect_ambush_positioning() {
        use crate::ai::ai_version::AiVersion;

        let run = |version: AiVersion| -> usize {
            let mut world = setup_v3_test_world(12, version);
            let mut dc = DamageChart::new();
            dc.insert_damage(UnitType::LightSpGun, UnitType::Tank, 55);
            dc.insert_damage(UnitType::Tank, UnitType::LightSpGun, 56);
            world.insert_resource(dc);

            // 自軍: 軽自走砲 (射程2-3, 移動4) at x=0
            let sp_gun = spawn_v3_test_unit(
                &mut world,
                PlayerId(1),
                0,
                100,
                UnitStats {
                    unit_type: UnitType::LightSpGun,
                    cost: 6200,
                    max_movement: 4,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 2,
                    max_range: 3,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
            );

            // 敵軍: 軽戦車 at x=5
            world.spawn((
                Faction(PlayerId(2)),
                HasMoved(true),
                ActionCompleted(true),
                GridPosition { x: 5, y: 0 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    cost: 6000,
                    max_movement: 4,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 1,
                    max_range: 1,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ));

            // 部隊目標は敵戦車の位置
            insert_single_unit_squad(&mut world, sp_gun, GridPosition { x: 5, y: 0 });

            let skips = crate::collections::HashSet::default();
            let action =
                decide_ai_action_v2(&mut world, PlayerId(1), &skips).expect("行動が決定されること");
            match action.1 {
                AiCommand::Wait { target_pos } => target_pos.x,
                other => panic!("Wait/Move を期待したが {:?}", other),
            }
        };

        // V2: 接近ボーナスの勾配に従い、最小射程の内側 x=4 (距離1) まで前進してしまう
        let v2_x = run(AiVersion::V2);
        assert_eq!(v2_x, 4, "V2 は最小射程の内側 (x=4) まで前進するはず");

        // V3: 先制攻撃圏 (距離2-3 = x in [2,3]) で待ち伏せする
        let v3_x = run(AiVersion::V3);
        let v3_dist = 5 - v3_x;
        assert!(
            (2..=3).contains(&v3_dist),
            "V3 は射程内の待ち伏せ位置 (距離2-3) で待機するはず (actual: x={}, dist={})",
            v3_x,
            v3_dist
        );
    }

    /// Issue #44: HP が低下したユニットが、接近ボーナスの勾配に逆らってでも
    /// 平地より防御効果の高い森で待機することを検証する。
    /// ただし敵の脅威が近くにある場合に限る（Gemini 指摘 #3: 安全な後方での
    /// 無意味な引きこもりを防ぐゲート）。
    #[test]
    fn test_v3_low_hp_prefers_defensive_terrain() {
        use crate::ai::ai_version::AiVersion;

        // enemy_x: 敵ユニットの位置。None なら敵なし（安全な後方）
        let run = |version: AiVersion, hp: u32, enemy_x: Option<usize>| -> usize {
            let mut world = setup_v3_test_world(12, version);
            world.insert_resource(DamageChart::new());
            // x=2 だけ森 (防御20)、他は平地 (防御5)
            world
                .resource_mut::<Map>()
                .set_terrain(2, 0, Terrain::Forest)
                .unwrap();

            // 自軍: 軽戦車 at x=0 (部隊目標 x=10 に向かって前進中)
            let tank = spawn_v3_test_unit(
                &mut world,
                PlayerId(1),
                0,
                hp,
                UnitStats {
                    unit_type: UnitType::Tank,
                    cost: 6000,
                    max_movement: 4,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 1,
                    max_range: 1,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
            );
            insert_single_unit_squad(&mut world, tank, GridPosition { x: 10, y: 0 });

            // 敵ユニット (行動済み・脅威の存在のみを表現)
            if let Some(ex) = enemy_x {
                world.spawn((
                    Faction(PlayerId(2)),
                    HasMoved(true),
                    ActionCompleted(true),
                    GridPosition { x: ex, y: 0 },
                    UnitStats {
                        unit_type: UnitType::Tank,
                        cost: 6000,
                        max_movement: 4,
                        movement_type: crate::resources::MovementType::Tank,
                        min_range: 1,
                        max_range: 1,
                        ..UnitStats::mock()
                    },
                    Health {
                        current: 100,
                        max: 100,
                    },
                ));
            }

            let skips = crate::collections::HashSet::default();
            let action =
                decide_ai_action_v2(&mut world, PlayerId(1), &skips).expect("行動が決定されること");
            match action.1 {
                AiCommand::Wait { target_pos }
                | AiCommand::Attack { target_pos, .. }
                | AiCommand::Capture { target_pos }
                | AiCommand::Merge { target_pos, .. } => target_pos.x,
                other => panic!("位置を伴う行動を期待したが {:?}", other),
            }
        };

        // 近傍脅威あり (敵戦車 x=6, 前線の森 x=2 から距離4)
        let threat = Some(6usize);

        // V2 は低HPでも目標へ最短で前進する (森 x=2 の移動コストにより x=3 が最遠到達点)
        let v2_x = run(AiVersion::V2, 40, threat);
        assert_eq!(v2_x, 3, "V2 は低HPでも平地 (x=3) まで前進するはず");

        // V3 は健全時は前進を優先し、低HP＋近傍脅威ありなら森 (x=2) で待機する
        let v3_healthy_x = run(AiVersion::V3, 100, threat);
        assert_eq!(v3_healthy_x, 3, "V3 も健全時は前進を優先するはず");
        let v3_low_hp_x = run(AiVersion::V3, 40, threat);
        assert_eq!(
            v3_low_hp_x, 2,
            "V3 は低HP+近傍脅威で森 (x=2) に退避するはず (actual: x={})",
            v3_low_hp_x
        );

        // #3 ゲート: 敵が近くにいない安全な後方では、低HPでも森に引きこもらず前進する
        let v3_low_hp_safe = run(AiVersion::V3, 40, None);
        assert_eq!(
            v3_low_hp_safe, 3,
            "V3 は低HPでも脅威がなければ森に籠らず前進するはず (actual: x={})",
            v3_low_hp_safe
        );
    }
    #[test]
    fn v4_profiles_share_turn_resources_and_reset_them_at_turn_end() {
        use crate::ai::strategy_profile::{V4StrategyProfile, profile_for};
        use crate::ai::{AiVersion, PlayerAiSettings};
        let master_data = MasterDataRegistry::load().expect("マスターデータの読み込みに失敗");
        for (map_name, expected_profile) in [
            ("map_6", V4StrategyProfile::Standard),
            ("map_1", V4StrategyProfile::SmallMapExpansion),
        ] {
            let (mut world, mut schedule) =
                crate::setup::initialize_world_from_master_data(&master_data, map_name)
                    .expect("ワールド初期化に失敗");
            let player = PlayerId(1);
            let mut settings = PlayerAiSettings::default();
            settings.set_version(player, AiVersion::V4);
            world.insert_resource(settings);
            assert_eq!(profile_for(&mut world, player), expected_profile);

            // 両プロファイルが同じAIキャッシュと生産クールダウンを利用することを保証する。
            assert!(execute_ai_turn(&mut world, player).is_some());
            assert!(
                world.contains_resource::<crate::ai::planning::decision::AdoptedTurnPortfolio>()
            );
            // 生産旗と距離cacheは、利用しない手番には生成されない。
            // その場合も境界で破棄されることを確認するため、空の状態を用意する。
            world.init_resource::<crate::ai::production::turn_state::ProductionTurnState>();
            world.init_resource::<crate::ai::tactics::distance_cache::ActionDistanceCache>();
            assert!(
                world.contains_resource::<crate::ai::production::turn_state::ProductionTurnState>()
            );
            assert!(world.contains_resource::<crate::ai::runtime::state::AiTurnProgress>());
            assert!(
                world
                    .contains_resource::<crate::ai::tactics::distance_cache::ActionDistanceCache>()
            );
            assert!(!world.resource::<AiProductionCooldown>().0.is_empty());
            schedule.run(&mut world);

            // ターンだけの状態は破棄し、配備・作戦の永続台帳は共通の型で保持する。
            world
                .resource_mut::<Events<crate::events::NextPhaseCommand>>()
                .send(crate::events::NextPhaseCommand);
            schedule.run(&mut world);
            assert!(
                !world.contains_resource::<crate::ai::planning::decision::AdoptedTurnPortfolio>()
            );
            assert!(
                !world
                    .contains_resource::<crate::ai::production::turn_state::ProductionTurnState>()
            );
            assert!(!world.contains_resource::<crate::ai::runtime::state::AiTurnProgress>());
            assert!(
                !world
                    .contains_resource::<crate::ai::tactics::distance_cache::ActionDistanceCache>()
            );
            assert!(!world.contains_resource::<AiActionCooldown>());
            assert!(!world.contains_resource::<AiProductionCooldown>());
            assert!(world.contains_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>());
            assert!(
                world.contains_resource::<crate::ai::v4::plan_revision::V4RollingPlanRegistry>()
            );
            assert!(
                world.contains_resource::<crate::ai::v4::victory_roadmap::VictoryRoadmapRegistry>()
            );
            assert_eq!(profile_for(&mut world, player), expected_profile);
        }
    }
}

// 小規模マップ戦略など拡張経路の実装。main互換経路と本文が異なる関数と拡張専用の項目を置く。
