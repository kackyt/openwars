use bevy_ecs::prelude::*;
use uuid::Uuid;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnitId(pub Uuid);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridPosition {
    pub x: usize,
    pub y: usize,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlayerId(pub u32);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Faction(pub PlayerId);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Health {
    pub current: u32,
    pub max: u32,
}

impl Health {
    pub fn new(max: u32) -> Self {
        Self { current: max, max }
    }
    pub fn damage(&mut self, amount: u32) {
        self.current = self.current.saturating_sub(amount);
    }
    pub fn is_destroyed(&self) -> bool {
        self.current == 0
    }
    pub fn get_display_hp(&self) -> u32 {
        (self.current.saturating_add(9)) / 10
    }
}

#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct UnitStats {
    pub unit_type: crate::resources::UnitType,
    pub cost: u32,
    pub max_movement: u32,
    pub movement_type: crate::resources::MovementType,
    pub max_fuel: u32,
    pub max_ammo1: u32,
    pub max_ammo2: u32,
    pub min_range: u32,
    pub max_range: u32,
    pub daily_fuel_consumption: u32,
    pub can_capture: bool,
    pub can_supply: bool,
    pub max_cargo: u32,
    pub loadable_unit_types: Vec<crate::resources::UnitType>,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fuel {
    pub current: u32,
    pub max: u32,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ammo {
    pub ammo1: u32,
    pub max_ammo1: u32,
    pub ammo2: u32,
    pub max_ammo2: u32,
}

#[derive(Component, Debug, Clone)]
pub struct CargoCapacity {
    pub max: u32,
    // Note: In Bevy ECS, `Entity` IS the unique ID for an entity.
    // It acts as a lightweight 64-bit identifier (generation + index).
    // Using `Entity` here satisfies the ID-based reference rule without
    // fighting the ECS framework's native relation mechanisms.
    pub loaded: Vec<Entity>,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transporting(pub Entity); // Reference by Entity ID


#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HasMoved(pub bool);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActionCompleted(pub bool);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Property {
    pub terrain: crate::resources::Terrain,
    pub owner_id: Option<PlayerId>,
    pub capture_points: u32,
}

impl Property {
    pub fn new(terrain: crate::resources::Terrain, owner_id: Option<PlayerId>) -> Self {
        Self {
            terrain,
            owner_id,
            capture_points: terrain.max_capture_points(),
        }
    }
}
