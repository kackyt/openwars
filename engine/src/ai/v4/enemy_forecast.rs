//! 敵生産の観測学習と増援予測。

use crate::ai::v4::*;

/// Expectedへ入れる敵生産の波数。Expectedは毎ターン再評価する直近波であり、
/// 作戦期限全体の全生産を一括で仮想敵へ積むStressとは意図的に分ける。
pub(in crate::ai::v4) const EXPECTED_REINFORCEMENT_WAVES: u32 = 1;

/// 傾向推定に残す直近の観測手番数。古い開幕編成を現在の生産傾向に混ぜない。
pub(in crate::ai::v4) const ENEMY_PRODUCTION_HISTORY_TURNS: u32 = 6;

/// 実際に新規観測した敵Entity 1 体分の生産実績。
#[derive(Debug, Clone, Copy)]
pub(in crate::ai::v4) struct ObservedEnemyProduction {
    pub(in crate::ai::v4) turn: u32,
    pub(in crate::ai::v4) unit_type: UnitType,
    pub(in crate::ai::v4) cost: u32,
}

/// 予測した次回観測と、その手番で照合する集計値。
#[derive(Debug, Clone, Copy)]
pub(in crate::ai::v4) struct PendingEnemyProductionForecast {
    pub(in crate::ai::v4) turn: u32,
    pub(in crate::ai::v4) units: u32,
    pub(in crate::ai::v4) cost: u32,
}

/// 敵生産の実績・予測誤差を、自軍AIごとに保持する状態。
#[derive(Debug, Default)]
pub(in crate::ai::v4) struct EnemyProductionEstimatorState {
    pub(in crate::ai::v4) initialized: bool,
    pub(in crate::ai::v4) known_entities: HashSet<Entity>,
    pub(in crate::ai::v4) history: VecDeque<ObservedEnemyProduction>,
    pub(in crate::ai::v4) pending: VecDeque<PendingEnemyProductionForecast>,
    pub(in crate::ai::v4) evaluated_samples: u32,
    pub(in crate::ai::v4) absolute_unit_error_sum: u32,
    pub(in crate::ai::v4) absolute_cost_error_sum: u32,
}

/// 敵の実生産履歴から次手番の生産量を推定する台帳。
///
/// 初回走査で盤上にいる敵は開幕配置か既存戦力かを区別できないため、学習対象にしない。
/// 以後に新規出現したEntityだけを実生産として扱う。
#[derive(Resource, Debug, Default)]
pub(in crate::ai::v4) struct EnemyProductionEstimator {
    pub(in crate::ai::v4) by_observer: HashMap<PlayerId, EnemyProductionEstimatorState>,
}

/// 直近の実生産から、次の敵手番に現れる量を予測して前回予測を採点する。
///
/// 基準は履歴全体の平均で、直近2手番の平均がその前の2手番を上回るときだけ
/// 差分を加える。したがって「同じ傾向が続き、増加中なら少し増える」予測になり、
/// 施設数×期限の最悪ケースをそのまま要求量へ変換しない。
pub(in crate::ai::v4) fn observe_enemy_production(
    world: &mut World,
    player_id: PlayerId,
    turn: u32,
    scan: &BoardScan,
) -> EnemyProductionForecastTrace {
    let mut estimator = world
        .remove_resource::<EnemyProductionEstimator>()
        .unwrap_or_default();
    let state = estimator.by_observer.entry(player_id).or_default();
    let visible = scan
        .enemy_units
        .iter()
        .filter_map(|unit| unit.entity.map(|entity| (entity, unit)))
        .collect::<HashMap<_, _>>();

    if !state.initialized {
        state.initialized = true;
        state.known_entities = visible.keys().copied().collect();
        world.insert_resource(estimator);
        return EnemyProductionForecastTrace::default();
    }

    let new_productions = visible
        .iter()
        .filter(|(entity, _)| !state.known_entities.contains(entity))
        .map(|(_, unit)| ObservedEnemyProduction {
            turn,
            unit_type: unit.stats.unit_type,
            cost: unit.stats.cost,
        })
        .collect::<Vec<_>>();
    state.known_entities = visible.keys().copied().collect();

    let actual_units = u32::try_from(new_productions.len()).unwrap_or(u32::MAX);
    let actual_cost = new_productions
        .iter()
        .map(|production| production.cost)
        .fold(0_u32, u32::saturating_add);
    while state
        .pending
        .front()
        .is_some_and(|forecast| forecast.turn <= turn)
    {
        let forecast = state.pending.pop_front().expect("frontで存在を確認済み");
        state.evaluated_samples = state.evaluated_samples.saturating_add(1);
        state.absolute_unit_error_sum = state
            .absolute_unit_error_sum
            .saturating_add(forecast.units.abs_diff(actual_units));
        state.absolute_cost_error_sum = state
            .absolute_cost_error_sum
            .saturating_add(forecast.cost.abs_diff(actual_cost));
    }
    state.history.extend(new_productions);
    let oldest_turn = turn.saturating_sub(ENEMY_PRODUCTION_HISTORY_TURNS.saturating_sub(1));
    while state
        .history
        .front()
        .is_some_and(|production| production.turn < oldest_turn)
    {
        state.history.pop_front();
    }

    let history_start = state
        .history
        .front()
        .map_or(turn, |production| production.turn);
    let observed_turns = turn.saturating_sub(history_start).saturating_add(1).max(1);
    let observed_count = u32::try_from(state.history.len()).unwrap_or(u32::MAX);
    let baseline_per_turn = observed_count.div_ceil(observed_turns);
    let recent_start = turn.saturating_sub(1);
    let previous_start = turn.saturating_sub(3);
    let previous_end = turn.saturating_sub(2);
    let recent_count = state
        .history
        .iter()
        .filter(|production| production.turn >= recent_start)
        .count() as u32;
    let previous_count = state
        .history
        .iter()
        .filter(|production| production.turn >= previous_start && production.turn <= previous_end)
        .count() as u32;
    // 増加傾向は比較対象となる過去2手番が揃ってからだけ加える。初回に1体
    // 観測しただけで「増加中」と誤認して予測を二倍にしない。
    let growth_per_turn = if turn.saturating_sub(history_start) >= 3 {
        recent_count
            .div_ceil(2)
            .saturating_sub(previous_count.div_ceil(2))
    } else {
        0
    };
    let expected_units = baseline_per_turn
        .saturating_add(growth_per_turn)
        .min(scan.enemy_production_slots);
    let observed_cost = state
        .history
        .iter()
        .map(|production| production.cost)
        .fold(0_u32, u32::saturating_add);
    let average_cost = observed_cost / observed_count.max(1);
    let expected_cost = average_cost.saturating_mul(expected_units);
    let dominant_unit_type = state
        .history
        .iter()
        .fold(Vec::<(UnitType, u32)>::new(), |mut counts, production| {
            if let Some((_, count)) = counts
                .iter_mut()
                .find(|(unit_type, _)| *unit_type == production.unit_type)
            {
                *count = count.saturating_add(1);
            } else {
                counts.push((production.unit_type, 1));
            }
            counts
        })
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(unit_type, _)| unit_type);
    state.pending.push_back(PendingEnemyProductionForecast {
        turn: turn.saturating_add(1),
        units: expected_units,
        cost: expected_cost,
    });
    let samples = state.evaluated_samples.max(1);
    let forecast = EnemyProductionForecastTrace {
        expected_units_next_turn: expected_units,
        expected_cost_next_turn: expected_cost,
        dominant_unit_type,
        evaluated_samples: state.evaluated_samples,
        mean_absolute_unit_error: state.absolute_unit_error_sum / samples,
        mean_absolute_cost_error: state.absolute_cost_error_sum / samples,
    };
    world.insert_resource(estimator);
    forecast
}

/// 敵施設が作戦地点へ到達させられる将来戦力の、用途別の見積り。
///
/// `expected_funds` は継続生産の必要量を作るための通常scenario、
/// `stress_funds` は敵が施設・収入をより強く使った場合の再計画監視用である。
/// 最小の進撃Goはこの値を読まない。Goを楽観的に保つことと、増援を作り続けることを
/// 同じ閾値にしないためである。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::ai::v4) struct EnemyReinforcementEnvelope {
    pub(in crate::ai::v4) expected_funds: u32,
    pub(in crate::ai::v4) stress_funds: u32,
}

/// 敵施設も期限内に到着できる最寄り作戦へ一意に割り当て、全収入の重複計上を防ぐ。
pub(in crate::ai::v4) fn projected_enemy_reinforcement_envelope(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    anchors: &[GridPosition],
    horizons: &[u32],
    anchor_index: usize,
) -> EnemyReinforcementEnvelope {
    if scan.enemy_production_slots == 0 {
        return EnemyReinforcementEnvelope::default();
    }
    let mut local_slots = 0_u32;
    let mut production_capacity = 0_u32;
    let mut available_slot_turns = 0_u32;
    let mut expected_wave_capacity = 0_u32;
    for facility in &scan.enemy_facilities {
        let assignment = anchors
            .iter()
            .enumerate()
            .filter_map(|(index, anchor)| {
                // 編成中でまだ前進を許可していない首都攻略は、敵増援の配分先ではない。
                // ここへ距離0で全枠を吸わせると、実際に占領中の中央島を「増援なし」と
                // 誤認して占領兵だけを送り続けるため、実行中の局地作戦を先に評価する。
                let is_unauthorized_capital_formation = scan
                    .campaign_objectives
                    .iter()
                    .find(|objective| objective.anchor == *anchor)
                    .is_some_and(|objective| {
                        objective.kind == OperationKind::AssaultCapital
                            && !objective.execution_authorized
                    });
                if is_unauthorized_capital_formation {
                    return None;
                }
                let (eta, cost) = enemy_facility_arrival(scan, ctx, *facility, *anchor)?;
                (eta <= horizons.get(index).copied().unwrap_or(0)).then_some((eta, index, cost))
            })
            .min_by_key(|(eta, index, _)| (*eta, *index));
        let Some((eta, assigned_index, unit_cost)) = assignment else {
            continue;
        };
        if assigned_index != anchor_index {
            continue;
        }
        let production_turns = horizons[anchor_index].saturating_sub(eta);
        if production_turns == 0 {
            continue;
        }
        local_slots = local_slots.saturating_add(1);
        available_slot_turns = available_slot_turns.saturating_add(production_turns);
        production_capacity =
            production_capacity.saturating_add(unit_cost.saturating_mul(production_turns));
        // Expectedでは将来全turnではなく、次の生産波にこの施設が出せる代表unitの
        // 価格だけを使う。次波以降は次手番の盤面・観測で再計画する。
        expected_wave_capacity = expected_wave_capacity.saturating_add(unit_cost);
    }
    if local_slots == 0 {
        return EnemyReinforcementEnvelope::default();
    }
    let allocated_income_per_turn = u64::from(scan.enemy_income)
        .saturating_mul(u64::from(local_slots))
        / u64::from(scan.enemy_production_slots.max(1));
    let income_capacity =
        allocated_income_per_turn.saturating_mul(u64::from(horizons[anchor_index]));
    // 敵工場数×期限は物理上限に過ぎない。収入で賄えない額をstress scenarioへ
    // 混ぜないよう先に物理・資金両方で上限を固定する。
    let stress_capacity =
        production_capacity.min(u32::try_from(income_capacity).unwrap_or(u32::MAX));
    // 実観測をExpectedへ反映する。一方、初回観測は開幕配置と区別できないため
    // forecastが0になる。そこで次の生産波の施設価格を事前分布として下限にし、
    // 未観測=敵が生産しない、という誤った停止を防ぐ。
    let observed_wave_capacity = u64::from(scan.enemy_production_forecast.expected_cost_next_turn)
        .saturating_mul(u64::from(local_slots))
        / u64::from(scan.enemy_production_slots.max(1));
    let prior_wave_capacity =
        expected_wave_capacity.min(u32::try_from(allocated_income_per_turn).unwrap_or(u32::MAX));
    let expected_capacity = u32::try_from(observed_wave_capacity)
        .unwrap_or(u32::MAX)
        .max(prior_wave_capacity)
        .min(stress_capacity);
    EnemyReinforcementEnvelope {
        expected_funds: expected_capacity,
        stress_funds: stress_capacity,
    }
}

/// Expectedは継続生産で倒す対象、Stressは予備counterと再計画の対象として扱う。
/// どちらも進撃Goを決めないため、最小のGo条件を強めずに生産だけを強くできる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ai::v4) enum ReinforcementScenario {
    Expected,
    Stress,
}

#[cfg(test)]
mod tests;
