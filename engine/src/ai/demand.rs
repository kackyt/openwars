use crate::ai::turn_distance::{ActionTurnDistanceCache, calculate_action_distance_to_range};
use crate::components::{GridPosition, PlayerId, UnitStats};
use crate::resources::{
    DamageChart, Map, MovementType, Terrain, UnitRegistry, UnitType,
    master_data::MasterDataRegistry,
};
use crate::systems::movement::OccupantInfo;
use std::collections::HashMap;

/// ユニットの戦闘カテゴリ。
/// `MovementType` をもとに Ground / Air / Sea に分類します。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitCategory {
    Ground,
    Air,
    Sea,
}

impl UnitCategory {
    pub fn from_movement_type(mt: MovementType) -> Self {
        match mt {
            MovementType::Air => Self::Air,
            MovementType::Ship => Self::Sea,
            _ => Self::Ground,
        }
    }
}

/// ユニットタイプの各カテゴリへの「攻撃適性」（0.0〜1.0）。
/// `DamageChart` から自動算出されます。
#[derive(Debug, Clone, Default)]
pub struct UnitAffinity {
    /// 地上ユニットへの平均攻撃適性
    pub anti_ground: f32,
    /// 航空ユニットへの平均攻撃適性
    pub anti_air: f32,
    /// 海上ユニットへの平均攻撃適性
    pub anti_sea: f32,
}

/// 自軍が直面している「需要の欠け」（0.0〜1.0 に正規化）。
/// 値が 1.0 に近いほど、そのカテゴリへの対応が緊急であることを示します。
#[derive(Debug, Clone, Default)]
pub struct DemandMatrix {
    /// 敵地上部隊に対する反撃能力の不足度
    pub anti_ground: f32,
    /// 敵航空部隊に対する反撃能力の不足度（対空需要）
    pub anti_air: f32,
    /// 敵海上部隊に対する反撃能力の不足度
    pub anti_sea: f32,
    /// 占領可能ユニットの不足度（致死的脅威ベース）
    pub capture: f32,
    /// 輸送力の不足度
    pub logistics: f32,
}

impl DemandMatrix {
    /// 需要マトリクスと適性のドット積を計算します（戦闘カテゴリのみ）。
    /// 結果は [0.0, 3.0] の範囲となります。
    pub fn dot(&self, affinity: &UnitAffinity) -> f32 {
        affinity.anti_ground * self.anti_ground
            + affinity.anti_air * self.anti_air
            + affinity.anti_sea * self.anti_sea
    }
}

/// 対空カバレッジ計算に必要な軽量な戦闘能力スナップショット。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CombatCapabilitySnapshot {
    pub faction: PlayerId,
    pub position: GridPosition,
    pub unit_type: UnitType,
    pub movement_type: MovementType,
    pub hp: u32,
    pub cost: u32,
    pub max_movement: u32,
    pub min_range: u32,
    pub max_range: u32,
    pub ammo1: u32,
    pub max_ammo1: u32,
    pub ammo2: u32,
    pub max_ammo2: u32,
    pub fuel: u32,
    /// 現在の行動完了状態によって次に射撃できるまで待つ自軍ターン数。
    pub action_delay: u32,
}

/// 生産候補が対応すべき個別の航空脅威。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AirThreatTarget {
    /// 観測時点の航空機位置。既存戦力が現在の脅威へ到達できるかの判定に使用する。
    pub position: GridPosition,
    pub unit_type: UnitType,
    pub hp: u32,
    pub cost: u32,
    /// 現在の弾薬で発揮できる最大攻撃力。脅威価値の重みとして使用する。
    pub attack_power: u32,
    pub deadline_turns: u32,
}

/// 現在の航空脅威と有効対空カバレッジの集約結果。
#[derive(Debug, Clone, Default)]
pub struct AirDefenseAssessment {
    pub targets: Vec<AirThreatTarget>,
    /// 各航空脅威へ割り当て済みのカバレッジ。`targets` と同じ順序で保持する。
    pub coverage_by_target: Vec<f32>,
    pub required_coverage: f32,
    pub current_coverage: f32,
    pub shortage_ratio: f32,
    pub has_effective_coverage: bool,
}

/// 生産候補1体が各航空脅威へ追加できるカバレッジ。
#[derive(Debug, Clone, Default)]
pub struct AirCoverageContribution {
    pub by_target: Vec<f32>,
    pub total: f32,
}

impl AirDefenseAssessment {
    pub(crate) const COVERAGE_EPSILON: f32 = 0.001;
    const EMERGENCY_COVERAGE_FLOOR: f32 = 0.5;
    const EMERGENCY_MAX_DEADLINE_TURNS: u32 = 2;

    fn refresh_coverage_summary(&mut self) {
        self.current_coverage = self.coverage_by_target.iter().sum();
        self.shortage_ratio = if self.required_coverage > 0.0 {
            ((self.required_coverage - self.current_coverage) / self.required_coverage)
                .clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.has_effective_coverage = self.current_coverage > 0.0;
    }

    /// 対象ごとの期限内カバレッジ率を返します。
    pub(crate) fn target_coverage_ratio(&self, index: usize) -> f32 {
        let Some(target) = self.targets.get(index) else {
            return 1.0;
        };
        let target_value = target_threat_value(target);
        if target_value <= Self::COVERAGE_EPSILON {
            return 1.0;
        }
        (self.coverage_by_target.get(index).copied().unwrap_or(0.0) / target_value).clamp(0.0, 1.0)
    }

    pub(crate) fn target_threat_value_at(&self, index: usize) -> f32 {
        self.targets
            .get(index)
            .map(target_threat_value)
            .unwrap_or(0.0)
    }

    pub(crate) fn remaining_threat_value(&self, index: usize) -> f32 {
        self.target_threat_value_at(index) * (1.0 - self.target_coverage_ratio(index))
    }

    /// 既存カバレッジでまだ抑止できていない敵航空機のHP補正済み資産価値を返します。
    /// ETA緊急度は含めず、生産費が敵機価格の何倍にも膨らむことを防ぎます。
    pub(crate) fn remaining_air_asset_value(&self, index: usize) -> f32 {
        let Some(target) = self.targets.get(index) else {
            return 0.0;
        };
        target.cost as f32 * target.hp as f32 / 100.0 * (1.0 - self.target_coverage_ratio(index))
    }

    pub(crate) fn is_emergency_target(&self, index: usize) -> bool {
        self.targets.get(index).is_some_and(|target| {
            let coverage = self.coverage_by_target.get(index).copied().unwrap_or(0.0);
            target_threat_value(target) > Self::COVERAGE_EPSILON
                && (coverage <= Self::COVERAGE_EPSILON
                    || (target.deadline_turns <= Self::EMERGENCY_MAX_DEADLINE_TURNS
                        && self.target_coverage_ratio(index) < Self::EMERGENCY_COVERAGE_FLOOR))
        })
    }

    pub fn requires_emergency_production(&self) -> bool {
        (0..self.targets.len()).any(|index| self.is_emergency_target(index))
    }

    /// 緊急条件を満たす航空機だけを候補評価へ残します。
    pub(crate) fn emergency_targets_only(&self) -> Self {
        let mut focused = self.clone();
        if focused.coverage_by_target.len() != focused.targets.len() {
            focused.coverage_by_target = vec![0.0; focused.targets.len()];
        }
        for (index, target) in focused.targets.iter().enumerate() {
            if !self.is_emergency_target(index) {
                focused.coverage_by_target[index] = target_threat_value(target);
            }
        }
        focused.refresh_coverage_summary();
        focused.has_effective_coverage = focused.current_coverage > Self::COVERAGE_EPSILON;
        focused
    }

    /// 緊急対象だけを未割当へ戻し、既存戦力から封じ込め投資を再構築できる状態にします。
    pub(crate) fn uncovered_emergency_targets_only(&self) -> Self {
        let mut focused = self.emergency_targets_only();
        for index in 0..focused.targets.len() {
            if self.is_emergency_target(index) {
                focused.coverage_by_target[index] = 0.0;
            }
        }
        focused.refresh_coverage_summary();
        focused.has_effective_coverage = focused.current_coverage > Self::COVERAGE_EPSILON;
        focused
    }

    pub fn apply_coverage(&mut self, contribution: &AirCoverageContribution) {
        if self.coverage_by_target.len() != self.targets.len() {
            self.coverage_by_target = vec![0.0; self.targets.len()];
        }
        for (index, added) in contribution.by_target.iter().copied().enumerate() {
            let Some(target) = self.targets.get(index) else {
                break;
            };
            let target_value = target_threat_value(target);
            self.coverage_by_target[index] =
                (self.coverage_by_target[index] + added).min(target_value);
        }
        self.refresh_coverage_summary();
    }
}

/// `DamageChart` を走査し、全ユニット×全カテゴリの平均攻撃期待値を算出します。
/// これを正規化スケールとして使用することで、ユニット追加・変更に自動対応します。
pub fn average_attack_expectation(damage_chart: &DamageChart, unit_registry: &UnitRegistry) -> f32 {
    let mut total = 0.0f32;
    let mut count = 0u32;

    for attacker_type in unit_registry.0.keys() {
        for defender_type in unit_registry.0.keys() {
            // 主武器
            if let Some(dmg) = damage_chart.get_base_damage(*attacker_type, *defender_type) {
                // 攻撃力を持たない組み合わせは除外（分母に入れない）
                if dmg > 0 {
                    total += dmg as f32;
                    count += 1;
                }
            }
        }
    }

    if count == 0 {
        100.0 // フォールバック
    } else {
        (total / count as f32).max(1.0)
    }
}

/// `DamageChart` からユニットタイプの各カテゴリへの攻撃適性を自動算出します。
/// 適性は 0.0〜1.0 に正規化されます。
pub fn compute_unit_affinity(
    unit_type: UnitType,
    damage_chart: &DamageChart,
    unit_registry: &UnitRegistry,
    normalization_scale: f32,
) -> UnitAffinity {
    let mut ground_sum = 0.0f32;
    let mut ground_count = 0u32;
    let mut air_sum = 0.0f32;
    let mut air_count = 0u32;
    let mut sea_sum = 0.0f32;
    let mut sea_count = 0u32;

    for (defender_type, defender_stats) in &unit_registry.0 {
        let category = UnitCategory::from_movement_type(defender_stats.movement_type);

        // 主武器ダメージ
        let primary = damage_chart
            .get_base_damage(unit_type, *defender_type)
            .unwrap_or(0) as f32;
        // 副武器ダメージ
        let secondary = damage_chart
            .get_base_damage_secondary(unit_type, *defender_type)
            .unwrap_or(0) as f32;
        // 高いほうを採用
        let dmg = primary.max(secondary);

        match category {
            UnitCategory::Ground => {
                ground_sum += dmg;
                ground_count += 1;
            }
            UnitCategory::Air => {
                air_sum += dmg;
                air_count += 1;
            }
            UnitCategory::Sea => {
                sea_sum += dmg;
                sea_count += 1;
            }
        }
    }

    let normalize = |sum: f32, count: u32| -> f32 {
        if count == 0 {
            0.0
        } else {
            ((sum / count as f32) / normalization_scale).clamp(0.0, 1.0)
        }
    };

    UnitAffinity {
        anti_ground: normalize(ground_sum, ground_count),
        anti_air: normalize(air_sum, air_count),
        anti_sea: normalize(sea_sum, sea_count),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WeaponSlot {
    Primary,
    Secondary,
}

/// 残弾がある主・副武器から対象へ最も高い基礎ダメージを与える武器を選ぶ。
/// 弾数上限が0の武器は弾薬を消費しない武器として常に使用可能とみなす。
fn available_weapon_against(
    unit: &CombatCapabilitySnapshot,
    defender: UnitType,
    damage_chart: &DamageChart,
) -> Option<(u32, WeaponSlot)> {
    let primary = if unit.max_ammo1 == 0 || unit.ammo1 > 0 {
        damage_chart
            .get_base_damage(unit.unit_type, defender)
            .unwrap_or(0)
    } else {
        0
    };
    let secondary = if unit.max_ammo2 == 0 || unit.ammo2 > 0 {
        damage_chart
            .get_base_damage_secondary(unit.unit_type, defender)
            .unwrap_or(0)
    } else {
        0
    };
    match (primary, secondary) {
        (0, 0) => None,
        (left, right) if right > left => Some((right, WeaponSlot::Secondary)),
        (damage, _) => Some((damage, WeaponSlot::Primary)),
    }
}

fn consume_weapon_ammo(unit: &mut CombatCapabilitySnapshot, weapon: WeaponSlot) {
    match weapon {
        WeaponSlot::Primary if unit.max_ammo1 > 0 => {
            unit.ammo1 = unit.ammo1.saturating_sub(1);
        }
        WeaponSlot::Secondary if unit.max_ammo2 > 0 => {
            unit.ammo2 = unit.ammo2.saturating_sub(1);
        }
        _ => {}
    }
}

fn target_threat_value(target: &AirThreatTarget) -> f32 {
    let base = target.cost as f32 * target.hp as f32 / 100.0 * target.attack_power as f32 / 100.0;
    // 到達が近い航空脅威ほど生産猶予が短いため、1.0〜2.0倍の範囲で緊急度を加える。
    let urgency = 1.0 + 1.0 / target.deadline_turns.max(1) as f32;
    base * urgency
}

fn maximum_available_attack_power(
    unit: &CombatCapabilitySnapshot,
    friendly_units: &[CombatCapabilitySnapshot],
    player_id: PlayerId,
    damage_chart: &DamageChart,
) -> u32 {
    friendly_units
        .iter()
        .filter(|friendly| friendly.faction == player_id)
        .filter_map(|friendly| available_weapon_against(unit, friendly.unit_type, damage_chart))
        .map(|(damage, _)| damage)
        .max()
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
struct AirCoverageUnitState {
    unit: CombatCapabilitySnapshot,
    ready_turns: Vec<Option<u32>>,
    requires_movement: Vec<bool>,
    locked_target: Option<usize>,
    has_fired: bool,
}

fn available_air_target(
    state: &AirCoverageUnitState,
    target_index: usize,
    targets: &[AirThreatTarget],
    coverage_by_target: &[f32],
    damage_chart: &DamageChart,
) -> Option<(u32, WeaponSlot)> {
    let target = &targets[target_index];
    if state
        .locked_target
        .is_some_and(|locked| locked != target_index)
        || (state.locked_target.is_none()
            && state.has_fired
            && state.requires_movement[target_index])
        || state.ready_turns[target_index].is_none_or(|ready| ready > target.deadline_turns)
        || coverage_by_target[target_index] >= target_threat_value(target)
    {
        return None;
    }
    available_weapon_against(&state.unit, target.unit_type, damage_chart)
}

fn feasible_air_target(
    state: &AirCoverageUnitState,
    target_index: usize,
    action_turn: u32,
    targets: &[AirThreatTarget],
    coverage_by_target: &[f32],
    damage_chart: &DamageChart,
) -> Option<(u32, WeaponSlot)> {
    let ready = state.ready_turns[target_index]?;
    if ready > action_turn || action_turn > targets[target_index].deadline_turns {
        return None;
    }
    available_air_target(
        state,
        target_index,
        targets,
        coverage_by_target,
        damage_chart,
    )
}

fn project_remaining_actions(
    mut states: Vec<AirCoverageUnitState>,
    current_available_units: &[usize],
    start_action_turn: u32,
    targets: &[AirThreatTarget],
    initial_coverage: &[f32],
    damage_chart: &DamageChart,
) -> f32 {
    let mut projected_coverage = initial_coverage.to_vec();
    let mut added = 0.0;
    // 到達期限のターン中の射撃は有効だが、期限を過ぎた射撃はカバレッジへ含めない。
    let last_action_turn = targets
        .iter()
        .map(|target| target.deadline_turns)
        .max()
        .unwrap_or(0);
    for action_turn in start_action_turn..=last_action_turn {
        let mut available_units = if action_turn == start_action_turn {
            current_available_units.to_vec()
        } else {
            (0..states.len()).collect::<Vec<_>>()
        };
        while !available_units.is_empty() {
            let mut best_edge = None;
            for (unit_position, state_index) in available_units.iter().copied().enumerate() {
                for target_index in 0..targets.len() {
                    let Some((damage, weapon)) = feasible_air_target(
                        &states[state_index],
                        target_index,
                        action_turn,
                        targets,
                        &projected_coverage,
                        damage_chart,
                    ) else {
                        continue;
                    };
                    let target_value = target_threat_value(&targets[target_index]);
                    let raw = target_value * damage as f32 / 100.0
                        * states[state_index].unit.hp as f32
                        / 100.0;
                    let marginal =
                        raw.min((target_value - projected_coverage[target_index]).max(0.0));
                    if marginal <= 0.0 {
                        continue;
                    }
                    let candidate = (unit_position, target_index, marginal, state_index, weapon);
                    let replace = best_edge.as_ref().is_none_or(
                        |current: &(usize, usize, f32, usize, WeaponSlot)| {
                            candidate
                                .2
                                .total_cmp(&current.2)
                                .then_with(|| current.1.cmp(&candidate.1))
                                .then_with(|| current.3.cmp(&candidate.3))
                                .is_gt()
                        },
                    );
                    if replace {
                        best_edge = Some(candidate);
                    }
                }
            }
            let Some((unit_position, target_index, marginal, state_index, weapon)) = best_edge
            else {
                break;
            };
            projected_coverage[target_index] += marginal;
            added += marginal;
            if states[state_index].requires_movement[target_index] {
                states[state_index].locked_target = Some(target_index);
            }
            states[state_index].has_fired = true;
            consume_weapon_ammo(&mut states[state_index].unit, weapon);
            available_units.remove(unit_position);
        }
    }
    added
}

#[allow(clippy::too_many_arguments)]
fn allocate_air_coverage(
    units: &[CombatCapabilitySnapshot],
    targets: &[AirThreatTarget],
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    damage_chart: &DamageChart,
    coverage_by_target: &mut [f32],
) {
    let mut distance_cache = ActionTurnDistanceCache::default();
    allocate_air_coverage_cached(
        units,
        targets,
        map,
        registry,
        unit_positions,
        damage_chart,
        coverage_by_target,
        &mut distance_cache,
    );
}

#[allow(clippy::too_many_arguments)]
fn allocate_air_coverage_cached(
    units: &[CombatCapabilitySnapshot],
    targets: &[AirThreatTarget],
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    damage_chart: &DamageChart,
    coverage_by_target: &mut [f32],
    distance_cache: &mut ActionTurnDistanceCache,
) {
    let mut states = units
        .iter()
        .copied()
        .map(|unit| {
            let arrivals = targets
                .iter()
                .map(|target| {
                    calculate_action_distance_to_range(
                        map,
                        registry,
                        unit_positions,
                        (unit.position.x, unit.position.y),
                        (target.position.x, target.position.y),
                        unit.movement_type,
                        unit.max_movement,
                        unit.fuel,
                        unit.min_range,
                        unit.max_range.max(unit.min_range),
                        unit.faction,
                        distance_cache,
                    )
                })
                .collect::<Vec<_>>();
            AirCoverageUnitState {
                unit,
                ready_turns: arrivals
                    .iter()
                    .map(|arrival| {
                        arrival
                            .map(|distance| distance.turns.max(1).saturating_add(unit.action_delay))
                    })
                    .collect(),
                requires_movement: arrivals
                    .iter()
                    .map(|arrival| arrival.is_some_and(|distance| distance.requires_movement))
                    .collect(),
                locked_target: None,
                has_fired: false,
            }
        })
        .collect::<Vec<_>>();
    // 到達期限のターン中の射撃は有効だが、期限を過ぎた射撃はカバレッジへ含めない。
    let last_action_turn = targets
        .iter()
        .map(|target| target.deadline_turns)
        .max()
        .unwrap_or(0);

    for action_turn in 1..=last_action_turn {
        let mut available_units = (0..states.len()).collect::<Vec<_>>();
        while !available_units.is_empty() {
            // 各ターンも制約の強いユニットから射撃させ、挿入順に依存しない割当を行う。
            let selected_position = available_units
                .iter()
                .enumerate()
                .filter_map(|(position, state_index)| {
                    let state = &states[*state_index];
                    let feasible_count = (0..targets.len())
                        .filter(|target_index| {
                            feasible_air_target(
                                state,
                                *target_index,
                                action_turn,
                                targets,
                                coverage_by_target,
                                damage_chart,
                            )
                            .is_some()
                        })
                        .count();
                    (feasible_count > 0).then_some((position, feasible_count, state_index))
                })
                .min_by(|left, right| {
                    let left_state = &states[*left.2];
                    let right_state = &states[*right.2];
                    left.1
                        .cmp(&right.1)
                        .then_with(|| left_state.unit.position.y.cmp(&right_state.unit.position.y))
                        .then_with(|| left_state.unit.position.x.cmp(&right_state.unit.position.x))
                        .then_with(|| {
                            left_state
                                .unit
                                .unit_type
                                .as_str()
                                .cmp(right_state.unit.unit_type.as_str())
                        })
                })
                .map(|(position, _, _)| position);
            let Some(selected_position) = selected_position else {
                break;
            };
            let state_index = available_units.remove(selected_position);
            let selected_target = (0..targets.len())
                .filter_map(|target_index| {
                    let weapon = feasible_air_target(
                        &states[state_index],
                        target_index,
                        action_turn,
                        targets,
                        coverage_by_target,
                        damage_chart,
                    )?;
                    let target_value = target_threat_value(&targets[target_index]);
                    let raw_contribution = target_value * weapon.0 as f32 / 100.0
                        * states[state_index].unit.hp as f32
                        / 100.0;
                    let marginal_contribution = raw_contribution
                        .min((target_value - coverage_by_target[target_index]).max(0.0));
                    let mut coverage_after_shot = coverage_by_target.to_vec();
                    coverage_after_shot[target_index] += marginal_contribution;
                    let mut projected_states = states.clone();
                    if projected_states[state_index].requires_movement[target_index] {
                        projected_states[state_index].locked_target = Some(target_index);
                    }
                    projected_states[state_index].has_fired = true;
                    consume_weapon_ammo(&mut projected_states[state_index].unit, weapon.1);
                    let projected_future = project_remaining_actions(
                        projected_states,
                        &available_units,
                        action_turn,
                        targets,
                        &coverage_after_shot,
                        damage_chart,
                    );
                    Some((
                        target_index,
                        weapon,
                        marginal_contribution,
                        marginal_contribution + projected_future,
                    ))
                })
                .min_by(|left, right| {
                    right
                        .3
                        .total_cmp(&left.3)
                        .then_with(|| {
                            targets[left.0]
                                .deadline_turns
                                .cmp(&targets[right.0].deadline_turns)
                        })
                        .then_with(|| right.2.total_cmp(&left.2))
                        .then_with(|| left.0.cmp(&right.0))
                });
            let Some((target_index, (damage, weapon), _, _)) = selected_target else {
                continue;
            };
            let target_value = target_threat_value(&targets[target_index]);
            let contribution =
                target_value * damage as f32 / 100.0 * states[state_index].unit.hp as f32 / 100.0;
            coverage_by_target[target_index] =
                (coverage_by_target[target_index] + contribution).min(target_value);
            if states[state_index].requires_movement[target_index] {
                states[state_index].locked_target = Some(target_index);
            }
            states[state_index].has_fired = true;
            consume_weapon_ammo(&mut states[state_index].unit, weapon);
        }
    }
}

/// 現在の盤面から航空脅威と実際に間に合う対空カバレッジを算出します。
#[allow(clippy::too_many_arguments)]
pub fn assess_air_defense(
    player_id: PlayerId,
    units: &[CombatCapabilitySnapshot],
    critical_sites: &[GridPosition],
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    damage_chart: &DamageChart,
) -> AirDefenseAssessment {
    let mut targets = Vec::new();
    for enemy in units
        .iter()
        .filter(|unit| unit.faction != player_id && unit.movement_type == MovementType::Air)
    {
        let attack_power = maximum_available_attack_power(enemy, units, player_id, damage_chart);
        if attack_power == 0 {
            continue;
        }

        // 所有拠点に加えて、実際に攻撃可能な高価値自軍ユニットも防衛対象へ含める。
        // 安価な歩兵を囮にした過剰反応を避け、輸送・間接攻撃・航空・海上戦力相当を対象にする。
        const MOBILE_ASSET_MIN_COST: u32 = 5_000;
        let mut response_positions = critical_sites.to_vec();
        response_positions.extend(
            units
                .iter()
                .filter(|friendly| {
                    friendly.faction == player_id && friendly.cost >= MOBILE_ASSET_MIN_COST
                })
                .filter(|friendly| {
                    available_weapon_against(enemy, friendly.unit_type, damage_chart).is_some()
                })
                .map(|friendly| friendly.position),
        );
        response_positions.sort_by_key(|position| (position.y, position.x));
        response_positions.dedup();

        let mut response = None;
        let mut cache = ActionTurnDistanceCache::default();
        for position in response_positions {
            let Some(distance) = calculate_action_distance_to_range(
                map,
                registry,
                unit_positions,
                (enemy.position.x, enemy.position.y),
                (position.x, position.y),
                enemy.movement_type,
                enemy.max_movement,
                enemy.fuel,
                enemy.min_range,
                enemy.max_range.max(enemy.min_range),
                enemy.faction,
                &mut cache,
            ) else {
                continue;
            };
            let candidate = (distance.turns.max(1), position.y, position.x);
            if response.is_none_or(|current| candidate < current) {
                response = Some(candidate);
            }
        }
        let Some((deadline_turns, _, _)) = response else {
            continue;
        };
        targets.push(AirThreatTarget {
            position: enemy.position,
            unit_type: enemy.unit_type,
            hp: enemy.hp,
            cost: enemy.cost,
            attack_power,
            deadline_turns,
        });
    }
    targets.sort_by_key(|target| (target.deadline_turns, target.position.y, target.position.x));

    let required_coverage = targets.iter().map(target_threat_value).sum::<f32>();
    let mut coverage_by_target = vec![0.0; targets.len()];
    let friendly_units = units
        .iter()
        .copied()
        .filter(|unit| unit.faction == player_id)
        .collect::<Vec<_>>();
    allocate_air_coverage(
        &friendly_units,
        &targets,
        map,
        registry,
        unit_positions,
        damage_chart,
        &mut coverage_by_target,
    );
    let current_coverage = coverage_by_target.iter().sum::<f32>();
    let has_effective_coverage = current_coverage > 0.0;
    let shortage_ratio = if required_coverage > 0.0 {
        ((required_coverage - current_coverage) / required_coverage).clamp(0.0, 1.0)
    } else {
        0.0
    };
    AirDefenseAssessment {
        targets,
        coverage_by_target,
        required_coverage,
        current_coverage,
        shortage_ratio,
        has_effective_coverage,
    }
}

/// 生産候補1体が現在の航空脅威へ追加する有効カバレッジを算出します。
#[allow(clippy::too_many_arguments)]
pub fn candidate_air_coverage(
    stats: &UnitStats,
    production_position: GridPosition,
    player_id: PlayerId,
    assessment: &AirDefenseAssessment,
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    damage_chart: &DamageChart,
) -> AirCoverageContribution {
    let mut distance_cache = ActionTurnDistanceCache::default();
    candidate_air_coverage_with_cache(
        stats,
        production_position,
        player_id,
        assessment,
        map,
        registry,
        unit_positions,
        damage_chart,
        &mut distance_cache,
    )
}

/// 同一生産計画の候補間で対空到達距離を共有して限界カバレッジを算出する。
#[allow(clippy::too_many_arguments)]
pub(crate) fn candidate_air_coverage_with_cache(
    stats: &UnitStats,
    production_position: GridPosition,
    player_id: PlayerId,
    assessment: &AirDefenseAssessment,
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    damage_chart: &DamageChart,
    distance_cache: &mut ActionTurnDistanceCache,
) -> AirCoverageContribution {
    candidate_air_coverage_with_delay_internal(
        stats,
        production_position,
        player_id,
        assessment,
        map,
        registry,
        unit_positions,
        damage_chart,
        1,
        Some(distance_cache),
    )
}

/// 購入待ちを含む行動遅延を指定し、生産候補の限界カバレッジを算出します。
#[allow(clippy::too_many_arguments)]
pub(crate) fn candidate_air_coverage_with_delay(
    stats: &UnitStats,
    production_position: GridPosition,
    player_id: PlayerId,
    assessment: &AirDefenseAssessment,
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    damage_chart: &DamageChart,
    action_delay: u32,
) -> AirCoverageContribution {
    candidate_air_coverage_with_delay_internal(
        stats,
        production_position,
        player_id,
        assessment,
        map,
        registry,
        unit_positions,
        damage_chart,
        action_delay,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn candidate_air_coverage_with_delay_internal(
    stats: &UnitStats,
    production_position: GridPosition,
    player_id: PlayerId,
    assessment: &AirDefenseAssessment,
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    damage_chart: &DamageChart,
    action_delay: u32,
    distance_cache: Option<&mut ActionTurnDistanceCache>,
) -> AirCoverageContribution {
    let candidate = CombatCapabilitySnapshot {
        faction: player_id,
        position: production_position,
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
        action_delay,
    };
    let mut combined_coverage = if assessment.coverage_by_target.len() == assessment.targets.len() {
        assessment.coverage_by_target.clone()
    } else {
        vec![0.0; assessment.targets.len()]
    };
    let baseline = combined_coverage.clone();
    let mut local_distance_cache = ActionTurnDistanceCache::default();
    allocate_air_coverage_cached(
        &[candidate],
        &assessment.targets,
        map,
        registry,
        unit_positions,
        damage_chart,
        &mut combined_coverage,
        distance_cache.unwrap_or(&mut local_distance_cache),
    );
    let by_target = combined_coverage
        .iter()
        .zip(baseline)
        .map(|(combined, existing)| (combined - existing).max(0.0))
        .collect::<Vec<_>>();
    let total = by_target.iter().sum();
    AirCoverageContribution { by_target, total }
}

/// 複数の実戦力を一括割当し、限定ターン内の封じ込めカバレッジを元の脅威尺度で返します。
#[allow(clippy::too_many_arguments)]
pub(crate) fn air_coverage_with_timing(
    units: &[CombatCapabilitySnapshot],
    assessment: &AirDefenseAssessment,
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    damage_chart: &DamageChart,
    deadline_grace: u32,
) -> AirCoverageContribution {
    let original_ratios = (0..assessment.targets.len())
        .map(|index| assessment.target_coverage_ratio(index))
        .collect::<Vec<_>>();
    let mut relaxed = assessment.clone();
    for target in &mut relaxed.targets {
        target.deadline_turns = target.deadline_turns.saturating_add(deadline_grace);
    }
    relaxed.required_coverage = (0..relaxed.targets.len())
        .map(|index| relaxed.target_threat_value_at(index))
        .sum();
    relaxed.coverage_by_target = original_ratios
        .iter()
        .copied()
        .enumerate()
        .map(|(index, ratio)| relaxed.target_threat_value_at(index) * ratio)
        .collect();
    relaxed.apply_coverage(&AirCoverageContribution::default());

    let mut combined_coverage = relaxed.coverage_by_target.clone();
    let baseline = combined_coverage.clone();
    allocate_air_coverage(
        units,
        &relaxed.targets,
        map,
        registry,
        unit_positions,
        damage_chart,
        &mut combined_coverage,
    );
    let by_target = combined_coverage
        .iter()
        .zip(baseline)
        .enumerate()
        .map(|(index, (combined, existing))| {
            let relaxed_value = relaxed.target_threat_value_at(index);
            if relaxed_value <= AirDefenseAssessment::COVERAGE_EPSILON {
                0.0
            } else {
                let added = (combined - existing).max(0.0);
                assessment.target_threat_value_at(index) * (added / relaxed_value)
            }
        })
        .collect::<Vec<_>>();
    let total = by_target.iter().sum();
    AirCoverageContribution { by_target, total }
}

/// 緊急期限後も限定ターン内に封じ込められるかを、元の脅威尺度へ正規化して返します。
#[allow(clippy::too_many_arguments)]
pub(crate) fn candidate_air_coverage_with_timing(
    stats: &UnitStats,
    production_position: GridPosition,
    player_id: PlayerId,
    assessment: &AirDefenseAssessment,
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    damage_chart: &DamageChart,
    action_delay: u32,
    deadline_grace: u32,
) -> AirCoverageContribution {
    let candidate = CombatCapabilitySnapshot {
        faction: player_id,
        position: production_position,
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
        action_delay,
    };
    air_coverage_with_timing(
        &[candidate],
        assessment,
        map,
        registry,
        unit_positions,
        damage_chart,
        deadline_grace,
    )
}

/// 自軍・敵軍の状況から需要マトリクスを計算します。
///
/// # 計算方針
///
/// ## 消耗ギャップ（Attrition Gap）
/// カテゴリ別に「敵の攻撃期待値 - 自軍の反撃期待値」を算出します。
/// 対空が0体で敵ヘリが1体いれば `anti_air` は最大値になります。
///
/// ## 占領脅威（Capture Threat）
/// 敵の占領可能ユニットが重要拠点（首都・工場・空港）にどれだけ近いかを評価します。
/// 拠点の重要度 × 到達しやすさ（1/ETA）の積和です。
pub fn compute_demand(
    my_units: &[(GridPosition, UnitStats)],
    enemy_units: &[(GridPosition, UnitStats)],
    my_properties: &[(GridPosition, Terrain)],
    damage_chart: &DamageChart,
    unit_registry: &UnitRegistry,
) -> DemandMatrix {
    let normalization_scale = average_attack_expectation(damage_chart, unit_registry);

    // --- 消耗ギャップの計算 ---
    // 「敵が持つカテゴリ別の攻撃期待値」と「自軍がそのカテゴリに対して持つ反撃能力」の差を計算する。
    //
    // キー思想：
    //   - 敵に航空ユニット（Bcopters）がいる → 自軍に「対空能力」が必要
    //   - 「対空能力の不足」= 敵航空の攻撃力 - 自軍の anti_air 適性の合計
    //
    // 敵ユニットの脅威 = そのユニットが与えうるダメージ（全カテゴリへの平均）
    // 自軍の反撃能力  = 敵のカテゴリ（Air/Ground/Sea）に有効なユニットの適性合計

    // 敵の各カテゴリ（Air/Ground/Sea）が持つ攻撃の総量
    // （どれだけ強力な脅威が存在するか）
    let mut enemy_air_threat = 0.0f32; // 敵航空ユニットの総攻撃力（地上への攻撃能力）
    let mut enemy_ground_threat = 0.0f32;
    let mut enemy_sea_threat = 0.0f32;

    for (_, enemy_stats) in enemy_units {
        // 非戦闘ユニットはスキップ
        if enemy_stats.max_ammo1 == 0 && enemy_stats.max_ammo2 == 0 {
            continue;
        }
        // ユニット自体の適性（このユニットが何に強いか）を使って脅威を分類
        let affinity = compute_unit_affinity(
            enemy_stats.unit_type,
            damage_chart,
            unit_registry,
            normalization_scale,
        );
        let category = UnitCategory::from_movement_type(enemy_stats.movement_type);
        // 敵ユニットのカテゴリに応じて脅威を記録する
        // 「敵が Air カテゴリ」= 自軍は anti_air 能力が必要
        match category {
            UnitCategory::Air => {
                // 航空ユニットの「地上への攻撃適性」を航空脅威として積算。さらに標的としての最小脅威0.1を保証
                let threat = affinity.anti_ground.max(affinity.anti_sea).max(0.1);
                enemy_air_threat += threat;
            }
            UnitCategory::Ground => {
                // 地上ユニットの対地適性を積算。占領可能ユニット（歩兵）は最小脅威0.25、その他は0.1を保証
                let base_min = if enemy_stats.can_capture { 0.25 } else { 0.1 };
                let threat = affinity.anti_ground.max(base_min);
                enemy_ground_threat += threat;
            }
            UnitCategory::Sea => {
                // 海上ユニットの対海適性を積算。さらに標的としての最小脅威0.1を保証
                let threat = affinity.anti_sea.max(affinity.anti_ground).max(0.1);
                enemy_sea_threat += threat;
            }
        }
    }

    // 自軍の各カテゴリへの反撃能力を集計
    let mut my_power_vs_air = 0.0f32;
    let mut my_power_vs_ground = 0.0f32;
    let mut my_power_vs_sea = 0.0f32;

    for (_, my_stats) in my_units {
        if my_stats.max_ammo1 == 0 && my_stats.max_ammo2 == 0 {
            continue;
        }
        let affinity = compute_unit_affinity(
            my_stats.unit_type,
            damage_chart,
            unit_registry,
            normalization_scale,
        );
        my_power_vs_air += affinity.anti_air;
        my_power_vs_ground += affinity.anti_ground;
        my_power_vs_sea += affinity.anti_sea;
    }

    // ギャップ = 敵の脅威 - 自軍の反撃力（負にはならない）
    let gap_air = (enemy_air_threat - my_power_vs_air).max(0.0);
    let gap_ground = (enemy_ground_threat - my_power_vs_ground).max(0.0);
    let gap_sea = (enemy_sea_threat - my_power_vs_sea).max(0.0);

    // 正規化スケール：「1体分の適性値（≒1.0）」を基準とする。敵の総数が極めて少ない序盤は、戦闘需要に過剰反応しないように scale を底上げする
    let base_scale = if enemy_units.len() <= 2 {
        3.0f32 // 敵が少ない場合は3体分に相当するスケールにして需要を抑制
    } else {
        1.0f32
    };
    let unit_scale = base_scale.max(normalization_scale / 100.0);

    let anti_air = (gap_air / unit_scale).clamp(0.0, 1.0);
    let anti_ground = (gap_ground / unit_scale).clamp(0.0, 1.0);
    let anti_sea = (gap_sea / unit_scale).clamp(0.0, 1.0);

    // --- 占領脅威の計算 ---
    // 敵の占領可能ユニットが重要拠点に与えるリスクを算出
    let mut capture_threat = 0.0f32;

    // 拠点の重要度テーブル
    let importance = |terrain: Terrain| -> f32 {
        match terrain {
            Terrain::Capital => 3.0,
            Terrain::Factory | Terrain::Airport | Terrain::Port => 2.0,
            Terrain::City => 1.0,
            _ => 0.0,
        }
    };

    for (enemy_pos, enemy_stats) in enemy_units {
        if !enemy_stats.can_capture {
            continue;
        }
        // 最も近い重要拠点への脅威を評価
        for (prop_pos, terrain) in my_properties {
            let dist = (enemy_pos.x as i32 - prop_pos.x as i32).unsigned_abs()
                + (enemy_pos.y as i32 - prop_pos.y as i32).unsigned_abs();
            // 移動力を考慮した ETA の簡易見積もり（最低1ターン）
            let eta = (dist / enemy_stats.max_movement.max(1)).max(1);
            // 重要度 × 到達しやすさ（ETAが短いほど高い）
            capture_threat += importance(*terrain) / eta as f32;
        }
    }

    // 占領脅威を正規化（「重要拠点に1ターンで到達できる歩兵1体」が1.0相当）
    let capture_scale = 3.0f32; // Capital の importance が 3.0
    let capture = (capture_threat / capture_scale).clamp(0.0, 1.0);

    // --- 輸送需要（既存ロジックと同等、ここでは簡易計算） ---
    let total_targets = my_properties.len() as u32;
    let current_capacity: u32 = my_units.iter().map(|(_, s)| s.max_cargo).sum();
    let logistics = if total_targets > current_capacity {
        ((total_targets - current_capacity) as f32 / total_targets as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };

    DemandMatrix {
        anti_ground,
        anti_air,
        anti_sea,
        capture,
        logistics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::UnitStats;
    use crate::resources::{DamageChart, UnitRegistry};
    use std::collections::HashMap;

    fn capability(
        faction: PlayerId,
        position: GridPosition,
        unit_type: UnitType,
        movement_type: MovementType,
        max_movement: u32,
        max_range: u32,
        cost: u32,
    ) -> CombatCapabilitySnapshot {
        CombatCapabilitySnapshot {
            faction,
            position,
            unit_type,
            movement_type,
            hp: 100,
            cost,
            max_movement,
            min_range: 1,
            max_range,
            ammo1: 9,
            max_ammo1: 9,
            ammo2: 9,
            max_ammo2: 9,
            fuel: 99,
            action_delay: 0,
        }
    }

    fn make_registry_with(types: Vec<(UnitType, MovementType, u32, u32)>) -> UnitRegistry {
        let mut map = HashMap::new();
        for (ut, mt, ammo1, ammo2) in types {
            map.insert(
                ut,
                UnitStats {
                    unit_type: ut,
                    movement_type: mt,
                    max_ammo1: ammo1,
                    max_ammo2: ammo2,
                    max_movement: 3,
                    can_capture: matches!(ut, UnitType::Infantry | UnitType::Mech),
                    ..UnitStats::mock()
                },
            );
        }
        UnitRegistry(map)
    }

    #[test]
    fn issue75_air_threat_without_effective_coverage_is_emergency() {
        let map = Map::new(
            10,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bomber, UnitType::Infantry, 100);
        let units = vec![
            capability(
                PlayerId(2),
                GridPosition { x: 8, y: 0 },
                UnitType::Bomber,
                MovementType::Air,
                6,
                1,
                20_000,
            ),
            capability(
                PlayerId(1),
                GridPosition { x: 1, y: 0 },
                UnitType::Infantry,
                MovementType::Infantry,
                3,
                1,
                1_000,
            ),
        ];

        let assessment = assess_air_defense(
            PlayerId(1),
            &units,
            &[GridPosition { x: 1, y: 0 }],
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );

        assert!(assessment.requires_emergency_production());
        assert_eq!(assessment.shortage_ratio, 1.0);
    }

    #[test]
    fn issue75_existing_effective_unit_counts_as_air_coverage() {
        let map = Map::new(
            10,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 80);
        chart.insert_damage(UnitType::Bomber, UnitType::AntiAir, 100);
        let units = vec![
            capability(
                PlayerId(2),
                GridPosition { x: 6, y: 0 },
                UnitType::Bomber,
                MovementType::Air,
                6,
                1,
                20_000,
            ),
            capability(
                PlayerId(1),
                GridPosition { x: 1, y: 0 },
                UnitType::AntiAir,
                MovementType::Tank,
                6,
                1,
                8_000,
            ),
        ];

        let assessment = assess_air_defense(
            PlayerId(1),
            &units,
            &[GridPosition { x: 0, y: 0 }],
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );

        assert!(assessment.has_effective_coverage);
        assert!(!assessment.requires_emergency_production());
        assert!(assessment.shortage_ratio < 1.0);
    }

    #[test]
    fn issue75_action_completed_counter_keeps_its_next_turn_delay() {
        let map = Map::new(
            10,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 80);
        chart.insert_damage(UnitType::Bomber, UnitType::AntiAir, 100);
        let mut completed_counter = capability(
            PlayerId(1),
            GridPosition { x: 1, y: 0 },
            UnitType::AntiAir,
            MovementType::Tank,
            6,
            1,
            8_000,
        );
        completed_counter.action_delay = 1;
        let units = vec![
            capability(
                PlayerId(2),
                GridPosition { x: 6, y: 0 },
                UnitType::Bomber,
                MovementType::Air,
                6,
                1,
                20_000,
            ),
            completed_counter,
        ];

        let assessment = assess_air_defense(
            PlayerId(1),
            &units,
            &[GridPosition { x: 0, y: 0 }],
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );

        assert_eq!(assessment.current_coverage, 0.0);
        assert!(assessment.requires_emergency_production());
    }

    #[test]
    fn issue75_empty_ammo_does_not_count_as_effective_coverage() {
        let map = Map::new(
            10,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 80);
        chart.insert_damage(UnitType::Bomber, UnitType::AntiAir, 100);
        let mut empty_anti_air = capability(
            PlayerId(1),
            GridPosition { x: 1, y: 0 },
            UnitType::AntiAir,
            MovementType::Tank,
            6,
            1,
            8_000,
        );
        empty_anti_air.ammo1 = 0;
        empty_anti_air.ammo2 = 0;
        let units = vec![
            capability(
                PlayerId(2),
                GridPosition { x: 6, y: 0 },
                UnitType::Bomber,
                MovementType::Air,
                6,
                1,
                20_000,
            ),
            empty_anti_air,
        ];

        let assessment = assess_air_defense(
            PlayerId(1),
            &units,
            &[GridPosition { x: 0, y: 0 }],
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );

        assert!(!assessment.has_effective_coverage);
        assert!(assessment.requires_emergency_production());
        assert_eq!(assessment.current_coverage, 0.0);
    }

    #[test]
    fn issue75_aircraft_without_ammo_is_not_counted_as_immediate_threat() {
        let map = Map::new(
            10,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bomber, UnitType::Infantry, 100);
        let mut bomber = capability(
            PlayerId(2),
            GridPosition { x: 6, y: 0 },
            UnitType::Bomber,
            MovementType::Air,
            6,
            1,
            20_000,
        );
        bomber.ammo1 = 0;
        bomber.ammo2 = 0;

        let units = vec![
            bomber,
            capability(
                PlayerId(1),
                GridPosition { x: 1, y: 0 },
                UnitType::Infantry,
                MovementType::Infantry,
                3,
                1,
                1_000,
            ),
        ];
        let assessment = assess_air_defense(
            PlayerId(1),
            &units,
            &[GridPosition { x: 0, y: 0 }],
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );

        assert!(assessment.targets.is_empty());
        assert!(!assessment.requires_emergency_production());
    }

    #[test]
    fn issue75_aircraft_without_route_fuel_is_not_counted_as_immediate_threat() {
        let map = Map::new(
            10,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bomber, UnitType::Infantry, 100);
        let mut bomber = capability(
            PlayerId(2),
            GridPosition { x: 9, y: 0 },
            UnitType::Bomber,
            MovementType::Air,
            6,
            1,
            20_000,
        );
        bomber.fuel = 1;

        let units = vec![
            bomber,
            capability(
                PlayerId(1),
                GridPosition { x: 1, y: 0 },
                UnitType::Infantry,
                MovementType::Infantry,
                3,
                1,
                1_000,
            ),
        ];
        let assessment = assess_air_defense(
            PlayerId(1),
            &units,
            &[GridPosition { x: 0, y: 0 }],
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );

        assert!(assessment.targets.is_empty());
    }

    #[test]
    fn issue75_air_threat_power_uses_actual_friendly_targets() {
        let map = Map::new(
            10,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::HeavyFighter, UnitType::Bcopters, 95);
        chart.insert_damage(UnitType::HeavyFighter, UnitType::Infantry, 15);
        let units = vec![
            capability(
                PlayerId(2),
                GridPosition { x: 4, y: 0 },
                UnitType::HeavyFighter,
                MovementType::Air,
                8,
                1,
                26_000,
            ),
            capability(
                PlayerId(1),
                GridPosition { x: 1, y: 0 },
                UnitType::Infantry,
                MovementType::Infantry,
                3,
                1,
                1_000,
            ),
        ];

        let assessment = assess_air_defense(
            PlayerId(1),
            &units,
            &[GridPosition { x: 0, y: 0 }],
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );

        assert_eq!(assessment.targets.len(), 1);
        assert_eq!(assessment.targets[0].attack_power, 15);
        assert_eq!(assessment.required_coverage, 7_800.0);
    }

    #[test]
    fn issue75_one_ammo_round_is_not_credited_against_every_aircraft() {
        let map = Map::new(
            10,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bomber, UnitType::AntiAir, 100);
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 100);
        let mut anti_air = capability(
            PlayerId(1),
            GridPosition { x: 1, y: 0 },
            UnitType::AntiAir,
            MovementType::Tank,
            6,
            1,
            8_000,
        );
        anti_air.ammo1 = 1;
        anti_air.max_ammo1 = 1;
        anti_air.ammo2 = 0;
        anti_air.max_ammo2 = 1;
        let units = vec![
            capability(
                PlayerId(2),
                GridPosition { x: 6, y: 0 },
                UnitType::Bomber,
                MovementType::Air,
                6,
                1,
                20_000,
            ),
            capability(
                PlayerId(2),
                GridPosition { x: 7, y: 0 },
                UnitType::Bomber,
                MovementType::Air,
                6,
                1,
                20_000,
            ),
            anti_air,
        ];

        let assessment = assess_air_defense(
            PlayerId(1),
            &units,
            &[GridPosition { x: 0, y: 0 }],
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );

        assert_eq!(assessment.required_coverage, 80_000.0);
        assert_eq!(assessment.current_coverage, 40_000.0);
        assert_eq!(assessment.shortage_ratio, 0.5);
    }

    #[test]
    fn issue75_coverage_assignment_is_independent_of_unit_order() {
        let map = Map::new(
            10,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bomber, UnitType::Infantry, 100);
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 100);
        let enemy_a = capability(
            PlayerId(2),
            GridPosition { x: 4, y: 0 },
            UnitType::Bomber,
            MovementType::Air,
            6,
            1,
            20_000,
        );
        let enemy_b = capability(
            PlayerId(2),
            GridPosition { x: 8, y: 0 },
            UnitType::Bomber,
            MovementType::Air,
            3,
            1,
            20_000,
        );
        let mut flexible = capability(
            PlayerId(1),
            GridPosition { x: 5, y: 0 },
            UnitType::AntiAir,
            MovementType::Tank,
            6,
            1,
            8_000,
        );
        flexible.ammo1 = 1;
        flexible.max_ammo1 = 1;
        flexible.ammo2 = 0;
        flexible.max_ammo2 = 1;
        let mut constrained = capability(
            PlayerId(1),
            GridPosition { x: 1, y: 0 },
            UnitType::AntiAir,
            MovementType::Tank,
            2,
            1,
            8_000,
        );
        constrained.ammo1 = 1;
        constrained.max_ammo1 = 1;
        constrained.ammo2 = 0;
        constrained.max_ammo2 = 1;
        let protected = capability(
            PlayerId(1),
            GridPosition { x: 0, y: 0 },
            UnitType::Infantry,
            MovementType::Infantry,
            3,
            1,
            1_000,
        );
        let assess = |defenders: [CombatCapabilitySnapshot; 2]| {
            assess_air_defense(
                PlayerId(1),
                &[enemy_a, enemy_b, protected, defenders[0], defenders[1]],
                &[GridPosition { x: 0, y: 0 }],
                &map,
                &registry,
                &HashMap::new(),
                &chart,
            )
        };

        let forward = assess([flexible, constrained]);
        let reversed = assess([constrained, flexible]);

        assert_eq!(reversed.coverage_by_target, forward.coverage_by_target);
        assert_eq!(forward.current_coverage, forward.required_coverage);
        assert_eq!(forward.shortage_ratio, 0.0);
    }

    #[test]
    fn issue75_flexible_shot_prefers_larger_uncovered_value() {
        let map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Missiles, UnitType::Bomber, 100);
        let targets = vec![
            AirThreatTarget {
                position: GridPosition { x: 2, y: 0 },
                unit_type: UnitType::Bomber,
                hp: 100,
                cost: 20_000,
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
        ];
        let mut missiles = capability(
            PlayerId(1),
            GridPosition { x: 0, y: 0 },
            UnitType::Missiles,
            MovementType::Tank,
            5,
            5,
            12_000,
        );
        missiles.min_range = 2;
        missiles.ammo1 = 1;
        missiles.max_ammo1 = 1;
        missiles.ammo2 = 0;
        missiles.max_ammo2 = 1;
        let mut coverage = vec![18_000.0, 0.0];

        allocate_air_coverage(
            &[missiles],
            &targets,
            &map,
            &registry,
            &HashMap::new(),
            &chart,
            &mut coverage,
        );

        assert_eq!(coverage, vec![18_000.0, 30_000.0]);
    }

    #[test]
    fn issue75_flexible_shot_accounts_for_future_weak_alternative() {
        let map = Map::new(
            9,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Missiles, UnitType::Bomber, 100);
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 10);
        let targets = vec![
            AirThreatTarget {
                position: GridPosition { x: 0, y: 0 },
                unit_type: UnitType::Bomber,
                hp: 100,
                cost: 20_000,
                attack_power: 100,
                deadline_turns: 2,
            },
            AirThreatTarget {
                position: GridPosition { x: 8, y: 0 },
                unit_type: UnitType::Bomber,
                hp: 100,
                cost: 20_000,
                attack_power: 100,
                deadline_turns: 2,
            },
        ];
        let mut flexible = capability(
            PlayerId(1),
            GridPosition { x: 4, y: 0 },
            UnitType::Missiles,
            MovementType::Tank,
            5,
            5,
            12_000,
        );
        flexible.min_range = 2;
        flexible.ammo1 = 1;
        flexible.max_ammo1 = 1;
        flexible.ammo2 = 0;
        flexible.max_ammo2 = 1;
        let mut weak_alternative = capability(
            PlayerId(1),
            GridPosition { x: 5, y: 0 },
            UnitType::AntiAir,
            MovementType::Tank,
            1,
            1,
            8_000,
        );
        weak_alternative.ammo1 = 1;
        weak_alternative.max_ammo1 = 1;
        weak_alternative.ammo2 = 0;
        weak_alternative.max_ammo2 = 1;
        let mut coverage = vec![19_800.0, 0.0];

        allocate_air_coverage(
            &[flexible, weak_alternative],
            &targets,
            &map,
            &registry,
            &HashMap::new(),
            &chart,
            &mut coverage,
        );

        assert_eq!(coverage, vec![19_800.0, 30_000.0]);
    }

    #[test]
    fn issue75_future_projection_does_not_reuse_one_defender() {
        let map = Map::new(
            7,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Missiles, UnitType::Bomber, 100);
        let targets = [2, 3, 4]
            .into_iter()
            .map(|x| AirThreatTarget {
                position: GridPosition { x, y: 0 },
                unit_type: UnitType::Bomber,
                hp: 100,
                cost: 20_000,
                attack_power: 100,
                deadline_turns: 2,
            })
            .collect::<Vec<_>>();
        let missile = |x| {
            let mut unit = capability(
                PlayerId(1),
                GridPosition { x, y: 0 },
                UnitType::Missiles,
                MovementType::Tank,
                5,
                5,
                12_000,
            );
            unit.min_range = 2;
            unit.ammo1 = 1;
            unit.max_ammo1 = 1;
            unit.ammo2 = 0;
            unit.max_ammo2 = 1;
            unit
        };
        let mut coverage = vec![19_800.0, 0.0, 0.0];

        allocate_air_coverage(
            &[missile(0), missile(6)],
            &targets,
            &map,
            &registry,
            &HashMap::new(),
            &chart,
            &mut coverage,
        );

        assert_eq!(coverage, vec![19_800.0, 30_000.0, 30_000.0]);
    }

    #[test]
    fn issue75_stationary_unit_can_fire_at_multiple_aircraft_over_time() {
        let map = Map::new(
            6,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Missiles, UnitType::Bomber, 100);
        let targets = vec![
            AirThreatTarget {
                position: GridPosition { x: 3, y: 0 },
                unit_type: UnitType::Bomber,
                hp: 100,
                cost: 20_000,
                attack_power: 100,
                deadline_turns: 3,
            },
            AirThreatTarget {
                position: GridPosition { x: 4, y: 0 },
                unit_type: UnitType::Bomber,
                hp: 100,
                cost: 20_000,
                attack_power: 100,
                deadline_turns: 3,
            },
        ];
        let required_coverage = targets.iter().map(target_threat_value).sum();
        let assessment = AirDefenseAssessment {
            targets,
            coverage_by_target: vec![0.0, 0.0],
            required_coverage,
            current_coverage: 0.0,
            shortage_ratio: 1.0,
            has_effective_coverage: false,
        };
        let missiles = UnitStats {
            unit_type: UnitType::Missiles,
            movement_type: MovementType::Tank,
            max_movement: 5,
            max_fuel: 99,
            max_ammo1: 4,
            min_range: 2,
            max_range: 5,
            ..UnitStats::mock()
        };

        let contribution = candidate_air_coverage(
            &missiles,
            GridPosition { x: 0, y: 0 },
            PlayerId(1),
            &assessment,
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );

        let target_value = target_threat_value(&assessment.targets[0]);
        assert_eq!(contribution.by_target, vec![target_value, target_value]);
        assert_eq!(contribution.total, target_value * 2.0);
    }

    #[test]
    fn issue75_multiple_action_turns_can_finish_one_aircraft() {
        let map = Map::new(
            10,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bomber, UnitType::Infantry, 100);
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 40);
        let mut anti_air = capability(
            PlayerId(1),
            GridPosition { x: 4, y: 0 },
            UnitType::AntiAir,
            MovementType::Tank,
            6,
            1,
            8_000,
        );
        anti_air.ammo1 = 5;
        anti_air.max_ammo1 = 5;
        anti_air.ammo2 = 0;
        anti_air.max_ammo2 = 1;
        let units = vec![
            capability(
                PlayerId(2),
                GridPosition { x: 7, y: 0 },
                UnitType::Bomber,
                MovementType::Air,
                2,
                1,
                20_000,
            ),
            capability(
                PlayerId(1),
                GridPosition { x: 0, y: 0 },
                UnitType::Infantry,
                MovementType::Infantry,
                3,
                1,
                1_000,
            ),
            anti_air,
        ];

        let assessment = assess_air_defense(
            PlayerId(1),
            &units,
            &[GridPosition { x: 0, y: 0 }],
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );

        assert_eq!(assessment.targets[0].deadline_turns, 3);
        assert_eq!(assessment.current_coverage, assessment.required_coverage);
        assert_eq!(assessment.shortage_ratio, 0.0);
    }

    #[test]
    fn issue75_large_air_threat_keeps_residual_demand_after_one_unit() {
        let mut assessment = AirDefenseAssessment {
            targets: vec![AirThreatTarget {
                position: GridPosition { x: 6, y: 0 },
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
        };

        assessment.apply_coverage(&AirCoverageContribution {
            by_target: vec![5_000.0],
            total: 5_000.0,
        });

        assert!(assessment.has_effective_coverage);
        assert!(assessment.requires_emergency_production());
        assert_eq!(assessment.shortage_ratio, 0.875);

        assessment.apply_coverage(&AirCoverageContribution {
            by_target: vec![15_000.0],
            total: 15_000.0,
        });

        assert!(!assessment.requires_emergency_production());
        assert_eq!(assessment.shortage_ratio, 0.5);
    }

    #[test]
    fn issue75_distant_zero_coverage_air_threat_starts_preparation_early() {
        let target = AirThreatTarget {
            position: GridPosition { x: 6, y: 0 },
            unit_type: UnitType::Bomber,
            hp: 100,
            cost: 20_000,
            attack_power: 100,
            deadline_turns: 3,
        };
        let assessment = AirDefenseAssessment {
            targets: vec![target],
            coverage_by_target: vec![0.0],
            required_coverage: target_threat_value(&target),
            current_coverage: 0.0,
            shortage_ratio: 1.0,
            has_effective_coverage: false,
        };

        assert!(assessment.requires_emergency_production());
    }

    #[test]
    fn issue75_uncovered_bomber_keeps_emergency_gate_active() {
        let mut assessment = AirDefenseAssessment {
            targets: vec![
                AirThreatTarget {
                    position: GridPosition { x: 3, y: 0 },
                    unit_type: UnitType::Bcopters,
                    hp: 100,
                    cost: 7_500,
                    attack_power: 100,
                    deadline_turns: 1,
                },
                AirThreatTarget {
                    position: GridPosition { x: 6, y: 0 },
                    unit_type: UnitType::Bomber,
                    hp: 100,
                    cost: 20_000,
                    attack_power: 100,
                    deadline_turns: 1,
                },
            ],
            coverage_by_target: vec![5_000.0, 0.0],
            required_coverage: 55_000.0,
            current_coverage: 5_000.0,
            shortage_ratio: 50_000.0 / 55_000.0,
            has_effective_coverage: true,
        };

        assert!(assessment.requires_emergency_production());

        assessment.apply_coverage(&AirCoverageContribution {
            by_target: vec![0.0, 1.0],
            total: 1.0,
        });

        assert!(assessment.requires_emergency_production());
    }

    #[test]
    fn issue75_candidate_decay_does_not_double_count_covered_target() {
        let map = Map {
            width: 3,
            height: 1,
            tiles: vec![Terrain::Plains, Terrain::Sea, Terrain::Plains],
            topology: crate::resources::GridTopology::Square,
        };
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 100);
        let mut assessment = AirDefenseAssessment {
            targets: vec![
                AirThreatTarget {
                    position: GridPosition { x: 0, y: 0 },
                    unit_type: UnitType::Bomber,
                    hp: 100,
                    cost: 20_000,
                    attack_power: 100,
                    deadline_turns: 2,
                },
                AirThreatTarget {
                    position: GridPosition { x: 2, y: 0 },
                    unit_type: UnitType::Bomber,
                    hp: 100,
                    cost: 20_000,
                    attack_power: 100,
                    deadline_turns: 2,
                },
            ],
            coverage_by_target: vec![30_000.0, 0.0],
            required_coverage: 60_000.0,
            current_coverage: 30_000.0,
            shortage_ratio: 0.5,
            has_effective_coverage: true,
        };
        let anti_air = UnitStats {
            unit_type: UnitType::AntiAir,
            movement_type: MovementType::Tank,
            max_movement: 6,
            max_fuel: 99,
            max_ammo1: 9,
            max_range: 1,
            ..UnitStats::mock()
        };

        let contribution = candidate_air_coverage(
            &anti_air,
            GridPosition { x: 0, y: 0 },
            PlayerId(1),
            &assessment,
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );
        assessment.apply_coverage(&contribution);

        assert_eq!(contribution.total, 0.0);
        assert_eq!(assessment.coverage_by_target, vec![30_000.0, 0.0]);
        assert_eq!(assessment.shortage_ratio, 0.5);
    }

    #[test]
    fn issue75_nearer_air_threat_has_higher_required_value() {
        let near = AirThreatTarget {
            position: GridPosition { x: 1, y: 0 },
            unit_type: UnitType::Bomber,
            hp: 100,
            cost: 20_000,
            attack_power: 100,
            deadline_turns: 1,
        };
        let far = AirThreatTarget {
            deadline_turns: 4,
            ..near
        };

        assert!(target_threat_value(&near) > target_threat_value(&far));
    }

    #[test]
    fn issue75_deadline_does_not_include_an_extra_action_turn() {
        let map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 100);
        let assessment = AirDefenseAssessment {
            targets: vec![AirThreatTarget {
                position: GridPosition { x: 4, y: 0 },
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
        };
        let anti_air = UnitStats {
            unit_type: UnitType::AntiAir,
            movement_type: MovementType::Tank,
            max_movement: 2,
            max_fuel: 99,
            max_range: 1,
            ..UnitStats::mock()
        };

        let contribution = candidate_air_coverage(
            &anti_air,
            GridPosition { x: 0, y: 0 },
            PlayerId(1),
            &assessment,
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );

        assert_eq!(contribution.total, 0.0);
    }

    #[test]
    fn issue75_minimum_range_dead_zone_requires_repositioning() {
        let map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Missiles, UnitType::Bomber, 100);
        let assessment = AirDefenseAssessment {
            targets: vec![AirThreatTarget {
                position: GridPosition { x: 4, y: 0 },
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
        };
        let missiles = UnitStats {
            unit_type: UnitType::Missiles,
            movement_type: MovementType::Tank,
            max_movement: 4,
            max_fuel: 99,
            max_ammo1: 6,
            min_range: 2,
            max_range: 5,
            ..UnitStats::mock()
        };

        let contribution = candidate_air_coverage(
            &missiles,
            GridPosition { x: 3, y: 0 },
            PlayerId(1),
            &assessment,
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );

        assert_eq!(contribution.total, 0.0);
    }

    #[test]
    fn issue75_produced_counter_waits_until_next_friendly_turn() {
        let map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Missiles, UnitType::Bomber, 100);
        let assessment = AirDefenseAssessment {
            targets: vec![AirThreatTarget {
                position: GridPosition { x: 3, y: 0 },
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
        };
        let missiles = UnitStats {
            unit_type: UnitType::Missiles,
            movement_type: MovementType::Tank,
            max_movement: 4,
            max_fuel: 99,
            max_ammo1: 6,
            min_range: 2,
            max_range: 5,
            ..UnitStats::mock()
        };

        let contribution = candidate_air_coverage(
            &missiles,
            GridPosition { x: 0, y: 0 },
            PlayerId(1),
            &assessment,
            &map,
            &registry,
            &HashMap::new(),
            &chart,
        );

        assert_eq!(contribution.total, 0.0);
    }

    #[test]
    fn issue75_candidate_coverage_uses_damage_mobility_and_deadline() {
        let map = Map::new(
            10,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 80);
        let assessment = AirDefenseAssessment {
            targets: vec![AirThreatTarget {
                position: GridPosition { x: 6, y: 0 },
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
        };
        let anti_air = UnitStats {
            unit_type: UnitType::AntiAir,
            movement_type: MovementType::Tank,
            max_movement: 6,
            max_fuel: 99,
            max_range: 1,
            ..UnitStats::mock()
        };
        let rockets = UnitStats {
            unit_type: UnitType::Rockets,
            movement_type: MovementType::Tank,
            max_movement: 5,
            max_fuel: 99,
            min_range: 3,
            max_range: 5,
            ..UnitStats::mock()
        };

        assert!(
            candidate_air_coverage(
                &anti_air,
                GridPosition { x: 0, y: 0 },
                PlayerId(1),
                &assessment,
                &map,
                &registry,
                &HashMap::new(),
                &chart,
            )
            .total
                > 0.0
        );
        assert_eq!(
            candidate_air_coverage(
                &rockets,
                GridPosition { x: 0, y: 0 },
                PlayerId(1),
                &assessment,
                &map,
                &registry,
                &HashMap::new(),
                &chart,
            )
            .total,
            0.0
        );
    }

    #[test]
    fn issue75_candidate_coverage_rejects_impassable_route() {
        let map = Map {
            width: 3,
            height: 1,
            tiles: vec![Terrain::Plains, Terrain::Sea, Terrain::Plains],
            topology: crate::resources::GridTopology::Square,
        };
        let registry = MasterDataRegistry::load().unwrap();
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::AntiAir, UnitType::Bomber, 80);
        let assessment = AirDefenseAssessment {
            targets: vec![AirThreatTarget {
                position: GridPosition { x: 2, y: 0 },
                unit_type: UnitType::Bomber,
                hp: 100,
                cost: 20_000,
                attack_power: 100,
                deadline_turns: 10,
            }],
            coverage_by_target: vec![0.0],
            required_coverage: 22_000.0,
            current_coverage: 0.0,
            shortage_ratio: 1.0,
            has_effective_coverage: false,
        };
        let anti_air = UnitStats {
            unit_type: UnitType::AntiAir,
            movement_type: MovementType::Tank,
            max_movement: 6,
            max_fuel: 99,
            max_range: 1,
            ..UnitStats::mock()
        };

        assert_eq!(
            candidate_air_coverage(
                &anti_air,
                GridPosition { x: 0, y: 0 },
                PlayerId(1),
                &assessment,
                &map,
                &registry,
                &HashMap::new(),
                &chart,
            )
            .total,
            0.0
        );
    }

    /// 敵に航空機のみが存在し、自軍に対空ユニットがない場合、anti_air が高くなること
    #[test]
    fn test_anti_air_demand_rises_with_air_enemy() {
        let mut chart = DamageChart::new();
        // 対空戦車 → 戦闘ヘリへの高ダメージ
        chart.insert_damage(UnitType::AntiAir, UnitType::Bcopters, 120);
        // 装甲車 → 戦闘ヘリへの低ダメージ
        chart.insert_damage(UnitType::Recon, UnitType::Bcopters, 10);
        // 戦闘ヘリ → 地上ユニットへの攻撃
        chart.insert_damage(UnitType::Bcopters, UnitType::Recon, 70);

        let registry = make_registry_with(vec![
            (UnitType::AntiAir, MovementType::Tank, 9, 0),
            (UnitType::Recon, MovementType::ArmoredCar, 6, 0),
            (UnitType::Bcopters, MovementType::Air, 6, 0),
        ]);

        // 自軍：装甲車のみ（対空なし）
        let my_units = vec![(
            GridPosition { x: 3, y: 3 },
            UnitStats {
                unit_type: UnitType::Recon,
                movement_type: MovementType::ArmoredCar,
                max_ammo1: 6,
                max_ammo2: 0,
                ..UnitStats::mock()
            },
        )];
        // 敵：戦闘ヘリ
        let enemy_units = vec![(
            GridPosition { x: 5, y: 5 },
            UnitStats {
                unit_type: UnitType::Bcopters,
                movement_type: MovementType::Air,
                max_ammo1: 6,
                max_ammo2: 0,
                ..UnitStats::mock()
            },
        )];

        let demand = compute_demand(&my_units, &enemy_units, &[], &chart, &registry);

        assert!(
            demand.anti_air > 0.0,
            "敵ヘリがいて対空なし → anti_air > 0 のはずだが {}",
            demand.anti_air
        );
        assert!(
            demand.anti_air > demand.anti_ground,
            "航空脅威 > 地上脅威 のはずだが anti_air={} anti_ground={}",
            demand.anti_air,
            demand.anti_ground
        );
    }

    /// 占領可能ユニットが重要拠点（首都）の近くにいる場合、capture 需要が高くなること
    #[test]
    fn test_capture_threat_near_capital() {
        let chart = DamageChart::new();
        let registry = make_registry_with(vec![
            (UnitType::Infantry, MovementType::Infantry, 0, 0),
            (UnitType::Recon, MovementType::ArmoredCar, 6, 0),
        ]);

        // 敵歩兵が首都の1マス隣（ETA=1）
        let enemy_units = vec![(
            GridPosition { x: 4, y: 3 },
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                can_capture: true,
                max_ammo1: 0,
                max_ammo2: 0,
                ..UnitStats::mock()
            },
        )];

        // 自軍：首都を所有
        let my_properties = vec![(GridPosition { x: 3, y: 3 }, Terrain::Capital)];

        let demand_near = compute_demand(&[], &enemy_units, &my_properties, &chart, &registry);

        // 敵歩兵が遠い場合（ETA=4）
        let enemy_far = vec![(
            GridPosition { x: 10, y: 10 },
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                can_capture: true,
                max_ammo1: 0,
                max_ammo2: 0,
                ..UnitStats::mock()
            },
        )];
        let demand_far = compute_demand(&[], &enemy_far, &my_properties, &chart, &registry);

        assert!(
            demand_near.capture > demand_far.capture,
            "首都近くの敵歩兵の方が遠い敵より capture 需要が高いはずだが near={} far={}",
            demand_near.capture,
            demand_far.capture
        );
        assert!(
            demand_near.capture > 0.0,
            "capture 需要 > 0 のはずだが {}",
            demand_near.capture
        );
    }

    /// 対空戦車の anti_air 適性が装甲車より高いこと
    #[test]
    fn test_unit_affinity_antiair_vs_armor() {
        let master_data = crate::resources::MasterDataRegistry::load().unwrap();
        let (world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_2").unwrap();

        let damage_chart = world.get_resource::<DamageChart>().unwrap();
        let unit_registry = world.get_resource::<UnitRegistry>().unwrap();
        let scale = average_attack_expectation(damage_chart, unit_registry);

        let antiair_affinity =
            compute_unit_affinity(UnitType::AntiAir, damage_chart, unit_registry, scale);
        let recon_affinity =
            compute_unit_affinity(UnitType::Recon, damage_chart, unit_registry, scale);

        assert!(
            antiair_affinity.anti_air > recon_affinity.anti_air,
            "対空戦車の anti_air 適性({}) > 装甲車({}) のはずだが",
            antiair_affinity.anti_air,
            recon_affinity.anti_air
        );
    }

    /// 敵の数が極めて少ない序盤に戦闘需要が適切に抑制されること、および normalization_scale の境界での挙動を確認する
    #[test]
    fn test_early_game_scaling_logic() {
        let mut chart = DamageChart::new();
        // 適性計算用の適当なダミーダメージを設定
        chart.insert_damage(UnitType::Recon, UnitType::Recon, 50);

        let registry = make_registry_with(vec![(UnitType::Recon, MovementType::ArmoredCar, 6, 0)]);

        let my_units = vec![
            (
                GridPosition { x: 3, y: 3 },
                UnitStats {
                    unit_type: UnitType::Recon,
                    movement_type: MovementType::ArmoredCar,
                    max_ammo1: 6,
                    max_ammo2: 0,
                    ..UnitStats::mock()
                },
            ),
            (
                GridPosition { x: 3, y: 4 },
                UnitStats {
                    unit_type: UnitType::Recon,
                    movement_type: MovementType::ArmoredCar,
                    max_ammo1: 6,
                    max_ammo2: 0,
                    ..UnitStats::mock()
                },
            ),
        ];

        // --- ケース1: 敵が2体以下 (enemy_units.len() <= 2) ---
        let enemy_units_2 = vec![
            (
                GridPosition { x: 5, y: 5 },
                UnitStats {
                    unit_type: UnitType::Recon,
                    movement_type: MovementType::ArmoredCar,
                    max_ammo1: 6,
                    max_ammo2: 0,
                    ..UnitStats::mock()
                },
            ),
            (
                GridPosition { x: 6, y: 6 },
                UnitStats {
                    unit_type: UnitType::Recon,
                    movement_type: MovementType::ArmoredCar,
                    max_ammo1: 6,
                    max_ammo2: 0,
                    ..UnitStats::mock()
                },
            ),
        ];

        // 期待される挙動: 敵が2体以下なので base_scale = 3.0。需要が抑制される
        let demand_2 = compute_demand(&my_units, &enemy_units_2, &[], &chart, &registry);

        // --- ケース2: 敵が3体以上 (enemy_units.len() >= 3) ---
        let enemy_units_3 = vec![
            (
                GridPosition { x: 5, y: 5 },
                UnitStats {
                    unit_type: UnitType::Recon,
                    movement_type: MovementType::ArmoredCar,
                    max_ammo1: 6,
                    max_ammo2: 0,
                    ..UnitStats::mock()
                },
            ),
            (
                GridPosition { x: 6, y: 6 },
                UnitStats {
                    unit_type: UnitType::Recon,
                    movement_type: MovementType::ArmoredCar,
                    max_ammo1: 6,
                    max_ammo2: 0,
                    ..UnitStats::mock()
                },
            ),
            (
                GridPosition { x: 7, y: 7 },
                UnitStats {
                    unit_type: UnitType::Recon,
                    movement_type: MovementType::ArmoredCar,
                    max_ammo1: 6,
                    max_ammo2: 0,
                    ..UnitStats::mock()
                },
            ),
        ];

        // 期待される挙動: 敵が3体以上なので base_scale = 1.0。スケーリングによる需要抑制が解除され、需要が高くなる
        let demand_3 = compute_demand(&my_units, &enemy_units_3, &[], &chart, &registry);

        // 敵2体のときの方が、敵3体のときよりも需要が抑制される（base_scaleが大きいため）
        assert!(
            demand_2.anti_ground < demand_3.anti_ground,
            "敵が2体以下(base_scale=3.0)の時の方が、敵3体以上(base_scale=1.0)の時より需要が小さくなるはずだが、demand_2.anti_ground={}, demand_3.anti_ground={}",
            demand_2.anti_ground,
            demand_3.anti_ground
        );

        // --- ケース3: normalization_scale が 100.0 の境界付近でのテスト ---
        // 高ダメージ (normalization_scale = 200.0) -> unit_scale = 1.0.max(2.0) = 2.0
        let mut chart_high = DamageChart::new();
        chart_high.insert_damage(UnitType::Recon, UnitType::Recon, 200);
        let demand_high = compute_demand(&my_units, &enemy_units_3, &[], &chart_high, &registry);

        // 低ダメージ (normalization_scale = 50.0) -> unit_scale = 1.0.max(0.5) = 1.0
        let mut chart_low = DamageChart::new();
        chart_low.insert_damage(UnitType::Recon, UnitType::Recon, 50);
        let demand_low = compute_demand(&my_units, &enemy_units_3, &[], &chart_low, &registry);

        // 高ダメージ（＝平均期待値が大きい＝敵戦力が脅威）の方が unit_scale が大きくなるため、個々の戦闘ユニット差による需要は相対的に抑制される
        assert!(
            demand_high.anti_ground < demand_low.anti_ground,
            "normalization_scaleが大きい（high）場合の方が、unit_scaleが大きくなって相対的需要が抑制されるはずだが、high={}, low={}",
            demand_high.anti_ground,
            demand_low.anti_ground
        );
    }

    /// 敵が歩兵（戦闘力は低いが占領可能）を大量に持っている場合、標的ボリュームにより anti_ground の需要が高くなること
    #[test]
    fn test_anti_ground_demand_rises_with_many_enemy_infantry() {
        let mut chart = DamageChart::new();
        // 攻撃適性計算用の適当なダミーダメージを設定
        chart.insert_damage(UnitType::Recon, UnitType::Infantry, 70);
        chart.insert_damage(UnitType::Infantry, UnitType::Recon, 10); // 歩兵からの反撃は弱い

        let registry = make_registry_with(vec![
            (UnitType::Infantry, MovementType::Infantry, 0, 0),
            (UnitType::Recon, MovementType::ArmoredCar, 6, 0),
        ]);

        let my_units = vec![]; // 自軍は戦闘ユニットなし

        // 敵歩兵が5体
        let enemy_units = (0..5)
            .map(|i| {
                (
                    GridPosition { x: i, y: 5 },
                    UnitStats {
                        unit_type: UnitType::Infantry,
                        movement_type: MovementType::Infantry,
                        can_capture: true,
                        // 攻撃能力を持たせるために弾薬を設定（非戦闘ユニット扱いされないように）
                        max_ammo1: 1,
                        ..UnitStats::mock()
                    },
                )
            })
            .collect::<Vec<_>>();

        let demand = compute_demand(&my_units, &enemy_units, &[], &chart, &registry);

        assert!(
            demand.anti_ground > 0.5,
            "敵歩兵が大量に存在する場合、自軍地上戦力がなければ anti_ground 需要が高くなるべきだが、実際は {}",
            demand.anti_ground
        );
    }
}
