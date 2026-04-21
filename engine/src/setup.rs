use crate::events::*;
use crate::systems::*;
use bevy_ecs::prelude::*;

/// エンジンの World と Schedule を初期化し、必要なリソースやイベントを登録します。
pub fn create_world() -> (World, Schedule) {
    let mut world = World::new();
    let mut schedule = Schedule::default();

    // Register events
    world.init_resource::<Events<ProduceUnitCommand>>();
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

    (world, schedule)
}
