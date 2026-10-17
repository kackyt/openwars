//! RollingPlanのfacility_assignment責務。
use super::role_multiset::ProductionRole;
use super::{
    GridPosition, HashMap, ProductionPlanOption, ProductionSlot, RollingPlanInput, SearchState,
};

/// ある施設に特定の役割を割り当てた際のコスト（所要ターン数・距離）。
/// 小さいほど優秀な割当。鈍足ユニット（歩兵等）を前線工場に、俊足ユニットを後方工場に
/// 自然に誘導する。
pub(super) fn calculate_assignment_cost(
    input: &RollingPlanInput,
    opt: &ProductionPlanOption,
    opt_idx: usize,
    fac_pos: GridPosition,
) -> u64 {
    if let Some(target) = opt.capture_target {
        let arrival = opt.capture_arrival_turn.unwrap_or(99) as u64;
        let completion = opt.capture_completion_turn.unwrap_or(99) as u64;
        let dist = input.map.distance(fac_pos.x, fac_pos.y, target.x, target.y) as u64;
        arrival * 10_000 + completion * 1_000 + dist
    } else {
        let speed = opt.stats.max_movement.max(1) as u64;
        let proj_earliest = input
            .production_attack_projections
            .get(opt_idx)
            .and_then(|projs| projs.iter().flatten().map(|p| p.ready_turn).min());

        let target_pos = input
            .enemies
            .iter()
            .min_by_key(|e| {
                input
                    .map
                    .distance(fac_pos.x, fac_pos.y, e.position.x, e.position.y)
            })
            .map(|e| e.position)
            .or_else(|| {
                input
                    .production_options
                    .iter()
                    .filter_map(|o| o.capture_target)
                    .min_by_key(|t| input.map.distance(fac_pos.x, fac_pos.y, t.x, t.y))
            });
        let dist = target_pos
            .map(|t| input.map.distance(fac_pos.x, fac_pos.y, t.x, t.y) as u64)
            .unwrap_or(0);

        if let Some(ready_turn) = proj_earliest {
            (ready_turn as u64) * 10_000 + dist
        } else {
            let eta = dist.div_ceil(speed);
            eta * 10_000 + dist
        }
    }
}

/// 単一の部隊編成（マルチセット）に対し、各役割をどの生産施設に配置するのが
/// 最適（ETAおよび移動距離の総和が最小）かを解き、SearchStateを構築する。
pub(super) fn optimize_multiset_facility_assignment(
    input: &RollingPlanInput,
    multiset: &[ProductionRole],
    facilities: &[(GridPosition, Vec<usize>)],
    facility_role_options: &HashMap<(GridPosition, ProductionRole), usize>,
) -> Option<SearchState> {
    let m = multiset.len();
    let n = facilities.len();
    if m > n {
        return None;
    }

    let mut cost_matrix = Vec::with_capacity(m);
    for role in multiset {
        let mut row = Vec::with_capacity(n);
        for (fac_pos, _) in facilities {
            if let Some(&opt_idx) = facility_role_options.get(&(*fac_pos, *role)) {
                let opt = &input.production_options[opt_idx];
                let cost = calculate_assignment_cost(input, opt, opt_idx, *fac_pos);
                row.push(Some(cost));
            } else {
                row.push(None);
            }
        }
        if row.iter().all(|c| c.is_none()) {
            return None;
        }
        cost_matrix.push(row);
    }

    // 割当は多項式時間のソルバ（assignment.rs）へ委譲する。同点時は施設indexが
    // 辞書順最小の解が返るため、旧・分枝限定DFSと同じ生産施設が選ばれる。
    let best_assignment = super::super::assignment::solve_min_cost_assignment(&cost_matrix, n)?;

    let mut state = SearchState::default();
    for (role_i, &fac_idx) in best_assignment.iter().enumerate() {
        let fac_pos = facilities[fac_idx].0;
        let opt_idx = facility_role_options[&(fac_pos, multiset[role_i])];
        let option = &input.production_options[opt_idx];
        state.option_indices.push(opt_idx);
        state.used_slots.insert(ProductionSlot {
            facility: fac_pos,
            build_turn: 0,
        });
        if let Some(target) = option.capture_target {
            state.used_capture_targets.insert(target);
        }
        state.cost = state.cost.saturating_add(option.purchase.cost);
    }
    Some(state)
}
