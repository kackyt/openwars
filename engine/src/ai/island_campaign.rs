use crate::ai::islands::IslandId;
use crate::ai::squad::TransportPhase;
use crate::components::{GridPosition, PlayerId};
use crate::resources::{Map, UnitType};
use bevy_ecs::prelude::{Entity, Resource};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

/// 戦力分散を避けるために同時維持する攻勢作戦数。
const MAX_ACTIVE_OFFENSIVES: usize = 3;

/// これ以上の輸送ETAを持つ敵島は、直行前に前進兵站拠点を必要とする長距離目標とみなす。
const LONG_RANGE_ASSAULT_ETA: u32 = 4;
/// 輸送役が既に前進してETAが短く見えても、補給線が必要とみなす首都からの距離。
const LONG_RANGE_ASSAULT_DISTANCE: u32 = 12;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CampaignSustainmentCoverage {
    pub(crate) ground: bool,
    pub(crate) air: bool,
    pub(crate) sea: bool,
}

/// 兵站作戦が島全体ではなく、補給カテゴリに対応する施設を直接狙うための候補座標。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CampaignSustainmentTargets {
    pub(crate) ground: Option<GridPosition>,
    pub(crate) air: Option<GridPosition>,
    pub(crate) sea: Option<GridPosition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IslandCampaignState {
    Ignored,
    OpenNeutral,
    Secured,
    Threatened,
    Contested,
    EnemyHeld,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IslandCampaignDecision {
    Observe,
    Expand,
    Secure,
    Defend,
    Contest,
    Reinforce,
    Withdraw,
    Assault,
}

/// その分析呼び出しで作戦を停止した制御上の理由。診断文言とは分離して扱う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IslandCampaignPauseCause {
    DefensePreemption,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignFacts {
    pub island_id: IslandId,
    pub capturable_properties: u32,
    pub strategic_production_sites: u32,
    pub roi_production_sites: u32,
    pub neutral_properties: u32,
    pub friendly_properties: u32,
    pub enemy_properties: u32,
    pub friendly_units: u32,
    pub enemy_units: u32,
    pub friendly_combat_units: u32,
    pub enemy_combat_units: u32,
    pub friendly_arrival_eta: Option<u32>,
    pub enemy_arrival_eta: Option<u32>,
    pub friendly_capture_eta: Option<u32>,
    pub enemy_capture_eta: Option<u32>,
    pub transport_eta: Option<u32>,
    pub capture_turns: u32,
    pub island_income_per_turn: u32,
    pub missing_expansion_package_cost: u32,
    pub reachable: bool,
    pub has_unowned_properties: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignAssessment {
    pub island_id: IslandId,
    pub state: IslandCampaignState,
    pub decision: IslandCampaignDecision,
    pub state_reason: String,
    pub decision_reason: String,
    pub pause_cause: Option<IslandCampaignPauseCause>,
    pub neutral_properties: u32,
    pub friendly_properties: u32,
    pub enemy_properties: u32,
    pub friendly_combat_units: u32,
    pub enemy_combat_units: u32,
    pub friendly_arrival_eta: Option<u32>,
    pub enemy_arrival_eta: Option<u32>,
    pub friendly_capture_eta: Option<u32>,
    pub enemy_capture_eta: Option<u32>,
    pub roi_production_sites: u32,
    pub transport_eta: Option<u32>,
    pub expansion_payback_turns: Option<u32>,
    pub required_budget: u32,
    pub allocated_budget: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignRequirement {
    pub preferred_transport: Option<UnitType>,
    pub transport_slots: u32,
    pub capture_units: u32,
    /// 敵領へ一度に揚陸させる地上戦闘unit数。
    pub ground_combat_units: u32,
    /// 作戦へ追加で割り当てる戦闘Entity数。戦闘能力や価格の代用品ではない。
    pub combat_units: u32,
    pub total_budget: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignAssignment {
    pub island_id: IslandId,
    pub decision: IslandCampaignDecision,
    /// 作戦の主目標。輸送・戦闘部隊の進出基準として使う。
    pub target_position: GridPosition,
    /// 占領要員ごとに分担できる、優先順付きの未所有施設目標。
    pub capture_target_positions: Vec<GridPosition>,
    /// 現在の波が優先して処理する敵兵種。
    pub priority_enemy_types: Vec<UnitType>,
    pub requirement: IslandCampaignRequirement,
    pub purchase_shortfall: IslandCampaignRequirement,
    pub allocated_budget: u32,
    pub transport_entities: Vec<Entity>,
    pub capture_entities: Vec<Entity>,
    pub combat_entities: Vec<Entity>,
    pub operation_ready: bool,
    pub continued_from_existing_squad: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandCampaignShortfall {
    pub island_id: IslandId,
    pub decision: IslandCampaignDecision,
    /// 不足戦力を投入する作戦地点。生産施設から自力到達できない地上戦力を
    /// 「将来輸送できるはず」と見なして遊兵化させないために使う。
    pub target_position: GridPosition,
    pub light_transport_slots: u32,
    pub heavy_transport_slots: u32,
    pub capture_units: u32,
    pub ground_combat_units: u32,
    /// まだ実Entityまたは生産計画へ接続できていない戦闘Entity数。
    pub combat_units: u32,
    pub priority_enemy_types: Vec<UnitType>,
    pub reserved_budget: u32,
    pub priority_rank: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IslandCampaignPortfolio {
    pub islands: Vec<IslandCampaignAssessment>,
    pub active_offensives: Vec<IslandCampaignAssignment>,
    pub defenses: Vec<IslandCampaignAssignment>,
}

/// V3が最後に再計算した島嶼キャンペーンをプレイヤー別に保持する診断専用Resource。
/// AI判断からは参照せず、作戦の永続ライフサイクル状態としても使用しない。
#[derive(Resource, Debug, Clone, Default)]
pub struct IslandCampaignDiagnostics {
    pub by_player: HashMap<PlayerId, IslandCampaignPortfolio>,
}

impl IslandCampaignPortfolio {
    pub fn assignment_for(&self, island_id: IslandId) -> Option<&IslandCampaignAssignment> {
        self.defenses
            .iter()
            .chain(self.active_offensives.iter())
            .find(|assignment| assignment.island_id == island_id)
    }

    pub fn offensive_target_positions(&self) -> Vec<GridPosition> {
        let mut positions = Vec::new();
        for assignment in &self.active_offensives {
            if !positions.contains(&assignment.target_position) {
                positions.push(assignment.target_position);
            }
        }
        positions
    }

    pub fn aggregate_missing_requirements(&self) -> Vec<IslandCampaignShortfall> {
        let mut shortfalls = Vec::new();
        for assignment in self.defenses.iter().chain(self.active_offensives.iter()) {
            let missing = &assignment.purchase_shortfall;
            // 出航可能になった強襲の戦闘支援は汎用戦闘作戦へ戻す。実行中の便へ
            // 後からcargoを追加できないため、専用生産を継続すると任務なし兵が増える。
            let combat_units = if assignment.decision == IslandCampaignDecision::Assault
                && assignment.operation_ready
            {
                0
            } else {
                missing.combat_units
            };
            let reserved_budget = missing.total_budget;
            if reserved_budget == 0
                && missing.transport_slots == 0
                && missing.capture_units == 0
                && missing.ground_combat_units == 0
                && combat_units == 0
            {
                continue;
            }
            // 既存Assaultの完成待ちで、残存施設のSecureや進行中の島争奪を
            // 飢餓させない。作戦種別を先に比較し、同種内だけ継続作戦を優先する。
            let existing_offset = u8::from(!assignment.continued_from_existing_squad);
            let priority_rank = match assignment.decision {
                IslandCampaignDecision::Defend => 0,
                IslandCampaignDecision::Secure => 1 + existing_offset,
                IslandCampaignDecision::Contest | IslandCampaignDecision::Reinforce => {
                    3 + existing_offset
                }
                IslandCampaignDecision::Expand => 5 + existing_offset,
                IslandCampaignDecision::Assault => 7 + existing_offset,
                _ => continue,
            };
            let (light_transport_slots, heavy_transport_slots) = match missing.preferred_transport {
                Some(UnitType::TransportHelicopter) => (missing.transport_slots, 0),
                Some(UnitType::Lander)
                    if assignment.decision == IslandCampaignDecision::Assault =>
                {
                    // AssaultはLander不足を先頭2枠、残りを同時必須の輸送ヘリ不足として表す。
                    let heavy = missing.transport_slots.min(2);
                    (missing.transport_slots.saturating_sub(heavy), heavy)
                }
                Some(UnitType::Lander) => (0, missing.transport_slots),
                _ => (0, 0),
            };
            shortfalls.push(IslandCampaignShortfall {
                island_id: assignment.island_id,
                decision: assignment.decision,
                target_position: assignment.target_position,
                light_transport_slots,
                heavy_transport_slots,
                capture_units: missing.capture_units,
                ground_combat_units: missing.ground_combat_units,
                combat_units,
                priority_enemy_types: assignment.priority_enemy_types.clone(),
                reserved_budget,
                priority_rank,
            });
        }
        shortfalls.sort_by_key(|shortfall| (shortfall.priority_rank, shortfall.island_id.0));
        shortfalls
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CampaignUnitCandidate {
    pub(crate) entity: Entity,
    pub(crate) unit_type: UnitType,
    pub(crate) cost: u32,
    pub(crate) can_capture: bool,
    pub(crate) can_secure_local_property: bool,
    pub(crate) available_cargo_slots: u32,
    pub(crate) loaded_cargo_entities: Vec<Entity>,
    pub(crate) loadable_unit_types: Vec<UnitType>,
    pub(crate) is_transporting: bool,
    pub(crate) reachable_positions: Vec<GridPosition>,
    pub(crate) island_id: Option<IslandId>,
    pub(crate) assigned_island: Option<IslandId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CampaignResourcePool {
    pub(crate) available_funds: u32,
    pub(crate) units: Vec<CampaignUnitCandidate>,
}

/// キャンペーンが不足輸送を生産要求へ変換するための、実際に生産可能な輸送諸元。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CampaignTransportBlueprint {
    pub(crate) unit_type: UnitType,
    pub(crate) cost: u32,
    pub(crate) cargo_slots: u32,
    pub(crate) loadable_unit_types: Vec<UnitType>,
    pub(crate) source_island: IslandId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IslandCampaignCandidate {
    pub(crate) assessment: IslandCampaignAssessment,
    pub(crate) target_position: GridPosition,
    pub(crate) capture_target_positions: Vec<GridPosition>,
    pub(crate) priority_enemy_types: Vec<UnitType>,
    pub(crate) roi_production_sites: u32,
    pub(crate) transport_eta: Option<u32>,
    /// 所有するとターン開始補給・修理に使える地上／航空／海上拠点の数。
    pub(crate) ground_sustainment_sites: u32,
    pub(crate) air_sustainment_sites: u32,
    pub(crate) sea_sustainment_sites: u32,
    pub(crate) sustainment_targets: CampaignSustainmentTargets,
    pub(crate) island_income_per_turn: u32,
    /// 最終攻勢を始める前に確保すべき、収入と継戦能力を担う橋頭堡。
    pub(crate) logistics_prerequisite: bool,
    /// 永続兵站経路内の工程順。単にROIが高い別島が先行して中核工程を遅らせない。
    pub(crate) logistics_priority_rank: Option<u32>,
    pub(crate) requirement: IslandCampaignRequirement,
    /// Assaultで必須とする輸送役の構成。生産圏内の実拠点から組める編成だけを保持する。
    pub(crate) assault_transport_types: Vec<UnitType>,
    /// 自軍拠点で実際に生産できる輸送手段。Reinforceの不足を盤面依存で導出する。
    pub(crate) producible_transports: Vec<CampaignTransportBlueprint>,
    pub(crate) existing_operation: Option<ExistingCampaignOperation>,
}

/// 永続状態を追加せず、現在も生存しているSquadのIDと目標だけから作戦継続情報を復元する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExistingCampaignOperation {
    pub island_id: IslandId,
    pub target_position: GridPosition,
    pub transport_phase: Option<TransportPhase>,
    pub is_forming: bool,
    pub transport_entities: Vec<Entity>,
    pub capture_entities: Vec<Entity>,
    pub combat_entities: Vec<Entity>,
}

pub(crate) fn campaign_unit_type_rank(unit_type: UnitType) -> u8 {
    match unit_type {
        UnitType::Infantry => 0,
        UnitType::Mech => 1,
        UnitType::Recon => 2,
        UnitType::Tank => 3,
        UnitType::MdTank => 4,
        UnitType::TankZ => 5,
        UnitType::Artillery => 6,
        UnitType::LightSpGun => 7,
        UnitType::HeavySpGun => 8,
        UnitType::Rockets => 9,
        UnitType::AntiAir => 10,
        UnitType::Missiles => 11,
        UnitType::Fighter => 12,
        UnitType::HeavyFighter => 13,
        UnitType::Bomber => 14,
        UnitType::Bcopters => 15,
        UnitType::TransportHelicopter => 16,
        UnitType::Battleship => 17,
        UnitType::Carrier => 18,
        UnitType::Lander => 19,
        UnitType::SupplyTruck => 20,
    }
}

fn is_live_campaign_transport_phase(phase: Option<TransportPhase>) -> bool {
    phase.is_some_and(|phase| {
        matches!(
            phase,
            TransportPhase::Pickup | TransportPhase::Transit | TransportPhase::Drop
        )
    })
}

fn is_offshore_transport(unit_type: UnitType) -> bool {
    matches!(unit_type, UnitType::TransportHelicopter | UnitType::Lander)
}

fn is_campaign_support_unit(unit_type: UnitType) -> bool {
    matches!(
        unit_type,
        UnitType::TransportHelicopter | UnitType::Lander | UnitType::SupplyTruck
    )
}

type OffensivePriorityKey = (
    u8,
    u8,
    u32,
    u32,
    Reverse<u32>,
    Reverse<u32>,
    u32,
    u32,
    u32,
    u32,
    usize,
);

/// decisionごとの固定keyへ正規化し、HashMapや入力順に依存しない攻勢優先順位を作る。
fn offensive_priority_key(candidate: &IslandCampaignCandidate) -> OffensivePriorityKey {
    // 進行中作戦は同時攻勢上限より先に枠を確保する。毎ターン再評価されるdecisionを
    // 先にすると、新規候補3件だけで上限が埋まり、占領途中の島作戦が消えてしまう。
    let continuity_rank = if candidate.logistics_prerequisite
        || candidate.assessment.decision == IslandCampaignDecision::Secure
    {
        // 自島・確保済み島の未所有施設は、新規資源を使って短く閉じる。
        0
    } else if candidate.existing_operation.is_some() {
        1
    } else {
        2
    };
    let logistics_rank = candidate.logistics_priority_rank.unwrap_or(u32::MAX);
    let decision_rank = match candidate.assessment.decision {
        _ if candidate.logistics_prerequisite => 0,
        IslandCampaignDecision::Secure => 1,
        IslandCampaignDecision::Contest => 2,
        IslandCampaignDecision::Reinforce => 3,
        IslandCampaignDecision::Expand => 4,
        IslandCampaignDecision::Assault => 5,
        _ => u8::MAX,
    };
    match candidate.assessment.decision {
        IslandCampaignDecision::Expand | IslandCampaignDecision::Secure => (
            continuity_rank,
            decision_rank,
            logistics_rank,
            candidate
                .assessment
                .expansion_payback_turns
                .unwrap_or(u32::MAX),
            Reverse(candidate.roi_production_sites),
            Reverse(candidate.assessment.neutral_properties),
            candidate.transport_eta.unwrap_or(u32::MAX),
            0,
            0,
            0,
            candidate.assessment.island_id.0,
        ),
        IslandCampaignDecision::Contest | IslandCampaignDecision::Reinforce => (
            continuity_rank,
            decision_rank,
            logistics_rank,
            0,
            Reverse(0),
            Reverse(0),
            candidate
                .assessment
                .friendly_capture_eta
                .unwrap_or(u32::MAX),
            candidate.assessment.enemy_combat_units,
            0,
            0,
            candidate.assessment.island_id.0,
        ),
        IslandCampaignDecision::Assault => (
            continuity_rank,
            decision_rank,
            logistics_rank,
            0,
            Reverse(candidate.roi_production_sites),
            Reverse(candidate.assessment.enemy_properties),
            candidate.transport_eta.unwrap_or(u32::MAX),
            candidate.assessment.required_budget,
            candidate.assessment.enemy_combat_units,
            0,
            candidate.assessment.island_id.0,
        ),
        _ => (
            continuity_rank,
            decision_rank,
            logistics_rank,
            0,
            Reverse(0),
            Reverse(0),
            0,
            0,
            0,
            0,
            candidate.assessment.island_id.0,
        ),
    }
}

pub(crate) fn preferred_logistics_target(
    candidate: &IslandCampaignCandidate,
    assault: &IslandCampaignCandidate,
    coverage: CampaignSustainmentCoverage,
) -> Option<GridPosition> {
    // 作戦に使う輸送役の補給施設を先に確保し、その後で地上兵の修理拠点を確保する。
    // 同じカテゴリ内の座標は分析側で決定的に選択済みである。
    for unit_type in &assault.assault_transport_types {
        match unit_type {
            UnitType::TransportHelicopter if !coverage.air => {
                if let Some(target) = candidate.sustainment_targets.air {
                    return Some(target);
                }
            }
            UnitType::Lander if !coverage.sea => {
                if let Some(target) = candidate.sustainment_targets.sea {
                    return Some(target);
                }
            }
            _ => {}
        }
    }
    (!coverage.ground)
        .then_some(candidate.sustainment_targets.ground)
        .flatten()
}

/// 候補島が地上兵と強襲輸送役に提供できる補給カテゴリ数を返す。
/// 強襲で実際に使う輸送種別だけを数え、同じ輸送種別の重複要求は1カテゴリにまとめる。
fn logistics_coverage(
    candidate: &IslandCampaignCandidate,
    assault: &IslandCampaignCandidate,
) -> u32 {
    let mut coverage = u32::from(candidate.ground_sustainment_sites > 0);
    let mut required_transport_types = assault.assault_transport_types.clone();
    required_transport_types.sort_by_key(|unit_type| campaign_unit_type_rank(*unit_type));
    required_transport_types.dedup();
    for unit_type in required_transport_types {
        coverage = coverage.saturating_add(match unit_type {
            UnitType::TransportHelicopter => u32::from(candidate.air_sustainment_sites > 0),
            UnitType::Lander => u32::from(candidate.sea_sustainment_sites > 0),
            _ => 0,
        });
    }
    coverage
}

fn sustainment_network_covers_assault(
    coverage: CampaignSustainmentCoverage,
    assault: &IslandCampaignCandidate,
) -> bool {
    if !coverage.ground {
        return false;
    }
    assault
        .assault_transport_types
        .iter()
        .all(|unit_type| match unit_type {
            UnitType::TransportHelicopter => coverage.air,
            UnitType::Lander => coverage.sea,
            _ => true,
        })
}

/// 収入競争または長距離強襲に備える必要があるとき、最も有効な中間島を兵站前提へ昇格する。
/// マップ名ではなく、収入差・既存の前進拠点・輸送ETA・補給種別・目標距離だけを使う。
pub(crate) fn promote_logistics_prerequisite(
    candidates: &mut [IslandCampaignCandidate],
    map: &Map,
    friendly_income_per_turn: u32,
    enemy_income_per_turn: u32,
    forward_sustainment_coverage: CampaignSustainmentCoverage,
    home_position: Option<GridPosition>,
) {
    for candidate in candidates.iter_mut() {
        candidate.logistics_prerequisite = false;
        candidate.logistics_priority_rank = None;
    }

    let Some(primary_assault_index) = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.assessment.decision == IslandCampaignDecision::Assault
                && candidate.existing_operation.is_none()
        })
        .min_by_key(|(_, candidate)| offensive_priority_key(candidate))
        .map(|(index, _)| index)
    else {
        return;
    };
    let assault = &candidates[primary_assault_index];
    let assault_eta = assault.transport_eta.unwrap_or(u32::MAX);
    let economy_under_pressure = friendly_income_per_turn < enemy_income_per_turn;
    let direct_distance = home_position.map_or(0, |home| {
        map.distance(
            home.x,
            home.y,
            assault.target_position.x,
            assault.target_position.y,
        )
    });
    let is_long_range =
        assault_eta >= LONG_RANGE_ASSAULT_ETA || direct_distance >= LONG_RANGE_ASSAULT_DISTANCE;
    let needs_forward_base =
        is_long_range && !sustainment_network_covers_assault(forward_sustainment_coverage, assault);
    if !economy_under_pressure && !needs_forward_base {
        return;
    }

    let assault_target = assault.target_position;
    let best_index = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            (matches!(
                candidate.assessment.decision,
                IslandCampaignDecision::Expand
                    | IslandCampaignDecision::Secure
                    | IslandCampaignDecision::Contest
                    | IslandCampaignDecision::Reinforce
            ) || (candidate.assessment.decision == IslandCampaignDecision::Assault
                && candidate.existing_operation.is_some()))
                && candidate
                    .ground_sustainment_sites
                    .saturating_add(candidate.air_sustainment_sites)
                    .saturating_add(candidate.sea_sustainment_sites)
                    > 0
                && preferred_logistics_target(candidate, assault, forward_sustainment_coverage)
                    .is_some()
                && candidate
                    .transport_eta
                    .is_some_and(|eta| eta <= assault_eta)
        })
        .min_by_key(|(_, candidate)| {
            (
                u8::from(candidate.existing_operation.is_none()),
                u8::from(candidate.assessment.decision != IslandCampaignDecision::Secure),
                Reverse(logistics_coverage(candidate, assault)),
                map.distance(
                    candidate.target_position.x,
                    candidate.target_position.y,
                    assault_target.x,
                    assault_target.y,
                ),
                candidate
                    .assessment
                    .expansion_payback_turns
                    .unwrap_or(u32::MAX),
                Reverse(candidate.island_income_per_turn),
                candidate.transport_eta.unwrap_or(u32::MAX),
                candidate.assessment.island_id.0,
            )
        })
        .map(|(index, _)| index);

    if let Some(index) = best_index {
        let target =
            preferred_logistics_target(&candidates[index], assault, forward_sustainment_coverage)
                .expect("候補抽出時に兵站施設座標を確認済み");
        let candidate = &mut candidates[index];
        candidate.logistics_prerequisite = true;
        candidate.logistics_priority_rank = Some(0);
        candidate.target_position = target;
        // 兵站カテゴリの主施設を先頭にしつつ、残る占領兵は別施設を並行確保できるよう残す。
        candidate
            .capture_target_positions
            .retain(|position| *position != target);
        candidate.capture_target_positions.insert(0, target);
        if candidate.assessment.decision == IslandCampaignDecision::Assault
            && candidate.existing_operation.is_some()
        {
            // 先遣隊の上陸後に敵が到着しても、通常強襲の4枠編成へ組み替えて作戦を止めない。
            // 構造要件は進行中の軽量輸送隊を維持し、敵戦力分だけを後段で加算する。
            candidate.requirement.preferred_transport = Some(UnitType::TransportHelicopter);
            candidate.requirement.transport_slots = 2;
            candidate.requirement.capture_units = 2;
            candidate.requirement.combat_units = 0;
            candidate.requirement.total_budget = 6_000;
            candidate.assault_transport_types.clear();
            candidate.assessment.required_budget = candidate.requirement.total_budget;
        }
        if candidate.assessment.decision == IslandCampaignDecision::Secure
            && candidate.requirement.capture_units == 0
        {
            candidate.requirement.capture_units = 1;
            candidate.requirement.total_budget =
                candidate.requirement.total_budget.saturating_add(1_000);
            candidate.assessment.required_budget = candidate.requirement.total_budget;
        }
        candidate.assessment.decision_reason =
            "収入均衡と修理・燃料・弾薬の前進補給を確保してから敵本拠地を強襲する".to_owned();
    }
}

fn defense_priority_key(candidate: &IslandCampaignCandidate) -> (u32, Reverse<u32>, usize) {
    (
        candidate.assessment.enemy_arrival_eta.unwrap_or(u32::MAX),
        Reverse(candidate.assessment.enemy_combat_units),
        candidate.assessment.island_id.0,
    )
}

fn sorted_pool_units(
    pool: &CampaignResourcePool,
    target_island: IslandId,
) -> Vec<CampaignUnitCandidate> {
    let mut units: Vec<_> = pool
        .units
        .iter()
        .filter(|unit| {
            unit.assigned_island.is_none() || unit.assigned_island == Some(target_island)
        })
        .cloned()
        .collect();
    units.sort_by_key(|unit| {
        (
            u8::from(unit.island_id != Some(target_island)),
            unit.cost,
            campaign_unit_type_rank(unit.unit_type),
            unit.entity.to_bits(),
        )
    });
    units
}

fn remove_entity(pool: &mut CampaignResourcePool, entity: Entity) {
    pool.units.retain(|unit| unit.entity != entity);
}

fn push_unique_entity(entities: &mut Vec<Entity>, entity: Entity) {
    if !entities.contains(&entity) {
        entities.push(entity);
    }
}

fn search_campaign_transport_coverage(
    cargo: &[&CampaignUnitCandidate],
    transports: &[&CampaignUnitCandidate],
    cargo_index: usize,
    assigned_counts: &mut [u32],
) -> bool {
    if cargo_index == cargo.len() {
        return true;
    }
    let unit = cargo[cargo_index];
    for (index, transport) in transports.iter().enumerate() {
        if assigned_counts[index] >= transport.available_cargo_slots
            || transport.island_id != unit.island_id
            || !transport.loadable_unit_types.contains(&unit.unit_type)
        {
            continue;
        }
        assigned_counts[index] = assigned_counts[index].saturating_add(1);
        if search_campaign_transport_coverage(
            cargo,
            transports,
            cargo_index.saturating_add(1),
            assigned_counts,
        ) {
            return true;
        }
        assigned_counts[index] = assigned_counts[index].saturating_sub(1);
    }
    false
}

fn campaign_transport_package_covers(
    cargo_entities: &[Entity],
    transport_entities: &[Entity],
    catalog: &HashMap<Entity, CampaignUnitCandidate>,
) -> bool {
    if cargo_entities.is_empty() {
        return true;
    }
    let mut cargo: Vec<_> = cargo_entities
        .iter()
        .filter_map(|entity| catalog.get(entity))
        .collect();
    let mut transports: Vec<_> = transport_entities
        .iter()
        .filter_map(|entity| catalog.get(entity))
        .collect();
    if cargo.len() != cargo_entities.len() || transports.is_empty() {
        return false;
    }
    transports.sort_by_key(|transport| transport.entity.to_bits());
    let mut loaded_owner = HashMap::new();
    let mut assigned_counts = vec![0_u32; transports.len()];
    for (index, transport) in transports.iter().enumerate() {
        if transport.loaded_cargo_entities.len() as u32 > transport.available_cargo_slots {
            return false;
        }
        for loaded in &transport.loaded_cargo_entities {
            let Some(loaded_unit) = catalog.get(loaded) else {
                return false;
            };
            if !cargo_entities.contains(loaded)
                || transport.island_id != loaded_unit.island_id
                || !transport
                    .loadable_unit_types
                    .contains(&loaded_unit.unit_type)
                || loaded_owner.insert(*loaded, transport.entity).is_some()
            {
                return false;
            }
            assigned_counts[index] = assigned_counts[index].saturating_add(1);
        }
    }
    cargo.retain(|unit| !loaded_owner.contains_key(&unit.entity));
    cargo.sort_by_key(|unit| {
        let compatible = transports
            .iter()
            .enumerate()
            .filter(|(index, transport)| {
                assigned_counts[*index] < transport.available_cargo_slots
                    && transport.island_id == unit.island_id
                    && transport.loadable_unit_types.contains(&unit.unit_type)
            })
            .count();
        (compatible, unit.entity.to_bits())
    });
    search_campaign_transport_coverage(&cargo, &transports, 0, &mut assigned_counts)
}

/// 現在予約済みの輸送役が、指定cargoのうち最大何体を同時に運べるか。
/// 完全被覆だけでなく不足スロット数を生産へ返すため、同じ二部マッチングを最大化する。
fn campaign_transport_covered_count(
    cargo_entities: &[Entity],
    transport_entities: &[Entity],
    catalog: &HashMap<Entity, CampaignUnitCandidate>,
) -> usize {
    let cargo: Vec<_> = cargo_entities
        .iter()
        .filter_map(|entity| catalog.get(entity))
        .collect();
    let transports: Vec<_> = transport_entities
        .iter()
        .filter_map(|entity| catalog.get(entity))
        .collect();
    if cargo.len() != cargo_entities.len() || transports.is_empty() {
        return 0;
    }

    fn search(
        cargo: &[&CampaignUnitCandidate],
        transports: &[&CampaignUnitCandidate],
        index: usize,
        assigned: &mut [u32],
    ) -> usize {
        if index == cargo.len() {
            return 0;
        }
        let unit = cargo[index];
        let mut best = search(cargo, transports, index.saturating_add(1), assigned);
        for (transport_index, transport) in transports.iter().enumerate() {
            if assigned[transport_index] >= transport.available_cargo_slots
                || transport.island_id != unit.island_id
                || !transport.loadable_unit_types.contains(&unit.unit_type)
            {
                continue;
            }
            assigned[transport_index] = assigned[transport_index].saturating_add(1);
            best = best.max(1 + search(cargo, transports, index.saturating_add(1), assigned));
            assigned[transport_index] = assigned[transport_index].saturating_sub(1);
        }
        best
    }

    search(&cargo, &transports, 0, &mut vec![0; transports.len()])
}

/// 洋上展開で不足する輸送スロットと、最小費用で全cargoを積める輸送種別を返す。
fn remote_transport_shortfall(
    cargo_entities: &[Entity],
    transport_entities: &[Entity],
    catalog: &HashMap<Entity, CampaignUnitCandidate>,
    blueprints: &[CampaignTransportBlueprint],
) -> Option<(UnitType, u32, u32)> {
    let covered = campaign_transport_covered_count(cargo_entities, transport_entities, catalog);
    let missing_slots = u32::try_from(cargo_entities.len().saturating_sub(covered)).ok()?;
    if missing_slots == 0 {
        return None;
    }

    let mut cargo_by_island: HashMap<IslandId, Vec<UnitType>> = HashMap::new();
    for entity in cargo_entities {
        let cargo = catalog.get(entity)?;
        cargo_by_island
            .entry(cargo.island_id?)
            .or_default()
            .push(cargo.unit_type);
    }

    let mut candidates = Vec::new();
    for unit_type in [UnitType::TransportHelicopter, UnitType::Lander] {
        let mut total_cost = 0_u32;
        let mut possible = true;
        for (source_island, cargo_types) in &cargo_by_island {
            let Some(blueprint) = blueprints.iter().find(|blueprint| {
                blueprint.unit_type == unit_type
                    && blueprint.source_island == *source_island
                    && cargo_types
                        .iter()
                        .all(|cargo_type| blueprint.loadable_unit_types.contains(cargo_type))
            }) else {
                possible = false;
                break;
            };
            let units = u32::try_from(cargo_types.len())
                .unwrap_or(u32::MAX)
                .div_ceil(blueprint.cargo_slots.max(1));
            total_cost = total_cost.saturating_add(units.saturating_mul(blueprint.cost));
        }
        if possible {
            candidates.push((total_cost, campaign_unit_type_rank(unit_type), unit_type));
        }
    }
    candidates.sort_unstable_by_key(|(cost, rank, _)| (*cost, *rank));
    candidates
        .first()
        .map(|(cost, _, unit_type)| (*unit_type, missing_slots, *cost))
}

fn minimum_purchase_floor(
    decision: IslandCampaignDecision,
    missing_transport_slots: u32,
    missing_lander: bool,
    missing_helicopter: bool,
    missing_capture_units: u32,
) -> u32 {
    let transport_floor = match decision {
        IslandCampaignDecision::Expand => {
            u32::from(missing_transport_slots > 0).saturating_mul(4_000)
        }
        IslandCampaignDecision::Assault => u32::from(missing_lander)
            .saturating_mul(16_500)
            .saturating_add(u32::from(missing_helicopter).saturating_mul(4_000)),
        _ => 0,
    };
    transport_floor.saturating_add(missing_capture_units.saturating_mul(1_000))
}

fn reserve_candidate(
    candidate: &IslandCampaignCandidate,
    pool: &CampaignResourcePool,
    catalog: &HashMap<Entity, CampaignUnitCandidate>,
    allow_future_budget_reservation: bool,
) -> Option<(IslandCampaignAssignment, CampaignResourcePool)> {
    // 候補ごとのcloneへ全予約を適用し、完全編成を賄えない場合は元poolを一切変更しない。
    let mut provisional = pool.clone();
    let island_id = candidate.assessment.island_id;
    let requirement = &candidate.requirement;
    let mut transport_entities = Vec::new();
    let mut capture_entities = Vec::new();
    let mut combat_entities = Vec::new();
    let mut reserved_entity_value = 0_u32;
    let mut requirement_credit = 0_u32;
    let mut remote_transport_demand = None;
    let existing = candidate.existing_operation.as_ref();

    if let Some(operation) = existing {
        if operation.is_forming || is_live_campaign_transport_phase(operation.transport_phase) {
            for entity in &operation.transport_entities {
                if let Some(unit) = catalog.get(entity)
                    && (unit.assigned_island.is_none() || unit.assigned_island == Some(island_id))
                {
                    push_unique_entity(&mut transport_entities, *entity);
                    reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
                    remove_entity(&mut provisional, *entity);
                }
            }
        } else {
            // Return/Completed輸送は同じ島への再予約候補としても扱わない。
            for entity in &operation.transport_entities {
                remove_entity(&mut provisional, *entity);
            }
        }
        for entity in &operation.capture_entities {
            if let Some(unit) = catalog.get(entity)
                && (unit.assigned_island.is_none() || unit.assigned_island == Some(island_id))
                && (candidate.assessment.decision != IslandCampaignDecision::Defend
                    || !unit.is_transporting
                        && unit
                            .reachable_positions
                            .contains(&candidate.target_position))
            {
                push_unique_entity(&mut capture_entities, *entity);
                reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
                remove_entity(&mut provisional, *entity);
            }
        }
        for entity in &operation.combat_entities {
            if let Some(unit) = catalog.get(entity)
                && (unit.assigned_island.is_none() || unit.assigned_island == Some(island_id))
                && (candidate.assessment.decision != IslandCampaignDecision::Defend
                    || !unit.is_transporting
                        && unit
                            .reachable_positions
                            .contains(&candidate.target_position))
            {
                push_unique_entity(&mut combat_entities, *entity);
                reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
                remove_entity(&mut provisional, *entity);
            }
        }
    }
    let continued_from_existing_squad = !transport_entities.is_empty()
        || !capture_entities.is_empty()
        || !combat_entities.is_empty();

    if candidate.assessment.decision == IslandCampaignDecision::Contest {
        let local_assets: Vec<_> = sorted_pool_units(&provisional, island_id)
            .into_iter()
            .filter(|unit| {
                unit.island_id == Some(island_id) && !is_campaign_support_unit(unit.unit_type)
            })
            .collect();
        for unit in local_assets {
            if unit.can_capture {
                push_unique_entity(&mut capture_entities, unit.entity);
            } else {
                push_unique_entity(&mut combat_entities, unit.entity);
            }
            reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
            remove_entity(&mut provisional, unit.entity);
        }
    }

    let mut remaining_transport_slots = requirement.transport_slots;
    let mut missing_assault_transports = candidate.assault_transport_types.clone();
    let mut missing_helicopter = candidate.assessment.decision == IslandCampaignDecision::Expand;
    for entity in &transport_entities {
        let Some(unit) = catalog.get(entity) else {
            continue;
        };
        if let Some(index) = missing_assault_transports
            .iter()
            .position(|required| *required == unit.unit_type)
        {
            missing_assault_transports.remove(index);
        }
        if candidate.assessment.decision == IslandCampaignDecision::Expand
            && unit.unit_type == UnitType::TransportHelicopter
        {
            missing_helicopter = false;
        }
        let satisfies_transport_requirement = match candidate.assessment.decision {
            IslandCampaignDecision::Expand => unit.unit_type == UnitType::TransportHelicopter,
            _ => is_offshore_transport(unit.unit_type),
        };
        if satisfies_transport_requirement {
            let credited_slots = remaining_transport_slots.min(unit.available_cargo_slots);
            remaining_transport_slots = remaining_transport_slots.saturating_sub(credited_slots);
            requirement_credit = requirement_credit.saturating_add(unit.cost);
        }
    }

    let mut available = sorted_pool_units(&provisional, island_id);
    if candidate.assessment.decision == IslandCampaignDecision::Assault {
        let required_types = missing_assault_transports.clone();
        for required_type in required_types {
            if let Some(index) = available.iter().position(|unit| {
                unit.unit_type == required_type && is_offshore_transport(unit.unit_type)
            }) {
                let unit = available.remove(index);
                push_unique_entity(&mut transport_entities, unit.entity);
                remaining_transport_slots =
                    remaining_transport_slots.saturating_sub(unit.available_cargo_slots);
                requirement_credit = requirement_credit.saturating_add(unit.cost);
                reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
                remove_entity(&mut provisional, unit.entity);
                if let Some(index) = missing_assault_transports
                    .iter()
                    .position(|missing| *missing == required_type)
                {
                    missing_assault_transports.remove(index);
                }
            }
        }
    } else {
        while remaining_transport_slots > 0 {
            let Some(index) = available.iter().position(|unit| {
                is_offshore_transport(unit.unit_type)
                    && requirement
                        .preferred_transport
                        .is_none_or(|preferred| unit.unit_type == preferred)
            }) else {
                break;
            };
            let unit = available.remove(index);
            push_unique_entity(&mut transport_entities, unit.entity);
            remaining_transport_slots =
                remaining_transport_slots.saturating_sub(unit.available_cargo_slots);
            requirement_credit = requirement_credit.saturating_add(unit.cost);
            reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
            remove_entity(&mut provisional, unit.entity);
            if unit.unit_type == UnitType::TransportHelicopter {
                missing_helicopter = false;
            }
        }
    }

    let mut remaining_capture_units = requirement.capture_units;
    let mut capture_entities_used_for_capture = 0_usize;
    for entity in &capture_entities {
        if remaining_capture_units == 0 {
            break;
        }
        if let Some(unit) = catalog.get(entity) {
            remaining_capture_units = remaining_capture_units.saturating_sub(1);
            capture_entities_used_for_capture = capture_entities_used_for_capture.saturating_add(1);
            requirement_credit = requirement_credit.saturating_add(unit.cost);
        }
    }
    available = sorted_pool_units(&provisional, island_id);
    while remaining_capture_units > 0 {
        let Some(index) = available.iter().position(|unit| unit.can_capture) else {
            break;
        };
        let unit = available.remove(index);
        push_unique_entity(&mut capture_entities, unit.entity);
        remaining_capture_units = remaining_capture_units.saturating_sub(1);
        capture_entities_used_for_capture = capture_entities_used_for_capture.saturating_add(1);
        requirement_credit = requirement_credit.saturating_add(unit.cost);
        reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
        remove_entity(&mut provisional, unit.entity);
    }

    let mut remaining_combat_units = requirement.combat_units;
    for entity in capture_entities
        .iter()
        .skip(capture_entities_used_for_capture)
        .chain(combat_entities.iter())
    {
        if remaining_combat_units == 0 {
            break;
        }
        if catalog.contains_key(entity) {
            // capture枠へ使わなかった実Entityを、戦闘参加者として一体だけ充当する。
            remaining_combat_units = remaining_combat_units.saturating_sub(1);
            // 戦闘Entityの価格で輸送・占領の構造予算を相殺してはならない。
        }
    }
    let assault_wave_is_frozen = candidate.assessment.decision == IslandCampaignDecision::Assault
        && existing
            .is_some_and(|operation| is_live_campaign_transport_phase(operation.transport_phase));
    available = sorted_pool_units(&provisional, island_id);
    // Combat候補ごとにpool全体を再走査しない。既に選択済みと、この段階で予約可能な
    // 輸送役を一度だけまとめ、各cargo追加時の二部マッチングへ再利用する。
    let mut route_transports = transport_entities.clone();
    for transport in &available {
        if is_offshore_transport(transport.unit_type) {
            push_unique_entity(&mut route_transports, transport.entity);
        }
    }
    while remaining_combat_units > 0 && !assault_wave_is_frozen {
        let Some(index) = available.iter().position(|unit| {
            if is_campaign_support_unit(unit.unit_type) {
                return false;
            }
            if candidate.assessment.decision == IslandCampaignDecision::Defend {
                return !unit.is_transporting
                    && unit
                        .reachable_positions
                        .contains(&candidate.target_position);
            }
            if unit.island_id == Some(island_id)
                || unit
                    .reachable_positions
                    .contains(&candidate.target_position)
            {
                return true;
            }

            // 自力展開できない渡洋支援は、現在の輸送役または生産可能な輸送手段で
            // 実際に運べる場合だけ選ぶ。Contest/Reinforceを無条件で通すと、戦車など
            // 積載不能Entityが完全編成へ混ざり、後段で島作戦全体が消える。
            let mut remote_cargo: Vec<_> = capture_entities
                .iter()
                .chain(combat_entities.iter())
                .copied()
                .filter(|entity| {
                    catalog.get(entity).is_some_and(|cargo| {
                        cargo.island_id != Some(island_id)
                            && !cargo
                                .reachable_positions
                                .contains(&candidate.target_position)
                    })
                })
                .collect();
            remote_cargo.push(unit.entity);
            // transport_slotsが0のReinforceでも、pool内に完成済み輸送役があれば
            // 後段でその実Entityを予約できる。先に選択済みの輸送役だけを見ると、
            // 合法なLander付き編成まで「輸送経路なし」として落としてしまう。
            campaign_transport_package_covers(&remote_cargo, &route_transports, catalog)
                || remote_transport_shortfall(
                    &remote_cargo,
                    &route_transports,
                    catalog,
                    &candidate.producible_transports,
                )
                .is_some()
        }) else {
            break;
        };
        let unit = available.remove(index);
        push_unique_entity(&mut combat_entities, unit.entity);
        remaining_combat_units = remaining_combat_units.saturating_sub(1);
        reserved_entity_value = reserved_entity_value.saturating_add(unit.cost);
        remove_entity(&mut provisional, unit.entity);
    }
    if matches!(
        candidate.assessment.decision,
        IslandCampaignDecision::Expand
            | IslandCampaignDecision::Secure
            | IslandCampaignDecision::Contest
            | IslandCampaignDecision::Reinforce
    ) {
        let remote_cargo: Vec<_> = capture_entities
            .iter()
            .chain(combat_entities.iter())
            .copied()
            .filter(|entity| {
                catalog.get(entity).is_some_and(|unit| {
                    !unit.is_transporting
                        && unit
                            .island_id
                            .is_some_and(|source_island| source_island != island_id)
                        && !unit
                            .reachable_positions
                            .contains(&candidate.target_position)
                })
            })
            .collect();
        if !remote_cargo.is_empty() {
            // 後続波も総輸送枠だけでreadyにせず、cargoと同じ出発島から実搭載できることを確認する。
            available = sorted_pool_units(&provisional, island_id);
            while !campaign_transport_package_covers(&remote_cargo, &transport_entities, catalog) {
                let Some(index) = available.iter().position(|transport| {
                    is_offshore_transport(transport.unit_type)
                        && remote_cargo.iter().any(|cargo| {
                            catalog.get(cargo).is_some_and(|cargo| {
                                cargo.island_id == transport.island_id
                                    && transport.loadable_unit_types.contains(&cargo.unit_type)
                            })
                        })
                }) else {
                    // 既存輸送が無くても作戦を消さない。不足量を後段のpurchase_shortfallへ上げる。
                    break;
                };
                let transport = available.remove(index);
                push_unique_entity(&mut transport_entities, transport.entity);
                reserved_entity_value = reserved_entity_value.saturating_add(transport.cost);
                remove_entity(&mut provisional, transport.entity);
            }
            if !campaign_transport_package_covers(&remote_cargo, &transport_entities, catalog) {
                // 生産可能な輸送手段が無ければ、安全に完全パッケージを作れない。
                let demand = remote_transport_shortfall(
                    &remote_cargo,
                    &transport_entities,
                    catalog,
                    &candidate.producible_transports,
                )?;
                remote_transport_demand = Some(demand);
            }
        }
    }

    let missing_lander = missing_assault_transports.contains(&UnitType::Lander);
    let missing_helicopter =
        missing_helicopter || missing_assault_transports.contains(&UnitType::TransportHelicopter);
    // 高価な既存unitのcreditで輸送・占領など必須カテゴリの購入費が消えないよう下限を戻す。
    let structural_floor = minimum_purchase_floor(
        candidate.assessment.decision,
        remaining_transport_slots,
        missing_lander,
        missing_helicopter,
        remaining_capture_units,
    );
    let base_purchase_budget = requirement
        .total_budget
        .saturating_sub(requirement_credit)
        .max(structural_floor);
    // 現地要員は輸送不要なため、実際のremote cargoから判明した輸送費だけを加算する。
    let remote_transport_budget = remote_transport_demand
        .map(|(_, _, cost)| cost)
        .unwrap_or(0);
    let purchase_budget = base_purchase_budget.saturating_add(remote_transport_budget);
    if provisional.available_funds < purchase_budget && !allow_future_budget_reservation {
        return None;
    }
    // 最上位Assaultだけは現在資金を全額予約し、未達分を次ターンへ持ち越す。
    // shortfallには必要総額を残し、allocated_budgetには実際に拘束した現金だけを記録する。
    let reserved_funds = provisional.available_funds.min(purchase_budget);
    provisional.available_funds = provisional.available_funds.saturating_sub(reserved_funds);

    if let Some((_, slots, _)) = remote_transport_demand {
        remaining_transport_slots = remaining_transport_slots.max(slots);
    }
    let preferred_transport = if candidate.assessment.decision == IslandCampaignDecision::Assault {
        missing_assault_transports.first().copied()
    } else if let Some((unit_type, _, _)) = remote_transport_demand {
        Some(unit_type)
    } else if remaining_transport_slots > 0 {
        requirement.preferred_transport
    } else {
        None
    };
    let purchase_shortfall = IslandCampaignRequirement {
        preferred_transport,
        transport_slots: remaining_transport_slots,
        capture_units: remaining_capture_units,
        // 敵領戦闘力は時系列planの結果であり、島campaignの固定台数要求にしない。
        ground_combat_units: 0,
        combat_units: remaining_combat_units,
        total_budget: purchase_budget,
    };
    let capture_package_ready = purchase_shortfall.capture_units == 0
        || candidate.assessment.decision == IslandCampaignDecision::Reinforce
            && !capture_entities.is_empty();
    let structural_package_ready = purchase_shortfall.transport_slots == 0 && capture_package_ready;
    // 中立の兵站前提だけは構造パッケージ完成時点で先行できる。
    // 敵領Assaultは敵戦力を上回る離散的な損耗余裕まで揃わなければ出航させない。
    let operation_ready = structural_package_ready
        && ((candidate.logistics_prerequisite
            && candidate.assessment.decision != IslandCampaignDecision::Assault)
            || purchase_shortfall.combat_units == 0);
    transport_entities.sort_by_key(|entity| entity.to_bits());
    capture_entities.sort_by_key(|entity| entity.to_bits());
    combat_entities.sort_by_key(|entity| entity.to_bits());

    Some((
        IslandCampaignAssignment {
            island_id,
            decision: candidate.assessment.decision,
            target_position: candidate.target_position,
            capture_target_positions: candidate.capture_target_positions.clone(),
            priority_enemy_types: candidate.priority_enemy_types.clone(),
            requirement: requirement.clone(),
            purchase_shortfall,
            allocated_budget: reserved_entity_value.saturating_add(reserved_funds),
            transport_entities,
            capture_entities,
            combat_entities,
            operation_ready,
            continued_from_existing_squad,
        },
        provisional,
    ))
}

fn update_assessment_for_assignment(
    assessments: &mut [IslandCampaignAssessment],
    assignment: &IslandCampaignAssignment,
) {
    if let Some(assessment) = assessments
        .iter_mut()
        .find(|assessment| assessment.island_id == assignment.island_id)
    {
        assessment.decision = assignment.decision;
        assessment.pause_cause = None;
        assessment.allocated_budget = assignment.allocated_budget;
    }
}

fn mark_unallocated(
    assessments: &mut [IslandCampaignAssessment],
    island_id: IslandId,
    decision: IslandCampaignDecision,
    reason: &str,
    pause_cause: Option<IslandCampaignPauseCause>,
) {
    if let Some(assessment) = assessments
        .iter_mut()
        .find(|assessment| assessment.island_id == island_id)
    {
        assessment.decision = decision;
        assessment.decision_reason = reason.to_owned();
        assessment.pause_cause = pause_cause;
        assessment.allocated_budget = 0;
    }
}

type ContestProtection = HashMap<IslandId, Vec<(Entity, Option<IslandId>)>>;

fn protect_contest_assets(
    candidates: &[IslandCampaignCandidate],
    pool: &mut CampaignResourcePool,
) -> ContestProtection {
    let mut protections: ContestProtection = HashMap::new();
    let mut contest_islands: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.assessment.decision == IslandCampaignDecision::Contest)
        .map(|candidate| candidate.assessment.island_id)
        .collect();
    contest_islands.sort_by_key(|island_id| island_id.0);
    contest_islands.dedup();
    for island_id in contest_islands {
        for unit in &mut pool.units {
            let can_preserve = unit.island_id == Some(island_id)
                && !is_campaign_support_unit(unit.unit_type)
                && (unit.assigned_island.is_none() || unit.assigned_island == Some(island_id));
            if can_preserve {
                protections
                    .entry(island_id)
                    .or_default()
                    .push((unit.entity, unit.assigned_island));
                // Contest成立に使った現地戦力を先行する他島攻勢から一時的に保護する。
                unit.assigned_island = Some(island_id);
            }
        }
    }
    protections
}

fn restore_contest_assets(
    island_id: IslandId,
    pool: &mut CampaignResourcePool,
    protections: &mut ContestProtection,
) {
    let Some(protected) = protections.remove(&island_id) else {
        return;
    };
    for (entity, prior_assignment) in protected {
        if let Some(unit) = pool.units.iter_mut().find(|unit| unit.entity == entity) {
            // cap超過や予約失敗時は一時markerだけを元へ戻し、後続Defendへ解放する。
            unit.assigned_island = prior_assignment;
        }
    }
}

fn reserve_secure_capture_units(
    candidates: &[IslandCampaignCandidate],
    pool: &mut CampaignResourcePool,
) {
    let mut secure_islands: Vec<_> = candidates
        .iter()
        .filter(|candidate| {
            candidate.assessment.decision == IslandCampaignDecision::Secure
                && !candidate.logistics_prerequisite
        })
        .map(|candidate| candidate.assessment.island_id)
        .collect();
    secure_islands.sort_by_key(|island_id| island_id.0);
    secure_islands.dedup();
    for island_id in secure_islands {
        let mut local_capture_units: Vec<_> = pool
            .units
            .iter()
            .filter(|unit| {
                unit.island_id == Some(island_id) && unit.can_capture && !unit.is_transporting
            })
            .cloned()
            .collect();
        local_capture_units.sort_by_key(|unit| {
            (
                unit.cost,
                campaign_unit_type_rank(unit.unit_type),
                unit.entity.to_bits(),
            )
        });
        let entity = local_capture_units
            .iter()
            .find(|unit| unit.can_secure_local_property)
            .or_else(|| local_capture_units.first())
            .map(|unit| unit.entity);
        if let Some(entity) = entity {
            // assignmentを持たない旧来のSecureだけは、現地占領要員1体を他島へ流用しない。
            remove_entity(pool, entity);
        }
    }
}

fn release_assignment(
    assignment: &IslandCampaignAssignment,
    pool: &mut CampaignResourcePool,
    catalog: &HashMap<Entity, CampaignUnitCandidate>,
) {
    let mut assigned_entities = HashSet::new();
    let assigned_entity_value = assignment
        .transport_entities
        .iter()
        .chain(assignment.capture_entities.iter())
        .chain(assignment.combat_entities.iter())
        .filter(|entity| assigned_entities.insert(**entity))
        .filter_map(|entity| catalog.get(entity))
        .fold(0_u32, |total, unit| total.saturating_add(unit.cost));
    let reserved_funds = assignment
        .allocated_budget
        .saturating_sub(assigned_entity_value);
    pool.available_funds = pool.available_funds.saturating_add(reserved_funds);
    for entity in assignment
        .transport_entities
        .iter()
        .chain(assignment.capture_entities.iter())
        .chain(assignment.combat_entities.iter())
    {
        if pool.units.iter().any(|unit| unit.entity == *entity) {
            continue;
        }
        if let Some(unit) = catalog.get(entity) {
            let mut released = unit.clone();
            released.assigned_island = None;
            pool.units.push(released);
        }
    }
}

/// 共有資金とEntity候補を1つの可変poolとして扱い、完全編成だけを決定的に予約する。
pub(crate) fn allocate_campaign_portfolio(
    candidates: Vec<IslandCampaignCandidate>,
    mut pool: CampaignResourcePool,
) -> IslandCampaignPortfolio {
    let logistics_prerequisite_pending = candidates
        .iter()
        .any(|candidate| candidate.logistics_prerequisite);
    let has_expansion_candidate = candidates
        .iter()
        .any(|candidate| candidate.assessment.decision == IslandCampaignDecision::Expand);
    let catalog: HashMap<_, _> = pool
        .units
        .iter()
        .cloned()
        .map(|unit| (unit.entity, unit))
        .collect();
    let mut contest_protections = protect_contest_assets(&candidates, &mut pool);
    reserve_secure_capture_units(
        &candidates
            .iter()
            .filter(|candidate| candidate.requirement.capture_units == 0)
            .cloned()
            .collect::<Vec<_>>(),
        &mut pool,
    );
    let mut assessments: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.assessment.clone())
        .collect();
    let mut offenses: Vec<_> = candidates
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.assessment.decision,
                IslandCampaignDecision::Expand
                    | IslandCampaignDecision::Secure
                    | IslandCampaignDecision::Contest
                    | IslandCampaignDecision::Reinforce
                    | IslandCampaignDecision::Assault
            ) && (candidate.assessment.decision != IslandCampaignDecision::Secure
                || candidate.requirement.capture_units > 0
                || candidate.existing_operation.is_some())
        })
        .cloned()
        .collect();
    offenses.sort_by_key(offensive_priority_key);
    let mut active_offensives = Vec::new();
    let mut active_offensive_count = 0_usize;
    for candidate in offenses {
        if logistics_prerequisite_pending
            && !candidate.logistics_prerequisite
            && candidate.existing_operation.is_none()
            && candidate.assessment.decision == IslandCampaignDecision::Assault
        {
            mark_unallocated(
                &mut assessments,
                candidate.assessment.island_id,
                IslandCampaignDecision::Observe,
                "前進兵站拠点の確保まで新規強襲を待機する",
                None,
            );
            continue;
        }
        let consumes_offensive_slot =
            candidate.assessment.decision != IslandCampaignDecision::Secure;
        if consumes_offensive_slot && active_offensive_count == MAX_ACTIVE_OFFENSIVES {
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                restore_contest_assets(
                    candidate.assessment.island_id,
                    &mut pool,
                    &mut contest_protections,
                );
            }
            mark_unallocated(
                &mut assessments,
                candidate.assessment.island_id,
                IslandCampaignDecision::Observe,
                "同時攻勢上限のため監視する",
                None,
            );
            continue;
        }
        // 最上位の新規Assaultは全額がまだ無くても作戦として保持し、現在資金を
        // 下位Expandへ流さず複数ターンで完全パッケージを調達する。
        let allow_future_budget_reservation = matches!(
            candidate.assessment.decision,
            IslandCampaignDecision::Secure
                | IslandCampaignDecision::Contest
                | IslandCampaignDecision::Reinforce
        ) || candidate
            .existing_operation
            .as_ref()
            .is_some_and(|operation| operation.is_forming)
            || active_offensives.is_empty()
                && (candidate.assessment.decision == IslandCampaignDecision::Assault
                    || candidate.logistics_prerequisite);
        if let Some((assignment, provisional)) =
            reserve_candidate(&candidate, &pool, &catalog, allow_future_budget_reservation)
        {
            pool = provisional;
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                contest_protections.remove(&candidate.assessment.island_id);
            }
            update_assessment_for_assignment(&mut assessments, &assignment);
            active_offensives.push(assignment);
            active_offensive_count =
                active_offensive_count.saturating_add(usize::from(consumes_offensive_slot));
        } else {
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                restore_contest_assets(
                    candidate.assessment.island_id,
                    &mut pool,
                    &mut contest_protections,
                );
            }
            let has_active_expansion = active_offensives
                .iter()
                .any(|assignment| assignment.decision == IslandCampaignDecision::Expand);
            let (decision, reason) = if candidate.assessment.decision
                == IslandCampaignDecision::Reinforce
                && (has_active_expansion || has_expansion_candidate)
            {
                (
                    IslandCampaignDecision::Withdraw,
                    "より投資回収効率のよい中立島作戦を優先する",
                )
            } else if candidate.assessment.decision == IslandCampaignDecision::Reinforce {
                (
                    IslandCampaignDecision::Contest,
                    "完全増援を予約できないため現地作戦を継続する",
                )
            } else {
                (
                    IslandCampaignDecision::Observe,
                    "完全編成を予約できないため監視する",
                )
            };
            mark_unallocated(
                &mut assessments,
                candidate.assessment.island_id,
                decision,
                reason,
                None,
            );
        }
    }

    let mut defense_candidates: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| candidate.assessment.decision == IslandCampaignDecision::Defend)
        .collect();
    defense_candidates.sort_by_key(defense_priority_key);
    let mut defenses = Vec::new();
    for candidate in defense_candidates {
        loop {
            if let Some((assignment, provisional)) =
                reserve_candidate(&candidate, &pool, &catalog, false)
            {
                pool = provisional;
                update_assessment_for_assignment(&mut assessments, &assignment);
                defenses.push(assignment);
                break;
            }
            let Some(released) = active_offensives.pop() else {
                mark_unallocated(
                    &mut assessments,
                    candidate.assessment.island_id,
                    IslandCampaignDecision::Observe,
                    "完全な防衛編成を予約できないため監視する",
                    None,
                );
                break;
            };
            release_assignment(&released, &mut pool, &catalog);
            mark_unallocated(
                &mut assessments,
                released.island_id,
                IslandCampaignDecision::Observe,
                "Threatened島防衛のため攻勢を一時停止",
                Some(IslandCampaignPauseCause::DefensePreemption),
            );
        }
    }

    assessments.sort_by_key(|assessment| assessment.island_id.0);
    IslandCampaignPortfolio {
        islands: assessments,
        active_offensives,
        defenses,
    }
}

/// 島状態は保存済みの遷移状態ではなく、各ターンの事実から再計算する一時的な分類である。
/// 判定順序そのものがドメインルールであり、交戦中を脅威状態より優先するなど、複数条件が
/// 同時に成立しても必ず排他的な状態を返すため、最初に一致した分類を直ちに返す。
pub fn classify_island(facts: &IslandCampaignFacts) -> IslandCampaignState {
    if facts.capturable_properties == 0
        || !facts.reachable
        || (facts.island_income_per_turn == 0 && facts.strategic_production_sites == 0)
    {
        return IslandCampaignState::Ignored;
    }
    if facts.friendly_units > 0 && facts.enemy_units > 0 {
        return IslandCampaignState::Contested;
    }
    if facts.enemy_units == 0
        && (facts.friendly_units > 0 || facts.friendly_properties > 0)
        && facts.enemy_arrival_eta.is_some_and(|eta| eta <= 2)
    {
        return IslandCampaignState::Threatened;
    }
    if facts.neutral_properties > 0
        && facts.friendly_properties == 0
        && facts.enemy_properties == 0
        && facts.friendly_units == 0
        && facts.enemy_units == 0
    {
        return IslandCampaignState::OpenNeutral;
    }
    if facts.enemy_units == 0 && (facts.friendly_units > 0 || facts.friendly_properties > 0) {
        return IslandCampaignState::Secured;
    }
    if facts.friendly_units == 0 && (facts.enemy_units > 0 || facts.enemy_properties > 0) {
        return IslandCampaignState::EnemyHeld;
    }
    IslandCampaignState::Ignored
}

fn ceil_div(numerator: u32, denominator: u32) -> u32 {
    numerator / denominator + u32::from(!numerator.is_multiple_of(denominator))
}

/// 輸送・占領・不足パッケージ回収を合わせた投資回収ターン数を計算する。
pub fn calculate_expansion_payback_turns(
    transport_eta: Option<u32>,
    capture_turns: u32,
    missing_package_cost: u32,
    island_income_per_turn: u32,
) -> Option<u32> {
    let transport_eta = transport_eta?;
    if island_income_per_turn == 0 {
        return None;
    }

    Some(
        transport_eta
            .saturating_add(capture_turns)
            .saturating_add(ceil_div(missing_package_cost, island_income_per_turn)),
    )
}

/// 交戦中の島について、共有資源配分側から渡された補強可否と代替投資有無で判断する。
pub fn decide_contested(
    facts: &IslandCampaignFacts,
    _reinforced_friendly_units: u32,
    can_allocate_reinforcement: bool,
    has_better_open_neutral: bool,
) -> IslandCampaignDecision {
    let capture_race_is_competitive = facts
        .friendly_capture_eta
        .zip(facts.enemy_capture_eta)
        .is_some_and(|(friendly_eta, enemy_eta)| friendly_eta <= enemy_eta.saturating_add(1));
    if capture_race_is_competitive {
        return IslandCampaignDecision::Contest;
    }

    // 撃破可否をEntity数の単純比較では決めない。補強packageを割り当てられるなら
    // 作戦を継続し、必要編成の可否はターン別戦闘計画器へ委譲する。
    if can_allocate_reinforcement {
        return IslandCampaignDecision::Reinforce;
    }
    if has_better_open_neutral {
        return IslandCampaignDecision::Withdraw;
    }

    IslandCampaignDecision::Contest
}

fn ignored_state_reason(facts: &IslandCampaignFacts) -> &'static str {
    if facts.capturable_properties == 0 {
        "占領可能な拠点がないため対象外"
    } else if !facts.reachable {
        "到達できないため対象外"
    } else if facts.island_income_per_turn == 0 && facts.strategic_production_sites == 0 {
        "収益性と戦略価値がないため対象外"
    } else {
        "島嶼作戦の分類条件を満たさないため対象外"
    }
}

/// 各島の事実だけから状態と暫定判断を作る。共有資源による最終確定は配分側で行う。
pub fn assess_island(facts: &IslandCampaignFacts) -> IslandCampaignAssessment {
    let state = classify_island(facts);
    let expansion_payback_turns = if state == IslandCampaignState::OpenNeutral {
        calculate_expansion_payback_turns(
            facts.transport_eta,
            facts.capture_turns,
            facts.missing_expansion_package_cost,
            facts.island_income_per_turn,
        )
    } else {
        None
    };

    // 状態ごとの分岐に対応した固定文言を使い、構造体のDebug表現には依存しない。
    let (decision, state_reason, decision_reason, required_budget) = match state {
        IslandCampaignState::Ignored => (
            IslandCampaignDecision::Observe,
            ignored_state_reason(facts),
            "侵攻条件を満たさないため監視する",
            0,
        ),
        IslandCampaignState::OpenNeutral if expansion_payback_turns.is_some() => (
            IslandCampaignDecision::Expand,
            "未占領の中立島である",
            "投資回収ターンを算出できるため暫定的に拡張候補とする",
            0,
        ),
        IslandCampaignState::OpenNeutral => (
            IslandCampaignDecision::Observe,
            "未占領の中立島である",
            "投資回収ターンを算出できないため監視する",
            0,
        ),
        IslandCampaignState::Secured if facts.has_unowned_properties => (
            IslandCampaignDecision::Secure,
            "友軍が足場を確保している島である",
            "未所有の拠点を確保する",
            0,
        ),
        IslandCampaignState::Secured => (
            IslandCampaignDecision::Observe,
            "友軍が足場を確保している島である",
            "追加で確保する拠点がないため監視する",
            0,
        ),
        IslandCampaignState::Threatened => (
            IslandCampaignDecision::Defend,
            "敵軍の接近が予測される友軍拠点である",
            "敵軍到着に備えて防衛する",
            0,
        ),
        IslandCampaignState::Contested => (
            IslandCampaignDecision::Contest,
            "友軍と敵軍が同時に展開している島である",
            "共有資源配分前のため暫定的に現地で交戦を継続する",
            0,
        ),
        IslandCampaignState::EnemyHeld => (
            IslandCampaignDecision::Assault,
            "敵軍が支配している島である",
            "敵Entityを排除できる実行計画を探索する強襲候補とする",
            0,
        ),
    };

    IslandCampaignAssessment {
        island_id: facts.island_id,
        state,
        decision,
        state_reason: state_reason.to_owned(),
        decision_reason: decision_reason.to_owned(),
        pause_cause: None,
        neutral_properties: facts.neutral_properties,
        friendly_properties: facts.friendly_properties,
        enemy_properties: facts.enemy_properties,
        friendly_combat_units: facts.friendly_combat_units,
        enemy_combat_units: facts.enemy_combat_units,
        friendly_arrival_eta: facts.friendly_arrival_eta,
        enemy_arrival_eta: facts.enemy_arrival_eta,
        friendly_capture_eta: facts.friendly_capture_eta,
        enemy_capture_eta: facts.enemy_capture_eta,
        roi_production_sites: facts.roi_production_sites,
        transport_eta: facts.transport_eta,
        expansion_payback_turns,
        required_budget,
        allocated_budget: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::islands::IslandId;
    use crate::resources::{GridTopology, Terrain};
    use std::collections::HashSet;

    fn facts_for_empty_neutral_island() -> IslandCampaignFacts {
        IslandCampaignFacts {
            island_id: IslandId(0),
            capturable_properties: 1,
            strategic_production_sites: 0,
            roi_production_sites: 0,
            neutral_properties: 1,
            friendly_properties: 0,
            enemy_properties: 0,
            friendly_units: 0,
            enemy_units: 0,
            friendly_combat_units: 0,
            enemy_combat_units: 0,
            friendly_arrival_eta: None,
            enemy_arrival_eta: None,
            friendly_capture_eta: None,
            enemy_capture_eta: None,
            transport_eta: None,
            capture_turns: 1,
            island_income_per_turn: 1_000,
            missing_expansion_package_cost: 0,
            reachable: true,
            has_unowned_properties: true,
        }
    }

    fn facts_without_capturable_properties() -> IslandCampaignFacts {
        let mut facts = facts_for_empty_neutral_island();
        facts.capturable_properties = 0;
        facts.neutral_properties = 0;
        facts.has_unowned_properties = false;
        facts
    }

    fn facts_with_both_armies_present() -> IslandCampaignFacts {
        let mut facts = facts_for_empty_neutral_island();
        facts.friendly_units = 1;
        facts.enemy_units = 1;
        facts
    }

    fn facts_with_friendly_foothold_and_enemy_eta(eta: u32) -> IslandCampaignFacts {
        let mut facts = facts_for_safe_friendly_foothold();
        facts.enemy_arrival_eta = Some(eta);
        facts
    }

    fn facts_for_safe_friendly_foothold() -> IslandCampaignFacts {
        let mut facts = facts_for_empty_neutral_island();
        facts.neutral_properties = 0;
        facts.friendly_properties = 1;
        facts.friendly_units = 1;
        facts.has_unowned_properties = false;
        facts
    }

    fn facts_for_enemy_foothold() -> IslandCampaignFacts {
        let mut facts = facts_for_empty_neutral_island();
        facts.neutral_properties = 0;
        facts.enemy_properties = 1;
        facts.enemy_units = 1;
        facts.has_unowned_properties = false;
        facts
    }

    fn facts_with_capture_etas(
        friendly_capture_eta: Option<u32>,
        enemy_capture_eta: Option<u32>,
    ) -> IslandCampaignFacts {
        let mut facts = facts_with_both_armies_present();
        facts.friendly_combat_units = 1;
        facts.enemy_combat_units = 1;
        facts.friendly_capture_eta = friendly_capture_eta;
        facts.enemy_capture_eta = enemy_capture_eta;
        facts
    }

    fn facts_with_power(
        friendly_combat_units: u32,
        enemy_combat_units: u32,
    ) -> IslandCampaignFacts {
        let mut facts = facts_with_both_armies_present();
        facts.friendly_combat_units = friendly_combat_units;
        facts.enemy_combat_units = enemy_combat_units;
        facts
    }

    #[test]
    fn classifies_islands_in_required_precedence_order() {
        let cases = [
            (
                facts_without_capturable_properties(),
                IslandCampaignState::Ignored,
            ),
            (
                facts_with_both_armies_present(),
                IslandCampaignState::Contested,
            ),
            (
                facts_with_friendly_foothold_and_enemy_eta(2),
                IslandCampaignState::Threatened,
            ),
            (
                facts_for_empty_neutral_island(),
                IslandCampaignState::OpenNeutral,
            ),
            (
                facts_for_safe_friendly_foothold(),
                IslandCampaignState::Secured,
            ),
            (facts_for_enemy_foothold(), IslandCampaignState::EnemyHeld),
        ];

        for (facts, expected) in cases {
            assert_eq!(classify_island(&facts), expected);
        }
    }

    #[test]
    fn contested_precedes_threatened_when_both_armies_are_present() {
        let mut facts = facts_with_both_armies_present();
        facts.enemy_arrival_eta = Some(1);

        assert_eq!(classify_island(&facts), IslandCampaignState::Contested);
    }

    #[test]
    fn derives_state_transitions_from_current_facts() {
        let mut open = facts_for_empty_neutral_island();

        assert_eq!(classify_island(&open), IslandCampaignState::OpenNeutral);
        open.friendly_units = 1;
        open.friendly_properties = 1;
        assert_eq!(classify_island(&open), IslandCampaignState::Secured);
        open.enemy_arrival_eta = Some(2);
        assert_eq!(classify_island(&open), IslandCampaignState::Threatened);
        open.enemy_units = 1;
        assert_eq!(classify_island(&open), IslandCampaignState::Contested);
    }

    #[test]
    fn classifies_required_initial_island_states() {
        let own_home_island = facts_for_safe_friendly_foothold();
        let enemy_home_island = facts_for_enemy_foothold();
        let empty_neutral_island = facts_for_empty_neutral_island();
        let no_property_island = facts_without_capturable_properties();

        assert_eq!(
            classify_island(&own_home_island),
            IslandCampaignState::Secured
        );
        assert_eq!(
            classify_island(&enemy_home_island),
            IslandCampaignState::EnemyHeld
        );
        assert_eq!(
            classify_island(&empty_neutral_island),
            IslandCampaignState::OpenNeutral
        );
        assert_eq!(
            classify_island(&no_property_island),
            IslandCampaignState::Ignored
        );
    }

    #[test]
    fn calculates_open_neutral_payback_with_ceiling_division() {
        assert_eq!(
            calculate_expansion_payback_turns(Some(2), 3, 6_001, 1_000),
            Some(12),
        );
        assert_eq!(
            calculate_expansion_payback_turns(Some(2), 3, 6_000, 0),
            None
        );
        assert_eq!(
            calculate_expansion_payback_turns(None, 3, 6_000, 1_000),
            None
        );
        assert_eq!(
            calculate_expansion_payback_turns(Some(u32::MAX), 1, u32::MAX, 1),
            Some(u32::MAX),
        );
    }

    #[test]
    fn decides_contested_action_from_capture_race_and_complete_reinforcement() {
        assert_eq!(
            decide_contested(
                &facts_with_capture_etas(Some(3), Some(2)),
                10_000,
                false,
                false,
            ),
            IslandCampaignDecision::Contest,
        );
        assert_eq!(
            decide_contested(&facts_with_power(8_000, 10_000), 12_000, true, false),
            IslandCampaignDecision::Reinforce,
        );
        assert_eq!(
            decide_contested(&facts_with_power(8_000, 10_000), 8_000, false, true),
            IslandCampaignDecision::Withdraw,
        );
        assert_eq!(
            decide_contested(&facts_with_power(8_000, 10_000), 10_999, true, true),
            IslandCampaignDecision::Reinforce,
        );
        assert_eq!(
            decide_contested(&facts_with_power(8_000, 10_000), 8_000, false, false),
            IslandCampaignDecision::Contest,
        );
    }

    #[test]
    fn assesses_each_state_with_provisional_decision_and_local_values() {
        let ignored = assess_island(&facts_without_capturable_properties());
        assert_eq!(ignored.state, IslandCampaignState::Ignored);
        assert_eq!(ignored.decision, IslandCampaignDecision::Observe);
        assert_eq!(ignored.state_reason, "占領可能な拠点がないため対象外");
        assert_eq!(ignored.decision_reason, "侵攻条件を満たさないため監視する");

        let mut open = facts_for_empty_neutral_island();
        open.transport_eta = Some(2);
        open.roi_production_sites = 2;
        open.capture_turns = 3;
        open.missing_expansion_package_cost = 6_001;
        let expansion = assess_island(&open);
        assert_eq!(expansion.state, IslandCampaignState::OpenNeutral);
        assert_eq!(expansion.decision, IslandCampaignDecision::Expand);
        assert_eq!(expansion.roi_production_sites, 2);
        assert_eq!(expansion.transport_eta, Some(2));
        assert_eq!(expansion.expansion_payback_turns, Some(12));
        assert_eq!(
            expansion.decision_reason,
            "投資回収ターンを算出できるため暫定的に拡張候補とする"
        );

        open.transport_eta = None;
        let unpriced_expansion = assess_island(&open);
        assert_eq!(unpriced_expansion.decision, IslandCampaignDecision::Observe);
        assert_eq!(unpriced_expansion.expansion_payback_turns, None);
        assert_eq!(
            unpriced_expansion.decision_reason,
            "投資回収ターンを算出できないため監視する"
        );

        let mut secured_facts = facts_for_safe_friendly_foothold();
        secured_facts.neutral_properties = 1;
        secured_facts.has_unowned_properties = true;
        let secured = assess_island(&secured_facts);
        assert_eq!(secured.state, IslandCampaignState::Secured);
        assert_eq!(secured.decision, IslandCampaignDecision::Secure);

        secured_facts.neutral_properties = 0;
        secured_facts.has_unowned_properties = false;
        assert_eq!(
            assess_island(&secured_facts).decision,
            IslandCampaignDecision::Observe
        );

        let threatened = assess_island(&facts_with_friendly_foothold_and_enemy_eta(2));
        assert_eq!(threatened.state, IslandCampaignState::Threatened);
        assert_eq!(threatened.decision, IslandCampaignDecision::Defend);

        let contested = assess_island(&facts_with_both_armies_present());
        assert_eq!(contested.state, IslandCampaignState::Contested);
        assert_eq!(contested.decision, IslandCampaignDecision::Contest);
        assert_eq!(
            contested.decision_reason,
            "共有資源配分前のため暫定的に現地で交戦を継続する"
        );

        let mut enemy_held_facts = facts_for_enemy_foothold();
        enemy_held_facts.enemy_combat_units = 1;
        let enemy_held = assess_island(&enemy_held_facts);
        assert_eq!(enemy_held.state, IslandCampaignState::EnemyHeld);
        assert_eq!(enemy_held.decision, IslandCampaignDecision::Assault);
        assert_eq!(enemy_held.required_budget, 0);
        assert_eq!(enemy_held.allocated_budget, 0);
        assert_eq!(
            enemy_held.decision_reason,
            "敵Entityを排除できる実行計画を探索する強襲候補とする"
        );
    }

    fn allocation_assessment(
        island: usize,
        state: IslandCampaignState,
        decision: IslandCampaignDecision,
    ) -> IslandCampaignAssessment {
        IslandCampaignAssessment {
            island_id: IslandId(island),
            state,
            decision,
            state_reason: "test state".to_owned(),
            decision_reason: "test decision".to_owned(),
            pause_cause: None,
            neutral_properties: u32::from(state == IslandCampaignState::OpenNeutral),
            friendly_properties: u32::from(state == IslandCampaignState::Threatened),
            enemy_properties: u32::from(state == IslandCampaignState::EnemyHeld),
            friendly_combat_units: 0,
            enemy_combat_units: 0,
            friendly_arrival_eta: None,
            enemy_arrival_eta: None,
            friendly_capture_eta: None,
            enemy_capture_eta: None,
            roi_production_sites: 0,
            transport_eta: None,
            expansion_payback_turns: None,
            required_budget: 0,
            allocated_budget: 0,
        }
    }

    fn expansion_candidate(island: usize, payback: u32) -> IslandCampaignCandidate {
        let mut assessment = allocation_assessment(
            island,
            IslandCampaignState::OpenNeutral,
            IslandCampaignDecision::Expand,
        );
        assessment.expansion_payback_turns = Some(payback);
        IslandCampaignCandidate {
            assessment,
            target_position: GridPosition { x: island, y: 0 },
            capture_target_positions: vec![GridPosition { x: island, y: 0 }],
            roi_production_sites: 0,
            transport_eta: Some(1),
            ground_sustainment_sites: 1,
            air_sustainment_sites: 0,
            sea_sustainment_sites: 0,
            sustainment_targets: CampaignSustainmentTargets {
                ground: Some(GridPosition { x: island, y: 0 }),
                ..CampaignSustainmentTargets::default()
            },
            island_income_per_turn: 1_000,
            logistics_prerequisite: false,
            logistics_priority_rank: None,
            priority_enemy_types: Vec::new(),
            requirement: IslandCampaignRequirement {
                preferred_transport: Some(UnitType::TransportHelicopter),
                transport_slots: 2,
                capture_units: 2,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 6_000,
            },
            assault_transport_types: Vec::new(),
            producible_transports: test_transport_blueprints(),
            existing_operation: None,
        }
    }

    fn test_transport_blueprints() -> Vec<CampaignTransportBlueprint> {
        (0..=16)
            .flat_map(|source_island| {
                [
                    CampaignTransportBlueprint {
                        unit_type: UnitType::TransportHelicopter,
                        cost: 4_000,
                        cargo_slots: 2,
                        loadable_unit_types: vec![UnitType::Infantry, UnitType::Mech],
                        source_island: IslandId(source_island),
                    },
                    CampaignTransportBlueprint {
                        unit_type: UnitType::Lander,
                        cost: 16_500,
                        cargo_slots: 2,
                        loadable_unit_types: vec![
                            UnitType::Infantry,
                            UnitType::Mech,
                            UnitType::Recon,
                            UnitType::Tank,
                            UnitType::MdTank,
                            UnitType::Artillery,
                        ],
                        source_island: IslandId(source_island),
                    },
                ]
            })
            .collect()
    }

    #[test]
    fn logistics_stage_rank_precedes_local_payback_order() {
        let mut later_high_roi = expansion_candidate(2, 1);
        later_high_roi.logistics_prerequisite = true;
        later_high_roi.logistics_priority_rank = Some(1);
        let mut first_core_stage = expansion_candidate(3, 9);
        first_core_stage.logistics_prerequisite = true;
        first_core_stage.logistics_priority_rank = Some(0);

        let portfolio = allocate_campaign_portfolio(
            vec![later_high_roi, first_core_stage],
            CampaignResourcePool {
                available_funds: 6_000,
                units: Vec::new(),
            },
        );

        assert_eq!(portfolio.active_offensives.len(), 1);
        assert_eq!(portfolio.active_offensives[0].island_id, IslandId(3));
    }

    #[test]
    fn promotes_matching_forward_airfield_allows_parallel_expansion_and_blocks_assault() {
        let map = Map::new(12, 3, Terrain::Sea, GridTopology::Square);
        let mut airfield = expansion_candidate(1, 6);
        airfield.target_position = GridPosition { x: 4, y: 1 };
        airfield.transport_eta = Some(2);
        airfield.air_sustainment_sites = 1;
        airfield.sustainment_targets.air = Some(GridPosition { x: 4, y: 1 });
        airfield.island_income_per_turn = 3_000;

        // 敵に近い都市だけの島より、侵攻波の輸送ヘリも再補給できる空港島を選ぶ。
        let mut city_only = expansion_candidate(2, 4);
        city_only.target_position = GridPosition { x: 8, y: 1 };
        city_only.transport_eta = Some(2);
        city_only.island_income_per_turn = 4_000;

        let mut assault = assault_candidate(9);
        assault.target_position = GridPosition { x: 10, y: 1 };
        assault.transport_eta = Some(5);
        assault.assault_transport_types = vec![UnitType::TransportHelicopter];

        let mut candidates = vec![city_only, assault, airfield];
        promote_logistics_prerequisite(
            &mut candidates,
            &map,
            14_000,
            14_000,
            CampaignSustainmentCoverage::default(),
            Some(GridPosition { x: 0, y: 1 }),
        );

        assert!(candidates[2].logistics_prerequisite);
        assert!(!candidates[0].logistics_prerequisite);
        assert_eq!(candidates[2].target_position, GridPosition { x: 4, y: 1 });
        let portfolio = allocate_campaign_portfolio(
            candidates,
            CampaignResourcePool {
                available_funds: 12_000,
                units: Vec::new(),
            },
        );
        let active_islands: Vec<_> = portfolio
            .active_offensives
            .iter()
            .map(|assignment| assignment.island_id)
            .collect();
        assert_eq!(active_islands, vec![IslandId(1), IslandId(2)]);
        let assault_assessment = portfolio
            .islands
            .iter()
            .find(|assessment| assessment.island_id == IslandId(9))
            .unwrap();
        assert_eq!(assault_assessment.decision, IslandCampaignDecision::Observe);
        assert_eq!(
            assault_assessment.decision_reason,
            "前進兵站拠点の確保まで新規強襲を待機する"
        );
    }

    #[test]
    fn contested_logistics_operation_stays_prioritized_and_advances_to_ground_facility() {
        let map = Map::new(12, 3, Terrain::Sea, GridTopology::Square);
        let mut bridgehead = expansion_candidate(1, 6);
        bridgehead.assessment.state = IslandCampaignState::Contested;
        bridgehead.assessment.decision = IslandCampaignDecision::Contest;
        bridgehead.assessment.friendly_properties = 1;
        bridgehead.target_position = GridPosition { x: 4, y: 1 };
        bridgehead.air_sustainment_sites = 1;
        bridgehead.sustainment_targets = CampaignSustainmentTargets {
            ground: Some(GridPosition { x: 5, y: 1 }),
            air: None,
            sea: None,
        };
        bridgehead.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(1),
            target_position: GridPosition { x: 4, y: 1 },
            transport_phase: Some(TransportPhase::Drop),
            is_forming: false,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
        });
        let mut assault = assault_candidate(9);
        assault.target_position = GridPosition { x: 10, y: 1 };
        assault.transport_eta = Some(5);
        assault.assault_transport_types = vec![UnitType::TransportHelicopter];
        let mut candidates = vec![bridgehead, assault];

        promote_logistics_prerequisite(
            &mut candidates,
            &map,
            14_000,
            16_000,
            CampaignSustainmentCoverage {
                ground: false,
                air: true,
                sea: false,
            },
            Some(GridPosition { x: 0, y: 1 }),
        );

        assert!(candidates[0].logistics_prerequisite);
        assert_eq!(candidates[0].target_position, GridPosition { x: 5, y: 1 });
        assert_eq!(offensive_priority_key(&candidates[0]).0, 0);
    }

    #[test]
    fn landed_logistics_assault_keeps_lightweight_structural_package() {
        let map = Map::new(12, 3, Terrain::Sea, GridTopology::Square);
        let mut bridgehead = assault_candidate(1);
        bridgehead.target_position = GridPosition { x: 4, y: 1 };
        bridgehead.air_sustainment_sites = 1;
        bridgehead.sustainment_targets = CampaignSustainmentTargets {
            ground: Some(GridPosition { x: 5, y: 1 }),
            air: None,
            sea: None,
        };
        bridgehead.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(1),
            target_position: GridPosition { x: 4, y: 1 },
            transport_phase: Some(TransportPhase::Drop),
            is_forming: false,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
        });
        let mut enemy_home = assault_candidate(9);
        enemy_home.target_position = GridPosition { x: 10, y: 1 };
        enemy_home.transport_eta = Some(5);
        enemy_home.assault_transport_types = vec![UnitType::TransportHelicopter];
        let mut candidates = vec![bridgehead, enemy_home];

        promote_logistics_prerequisite(
            &mut candidates,
            &map,
            14_000,
            16_000,
            CampaignSustainmentCoverage {
                ground: false,
                air: true,
                sea: false,
            },
            Some(GridPosition { x: 0, y: 1 }),
        );

        let bridgehead = &candidates[0];
        assert!(bridgehead.logistics_prerequisite);
        assert_eq!(bridgehead.target_position, GridPosition { x: 5, y: 1 });
        assert_eq!(
            bridgehead.requirement.preferred_transport,
            Some(UnitType::TransportHelicopter)
        );
        assert_eq!(bridgehead.requirement.transport_slots, 2);
        assert_eq!(bridgehead.requirement.capture_units, 2);
        assert_eq!(bridgehead.requirement.combat_units, 0);
        assert_eq!(bridgehead.requirement.total_budget, 6_000);
        assert!(bridgehead.assault_transport_types.is_empty());
    }

    #[test]
    fn skips_logistics_prerequisite_after_income_parity_and_forward_base() {
        let map = Map::new(12, 3, Terrain::Sea, GridTopology::Square);
        let mut expansion = expansion_candidate(1, 6);
        expansion.air_sustainment_sites = 1;
        let mut assault = assault_candidate(9);
        assault.transport_eta = Some(5);
        assault.assault_transport_types = vec![UnitType::TransportHelicopter];
        let mut candidates = vec![expansion, assault];

        promote_logistics_prerequisite(
            &mut candidates,
            &map,
            14_000,
            14_000,
            CampaignSustainmentCoverage {
                ground: true,
                air: true,
                sea: false,
            },
            Some(GridPosition { x: 0, y: 1 }),
        );

        assert!(
            candidates
                .iter()
                .all(|candidate| !candidate.logistics_prerequisite)
        );
    }

    #[test]
    fn does_not_mistake_home_island_cleanup_for_a_forward_logistics_base() {
        let map = Map::new(12, 3, Terrain::Sea, GridTopology::Square);
        let mut home_cleanup = secure_candidate(0);
        home_cleanup.assessment.friendly_properties = 8;
        home_cleanup.ground_sustainment_sites = 6;
        home_cleanup.air_sustainment_sites = 2;
        home_cleanup.sustainment_targets = CampaignSustainmentTargets::default();
        let mut forward_airfield = expansion_candidate(3, 8);
        forward_airfield.air_sustainment_sites = 1;
        let mut assault = assault_candidate(9);
        assault.transport_eta = Some(5);
        assault.assault_transport_types = vec![UnitType::TransportHelicopter];
        let mut candidates = vec![home_cleanup, forward_airfield, assault];

        promote_logistics_prerequisite(
            &mut candidates,
            &map,
            14_000,
            14_000,
            CampaignSustainmentCoverage::default(),
            Some(GridPosition { x: 0, y: 1 }),
        );

        assert!(!candidates[0].logistics_prerequisite);
        assert!(candidates[1].logistics_prerequisite);
    }

    fn unit_candidate(
        entity: u32,
        unit_type: UnitType,
        cost: u32,
        can_capture: bool,
        available_cargo_slots: u32,
    ) -> CampaignUnitCandidate {
        CampaignUnitCandidate {
            entity: Entity::from_raw(entity),
            unit_type,
            cost,
            can_capture,
            can_secure_local_property: can_capture,
            available_cargo_slots,
            loaded_cargo_entities: Vec::new(),
            loadable_unit_types: match unit_type {
                UnitType::TransportHelicopter => vec![UnitType::Infantry, UnitType::Mech],
                UnitType::Lander => vec![
                    UnitType::Infantry,
                    UnitType::Mech,
                    UnitType::Recon,
                    UnitType::Tank,
                    UnitType::MdTank,
                    UnitType::Artillery,
                ],
                _ => Vec::new(),
            },
            is_transporting: false,
            reachable_positions: (0..=16).map(|x| GridPosition { x, y: 1 }).collect(),
            island_id: None,
            assigned_island: None,
        }
    }

    #[test]
    fn allocates_only_the_top_three_open_neutral_packages() {
        let candidates = vec![
            expansion_candidate(3, 7),
            expansion_candidate(1, 5),
            expansion_candidate(0, 4),
            expansion_candidate(2, 6),
        ];
        let initial_available_funds = 24_000;

        let portfolio = allocate_campaign_portfolio(
            candidates,
            CampaignResourcePool {
                available_funds: initial_available_funds,
                units: Vec::new(),
            },
        );

        assert_eq!(
            portfolio
                .active_offensives
                .iter()
                .map(|assignment| assignment.island_id)
                .collect::<Vec<_>>(),
            vec![IslandId(0), IslandId(1), IslandId(2)]
        );
        let reserved_funds = portfolio
            .active_offensives
            .iter()
            .fold(0_u32, |total, assignment| {
                total.saturating_add(assignment.purchase_shortfall.total_budget)
            });
        assert!(reserved_funds <= initial_available_funds);
        let rejected = portfolio
            .islands
            .iter()
            .find(|assessment| assessment.island_id == IslandId(3))
            .unwrap();
        assert_eq!(rejected.decision, IslandCampaignDecision::Observe);
        assert_eq!(rejected.allocated_budget, 0);
        assert!(portfolio.assignment_for(IslandId(3)).is_none());
    }

    #[test]
    fn secure_cleanup_is_assigned_without_consuming_the_three_offensive_slots() {
        let mut secure = secure_candidate(4);
        secure.requirement.capture_units = 2;
        secure.requirement.total_budget = 2_000;
        secure.assessment.neutral_properties = 2;
        let portfolio = allocate_campaign_portfolio(
            vec![
                expansion_candidate(0, 4),
                expansion_candidate(1, 5),
                expansion_candidate(2, 6),
                expansion_candidate(3, 7),
                secure,
            ],
            CampaignResourcePool {
                available_funds: 20_000,
                units: Vec::new(),
            },
        );

        let secure = portfolio
            .assignment_for(IslandId(4))
            .expect("Secure cleanup must remain an actionable assignment");
        assert_eq!(secure.decision, IslandCampaignDecision::Secure);
        assert_eq!(secure.purchase_shortfall.capture_units, 2);
        assert_eq!(
            portfolio
                .active_offensives
                .iter()
                .filter(|assignment| assignment.decision != IslandCampaignDecision::Secure)
                .count(),
            MAX_ACTIVE_OFFENSIVES
        );
        assert!(portfolio.assignment_for(IslandId(3)).is_none());
    }

    #[test]
    fn secure_remote_capture_unit_requests_compatible_transport() {
        let capture = Entity::from_raw(151);
        let mut secure = secure_candidate(0);
        secure.requirement.capture_units = 1;
        secure.requirement.total_budget = 1_000;
        secure.assessment.neutral_properties = 1;
        let mut remote_capture = unit_candidate(151, UnitType::Infantry, 1_000, true, 0);
        remote_capture.island_id = Some(IslandId(1));
        remote_capture.reachable_positions.clear();

        let portfolio = allocate_campaign_portfolio(
            vec![secure],
            CampaignResourcePool {
                available_funds: 4_000,
                units: vec![remote_capture],
            },
        );

        let assignment = portfolio.assignment_for(IslandId(0)).unwrap();
        assert_eq!(assignment.capture_entities, vec![capture]);
        assert_eq!(
            assignment.purchase_shortfall.preferred_transport,
            Some(UnitType::TransportHelicopter)
        );
        assert_eq!(assignment.purchase_shortfall.transport_slots, 1);
        assert_eq!(assignment.purchase_shortfall.total_budget, 4_000);
        assert!(!assignment.operation_ready);
    }

    #[test]
    fn secure_uses_new_resources_before_a_forming_assault_adds_reinforcements() {
        let capture = Entity::from_raw(151);
        let transport = Entity::from_raw(152);
        let assault_guard = Entity::from_raw(153);

        let mut secure = secure_candidate(2);
        secure.requirement.capture_units = 1;
        secure.requirement.total_budget = 1_000;
        secure.assessment.neutral_properties = 1;

        let mut forming_assault = assault_candidate(9);
        forming_assault.requirement.transport_slots = 2;
        forming_assault.requirement.capture_units = 1;
        forming_assault.requirement.total_budget = 15_200;
        forming_assault.assault_transport_types = vec![UnitType::TransportHelicopter];
        forming_assault.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(9),
            target_position: forming_assault.target_position,
            transport_phase: None,
            is_forming: true,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: vec![assault_guard],
        });

        let mut remote_capture = unit_candidate(151, UnitType::Infantry, 1_000, true, 0);
        remote_capture.island_id = Some(IslandId(0));
        remote_capture.reachable_positions.clear();
        let mut free_transport =
            unit_candidate(152, UnitType::TransportHelicopter, 4_000, false, 2);
        free_transport.island_id = Some(IslandId(0));
        let mut assigned_guard = unit_candidate(153, UnitType::Tank, 10_000, false, 0);
        assigned_guard.island_id = Some(IslandId(0));
        assigned_guard.assigned_island = Some(IslandId(9));

        let portfolio = allocate_campaign_portfolio(
            vec![forming_assault, secure],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![remote_capture, free_transport, assigned_guard],
            },
        );

        let secure_assignment = portfolio.assignment_for(IslandId(2)).unwrap();
        assert_eq!(secure_assignment.capture_entities, vec![capture]);
        assert_eq!(secure_assignment.transport_entities, vec![transport]);
        assert!(secure_assignment.operation_ready);

        let assault_assignment = portfolio
            .assignment_for(IslandId(9))
            .expect("forming assault keeps its dedicated entities and future shortfall");
        assert_eq!(assault_assignment.combat_entities, vec![assault_guard]);
        assert!(!assault_assignment.capture_entities.contains(&capture));
        assert!(!assault_assignment.transport_entities.contains(&transport));
        assert!(!assault_assignment.operation_ready);
    }

    /// 未着手の敵本土よりも、投資回収可能な中立島の争奪を先に進める。
    #[test]
    fn allocates_neutral_expansions_before_new_assault() {
        let portfolio = allocate_campaign_portfolio(
            vec![
                expansion_candidate(2, 6),
                expansion_candidate(1, 5),
                assault_candidate(9),
                expansion_candidate(0, 4),
            ],
            CampaignResourcePool {
                available_funds: 45_000,
                units: Vec::new(),
            },
        );

        assert_eq!(
            portfolio
                .active_offensives
                .iter()
                .map(|assignment| assignment.island_id)
                .collect::<Vec<_>>(),
            vec![IslandId(0), IslandId(1), IslandId(2)]
        );
        assert!(portfolio.assignment_for(IslandId(9)).is_none());
    }

    /// 未着手Assaultの積立より、回収可能な中立島パッケージを先に完成させる。
    #[test]
    fn underfunded_new_assault_does_not_starve_neutral_expansions() {
        let portfolio = allocate_campaign_portfolio(
            vec![
                expansion_candidate(1, 5),
                assault_candidate(9),
                expansion_candidate(0, 4),
            ],
            CampaignResourcePool {
                available_funds: 14_000,
                units: Vec::new(),
            },
        );

        assert_eq!(portfolio.active_offensives.len(), 2);
        assert_eq!(
            portfolio
                .active_offensives
                .iter()
                .map(|assignment| assignment.island_id)
                .collect::<Vec<_>>(),
            vec![IslandId(0), IslandId(1)]
        );
        assert!(portfolio.assignment_for(IslandId(9)).is_none());
    }

    /// Combatだけを要求する防衛は価格予約を作らず、兵站作戦の構造予算を奪わない。
    #[test]
    fn combat_only_defense_does_not_preempt_structural_campaign_funds() {
        let portfolio = allocate_campaign_portfolio(
            vec![
                assault_candidate(9),
                defense_candidate(10, 1, 1),
                defense_candidate(11, 1, 1),
            ],
            CampaignResourcePool {
                available_funds: 14_000,
                units: Vec::new(),
            },
        );

        assert_eq!(portfolio.active_offensives.len(), 1);
        assert_eq!(portfolio.defenses.len(), 2);
        assert_eq!(portfolio.defenses[0].island_id, IslandId(10));
        assert_eq!(portfolio.defenses[0].allocated_budget, 0);
    }

    #[test]
    fn combat_entity_price_does_not_credit_missing_transport_or_capture_structure() {
        let combat = unit_candidate(99, UnitType::Tank, 7_000, false, 0);
        let portfolio = allocate_campaign_portfolio(
            vec![assault_candidate(9)],
            CampaignResourcePool {
                available_funds: 22_500,
                units: vec![combat],
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert_eq!(assignment.combat_entities, vec![Entity::from_raw(99)]);
        assert_eq!(assignment.purchase_shortfall.combat_units, 1);
        assert_eq!(assignment.purchase_shortfall.transport_slots, 4);
        assert_eq!(assignment.purchase_shortfall.capture_units, 2);
        assert_eq!(assignment.purchase_shortfall.total_budget, 22_500);
    }

    #[test]
    fn allocates_each_transport_entity_to_only_one_island() {
        let candidates = vec![expansion_candidate(0, 4), expansion_candidate(1, 5)];
        let units = vec![
            unit_candidate(10, UnitType::TransportHelicopter, 4_000, false, 2),
            unit_candidate(11, UnitType::Infantry, 1_000, true, 0),
            unit_candidate(12, UnitType::Infantry, 1_000, true, 0),
            unit_candidate(13, UnitType::Infantry, 1_000, true, 0),
            unit_candidate(14, UnitType::Infantry, 1_000, true, 0),
        ];

        let portfolio = allocate_campaign_portfolio(
            candidates,
            CampaignResourcePool {
                available_funds: 4_000,
                units,
            },
        );

        let assigned_ids: Vec<u64> = portfolio
            .active_offensives
            .iter()
            .flat_map(|assignment| assignment.transport_entities.iter())
            .map(|entity| entity.to_bits())
            .collect();
        let unique_ids: HashSet<u64> = assigned_ids.iter().copied().collect();
        assert_eq!(assigned_ids.len(), unique_ids.len());
        assert_eq!(assigned_ids, vec![Entity::from_raw(10).to_bits()]);
        let capture_ids: Vec<u64> = portfolio
            .active_offensives
            .iter()
            .flat_map(|assignment| assignment.capture_entities.iter())
            .map(|entity| entity.to_bits())
            .collect();
        let unique_capture_ids: HashSet<u64> = capture_ids.iter().copied().collect();
        assert_eq!(capture_ids.len(), unique_capture_ids.len());
        assert_eq!(portfolio.active_offensives.len(), 2);
    }

    fn secure_candidate(island: usize) -> IslandCampaignCandidate {
        IslandCampaignCandidate {
            assessment: allocation_assessment(
                island,
                IslandCampaignState::Secured,
                IslandCampaignDecision::Secure,
            ),
            target_position: GridPosition { x: island, y: 0 },
            capture_target_positions: vec![GridPosition { x: island, y: 0 }],
            roi_production_sites: 0,
            transport_eta: Some(0),
            ground_sustainment_sites: 1,
            air_sustainment_sites: 0,
            sea_sustainment_sites: 0,
            sustainment_targets: CampaignSustainmentTargets::default(),
            island_income_per_turn: 1_000,
            logistics_prerequisite: false,
            logistics_priority_rank: None,
            priority_enemy_types: Vec::new(),
            requirement: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            assault_transport_types: Vec::new(),
            producible_transports: test_transport_blueprints(),
            existing_operation: None,
        }
    }

    fn contest_candidate(island: usize, friendly_units: u32) -> IslandCampaignCandidate {
        let mut assessment = allocation_assessment(
            island,
            IslandCampaignState::Contested,
            IslandCampaignDecision::Contest,
        );
        assessment.friendly_combat_units = friendly_units;
        assessment.enemy_combat_units = friendly_units;
        assessment.friendly_capture_eta = Some(1);
        assessment.enemy_capture_eta = Some(1);
        IslandCampaignCandidate {
            assessment,
            target_position: GridPosition { x: island, y: 1 },
            capture_target_positions: vec![GridPosition { x: island, y: 1 }],
            roi_production_sites: 0,
            transport_eta: Some(0),
            ground_sustainment_sites: 0,
            air_sustainment_sites: 0,
            sea_sustainment_sites: 0,
            sustainment_targets: CampaignSustainmentTargets::default(),
            island_income_per_turn: 0,
            logistics_prerequisite: false,
            logistics_priority_rank: None,
            priority_enemy_types: Vec::new(),
            requirement: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            assault_transport_types: Vec::new(),
            producible_transports: test_transport_blueprints(),
            existing_operation: None,
        }
    }

    fn reinforcement_candidate(island: usize, required_units: u32) -> IslandCampaignCandidate {
        let mut assessment = allocation_assessment(
            island,
            IslandCampaignState::Contested,
            IslandCampaignDecision::Reinforce,
        );
        assessment.enemy_combat_units = required_units;
        assessment.required_budget = 0;
        IslandCampaignCandidate {
            assessment,
            target_position: GridPosition { x: island, y: 1 },
            capture_target_positions: vec![GridPosition { x: island, y: 1 }],
            roi_production_sites: 0,
            transport_eta: Some(0),
            ground_sustainment_sites: 0,
            air_sustainment_sites: 0,
            sea_sustainment_sites: 0,
            sustainment_targets: CampaignSustainmentTargets::default(),
            island_income_per_turn: 0,
            logistics_prerequisite: false,
            logistics_priority_rank: None,
            priority_enemy_types: Vec::new(),
            requirement: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: required_units,
                total_budget: 0,
            },
            assault_transport_types: Vec::new(),
            producible_transports: test_transport_blueprints(),
            existing_operation: None,
        }
    }

    fn assault_candidate(island: usize) -> IslandCampaignCandidate {
        let mut assessment = allocation_assessment(
            island,
            IslandCampaignState::EnemyHeld,
            IslandCampaignDecision::Assault,
        );
        assessment.enemy_combat_units = 1;
        assessment.required_budget = 22_500;
        IslandCampaignCandidate {
            assessment,
            target_position: GridPosition { x: island, y: 1 },
            capture_target_positions: vec![GridPosition { x: island, y: 1 }],
            roi_production_sites: 0,
            transport_eta: Some(2),
            ground_sustainment_sites: 1,
            air_sustainment_sites: 0,
            sea_sustainment_sites: 0,
            sustainment_targets: CampaignSustainmentTargets::default(),
            island_income_per_turn: 1_000,
            logistics_prerequisite: false,
            logistics_priority_rank: None,
            priority_enemy_types: vec![UnitType::Infantry],
            requirement: IslandCampaignRequirement {
                preferred_transport: Some(UnitType::Lander),
                transport_slots: 4,
                capture_units: 2,
                ground_combat_units: 2,
                combat_units: 2,
                // Lander 1 + TransportHelicopter 1 + Infantry 2 の構造費だけを予約する。
                total_budget: 22_500,
            },
            assault_transport_types: vec![UnitType::Lander, UnitType::TransportHelicopter],
            producible_transports: test_transport_blueprints(),
            existing_operation: None,
        }
    }

    fn defense_candidate(
        island: usize,
        enemy_eta: u32,
        enemy_units: u32,
    ) -> IslandCampaignCandidate {
        let mut assessment = allocation_assessment(
            island,
            IslandCampaignState::Threatened,
            IslandCampaignDecision::Defend,
        );
        assessment.enemy_arrival_eta = Some(enemy_eta);
        assessment.enemy_combat_units = enemy_units;
        IslandCampaignCandidate {
            assessment,
            target_position: GridPosition { x: island, y: 1 },
            capture_target_positions: vec![GridPosition { x: island, y: 1 }],
            roi_production_sites: 0,
            transport_eta: None,
            ground_sustainment_sites: 1,
            air_sustainment_sites: 0,
            sea_sustainment_sites: 0,
            sustainment_targets: CampaignSustainmentTargets::default(),
            island_income_per_turn: 1_000,
            logistics_prerequisite: false,
            logistics_priority_rank: None,
            priority_enemy_types: Vec::new(),
            requirement: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: enemy_units,
                // 戦闘編成費は具体的なRollingPlanが算出する。島要求へ価格を載せない。
                total_budget: 0,
            },
            assault_transport_types: Vec::new(),
            producible_transports: test_transport_blueprints(),
            existing_operation: None,
        }
    }

    #[test]
    fn rejects_transported_and_unreachable_entities_for_defense_reservation() {
        let mut unreachable = unit_candidate(13, UnitType::Tank, 7_000, false, 0);
        unreachable.island_id = Some(IslandId(0));
        // 同じ島の別tileへは到達できても、assignmentの正確な防衛座標へ届かなければ除外する。
        unreachable.reachable_positions = vec![GridPosition { x: 1, y: 1 }];
        let mut transported = unit_candidate(14, UnitType::Tank, 7_000, false, 0);
        transported.island_id = Some(IslandId(0));
        transported.reachable_positions = vec![GridPosition { x: 0, y: 1 }];
        transported.is_transporting = true;

        let portfolio = allocate_campaign_portfolio(
            vec![defense_candidate(0, 1, 1)],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![unreachable, transported],
            },
        );

        let assignment = &portfolio.defenses[0];
        assert!(assignment.combat_entities.is_empty());
        assert_eq!(assignment.purchase_shortfall.combat_units, 1);
        assert!(!assignment.operation_ready);
    }

    #[test]
    fn allocates_same_island_defender_that_reaches_the_exact_assignment_target() {
        let defender = Entity::from_raw(15);
        let mut reachable = unit_candidate(15, UnitType::Tank, 7_000, false, 0);
        reachable.island_id = Some(IslandId(0));
        reachable.reachable_positions = vec![GridPosition { x: 0, y: 1 }];

        let portfolio = allocate_campaign_portfolio(
            vec![defense_candidate(0, 1, 1)],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![reachable],
            },
        );

        assert_eq!(portfolio.defenses.len(), 1);
        assert_eq!(portfolio.defenses[0].combat_entities, vec![defender]);
        assert!(portfolio.defenses[0].operation_ready);
    }

    #[test]
    fn allocates_no_secure_island_capture_unit_to_an_offshore_candidate() {
        let mut local_capture = unit_candidate(15, UnitType::Infantry, 1_000, true, 0);
        local_capture.island_id = Some(IslandId(0));
        local_capture.assigned_island = Some(IslandId(1));
        local_capture.can_secure_local_property = false;
        let portfolio = allocate_campaign_portfolio(
            vec![secure_candidate(0), expansion_candidate(1, 4)],
            CampaignResourcePool {
                available_funds: 1_000,
                units: vec![
                    local_capture,
                    unit_candidate(16, UnitType::TransportHelicopter, 4_000, false, 2),
                ],
            },
        );

        assert!(portfolio.active_offensives.is_empty());
        assert_eq!(
            portfolio.islands[0].decision,
            IslandCampaignDecision::Secure
        );
        assert_eq!(
            portfolio.islands[1].decision,
            IslandCampaignDecision::Observe
        );
    }

    #[test]
    fn combat_only_defense_keeps_parallel_funded_expansions() {
        let candidates = vec![
            expansion_candidate(0, 4),
            expansion_candidate(1, 5),
            defense_candidate(2, 1, 1),
        ];

        let portfolio = allocate_campaign_portfolio(
            candidates,
            CampaignResourcePool {
                available_funds: 12_000,
                units: Vec::new(),
            },
        );

        assert_eq!(portfolio.active_offensives.len(), 2);
        assert_eq!(portfolio.active_offensives[0].island_id, IslandId(0));
        assert_eq!(portfolio.defenses.len(), 1);
        assert_eq!(portfolio.defenses[0].island_id, IslandId(2));
        assert_eq!(portfolio.defenses[0].purchase_shortfall.total_budget, 0);
    }

    #[test]
    fn allocates_no_partial_package_when_funds_are_one_gold_short() {
        let portfolio = allocate_campaign_portfolio(
            vec![expansion_candidate(0, 4)],
            CampaignResourcePool {
                available_funds: 5_999,
                units: Vec::new(),
            },
        );

        assert!(portfolio.active_offensives.is_empty());
        assert_eq!(
            portfolio.islands[0].decision,
            IslandCampaignDecision::Observe
        );
        assert_eq!(portfolio.islands[0].allocated_budget, 0);
        assert!(portfolio.assignment_for(IslandId(0)).is_none());
        assert!(portfolio.aggregate_missing_requirements().is_empty());
    }

    #[test]
    fn allocates_offshore_transport_without_counting_recon_cargo_capacity() {
        let portfolio = allocate_campaign_portfolio(
            vec![expansion_candidate(0, 4)],
            CampaignResourcePool {
                available_funds: 4_000,
                units: vec![
                    unit_candidate(20, UnitType::Recon, 4_200, false, 2),
                    unit_candidate(21, UnitType::Infantry, 1_000, true, 0),
                    unit_candidate(22, UnitType::Infantry, 1_000, true, 0),
                ],
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert!(assignment.transport_entities.is_empty());
        assert_eq!(assignment.purchase_shortfall.transport_slots, 2);
        assert_eq!(assignment.purchase_shortfall.total_budget, 4_000);
        assert!(!assignment.operation_ready);
    }

    #[test]
    fn transport_coverage_locks_loaded_cargo_and_uses_only_remaining_slots() {
        let loaded = Entity::from_raw(23);
        let waiting = Entity::from_raw(24);
        let unrelated = Entity::from_raw(25);
        let transport = Entity::from_raw(26);
        let mut loaded_unit = unit_candidate(23, UnitType::Infantry, 1_000, true, 0);
        loaded_unit.island_id = Some(IslandId(0));
        let mut waiting_unit = unit_candidate(24, UnitType::Infantry, 1_000, true, 0);
        waiting_unit.island_id = Some(IslandId(0));
        let mut unrelated_unit = unit_candidate(25, UnitType::Infantry, 1_000, true, 0);
        unrelated_unit.island_id = Some(IslandId(0));
        let mut transport_unit = unit_candidate(26, UnitType::TransportHelicopter, 4_000, false, 2);
        transport_unit.island_id = Some(IslandId(0));
        transport_unit.loaded_cargo_entities = vec![loaded];
        let catalog = [
            loaded_unit,
            waiting_unit,
            unrelated_unit,
            transport_unit.clone(),
        ]
        .into_iter()
        .map(|unit| (unit.entity, unit))
        .collect();

        assert!(campaign_transport_package_covers(
            &[loaded, waiting],
            &[transport],
            &catalog,
        ));

        let mut unrelated_transport = transport_unit;
        unrelated_transport.loaded_cargo_entities = vec![unrelated];
        let unrelated_catalog = catalog
            .into_iter()
            .map(|(entity, unit)| {
                if entity == transport {
                    (entity, unrelated_transport.clone())
                } else {
                    (entity, unit)
                }
            })
            .collect();
        assert!(!campaign_transport_package_covers(
            &[loaded, waiting],
            &[transport],
            &unrelated_catalog,
        ));
    }

    #[test]
    fn allocates_deterministically_for_reversed_candidate_and_unit_order() {
        let candidates = vec![
            expansion_candidate(3, 7),
            expansion_candidate(1, 5),
            expansion_candidate(0, 4),
            expansion_candidate(2, 6),
        ];
        let units = vec![
            unit_candidate(30, UnitType::TransportHelicopter, 4_000, false, 2),
            unit_candidate(31, UnitType::Infantry, 1_000, true, 0),
            unit_candidate(32, UnitType::Infantry, 1_000, true, 0),
        ];
        let expected = allocate_campaign_portfolio(
            candidates.clone(),
            CampaignResourcePool {
                available_funds: 18_000,
                units: units.clone(),
            },
        );
        let mut reversed_candidates = candidates;
        reversed_candidates.reverse();
        let mut reversed_units = units;
        reversed_units.reverse();

        let actual = allocate_campaign_portfolio(
            reversed_candidates,
            CampaignResourcePool {
                available_funds: 18_000,
                units: reversed_units,
            },
        );

        assert_eq!(actual, expected);
    }

    #[test]
    fn allocates_units_already_assigned_to_the_same_target_only() {
        let mut same_target_transport =
            unit_candidate(35, UnitType::TransportHelicopter, 4_000, false, 2);
        same_target_transport.assigned_island = Some(IslandId(0));
        let mut other_target_capture = unit_candidate(36, UnitType::Infantry, 1_000, true, 0);
        other_target_capture.assigned_island = Some(IslandId(9));
        let portfolio = allocate_campaign_portfolio(
            vec![expansion_candidate(0, 4)],
            CampaignResourcePool {
                available_funds: 1_000,
                units: vec![
                    same_target_transport,
                    other_target_capture,
                    unit_candidate(37, UnitType::Infantry, 1_000, true, 0),
                ],
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert_eq!(assignment.transport_entities, vec![Entity::from_raw(35)]);
        assert_eq!(assignment.capture_entities, vec![Entity::from_raw(37)]);
        assert_eq!(assignment.purchase_shortfall.capture_units, 1);
        assert!(!assignment.capture_entities.contains(&Entity::from_raw(36)));
    }

    #[test]
    fn allocates_existing_capture_member_as_defense_combat_unit() {
        let infantry = Entity::from_raw(41);
        let mut candidate = defense_candidate(0, 1, 1);
        candidate.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(0),
            target_position: candidate.target_position,
            transport_phase: None,
            is_forming: false,
            transport_entities: Vec::new(),
            capture_entities: vec![infantry],
            combat_entities: Vec::new(),
        });
        let mut unit = unit_candidate(41, UnitType::Infantry, 1_000, true, 0);
        unit.island_id = Some(IslandId(0));
        unit.assigned_island = Some(IslandId(0));

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![unit],
            },
        );

        let assignment = &portfolio.defenses[0];
        assert_eq!(assignment.capture_entities, vec![infantry]);
        assert!(assignment.combat_entities.is_empty());
        assert_eq!(assignment.purchase_shortfall.combat_units, 0);
        assert_eq!(assignment.purchase_shortfall.total_budget, 0);
        assert!(assignment.operation_ready);
    }

    #[test]
    fn allocates_small_combat_remainder_with_a_real_unit_purchase_budget() {
        let infantry = Entity::from_raw(45);
        let mut candidate = defense_candidate(0, 1, 2);
        candidate.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(0),
            target_position: candidate.target_position,
            transport_phase: None,
            is_forming: false,
            transport_entities: Vec::new(),
            capture_entities: vec![infantry],
            combat_entities: Vec::new(),
        });
        let mut unit = unit_candidate(45, UnitType::Infantry, 1_000, true, 0);
        unit.island_id = Some(IslandId(0));
        unit.assigned_island = Some(IslandId(0));

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 1_000,
                units: vec![unit],
            },
        );

        let assignment = &portfolio.defenses[0];
        assert_eq!(assignment.purchase_shortfall.combat_units, 1);
        assert_eq!(assignment.purchase_shortfall.total_budget, 0);
        assert!(!assignment.operation_ready);
    }

    #[test]
    fn rejects_combat_purchase_when_no_eligible_unit_is_producible() {
        let candidate = defense_candidate(0, 1, 1);

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 1_080,
                units: Vec::new(),
            },
        );

        let assignment = &portfolio.defenses[0];
        assert_eq!(assignment.purchase_shortfall.combat_units, 1);
        assert_eq!(assignment.purchase_shortfall.total_budget, 0);
        assert!(!assignment.operation_ready);
    }

    #[test]
    fn allocates_existing_capture_member_as_reinforcement_combat_unit() {
        let infantry = Entity::from_raw(42);
        let mut candidate = reinforcement_candidate(0, 1);
        candidate.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(0),
            target_position: candidate.target_position,
            transport_phase: None,
            is_forming: false,
            transport_entities: Vec::new(),
            capture_entities: vec![infantry],
            combat_entities: Vec::new(),
        });
        let mut unit = unit_candidate(42, UnitType::Infantry, 1_000, true, 0);
        unit.island_id = Some(IslandId(0));
        unit.assigned_island = Some(IslandId(0));

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![unit],
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert_eq!(assignment.decision, IslandCampaignDecision::Reinforce);
        assert_eq!(assignment.capture_entities, vec![infantry]);
        assert!(assignment.combat_entities.is_empty());
        assert_eq!(assignment.purchase_shortfall.combat_units, 0);
        assert_eq!(assignment.purchase_shortfall.total_budget, 0);
        assert!(assignment.operation_ready);
    }

    /// 別島の増援戦力に輸送役が無い場合も作戦を消さず、Lander不足として生産へ返す。
    #[test]
    fn reinforcement_without_transport_reports_heavy_transport_shortfall() {
        let tank = Entity::from_raw(142);
        let candidate = reinforcement_candidate(0, 1);
        let mut remote_tank = unit_candidate(142, UnitType::Tank, 7_000, false, 0);
        remote_tank.island_id = Some(IslandId(1));
        remote_tank.reachable_positions.clear();

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 16_500,
                units: vec![remote_tank],
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert_eq!(assignment.decision, IslandCampaignDecision::Reinforce);
        assert_eq!(assignment.combat_entities, vec![tank]);
        assert_eq!(
            assignment.purchase_shortfall.preferred_transport,
            Some(UnitType::Lander)
        );
        assert_eq!(assignment.purchase_shortfall.transport_slots, 1);
        assert_eq!(assignment.purchase_shortfall.total_budget, 16_500);
        assert!(!assignment.operation_ready);
    }

    /// 目標島にいる増援は陸路で合流できるため、輸送shortfallを立てない。
    #[test]
    fn local_reinforcement_does_not_request_transport() {
        let tank = Entity::from_raw(143);
        let candidate = reinforcement_candidate(0, 1);
        let mut local_tank = unit_candidate(143, UnitType::Tank, 7_000, false, 0);
        local_tank.island_id = Some(IslandId(0));

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![local_tank],
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert_eq!(assignment.combat_entities, vec![tank]);
        assert_eq!(assignment.purchase_shortfall.transport_slots, 0);
        assert_eq!(assignment.purchase_shortfall.preferred_transport, None);
        assert!(assignment.operation_ready);
    }

    #[test]
    fn allocates_competitive_contest_assets_before_expand_can_consume_them() {
        let contested_infantry = Entity::from_raw(43);
        let mut infantry = unit_candidate(43, UnitType::Infantry, 1_000, true, 0);
        infantry.island_id = Some(IslandId(1));

        let candidates = vec![expansion_candidate(0, 4), contest_candidate(1, 1)];
        let units = vec![
            unit_candidate(44, UnitType::TransportHelicopter, 4_000, false, 2),
            infantry,
        ];
        let portfolio = allocate_campaign_portfolio(
            candidates.clone(),
            CampaignResourcePool {
                available_funds: 1_000,
                units: units.clone(),
            },
        );
        let mut reversed_candidates = candidates;
        reversed_candidates.reverse();
        let mut reversed_units = units;
        reversed_units.reverse();
        let reversed = allocate_campaign_portfolio(
            reversed_candidates,
            CampaignResourcePool {
                available_funds: 1_000,
                units: reversed_units,
            },
        );

        assert_eq!(reversed, portfolio);
        assert!(portfolio.assignment_for(IslandId(0)).is_none());
        let contest = portfolio.assignment_for(IslandId(1)).unwrap();
        assert_eq!(contest.decision, IslandCampaignDecision::Contest);
        assert_eq!(contest.capture_entities, vec![contested_infantry]);
        assert!(contest.operation_ready);
    }

    #[test]
    fn prioritizes_live_contest_then_preempts_expansion_for_defense() {
        let mut contest_asset = unit_candidate(52, UnitType::Tank, 10_000, false, 0);
        contest_asset.island_id = Some(IslandId(3));
        let candidates = vec![
            expansion_candidate(0, 4),
            expansion_candidate(1, 5),
            expansion_candidate(2, 6),
            contest_candidate(3, 1),
            defense_candidate(4, 1, 1),
        ];

        let portfolio = allocate_campaign_portfolio(
            candidates,
            CampaignResourcePool {
                available_funds: 18_000,
                units: vec![contest_asset],
            },
        );

        assert_eq!(
            portfolio
                .active_offensives
                .iter()
                .map(|assignment| assignment.island_id)
                .collect::<Vec<_>>(),
            vec![IslandId(3), IslandId(0), IslandId(1)]
        );
        let contest = portfolio
            .islands
            .iter()
            .find(|assessment| assessment.island_id == IslandId(3))
            .unwrap();
        assert_eq!(contest.decision, IslandCampaignDecision::Contest);
        let defense = &portfolio.defenses[0];
        assert!(defense.combat_entities.is_empty());
        assert_eq!(defense.purchase_shortfall.total_budget, 0);
        assert!(!defense.operation_ready);
    }

    #[test]
    fn keeps_understrength_contest_for_rolling_plan_reinforcement() {
        let mut contest = contest_candidate(0, 1);
        contest.requirement.combat_units = 2;
        contest.requirement.total_budget = 0;
        let mut contest_asset = unit_candidate(53, UnitType::Infantry, 1_000, true, 0);
        contest_asset.island_id = Some(IslandId(0));

        let portfolio = allocate_campaign_portfolio(
            vec![contest, defense_candidate(1, 1, 1)],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![contest_asset],
            },
        );

        assert_eq!(portfolio.active_offensives.len(), 1);
        assert_eq!(
            portfolio.islands[0].decision,
            IslandCampaignDecision::Contest
        );
        let defense = &portfolio.defenses[0];
        assert!(defense.combat_entities.is_empty());
        assert!(portfolio.active_offensives[0].combat_entities.is_empty());
        assert_eq!(
            portfolio.active_offensives[0]
                .purchase_shortfall
                .combat_units,
            1
        );
        assert_eq!(defense.purchase_shortfall.total_budget, 0);
        assert!(!defense.operation_ready);
    }

    #[test]
    fn contest_ignores_remote_combat_entity_without_a_transport_route() {
        let mut contest = contest_candidate(0, 1);
        contest.requirement.combat_units = 1;
        contest.producible_transports.clear();
        let mut stranded_tank = unit_candidate(90, UnitType::Tank, 7_000, false, 0);
        stranded_tank.island_id = Some(IslandId(1));
        stranded_tank.reachable_positions.clear();

        let portfolio = allocate_campaign_portfolio(
            vec![contest],
            CampaignResourcePool {
                available_funds: 20_000,
                units: vec![stranded_tank],
            },
        );

        let assignment = portfolio
            .assignment_for(IslandId(0))
            .expect("積載不能Entityだけを理由に継続中Contestを消さない");
        assert!(assignment.combat_entities.is_empty());
        assert_eq!(assignment.purchase_shortfall.combat_units, 1);
    }

    #[test]
    fn allocates_live_lander_without_satisfying_expand_helicopter_requirement() {
        let lander = Entity::from_raw(45);
        let capture_a = Entity::from_raw(46);
        let capture_b = Entity::from_raw(47);
        let mut candidate = expansion_candidate(0, 4);
        candidate.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(0),
            target_position: candidate.target_position,
            transport_phase: Some(TransportPhase::Transit),
            is_forming: false,
            transport_entities: vec![lander],
            capture_entities: vec![capture_a, capture_b],
            combat_entities: Vec::new(),
        });
        let mut lander_unit = unit_candidate(45, UnitType::Lander, 16_500, false, 2);
        lander_unit.assigned_island = Some(IslandId(0));
        let mut capture_unit_a = unit_candidate(46, UnitType::Infantry, 1_000, true, 0);
        capture_unit_a.assigned_island = Some(IslandId(0));
        let mut capture_unit_b = unit_candidate(47, UnitType::Infantry, 1_000, true, 0);
        capture_unit_b.assigned_island = Some(IslandId(0));

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 4_000,
                units: vec![lander_unit, capture_unit_a, capture_unit_b],
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert_eq!(assignment.transport_entities, vec![lander]);
        assert_eq!(
            assignment.purchase_shortfall.preferred_transport,
            Some(UnitType::TransportHelicopter)
        );
        assert_eq!(assignment.purchase_shortfall.transport_slots, 2);
        assert_eq!(assignment.purchase_shortfall.total_budget, 4_000);
        assert!(!assignment.operation_ready);
    }

    #[test]
    fn allocates_assault_capture_units_without_double_crediting_combat_units() {
        let lander = Entity::from_raw(48);
        let helicopter = Entity::from_raw(49);
        let capture_a = Entity::from_raw(50);
        let capture_b = Entity::from_raw(51);
        let mut candidate = assault_candidate(0);
        candidate.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(0),
            target_position: candidate.target_position,
            transport_phase: Some(TransportPhase::Transit),
            is_forming: false,
            transport_entities: vec![lander, helicopter],
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
        });
        let mut lander_unit = unit_candidate(48, UnitType::Lander, 16_500, false, 2);
        lander_unit.assigned_island = Some(IslandId(0));
        let mut helicopter_unit =
            unit_candidate(49, UnitType::TransportHelicopter, 4_000, false, 2);
        helicopter_unit.assigned_island = Some(IslandId(0));
        let units = vec![
            lander_unit,
            helicopter_unit,
            unit_candidate(50, UnitType::Infantry, 1_000, true, 0),
            unit_candidate(51, UnitType::Infantry, 1_000, true, 0),
        ];

        let underfunded = allocate_campaign_portfolio(
            vec![candidate.clone()],
            CampaignResourcePool {
                available_funds: 8_200,
                units: units.clone(),
            },
        );
        let underfunded_assignment = &underfunded.active_offensives[0];
        assert_eq!(
            underfunded_assignment.decision,
            IslandCampaignDecision::Assault
        );
        assert_eq!(underfunded_assignment.allocated_budget, 22_500);
        assert_eq!(underfunded_assignment.purchase_shortfall.combat_units, 2);
        assert!(!underfunded_assignment.operation_ready);

        let funded = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 10_200,
                units,
            },
        );
        let assignment = &funded.active_offensives[0];
        assert_eq!(assignment.capture_entities, vec![capture_a, capture_b]);
        assert!(assignment.combat_entities.is_empty());
        assert_eq!(assignment.purchase_shortfall.capture_units, 0);
        assert_eq!(assignment.purchase_shortfall.combat_units, 2);
        assert_eq!(assignment.purchase_shortfall.total_budget, 0);
        assert_eq!(assignment.allocated_budget, 22_500);
        assert!(!assignment.operation_ready);
    }

    #[test]
    fn assault_reserves_only_remote_combat_that_fits_with_capture_cargo() {
        let mut units = vec![
            unit_candidate(60, UnitType::Lander, 16_500, false, 2),
            unit_candidate(61, UnitType::TransportHelicopter, 4_000, false, 2),
            unit_candidate(62, UnitType::Infantry, 1_000, true, 0),
            unit_candidate(63, UnitType::Infantry, 1_000, true, 0),
        ];
        units
            .extend((64..69).map(|entity| unit_candidate(entity, UnitType::Mech, 3_000, false, 0)));
        for unit in &mut units {
            unit.island_id = Some(IslandId(0));
            unit.reachable_positions.clear();
        }

        let portfolio = allocate_campaign_portfolio(
            vec![assault_candidate(9)],
            CampaignResourcePool {
                available_funds: 10_200,
                units,
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert_eq!(assignment.capture_entities.len(), 2);
        assert_eq!(assignment.combat_entities.len(), 2);
        assert_eq!(assignment.purchase_shortfall.ground_combat_units, 0);
        assert_eq!(assignment.purchase_shortfall.combat_units, 0);
        assert!(assignment.operation_ready);
    }

    #[test]
    fn live_assault_wave_does_not_add_late_combat_cargo() {
        let lander = Entity::from_raw(70);
        let helicopter = Entity::from_raw(71);
        let capture_a = Entity::from_raw(72);
        let capture_b = Entity::from_raw(73);
        let mut candidate = assault_candidate(9);
        candidate.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(9),
            target_position: candidate.target_position,
            transport_phase: Some(TransportPhase::Pickup),
            is_forming: false,
            transport_entities: vec![lander, helicopter],
            capture_entities: vec![capture_a, capture_b],
            combat_entities: Vec::new(),
        });
        let mut units = vec![
            unit_candidate(70, UnitType::Lander, 16_500, false, 2),
            unit_candidate(71, UnitType::TransportHelicopter, 4_000, false, 2),
            unit_candidate(72, UnitType::Infantry, 1_000, true, 0),
            unit_candidate(73, UnitType::Infantry, 1_000, true, 0),
            unit_candidate(74, UnitType::Mech, 3_000, false, 0),
        ];
        for unit in &mut units {
            unit.island_id = Some(IslandId(0));
            unit.assigned_island = Some(IslandId(9));
            unit.reachable_positions.clear();
        }

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 10_200,
                units,
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert!(assignment.combat_entities.is_empty());
        assert_eq!(assignment.purchase_shortfall.ground_combat_units, 0);
        assert_eq!(assignment.purchase_shortfall.combat_units, 2);
        assert!(!assignment.operation_ready);
    }

    #[test]
    fn allocates_no_return_phase_transport_as_existing_or_held_capacity() {
        let transport = Entity::from_raw(38);
        let mut candidate = expansion_candidate(0, 4);
        candidate.existing_operation = Some(ExistingCampaignOperation {
            island_id: IslandId(0),
            target_position: candidate.target_position,
            transport_phase: Some(TransportPhase::Return),
            is_forming: false,
            transport_entities: vec![transport],
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
        });
        let mut returning_transport =
            unit_candidate(38, UnitType::TransportHelicopter, 4_000, false, 2);
        returning_transport.assigned_island = Some(IslandId(0));

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![
                    returning_transport,
                    unit_candidate(39, UnitType::Infantry, 1_000, true, 0),
                    unit_candidate(40, UnitType::Infantry, 1_000, true, 0),
                ],
            },
        );

        assert!(portfolio.active_offensives.is_empty());
        assert_eq!(
            portfolio.islands[0].decision,
            IslandCampaignDecision::Observe
        );
    }

    #[test]
    fn allocates_no_defense_combat_unit_from_support_trucks() {
        let portfolio = allocate_campaign_portfolio(
            vec![defense_candidate(0, 1, 1)],
            CampaignResourcePool {
                available_funds: 0,
                units: vec![unit_candidate(40, UnitType::SupplyTruck, 5_000, false, 1)],
            },
        );

        let assignment = &portfolio.defenses[0];
        assert!(assignment.combat_entities.is_empty());
        assert_eq!(assignment.purchase_shortfall.combat_units, 1);
        assert!(!assignment.operation_ready);
    }

    fn assignment_with_shortfall(
        island: usize,
        decision: IslandCampaignDecision,
        continued: bool,
        target_position: GridPosition,
    ) -> IslandCampaignAssignment {
        IslandCampaignAssignment {
            island_id: IslandId(island),
            decision,
            target_position,
            capture_target_positions: vec![target_position],
            priority_enemy_types: Vec::new(),
            requirement: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 1,
                total_budget: 0,
            },
            purchase_shortfall: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 1,
                total_budget: 0,
            },
            allocated_budget: 0,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: continued,
        }
    }

    #[test]
    fn portfolio_aggregates_missing_requirements_in_production_priority_order() {
        let portfolio = IslandCampaignPortfolio {
            islands: Vec::new(),
            active_offensives: vec![
                assignment_with_shortfall(
                    5,
                    IslandCampaignDecision::Secure,
                    false,
                    GridPosition { x: 5, y: 0 },
                ),
                assignment_with_shortfall(
                    4,
                    IslandCampaignDecision::Assault,
                    false,
                    GridPosition { x: 4, y: 0 },
                ),
                assignment_with_shortfall(
                    2,
                    IslandCampaignDecision::Expand,
                    false,
                    GridPosition { x: 2, y: 0 },
                ),
                assignment_with_shortfall(
                    1,
                    IslandCampaignDecision::Assault,
                    true,
                    GridPosition { x: 1, y: 0 },
                ),
                assignment_with_shortfall(
                    3,
                    IslandCampaignDecision::Reinforce,
                    false,
                    GridPosition { x: 3, y: 0 },
                ),
            ],
            defenses: vec![assignment_with_shortfall(
                0,
                IslandCampaignDecision::Defend,
                false,
                GridPosition { x: 0, y: 0 },
            )],
        };

        assert_eq!(
            portfolio
                .aggregate_missing_requirements()
                .iter()
                .map(|shortfall| (shortfall.priority_rank, shortfall.island_id))
                .collect::<Vec<_>>(),
            vec![
                (0, IslandId(0)),
                (2, IslandId(5)),
                (4, IslandId(3)),
                (6, IslandId(2)),
                (7, IslandId(1)),
                (8, IslandId(4)),
            ]
        );
    }

    #[test]
    fn ready_assault_delegates_remaining_combat_to_generic_operations() {
        let mut assault = assignment_with_shortfall(
            4,
            IslandCampaignDecision::Assault,
            true,
            GridPosition { x: 4, y: 0 },
        );
        assault.operation_ready = true;
        let portfolio = IslandCampaignPortfolio {
            islands: Vec::new(),
            active_offensives: vec![assault],
            defenses: Vec::new(),
        };

        assert!(portfolio.aggregate_missing_requirements().is_empty());
    }

    #[test]
    fn portfolio_reports_both_missing_assault_transport_types() {
        let mut assault = assignment_with_shortfall(
            4,
            IslandCampaignDecision::Assault,
            false,
            GridPosition { x: 4, y: 0 },
        );
        assault.purchase_shortfall.preferred_transport = Some(UnitType::Lander);
        assault.purchase_shortfall.transport_slots = 4;
        assault.purchase_shortfall.capture_units = 2;
        assault.purchase_shortfall.combat_units = 2;
        assault.purchase_shortfall.total_budget = 22_500;
        let portfolio = IslandCampaignPortfolio {
            islands: Vec::new(),
            active_offensives: vec![assault],
            defenses: Vec::new(),
        };

        let shortfall = &portfolio.aggregate_missing_requirements()[0];

        assert_eq!(shortfall.light_transport_slots, 2);
        assert_eq!(shortfall.heavy_transport_slots, 2);
        assert_eq!(shortfall.reserved_budget, 22_500);
    }

    #[test]
    fn portfolio_returns_deduplicated_targets_in_assignment_order() {
        let duplicate = GridPosition { x: 2, y: 3 };
        let portfolio = IslandCampaignPortfolio {
            islands: Vec::new(),
            active_offensives: vec![
                assignment_with_shortfall(1, IslandCampaignDecision::Expand, false, duplicate),
                assignment_with_shortfall(2, IslandCampaignDecision::Reinforce, false, duplicate),
                assignment_with_shortfall(
                    3,
                    IslandCampaignDecision::Assault,
                    false,
                    GridPosition { x: 4, y: 5 },
                ),
            ],
            defenses: Vec::new(),
        };

        assert_eq!(
            portfolio.assignment_for(IslandId(2)).unwrap().island_id,
            IslandId(2)
        );
        assert_eq!(
            portfolio.offensive_target_positions(),
            vec![duplicate, GridPosition { x: 4, y: 5 }]
        );
    }

    #[test]
    fn allocates_assault_with_lander_helicopter_capture_and_combat_minimums() {
        let mut assessment = allocation_assessment(
            0,
            IslandCampaignState::EnemyHeld,
            IslandCampaignDecision::Assault,
        );
        assessment.enemy_combat_units = 2;
        assessment.required_budget = 22_500;
        let candidate = IslandCampaignCandidate {
            assessment,
            target_position: GridPosition { x: 0, y: 0 },
            capture_target_positions: vec![GridPosition { x: 0, y: 0 }],
            roi_production_sites: 0,
            transport_eta: Some(2),
            ground_sustainment_sites: 1,
            air_sustainment_sites: 0,
            sea_sustainment_sites: 0,
            sustainment_targets: CampaignSustainmentTargets::default(),
            island_income_per_turn: 1_000,
            logistics_prerequisite: false,
            logistics_priority_rank: None,
            priority_enemy_types: vec![UnitType::Infantry],
            requirement: IslandCampaignRequirement {
                preferred_transport: Some(UnitType::Lander),
                transport_slots: 4,
                capture_units: 2,
                ground_combat_units: 2,
                combat_units: 2,
                total_budget: 22_500,
            },
            assault_transport_types: vec![UnitType::Lander, UnitType::TransportHelicopter],
            producible_transports: test_transport_blueprints(),
            existing_operation: None,
        };

        let portfolio = allocate_campaign_portfolio(
            vec![candidate],
            CampaignResourcePool {
                available_funds: 22_500,
                units: Vec::new(),
            },
        );

        let assignment = &portfolio.active_offensives[0];
        assert_eq!(
            assignment.purchase_shortfall.preferred_transport,
            Some(UnitType::Lander)
        );
        assert_eq!(assignment.purchase_shortfall.transport_slots, 4);
        assert_eq!(assignment.purchase_shortfall.capture_units, 2);
        assert_eq!(assignment.purchase_shortfall.ground_combat_units, 0);
        assert_eq!(assignment.purchase_shortfall.combat_units, 2);
        assert_eq!(assignment.purchase_shortfall.total_budget, 22_500);
        assert!(!assignment.operation_ready);
    }
}
