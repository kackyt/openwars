use crate::components::{GridPosition, PlayerId, Property};
use crate::events::*;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::*;
use crate::systems::*;
use bevy_ecs::prelude::*;

/// エンジンの World と Schedule を初期化し、必要なリソースやイベントを登録します。
pub fn create_world() -> (World, Schedule) {
    let mut world = World::new();
    let mut schedule = Schedule::default();

    // Register events
    world.init_resource::<Events<ProduceUnitCommand>>();
    world.init_resource::<ProductionDiagnostic>();
    world.init_resource::<Events<MoveUnitCommand>>();
    world.init_resource::<Events<AttackUnitCommand>>();
    world.init_resource::<Events<CapturePropertyCommand>>();
    world.init_resource::<Events<MergeUnitCommand>>();
    world.init_resource::<Events<SupplyUnitCommand>>();
    world.init_resource::<Events<LoadUnitCommand>>();
    world.init_resource::<Events<UnloadUnitCommand>>();
    world.init_resource::<Events<WaitUnitCommand>>();
    world.init_resource::<Events<NextPhaseCommand>>();
    world.init_resource::<Events<UndoMoveCommand>>();

    world.init_resource::<Events<UnitMovedEvent>>();
    world.init_resource::<Events<UnitAttackedEvent>>();
    world.init_resource::<Events<UnitDestroyedEvent>>();
    world.init_resource::<Events<UnitMergedEvent>>();
    world.init_resource::<Events<PropertyCapturedEvent>>();
    world.init_resource::<Events<GamePhaseChangedEvent>>();
    world.init_resource::<Events<GameOverEvent>>();

    // Add main game systems
    add_main_game_systems(&mut schedule);

    // Add event update system to be run at the end of each frame
    schedule.add_systems(update_all_events.after(crate::systems::GameSystemSet));

    (world, schedule)
}

/// 全イベントバッファをフレーム末尾で回転させるシステム。
/// 手動で Schedule を実行する場合、各ターンの末尾でこれを呼び出す必要があります。
pub fn update_all_events(world: &mut World) {
    world.resource_mut::<Events<ProduceUnitCommand>>().update();
    world.resource_mut::<Events<MoveUnitCommand>>().update();
    world.resource_mut::<Events<AttackUnitCommand>>().update();
    world
        .resource_mut::<Events<CapturePropertyCommand>>()
        .update();
    world.resource_mut::<Events<MergeUnitCommand>>().update();
    world.resource_mut::<Events<SupplyUnitCommand>>().update();
    world.resource_mut::<Events<LoadUnitCommand>>().update();
    world.resource_mut::<Events<UnloadUnitCommand>>().update();
    world.resource_mut::<Events<WaitUnitCommand>>().update();
    world.resource_mut::<Events<NextPhaseCommand>>().update();
    world.resource_mut::<Events<UndoMoveCommand>>().update();

    world.resource_mut::<Events<UnitMovedEvent>>().update();
    world.resource_mut::<Events<UnitAttackedEvent>>().update();
    world.resource_mut::<Events<UnitDestroyedEvent>>().update();
    world.resource_mut::<Events<UnitMergedEvent>>().update();
    world
        .resource_mut::<Events<PropertyCapturedEvent>>()
        .update();
    world
        .resource_mut::<Events<GamePhaseChangedEvent>>()
        .update();
    world.resource_mut::<Events<GameOverEvent>>().update();
}

/// マスターデータの設定中に発生するエラー。
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("Map '{0}' not found")]
    MapNotFound(String),
    #[error("Weapon '{0}' not found")]
    WeaponNotFound(String),
    #[error("Unit type for '{0}' not found")]
    UnitTypeNotFound(String),
    #[error("Master data error: {0}")]
    MasterData(String),
}

/// マスターデータからワールドを完全に初期化します。
pub fn initialize_world_from_master_data(
    master_data: &MasterDataRegistry,
    map_name: &str,
) -> Result<(World, Schedule), SetupError> {
    let (mut world, schedule) = create_world();

    // 1. DamageChart & UnitRegistry の構築
    let mut damage_chart = DamageChart::new();
    for (unit_name, unit_record) in &master_data.units {
        let att_type = master_data
            .unit_type_for_name(&unit_name.0)
            .map_err(|e| SetupError::UnitTypeNotFound(format!("{:?}", e)))?;

        if let Some(w1_name) = &unit_record.weapon1 {
            let weapon = master_data
                .weapons
                .get(&crate::resources::master_data::UnitName(w1_name.clone()))
                .ok_or_else(|| SetupError::WeaponNotFound(w1_name.clone()))?;
            for (def_name, dmg) in &weapon.damages {
                let def_type = master_data
                    .unit_type_for_name(def_name)
                    .map_err(|e| SetupError::UnitTypeNotFound(format!("{:?}", e)))?;
                damage_chart.insert_damage(att_type, def_type, *dmg);
            }
        }
        if let Some(w2_name) = &unit_record.weapon2 {
            let weapon = master_data
                .weapons
                .get(&crate::resources::master_data::UnitName(w2_name.clone()))
                .ok_or_else(|| SetupError::WeaponNotFound(w2_name.clone()))?;
            for (def_name, dmg) in &weapon.damages {
                let def_type = master_data
                    .unit_type_for_name(def_name)
                    .map_err(|e| SetupError::UnitTypeNotFound(format!("{:?}", e)))?;
                damage_chart.insert_secondary_damage(att_type, def_type, *dmg);
            }
        }
    }
    world.insert_resource(damage_chart);

    let mut unit_registry_map = std::collections::HashMap::new();
    for name in master_data.units.keys() {
        let stats = master_data
            .create_unit_stats(name)
            .map_err(|e| SetupError::MasterData(format!("{:?}", e)))?;
        unit_registry_map.insert(stats.unit_type, stats);
    }
    world.insert_resource(UnitRegistry(unit_registry_map));
    world.insert_resource(GameRng::default());

    // 2. マップとプレイヤーの構築
    let map_data = master_data
        .get_map(map_name)
        .ok_or_else(|| SetupError::MapNotFound(map_name.to_string()))?;

    let mut ecs_map = Map::new(
        map_data.width,
        map_data.height,
        Terrain::Plains,
        GridTopology::Square,
    );
    let mut players_set = std::collections::HashSet::new();

    for y in 0..map_data.height {
        for x in 0..map_data.width {
            if let Some(cell) = map_data.get_cell(x, y) {
                let terrain = master_data
                    .terrain_from_id(cell.terrain_id)
                    .map_err(|e| SetupError::MasterData(format!("{:?}", e)))?;
                let _ = ecs_map.set_terrain(x, y, terrain);

                if cell.player_id != 0 {
                    players_set.insert(cell.player_id);
                }

                let durability = master_data.landscape_durability(terrain.as_str());
                if durability > 0 {
                    let owner = if cell.player_id == 0 {
                        None
                    } else {
                        Some(PlayerId(cell.player_id))
                    };
                    world.spawn((
                        GridPosition { x, y },
                        Property::new(terrain, owner, durability),
                    ));
                }
            }
        }
    }
    world.insert_resource(ecs_map);

    let mut player_list = vec![];
    // P1, P2 は最低限保証
    players_set.insert(1);
    players_set.insert(2);

    for &pid in &players_set {
        let p = Player::new(pid, format!("Player {}", pid));
        player_list.push(p);
    }
    player_list.sort_by_key(|p| p.id.0);

    let player_count = player_list.len();
    world.insert_resource(Players(player_list));
    world.insert_resource(MatchState {
        current_turn_number: TurnNumber(0),
        active_player_index: PlayerIndex(player_count - 1),
        current_phase: Phase::EndTurn,
        game_over: None,
    });
    world.insert_resource(master_data.clone());

    // 最初（Turn 1, P1）のターン開始処理（資金付与・補給など）を直接実行します。
    crate::systems::turn_management::advance_next_phase(&mut world);

    Ok((world, schedule))
}
