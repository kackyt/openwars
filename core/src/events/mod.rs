use bevy_ecs::prelude::*;
use crate::components::PlayerId;

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
pub struct ProduceUnitCommand {
    pub player_id: PlayerId,
    pub target_x: usize,
    pub target_y: usize,
    pub unit_type: crate::resources::UnitType,
}

#[derive(Event, Debug, Clone)]
pub struct NextPhaseCommand;

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
}

#[derive(Event, Debug, Clone)]
pub struct UnitDestroyedEvent {
    pub entity: Entity,
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
