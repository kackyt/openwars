use crate::ai::demand::{
    AirCoverageContribution, AirDefenseAssessment, CombatCapabilitySnapshot,
    air_coverage_with_timing, candidate_air_coverage, candidate_air_coverage_with_delay,
    candidate_air_coverage_with_timing,
};
use crate::ai::island_campaign::{
    IslandCampaignDecision, IslandCampaignShortfall, campaign_unit_type_rank,
};
use crate::ai::strategy::{
    EmergencyAntiAirReservation, ProductionPlan, ProductionStrategy, analyze_strategy_for_turn,
    sea_transport_capacity_from_slots,
};
use crate::ai::turn_distance::TerrainConnectivity;
use crate::components::{
    ActionCompleted, Ammo, Faction, Fuel, GridPosition, Health, PlayerId, Property, UnitStats,
};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, MovementType, Players, Terrain, UnitRegistry, UnitType};
use bevy_ecs::prelude::*;

use super::strategy::GamePhase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CampaignProductionRequirement {
    HeavyTransport,
    LightTransport,
    Capture,
    Combat,
}

#[derive(Debug)]
pub(crate) struct CampaignProductionOutcome {
    pub(crate) commands: Vec<ProduceUnitCommand>,
    pub(crate) remaining_funds: u32,
    /// 全キャンペーン不足の予約額を残した後、汎用の迎撃・戦闘へ使ってよい額。
    pub(crate) generic_funds: u32,
    pub(crate) used_facilities: std::collections::HashSet<GridPosition>,
    pub(crate) completed_all_rows: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProductionCandidate {
    score: u32,
    facility_position: GridPosition,
    unit_type: UnitType,
    cost: u32,
    max_cargo: u32,
    can_capture: bool,
}

#[derive(Debug, Clone, Copy)]
struct EmergencyAntiAirCandidate {
    facility_position: GridPosition,
    unit_type: UnitType,
    cost: u32,
    coverage: f32,
    protected_asset_value: f32,
    meets_deadline: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct AirResponseEconomics {
    coverage_value: f32,
    protected_asset_value: f32,
}

const AIR_DEFENSE_VALUE_PREMIUM: f32 = 1.25;
const AIR_CONTAINMENT_GRACE_TURNS: u32 = 2;

/// 候補1体が減らせる脅威割合を、HP補正済みの敵航空資産価値へ換算します。
fn air_response_economics(
    assessment: &AirDefenseAssessment,
    contribution: &AirCoverageContribution,
) -> AirResponseEconomics {
    let protected_asset_value = contribution
        .by_target
        .iter()
        .copied()
        .enumerate()
        .map(|(index, added)| {
            let remaining_threat = assessment.remaining_threat_value(index);
            if remaining_threat <= AirDefenseAssessment::COVERAGE_EPSILON {
                return 0.0;
            }
            let protected_fraction = (added / remaining_threat).clamp(0.0, 1.0);
            assessment.remaining_air_asset_value(index) * protected_fraction
        })
        .sum();
    AirResponseEconomics {
        coverage_value: contribution.total,
        protected_asset_value,
    }
}

fn is_economically_justified_air_response(cost: u32, economics: AirResponseEconomics) -> bool {
    economics.coverage_value + AirDefenseAssessment::COVERAGE_EPSILON >= cost as f32
        && cost as f32
            <= economics.protected_asset_value * AIR_DEFENSE_VALUE_PREMIUM
                + AirDefenseAssessment::COVERAGE_EPSILON
}

#[allow(clippy::too_many_arguments)]
fn build_air_containment_assessment(
    strategy: &ProductionStrategy,
    existing_units: &[CombatCapabilitySnapshot],
    master_data: &MasterDataRegistry,
    map: &crate::resources::Map,
    unit_positions: &std::collections::HashMap<
        (usize, usize),
        crate::systems::movement::OccupantInfo,
    >,
    damage_chart: &DamageChart,
) -> AirDefenseAssessment {
    let mut containment = strategy.air_defense.uncovered_emergency_targets_only();
    let capable_units = existing_units
        .iter()
        .copied()
        .filter(|unit| {
            containment.targets.iter().any(|target| {
                damage_chart
                    .get_base_damage(unit.unit_type, target.unit_type)
                    .unwrap_or(0)
                    .max(
                        damage_chart
                            .get_base_damage_secondary(unit.unit_type, target.unit_type)
                            .unwrap_or(0),
                    )
                    > 0
            })
        })
        .collect::<Vec<_>>();
    let contribution = air_coverage_with_timing(
        &capable_units,
        &containment,
        map,
        master_data,
        unit_positions,
        damage_chart,
        AIR_CONTAINMENT_GRACE_TURNS,
    );
    containment.apply_coverage(&contribution);
    containment
}

#[allow(clippy::too_many_arguments)]
fn select_emergency_anti_air_candidate_with_existing(
    facilities: &[(GridPosition, Terrain)],
    available_types: &[(UnitType, UnitStats)],
    existing_units: &[CombatCapabilitySnapshot],
    player_id: PlayerId,
    strategy: &ProductionStrategy,
    master_data: &MasterDataRegistry,
    map: &crate::resources::Map,
    unit_positions: &std::collections::HashMap<
        (usize, usize),
        crate::systems::movement::OccupantInfo,
    >,
    damage_chart: &DamageChart,
    current_funds: u32,
) -> Option<EmergencyAntiAirCandidate> {
    let emergency_assessment = strategy.air_defense.emergency_targets_only();
    let containment_assessment = build_air_containment_assessment(
        strategy,
        existing_units,
        master_data,
        map,
        unit_positions,
        damage_chart,
    );
    let mut candidates = Vec::new();
    for (facility_position, terrain) in facilities {
        for (unit_type, stats) in available_types {
            if stats.cost == 0 || !master_data.can_produce_unit(terrain.as_str(), *unit_type) {
                continue;
            }
            let strict_coverage = candidate_air_coverage(
                stats,
                *facility_position,
                player_id,
                &emergency_assessment,
                map,
                master_data,
                unit_positions,
                damage_chart,
            );
            let containment_coverage = candidate_air_coverage_with_timing(
                stats,
                *facility_position,
                player_id,
                &containment_assessment,
                map,
                master_data,
                unit_positions,
                damage_chart,
                1,
                AIR_CONTAINMENT_GRACE_TURNS,
            );
            // 期限内射撃を最優先しつつ、期限後2ターン以内に封じ込められる対処は
            // 敵航空資産価値に見合う1回限りの戦略対応として許可する。
            let economics = air_response_economics(&containment_assessment, &containment_coverage);
            if containment_coverage.total > AirDefenseAssessment::COVERAGE_EPSILON
                && is_economically_justified_air_response(stats.cost, economics)
            {
                candidates.push(EmergencyAntiAirCandidate {
                    facility_position: *facility_position,
                    unit_type: *unit_type,
                    cost: stats.cost,
                    coverage: containment_coverage.total,
                    protected_asset_value: economics.protected_asset_value,
                    meets_deadline: strict_coverage.total > AirDefenseAssessment::COVERAGE_EPSILON,
                });
            }
        }
    }

    candidates.sort_by(|left, right| {
        let left_affordable = left.cost <= current_funds;
        let right_affordable = right.cost <= current_funds;
        right_affordable
            .cmp(&left_affordable)
            .then_with(|| {
                if !left_affordable && !right_affordable {
                    // 購入不能候補は、貯金期間を短くするため最安を優先する。
                    left.cost
                        .cmp(&right.cost)
                        .then_with(|| right.meets_deadline.cmp(&left.meets_deadline))
                        .then_with(|| {
                            right
                                .protected_asset_value
                                .total_cmp(&left.protected_asset_value)
                        })
                } else {
                    right
                        .meets_deadline
                        .cmp(&left.meets_deadline)
                        .then_with(|| {
                            right
                                .protected_asset_value
                                .total_cmp(&left.protected_asset_value)
                        })
                        .then_with(|| right.coverage.total_cmp(&left.coverage))
                        .then_with(|| left.cost.cmp(&right.cost))
                }
            })
            .then_with(|| left.facility_position.y.cmp(&right.facility_position.y))
            .then_with(|| left.facility_position.x.cmp(&right.facility_position.x))
            .then_with(|| left.unit_type.as_str().cmp(right.unit_type.as_str()))
    });
    candidates.into_iter().next()
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn select_emergency_anti_air_candidate(
    facilities: &[(GridPosition, Terrain)],
    available_types: &[(UnitType, UnitStats)],
    player_id: PlayerId,
    strategy: &ProductionStrategy,
    master_data: &MasterDataRegistry,
    map: &crate::resources::Map,
    unit_positions: &std::collections::HashMap<
        (usize, usize),
        crate::systems::movement::OccupantInfo,
    >,
    damage_chart: &DamageChart,
    current_funds: u32,
) -> Option<EmergencyAntiAirCandidate> {
    select_emergency_anti_air_candidate_with_existing(
        facilities,
        available_types,
        &[],
        player_id,
        strategy,
        master_data,
        map,
        unit_positions,
        damage_chart,
        current_funds,
    )
}

fn compare_production_candidates(
    left: &ProductionCandidate,
    right: &ProductionCandidate,
) -> std::cmp::Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.facility_position.y.cmp(&right.facility_position.y))
        .then_with(|| left.facility_position.x.cmp(&right.facility_position.x))
        .then_with(|| {
            campaign_unit_type_rank(left.unit_type).cmp(&campaign_unit_type_rank(right.unit_type))
        })
        .then_with(|| left.cost.cmp(&right.cost))
}

fn select_best_production_candidate(
    candidates: &[ProductionCandidate],
) -> Option<&ProductionCandidate> {
    candidates
        .iter()
        .filter(|candidate| candidate.score > 0)
        .min_by(|left, right| compare_production_candidates(left, right))
}

fn remaining_campaign_requirements(
    shortfall: &IslandCampaignShortfall,
) -> Vec<CampaignProductionRequirement> {
    let mut requirements = Vec::new();
    if shortfall.heavy_transport_slots > 0 {
        requirements.push(CampaignProductionRequirement::HeavyTransport);
    }
    if shortfall.light_transport_slots > 0 {
        requirements.push(CampaignProductionRequirement::LightTransport);
    }
    if shortfall.capture_units > 0 {
        requirements.push(CampaignProductionRequirement::Capture);
    }
    if shortfall.combat_budget > 0 {
        requirements.push(CampaignProductionRequirement::Combat);
    }
    requirements
}

fn campaign_candidate_matches(
    requirement: CampaignProductionRequirement,
    unit_type: UnitType,
    stats: &UnitStats,
) -> bool {
    // 要求量を減らせない不正なマスターデータは候補から除外し、計画を確実に前進させる。
    match requirement {
        CampaignProductionRequirement::HeavyTransport => {
            unit_type == UnitType::Lander && stats.max_cargo > 0
        }
        CampaignProductionRequirement::LightTransport => {
            unit_type == UnitType::TransportHelicopter && stats.max_cargo > 0
        }
        CampaignProductionRequirement::Capture => stats.can_capture,
        CampaignProductionRequirement::Combat => {
            stats.cost > 0
                && !matches!(
                    unit_type,
                    UnitType::TransportHelicopter | UnitType::Lander | UnitType::SupplyTruck
                )
        }
    }
}

/// 島嶼輸送と敵拡張阻止を同じ手番に並行するため、対地航空用に空港を1枠残す。
pub(crate) fn select_expansion_denial_airport(
    facilities: &[(GridPosition, Terrain)],
    owned_airport_count: u32,
    available_types: &[(UnitType, UnitStats)],
    enemy_units: &[UnitStats],
    damage_chart: &DamageChart,
    master_data: &MasterDataRegistry,
    generic_funds: u32,
) -> Option<GridPosition> {
    let mut airports: Vec<_> = facilities
        .iter()
        .filter_map(|(position, terrain)| (*terrain == Terrain::Airport).then_some(*position))
        .collect();
    // 所有空港が複数なら、他方が一時的に占有されていて空きが1枠しかなくても
    // その1枠を戦闘用に残す。唯一の所有空港しかないマップでは輸送を止めない。
    if owned_airport_count < 2 || airports.is_empty() {
        return None;
    }
    let territory_control_targets: Vec<_> = enemy_units
        .iter()
        .filter(|enemy| {
            !matches!(enemy.movement_type, MovementType::Air | MovementType::Ship)
                || enemy.can_capture
                || enemy.max_cargo > 0
        })
        .collect();
    if territory_control_targets.is_empty() {
        return None;
    }
    let can_fund_effective_air = available_types.iter().any(|(unit_type, stats)| {
        stats.movement_type == MovementType::Air
            && !stats.can_capture
            && stats.max_cargo == 0
            && stats.cost > 0
            && stats.cost <= generic_funds
            && master_data.can_produce_unit(Terrain::Airport.as_str(), *unit_type)
            && territory_control_targets.iter().any(|enemy| {
                damage_chart
                    .get_base_damage(*unit_type, enemy.unit_type)
                    .unwrap_or(0)
                    .max(
                        damage_chart
                            .get_base_damage_secondary(*unit_type, enemy.unit_type)
                            .unwrap_or(0),
                    )
                    > 0
            })
    });
    if !can_fund_effective_air {
        return None;
    }

    airports.sort_by_key(|position| (position.y, position.x));
    airports.pop()
}

/// 島嶼作戦を仮計画し、当該手番の購入後に対地航空を買える場合は空港を残して再計画する。
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_campaign_with_expansion_denial_reserve(
    player_id: PlayerId,
    shortfalls: &[IslandCampaignShortfall],
    facilities: &[(GridPosition, Terrain)],
    owned_airport_count: u32,
    available_types: &[(UnitType, UnitStats)],
    enemy_units: &[UnitStats],
    damage_chart: &DamageChart,
    map: &crate::resources::Map,
    master_data: &MasterDataRegistry,
    available_funds: u32,
) -> CampaignProductionOutcome {
    let mut outcome = plan_campaign_shortfall_production_with_damage(
        player_id,
        shortfalls,
        facilities,
        available_types,
        map,
        master_data,
        available_funds,
        Some(damage_chart),
    );
    let Some(reserved_airport) = select_expansion_denial_airport(
        facilities,
        owned_airport_count,
        available_types,
        enemy_units,
        damage_chart,
        master_data,
        outcome.remaining_funds,
    ) else {
        return outcome;
    };

    let campaign_facilities: Vec<_> = facilities
        .iter()
        .filter(|(position, _)| *position != reserved_airport)
        .copied()
        .collect();
    outcome = plan_campaign_shortfall_production_with_damage(
        player_id,
        shortfalls,
        &campaign_facilities,
        available_types,
        map,
        master_data,
        available_funds,
        Some(damage_chart),
    );
    // 将来ターン向けのcampaign予約が現在の空き空港と現金まで完全に遮断すると、
    // 敵が増産中でも輸送だけを買い続けて掃討戦力が立ち上がらない。現在手番の
    // 構造購入を壊さず残った実資金は、予約額を超えていなくてもこの1枠へ開放する。
    outcome.generic_funds = outcome.remaining_funds;
    outcome
}

fn consume_transport_demand_after_production(
    strategy: &mut ProductionStrategy,
    unit_type: UnitType,
    max_cargo: u32,
    is_v3: bool,
) {
    if is_v3 {
        let (light_slots, heavy_slots) = sea_transport_capacity_from_slots(unit_type, max_cargo);
        match unit_type {
            UnitType::TransportHelicopter => {
                strategy.light_transport_demand =
                    strategy.light_transport_demand.saturating_sub(light_slots);
            }
            UnitType::Lander if strategy.heavy_transport_demand > 0 => {
                strategy.heavy_transport_demand =
                    strategy.heavy_transport_demand.saturating_sub(heavy_slots);
            }
            UnitType::Lander => {
                strategy.light_transport_demand =
                    strategy.light_transport_demand.saturating_sub(light_slots);
            }
            _ => {}
        }
        return;
    }

    // V1は従来どおり、Lander以外のcargo枠を軽輸送需要へ計上する。
    if max_cargo == 0 {
        return;
    }
    if unit_type == UnitType::Lander && strategy.heavy_transport_demand > 0 {
        strategy.heavy_transport_demand = strategy.heavy_transport_demand.saturating_sub(max_cargo);
    } else {
        strategy.light_transport_demand = strategy.light_transport_demand.saturating_sub(max_cargo);
    }
}

fn consume_campaign_candidate(
    shortfall: &mut IslandCampaignShortfall,
    requirement: CampaignProductionRequirement,
    stats: &UnitStats,
) {
    match requirement {
        CampaignProductionRequirement::HeavyTransport => {
            shortfall.heavy_transport_slots = shortfall
                .heavy_transport_slots
                .saturating_sub(stats.max_cargo);
        }
        CampaignProductionRequirement::LightTransport => {
            shortfall.light_transport_slots = shortfall
                .light_transport_slots
                .saturating_sub(stats.max_cargo);
        }
        CampaignProductionRequirement::Capture => {
            shortfall.capture_units = shortfall.capture_units.saturating_sub(1);
        }
        CampaignProductionRequirement::Combat => {
            shortfall.combat_budget = shortfall.combat_budget.saturating_sub(stats.cost);
        }
    }
    shortfall.reserved_budget = shortfall.reserved_budget.saturating_sub(stats.cost);
}

#[cfg(test)]
pub(crate) fn plan_campaign_shortfall_production(
    player_id: PlayerId,
    shortfalls: &[IslandCampaignShortfall],
    facilities: &[(GridPosition, Terrain)],
    available_types: &[(UnitType, UnitStats)],
    map: &crate::resources::Map,
    master_data: &MasterDataRegistry,
    available_funds: u32,
) -> CampaignProductionOutcome {
    plan_campaign_shortfall_production_with_damage(
        player_id,
        shortfalls,
        facilities,
        available_types,
        map,
        master_data,
        available_funds,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn plan_campaign_shortfall_production_with_damage(
    player_id: PlayerId,
    shortfalls: &[IslandCampaignShortfall],
    facilities: &[(GridPosition, Terrain)],
    available_types: &[(UnitType, UnitStats)],
    map: &crate::resources::Map,
    master_data: &MasterDataRegistry,
    available_funds: u32,
    damage_chart: Option<&DamageChart>,
) -> CampaignProductionOutcome {
    let mut rows = shortfalls.to_vec();
    rows.sort_by_key(|row| (row.priority_rank, row.island_id.0));
    let mut sorted_facilities = facilities.to_vec();
    sorted_facilities.sort_by_key(|(position, _)| (position.y, position.x));
    let mut sorted_types = available_types.to_vec();
    sorted_types
        .sort_by_key(|(unit_type, stats)| (campaign_unit_type_rank(*unit_type), stats.cost));

    let mut outcome = CampaignProductionOutcome {
        commands: Vec::new(),
        remaining_funds: available_funds,
        generic_funds: 0,
        used_facilities: std::collections::HashSet::new(),
        completed_all_rows: true,
    };
    // 上位作戦が施設不足で同一手番に完成しなくても、その残予算だけは保護した上で、
    // 競合しない施設（例: SecureのFactoryに対するExpandのAirport）を下位作戦へ使う。
    let mut protected_higher_priority_funds = 0_u32;
    let mut combat_connectivity = TerrainConnectivity::default();

    'rows: for row in &mut rows {
        let mut produced_structural_assault_unit = false;
        loop {
            let requirements = remaining_campaign_requirements(row);
            if requirements.is_empty() {
                break;
            }
            if row.decision == IslandCampaignDecision::Assault
                && produced_structural_assault_unit
                && requirements == [CampaignProductionRequirement::Combat]
            {
                // 金額だけのCombat端数は輸送量を保証できないため、構造便へ後付けしない。
                outcome.completed_all_rows = false;
                break 'rows;
            }
            let mut selected = None;
            for (requirement_index, requirement) in requirements.into_iter().enumerate() {
                let mut candidates = Vec::new();
                for (facility_position, terrain) in &sorted_facilities {
                    if outcome.used_facilities.contains(facility_position) {
                        continue;
                    }
                    for (unit_type, stats) in &sorted_types {
                        if !master_data.can_produce_unit(terrain.as_str(), *unit_type)
                            || !campaign_candidate_matches(requirement, *unit_type, stats)
                            // 島嶼キャンペーンのCombat枠は、この生産関数が実在する
                            // 輸送Entity・空き枠・任務接続を保証できない。そこで地上兵を
                            // 「後で何かが運ぶ」と見込まず、施設から作戦地点へ自力到達
                            // できる候補だけを許す。渡洋支援は航空・艦船、または既に
                            // campaignへ割り当て済みの輸送可能戦力が担当する。
                            || (requirement == CampaignProductionRequirement::Combat
                                && !combat_connectivity.is_reachable(
                                    map,
                                    master_data,
                                    (facility_position.x, facility_position.y),
                                    (row.target_position.x, row.target_position.y),
                                    stats.movement_type,
                                ))
                            || stats.cost
                                > outcome
                                    .remaining_funds
                                    .saturating_sub(protected_higher_priority_funds)
                            || stats.cost > row.reserved_budget
                        {
                            continue;
                        }
                        let combat_coverage =
                            if requirement == CampaignProductionRequirement::Combat {
                                stats.cost.min(row.combat_budget)
                            } else {
                                0
                            };
                        let (covered_enemy_types, total_expected_damage) =
                            damage_chart.map_or((0_u32, 0_u32), |chart| {
                                row.priority_enemy_types.iter().fold(
                                    (0_u32, 0_u32),
                                    |(covered, total), enemy_type| {
                                        let damage = chart
                                            .get_base_damage(*unit_type, *enemy_type)
                                            .unwrap_or_default()
                                            .max(
                                                chart
                                                    .get_base_damage_secondary(
                                                        *unit_type,
                                                        *enemy_type,
                                                    )
                                                    .unwrap_or_default(),
                                            );
                                        (
                                            covered + u32::from(damage > 0),
                                            total.saturating_add(damage),
                                        )
                                    },
                                )
                            });
                        candidates.push((
                            std::cmp::Reverse(covered_enemy_types),
                            std::cmp::Reverse(total_expected_damage),
                            std::cmp::Reverse(combat_coverage),
                            stats.cost,
                            campaign_unit_type_rank(*unit_type),
                            facility_position.y,
                            facility_position.x,
                            *facility_position,
                            *unit_type,
                            stats,
                        ));
                    }
                }
                candidates.sort_by_key(|candidate| {
                    (
                        candidate.0,
                        candidate.1,
                        candidate.2,
                        candidate.3,
                        candidate.4,
                        candidate.5,
                        candidate.6,
                    )
                });
                if let Some((_, _, _, _, _, _, _, position, unit_type, stats)) =
                    candidates.into_iter().next()
                {
                    selected = Some((
                        requirement_index > 0,
                        requirement,
                        position,
                        unit_type,
                        stats,
                    ));
                    break;
                }
            }
            let Some((used_lower_requirement, requirement, position, unit_type, stats)) = selected
            else {
                // 未完成分の資金は下位rowから隔離する。ただし施設種別が競合しない場合まで
                // 生産全体を直列化するとAirportが遊ぶため、次のcampaign rowは評価する。
                let protectable = outcome
                    .remaining_funds
                    .saturating_sub(protected_higher_priority_funds);
                protected_higher_priority_funds = protected_higher_priority_funds
                    .saturating_add(row.reserved_budget.min(protectable));
                outcome.completed_all_rows = false;
                break;
            };

            outcome.commands.push(ProduceUnitCommand {
                player_id,
                target_x: position.x,
                target_y: position.y,
                unit_type,
            });
            outcome.remaining_funds = outcome.remaining_funds.saturating_sub(stats.cost);
            outcome.used_facilities.insert(position);
            consume_campaign_candidate(row, requirement, stats);
            if row.decision == IslandCampaignDecision::Assault
                && requirement != CampaignProductionRequirement::Combat
            {
                produced_structural_assault_unit = true;
            }
            if used_lower_requirement {
                // 最優先要件の購入資金を次ターンへ残すため、同じパッケージ内の
                // 安価な前提要素を1件だけ先行購入してこの手番の計画を止める。
                outcome.completed_all_rows = false;
                break 'rows;
            }
        }
    }

    // 未完成行だけでなく未処理の下位行も含め、残った予約総額を先に隔離する。
    // 現金が予約を上回る部分だけをV4の迎撃・戦闘へ開放し、島嶼作戦を資金面で
    // 壊さずに遊休生産枠と余剰資金を活用する。
    let remaining_reservations = rows.iter().fold(0_u32, |total, row| {
        total.saturating_add(row.reserved_budget)
    });
    outcome.generic_funds = outcome
        .remaining_funds
        .saturating_sub(remaining_reservations);
    outcome
}

/// 生産AI。
/// 以下のロジックで生産計画を立てます。
/// - 歩兵・重歩兵は占領等のため10体を目安に高く評価
/// - その他のユニットは戦略（フェーズ）、アンチ性能、到達ターン数（ETA）に基づき多角的に評価
/// - 予算（貯金を差し引いた仮想予算）内で最も評価が高くなるよう動的計画法（ナップサック問題）で生産を決定
pub fn decide_production(world: &mut World, player_id: PlayerId) -> Vec<ProduceUnitCommand> {
    // V4 は作戦駆動生産へ委譲する。V1/V2/V3 の経路は以下そのまま。
    if crate::ai::resolve_player_ai_version(world, player_id).uses_operation_driven_production() {
        return crate::ai::v4::decide_production_v4(world, player_id);
    }

    let mut commands = Vec::new();

    let strategy = analyze_strategy_for_turn(world, player_id);
    let map = world.resource::<crate::resources::Map>().clone();

    // V3 の生産拡張 (対編成カウンター効率) を有効にするかどうか
    let is_v3 = crate::ai::resolve_player_ai_version(world, player_id).uses_v3_tactics();

    let (unit_registry, damage_chart, master_data) = {
        let ur = world.get_resource::<UnitRegistry>().cloned();
        let dc = world.get_resource::<DamageChart>().cloned();
        let md = world.get_resource::<MasterDataRegistry>().cloned();
        if ur.is_none() || dc.is_none() || md.is_none() {
            return commands;
        }
        (ur.unwrap(), dc.unwrap(), md.unwrap())
    };

    let current_funds = if let Some(players) = world.get_resource::<Players>() {
        players
            .0
            .iter()
            .find(|p| p.id == player_id)
            .map(|p| p.funds)
            .unwrap_or(0)
    } else {
        return commands;
    };

    // --- 0. 施設・ユニット・首都のスキャン ---
    let mut occupied_positions = std::collections::HashSet::new();
    let mut unit_positions = std::collections::HashMap::new();
    let mut enemy_units = Vec::new();
    let mut my_units = Vec::new();
    let mut my_capability_units = Vec::new();
    let mut my_empty_transports = Vec::new();

    {
        let mut q_units = world.query::<(
            Entity,
            &GridPosition,
            &Faction,
            &UnitStats,
            Option<&Health>,
            Option<&Ammo>,
            Option<&Fuel>,
            Option<&ActionCompleted>,
            Option<&crate::components::CargoCapacity>,
            Option<&crate::components::Transporting>,
        )>();
        for (
            _entity,
            pos,
            faction,
            stats,
            health,
            ammo,
            fuel,
            action_completed,
            cargo_opt,
            transporting_opt,
        ) in q_units.iter(world)
        {
            if transporting_opt.is_some() {
                continue;
            }
            occupied_positions.insert(*pos);
            unit_positions.insert(
                (pos.x, pos.y),
                crate::systems::movement::OccupantInfo {
                    player_id: faction.0,
                    is_transport: stats.max_cargo > 0,
                    unit_type: stats.unit_type,
                    loadable_types: stats.loadable_unit_types.clone(),
                    free_slots: cargo_opt.map_or(stats.max_cargo, |cargo| {
                        stats.max_cargo.saturating_sub(cargo.loaded.len() as u32)
                    }),
                },
            );
            if faction.0 == player_id {
                my_units.push((*pos, stats.clone()));
                my_capability_units.push(CombatCapabilitySnapshot {
                    faction: faction.0,
                    position: *pos,
                    unit_type: stats.unit_type,
                    movement_type: stats.movement_type,
                    hp: health.map_or(100, |health| health.current),
                    cost: stats.cost,
                    max_movement: stats.max_movement,
                    min_range: stats.min_range,
                    max_range: stats.max_range,
                    ammo1: ammo.map_or(stats.max_ammo1, |ammo| ammo.ammo1),
                    max_ammo1: stats.max_ammo1,
                    ammo2: ammo.map_or(stats.max_ammo2, |ammo| ammo.ammo2),
                    max_ammo2: stats.max_ammo2,
                    fuel: fuel.map_or(stats.max_fuel, |fuel| fuel.current),
                    action_delay: u32::from(action_completed.is_none_or(|completed| completed.0)),
                });
                if let Some(cargo) = cargo_opt
                    && cargo.loaded.is_empty()
                    && stats.max_cargo > 0
                {
                    my_empty_transports.push((*pos, stats.clone()));
                }
            } else {
                enemy_units.push((*pos, stats.clone()));
            }
        }
    }

    let mut capital_pos = None;
    let mut my_facilities = Vec::new();
    let mut producible_types = std::collections::HashSet::new();
    let mut income_per_turn = 0u32;
    let mut owned_airport_count = 0u32;

    // 生産範囲判定に使うマップのトポロジー（スクエア/ヘックス）
    let topology = world
        .get_resource::<crate::resources::Map>()
        .map(|m| m.topology)
        .unwrap_or(crate::resources::GridTopology::Square);

    {
        let mut q_props = world.query::<(&GridPosition, &Property)>();
        // まず首都を探す
        for (pos, prop) in q_props.iter(world) {
            if prop.owner_id == Some(player_id) && prop.terrain == Terrain::Capital {
                capital_pos = Some(*pos);
                break;
            }
        }

        // 生産施設を収集し、生産可能なユニットタイプと次ターン収入を特定
        for (pos, prop) in q_props.iter(world) {
            if prop.owner_id == Some(player_id) {
                income_per_turn = income_per_turn
                    .saturating_add(master_data.landscape_income(prop.terrain.as_str()));
                if prop.terrain == Terrain::Airport
                    && crate::systems::production::is_within_production_range(
                        capital_pos.as_slice(),
                        pos.x,
                        pos.y,
                        topology,
                    )
                {
                    owned_airport_count = owned_airport_count.saturating_add(1);
                }
            }
            if prop.owner_id == Some(player_id)
                && master_data.is_production_facility(prop.terrain.as_str())
                && !occupied_positions.contains(pos)
            {
                // 首都から3マス以内（PRODUCTION_RANGE）の施設のみを有効とする
                let capital_positions = capital_pos.as_slice();
                if crate::systems::production::is_within_production_range(
                    capital_positions,
                    pos.x,
                    pos.y,
                    topology,
                ) {
                    my_facilities.push((*pos, prop.terrain));
                    // この施設で生産可能なユニットタイプを記録
                    for ut in unit_registry.0.keys() {
                        if master_data.can_produce_unit(prop.terrain.as_str(), *ut) {
                            producible_types.insert(*ut);
                        }
                    }
                }
            }
        }
    }

    if my_facilities.is_empty() {
        return commands;
    }

    let available_types: Vec<(UnitType, UnitStats)> = unit_registry
        .0
        .iter()
        .map(|(unit_type, stats)| (*unit_type, stats.clone()))
        .collect();

    // --- 1. 資金計画の更新 ---
    let mut reserves = 0;

    // ProductionPlanリソースの取得または作成
    if world.get_resource::<ProductionPlan>().is_none() {
        world.insert_resource(ProductionPlan::default());
    }

    let emergency_candidate = if is_v3 && strategy.air_defense.requires_emergency_production() {
        select_emergency_anti_air_candidate_with_existing(
            &my_facilities,
            &available_types,
            &my_capability_units,
            player_id,
            &strategy,
            &master_data,
            &map,
            &unit_positions,
            &damage_chart,
            current_funds,
        )
    } else {
        None
    };
    let mut plan = world.get_resource_mut::<ProductionPlan>().unwrap();
    if let Some(candidate) = emergency_candidate {
        if candidate.cost <= current_funds {
            plan.emergency_anti_air_reservations.remove(&player_id.0);
            return vec![ProduceUnitCommand {
                player_id,
                target_x: candidate.facility_position.x,
                target_y: candidate.facility_position.y,
                unit_type: candidate.unit_type,
            }];
        }
        let turns_to_afford = if income_per_turn == 0 {
            None
        } else {
            let deficit = candidate.cost.saturating_sub(current_funds);
            Some(deficit.div_ceil(income_per_turn))
        };
        let remains_effective = turns_to_afford.is_some_and(|wait_turns| {
            let Some(stats) = unit_registry.0.get(&candidate.unit_type) else {
                return false;
            };
            let future_coverage = candidate_air_coverage_with_delay(
                stats,
                candidate.facility_position,
                player_id,
                &strategy.air_defense.emergency_targets_only(),
                &map,
                &master_data,
                &unit_positions,
                &damage_chart,
                1u32.saturating_add(wait_turns),
            );
            future_coverage.total > AirDefenseAssessment::COVERAGE_EPSILON
                && is_economically_justified_air_response(
                    candidate.cost,
                    air_response_economics(&strategy.air_defense, &future_coverage),
                )
        });
        if remains_effective {
            // 購入できる時点でも期限内に有効な場合だけ、通常計画と独立して貯金する。
            plan.emergency_anti_air_reservations.insert(
                player_id.0,
                EmergencyAntiAirReservation {
                    unit_type: candidate.unit_type,
                    cost: candidate.cost,
                },
            );
            return commands;
        }
    }
    // 航空脅威または購入時点で有効な候補が消えたら、緊急対空予約だけを解除する。
    plan.emergency_anti_air_reservations.remove(&player_id.0);

    if strategy.phase == GamePhase::Defense || (is_v3 && !strategy.campaign_shortfalls.is_empty()) {
        // campaign allocatorが完全package資金を予約済みのため、generic貯金で二重に差し引かない。
        plan.reserves.insert(player_id.0, 0);
    } else {
        reserves = *plan.reserves.get(&player_id.0).unwrap_or(&0);

        // 欲しいユニット（一番スコアが高いもの）が買えない場合、貯金を検討
        // ただし、現在持っている施設で生産可能なものに限定する
        let mut saving_candidates = Vec::new();

        for (ut, stats) in &unit_registry.0 {
            if !producible_types.contains(ut) {
                continue;
            }
            let Some((facility_position, facility_terrain)) = my_facilities
                .iter()
                .filter(|(_, terrain)| master_data.can_produce_unit(terrain.as_str(), *ut))
                .min_by_key(|(position, _)| (position.y, position.x))
            else {
                continue;
            };

            let current_ratio = if !my_units.is_empty() {
                my_units.iter().filter(|(_, s)| s.unit_type == *ut).count() as f32
                    / my_units.len() as f32
            } else {
                0.0
            };
            let ratio_diff =
                strategy.ideal_composition.get(ut).copied().unwrap_or(0.0) - current_ratio;

            let score = calculate_unit_score_at(
                *ut,
                stats,
                *facility_position,
                player_id,
                &strategy,
                &enemy_units,
                &my_empty_transports,
                &damage_chart,
                &master_data,
                &map,
                &unit_positions,
                &unit_registry,
                *facility_terrain,
                ratio_diff,
                is_v3,
            );
            saving_candidates.push(ProductionCandidate {
                score,
                facility_position: *facility_position,
                unit_type: *ut,
                cost: stats.cost,
                max_cargo: stats.max_cargo,
                can_capture: stats.can_capture,
            });
        }

        let best_unit = select_best_production_candidate(&saving_candidates).copied();
        if let Some(candidate) = best_unit
            && candidate.cost > current_funds
            && candidate.cost > reserves
        {
            plan.reserves.insert(player_id.0, candidate.cost);
            plan.reservations
                .entry(player_id.0)
                .or_default()
                .push(candidate.unit_type);
            reserves = candidate.cost;
        } else if let Some(candidate) = best_unit
            && candidate.cost <= current_funds
        {
            // 買えるユニットがベストなら、貯金目標をリセット（または達成済みとする）
            if reserves > 0 && current_funds >= reserves {
                plan.reserves.insert(player_id.0, 0);
                reserves = 0;
            }
        }
    }

    // --- 2. 実行予算の算出 ---
    let available_funds = if strategy.phase == GamePhase::Defense {
        current_funds
    } else {
        // 貯金目標がある場合、貯金目標の達成を確実にするため、バッファを含めて予算を制限する
        let reserve_cut = if reserves > 0 { reserves / 2 + 1000 } else { 0 };
        let mut budget = current_funds.saturating_sub(reserve_cut);

        // ユニット数が極端に少ない(5体未満)場合は、即座の占領・戦力拡張を最優先するため全額を実行予算とする。
        // そうではなく、予算が歩兵コスト(1000G)を下回っているだけであれば、貯金を妥協しつつも歩兵1体分(1000G)程度に予算を抑える。
        if my_units.len() < 5 {
            budget = current_funds;
        } else if budget < 1000 {
            budget = 1000.min(current_funds);
        }
        budget
    };

    // --- 3. V3 campaign予約行をpriority rank・島ID順に先行消費 ---
    let mut campaign_outcome = CampaignProductionOutcome {
        commands: Vec::new(),
        remaining_funds: available_funds,
        generic_funds: 0,
        used_facilities: std::collections::HashSet::new(),
        completed_all_rows: true,
    };
    if is_v3 && !strategy.campaign_shortfalls.is_empty() {
        let campaign_plan_exists = world
            .get_resource::<crate::ai::engine::AiTurnStrategyCache>()
            .is_some_and(|cache| cache.campaign_production_planned(player_id));
        if campaign_plan_exists {
            // 最初の完全package計画を1commandずつ消費し、同じshortfallの重複生産を防ぐ。
            let mut cache = world
                .remove_resource::<crate::ai::engine::AiTurnStrategyCache>()
                .unwrap_or_default();
            let next_command = cache.take_campaign_production_command(player_id);
            let blocks_generic = cache.campaign_production_blocks_generic(player_id);
            let generic_budget = cache.campaign_production_generic_budget(player_id);
            world.insert_resource(cache);
            if let Some(command) = next_command {
                return vec![command];
            }
            if blocks_generic {
                return commands;
            }
            // campaign予約を侵食しない超過額だけで、同じ手番の空き施設を
            // 汎用戦闘生産へ開放する。V3も構造枠の二重購入は下で無効化する。
            campaign_outcome.remaining_funds = generic_budget.unwrap_or(0);
            campaign_outcome.generic_funds = generic_budget.unwrap_or(0);
        } else {
            let enemy_stats: Vec<_> = enemy_units.iter().map(|(_, stats)| stats.clone()).collect();
            campaign_outcome = plan_campaign_with_expansion_denial_reserve(
                player_id,
                &strategy.campaign_shortfalls,
                &my_facilities,
                owned_airport_count,
                &available_types,
                &enemy_stats,
                &damage_chart,
                &map,
                &master_data,
                available_funds,
            );
            let generic_budget = campaign_outcome.generic_funds;
            let campaign_commands = std::mem::take(&mut campaign_outcome.commands);
            let mut cache = world
                .remove_resource::<crate::ai::engine::AiTurnStrategyCache>()
                .unwrap_or_default();
            cache.set_campaign_production_plan_with_generic_budget(
                player_id,
                campaign_commands,
                generic_budget,
            );
            let next_command = cache.take_campaign_production_command(player_id);
            let blocks_generic = cache.campaign_production_blocks_generic(player_id);
            world.insert_resource(cache);
            if let Some(command) = next_command {
                return vec![command];
            }
            if blocks_generic {
                return commands;
            }
            campaign_outcome.remaining_funds = generic_budget;
        }
    }

    // --- 4. campaign完了後だけgeneric需要を予算と施設重複込みで評価 ---
    let mut remaining_funds = campaign_outcome.remaining_funds;
    let mut current_strategy = strategy.clone();
    if is_v3 && !current_strategy.campaign_shortfalls.is_empty() {
        current_strategy.capture_demand = 0;
        current_strategy.light_transport_demand = 0;
        current_strategy.heavy_transport_demand = 0;
    }
    let mut used_facilities = campaign_outcome.used_facilities;

    loop {
        let mut production_candidates = Vec::new();

        for (facility_pos, terrain) in &my_facilities {
            if used_facilities.contains(facility_pos) {
                continue;
            }

            let terrain_name = terrain.as_str();
            for (ut, stats) in &available_types {
                if !master_data.can_produce_unit(terrain_name, *ut) {
                    continue;
                }
                if stats.cost > remaining_funds {
                    continue;
                }

                // 予算制限（remaining_funds）がすでに reserve_cut を差し引いているため、
                // この範囲内で買えるものであれば、戦闘ユニットであっても生産してよい。

                let current_ratio = if !my_units.is_empty() {
                    my_units.iter().filter(|(_, s)| s.unit_type == *ut).count() as f32
                        / my_units.len() as f32
                } else {
                    0.0
                };
                let ratio_diff = current_strategy
                    .ideal_composition
                    .get(ut)
                    .copied()
                    .unwrap_or(0.0)
                    - current_ratio;

                // 現在の戦略（減衰後）でスコアを計算
                let score = calculate_unit_score_at(
                    *ut,
                    stats,
                    *facility_pos,
                    player_id,
                    &current_strategy,
                    &enemy_units,
                    &my_empty_transports,
                    &damage_chart,
                    &master_data,
                    &map,
                    &unit_positions,
                    &unit_registry,
                    *terrain,
                    ratio_diff,
                    is_v3,
                );

                production_candidates.push(ProductionCandidate {
                    score,
                    facility_position: *facility_pos,
                    unit_type: *ut,
                    cost: stats.cost,
                    max_cargo: stats.max_cargo,
                    can_capture: stats.can_capture,
                });
            }
        }

        if let Some(candidate) = select_best_production_candidate(&production_candidates).copied() {
            // 生産決定
            commands.push(ProduceUnitCommand {
                player_id,
                target_x: candidate.facility_position.x,
                target_y: candidate.facility_position.y,
                unit_type: candidate.unit_type,
            });
            remaining_funds = remaining_funds.saturating_sub(candidate.cost);
            used_facilities.insert(candidate.facility_position);

            // 需要を動的に減衰させる（次の候補評価に反映）。
            // V3では海を越えられる輸送種別だけがoffshore需要を消費する。
            consume_transport_demand_after_production(
                &mut current_strategy,
                candidate.unit_type,
                candidate.max_cargo,
                is_v3,
            );
            if candidate.can_capture {
                current_strategy.capture_demand = current_strategy.capture_demand.saturating_sub(1);
            }
            if is_v3 && let Some(stats) = unit_registry.0.get(&candidate.unit_type) {
                let coverage = candidate_air_coverage(
                    stats,
                    candidate.facility_position,
                    player_id,
                    &current_strategy.air_defense,
                    &map,
                    &master_data,
                    &unit_positions,
                    &damage_chart,
                );
                current_strategy.air_defense.apply_coverage(&coverage);
                current_strategy.demand.anti_air = current_strategy.air_defense.shortage_ratio;
            }
        } else {
            // これ以上生産可能なものがないか、予算不足
            break;
        }
    }

    commands
}

/// #53/#55 (V3): 交戦成立率。攻撃側が防御側に対してどれだけ容易に射撃機会を
/// 得られるかを射程と機動力から近似する。
/// アウトレンジする側 (射程で上回る側) は撃ち逃げで一方的に攻撃でき、
/// アウトレンジされる側は接近中に削られて攻撃機会が減る。
fn engagement_factor(attacker: &UnitStats, defender: &UnitStats) -> f32 {
    let att_reach = attacker.max_movement + attacker.max_range;
    let def_reach = defender.max_movement + defender.max_range;
    if attacker.max_range > defender.max_range {
        // アウトレンジ可能: リーチでも上回るなら完全な撃ち逃げが成立する
        if att_reach >= def_reach { 1.0 } else { 0.8 }
    } else if attacker.max_range < defender.max_range {
        // アウトレンジされる側: 射程内に入るまでに一方的に削られる
        0.5
    } else {
        1.0
    }
}

/// #53/#55 (V3): 対編成カウンター効率スコア。
/// 候補ユニット U を1体生産した場合の、敵軍全体との「価値交換」の期待値を
/// ゴールド換算で見積もる。敵ユニット e ごとに
///   与える価値 = dmg(U→e) × cost_e × 交戦成立率(U,e)
///   受ける価値 = dmg(e→U) × cost_U × 交戦成立率(e,U)
/// の差を取り、敵軍の平均を返す。敵の主力構成に対して効率よく価値を刈り取れる
/// ユニット (例: ロケラン主体の敵にはそれをアウトレンジする自走砲) が高評価になる。
#[derive(Debug, Clone, Copy, Default)]
struct CounterEfficiencyComponents {
    non_air_net: i64,
    air_net: i64,
    non_air_count: u32,
    air_count: u32,
}

impl CounterEfficiencyComponents {
    fn score_with_air_shortage(self, air_shortage: f32) -> i32 {
        let air_weight = air_shortage.clamp(0.0, 1.0);
        let active_count = if air_weight <= f32::EPSILON {
            self.non_air_count
        } else {
            self.non_air_count + self.air_count
        };
        if active_count == 0 {
            return 0;
        }
        let weighted_air = self.air_net as f32 * air_weight;
        ((self.non_air_net as f32 + weighted_air) / active_count as f32) as i32
    }
}

fn counter_efficiency_components(
    unit_stats: &UnitStats,
    enemy_units: &[(GridPosition, UnitStats)],
    damage_chart: &DamageChart,
) -> CounterEfficiencyComponents {
    let mut components = CounterEfficiencyComponents::default();
    for (_, e_stats) in enemy_units {
        // 与える価値 (主武器・副武器の高い方)
        let dmg_out = damage_chart
            .get_base_damage(unit_stats.unit_type, e_stats.unit_type)
            .unwrap_or(0)
            .max(
                damage_chart
                    .get_base_damage_secondary(unit_stats.unit_type, e_stats.unit_type)
                    .unwrap_or(0),
            );
        // 受ける価値
        let dmg_in = damage_chart
            .get_base_damage(e_stats.unit_type, unit_stats.unit_type)
            .unwrap_or(0)
            .max(
                damage_chart
                    .get_base_damage_secondary(e_stats.unit_type, unit_stats.unit_type)
                    .unwrap_or(0),
            );
        let value_out =
            dmg_out as f32 * e_stats.cost as f32 / 100.0 * engagement_factor(unit_stats, e_stats);
        let value_in =
            dmg_in as f32 * unit_stats.cost as f32 / 100.0 * engagement_factor(e_stats, unit_stats);
        let net = (value_out - value_in) as i64;
        if e_stats.movement_type == MovementType::Air {
            components.air_net += net;
            components.air_count += 1;
        } else {
            components.non_air_net += net;
            components.non_air_count += 1;
        }
    }
    components
}

#[cfg(test)]
pub(crate) fn counter_efficiency_score(
    unit_stats: &UnitStats,
    enemy_units: &[(GridPosition, UnitStats)],
    damage_chart: &DamageChart,
) -> i32 {
    counter_efficiency_components(unit_stats, enemy_units, damage_chart)
        .score_with_air_shortage(1.0)
}

/// 指定した地点で特定のユニットを生産した場合の期待スコアを算出します。
#[allow(clippy::too_many_arguments)]
pub fn calculate_unit_score_at(
    unit_type: UnitType,
    stats: &UnitStats,
    pos: GridPosition,
    player_id: PlayerId,
    strategy: &ProductionStrategy,
    enemy_units: &[(GridPosition, UnitStats)],
    my_empty_transports: &[(GridPosition, UnitStats)],
    damage_chart: &DamageChart,
    master_data: &MasterDataRegistry,
    map: &crate::resources::Map,
    unit_positions: &std::collections::HashMap<
        (usize, usize),
        crate::systems::movement::OccupantInfo,
    >,
    _unit_registry: &UnitRegistry,
    produced_at: Terrain,
    ratio_diff: f32,
    // V3 のみ true。対編成カウンター効率スコアで生産を敵構成に適応させる
    is_v3: bool,
) -> u32 {
    // 1. 基本スコア（敵との距離、脅威度）
    let mut min_eta = 99;
    let mut score: u32 = if !strategy.priority_targets.is_empty() {
        let mut local_min_eta = 99;
        let mut base_val: i32 = 2000; // ベースを引き上げ

        for target in &strategy.priority_targets {
            // ターゲットが未占領（中立）拠点か判定
            let is_unowned_property = strategy.unowned_properties.contains(target);

            // 論理防衛評価: 占領できない戦闘ユニットは、中立拠点のETA評価を無視（スキップ）する
            if is_unowned_property && !stats.can_capture {
                continue;
            }

            let mut dist = (pos.x as isize - target.x as isize).unsigned_abs()
                + (pos.y as isize - target.y as isize).unsigned_abs();

            let mut reachable_target = false;
            // 海軍ユニットの対地評価補正
            if stats.movement_type == MovementType::Ship {
                if let Some(t_terrain) = map.get_terrain(target.x, target.y) {
                    let move_cost = master_data
                        .get_movement_cost(MovementType::Ship, t_terrain.as_str())
                        .unwrap_or(99);
                    if move_cost < 99 {
                        reachable_target = true;
                    }
                }

                // 隣接マスが海なら「沿岸」として到達可能とみなす
                if !reachable_target {
                    for adj in map.get_adjacent(target.x, target.y) {
                        if let Some(at) = map.get_terrain(adj.0, adj.1)
                            && master_data
                                .get_movement_cost(MovementType::Ship, at.as_str())
                                .unwrap_or(99)
                                < 99
                        {
                            reachable_target = true;
                            break;
                        }
                    }
                }

                if !reachable_target {
                    // 目標が直接到達不能な場合
                    if stats.max_range <= 1 {
                        // 直接攻撃ユニットは距離ペナルティ
                        dist += 20;
                        if stats.max_cargo == 0 {
                            // 輸送能力もないならベース値を大幅に下げる
                            base_val /= 4;
                        } else {
                            // 輸送能力がある場合は沿岸まで到達できれば良いのでペナルティを軽減
                            dist -= 15; // +20されたのを+5に緩和
                        }
                    } else {
                        // 間接攻撃ユニットは多少マシにする
                        dist += 10;
                    }
                }
            }

            // 地形コストを考慮したETAの簡易見積もり
            let base_terrain = if stats.movement_type == MovementType::Ship {
                Terrain::Sea.as_str()
            } else {
                Terrain::Plains.as_str()
            };
            let move_cost = master_data
                .get_movement_cost(stats.movement_type, base_terrain)
                .unwrap_or(1);
            let mut eta =
                (dist as u32 * move_cost + stats.max_movement - 1) / stats.max_movement.max(1);

            // 7.1 フォワードETA評価: 工場に空の輸送車がいる場合、輸送車を利用したETAを算出
            for (t_pos, t_stats) in my_empty_transports {
                if t_pos.x == pos.x && t_pos.y == pos.y {
                    // 輸送車がそのユニットを搭載可能かチェック
                    if t_stats.loadable_unit_types.contains(&stats.unit_type) {
                        let t_move_cost = master_data
                            .get_movement_cost(t_stats.movement_type, Terrain::Plains.as_str())
                            .unwrap_or(1);
                        let assisted_eta = (dist as u32 * t_move_cost + t_stats.max_movement - 1)
                            / t_stats.max_movement.max(1);

                        if assisted_eta < eta {
                            eta = assisted_eta;
                        }
                    }
                }
            }

            // 船の場合、ターゲットが沿岸ならETAをさらに好意的に評価（海路は速いため）
            let mut final_eta = eta;
            if stats.movement_type == MovementType::Ship && reachable_target {
                final_eta = final_eta.saturating_sub(2).max(1);
            }

            if final_eta < local_min_eta {
                local_min_eta = final_eta;
            }
        }
        min_eta = local_min_eta;

        // 1ターン遅れるごとに40点のペナルティ（緩和）
        let eta_penalty = min_eta * 40;
        base_val.saturating_sub(eta_penalty as i32).max(1) as u32
    } else {
        // 敵がいない場合は均一
        100
    };

    // 2. 特殊役割ボーナス
    if stats.can_capture {
        // 不足している占領可能ユニット数（capture_demand）に応じて線形に価値を高める
        if strategy.capture_demand > 0 {
            score += 2500 * strategy.capture_demand; // 不足数が多い（特に収入危機時）ほど超強力に歩兵を優先
        } else if strategy.phase == GamePhase::Expansion {
            score = score.saturating_sub(1000);
        } else {
            score = score.saturating_sub(2000);
        }

        // 近く（ETA=1〜2）に未占領拠点がある場合、収入確保の近接占領ボーナスを付与
        if strategy.capture_demand > 0 && min_eta <= 2 {
            score += 2000;
        }
    }
    // 輸送ユニットの評価（期待状態価値の向上分に基づく）
    let transport_targets = if is_v3 {
        strategy.campaign_portfolio.offensive_target_positions()
    } else {
        strategy.priority_targets.clone()
    };
    let transport_utility_eligible = if is_v3 {
        let capacity = sea_transport_capacity_from_slots(unit_type, stats.max_cargo);
        capacity.0 > 0 || capacity.1 > 0
    } else {
        stats.max_cargo > 0
    };
    // V3は島嶼攻勢の実目標と海上輸送能力がそろう場合だけutilityを評価する。
    if transport_utility_eligible
        && !strategy.transport_candidates.is_empty()
        && !transport_targets.is_empty()
    {
        let mut transport_utility: f32 = 0.0;
        for (c_pos, c_stats, c_value) in &strategy.transport_candidates {
            // この輸送ユニットが搭載可能かチェック
            if stats.loadable_unit_types.contains(&c_stats.unit_type) {
                // 候補ユニットにとっての最寄りの実在ターゲットだけをETA計算へ渡す。
                let Some(best_target) = transport_targets.iter().min_by_key(|target| {
                    (c_pos.x as i32 - target.x as i32).abs()
                        + (c_pos.y as i32 - target.y as i32).abs()
                }) else {
                    continue;
                };
                let min_dist_to_target = (c_pos.x as i32 - best_target.x as i32).abs()
                    + (c_pos.y as i32 - best_target.y as i32).abs();

                // 自力ETAの見積もり（海越えなら大きなペナルティ）
                let mut is_blocked = false;
                let steps = 4;
                for i in 1..steps {
                    let cx = c_pos.x as i32 + (best_target.x as i32 - c_pos.x as i32) * i / steps;
                    let cy = c_pos.y as i32 + (best_target.y as i32 - c_pos.y as i32) * i / steps;
                    if let Some(Terrain::Sea | Terrain::Shoal) =
                        map.get_terrain(cx as usize, cy as usize)
                    {
                        is_blocked = true;
                        break;
                    }
                }

                let self_eta = if is_blocked {
                    20.0
                } else {
                    (min_dist_to_target as f32) / (c_stats.max_movement as f32).max(1.0)
                };

                // 輸送時のETA（生産地点からターゲットまでの輸送ユニットの移動時間）
                let dist_to_target = (pos.x as i32 - best_target.x as i32).abs()
                    + (pos.y as i32 - best_target.y as i32).abs();
                let transport_eta = (dist_to_target as f32) / (stats.max_movement as f32).max(1.0);

                // 短縮効果 (ETA Gain)
                let eta_gain = (self_eta - transport_eta).max(0.0);

                // ユーティリティ = ユニット価値 * 短縮ターン数
                transport_utility += c_value * eta_gain;
            }
        }

        // スコアへの統合（既存スコア体系とバランスを取るために係数 0.15 を適用）
        // 保有輸送ユニット数に応じた減衰 (1台増えるごとに評価を段階的に下げる)
        let attenuation = 1.0 / (1.0 + strategy.existing_transport_count as f32);
        score += (transport_utility * 0.15 * attenuation) as u32;

        // 2.5. Lander侵攻価値スコア (Invasion Value)
        let mut invasion_value = 0.0;
        for target in &transport_targets {
            let mut is_blocked = false;
            let steps = 4;
            for i in 1..steps {
                let cx = pos.x as i32 + (target.x as i32 - pos.x as i32) * i / steps;
                let cy = pos.y as i32 + (target.y as i32 - pos.y as i32) * i / steps;
                if let Some(Terrain::Sea | Terrain::Shoal) =
                    map.get_terrain(cx as usize, cy as usize)
                {
                    is_blocked = true;
                    break;
                }
            }

            if is_blocked {
                let property_value = if let Some(t_terrain) = map.get_terrain(target.x, target.y) {
                    match t_terrain {
                        Terrain::Capital => 5000,
                        Terrain::Factory => 3000,
                        Terrain::Port | Terrain::Airport => 2000,
                        _ => 1000,
                    }
                } else {
                    1000
                };

                let cargo_value = strategy
                    .transport_candidates
                    .iter()
                    .filter(|(_, c_stats, _)| {
                        stats.loadable_unit_types.contains(&c_stats.unit_type)
                    })
                    .map(|(_, _, val)| *val)
                    .fold(f32::MIN, |a, b| a.max(b));

                if cargo_value > f32::MIN {
                    let dist_to_target = (pos.x as i32 - target.x as i32).abs()
                        + (pos.y as i32 - target.y as i32).abs();
                    let transport_eta =
                        (dist_to_target as f32) / (stats.max_movement as f32).max(1.0);
                    invasion_value +=
                        (property_value as f32) * cargo_value / transport_eta.max(1.0);
                }
            }
        }
        let attenuation_inv = 1.0 / (1.0 + strategy.existing_transport_count as f32);
        score += (invasion_value * attenuation_inv * 0.002) as u32;

        // 輸送需要がない場合は減衰（既存ロジックの維持）
        let can_load_heavy = stats.loadable_unit_types.contains(&UnitType::Tank);
        let can_load_light = stats.loadable_unit_types.contains(&UnitType::Infantry);

        let demand = if can_load_heavy && can_load_light {
            strategy
                .heavy_transport_demand
                .max(strategy.light_transport_demand)
        } else if can_load_heavy {
            strategy.heavy_transport_demand
        } else {
            strategy.light_transport_demand
        };

        if demand == 0 {
            score = score.saturating_sub(3000);
        } else {
            // 基本的な需要ボーナス（過剰な固定加点ではなく、主役は transport_utility に任せる）
            score += demand * 1500;
        }

        // 輸送ユニットを持ちすぎている場合は強力なペナルティを課す
        if strategy.existing_transport_count >= 1 {
            score = (score as f32 * 0.5) as u32; // 2台目以降は半減
        }
        if strategy.existing_transport_count >= 2 {
            score = score.saturating_sub(2000); // 3台目以降はさらに減点
        }
    }

    // 港での艦船ボーナス
    if produced_at == Terrain::Port && stats.movement_type == MovementType::Ship {
        score += 3000; // 港なら船を作りたい（加点を倍増）
        if stats.max_range > 1 {
            score += 2000; // 戦艦などはさらに高評価
        }
    }

    let air_response_economics = if is_v3 && !strategy.air_defense.targets.is_empty() {
        let air_coverage = candidate_air_coverage(
            stats,
            pos,
            player_id,
            &strategy.air_defense,
            map,
            master_data,
            unit_positions,
            damage_chart,
        );
        Some(air_response_economics(&strategy.air_defense, &air_coverage))
    } else {
        None
    };
    let air_response_is_justified = air_response_economics
        .is_some_and(|economics| is_economically_justified_air_response(stats.cost, economics));

    // 3. アンチ性能ボーナス
    if is_v3 {
        // #53/#55 (V3): 対編成カウンター効率。敵軍の実構成に対する価値交換の
        // 期待値 (射程・機動の相性込み) で生産を適応させる。
        // 敵がロケラン主体ならそれをアウトレンジする自走砲、航空主体なら対空、
        // のように敵の主力へのカウンターが自動的に浮上する
        let air_shortage = if air_response_is_justified {
            strategy.air_defense.shortage_ratio
        } else {
            0.0
        };
        let counter = counter_efficiency_components(stats, enemy_units, damage_chart)
            .score_with_air_shortage(air_shortage);
        let mut scaled = (counter * 3).clamp(-4000, 8000);
        // 拡張期 (未交戦) はカウンター生産よりも経済 (歩兵・輸送) を優先する。
        // 敵が別の島にいて届かない段階でカウンターユニットを量産しても
        // 価値を発揮できず、拡張と輸送の予算を食い潰すだけになるため
        if strategy.phase == GamePhase::Expansion {
            scaled /= 4;
        }
        score = score.saturating_add_signed(scaled);
    } else {
        // V2: 敵の主力ユニットに対して有利なユニットを頭数で加点する従来方式
        for (_, enemy_stats) in enemy_units {
            // 武器1での相性
            if let Some(damage) = damage_chart.get_base_damage(unit_type, enemy_stats.unit_type) {
                if damage >= 50 {
                    score += 500;
                }
                if damage >= 80 {
                    score += 1000;
                }
            }
            // 武器2での相性
            if damage_chart
                .get_base_damage_secondary(unit_type, enemy_stats.unit_type)
                .is_some_and(|damage| damage >= 30)
            {
                score += 300;
            }
        }
    }

    // 3.5. 拠点競争阻止ボーナス (Interception Score)
    for target in &strategy.priority_targets {
        if strategy.unowned_properties.contains(target) {
            for (e_pos, e_stats) in enemy_units {
                if !e_stats.can_capture {
                    continue;
                }
                let enemy_dist = (e_pos.x as isize - target.x as isize).unsigned_abs()
                    + (e_pos.y as isize - target.y as isize).unsigned_abs();
                let enemy_eta =
                    (enemy_dist as u32 + e_stats.max_movement - 1) / e_stats.max_movement.max(1);

                let my_dist = (pos.x as isize - target.x as isize).unsigned_abs()
                    + (pos.y as isize - target.y as isize).unsigned_abs();
                let my_eta = (my_dist as u32 + stats.max_movement - 1) / stats.max_movement.max(1);

                if enemy_eta <= my_eta {
                    let property_value =
                        if let Some(t_terrain) = map.get_terrain(target.x, target.y) {
                            match t_terrain {
                                Terrain::Capital => 5000,
                                Terrain::Factory => 3000,
                                Terrain::Port | Terrain::Airport => 2000,
                                _ => 1000,
                            }
                        } else {
                            1000
                        };

                    let damage_vs_enemy = damage_chart
                        .get_base_damage(unit_type, e_stats.unit_type)
                        .unwrap_or(0);

                    if damage_vs_enemy > 0 {
                        let interception_score =
                            (property_value * damage_vs_enemy) / (my_eta.max(1) * 10);
                        score += interception_score;
                    }
                }
            }
        }
    }

    // 4. 戦略フェーズボーナス
    match strategy.phase {
        GamePhase::Expansion => {
            if stats.max_movement >= 6 {
                score += 500;
            }
        }
        GamePhase::Assault | GamePhase::Contested => {
            if stats.unit_type == UnitType::Tank
                || stats.unit_type == UnitType::MdTank
                || stats.unit_type == UnitType::TankZ
            {
                score += 1000;
            }
        }
        GamePhase::Defense => {
            // 防衛時は間接攻撃や安価な壁ユニットを評価
            if stats.min_range > 1 {
                score += 1500;
            }
            if stats.cost <= 3000 {
                score += 500;
            }
        }
    }

    // 5. DemandMatrixの対空不足を、候補が実際に追加できるカバレッジへ接続する。
    if strategy.air_defense.shortage_ratio > 0.0
        && air_response_is_justified
        && let Some(economics) = air_response_economics
    {
        let bonus = (economics.protected_asset_value * strategy.air_defense.shortage_ratio * 0.5)
            .min(6_000.0) as u32;
        score = score.saturating_add(bonus);
    }
    // 十分なカバレッジ後は航空分のcounter効率と追加需要がともに0へ減衰する。
    // 地上・海上への価値や通常構成比は残すため、多用途ユニットを一律禁止しない。

    // 5.5. (V3) 盤面の敵編成へ一切ダメージを与えられない純戦闘ユニットは死に駒として除外する。
    // 対空砲・地対空ミサイルは地上/艦船ユニットへ0ダメージのため、空港のないmap_1のように
    // 敵航空戦力が存在しない盤面では戦闘価値が完全に0になる。
    // それでも下の「コストが高いほど加点」ボーナス (cost/10) だけは残るため、
    // 他候補のスコアが最低値へ潰れた局面で最も高価な対空ユニットが選ばれてしまっていた。
    // 占領・輸送・補給といった非戦闘の役割も持たない場合に限り、最低スコアへ落として生産候補から外す。
    if is_v3
        && !enemy_units.is_empty()
        && !stats.can_capture
        && stats.max_cargo == 0
        && !stats.can_supply
    {
        let can_damage_any_enemy = enemy_units.iter().any(|(_, e_stats)| {
            damage_chart
                .get_base_damage(unit_type, e_stats.unit_type)
                .unwrap_or(0)
                > 0
                || damage_chart
                    .get_base_damage_secondary(unit_type, e_stats.unit_type)
                    .unwrap_or(0)
                    > 0
        });
        if !can_damage_any_enemy {
            return 1;
        }
    }

    // 6. コストに応じたボーナスを追加して強力なユニットを作りやすくする
    if !stats.can_capture && stats.max_cargo == 0 && !stats.can_supply {
        score += stats.cost / 10;
    }

    // --- 6. 敵の脅威がない平和な時の戦闘ユニット生産ロックを無効化 ---
    // V2は戦略的に前線を押し上げるため、平和な時期でも戦闘ユニットを生産して前線へ送る。
    // (scoreのゼロ化は行わない)

    // 7. 理想構成（ideal_composition）の適用
    let mut final_score = score as i32;
    if ratio_diff > 0.0 {
        // 例: 30%足りないなら 0.3 * 4000 = 1200 のボーナス
        final_score += (ratio_diff * 4000.0) as i32;
    } else if ratio_diff < -0.1 {
        // 例: 10%以上過剰ならペナルティ
        // ただし、序盤(Expansion)で占領需要が高い時は歩兵などへのペナルティを無効化する
        if strategy.phase == GamePhase::Expansion && stats.can_capture {
            // ペナルティなし
        } else {
            final_score -= 1000;
        }
    }

    final_score.max(1) as u32
}

#[cfg(test)]
mod additional_tests {
    use super::*;
    use crate::ai::demand::{AirDefenseAssessment, AirThreatTarget};
    use crate::ai::strategy;
    use crate::components::{Ammo, Fuel, Health};
    use crate::resources::{GridTopology, Map, Terrain};

    fn campaign_test_types(master_data: &MasterDataRegistry) -> Vec<(UnitType, UnitStats)> {
        master_data
            .unit_order
            .iter()
            .filter_map(|name| master_data.create_unit_stats(name).ok())
            .map(|stats| (stats.unit_type, stats))
            .collect()
    }

    /// 既存のcampaign生産テストは同一の陸続き盤面を前提とする。実コードの
    /// 到達性引数を隠さず渡しつつ、各テストの意図を生産順・予算へ集中させる。
    fn plan_campaign_shortfall_production(
        player_id: PlayerId,
        shortfalls: &[IslandCampaignShortfall],
        facilities: &[(GridPosition, Terrain)],
        available_types: &[(UnitType, UnitStats)],
        master_data: &MasterDataRegistry,
        available_funds: u32,
    ) -> CampaignProductionOutcome {
        let map = Map::new(64, 64, Terrain::Plains, GridTopology::Square);
        super::plan_campaign_shortfall_production(
            player_id,
            shortfalls,
            facilities,
            available_types,
            &map,
            master_data,
            available_funds,
        )
    }

    fn selection_candidate(
        score: u32,
        position: GridPosition,
        unit_type: UnitType,
        cost: u32,
    ) -> ProductionCandidate {
        ProductionCandidate {
            score,
            facility_position: position,
            unit_type,
            cost,
            max_cargo: 0,
            can_capture: unit_type == UnitType::Infantry,
        }
    }

    fn completed_capability(
        player_id: PlayerId,
        position: GridPosition,
        stats: &UnitStats,
    ) -> CombatCapabilitySnapshot {
        CombatCapabilitySnapshot {
            faction: player_id,
            position,
            unit_type: stats.unit_type,
            movement_type: stats.movement_type,
            hp: 100,
            cost: stats.cost,
            max_movement: stats.max_movement,
            min_range: stats.min_range,
            max_range: stats.max_range,
            ammo1: stats.max_ammo1,
            max_ammo1: stats.max_ammo1,
            ammo2: stats.max_ammo2,
            max_ammo2: stats.max_ammo2,
            fuel: stats.max_fuel,
            action_delay: 1,
        }
    }

    fn issue75_air_defense_strategy() -> ProductionStrategy {
        ProductionStrategy {
            air_defense: AirDefenseAssessment {
                targets: vec![AirThreatTarget {
                    position: GridPosition { x: 5, y: 0 },
                    unit_type: UnitType::Bomber,
                    hp: 100,
                    cost: 20_000,
                    attack_power: 100,
                    deadline_turns: 2,
                }],
                coverage_by_target: vec![0.0],
                required_coverage: 30_000.0,
                current_coverage: 0.0,
                shortage_ratio: 1.0,
                has_effective_coverage: false,
            },
            ..ProductionStrategy::default()
        }
    }

    #[test]
    fn issue75_zero_coverage_prefers_effective_anti_air_candidate() {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(6, 1, Terrain::Plains, GridTopology::Square);
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 80);
        let available_types = vec![
            (
                UnitType::Rockets,
                UnitStats {
                    unit_type: UnitType::Rockets,
                    cost: 15_000,
                    max_movement: 5,
                    min_range: 2,
                    max_range: 5,
                    ..UnitStats::mock()
                },
            ),
            (
                UnitType::AntiAir,
                UnitStats {
                    unit_type: UnitType::AntiAir,
                    cost: 8_000,
                    max_movement: 6,
                    max_fuel: 99,
                    min_range: 1,
                    max_range: 1,
                    ..UnitStats::mock()
                },
            ),
        ];

        let candidate = select_emergency_anti_air_candidate(
            &[(GridPosition { x: 0, y: 0 }, Terrain::Factory)],
            &available_types,
            PlayerId(1),
            &issue75_air_defense_strategy(),
            &master_data,
            &map,
            &std::collections::HashMap::new(),
            &chart,
            20_000,
        )
        .unwrap();

        assert_eq!(candidate.unit_type, UnitType::AntiAir);
        assert!(candidate.coverage > 0.0);
    }

    #[test]
    fn issue75_low_value_transport_does_not_justify_emergency_purchase() {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(4, 1, Terrain::Plains, GridTopology::Square);
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::TransportHelicopter, 65);
        let strategy = ProductionStrategy {
            air_defense: AirDefenseAssessment {
                targets: vec![AirThreatTarget {
                    position: GridPosition { x: 2, y: 0 },
                    unit_type: UnitType::TransportHelicopter,
                    hp: 100,
                    cost: 4_000,
                    attack_power: 100,
                    deadline_turns: 2,
                }],
                coverage_by_target: vec![0.0],
                required_coverage: 6_000.0,
                current_coverage: 0.0,
                shortage_ratio: 1.0,
                has_effective_coverage: false,
            },
            ..ProductionStrategy::default()
        };
        let available_types = vec![(
            UnitType::AntiAir,
            UnitStats {
                unit_type: UnitType::AntiAir,
                cost: 5_500,
                movement_type: MovementType::Tank,
                max_movement: 5,
                max_fuel: 99,
                min_range: 1,
                max_range: 1,
                ..UnitStats::mock()
            },
        )];

        let candidate = select_emergency_anti_air_candidate(
            &[(GridPosition { x: 0, y: 0 }, Terrain::Factory)],
            &available_types,
            PlayerId(1),
            &strategy,
            &master_data,
            &map,
            &std::collections::HashMap::new(),
            &chart,
            20_000,
        );

        assert!(candidate.is_none());
    }

    #[test]
    fn issue75_spawned_counter_prevents_repeated_bcopters_response() {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(5, 1, Terrain::Plains, GridTopology::Square);
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bcopters, 65);
        let strategy = ProductionStrategy {
            air_defense: AirDefenseAssessment {
                targets: vec![AirThreatTarget {
                    position: GridPosition { x: 3, y: 0 },
                    unit_type: UnitType::Bcopters,
                    hp: 100,
                    cost: 7_500,
                    attack_power: 100,
                    deadline_turns: 1,
                }],
                coverage_by_target: vec![0.0],
                required_coverage: 15_000.0,
                current_coverage: 0.0,
                shortage_ratio: 1.0,
                has_effective_coverage: false,
            },
            ..ProductionStrategy::default()
        };
        let anti_air = UnitStats {
            unit_type: UnitType::AntiAir,
            cost: 5_500,
            movement_type: MovementType::Tank,
            max_movement: 5,
            max_fuel: 99,
            max_ammo1: 1,
            min_range: 1,
            max_range: 1,
            ..UnitStats::mock()
        };
        let available_types = vec![(UnitType::AntiAir, anti_air.clone())];
        let existing_units = vec![completed_capability(
            PlayerId(1),
            GridPosition { x: 0, y: 0 },
            &anti_air,
        )];

        let candidate = select_emergency_anti_air_candidate_with_existing(
            &[(GridPosition { x: 1, y: 0 }, Terrain::Factory)],
            &available_types,
            &existing_units,
            PlayerId(1),
            &strategy,
            &master_data,
            &map,
            &std::collections::HashMap::new(),
            &chart,
            20_000,
        );

        assert!(candidate.is_none());

        let mut depleted_units = existing_units;
        depleted_units[0].ammo1 = 0;
        assert!(
            select_emergency_anti_air_candidate_with_existing(
                &[(GridPosition { x: 1, y: 0 }, Terrain::Factory)],
                &available_types,
                &depleted_units,
                PlayerId(1),
                &strategy,
                &master_data,
                &map,
                &std::collections::HashMap::new(),
                &chart,
                20_000,
            )
            .is_some(),
            "弾切れ対空を満タン扱いして追加生産を抑止してはならない"
        );
    }

    #[test]
    fn issue75_bomber_response_stops_when_marginal_value_is_exhausted() {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(6, 1, Terrain::Plains, GridTopology::Square);
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 68);
        let strategy = ProductionStrategy {
            air_defense: AirDefenseAssessment {
                targets: vec![AirThreatTarget {
                    position: GridPosition { x: 4, y: 0 },
                    unit_type: UnitType::Bomber,
                    hp: 100,
                    cost: 22_000,
                    attack_power: 100,
                    deadline_turns: 1,
                }],
                coverage_by_target: vec![0.0],
                required_coverage: 44_000.0,
                current_coverage: 0.0,
                shortage_ratio: 1.0,
                has_effective_coverage: false,
            },
            ..ProductionStrategy::default()
        };
        let anti_air = UnitStats {
            unit_type: UnitType::AntiAir,
            cost: 5_500,
            movement_type: MovementType::Tank,
            max_movement: 5,
            max_fuel: 99,
            max_ammo1: 1,
            min_range: 1,
            max_range: 1,
            ..UnitStats::mock()
        };
        let available_types = vec![(UnitType::AntiAir, anti_air.clone())];
        let one_counter = vec![completed_capability(
            PlayerId(1),
            GridPosition { x: 0, y: 0 },
            &anti_air,
        )];
        let two_counters = vec![
            completed_capability(PlayerId(1), GridPosition { x: 0, y: 0 }, &anti_air),
            completed_capability(PlayerId(1), GridPosition { x: 1, y: 0 }, &anti_air),
        ];
        let facilities = [(GridPosition { x: 2, y: 0 }, Terrain::Factory)];

        assert!(
            select_emergency_anti_air_candidate_with_existing(
                &facilities,
                &available_types,
                &one_counter,
                PlayerId(1),
                &strategy,
                &master_data,
                &map,
                &std::collections::HashMap::new(),
                &chart,
                20_000,
            )
            .is_some()
        );
        assert!(
            select_emergency_anti_air_candidate_with_existing(
                &facilities,
                &available_types,
                &two_counters,
                PlayerId(1),
                &strategy,
                &master_data,
                &map,
                &std::collections::HashMap::new(),
                &chart,
                20_000,
            )
            .is_none()
        );
    }

    #[test]
    fn issue75_deadline_invalid_counter_is_not_an_emergency_purchase() {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(6, 1, Terrain::Plains, GridTopology::Square);
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 100);
        let strategy = ProductionStrategy {
            air_defense: AirDefenseAssessment {
                targets: vec![AirThreatTarget {
                    position: GridPosition { x: 5, y: 0 },
                    unit_type: UnitType::Bomber,
                    hp: 100,
                    cost: 20_000,
                    attack_power: 100,
                    deadline_turns: 1,
                }],
                coverage_by_target: vec![0.0],
                required_coverage: 40_000.0,
                current_coverage: 0.0,
                shortage_ratio: 1.0,
                has_effective_coverage: false,
            },
            ..ProductionStrategy::default()
        };
        let available_types = vec![(
            UnitType::AntiAir,
            UnitStats {
                unit_type: UnitType::AntiAir,
                cost: 5_500,
                movement_type: MovementType::Tank,
                max_movement: 1,
                max_fuel: 99,
                min_range: 1,
                max_range: 1,
                ..UnitStats::mock()
            },
        )];

        let candidate = select_emergency_anti_air_candidate(
            &[(GridPosition { x: 0, y: 0 }, Terrain::Factory)],
            &available_types,
            PlayerId(1),
            &strategy,
            &master_data,
            &map,
            &std::collections::HashMap::new(),
            &chart,
            20_000,
        );

        assert!(candidate.is_none());
    }

    #[test]
    fn issue75_emergency_candidate_targets_zero_coverage_aircraft() {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(6, 1, Terrain::Plains, GridTopology::Square);
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Missiles, UnitType::Bcopters, 100);
        chart.insert_damage(UnitType::Missiles, UnitType::Bomber, 100);
        let strategy = ProductionStrategy {
            air_defense: AirDefenseAssessment {
                targets: vec![
                    AirThreatTarget {
                        position: GridPosition { x: 2, y: 0 },
                        unit_type: UnitType::Bcopters,
                        hp: 100,
                        cost: 100_000,
                        attack_power: 100,
                        deadline_turns: 2,
                    },
                    AirThreatTarget {
                        position: GridPosition { x: 3, y: 0 },
                        unit_type: UnitType::Bomber,
                        hp: 100,
                        cost: 20_000,
                        attack_power: 100,
                        deadline_turns: 2,
                    },
                ],
                coverage_by_target: vec![100_000.0, 0.0],
                required_coverage: 180_000.0,
                current_coverage: 100_000.0,
                shortage_ratio: 80_000.0 / 180_000.0,
                has_effective_coverage: true,
            },
            ..ProductionStrategy::default()
        };
        let available_types = vec![(
            UnitType::Missiles,
            UnitStats {
                unit_type: UnitType::Missiles,
                cost: 12_000,
                movement_type: MovementType::Tank,
                max_movement: 4,
                max_fuel: 99,
                max_ammo1: 1,
                min_range: 2,
                max_range: 5,
                ..UnitStats::mock()
            },
        )];

        let candidate = select_emergency_anti_air_candidate(
            &[(GridPosition { x: 0, y: 0 }, Terrain::Factory)],
            &available_types,
            PlayerId(1),
            &strategy,
            &master_data,
            &map,
            &std::collections::HashMap::new(),
            &chart,
            12_000,
        )
        .unwrap();

        assert_eq!(candidate.coverage, 30_000.0);
        assert_eq!(candidate.protected_asset_value, 20_000.0);
    }

    #[test]
    fn issue75_unaffordable_emergency_candidate_uses_cheapest_effective_unit() {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(6, 1, Terrain::Plains, GridTopology::Square);
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 50);
        chart.insert_damage(UnitType::Missiles, UnitType::Bomber, 100);
        let available_types = vec![
            (
                UnitType::Missiles,
                UnitStats {
                    unit_type: UnitType::Missiles,
                    cost: 12_000,
                    max_movement: 5,
                    max_fuel: 99,
                    min_range: 3,
                    max_range: 5,
                    ..UnitStats::mock()
                },
            ),
            (
                UnitType::AntiAir,
                UnitStats {
                    unit_type: UnitType::AntiAir,
                    cost: 8_000,
                    max_movement: 6,
                    max_fuel: 99,
                    min_range: 1,
                    max_range: 1,
                    ..UnitStats::mock()
                },
            ),
        ];

        let candidate = select_emergency_anti_air_candidate(
            &[(GridPosition { x: 0, y: 0 }, Terrain::Factory)],
            &available_types,
            PlayerId(1),
            &issue75_air_defense_strategy(),
            &master_data,
            &map,
            &std::collections::HashMap::new(),
            &chart,
            1_000,
        )
        .unwrap();

        assert_eq!(candidate.unit_type, UnitType::AntiAir);
        assert_eq!(candidate.cost, 8_000);
    }

    #[test]
    fn issue75_unaffordable_immediate_threat_does_not_create_stale_reservation() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, mut schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let player_id = PlayerId(1);
        let bomber = world
            .resource::<UnitRegistry>()
            .get_stats(UnitType::Bomber)
            .unwrap()
            .clone();
        let infantry = world
            .resource::<UnitRegistry>()
            .get_stats(UnitType::Infantry)
            .unwrap()
            .clone();

        let unit_entities = {
            let mut query = world.query::<(Entity, &Faction)>();
            query
                .iter(&world)
                .map(|(entity, _)| entity)
                .collect::<Vec<_>>()
        };
        for entity in unit_entities {
            world.despawn(entity);
        }
        let facilities = {
            let mut query = world.query::<(&GridPosition, &Property)>();
            query
                .iter(&world)
                .filter(|(_, property)| {
                    property.owner_id == Some(player_id)
                        && master_data.is_production_facility(property.terrain.as_str())
                })
                .map(|(position, property)| (*position, property.terrain))
                .collect::<Vec<_>>()
        };
        let origin = facilities[0].0;
        let threat_position = world.resource::<Map>().get_adjacent(origin.x, origin.y)[0];
        world.spawn((
            GridPosition {
                x: threat_position.0,
                y: threat_position.1,
            },
            Faction(PlayerId(2)),
            bomber,
            Health {
                current: 100,
                max: 100,
            },
        ));
        let friendly_position = {
            let map = world.resource::<Map>();
            GridPosition {
                x: map.width - 1,
                y: map.height - 1,
            }
        };
        world.spawn((
            friendly_position,
            Faction(player_id),
            infantry,
            Health {
                current: 100,
                max: 100,
            },
        ));
        let mut updated_funds = false;
        for player in &mut world.resource_mut::<Players>().0 {
            if player.id == player_id {
                player.funds = 0;
                updated_funds = true;
            }
        }
        assert!(updated_funds);
        let mut existing_plan = ProductionPlan::default();
        existing_plan.reserves.insert(player_id.0, 3_000);
        existing_plan
            .reservations
            .insert(player_id.0, vec![UnitType::Infantry]);
        world.insert_resource(existing_plan);
        let analyzed = strategy::analyze_strategy(&mut world, player_id);
        assert!(
            analyzed.air_defense.requires_emergency_production(),
            "航空脅威が緊急生産へ接続される必要がある: {:?}",
            analyzed.air_defense
        );
        let registry = world.resource::<UnitRegistry>().clone();
        let chart = world.resource::<DamageChart>().clone();
        let available_types = registry
            .0
            .iter()
            .map(|(unit_type, stats)| (*unit_type, stats.clone()))
            .collect::<Vec<_>>();
        let candidate = select_emergency_anti_air_candidate(
            &facilities,
            &available_types,
            player_id,
            &analyzed,
            &master_data,
            world.resource::<Map>(),
            &std::collections::HashMap::new(),
            &chart,
            0,
        )
        .expect("生産可能な有効対空候補が必要");

        let commands = decide_production(&mut world, player_id);
        assert!(commands.is_empty());
        {
            let plan = world.resource::<ProductionPlan>();
            assert!(
                !plan
                    .emergency_anti_air_reservations
                    .contains_key(&player_id.0),
                "購入待ち中に期限を超える対空候補を予約してはならない"
            );
            assert_eq!(plan.reserves.get(&player_id.0), Some(&0));
            assert_eq!(
                plan.reservations.get(&player_id.0),
                Some(&vec![UnitType::Infantry])
            );
        }

        for player in &mut world.resource_mut::<Players>().0 {
            if player.id == player_id {
                player.funds = candidate.cost;
            }
        }
        let affordable_commands = decide_production(&mut world, player_id);

        assert_eq!(affordable_commands.len(), 1);
        assert_eq!(affordable_commands[0].unit_type, candidate.unit_type);
        assert!(
            !world
                .resource::<ProductionPlan>()
                .emergency_anti_air_reservations
                .contains_key(&player_id.0)
        );

        world.send_event(affordable_commands[0].clone());
        schedule.run(&mut world);
        let follow_up = decide_production(&mut world, player_id);
        assert!(
            follow_up.iter().all(|command| !matches!(
                command.unit_type,
                UnitType::AntiAir | UnitType::Missiles
            )),
            "生産済み対空を封じ込め投資として数え、同じ脅威へ連続投入しない: {:?}",
            follow_up
        );
    }

    #[test]
    fn issue75_stale_emergency_reserve_is_cleared_after_threat_disappears() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let player_id = PlayerId(1);
        let unit_entities = {
            let mut query = world.query::<(Entity, &Faction)>();
            query
                .iter(&world)
                .map(|(entity, _)| entity)
                .collect::<Vec<_>>()
        };
        for entity in unit_entities {
            world.despawn(entity);
        }
        for player in &mut world.resource_mut::<Players>().0 {
            if player.id == player_id {
                player.funds = 100_000;
            }
        }
        let mut plan = ProductionPlan::default();
        plan.reserves.insert(player_id.0, 3_000);
        plan.reservations
            .insert(player_id.0, vec![UnitType::Infantry]);
        plan.emergency_anti_air_reservations.insert(
            player_id.0,
            EmergencyAntiAirReservation {
                unit_type: UnitType::AntiAir,
                cost: 8_000,
            },
        );
        world.insert_resource(plan);

        let _ = decide_production(&mut world, player_id);

        let plan = world.resource::<ProductionPlan>();
        assert!(
            !plan
                .emergency_anti_air_reservations
                .contains_key(&player_id.0)
        );
        assert_eq!(
            plan.reservations.get(&player_id.0),
            Some(&vec![UnitType::Infantry])
        );
    }

    #[test]
    fn issue75_map2_turn7_snapshot_does_not_buy_unreachable_ground_counter() {
        #[derive(Clone, Copy)]
        struct SnapshotUnit {
            player: u32,
            unit_type: UnitType,
            x: usize,
            y: usize,
            hp: u32,
            ammo1: u32,
            ammo2: u32,
            fuel: u32,
        }

        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_2").unwrap();
        let player_id = PlayerId(1);
        let registry = world.resource::<UnitRegistry>().clone();

        let unit_entities = {
            let mut query = world.query::<(Entity, &Faction)>();
            query
                .iter(&world)
                .map(|(entity, _)| entity)
                .collect::<Vec<_>>()
        };
        for entity in unit_entities {
            world.despawn(entity);
        }

        // battle_map2 Turn 7 の行動後・生産直前の所有状況を固定 fixture として再現する。
        let property_owners = [
            ((3, 3), 1),
            ((4, 3), 1),
            ((6, 3), 1),
            ((10, 3), 2),
            ((3, 4), 1),
            ((4, 4), 1),
            ((3, 5), 1),
            ((7, 5), 2),
            ((5, 6), 1),
            ((10, 6), 2),
            ((3, 7), 2),
            ((8, 8), 2),
            ((10, 8), 2),
            ((8, 9), 2),
            ((10, 9), 2),
            ((3, 10), 2),
            ((7, 10), 2),
            ((9, 10), 2),
            ((10, 10), 2),
        ]
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
        {
            let mut query = world.query::<(&GridPosition, &mut Property)>();
            for (position, mut property) in query.iter_mut(&mut world) {
                property.owner_id = property_owners
                    .get(&(position.x, position.y))
                    .copied()
                    .map(PlayerId);
            }
        }

        let units = [
            SnapshotUnit {
                player: 1,
                unit_type: UnitType::TransportHelicopter,
                x: 4,
                y: 4,
                hp: 62,
                ammo1: 8,
                ammo2: 0,
                fuel: 57,
            },
            SnapshotUnit {
                player: 1,
                unit_type: UnitType::Rockets,
                x: 5,
                y: 6,
                hp: 100,
                ammo1: 4,
                ammo2: 0,
                fuel: 50,
            },
            SnapshotUnit {
                player: 1,
                unit_type: UnitType::Infantry,
                x: 3,
                y: 6,
                hp: 52,
                ammo1: 8,
                ammo2: 0,
                fuel: 96,
            },
            SnapshotUnit {
                player: 1,
                unit_type: UnitType::Rockets,
                x: 3,
                y: 4,
                hp: 100,
                ammo1: 3,
                ammo2: 0,
                fuel: 50,
            },
            SnapshotUnit {
                player: 1,
                unit_type: UnitType::Bcopters,
                x: 2,
                y: 5,
                hp: 47,
                ammo1: 2,
                ammo2: 8,
                fuel: 60,
            },
            SnapshotUnit {
                player: 1,
                unit_type: UnitType::Artillery,
                x: 3,
                y: 3,
                hp: 100,
                ammo1: 4,
                ammo2: 0,
                fuel: 1,
            },
            SnapshotUnit {
                player: 1,
                unit_type: UnitType::Mech,
                x: 6,
                y: 4,
                hp: 100,
                ammo1: 3,
                ammo2: 3,
                fuel: 68,
            },
            SnapshotUnit {
                player: 2,
                unit_type: UnitType::Infantry,
                x: 10,
                y: 3,
                hp: 100,
                ammo1: 9,
                ammo2: 0,
                fuel: 99,
            },
            SnapshotUnit {
                player: 2,
                unit_type: UnitType::Infantry,
                x: 6,
                y: 6,
                hp: 100,
                ammo1: 8,
                ammo2: 0,
                fuel: 91,
            },
            SnapshotUnit {
                player: 2,
                unit_type: UnitType::Infantry,
                x: 3,
                y: 7,
                hp: 23,
                ammo1: 9,
                ammo2: 0,
                fuel: 96,
            },
            SnapshotUnit {
                player: 2,
                unit_type: UnitType::Infantry,
                x: 5,
                y: 7,
                hp: 80,
                ammo1: 9,
                ammo2: 0,
                fuel: 99,
            },
            SnapshotUnit {
                player: 2,
                unit_type: UnitType::Infantry,
                x: 7,
                y: 5,
                hp: 62,
                ammo1: 8,
                ammo2: 0,
                fuel: 94,
            },
            SnapshotUnit {
                player: 2,
                unit_type: UnitType::Rockets,
                x: 5,
                y: 9,
                hp: 100,
                ammo1: 3,
                ammo2: 0,
                fuel: 47,
            },
            SnapshotUnit {
                player: 2,
                unit_type: UnitType::Infantry,
                x: 8,
                y: 5,
                hp: 100,
                ammo1: 9,
                ammo2: 0,
                fuel: 97,
            },
            SnapshotUnit {
                player: 2,
                unit_type: UnitType::AntiAir,
                x: 7,
                y: 7,
                hp: 100,
                ammo1: 5,
                ammo2: 0,
                fuel: 46,
            },
            SnapshotUnit {
                player: 2,
                unit_type: UnitType::Bcopters,
                x: 1,
                y: 5,
                hp: 51,
                ammo1: 3,
                ammo2: 8,
                fuel: 56,
            },
            SnapshotUnit {
                player: 2,
                unit_type: UnitType::Mech,
                x: 5,
                y: 8,
                hp: 100,
                ammo1: 3,
                ammo2: 3,
                fuel: 66,
            },
            SnapshotUnit {
                player: 2,
                unit_type: UnitType::Mech,
                x: 7,
                y: 6,
                hp: 100,
                ammo1: 3,
                ammo2: 3,
                fuel: 68,
            },
            SnapshotUnit {
                player: 2,
                unit_type: UnitType::Bomber,
                x: 8,
                y: 9,
                hp: 100,
                ammo1: 6,
                ammo2: 0,
                fuel: 90,
            },
        ];
        for unit in units {
            let stats = registry.get_stats(unit.unit_type).unwrap().clone();
            world.spawn((
                GridPosition {
                    x: unit.x,
                    y: unit.y,
                },
                Faction(PlayerId(unit.player)),
                stats.clone(),
                Health {
                    current: unit.hp,
                    max: 100,
                },
                Ammo {
                    ammo1: unit.ammo1,
                    max_ammo1: stats.max_ammo1,
                    ammo2: unit.ammo2,
                    max_ammo2: stats.max_ammo2,
                },
                Fuel {
                    current: unit.fuel,
                    max: stats.max_fuel,
                },
            ));
        }
        for player in &mut world.resource_mut::<Players>().0 {
            if player.id == player_id {
                player.funds = 13_026;
            }
        }
        world.insert_resource(ProductionPlan::default());

        let analyzed = strategy::analyze_strategy(&mut world, player_id);
        assert_eq!(analyzed.air_defense.targets.len(), 2);
        assert!(analyzed.air_defense.shortage_ratio > 0.0);
        assert!(
            analyzed.air_defense.requires_emergency_production(),
            "爆撃機または戦闘ヘリが未カバーのままである必要がある: {:?}",
            analyzed.air_defense
        );

        let commands = decide_production(&mut world, player_id);

        assert!(!commands.is_empty());
        assert!(
            commands.iter().all(|command| !matches!(
                command.unit_type,
                UnitType::AntiAir | UnitType::Missiles
            )),
            "期限内に島を越えられない地上対空へ投資してはならない: {:?}",
            commands
        );
        assert!(
            !world
                .resource::<ProductionPlan>()
                .emergency_anti_air_reservations
                .contains_key(&player_id.0),
            "期限に間に合わない高価な対空候補へ貯金してはならない"
        );
    }

    /// 敵航空戦力が存在しない盤面で、地上へ0ダメージの対空ユニットが
    /// 「コストが高いほど加点」ボーナスだけで選ばれてしまう退行を防ぐ。
    /// map_1 のように空港がないマップでは対空戦車は完全な死に駒になる。
    fn no_air_threat_score(unit_type: UnitType, cost: u32, is_v3: bool) -> u32 {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(6, 1, Terrain::Plains, GridTopology::Square);
        let mut chart = DamageChart::new();
        // 対空ユニットは航空機にしかダメージを与えられない
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 80);
        // 戦車は地上の敵戦車を撃破できる
        chart.insert_damage(UnitType::Tank, UnitType::Tank, 55);
        let stats = UnitStats {
            unit_type,
            cost,
            max_movement: 6,
            max_fuel: 99,
            min_range: 1,
            max_range: 1,
            ..UnitStats::mock()
        };
        let enemy_tank = UnitStats {
            unit_type: UnitType::Tank,
            cost: 7_000,
            max_movement: 6,
            max_fuel: 99,
            min_range: 1,
            max_range: 1,
            ..UnitStats::mock()
        };
        // 航空脅威なし = air_defense.targets が空のデフォルト戦略
        let strategy = ProductionStrategy::default();
        calculate_unit_score_at(
            unit_type,
            &stats,
            GridPosition { x: 0, y: 0 },
            PlayerId(1),
            &strategy,
            &[(GridPosition { x: 5, y: 0 }, enemy_tank)],
            &[],
            &chart,
            &master_data,
            &map,
            &std::collections::HashMap::new(),
            &UnitRegistry(std::collections::HashMap::new()),
            Terrain::Factory,
            0.0,
            is_v3,
        )
    }

    #[test]
    fn issue75_v3_rejects_anti_air_without_any_enemy_air() {
        // 対空戦車は敵地上戦車へ0ダメージなので最低スコアへ落ちる
        let anti_air = no_air_threat_score(UnitType::AntiAir, 5_500, true);
        // 戦車は敵地上戦車を撃破できるので通常評価される
        let tank = no_air_threat_score(UnitType::Tank, 7_000, true);

        assert_eq!(
            anti_air, 1,
            "航空脅威が無い盤面の対空ユニットは死に駒として最低スコアにする"
        );
        assert!(
            tank > anti_air,
            "有効打を持つ戦車が対空ユニットより高く評価される必要がある: tank={tank}, anti_air={anti_air}"
        );
    }

    #[test]
    fn issue75_v2_keeps_legacy_anti_air_score_without_enemy_air() {
        // V1/V2 は評価の基準線として従来挙動のまま維持する
        let anti_air = no_air_threat_score(UnitType::AntiAir, 5_500, false);
        assert!(
            anti_air > 1,
            "V2の従来スコアリングを変更してはならない: anti_air={anti_air}"
        );
    }

    #[test]
    fn issue75_air_shortage_increases_effective_candidate_score() {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(6, 1, Terrain::Plains, GridTopology::Square);
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 80);
        let anti_air = UnitStats {
            unit_type: UnitType::AntiAir,
            cost: 8_000,
            max_movement: 6,
            max_fuel: 99,
            min_range: 1,
            max_range: 1,
            ..UnitStats::mock()
        };
        let registry = UnitRegistry(std::collections::HashMap::new());
        let shortage = issue75_air_defense_strategy();
        let mut covered = shortage.clone();
        let required_coverage = covered.air_defense.required_coverage;
        covered
            .air_defense
            .apply_coverage(&crate::ai::demand::AirCoverageContribution {
                by_target: vec![required_coverage],
                total: required_coverage,
            });

        let score_with_shortage = calculate_unit_score_at(
            UnitType::AntiAir,
            &anti_air,
            GridPosition { x: 0, y: 0 },
            PlayerId(1),
            &shortage,
            &[],
            &[],
            &chart,
            &master_data,
            &map,
            &std::collections::HashMap::new(),
            &registry,
            Terrain::Factory,
            0.0,
            true,
        );
        let score_after_coverage = calculate_unit_score_at(
            UnitType::AntiAir,
            &anti_air,
            GridPosition { x: 0, y: 0 },
            PlayerId(1),
            &covered,
            &[],
            &[],
            &chart,
            &master_data,
            &map,
            &std::collections::HashMap::new(),
            &registry,
            Terrain::Factory,
            0.0,
            true,
        );

        assert!(score_with_shortage > score_after_coverage);
        assert!(!covered.air_defense.requires_emergency_production());
    }

    #[test]
    fn issue75_uneconomic_air_target_does_not_feed_generic_counter_score() {
        let master_data = MasterDataRegistry::load().unwrap();
        let map = Map::new(6, 1, Terrain::Plains, GridTopology::Square);
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Missiles, UnitType::Bcopters, 100);
        let missiles = UnitStats {
            unit_type: UnitType::Missiles,
            movement_type: MovementType::Tank,
            cost: 12_000,
            max_movement: 4,
            max_fuel: 99,
            max_ammo1: 1,
            min_range: 2,
            max_range: 5,
            ..UnitStats::mock()
        };
        let enemy_bcopters = UnitStats {
            unit_type: UnitType::Bcopters,
            movement_type: MovementType::Air,
            cost: 7_500,
            max_movement: 6,
            max_range: 1,
            ..UnitStats::mock()
        };
        let target = AirThreatTarget {
            position: GridPosition { x: 5, y: 0 },
            unit_type: UnitType::Bcopters,
            hp: 100,
            cost: 7_500,
            attack_power: 100,
            deadline_turns: 2,
        };
        let required_coverage = 11_250.0;
        let shortage = ProductionStrategy {
            air_defense: AirDefenseAssessment {
                targets: vec![target],
                coverage_by_target: vec![0.0],
                required_coverage,
                current_coverage: 0.0,
                shortage_ratio: 1.0,
                has_effective_coverage: false,
            },
            ..ProductionStrategy::default()
        };
        let mut covered = shortage.clone();
        covered
            .air_defense
            .apply_coverage(&AirCoverageContribution {
                by_target: vec![required_coverage],
                total: required_coverage,
            });
        let enemy_units = vec![(GridPosition { x: 5, y: 0 }, enemy_bcopters)];
        let registry = UnitRegistry(std::collections::HashMap::new());
        let score = |strategy: &ProductionStrategy| {
            calculate_unit_score_at(
                UnitType::Missiles,
                &missiles,
                GridPosition { x: 0, y: 0 },
                PlayerId(1),
                strategy,
                &enemy_units,
                &[],
                &chart,
                &master_data,
                &map,
                &std::collections::HashMap::new(),
                &registry,
                Terrain::Factory,
                0.0,
                true,
            )
        };

        assert_eq!(score(&shortage), score(&covered));
    }

    #[test]
    fn equal_score_selection_is_insertion_order_independent() {
        let candidates = vec![
            selection_candidate(100, GridPosition { x: 1, y: 0 }, UnitType::Infantry, 1_000),
            selection_candidate(100, GridPosition { x: 0, y: 0 }, UnitType::Mech, 3_000),
            selection_candidate(100, GridPosition { x: 0, y: 0 }, UnitType::Infantry, 1_000),
        ];
        let mut reversed = candidates.clone();
        reversed.reverse();

        let selected = select_best_production_candidate(&candidates).unwrap();
        let reversed_selected = select_best_production_candidate(&reversed).unwrap();

        assert_eq!(selected.facility_position, GridPosition { x: 0, y: 0 });
        assert_eq!(selected.unit_type, UnitType::Infantry);
        assert_eq!(
            (
                reversed_selected.facility_position,
                reversed_selected.unit_type
            ),
            (selected.facility_position, selected.unit_type)
        );
    }

    #[test]
    fn equal_score_selection_uses_cost_after_type_rank() {
        let candidates = vec![
            selection_candidate(100, GridPosition { x: 0, y: 0 }, UnitType::Infantry, 1_001),
            selection_candidate(100, GridPosition { x: 0, y: 0 }, UnitType::Infantry, 1_000),
        ];

        assert_eq!(
            select_best_production_candidate(&candidates).unwrap().cost,
            1_000
        );
    }

    fn transport_score(
        unit_type: UnitType,
        strategy: &ProductionStrategy,
        map: &Map,
        is_v3: bool,
    ) -> u32 {
        let master_data = MasterDataRegistry::load().unwrap();
        let stats = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                unit_type.as_str().to_owned(),
            ))
            .unwrap();
        let damage_chart = DamageChart::new();
        let unit_registry = UnitRegistry(std::collections::HashMap::new());
        calculate_unit_score_at(
            unit_type,
            &stats,
            GridPosition { x: 0, y: 0 },
            PlayerId(1),
            strategy,
            &[],
            &[],
            &damage_chart,
            &master_data,
            map,
            &std::collections::HashMap::new(),
            &unit_registry,
            Terrain::Factory,
            0.0,
            is_v3,
        )
    }

    fn transport_test_candidate() -> (GridPosition, UnitStats, f32) {
        let master_data = MasterDataRegistry::load().unwrap();
        let infantry = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        (GridPosition { x: 0, y: 0 }, infantry, 10_000.0)
    }

    fn offshore_test_portfolio(
        target: GridPosition,
    ) -> crate::ai::island_campaign::IslandCampaignPortfolio {
        let empty_requirement = crate::ai::island_campaign::IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_budget: 0,
            total_budget: 0,
        };
        crate::ai::island_campaign::IslandCampaignPortfolio {
            islands: Vec::new(),
            active_offensives: vec![crate::ai::island_campaign::IslandCampaignAssignment {
                island_id: crate::ai::islands::IslandId(0),
                decision: crate::ai::island_campaign::IslandCampaignDecision::Expand,
                target_position: target,
                capture_target_positions: vec![target],
                priority_enemy_types: Vec::new(),
                requirement: empty_requirement.clone(),
                purchase_shortfall: empty_requirement,
                allocated_budget: 0,
                transport_entities: Vec::new(),
                capture_entities: Vec::new(),
                combat_entities: Vec::new(),
                operation_ready: true,
                continued_from_existing_squad: false,
            }],
            defenses: Vec::new(),
        }
    }

    #[test]
    fn targetless_candidate_does_not_inflate_recon_score() {
        let map = Map::new(5, 1, Terrain::Plains, GridTopology::Square);
        let strategy = ProductionStrategy {
            light_transport_demand: 1,
            transport_candidates: vec![transport_test_candidate()],
            ..ProductionStrategy::default()
        };
        let mut control = strategy.clone();
        control.transport_candidates.clear();

        assert_eq!(
            transport_score(UnitType::Recon, &strategy, &map, true),
            transport_score(UnitType::Recon, &control, &map, true)
        );
    }

    #[test]
    fn v3_offshore_transport_utility_excludes_ground_carriers() {
        let target = GridPosition { x: 4, y: 0 };
        let map = Map::new(5, 1, Terrain::Sea, GridTopology::Square);
        let strategy = ProductionStrategy {
            light_transport_demand: 1,
            heavy_transport_demand: 1,
            transport_candidates: vec![transport_test_candidate()],
            campaign_portfolio: offshore_test_portfolio(target),
            ..ProductionStrategy::default()
        };
        let mut control = strategy.clone();
        control.transport_candidates.clear();

        assert!(
            transport_score(UnitType::TransportHelicopter, &strategy, &map, true)
                > transport_score(UnitType::TransportHelicopter, &control, &map, true)
        );
        assert!(
            transport_score(UnitType::Lander, &strategy, &map, true)
                > transport_score(UnitType::Lander, &control, &map, true)
        );
        assert_eq!(
            transport_score(UnitType::Recon, &strategy, &map, true),
            transport_score(UnitType::Recon, &control, &map, true)
        );
    }

    #[test]
    fn v1_recon_keeps_ground_transport_utility() {
        let target = GridPosition { x: 4, y: 0 };
        let map = Map::new(5, 1, Terrain::Plains, GridTopology::Square);
        let strategy = ProductionStrategy {
            priority_targets: vec![target],
            light_transport_demand: 1,
            transport_candidates: vec![transport_test_candidate()],
            ..ProductionStrategy::default()
        };
        let mut control = strategy.clone();
        control.transport_candidates.clear();

        assert!(
            transport_score(UnitType::Recon, &strategy, &map, false)
                > transport_score(UnitType::Recon, &control, &map, false)
        );
    }

    #[test]
    fn v3_transport_demand_consumption_uses_offshore_types_only() {
        let master_data = MasterDataRegistry::load().unwrap();
        let stats = |unit_type: UnitType| {
            master_data
                .create_unit_stats(&crate::resources::master_data::UnitName(
                    unit_type.as_str().to_owned(),
                ))
                .unwrap()
        };

        let mut recon_strategy = ProductionStrategy {
            light_transport_demand: 2,
            heavy_transport_demand: 1,
            ..ProductionStrategy::default()
        };
        let recon = stats(UnitType::Recon);
        consume_transport_demand_after_production(
            &mut recon_strategy,
            UnitType::Recon,
            recon.max_cargo,
            true,
        );
        assert_eq!(recon_strategy.light_transport_demand, 2);
        assert_eq!(recon_strategy.heavy_transport_demand, 1);

        let mut helicopter_strategy = recon_strategy.clone();
        let helicopter = stats(UnitType::TransportHelicopter);
        consume_transport_demand_after_production(
            &mut helicopter_strategy,
            UnitType::TransportHelicopter,
            helicopter.max_cargo,
            true,
        );
        assert_eq!(helicopter_strategy.light_transport_demand, 0);
        assert_eq!(helicopter_strategy.heavy_transport_demand, 1);

        let mut heavy_lander_strategy = recon_strategy.clone();
        let lander = stats(UnitType::Lander);
        consume_transport_demand_after_production(
            &mut heavy_lander_strategy,
            UnitType::Lander,
            lander.max_cargo,
            true,
        );
        assert_eq!(heavy_lander_strategy.light_transport_demand, 2);
        assert_eq!(heavy_lander_strategy.heavy_transport_demand, 0);

        let mut light_lander_strategy = ProductionStrategy {
            light_transport_demand: 2,
            heavy_transport_demand: 0,
            ..ProductionStrategy::default()
        };
        consume_transport_demand_after_production(
            &mut light_lander_strategy,
            UnitType::Lander,
            lander.max_cargo,
            true,
        );
        assert_eq!(light_lander_strategy.light_transport_demand, 0);
    }

    #[test]
    fn v1_ground_carrier_keeps_legacy_transport_demand_consumption() {
        let master_data = MasterDataRegistry::load().unwrap();
        let recon = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Recon.as_str().to_owned(),
            ))
            .unwrap();
        let mut strategy = ProductionStrategy {
            light_transport_demand: 2,
            ..ProductionStrategy::default()
        };

        consume_transport_demand_after_production(
            &mut strategy,
            UnitType::Recon,
            recon.max_cargo,
            false,
        );

        assert_eq!(strategy.light_transport_demand, 1);
    }

    #[test]
    fn campaign_production_rejects_zero_capacity_transport() {
        let master_data = MasterDataRegistry::load().unwrap();
        let mut helicopter = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::TransportHelicopter.as_str().to_owned(),
            ))
            .unwrap();
        helicopter.max_cargo = 0;
        let rows = vec![crate::ai::island_campaign::IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: crate::ai::island_campaign::IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 0, y: 0 },
            light_transport_slots: 1,
            heavy_transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_budget: 0,
            reserved_budget: helicopter.cost,
            priority_rank: 0,
            priority_enemy_types: Vec::new(),
        }];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &[(GridPosition { x: 0, y: 0 }, Terrain::Airport)],
            &[(UnitType::TransportHelicopter, helicopter)],
            &master_data,
            u32::MAX,
        );

        assert!(outcome.commands.is_empty());
        assert!(!outcome.completed_all_rows);
    }

    #[test]
    fn campaign_production_rejects_zero_cost_combat_unit() {
        let master_data = MasterDataRegistry::load().unwrap();
        let mut infantry = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        infantry.cost = 0;
        let rows = vec![crate::ai::island_campaign::IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: crate::ai::island_campaign::IslandCampaignDecision::Defend,
            target_position: GridPosition { x: 0, y: 0 },
            light_transport_slots: 0,
            heavy_transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_budget: 1,
            reserved_budget: 1,
            priority_rank: 0,
            priority_enemy_types: Vec::new(),
        }];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &[(GridPosition { x: 0, y: 0 }, Terrain::Factory)],
            &[(UnitType::Infantry, infantry)],
            &master_data,
            1,
        );

        assert!(outcome.commands.is_empty());
        assert!(!outcome.completed_all_rows);
    }

    #[test]
    fn campaign_combat_does_not_buy_a_tank_for_a_sea_separated_target() {
        let master_data = MasterDataRegistry::load().unwrap();
        let tank = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::TankZ.as_str().to_owned(),
            ))
            .unwrap();
        let fighter = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Fighter.as_str().to_owned(),
            ))
            .unwrap();
        let rows = vec![IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(1),
            decision: IslandCampaignDecision::Assault,
            target_position: GridPosition { x: 3, y: 0 },
            light_transport_slots: 0,
            heavy_transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_budget: tank.cost,
            reserved_budget: tank.cost,
            priority_rank: 0,
            priority_enemy_types: Vec::new(),
        }];
        let facilities = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Factory),
            (GridPosition { x: 1, y: 0 }, Terrain::Airport),
        ];
        let mut map = Map::new(4, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Factory).unwrap();
        map.set_terrain(1, 0, Terrain::Airport).unwrap();
        map.set_terrain(2, 0, Terrain::Sea).unwrap();
        map.set_terrain(3, 0, Terrain::City).unwrap();

        let outcome = super::plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &facilities,
            &[(UnitType::TankZ, tank), (UnitType::Fighter, fighter)],
            &map,
            &master_data,
            u32::MAX,
        );

        assert_eq!(outcome.commands.len(), 1);
        assert_eq!(outcome.commands[0].unit_type, UnitType::Fighter);
    }

    #[test]
    fn expansion_denial_reserves_one_of_two_airports_when_surplus_can_fund_air_power() {
        let master_data = MasterDataRegistry::load().unwrap();
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 65);
        let helicopter = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Bcopters.as_str().to_owned(),
            ))
            .unwrap();
        let infantry = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        let airports = vec![
            (GridPosition { x: 1, y: 0 }, Terrain::Airport),
            (GridPosition { x: 2, y: 0 }, Terrain::Airport),
        ];

        assert_eq!(
            select_expansion_denial_airport(
                &airports,
                2,
                &[(UnitType::Bcopters, helicopter.clone())],
                std::slice::from_ref(&infantry),
                &damage_chart,
                &master_data,
                8_000,
            ),
            Some(GridPosition { x: 2, y: 0 })
        );
        assert_eq!(
            select_expansion_denial_airport(
                &airports,
                2,
                &[(UnitType::Bcopters, helicopter)],
                &[infantry],
                &damage_chart,
                &master_data,
                7_000,
            ),
            None,
        );
    }

    #[test]
    fn campaign_replans_one_transport_airport_for_expansion_denial() {
        let master_data = MasterDataRegistry::load().unwrap();
        let transport = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::TransportHelicopter.as_str().to_owned(),
            ))
            .unwrap();
        let helicopter = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Bcopters.as_str().to_owned(),
            ))
            .unwrap();
        let infantry = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        let shortfalls = vec![IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(1),
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 8, y: 0 },
            light_transport_slots: 4,
            heavy_transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_budget: 0,
            reserved_budget: 8_000,
            priority_rank: 0,
            priority_enemy_types: Vec::new(),
        }];
        let airports = vec![
            (GridPosition { x: 1, y: 0 }, Terrain::Airport),
            (GridPosition { x: 2, y: 0 }, Terrain::Airport),
        ];
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 65);
        let map = Map::new(10, 1, Terrain::Plains, GridTopology::Square);

        let outcome = plan_campaign_with_expansion_denial_reserve(
            PlayerId(1),
            &shortfalls,
            &airports,
            2,
            &[
                (UnitType::TransportHelicopter, transport),
                (UnitType::Bcopters, helicopter),
            ],
            &[infantry],
            &damage_chart,
            &map,
            &master_data,
            20_000,
        );

        assert_eq!(outcome.commands.len(), 1);
        assert_eq!(outcome.commands[0].unit_type, UnitType::TransportHelicopter);
        // 現在手番に実際に残った現金は、空けた第2空港のCombat計画へ使える。
        assert_eq!(outcome.generic_funds, 16_000);
    }

    #[test]
    fn expansion_denial_can_use_current_cash_before_future_transport_reservations() {
        let master_data = MasterDataRegistry::load().unwrap();
        let transport = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::TransportHelicopter.as_str().to_owned(),
            ))
            .unwrap();
        let helicopter = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Bcopters.as_str().to_owned(),
            ))
            .unwrap();
        let infantry = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        let shortfalls = vec![IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(1),
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 8, y: 0 },
            light_transport_slots: 8,
            heavy_transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_budget: 0,
            reserved_budget: 16_000,
            priority_rank: 0,
            priority_enemy_types: Vec::new(),
        }];
        let airports = vec![
            (GridPosition { x: 1, y: 0 }, Terrain::Airport),
            (GridPosition { x: 2, y: 0 }, Terrain::Airport),
        ];
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 65);
        let map = Map::new(10, 1, Terrain::Plains, GridTopology::Square);

        let outcome = plan_campaign_with_expansion_denial_reserve(
            PlayerId(1),
            &shortfalls,
            &airports,
            2,
            &[
                (UnitType::TransportHelicopter, transport),
                (UnitType::Bcopters, helicopter),
            ],
            &[infantry],
            &damage_chart,
            &map,
            &master_data,
            20_000,
        );

        assert_eq!(outcome.commands.len(), 1);
        assert_eq!(outcome.commands[0].unit_type, UnitType::TransportHelicopter);
        // 旧実装は将来の輸送予約12,000を全額保護してgenericへ4,000しか渡さず、
        // 空いた第2空港と現在資金16,000があっても戦闘ヘリを作れなかった。
        assert_eq!(outcome.remaining_funds, 16_000);
        assert_eq!(outcome.generic_funds, 16_000);
    }

    #[test]
    fn campaign_production_services_higher_priority_row_before_lower_rows() {
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let rows = vec![
            crate::ai::island_campaign::IslandCampaignShortfall {
                island_id: crate::ai::islands::IslandId(0),
                decision: crate::ai::island_campaign::IslandCampaignDecision::Defend,
                target_position: GridPosition { x: 0, y: 0 },
                light_transport_slots: 0,
                heavy_transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_budget: 7_000,
                reserved_budget: 7_000,
                priority_rank: 0,
                priority_enemy_types: Vec::new(),
            },
            crate::ai::island_campaign::IslandCampaignShortfall {
                island_id: crate::ai::islands::IslandId(1),
                decision: crate::ai::island_campaign::IslandCampaignDecision::Expand,
                target_position: GridPosition { x: 0, y: 0 },
                light_transport_slots: 2,
                heavy_transport_slots: 0,
                capture_units: 2,
                ground_combat_units: 0,
                combat_budget: 0,
                reserved_budget: 6_000,
                priority_rank: 2,
                priority_enemy_types: Vec::new(),
            },
            crate::ai::island_campaign::IslandCampaignShortfall {
                island_id: crate::ai::islands::IslandId(2),
                decision: crate::ai::island_campaign::IslandCampaignDecision::Assault,
                target_position: GridPosition { x: 0, y: 0 },
                light_transport_slots: 2,
                heavy_transport_slots: 2,
                capture_units: 2,
                ground_combat_units: 0,
                combat_budget: 10_200,
                reserved_budget: 32_700,
                priority_rank: 4,
                priority_enemy_types: Vec::new(),
            },
        ];
        let facilities = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Factory),
            (GridPosition { x: 1, y: 0 }, Terrain::Airport),
        ];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &facilities,
            &available_types,
            &master_data,
            7_000,
        );

        assert_eq!(outcome.commands.len(), 1);
        let produced_stats = available_types
            .iter()
            .find(|(unit_type, _)| *unit_type == outcome.commands[0].unit_type)
            .map(|(_, stats)| stats)
            .unwrap();
        assert!(produced_stats.cost <= 7_000);
        assert!(!produced_stats.can_capture);
        assert!(!matches!(
            outcome.commands[0].unit_type,
            UnitType::Lander | UnitType::TransportHelicopter | UnitType::SupplyTruck
        ));
        assert!(!outcome.completed_all_rows);
    }

    #[test]
    fn campaign_production_uses_non_competing_airport_without_spending_secure_reserve() {
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let rows = vec![
            crate::ai::island_campaign::IslandCampaignShortfall {
                island_id: crate::ai::islands::IslandId(5),
                decision: crate::ai::island_campaign::IslandCampaignDecision::Secure,
                target_position: GridPosition { x: 0, y: 0 },
                light_transport_slots: 0,
                heavy_transport_slots: 0,
                capture_units: 7,
                ground_combat_units: 0,
                combat_budget: 0,
                reserved_budget: 7_000,
                priority_rank: 2,
                priority_enemy_types: Vec::new(),
            },
            crate::ai::island_campaign::IslandCampaignShortfall {
                island_id: crate::ai::islands::IslandId(2),
                decision: crate::ai::island_campaign::IslandCampaignDecision::Expand,
                target_position: GridPosition { x: 0, y: 0 },
                light_transport_slots: 5,
                heavy_transport_slots: 0,
                capture_units: 5,
                ground_combat_units: 0,
                combat_budget: 0,
                reserved_budget: 17_000,
                priority_rank: 6,
                priority_enemy_types: Vec::new(),
            },
        ];
        let mut facilities = (0..5)
            .map(|x| (GridPosition { x, y: 0 }, Terrain::Factory))
            .collect::<Vec<_>>();
        facilities.push((GridPosition { x: 5, y: 0 }, Terrain::Airport));

        let outcome = plan_campaign_shortfall_production(
            PlayerId(2),
            &rows,
            &facilities,
            &available_types,
            &master_data,
            14_000,
        );

        assert_eq!(
            outcome
                .commands
                .iter()
                .filter(|command| command.unit_type == UnitType::Infantry)
                .count(),
            5
        );
        assert_eq!(
            outcome
                .commands
                .iter()
                .filter(|command| command.unit_type == UnitType::TransportHelicopter)
                .count(),
            1
        );
        // Secureの未生産歩兵2体分は下位Expandへ流用しない。
        assert_eq!(outcome.remaining_funds, 5_000);
        assert_eq!(outcome.generic_funds, 0);
        assert!(!outcome.completed_all_rows);
    }

    #[test]
    fn campaign_production_exposes_only_funds_above_all_remaining_reservations() {
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let rows = vec![crate::ai::island_campaign::IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: crate::ai::island_campaign::IslandCampaignDecision::Secure,
            target_position: GridPosition { x: 0, y: 0 },
            light_transport_slots: 0,
            heavy_transport_slots: 0,
            capture_units: 1,
            ground_combat_units: 0,
            combat_budget: 0,
            reserved_budget: 1_000,
            priority_rank: 1,
            priority_enemy_types: Vec::new(),
        }];
        let facilities = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Factory),
            (GridPosition { x: 1, y: 0 }, Terrain::Factory),
        ];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &facilities,
            &available_types,
            &master_data,
            10_000,
        );

        assert_eq!(outcome.commands.len(), 1);
        assert_eq!(outcome.commands[0].unit_type, UnitType::Infantry);
        assert_eq!(outcome.remaining_funds, 9_000);
        assert_eq!(outcome.generic_funds, 9_000);
        assert!(outcome.completed_all_rows);
    }

    #[test]
    fn campaign_combat_remainder_uses_reserved_real_unit_cost() {
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let facilities = vec![(GridPosition { x: 0, y: 0 }, Terrain::Factory)];
        let row =
            |combat_budget, reserved_budget| crate::ai::island_campaign::IslandCampaignShortfall {
                island_id: crate::ai::islands::IslandId(0),
                decision: crate::ai::island_campaign::IslandCampaignDecision::Defend,
                target_position: GridPosition { x: 0, y: 0 },
                light_transport_slots: 0,
                heavy_transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_budget,
                reserved_budget,
                priority_rank: 0,
                priority_enemy_types: Vec::new(),
            };

        let small_remainder = plan_campaign_shortfall_production(
            PlayerId(1),
            &[row(80, 1_000)],
            &facilities,
            &available_types,
            &master_data,
            1_000,
        );
        let large = plan_campaign_shortfall_production(
            PlayerId(1),
            &[row(30_000, 30_000)],
            &facilities,
            &available_types,
            &master_data,
            30_000,
        );

        assert_eq!(small_remainder.commands.len(), 1);
        assert!(small_remainder.completed_all_rows);
        assert_eq!(large.commands.len(), 1);
        assert!(!large.completed_all_rows);
    }

    #[test]
    fn campaign_assault_production_requires_lander_and_helicopter() {
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let rows = vec![crate::ai::island_campaign::IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: crate::ai::island_campaign::IslandCampaignDecision::Assault,
            target_position: GridPosition { x: 0, y: 0 },
            light_transport_slots: 2,
            heavy_transport_slots: 2,
            capture_units: 0,
            ground_combat_units: 0,
            combat_budget: 0,
            reserved_budget: 20_500,
            priority_rank: 4,
            priority_enemy_types: Vec::new(),
        }];
        let facilities = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Port),
            (GridPosition { x: 1, y: 0 }, Terrain::Airport),
        ];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &facilities,
            &available_types,
            &master_data,
            20_500,
        );
        let produced: std::collections::HashSet<_> = outcome
            .commands
            .iter()
            .map(|command| command.unit_type)
            .collect();

        assert_eq!(
            produced,
            std::collections::HashSet::from([UnitType::Lander, UnitType::TransportHelicopter,])
        );
        assert!(outcome.completed_all_rows);
    }

    #[test]
    fn campaign_assault_defers_combat_support_after_building_the_first_landing_wave() {
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let rows = vec![crate::ai::island_campaign::IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: crate::ai::island_campaign::IslandCampaignDecision::Assault,
            target_position: GridPosition { x: 0, y: 0 },
            light_transport_slots: 4,
            heavy_transport_slots: 0,
            capture_units: 2,
            ground_combat_units: 0,
            combat_budget: 10_200,
            reserved_budget: 20_200,
            priority_rank: 2,
            priority_enemy_types: Vec::new(),
        }];
        let facilities = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Airport),
            (GridPosition { x: 1, y: 0 }, Terrain::Airport),
            (GridPosition { x: 2, y: 0 }, Terrain::Factory),
            (GridPosition { x: 3, y: 0 }, Terrain::Factory),
            (GridPosition { x: 4, y: 0 }, Terrain::Factory),
        ];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &facilities,
            &available_types,
            &master_data,
            20_200,
        );

        assert_eq!(outcome.commands.len(), 4);
        assert_eq!(
            outcome
                .commands
                .iter()
                .filter(|command| command.unit_type == UnitType::TransportHelicopter)
                .count(),
            2
        );
        assert_eq!(
            outcome
                .commands
                .iter()
                .filter(|command| command.unit_type == UnitType::Infantry)
                .count(),
            2
        );
        assert_eq!(outcome.remaining_funds, 10_200);
        assert!(!outcome.completed_all_rows);
    }

    #[test]
    fn campaign_ground_wave_selects_the_unit_with_more_damage_against_observed_enemy() {
        let master_data = MasterDataRegistry::load().unwrap();
        let stats = |unit_type: UnitType| {
            master_data
                .create_unit_stats(&crate::resources::master_data::UnitName(
                    unit_type.as_str().to_owned(),
                ))
                .unwrap()
        };
        let infantry = stats(UnitType::Infantry);
        let tank = stats(UnitType::Tank);
        let tank_cost = tank.cost;
        let rows = vec![IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: IslandCampaignDecision::Assault,
            target_position: GridPosition { x: 4, y: 0 },
            light_transport_slots: 0,
            heavy_transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 1,
            combat_budget: tank_cost,
            reserved_budget: tank_cost,
            priority_rank: 0,
            priority_enemy_types: vec![UnitType::Infantry],
        }];
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Infantry, 20);
        damage_chart.insert_damage(UnitType::Tank, UnitType::Infantry, 100);
        let map = Map::new(5, 1, Terrain::Plains, GridTopology::Square);

        let outcome = plan_campaign_shortfall_production_with_damage(
            PlayerId(1),
            &rows,
            &[(GridPosition { x: 0, y: 0 }, Terrain::Factory)],
            &[(UnitType::Infantry, infantry), (UnitType::Tank, tank)],
            &map,
            &master_data,
            tank_cost,
            Some(&damage_chart),
        );

        assert_eq!(outcome.commands.len(), 1);
        assert_eq!(outcome.commands[0].unit_type, UnitType::Tank);
        assert!(outcome.completed_all_rows);
    }

    /// Lander資金が足りない初手でも同じパッケージの輸送ヘリを1件だけ先行し、
    /// 残金を次ターンのLander購入へ温存する。
    #[test]
    fn campaign_assault_produces_one_affordable_prerequisite_while_saving_for_lander() {
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let rows = vec![crate::ai::island_campaign::IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: crate::ai::island_campaign::IslandCampaignDecision::Assault,
            target_position: GridPosition { x: 0, y: 0 },
            light_transport_slots: 2,
            heavy_transport_slots: 2,
            capture_units: 2,
            ground_combat_units: 0,
            combat_budget: 10_200,
            reserved_budget: 32_700,
            priority_rank: 2,
            priority_enemy_types: Vec::new(),
        }];
        let facilities = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Port),
            (GridPosition { x: 1, y: 0 }, Terrain::Airport),
            (GridPosition { x: 2, y: 0 }, Terrain::Factory),
        ];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &facilities,
            &available_types,
            &master_data,
            14_000,
        );

        assert_eq!(outcome.commands.len(), 1);
        assert_eq!(outcome.commands[0].unit_type, UnitType::TransportHelicopter);
        assert_eq!(outcome.remaining_funds, 10_000);
        assert!(!outcome.completed_all_rows);
    }

    #[test]
    fn issue75_air_counter_efficiency_decays_with_remaining_shortage() {
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 100);
        let anti_air = UnitStats {
            unit_type: UnitType::AntiAir,
            movement_type: MovementType::Tank,
            cost: 5_500,
            max_movement: 5,
            max_range: 1,
            ..UnitStats::mock()
        };
        let bomber = UnitStats {
            unit_type: UnitType::Bomber,
            movement_type: MovementType::Air,
            cost: 22_000,
            max_movement: 7,
            max_range: 1,
            ..UnitStats::mock()
        };
        let enemy_units = vec![(GridPosition { x: 0, y: 0 }, bomber)];
        let components = counter_efficiency_components(&anti_air, &enemy_units, &chart);

        assert!(components.score_with_air_shortage(1.0) > 0);
        assert_eq!(components.score_with_air_shortage(0.0), 0);
        assert!(components.score_with_air_shortage(0.5) < components.score_with_air_shortage(1.0));
    }

    #[test]
    fn issue75_fully_covered_air_does_not_dilute_ground_counter_score() {
        let components = CounterEfficiencyComponents {
            non_air_net: 10_000,
            air_net: 90_000,
            non_air_count: 1,
            air_count: 9,
        };

        assert_eq!(components.score_with_air_shortage(0.0), 10_000);
    }

    /// #53/#55 (V3): 対編成カウンター効率スコアの検証。
    /// ロケラン主体の敵編成に対して、それをアウトレンジできる重自走砲が
    /// ロケラン同型や歩兵より高評価になること
    #[test]
    fn test_counter_efficiency_vs_rocket_army() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let registry = world.get_resource::<UnitRegistry>().unwrap().clone();
        let chart = world.get_resource::<DamageChart>().unwrap().clone();

        // 敵編成: ロケットランチャー主体 (V2 の典型的なスパム構成)
        let rockets_stats = registry.0.get(&UnitType::Rockets).unwrap().clone();
        let enemy_army: Vec<(GridPosition, UnitStats)> = (0..10)
            .map(|i| (GridPosition { x: i, y: 0 }, rockets_stats.clone()))
            .collect();

        let heavy_sp_gun = registry.0.get(&UnitType::HeavySpGun).unwrap();
        let infantry = registry.0.get(&UnitType::Infantry).unwrap();

        let sp_gun_score = counter_efficiency_score(heavy_sp_gun, &enemy_army, &chart);
        let rockets_score = counter_efficiency_score(&rockets_stats, &enemy_army, &chart);
        let infantry_score = counter_efficiency_score(infantry, &enemy_army, &chart);

        // 重自走砲 (射程3-5) はロケラン (射程2-3) をアウトレンジして一方的に叩ける
        assert!(
            sp_gun_score > 0,
            "重自走砲はロケラン軍への正の交換価値を持つはず (actual: {})",
            sp_gun_score
        );
        assert!(
            sp_gun_score > rockets_score,
            "重自走砲はロケラン同型生産より高評価のはず (sp_gun: {}, rockets: {})",
            sp_gun_score,
            rockets_score
        );
        assert!(
            sp_gun_score > infantry_score,
            "重自走砲は歩兵より高評価のはず (sp_gun: {}, infantry: {})",
            sp_gun_score,
            infantry_score
        );
        // 歩兵はロケランに一方的に虐殺される (87ダメージ) ため負の交換価値
        assert!(
            infantry_score < 0,
            "歩兵はロケラン軍に対して負の交換価値のはず (actual: {})",
            infantry_score
        );
    }

    #[test]
    fn test_ai_production_saving_for_mdtank() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let p1 = PlayerId(1);
        let mut plan = ProductionPlan::default();
        plan.reserves.insert(p1.0, 16000); // MdTank目標
        world.insert_resource(plan);

        if let Some(mut players) = world.get_resource_mut::<Players>() {
            for p in &mut players.0 {
                if p.id == p1 {
                    p.funds = 10000; // MdTank(16000G)やMissiles(12000G)に足りない金額
                }
            }
        }

        // ユニット統計情報を取得
        let unit_registry = world.get_resource::<UnitRegistry>().unwrap().clone();

        // 状況設定: 敵が遠くにいて、強力なユニットが欲しい状態
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }
        // 施設をセットアップ
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 100),
        ));

        // 自軍ユニットを数体配置（ユニット数が少ないと貯金より生産を優先するため）        // 10体の歩兵を配置して、my_units.len() < 5 の緊急戦力拡張発動を確実に防ぐ
        for i in 0..10 {
            world.spawn((
                GridPosition {
                    x: i % 5,
                    y: i / 5 + 1,
                },
                Faction(p1),
                UnitStats {
                    unit_type: UnitType::Infantry,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ));
        }

        // 敵の「中戦車(MdTank)」を配置（十分に遠ざけてDefenseフェーズを避ける）
        world.spawn((
            GridPosition { x: 14, y: 14 },
            Faction(PlayerId(2)),
            UnitStats {
                unit_type: UnitType::MdTank,
                cost: 16000,
                max_movement: 5,
                movement_type: MovementType::Tank,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        // 実行（上で追加した ProductionPlan を活かすため、ここでリセットしない）
        let commands = decide_production(&mut world, p1);

        let plan = world.get_resource::<ProductionPlan>().unwrap();
        let reserve = *plan.reserves.get(&p1.0).unwrap_or(&0);

        // 10000Gでは買えないユニット（MissilesやMdTankなど）を目標に貯金しているはず
        assert!(
            reserve >= 12000,
            "Reserve should be at least 12000. Got: {}",
            reserve
        );
        // 資金(12000) < 貯金目標(16000) なので、高価な純戦闘ユニット（戦車等）は控えるはず
        for cmd in &commands {
            let stats = unit_registry.get_stats(cmd.unit_type).unwrap();
            assert!(
                stats.cost <= 3000 || stats.max_cargo > 0,
                "Should only produce cheap units (<= 3000) or transport units while saving. Got: {:?}",
                cmd.unit_type
            );
        }
    }

    #[test]
    fn test_ai_production_forward_eta() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let p1 = PlayerId(1);

        // 1. 全ユニットをクリア
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }

        // 2. 工場と首都を設置
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        let factory_pos = GridPosition { x: 1, y: 0 };
        world.spawn((factory_pos, Property::new(Terrain::Factory, Some(p1), 100)));

        // 3. 遠くに敵拠点を設置（距離感を作る）
        let enemy_pos = GridPosition { x: 15, y: 0 };
        world.spawn((
            enemy_pos,
            Property::new(Terrain::City, Some(PlayerId(2)), 100),
        ));

        // 敵ユニットも設置
        let enemy_stats = UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1000,
            max_movement: 3,
            movement_type: MovementType::Tank,
            ..UnitStats::mock()
        };
        world.spawn((
            enemy_pos,
            Faction(PlayerId(2)),
            enemy_stats.clone(),
            Health {
                current: 100,
                max: 100,
            },
        ));

        let registry = world.get_resource::<UnitRegistry>().unwrap().clone();
        let chart = world.get_resource::<DamageChart>().unwrap().clone();
        let map = world.get_resource::<Map>().unwrap().clone();

        // テスト用の低速タンク（speed 3）
        let tank_stats = UnitStats {
            unit_type: UnitType::Tank,
            max_movement: 3,
            movement_type: MovementType::Tank,
            ..UnitStats::mock()
        };

        let enemy_units = vec![(enemy_pos, enemy_stats)];

        // シナリオA: 輸送車なしでタンクのスコアを計測
        let score_without_transport;
        {
            let strategy = strategy::analyze_strategy(&mut world, p1);
            score_without_transport = calculate_unit_score_at(
                UnitType::Tank,
                &tank_stats,
                factory_pos,
                p1,
                &strategy,
                &enemy_units,
                &[],
                &chart,
                &master_data,
                &map,
                &std::collections::HashMap::new(),
                &registry,
                Terrain::Factory,
                0.0,
                false,
            );
        }

        // シナリオB: 工場に空の輸送車(輸送ヘリ)を設置してスコアを再計算
        let score_with_transport;
        {
            // 高速な輸送車（speed 9）
            let t_stats = UnitStats {
                unit_type: UnitType::TransportHelicopter,
                max_movement: 9,
                movement_type: MovementType::Air,
                max_cargo: 1,
                loadable_unit_types: vec![UnitType::Infantry, UnitType::Tank],
                ..UnitStats::mock()
            };
            let empty_transports = vec![(factory_pos, t_stats)];

            let strategy = strategy::analyze_strategy(&mut world, p1);
            score_with_transport = calculate_unit_score_at(
                UnitType::Tank,
                &tank_stats,
                factory_pos,
                p1,
                &strategy,
                &enemy_units,
                &empty_transports,
                &chart,
                &master_data,
                &map,
                &std::collections::HashMap::new(),
                &registry,
                Terrain::Factory,
                0.0,
                false,
            );
        }

        // 検証: 輸送車がある方がETAが短縮され、スコアが高くなるはず
        assert!(
            score_with_transport > score_without_transport,
            "Score with transport ({}) should be higher than without ({}) due to Forward ETA",
            score_with_transport,
            score_without_transport
        );
    }

    #[test]
    fn test_ai_production_counter_selection() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let p1 = PlayerId(1);
        if let Some(mut players) = world.get_resource_mut::<Players>() {
            for p in &mut players.0 {
                if p.id == p1 {
                    p.funds = 25000; // 十分な資金
                }
            }
        }

        // 状況設定: 敵が「戦闘ヘリ(Bcopters)」を大量に出している
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }
        world.insert_resource(Map::new(6, 1, Terrain::Plains, GridTopology::Square));
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 100),
        ));
        let infantry = world
            .resource::<UnitRegistry>()
            .get_stats(UnitType::Infantry)
            .unwrap()
            .clone();
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Faction(p1),
            infantry,
            Health {
                current: 100,
                max: 100,
            },
        ));

        // 敵のヘリ
        for i in 0..2 {
            world.spawn((
                GridPosition { x: 3 + i, y: 0 },
                Faction(PlayerId(2)),
                UnitStats {
                    unit_type: UnitType::Bcopters,
                    cost: 9000,
                    max_movement: 6,
                    movement_type: MovementType::Air,
                    max_fuel: 99,
                    min_range: 1,
                    max_range: 1,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ));
        }

        // 実行
        world.insert_resource(ProductionPlan::default());
        let commands = decide_production(&mut world, p1);

        let produced_types: Vec<UnitType> = commands.iter().map(|c| c.unit_type).collect();

        // ヘリへのカウンターである「対空戦車(AntiAir)」または「地対空ミサイル(Missiles)」が選ばれるべき
        assert!(
            produced_types.contains(&UnitType::AntiAir)
                || produced_types.contains(&UnitType::Missiles),
            "Should produce anti-air units against helicopters. Got: {:?}",
            produced_types
        );
    }

    #[test]
    fn test_ai_production_infantry_priority_at_start() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        // テスト用の15x15平地マップを作成して挿入（島IDが正しく認識されるように）
        let map = Map {
            width: 15,
            height: 15,
            tiles: vec![Terrain::Plains; 225],
            topology: crate::resources::GridTopology::Square,
        };
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        world.insert_resource(map);
        world.insert_resource(island_map);

        let p1 = PlayerId(1);
        if let Some(mut players) = world.get_resource_mut::<Players>() {
            for p in &mut players.0 {
                if p.id == p1 {
                    p.funds = 10000; // 十分な資金（ロケットランチャー等も買える額）
                }
            }
        }

        // 全エンティティをクリアして初期マップ状態をシミュレート
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }

        // 自軍の生産施設
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 100),
        ));

        // 中立拠点が島に点在
        world.spawn((
            GridPosition { x: 3, y: 0 },
            Property::new(Terrain::City, None, 100),
        ));
        world.spawn((
            GridPosition { x: 0, y: 3 },
            Property::new(Terrain::City, None, 100),
        ));

        // 敵歩兵が極めて少数（1体）のみ、遠くに存在し平和な状態
        world.spawn((
            GridPosition { x: 10, y: 10 },
            Faction(PlayerId(2)),
            UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                max_movement: 3,
                movement_type: MovementType::Infantry,
                can_capture: true,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let commands = decide_production(&mut world, p1);

        let produced_types: Vec<UnitType> = commands.iter().map(|c| c.unit_type).collect();

        // 資金が豊富であっても、中立拠点獲得を最優先して「歩兵（軽歩兵または重歩兵）」を生産するはず
        assert!(
            produced_types.contains(&UnitType::Infantry)
                || produced_types.contains(&UnitType::Mech),
            "Should prioritize producing capturing units (Infantry/Mech) at start. Got: {:?}",
            produced_types
        );

        // ロケットランチャーなどの高額戦闘ユニットは生産されていないはず
        assert!(
            !produced_types.contains(&UnitType::Rockets),
            "Should not produce Rocket Launchers when it is peaceful and capturing is needed. Got: {:?}",
            produced_types
        );
    }
}
