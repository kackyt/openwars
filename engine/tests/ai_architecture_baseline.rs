use bevy_ecs::prelude::*;
use engine::ai::ai_version::*;
use engine::ai::execute_ai_turn;
use engine::ai::strategy_profile::{V4StrategyProfile, profile_for};
use engine::components::*;
use engine::resources::master_data::*;
use engine::resources::*;

// 共通のテストセットアップ
fn setup_test_world(map_name: &str) -> World {
    let master_data = MasterDataRegistry::load().expect("Failed to load master data");

    // 既存のセットアップ関数でワールド全体を初期化
    let (mut world, _) = engine::setup::initialize_world_from_master_data(&master_data, map_name)
        .expect("Failed to initialize world");

    // 既存のユニットとプロパティをすべて削除して、クリーンな環境にする
    let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
    for e in entities {
        if world.get::<GridPosition>(e).is_some() || world.get::<Property>(e).is_some() {
            world.despawn(e);
        }
    }

    // 共通のテストシナリオを配置
    let p1 = PlayerId(1);
    world.spawn((
        GridPosition { x: 5, y: 5 },
        Property::new(Terrain::Capital, Some(p1), 100),
    ));
    world.spawn((
        GridPosition { x: 6, y: 5 },
        Property::new(Terrain::City, None, 100), // 占領可能物件
    ));
    world.spawn((
        Faction(p1),
        HasMoved(false),
        ActionCompleted(false),
        UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1000,
            max_movement: 3,
            movement_type: MovementType::Infantry,
            max_fuel: 99,
            max_ammo1: 10,
            ammo1_cost: 0,
            max_ammo2: 0,
            ammo2_cost: 0,
            min_range: 1,
            max_range: 1,
            daily_fuel_consumption: 0,
            can_capture: true,
            can_supply: false,
            max_cargo: 0,
            loadable_unit_types: vec![],
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
            ammo1: 10,
            ammo2: 0,
            max_ammo1: 10,
            max_ammo2: 0,
        },
    ));

    world
}

#[test]
fn test_standard_v4_ai_behavior() {
    let p1 = PlayerId(1);
    let mut world = setup_test_world("map_6");

    // AIバージョンをV4に設定
    let mut ai_settings = PlayerAiSettings::default();
    ai_settings.set_version(p1, AiVersion::V4);
    world.insert_resource(ai_settings);
    assert_eq!(profile_for(&mut world, p1), V4StrategyProfile::Standard);

    // 最初のAIステップを実行
    let action = execute_ai_turn(&mut world, p1);

    // 統合前の標準V4が選んだ最初の行動を固定する。
    assert_eq!(
        action,
        Some("Capture { target_pos: GridPosition { x: 6, y: 5 } }".to_string())
    );
}

#[test]
fn test_small_map_v4_ai_behavior() {
    let p1 = PlayerId(1);
    let mut world = setup_test_world("map_1");

    // AIバージョンをV4に設定
    let mut ai_settings = PlayerAiSettings::default();
    ai_settings.set_version(p1, AiVersion::V4);
    world.insert_resource(ai_settings);
    assert_eq!(
        profile_for(&mut world, p1),
        V4StrategyProfile::SmallMapExpansion
    );

    // 最初のAIステップを実行
    let action = execute_ai_turn(&mut world, p1);

    // 統合前の小規模V4が選んだ最初の行動を固定する。
    assert_eq!(
        action,
        Some("Wait { target_pos: GridPosition { x: 6, y: 4 } }".to_string())
    );
}

#[test]
fn standard_v4_does_not_fill_idle_factories_with_unplanned_reinforcements() {
    let master_data = MasterDataRegistry::load().expect("マスターデータの読み込みに失敗");
    let (mut world, _) = engine::setup::initialize_world_from_master_data(&master_data, "map_6")
        .expect("ワールド初期化に失敗");
    let player = PlayerId(1);
    let mut settings = PlayerAiSettings::default();
    settings.set_version(player, AiVersion::V4);
    world.insert_resource(settings);
    assert_eq!(profile_for(&mut world, player), V4StrategyProfile::Standard);

    // 標準マップでは空き工場を埋めるだけの動員で、将来の戦闘編成資金を使わない。
    assert!(execute_ai_turn(&mut world, player).is_some());
    let diagnostics = world.resource::<engine::ai::v4::trace::ProductionTraceDiagnostics>();
    let trace = diagnostics
        .by_player
        .get(&player)
        .expect("標準V4の生産計画");
    assert!(!trace.steps.is_empty());
    assert!(trace.steps.iter().all(|step| {
        !matches!(
            step.decision,
            engine::ai::v4::trace::ProductionDecision::ProducedImmediateReinforcement { .. }
        )
    }));
}
