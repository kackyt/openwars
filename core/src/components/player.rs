use bevy_ecs::prelude::*;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlayerId(pub u32);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Faction(pub PlayerId);
