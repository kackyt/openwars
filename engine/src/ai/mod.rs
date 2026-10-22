pub mod ai_version;
/// main互換実装と拡張実装の経路選択
pub mod code_path;
pub mod demand;
pub(crate) mod deterministic_parallel;
pub(crate) mod engagement;
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
pub mod operation_assignment;
pub mod planner;
pub mod production;
pub mod pruning;
#[cfg(test)]
pub mod scenario_tests;
pub mod strategy;
pub mod strategy_profile;
/// Waitの原因観測。判断には使わない診断専用データ。
pub mod wait_probe;

pub mod beam_search;
pub mod cluster;
pub(crate) mod runtime;
pub mod simulation;
pub mod squad;
pub(crate) mod tactics;
pub mod threat;
pub mod turn_distance;
/// V100/V200の独立した逐次AI。
pub(crate) mod v100;
/// V4: 作戦駆動生産（V1〜V3 の生産ロジックとは分離した独立実装）
pub mod v4;

pub use ai_version::{AiVersion, PlayerAiSettings, resolve_player_ai_version};

pub mod command;
pub(crate) mod context;
pub(crate) mod planning;
pub(crate) mod v1;

// 外部公開APIの正規経路。内部moduleは定義元を直接参照する。
pub use command::AiCommand;
pub use engine::execute_ai_turn;
pub use runtime::issue::execute_ai_command;
pub use runtime::production_queue::AiProductionCooldown;
pub use runtime::state::{AiActionCooldown, clear_ai_cooldowns_system};
#[cfg(test)]
mod decision_tests;
