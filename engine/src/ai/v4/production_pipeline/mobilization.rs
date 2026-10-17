//! 手番内の生産判断に必要な補助処理。

#[allow(unused_imports)]
use crate::ai::planning::decision::AdoptedTurnPortfolio;

pub(in crate::ai::v4) fn mobilization_role(
    stats: &UnitStats,
    has_unresolved_live_threat: bool,
    op: &Operation,
    damage_chart: &DamageChart,
) -> SlotKind {
    let selected_for_engagement = has_unresolved_live_threat
        && op
            .reachable_threats
            .iter()
            .any(|threat| best_damage(damage_chart, stats.unit_type, threat.stats.unit_type) > 0);
    if stats.can_capture && !selected_for_engagement {
        SlotKind::Capture
    } else {
        SlotKind::Combat
    }
}
use super::*;
pub(super) fn allocate_surplus_mobilization(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
    surplus: &SurplusBudget,
) {
    let ProductionInputs {
        scan, player_id, ..
    } = *input;
    let ProductionState {
        ctx,
        operations,
        plan_trace,
        allocation,
        commands,
        ..
    } = state;
    let mut immediate_reinforcement_funds = surplus.funds;
    let resolved_by_rolling_plan = &surplus.resolved;
    // 空き施設があり、Must予約・予備費を除いた余剰資金が残っている場合は、
    // 工場枠の遊休化を防ぎ前線圧・戦線維持を継続するため、前線作戦へ向けて動員生産を行う。
    while immediate_reinforcement_funds >= 1000 {
        let free_slots =
            allocation.free_facility_count(scan.free_facilities.iter().map(|(pos, _)| pos));
        if free_slots == 0 {
            break;
        }
        let Some((op_index, op)) = operations
            .iter()
            .enumerate()
            .filter(|(_, op)| {
                op.kind == OperationKind::Capture || op.kind == OperationKind::AssaultCapital
            })
            .min_by_key(|(_, op)| operation_priority_rank(op, input.policy))
        else {
            break;
        };
        // Rolling Planで排除済みなら、未所有物件へ向かえる占領役だけを追加する。
        // 交戦相手がいないのに航空・戦車まで残額で埋めると、次手番に必要となる
        // 相性counterの資金と生産口を失う。輸送未成立の前線には単独の地上兵も出さない。
        if op.facts.requires_transport {
            break;
        }
        let has_unresolved_live_threat = op.reachable_threats.iter().any(|threat| {
            threat.available_turn == 0
                && threat
                    .entity
                    .is_some_and(|entity| !resolved_by_rolling_plan.contains(&entity))
        });

        let best_mobilization = select_mobilization_candidate(
            input,
            ctx,
            op,
            allocation,
            has_unresolved_live_threat,
            immediate_reinforcement_funds,
            free_slots,
        );
        let Some(candidate) = best_mobilization else {
            break;
        };
        // 動員購入の役割は、価格ではなく選定理由から購入案に一度だけ決める。
        // 未解決の実在脅威へ有効打を持つことを理由に選んだ兵はCombat、
        // それ以外で占領能力を持つ兵は前線物件へ向かうCapture、残りはCombat。
        let role = mobilization_role(
            candidate_stats(scan, &candidate),
            has_unresolved_live_threat,
            op,
            &scan.damage_chart,
        );

        let deployment =
            planned_deployment(scan, ctx, &operations[op_index], role, &candidate, None);

        if allocation
            .try_claim(
                candidate.facility,
                candidate.cost,
                operation_priority_rank(&operations[op_index], input.policy),
            )
            .is_err()
        {
            // 候補評価後に競合した予約は、後続の意図や工程へ反映せず配分を止める。
            break;
        }
        immediate_reinforcement_funds =
            immediate_reinforcement_funds.saturating_sub(candidate.cost);

        record_surplus_purchase(
            &operations[op_index],
            player_id,
            plan_trace,
            commands,
            SurplusPurchase {
                candidate,
                role,
                purpose: PurchasePurpose::Mobilization,
                deployment,
                remaining_funds_before: allocation.remaining_funds().saturating_add(candidate.cost),
            },
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn select_mobilization_candidate(
    input: &ProductionInputs<'_>,
    ctx: &mut ReachCtx,
    op: &Operation,
    allocation: &AllocationState,
    has_unresolved_live_threat: bool,
    immediate_reinforcement_funds: u32,
    free_slots: usize,
) -> Option<SlotCandidate> {
    let scan = input.scan;
    let is_small_map = scan.opening_policy == Some(OpeningProductionPolicy::SmallMapExpansion);
    let per_slot_budget = immediate_reinforcement_funds / free_slots as u32;
    let mut best_mobilization: Option<SlotCandidate> = None;
    for (facility, terrain) in &scan.free_facilities {
        if allocation.is_facility_used(facility) {
            continue;
        }
        for (unit_type, stats) in &scan.available_types {
            let allow_combat_unit = has_unresolved_live_threat || is_small_map;
            if stats.cost == 0
                || stats.cost > immediate_reinforcement_funds
                || (!allow_combat_unit && !stats.can_capture)
                || !scan.can_produce(*terrain, *unit_type)
                || !can_join_operation(
                    scan,
                    ctx,
                    &op.anchor,
                    op.facts.requires_transport,
                    facility,
                    stats,
                )
            {
                continue;
            }
            let candidate = SlotCandidate {
                unit_type: *unit_type,
                cost: stats.cost,
                facility: *facility,
                fitness: 1.0,
            };
            let ranking = MobilizationRanking {
                funds: immediate_reinforcement_funds,
                per_slot_budget,
                has_unresolved_live_threat,
                is_small_map,
            };
            let key = mobilization_key(
                scan,
                op,
                &candidate,
                stats.max_movement,
                stats.unit_type,
                ranking,
            );
            if best_mobilization.as_ref().is_none_or(|current| {
                let movement = scan
                    .available_types
                    .iter()
                    .find(|(t, _)| *t == current.unit_type)
                    .map_or(0, |(_, stats)| stats.max_movement);
                key < mobilization_key(scan, op, current, movement, current.unit_type, ranking)
            }) {
                best_mobilization = Some(candidate);
            }
        }
    }

    best_mobilization
}

#[derive(Clone, Copy)]
struct MobilizationRanking {
    funds: u32,
    per_slot_budget: u32,
    has_unresolved_live_threat: bool,
    is_small_map: bool,
}
type MobilizationKey = (
    bool,
    bool,
    std::cmp::Reverse<u32>,
    std::cmp::Reverse<u32>,
    u32,
    usize,
    usize,
);
/// 有効打、予算適合、機動力、費用・座標の順を両候補へ同じ定義で適用する。
fn mobilization_key(
    scan: &BoardScan,
    op: &Operation,
    candidate: &SlotCandidate,
    movement: u32,
    damage_type: UnitType,
    ranking: MobilizationRanking,
) -> MobilizationKey {
    let MobilizationRanking {
        funds,
        per_slot_budget,
        has_unresolved_live_threat,
        is_small_map,
    } = ranking;
    let fits_budget = if has_unresolved_live_threat || is_small_map {
        candidate.cost <= funds
    } else {
        candidate.cost <= per_slot_budget.max(1000)
    };
    let damage = if has_unresolved_live_threat {
        op.reachable_threats
            .iter()
            .map(|threat| best_damage(&scan.damage_chart, damage_type, threat.stats.unit_type))
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let has_damage = !has_unresolved_live_threat || damage > 0;
    let movement_rank = if is_small_map {
        std::cmp::Reverse(movement)
    } else {
        std::cmp::Reverse(0)
    };
    (
        !has_damage,
        !fits_budget,
        std::cmp::Reverse(damage),
        movement_rank,
        candidate.cost,
        candidate.facility.y,
        candidate.facility.x,
    )
}
