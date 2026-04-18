use crate::components::player::PlayerId;
use bevy_ecs::prelude::*;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridPosition {
    pub x: usize,
    pub y: usize,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Property {
    pub terrain: crate::resources::Terrain,
    pub owner_id: Option<PlayerId>,
    pub capture_points: u32,
    pub max_capture_points: u32,
}

impl Property {
    pub fn new(
        terrain: crate::resources::Terrain,
        owner_id: Option<PlayerId>,
        max_capture_points: u32,
    ) -> Self {
        Self {
            terrain,
            owner_id,
            capture_points: max_capture_points,
            max_capture_points,
        }
    }

    pub fn display_capture_points(&self) -> u32 {
        self.capture_points / 10
    }

    pub fn display_max_capture_points(&self) -> u32 {
        self.max_capture_points / 10
    }
}
