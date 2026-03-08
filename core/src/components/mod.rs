use bevy_ecs::prelude::*;
use uuid::Uuid;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnitId(pub Uuid);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridPosition {
    pub x: usize,
    pub y: usize,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Faction(pub u32); // player_id

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

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cargo {
    // Array to store entity IDs of loaded units, up to some max or dynamically via vector, but ECS Components should ideally be cheap.
    // However, Vec is fine for now, or we can use fixed arrays for cargo. Let's use Vec for simplicity as max_cargo is usually 1-2.
}

#[derive(Component, Debug, Clone)]
pub struct CargoCapacity {
    pub max: u32,
    pub loaded: Vec<Entity>,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transporting(pub Entity); // Reference to the transport unit if currently loaded

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HasMoved(pub bool);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActionCompleted(pub bool);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Property {
    pub terrain: crate::resources::Terrain,
    pub owner_id: Option<u32>,
    pub capture_points: u32,
}

impl Property {
    pub fn new(terrain: crate::resources::Terrain, owner_id: Option<u32>) -> Self {
        Self {
            terrain,
            owner_id,
            capture_points: terrain.max_capture_points(),
        }
    }
}

#[derive(Resource, Debug, Clone, Default)]
pub struct UnitRegistry {
    pub units: std::collections::HashMap<crate::resources::UnitType, UnitStats>,
}
