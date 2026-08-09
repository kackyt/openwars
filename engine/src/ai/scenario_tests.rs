//! AI（V1およびV2）の意思決定を検証するためのシナリオベースの結合テスト。
//! 各テストケースは、AIが特定の戦況でどのような判断を下すか（または下さないか）を検証し、リグレッションを防ぐことを目的とします。

#[cfg(test)]
mod tests {
    use crate::ai::engine::*;
    use crate::components::*;
    use crate::resources::master_data::*;
    use crate::resources::*;
    use bevy_ecs::prelude::*;

    /// 共通のテストワールドセットアップ
    fn setup_test_world(width: usize, height: usize, default_terrain: Terrain) -> World {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }

        let map = Map {
            width,
            height,
            tiles: vec![default_terrain; width * height],
            topology: GridTopology::Square,
        };
        world.insert_resource(map.clone());
        world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));

        if let Some(mut players) = world.get_resource_mut::<crate::resources::Players>() {
            for p in &mut players.0 {
                p.funds = 50000;
            }
        }
        world
    }

    /// 1. 首都防衛 (Capital Defense)
    fn setup_capital_defense() -> World {
        let mut world = setup_test_world(10, 10, Terrain::Plains);
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);
        world.spawn((
            GridPosition { x: 5, y: 5 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            p2,
            Faction(p2),
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                max_movement: 6,
                can_capture: false,
                ..UnitStats::mock()
            },
            GridPosition { x: 4, y: 5 },
            Health {
                current: 100,
                max: 100,
            },
        ));
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            UnitStats {
                unit_type: UnitType::MdTank,
                movement_type: MovementType::Tank,
                max_movement: 5,
                min_range: 1,
                max_range: 1,
                can_capture: false,
                ..UnitStats::mock()
            },
            GridPosition { x: 7, y: 5 },
            Health {
                current: 100,
                max: 100,
            },
            Fuel {
                current: 99,
                max: 99,
            },
            Ammo {
                ammo1: 9,
                ammo2: 0,
                max_ammo1: 9,
                max_ammo2: 0,
            },
        ));
        world
    }

    /// ## 1. 首都防衛 (Capital Defense)
    /// - **ケース**: 敵ユニットが自軍首都の目の前に迫っている状況。
    /// - **期待結果 (V1)**: ユニットごとに貪欲に攻撃や移動を判断するため、首都をがら空きにして敵に突っ込むなど、防衛目標を優先しない行動をとる。
    /// - **期待結果 (V2)**: 部隊（Squad）システムとビーム探索により、首都防衛（Defendミッション）を最優先と認識し、ユニットを首都周辺に待機させて防衛線を張る。
    #[test]
    fn test_scenario_1_capital_defense() {
        let p1 = PlayerId(1);

        // V1
        let mut world_v1 = setup_capital_defense();
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(p1, crate::ai::AiVersion::V1);
        world_v1.insert_resource(settings);
        let cmd_v1 = execute_ai_turn_v1(&mut world_v1, p1);
        let cmd_str_v1 = cmd_v1.expect("V1 should take action");
        assert!(
            !cmd_str_v1.contains("Wait { target_pos: GridPosition { x: 5, y: 5 } }"),
            "V1 should NOT move to capital (5,5) to Wait for defense (it prioritizes attacking greedily)"
        );

        // V2
        let mut world_v2 = setup_capital_defense();
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(p1, crate::ai::AiVersion::V2);
        world_v2.insert_resource(settings);
        crate::ai::squad::plan_squads(&mut world_v2, p1);
        let cmd_v2 = execute_ai_turn_v2(&mut world_v2, p1);
        let cmd_str_v2 = cmd_v2.expect("V2 should take action");
        assert!(
            cmd_str_v2.contains("GridPosition { x: 5, y: 5 }"),
            "V2 should move to or stay at the capital (5,5) to defend"
        );
    }

    /// 2. 拠点占領 (Property Capture)
    fn setup_property_capture() -> World {
        let mut world = setup_test_world(10, 10, Terrain::Plains);
        let p1 = PlayerId(1);
        world.spawn((
            GridPosition { x: 5, y: 5 },
            Property::new(Terrain::City, None, 100),
        ));
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                min_range: 1,
                max_range: 1,
                can_capture: true,
                ..UnitStats::mock()
            },
            GridPosition { x: 4, y: 5 },
            Health {
                current: 100,
                max: 100,
            },
            Fuel {
                current: 99,
                max: 99,
            },
            Ammo {
                ammo1: 0,
                ammo2: 0,
                max_ammo1: 0,
                max_ammo2: 0,
            },
        ));
        world
    }

    /// ## 2. 拠点占領 (Property Capture)
    /// - **ケース**: 歩兵が中立または敵の都市・工場の近くに配置されている状況。
    /// - **期待結果 (共通)**: 歩兵が近くの拠点を占領しようとする。
    /// - **期待結果 (V2特有)**: 歩兵が占領部隊として行動し、他の戦闘ユニットが護衛や露払いを行う連携がみられる。
    #[test]
    fn test_scenario_2_property_capture() {
        let p1 = PlayerId(1);

        let mut world_v1 = setup_property_capture();
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(p1, crate::ai::AiVersion::V1);
        world_v1.insert_resource(settings);
        let cmd_v1 = execute_ai_turn_v1(&mut world_v1, p1);
        assert!(
            cmd_v1.unwrap().starts_with("Capture"),
            "V1 Infantry should capture"
        );

        let mut world_v2 = setup_property_capture();
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(p1, crate::ai::AiVersion::V2);
        world_v2.insert_resource(settings);
        crate::ai::squad::plan_squads(&mut world_v2, p1);
        let cmd_v2 = execute_ai_turn_v2(&mut world_v2, p1);
        assert!(
            cmd_v2.unwrap().starts_with("Capture"),
            "V2 Infantry should execute Capture command"
        );
    }

    /// ## 3. 対抗生産 (Counter Production)
    /// - **ケース**: 敵が戦車（Tank）を大量に生産して攻めてきている状況での自軍の生産フェーズ。
    /// - **期待結果 (V1, V2共通)**: AIの生産ロジック（`decide_production`）は共通であるため、V1/V2ともに需要予測（Demand）ロジックにより、戦車に対するアンチユニット（対戦車兵器や中戦車など）を優先的に生産する。本シナリオはAIのバージョン差異ではなく、生産ロジックの正当性を検証する。
    #[test]
    fn test_scenario_3_counter_production() {
        let mut world = setup_test_world(10, 10, Terrain::Plains);
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 2 },
            Property::new(Terrain::Factory, Some(p1), 100),
        ));

        for i in 0..5 {
            world.spawn((
                p2,
                Faction(p2),
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    cost: 7000,
                    ..UnitStats::mock()
                },
                GridPosition { x: 8, y: i + 1 },
                Health {
                    current: 100,
                    max: 100,
                },
            ));
        }

        // V1/V2共有ロジックの契約を検証するため、既定値のV3に依存させない。
        world
            .resource_mut::<crate::ai::ai_version::PlayerAiSettings>()
            .set_version(p1, crate::ai::AiVersion::V1);
        let prod_commands = crate::ai::production::decide_production(&mut world, p1);
        let is_anti_tank = prod_commands.iter().any(|cmd| {
            matches!(
                cmd.unit_type,
                UnitType::Mech | UnitType::MdTank | UnitType::Artillery | UnitType::TankZ
            )
        });
        assert!(
            is_anti_tank,
            "Shared AI should produce anti-tank units: {prod_commands:?}"
        );
    }

    /// 4. 輸送連携 (Transport Invasion)
    fn setup_transport_invasion() -> World {
        let mut world = setup_test_world(10, 10, Terrain::Sea);
        world.resource_mut::<Map>().tiles[11] = Terrain::Port;
        world.resource_mut::<Map>().tiles[8 * 10 + 8] = Terrain::Plains;
        let map = world.resource::<Map>().clone();
        world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        world.insert_resource(crate::ai::missions::TransportMissionManager::default());

        world.spawn((
            GridPosition { x: 1, y: 1 },
            // 生産可能なcombat unit costをshortfallへ正規化できる有効Capital。
            Property::new(Terrain::Capital, Some(p1), 100),
        ));

        world.spawn((
            GridPosition { x: 8, y: 8 },
            Property::new(Terrain::City, Some(p2), 100),
        ));
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            UnitStats {
                unit_type: UnitType::Lander,
                movement_type: MovementType::Ship,
                max_movement: 6,
                max_cargo: 2,
                can_capture: false,
                loadable_unit_types: vec![UnitType::Infantry, UnitType::Mech],
                ..UnitStats::mock()
            },
            GridPosition { x: 1, y: 1 },
            CargoCapacity {
                max: 2,
                loaded: vec![],
            },
            Health {
                current: 100,
                max: 100,
            },
            Fuel {
                current: 99,
                max: 99,
            },
            Ammo {
                ammo1: 0,
                ammo2: 0,
                max_ammo1: 0,
                max_ammo2: 0,
            },
        ));
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                can_capture: true,
                ..UnitStats::mock()
            },
            GridPosition { x: 1, y: 1 },
            Health {
                current: 100,
                max: 100,
            },
            Fuel {
                current: 99,
                max: 99,
            },
            Ammo {
                ammo1: 0,
                ammo2: 0,
                max_ammo1: 0,
                max_ammo2: 0,
            },
        ));
        world
    }

    /// ## 4. 輸送連携 (Transport Invasion)
    /// - **ケース**: 陸続きではない島に自軍の歩兵と輸送船があり、離れた島に目標（中立拠点など）がある状況。
    /// - **期待結果 (V1, V2共通)**: 輸送ミッション（Transport）を計画し、歩兵が輸送船に乗車（Pickup）するフェーズへと移行する。V1は `TransportMissionManager`、V2は `SquadManager` にて内部ステートとして適切にミッションがスケジュールされることを確認する。
    #[test]
    fn test_scenario_4_transport_invasion() {
        let p1 = PlayerId(1);

        // V1: 従来から輸送ミッションをサポートするようになったため、V1も輸送計画を立てる
        let mut world_v1 = setup_transport_invasion();
        crate::ai::planner::assign_transport_missions(&mut world_v1, p1);
        let manager_v1 = world_v1
            .get_resource::<crate::ai::missions::TransportMissionManager>()
            .expect("V1 should create transport manager");
        assert!(
            manager_v1
                .missions
                .iter()
                .any(|m| m.phase == crate::ai::missions::TransportPhase::Pickup),
            "V1 should plan a Pickup transport mission"
        );

        // V2: 輸送ミッションを計画し、歩兵が輸送船に向かう
        let mut world_v2 = setup_transport_invasion();
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(p1, crate::ai::AiVersion::V2);
        world_v2.insert_resource(settings);
        crate::ai::squad::plan_squads(&mut world_v2, p1);
        let manager_v2 = world_v2
            .get_resource::<crate::ai::squad::SquadManager>()
            .expect("V2 should create squad manager");
        assert!(
            manager_v2.squads.iter().any(|s| s.mission_type
                == crate::ai::squad::MissionType::Transport
                && s.phase
                    == crate::ai::squad::MissionPhase::Transport(
                        crate::ai::squad::TransportPhase::Pickup
                    )),
            "V2 should plan a Pickup transport mission"
        );
    }

    /// 5. 戦術的退却 (Tactical Retreat)
    fn setup_tactical_retreat() -> World {
        let mut world = setup_test_world(10, 10, Terrain::Plains);
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::City, Some(p1), 100),
        ));
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                max_movement: 6,
                min_range: 1,
                max_range: 1,
                can_capture: false,
                ..UnitStats::mock()
            },
            GridPosition { x: 4, y: 4 },
            Health {
                current: 20,
                max: 100,
            },
            Fuel {
                current: 99,
                max: 99,
            },
            Ammo {
                ammo1: 9,
                ammo2: 0,
                max_ammo1: 9,
                max_ammo2: 0,
            },
        ));
        world.spawn((
            p2,
            Faction(p2),
            UnitStats {
                unit_type: UnitType::MdTank,
                movement_type: MovementType::Tank,
                max_movement: 5,
                can_capture: false,
                ..UnitStats::mock()
            },
            GridPosition { x: 5, y: 4 },
            Health {
                current: 100,
                max: 100,
            },
        ));
        world
    }

    /// ## 5. 戦術的退却 (Tactical Retreat)
    /// - **ケース**: 自軍ユニットのHPが極めて低く、敵が接近しているが、後方に回復可能な自軍都市が存在する状況。
    /// - **期待結果 (V1, V2共通)**: AI共通の盤面評価（`eval.rs`）において、自軍都市上に移動して回復・防衛ボーナスを得ることがスコア上有利に働くため、V1・V2ともに生存を優先して後方の回復可能な都市へ退却する。本シナリオはAI全体の生存優先ロジックの正当性を検証する。
    #[test]
    fn test_scenario_5_tactical_retreat() {
        let p1 = PlayerId(1);

        let mut world_v1 = setup_tactical_retreat();
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(p1, crate::ai::AiVersion::V1);
        world_v1.insert_resource(settings);
        let cmd_v1 = execute_ai_turn_v1(&mut world_v1, p1);
        let cmd_str_v1 = cmd_v1.expect("V1 should take action");
        // V1の評価基盤（eval.rs）でも自軍都市上での回復や防御が正当に評価されるため、V1も退却行動をとる。
        assert!(
            cmd_str_v1.contains("target_pos: GridPosition { x: 1, y: 1 }"),
            "V1 should also retreat to the city at (1,1)"
        );

        let mut world_v2 = setup_tactical_retreat();
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(p1, crate::ai::AiVersion::V2);
        world_v2.insert_resource(settings);
        crate::ai::squad::plan_squads(&mut world_v2, p1);
        let cmd_v2 = execute_ai_turn_v2(&mut world_v2, p1);
        let cmd_str_v2 = cmd_v2.expect("V2 should take action");
        assert!(
            cmd_str_v2.contains("target_pos: GridPosition { x: 1, y: 1 }"),
            "V2 should retreat to the city at (1,1)"
        );
    }

    /// 6. 海を隔てた強襲上陸と敵地攻撃 (Amphibious Assault & Attack)
    fn setup_amphibious_assault() -> World {
        let mut world = setup_test_world(10, 10, Terrain::Sea);
        world.resource_mut::<Map>().tiles[11] = Terrain::Port;
        world.resource_mut::<Map>().tiles[8 * 10 + 8] = Terrain::Plains;
        // 島Bに降車用の空き地(8,7)を追加
        world.resource_mut::<Map>().tiles[7 * 10 + 8] = Terrain::Plains;
        let map = world.resource::<Map>().clone();
        world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        world.insert_resource(crate::ai::missions::TransportMissionManager::default());

        world.spawn((
            GridPosition { x: 1, y: 1 }, // Map defaults to Sea, but let's assume it's acceptable or we change it to plains
            Property::new(Terrain::Factory, Some(p1), 100),
        ));
        world.resource_mut::<Map>().tiles[11] = Terrain::Port;

        world.spawn((
            GridPosition { x: 8, y: 8 },
            Property::new(Terrain::City, Some(p2), 100),
        ));
        world.spawn((
            p2,
            Faction(p2),
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                can_capture: true,
                ..UnitStats::mock()
            },
            GridPosition { x: 8, y: 8 },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let infantry = world
            .spawn((
                p1,
                Faction(p1),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        let tank = world
            .spawn((
                p1,
                Faction(p1),
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    can_capture: false,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            UnitStats {
                unit_type: UnitType::Lander,
                movement_type: MovementType::Ship,
                max_movement: 6,
                max_cargo: 2,
                can_capture: false,
                loadable_unit_types: vec![UnitType::Infantry, UnitType::Mech, UnitType::Tank],
                ..UnitStats::mock()
            },
            GridPosition { x: 7, y: 8 },
            CargoCapacity {
                max: 2,
                loaded: vec![infantry, tank],
            },
            Health {
                current: 100,
                max: 100,
            },
            Fuel {
                current: 99,
                max: 99,
            },
            Ammo {
                ammo1: 0,
                ammo2: 0,
                max_ammo1: 0,
                max_ammo2: 0,
            },
        ));
        world
    }

    /// ## 6. 海を隔てた強襲上陸と敵地攻撃 (Amphibious Assault & Attack)
    /// - **ケース**: 島Aに自軍の歩兵・戦車と輸送船があり、海を隔てた島Bに敵の歩兵と都市がある状況。
    /// - **期待結果 (V1, V2共通)**: 既にユニットが積載された輸送船に対し、目標の島Bへ移動・降車（Drop）するための Transitフェーズ の輸送ミッションを計画する。V1は `TransportMissionManager`、V2は `SquadManager` にて内部ステートとして適切にスケジュールされることを確認する。
    #[test]
    fn test_scenario_6_amphibious_assault() {
        let p1 = PlayerId(1);

        // V1: V1も輸送ロジックを共有しているため、Transitミッションを計画する
        let mut world_v1 = setup_amphibious_assault();
        crate::ai::planner::assign_transport_missions(&mut world_v1, p1);
        let manager_v1 = world_v1
            .get_resource::<crate::ai::missions::TransportMissionManager>()
            .expect("V1 should create transport manager");
        assert!(
            manager_v1
                .missions
                .iter()
                .any(|m| m.phase == crate::ai::missions::TransportPhase::Transit),
            "V1 should plan a Transit transport mission to drop units"
        );

        let mut world_v2 = setup_amphibious_assault();
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(p1, crate::ai::AiVersion::V2);
        world_v2.insert_resource(settings);
        crate::ai::squad::plan_squads(&mut world_v2, p1);
        let manager_v2 = world_v2
            .get_resource::<crate::ai::squad::SquadManager>()
            .expect("V2 should create squad manager");
        let transport_squad = manager_v2
            .squads
            .iter()
            .find(|squad| {
                squad.mission_type == crate::ai::squad::MissionType::Transport
                    && squad.phase
                        == crate::ai::squad::MissionPhase::Transport(
                            crate::ai::squad::TransportPhase::Transit,
                        )
            })
            .expect("V2 should plan a Transit transport mission to drop units");
        assert!(transport_squad.transport_entity.is_some());
        assert_eq!(
            transport_squad.cargo_entities.len(),
            2,
            "搭載済みの全カーゴを輸送部隊が追跡する必要がある"
        );
    }

    #[test]
    fn test_v3_transport_wave_contains_capture_and_combat_cargo() {
        let p1 = PlayerId(1);
        let mut world = setup_transport_invasion();
        let mut settings = crate::ai::ai_version::PlayerAiSettings::default();
        settings.set_version(p1, crate::ai::ai_version::AiVersion::V3);
        world.insert_resource(settings);

        // 輸送船が戦車を搭載できる侵攻用構成にする。
        let lander = world
            .query::<(Entity, &UnitStats)>()
            .iter(&world)
            .find(|(_, stats)| stats.unit_type == UnitType::Lander)
            .map(|(entity, _)| entity)
            .unwrap();
        world
            .get_mut::<UnitStats>(lander)
            .unwrap()
            .loadable_unit_types
            .push(UnitType::Tank);

        let tank = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    can_capture: false,
                    ..UnitStats::mock()
                },
                GridPosition { x: 1, y: 1 },
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
                Ammo {
                    ammo1: 9,
                    ammo2: 0,
                    max_ammo1: 9,
                    max_ammo2: 0,
                },
            ))
            .id();

        crate::ai::squad::plan_squads(&mut world, p1);
        let manager = world.resource::<crate::ai::squad::SquadManager>();
        let transport = manager
            .squads
            .iter()
            .find(|squad| squad.mission_type == crate::ai::squad::MissionType::Transport)
            .expect("V3 should create one invasion transport squad");
        assert_eq!(transport.transport_entity, Some(lander));
        assert_eq!(transport.cargo_entities.len(), 2);
        assert!(
            transport.cargo_entities.contains(&tank),
            "侵攻波には戦闘要員を含める"
        );
        assert!(transport.cargo_entities.iter().any(|entity| {
            world
                .get::<UnitStats>(*entity)
                .is_some_and(|stats| stats.can_capture)
        }));
        assert!(transport.cargo_entities.len() <= 2);
    }

    #[test]
    fn test_v3_unassigned_partial_load_uses_targetless_safe_drop() {
        let p1 = PlayerId(1);
        let mut world = setup_transport_invasion();
        let mut settings = crate::ai::ai_version::PlayerAiSettings::default();
        settings.set_version(p1, crate::ai::ai_version::AiVersion::V3);
        world.insert_resource(settings);
        let lander = world
            .query::<(Entity, &UnitStats)>()
            .iter(&world)
            .find(|(_, stats)| stats.unit_type == UnitType::Lander)
            .map(|(entity, _)| entity)
            .unwrap();
        world
            .get_mut::<UnitStats>(lander)
            .unwrap()
            .loadable_unit_types
            .push(UnitType::Tank);
        let infantry = world
            .query::<(Entity, &UnitStats)>()
            .iter(&world)
            .find(|(_, stats)| stats.unit_type == UnitType::Infantry)
            .map(|(entity, _)| entity)
            .unwrap();
        world.get_mut::<CargoCapacity>(lander).unwrap().loaded = vec![infantry];
        world.entity_mut(infantry).insert(Transporting(lander));
        *world.get_mut::<GridPosition>(infantry).unwrap() = GridPosition { x: 9999, y: 9999 };
        let tank = world
            .spawn((
                Faction(p1),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    ..UnitStats::mock()
                },
                HasMoved(false),
                ActionCompleted(false),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        crate::ai::squad::plan_squads(&mut world, p1);
        let manager = world.resource::<crate::ai::squad::SquadManager>();
        let transport = manager
            .squads
            .iter()
            .find(|squad| squad.mission_type == crate::ai::squad::MissionType::Transport)
            .unwrap();
        assert_eq!(transport.cargo_entities, vec![infantry]);
        assert!(!transport.cargo_entities.contains(&tank));
        assert_eq!(transport.target_island, None);
        assert_eq!(transport.target, None);
        assert_eq!(
            transport.phase,
            crate::ai::squad::MissionPhase::Transport(crate::ai::squad::TransportPhase::Drop)
        );
    }

    #[test]
    fn test_v3_keeps_neutral_island_transport_expansion() {
        let p1 = PlayerId(1);
        let mut world = setup_transport_invasion();
        let mut settings = crate::ai::ai_version::PlayerAiSettings::default();
        settings.set_version(p1, crate::ai::ai_version::AiVersion::V3);
        world.insert_resource(settings);
        let neutral_property = world
            .query::<(Entity, &GridPosition, &Property)>()
            .iter(&world)
            .find(|(_, position, _)| **position == GridPosition { x: 8, y: 8 })
            .map(|(entity, _, _)| entity)
            .unwrap();
        world
            .get_mut::<Property>(neutral_property)
            .unwrap()
            .owner_id = None;
        let transport = world
            .query::<(Entity, &UnitStats)>()
            .iter(&world)
            .find(|(_, stats)| stats.unit_type == UnitType::Lander)
            .map(|(entity, _)| entity)
            .unwrap();
        {
            let mut stats = world.get_mut::<UnitStats>(transport).unwrap();
            stats.unit_type = UnitType::TransportHelicopter;
            stats.movement_type = MovementType::Air;
            stats.loadable_unit_types = vec![UnitType::Infantry, UnitType::Mech];
        }
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                can_capture: true,
                ..UnitStats::mock()
            },
            GridPosition { x: 1, y: 1 },
            Health {
                current: 100,
                max: 100,
            },
            Fuel {
                current: 99,
                max: 99,
            },
            Ammo {
                ammo1: 0,
                ammo2: 0,
                max_ammo1: 0,
                max_ammo2: 0,
            },
        ));

        crate::ai::squad::plan_squads(&mut world, p1);
        assert!(
            world
                .resource::<crate::ai::squad::SquadManager>()
                .squads
                .iter()
                .any(|squad| squad.mission_type == crate::ai::squad::MissionType::Transport),
            "敵島侵攻が無い場合も中立島への輸送拡張を維持する"
        );
    }

    #[test]
    fn test_v3_multi_cargo_invasion_loads_transits_and_lands() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, mut schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(0, 1, Terrain::Port).unwrap();
        map.set_terrain(3, 1, Terrain::Shoal).unwrap();
        for position in [(3, 0), (4, 0), (4, 1), (4, 2)] {
            map.set_terrain(position.0, position.1, Terrain::Plains)
                .unwrap();
        }
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let target_island = island_map
            .get_island_at(&GridPosition { x: 4, y: 0 })
            .unwrap()
            .id;
        world.insert_resource(map);
        world.insert_resource(island_map);

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);
        world.spawn((
            GridPosition { x: 0, y: 1 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            GridPosition { x: 4, y: 0 },
            Property::new(Terrain::Capital, Some(p2), 100),
        ));
        world.spawn((
            Faction(p2),
            GridPosition { x: 4, y: 2 },
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                max_movement: 6,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let capture = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 0, y: 1 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    can_capture: true,
                    cost: 1000,
                    ..UnitStats::mock()
                },
                HasMoved(false),
                ActionCompleted(false),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();
        let combat = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 0, y: 1 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    cost: 7000,
                    ..UnitStats::mock()
                },
                HasMoved(false),
                ActionCompleted(false),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();
        let transport = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 0, y: 1 },
                UnitStats {
                    unit_type: UnitType::Lander,
                    movement_type: MovementType::Ship,
                    max_movement: 6,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry, UnitType::Tank],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
                HasMoved(false),
                ActionCompleted(false),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        let mut manager = crate::ai::squad::SquadManager::new();
        let squad = manager.create_squad(crate::ai::squad::MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities = vec![capture, combat];
        squad.pickup_position = Some(GridPosition { x: 0, y: 1 });
        squad.target_island = Some(target_island);
        squad.target = Some(GridPosition { x: 4, y: 0 });
        squad.phase =
            crate::ai::squad::MissionPhase::Transport(crate::ai::squad::TransportPhase::Pickup);
        world.insert_resource(manager);

        let execute_transport = |world: &mut World, schedule: &mut Schedule| {
            let mut manager = world
                .remove_resource::<crate::ai::squad::SquadManager>()
                .unwrap();
            let squad = manager
                .squads
                .iter_mut()
                .find(|squad| squad.mission_type == crate::ai::squad::MissionType::Transport)
                .unwrap();
            let (entity, command) = crate::ai::squad::execute_transport_squad_step(
                world,
                squad,
                &std::collections::HashSet::new(),
            )
            .expect("transport should produce a command");
            let result = command.clone();
            world.insert_resource(manager);
            crate::ai::engine::execute_ai_command(world, entity, command);
            schedule.run(world);
            result
        };
        let reset_actions = |world: &mut World| {
            let mut query = world.query::<(&mut ActionCompleted, &mut HasMoved)>();
            for (mut action, mut moved) in query.iter_mut(world) {
                action.0 = false;
                moved.0 = false;
            }
        };

        assert!(matches!(
            execute_transport(&mut world, &mut schedule),
            AiCommand::Load { .. }
        ));
        crate::ai::squad::update_squads(&mut world, p1);
        assert_eq!(
            world.get::<CargoCapacity>(transport).unwrap().loaded.len(),
            1
        );
        reset_actions(&mut world);
        assert!(matches!(
            execute_transport(&mut world, &mut schedule),
            AiCommand::Load { .. }
        ));
        crate::ai::squad::update_squads(&mut world, p1);
        assert_eq!(
            world.get::<CargoCapacity>(transport).unwrap().loaded.len(),
            2
        );
        assert_eq!(
            world
                .resource::<crate::ai::squad::SquadManager>()
                .squads
                .iter()
                .find(|squad| squad.mission_type == crate::ai::squad::MissionType::Transport)
                .unwrap()
                .phase,
            crate::ai::squad::MissionPhase::Transport(crate::ai::squad::TransportPhase::Transit)
        );

        reset_actions(&mut world);
        assert!(matches!(
            execute_transport(&mut world, &mut schedule),
            AiCommand::Drop { .. }
        ));
        crate::ai::squad::update_squads(&mut world, p1);
        assert_eq!(
            world.get::<CargoCapacity>(transport).unwrap().loaded.len(),
            1
        );
        assert!(
            world
                .resource::<crate::ai::squad::SquadManager>()
                .squads
                .iter()
                .any(|squad| squad.mission_type == crate::ai::squad::MissionType::Capture)
        );

        reset_actions(&mut world);
        assert!(matches!(
            execute_transport(&mut world, &mut schedule),
            AiCommand::Drop { .. }
        ));
        crate::ai::squad::update_squads(&mut world, p1);
        let manager = world.resource::<crate::ai::squad::SquadManager>();
        let transport_squad = manager
            .squads
            .iter()
            .find(|squad| squad.mission_type == crate::ai::squad::MissionType::Transport)
            .unwrap();
        assert_eq!(
            transport_squad.phase,
            crate::ai::squad::MissionPhase::Transport(crate::ai::squad::TransportPhase::Return)
        );
        assert!(transport_squad.cargo_entities.is_empty());
        assert!(
            manager
                .squads
                .iter()
                .any(|squad| squad.mission_type == crate::ai::squad::MissionType::Attack)
        );
        assert!(world.get::<GridPosition>(capture).unwrap().x >= 3);
        assert!(world.get::<GridPosition>(combat).unwrap().x >= 3);
    }
}
