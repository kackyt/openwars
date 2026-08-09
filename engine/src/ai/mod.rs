pub mod ai_version;
pub mod demand;
pub mod emergency;
pub mod engine;
pub mod eval;
/// 遊兵（任務なし・任務があるのに動けないユニット）の計測
pub mod idle_audit;
pub mod island_campaign;
pub mod island_campaign_analysis;
#[cfg(test)]
mod island_campaign_tests;
#[cfg(test)]
mod island_invasion_tests;
pub mod islands;
pub mod missions;
pub mod objectives;
pub mod planner;
pub mod production;
pub mod pruning;
#[cfg(test)]
pub mod scenario_tests;
pub mod strategy;

pub mod beam_search;
pub mod cluster;
pub mod simulation;
pub mod squad;
pub mod threat;
pub mod turn_distance;
/// V4: 作戦駆動生産（V1〜V3 の生産ロジックとは分離した独立実装）
pub mod v4;

pub use ai_version::{AiVersion, PlayerAiSettings, resolve_player_ai_version};
