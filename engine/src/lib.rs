pub mod ai;
pub mod components;
pub mod events;
pub mod resources;
pub mod serialize;
pub mod setup;
pub mod systems;

#[cfg(target_arch = "wasm32")]
pub mod wasm;
