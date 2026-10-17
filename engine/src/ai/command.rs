//! AI判断の出力コマンド型。選択・適用の各層に依存しない。
use crate::components::GridPosition;
use bevy_ecs::entity::Entity;

#[derive(Debug, Clone)]
pub enum AiCommand {
    Attack {
        target_pos: GridPosition,
        target_entity: Entity,
    },
    Capture {
        target_pos: GridPosition,
    },
    Wait {
        target_pos: GridPosition,
    },
    Merge {
        target_pos: GridPosition,
        target_entity: Entity,
    },
    Load {
        target_pos: GridPosition,
        transport_entity: Entity,
    },
    Drop {
        transport_target_pos: GridPosition,
        cargo_drop_pos: GridPosition,
        cargo_entity: Entity,
    },
    Supply {
        target_pos: GridPosition,
        target_entity: Entity,
    },
}
