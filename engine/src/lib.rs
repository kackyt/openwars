pub mod ai;
/// 決定的なハッシュ表（反復順をプロセス・スレッドに依存させない）。
pub mod collections;
pub mod components;
pub mod events;
pub mod resources;
pub mod serialize;
pub mod setup;
pub mod systems;

#[cfg(target_arch = "wasm32")]
pub mod wasm;
