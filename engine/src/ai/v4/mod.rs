//! V4: 作戦駆動生産 AI。
//!
//! V1〜V3 の生産ロジック（`GamePhase` ごとの理想構成をハードコードし、
//! 構成比の差分で買うものを決める方式）とは完全に分離した独立モジュール。
//!
//! 基本方針:
//! 1. 盤面から「作戦（Operation）」＝獲る／守るべき拠点のまとまりを切り出す
//! 2. 各作戦について、観測量だけから 5 つの枠を逆算する
//!    （占領枠・撃破枠・護衛枠・輸送枠・迎撃枠）
//! 3. 空いている生産枠を、最も不足している枠から順に埋める
//!
//! 「敵を減らす」と「占領する」は別フェーズではなく同一作戦の別枠として
//! 同時に立つため、倒してから占領するのではなく並行して進む。

use crate::ai::v4::board_scan::BoardScan;
use crate::ai::v4::property_control::PlannedCaptureUnit;
use crate::ai::v4::property_control::PropertyControlObjective;

pub mod campaign_execution;
mod candidate;
mod combat_input;
mod combat_profile;
pub mod deployment;
pub mod logistics_plan;
pub(crate) mod maritime;
mod mission_input;
pub mod operation;
mod operation_builder;
mod operation_selection;
pub mod plan_revision;
mod production_cancellation;
mod production_entry;
pub(crate) mod production_events;
mod production_finalize;
mod production_model;
mod production_pipeline;
pub mod rolling_plan;
mod slot_demand;
pub mod trace;
pub mod victory_roadmap;
use crate::ai::planning::decision::AdoptedTurnPortfolio;
use crate::ai::production::allocation::AllocationState;
use crate::ai::production::commit::{self, PurchasePurpose};
use crate::ai::production::demand::{DemandLedger, EffectiveDemandReason, UnfilledReason};
use crate::ai::production::plan_campaign_with_expansion_denial_reserve;
use crate::ai::production::turn_state::ProductionTurnState;
use crate::ai::squad::{MissionType, SquadId};
use crate::ai::turn_distance::{
    ActionTurnDistanceCache, TerrainConnectivity, calculate_action_distance_to_range,
    calculate_action_distance_to_range_after_leaving_start,
};
use crate::collections::{HashMap, HashSet};
use crate::components::{
    CargoCapacity, Faction, GridPosition, Health, PlayerId, Property, Transporting, UnitStats,
};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{
    DamageChart, Map, MatchState, MovementType, Players, Terrain, UnitRegistry, UnitType,
};
use crate::systems::movement::{
    OccupantInfo, calculate_reachable_tile_costs, get_valid_movement_cost,
};
use crate::systems::transport::can_unload_from_terrain;
use bevy_ecs::prelude::*;
use operation::{
    AcquisitionMode, MAX_CAPTURE_SLOTS, OperationFacts, OperationKind, OperationSlots,
    RESERVATION_PATIENCE_TURNS, SLOT_PRIORITY, SlotKind, SlotTier, acquisition_mode, derive_slots,
};
use operation_selection::{OperationCandidate, select_operation_candidates};
use plan_revision::{
    ActivePlanObjective, DeploymentExecutionObservation, PlanDisposition, PlanStepRef,
    SelectedPlan, V4RollingPlanRegistry,
};
use rolling_plan::{
    DEFAULT_SEARCH_TURNS, EnemyPlanUnit, FriendlyPlanUnit, OpeningProductionPolicy,
    ProductionAttackProjection, RollingPlanInput, evaluate_fixed_package, plan_force_package,
    production_options,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use trace::{
    CampaignTurnForecastTrace, EnemyProductionForecastTrace, ProductionDecision,
    ProductionDemandTrace, ProductionOperationTrace, ProductionPlanTrace, ProductionStepTrace,
    ProductionTraceDiagnostics, ReinforcementContingencyTrace, RollingCombatPlanTrace,
    RollingPurchaseTrace, RollingTargetTrace, UnfilledSlotTrace,
};

use candidate::*;
use operation_builder::*;
pub use production_entry::decide_production_v4;
use production_model::*;

use slot_demand::*;

#[cfg(test)]
mod tests;

mod assignment;
pub(crate) mod home_defense;
pub mod plan_contract;
pub mod property_control;
pub(crate) mod strategy_pipeline;

// 盤面snapshot、首都DAG、敵予測は担当moduleが所有し、生産方策は入口で選ぶ。
pub(crate) mod board_scan;
#[allow(unused_imports)]
pub(crate) mod capital_route;
mod enemy_forecast;
mod production_policy;
use production_policy::ProductionPolicy;
#[cfg(test)]
mod production_tests;
#[cfg(test)]
mod test_support;

pub use capital_route::commitment::capital_route_diagnostics_for_player;

#[cfg(test)]
use production_pipeline::plan_production;
