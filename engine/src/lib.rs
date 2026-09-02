pub mod ai;
// 標準マップはmain時点のAIを独立実装として保持し、小規模マップ用の拡張と混在させない。
// このモジュールの公開面は入口だけで、内部のmain互換実装は一括で到達する。
#[allow(dead_code, unused_imports)]
pub(crate) mod ai_standard;
pub mod components;
pub mod events;
pub mod resources;
pub mod serialize;
pub mod setup;
pub mod systems;

#[cfg(target_arch = "wasm32")]
pub mod wasm;
