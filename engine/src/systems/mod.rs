pub mod action;
pub mod combat;
pub mod merge;
pub mod movement;
pub mod production;
pub mod property;
pub mod supply;
pub mod transport;
pub mod turn_management;

pub use action::*;
pub use combat::*;
pub use merge::*;
pub use movement::*;
pub use production::*;
pub use property::*;
pub use supply::*;
pub use transport::*;
pub use turn_management::*;

use bevy_ecs::schedule::{IntoSystemConfigs, Schedule, SystemSet};

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct GameSystemSet;

/// すべてのゲームロジックシステムを正しい順序でスケジュールに追加します。
/// プレゼンテーション層はこの関数を呼び出すことで、ビジネスロジックの整合性を保つことができます。
pub fn add_main_game_systems(schedule: &mut Schedule) {
    schedule.configure_sets(GameSystemSet);
    schedule.add_systems(
        (
            crate::ai::engine::clear_ai_cooldowns_system,
            produce_unit_system,
            move_unit_system,
            attack_unit_system,
            sync_cargo_health_system,
            remove_destroyed_units_system,
            capture_property_system,
            merge_unit_system,
            supply_unit_system,
            load_unit_system,
            unload_unit_system,
            wait_unit_system,
            undo_move_system,
            next_phase_system,
            victory_check_system,
        )
            .chain()
            .in_set(GameSystemSet),
    );
}
