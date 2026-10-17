//! RollingPlanのopening_package責務。
use super::model::{PlannedCapturePurchase, PlannedCombatPurchase};
use super::{
    ForcePackagePlan, HashMap, HashSet, RollingPlanInput, SearchState, TargetForecast, best_damage,
};

/// 購入済み候補を、敵一体への最初の一発だけを割り当てて評価する。
/// 同じunitは同じ手番に二つの敵へ撃てないが、別手番なら別の敵を止められる。
#[allow(dead_code)]
pub(super) fn evaluate_opening_package_static(
    input: &RollingPlanInput,
    state: &SearchState,
) -> ForcePackagePlan {
    let mut purchases = state
        .option_indices
        .iter()
        .map(|index| input.production_options[*index].purchase)
        .collect::<Vec<_>>();
    purchases.sort_unstable_by_key(|purchase| {
        (
            purchase.build_turn,
            purchase.facility.y,
            purchase.facility.x,
        )
    });

    let capture_purchases = state
        .option_indices
        .iter()
        .filter_map(|index| {
            let option = &input.production_options[*index];
            Some(PlannedCapturePurchase {
                purchase: option.purchase,
                target: option.capture_target?,
                completion_turn: option.capture_completion_turn,
                blocking_enemy_index: option.capture_blocking_enemy_index,
                // 初動にはまだ敵の反撃手番が無い。生存判定は次手番に実盤面で行う。
                survived_at_completion: true,
            })
        })
        .collect::<Vec<_>>();

    // 明示的な妨害契約に加え、占領レーンを塞ぐ敵には占領完了時刻までの初撃を要求する。
    let mut contract_deadlines = HashMap::<usize, u32>::default();
    for (enemy_index, deadline) in &input.interdiction_deadlines {
        contract_deadlines
            .entry(*enemy_index)
            .and_modify(|current| *current = (*current).min(*deadline))
            .or_insert(*deadline);
    }
    for capture in &capture_purchases {
        if let (Some(enemy_index), Some(deadline)) =
            (capture.blocking_enemy_index, capture.completion_turn)
        {
            contract_deadlines
                .entry(enemy_index)
                .and_modify(|current| *current = (*current).min(deadline))
                .or_insert(deadline);
        }
    }
    let mut contracts = contract_deadlines.into_iter().collect::<Vec<_>>();
    contracts.sort_unstable_by_key(|(enemy_index, deadline)| (*deadline, *enemy_index));

    let mut used_unit_turns = HashSet::default();
    let mut target_first_attack_turns = vec![None; input.enemies.len()];
    let mut target_damage = vec![0_u32; input.enemies.len()];
    let mut combat_purchases = Vec::new();
    let mut assigned_units = HashSet::default();
    for (enemy_index, deadline) in contracts {
        let mut best: Option<(u32, std::cmp::Reverse<u32>, u32, usize, u32)> = None;
        for option_index in &state.option_indices {
            let option = &input.production_options[*option_index];
            if option.capture_target.is_some() {
                continue;
            }
            let Some(projection) = input
                .production_attack_projections
                .get(*option_index)
                .and_then(|projections| projections.get(enemy_index))
                .and_then(|projection| *projection)
            else {
                continue;
            };
            let damage = best_damage(
                &input.damage_chart,
                option.stats.unit_type,
                input.enemies[enemy_index].stats.unit_type,
            );
            if damage == 0 || projection.ready_turn > deadline {
                continue;
            }
            for turn in projection.ready_turn..=deadline {
                if used_unit_turns.contains(&(*option_index, turn)) {
                    continue;
                }
                let candidate = (
                    turn,
                    std::cmp::Reverse(damage),
                    option.purchase.cost,
                    *option_index,
                    damage,
                );
                if best.as_ref().is_none_or(|current| candidate < *current) {
                    best = Some(candidate);
                }
                break;
            }
        }
        if let Some((turn, _, _, option_index, damage)) = best {
            used_unit_turns.insert((option_index, turn));
            target_first_attack_turns[enemy_index] = Some(turn);
            target_damage[enemy_index] = target_damage[enemy_index].saturating_add(damage);
            if assigned_units.insert(option_index)
                && let Some(entity) = input.enemies[enemy_index].entity
            {
                combat_purchases.push(PlannedCombatPurchase {
                    purchase: input.production_options[option_index].purchase,
                    target: entity,
                });
            }
        }
    }

    let target_forecasts = input
        .enemies
        .iter()
        .enumerate()
        .map(|(index, enemy)| {
            let remaining_hp = enemy.hp.saturating_sub(target_damage[index]);
            TargetForecast {
                entity: enemy.entity,
                unit_type: enemy.stats.unit_type,
                available_turn: enemy.available_turn,
                initial_hp: enemy.hp,
                remaining_hp,
                destroyed_turn: (remaining_hp == 0)
                    .then_some(target_first_attack_turns[index].unwrap_or(u32::MAX)),
            }
        })
        .collect::<Vec<_>>();
    let first_attack_turn = target_first_attack_turns.iter().flatten().copied().min();
    let elimination_turn = target_forecasts
        .iter()
        .all(|target| target.destroyed_turn.is_some())
        .then(|| {
            target_forecasts
                .iter()
                .filter_map(|target| target.destroyed_turn)
                .max()
                .unwrap_or_default()
        });
    let occupation_turn = capture_purchases
        .iter()
        .map(|capture| capture.completion_turn)
        .collect::<Option<Vec<_>>>()
        .filter(|_| capture_purchases.len() >= input.required_capture_survivors)
        .map(|turns| turns.into_iter().max().unwrap_or_default());
    let deadline_capture_survivor_count = capture_purchases
        .iter()
        .filter(|capture| {
            capture
                .completion_turn
                .is_some_and(|turn| input.hard_deadline.is_none_or(|deadline| turn <= deadline))
        })
        .count()
        .min(input.required_capture_survivors);
    let surviving_combat_value = purchases.iter().map(|purchase| purchase.cost).sum();
    let protected_unit_count = capture_purchases.len();

    ForcePackagePlan {
        purchases,
        capture_purchases,
        combat_purchases,
        target_forecasts,
        turn_forecasts: Vec::new(),
        feasible: false,
        first_attack_turn,
        front_breakthrough_turn: elimination_turn,
        deadline_target_first_attack_turn: input
            .deadline_target_index
            .and_then(|index| target_first_attack_turns.get(index).and_then(|turn| *turn)),
        target_first_attack_turns,
        elimination_turn,
        occupation_turn,
        production_cost: state.cost,
        expected_loss: 0,
        blocked_next_turn_production_slots: 0,
        direct_fire_lane_count: 0,
        direct_front_shortfall: 0,
        direct_enemy_remaining_hp: Vec::new(),
        direct_damage_by_target: Vec::new(),
        existing_indirect_damage_by_target: Vec::new(),
        produced_indirect_damage_by_target: Vec::new(),
        surviving_combat_value,
        required_overmatch_value: 0,
        overmatch_ready: false,
        protected_unit_count,
        protected_survivor_count: deadline_capture_survivor_count,
        deadline_capture_survivor_count,
        required_capture_survivor_count: input.required_capture_survivors,
        candidates_considered: 0,
        candidates_pruned: 0,
        search_truncated: false,
    }
}
