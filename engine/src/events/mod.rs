use crate::components::PlayerId;
use bevy_ecs::prelude::*;

#[derive(Event, Debug, Clone)]
pub struct MoveUnitCommand {
    pub unit_entity: Entity,
    pub target_x: usize,
    pub target_y: usize,
}

#[derive(Event, Debug, Clone)]
pub struct AttackUnitCommand {
    pub attacker_entity: Entity,
    pub defender_entity: Entity,
}

#[derive(Event, Debug, Clone)]
pub struct CapturePropertyCommand {
    pub unit_entity: Entity,
}

#[derive(Event, Debug, Clone)]
pub struct MergeUnitCommand {
    pub source_entity: Entity,
    pub target_entity: Entity,
}

#[derive(Event, Debug, Clone)]
pub struct ProduceUnitCommand {
    pub player_id: PlayerId,
    pub target_x: usize,
    pub target_y: usize,
    pub unit_type: crate::resources::UnitType,
}

#[derive(Event, Debug, Clone)]
pub struct WaitUnitCommand {
    pub unit_entity: Entity,
}

#[derive(Event, Debug, Clone)]
pub struct NextPhaseCommand;

#[derive(Event, Debug, Clone)]
pub struct UndoMoveCommand;

#[derive(Event, Debug, Clone)]
pub struct SupplyUnitCommand {
    pub supplier_entity: Entity,
    pub target_entity: Entity,
}

#[derive(Event, Debug, Clone)]
pub struct LoadUnitCommand {
    pub transport_entity: Entity,
    pub unit_entity: Entity,
}

#[derive(Event, Debug, Clone)]
pub struct UnloadUnitCommand {
    pub transport_entity: Entity,
    pub cargo_entity: Entity,
    pub target_x: usize,
    pub target_y: usize,
}

// Result Events (To notify UI or other systems)

#[derive(Event, Debug, Clone)]
pub struct UnitMovedEvent {
    pub entity: Entity,
    pub from: crate::components::GridPosition,
    pub to: crate::components::GridPosition,
    pub fuel_used: u32,
}

#[derive(Event, Debug, Clone)]
pub struct UnitAttackedEvent {
    pub attacker: Entity,
    pub defender: Entity,
    pub damage_dealt: u32,
    pub counter_damage_dealt: Option<u32>,
    pub attacker_hp_before: u32,
    pub attacker_hp_after: u32,
    pub defender_hp_before: u32,
    pub defender_hp_after: u32,
}

#[derive(Event, Debug, Clone)]
pub struct UnitDestroyedEvent {
    pub entity: Entity,
}

#[derive(Event, Debug, Clone)]
pub struct UnitMergedEvent {
    pub source_entity: Entity,
    pub target_entity: Entity,
    pub refunded_funds: u32,
}

#[derive(Event, Debug, Clone)]
pub struct PropertyCapturedEvent {
    pub x: usize,
    pub y: usize,
    pub new_owner: Option<PlayerId>,
}

#[derive(Event, Debug, Clone)]
pub struct GamePhaseChangedEvent {
    pub new_phase: crate::resources::Phase,
    pub active_player: PlayerId,
}

#[derive(Event, Debug, Clone)]
pub struct GameOverEvent {
    pub condition: crate::resources::GameOverCondition,
}

/// ユニット生産完了イベント
#[derive(Event, Debug, Clone)]
pub struct UnitProducedEvent {
    pub player_id: PlayerId,
    pub target_x: usize,
    pub target_y: usize,
    pub unit_type: crate::resources::UnitType,
    pub entity: Entity,
}

/// 補給完了イベント
#[derive(Event, Debug, Clone)]
pub struct UnitSuppliedEvent {
    pub supplier: Entity,
    pub target: Entity,
}

/// 輸送ユニットへの積載完了イベント
#[derive(Event, Debug, Clone)]
pub struct UnitLoadedEvent {
    pub transport: Entity,
    pub cargo: Entity,
}

/// 輸送ユニットからの降車完了イベント
#[derive(Event, Debug, Clone)]
pub struct UnitUnloadedEvent {
    pub transport: Entity,
    pub cargo: Entity,
    pub target_x: usize,
    pub target_y: usize,
}

/// ユニット待機完了イベント
#[derive(Event, Debug, Clone)]
pub struct UnitWaitedEvent {
    pub entity: Entity,
}

/// AIの思考評価・決定情報イベント
#[derive(Event, Debug, Clone)]
pub struct AiActionEvaluatedEvent {
    pub entity: Entity,
    pub mission_type: String,
    pub action_type: String,
    pub score: i32,
}
