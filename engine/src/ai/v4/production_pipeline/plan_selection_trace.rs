//! 計画置換の理由を特定するための明示的な局面診断。通常実行では評価も出力もしない。
use super::*;
use serde_json::{Value, json};

fn enabled(turn: u32) -> bool {
    std::env::var("OPENWARS_PLAN_SELECTION_TRACE_TURN")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        == Some(turn)
}

/// 作戦の生成源・並存・優先順を、購入前の同じsnapshotから照合する。
pub(super) fn record_operations(
    input: &ProductionInputs<'_>,
    state: &ProductionState,
    registry: &V4RollingPlanRegistry,
) {
    if !enabled(input.turn) {
        return;
    }
    eprintln!(
        "PRODUCTION_OPERATIONS {}",
        json!({
            "turn": input.turn, "player": input.player_id.0, "funds": input.scan.funds,
            "campaign_sources": format!("{:?}", input.scan.campaign_objectives),
            "active_sources": format!("{:?}", registry.active_objectives(input.player_id)),
            "operations": state.operations.iter().enumerate().map(|(index, op)| json!({
                "index": index, "kind": format!("{:?}", op.kind),
                "anchor": [op.anchor.x, op.anchor.y], "island": format!("{:?}", op.island_id),
                "priority": operation_priority_rank(op, input.policy),
                "requested": format!("{:?}", input.policy.allocation_slots(op, index, &state.ledger)),
                "filled": format!("{:?}", op.filled), "execution_authorized": op.execution_authorized,
                "contingency": op.contingency_reserve_funds,
            })).collect::<Vec<_>>(),
        })
    );
}

/// 全作戦の配分後、発行前に確定した兵種・施設・工程参照を記録する。
pub(super) fn record_orders(input: &ProductionInputs<'_>, commands: &[PlannedProduction]) {
    if !enabled(input.turn) {
        return;
    }
    eprintln!(
        "PRODUCTION_ORDERS {}",
        json!({
            "turn": input.turn, "player": input.player_id.0,
            "orders": commands.iter().map(|p| json!({
                "unit": p.command.unit_type.as_str(), "facility": [p.command.target_x, p.command.target_y],
                "cost": p.cost, "role": format!("{:?}", p.role), "purpose": format!("{:?}", p.purpose),
            })).collect::<Vec<_>>(),
        })
    );
}

fn purchases(items: &[rolling_plan::PlannedPurchase]) -> Value {
    json!(
        items
            .iter()
            .map(|p| json!({
                "unit": p.unit_type.as_str(), "cost": p.cost, "build_turn": p.build_turn,
                "facility": [p.facility.x, p.facility.y],
            }))
            .collect::<Vec<_>>()
    )
}

fn plan_summary(plan: &rolling_plan::ForcePackagePlan) -> Value {
    json!({
        "purchases": purchases(&plan.purchases), "feasible": plan.feasible,
        "first_attack": plan.first_attack_turn, "elimination": plan.elimination_turn,
        "last_simulated_turn": plan.turn_forecasts.last().map(|f| f.turn),
        "occupation": plan.occupation_turn, "cost": plan.production_cost,
        "loss": plan.expected_loss, "overmatch_ready": plan.overmatch_ready,
        "capture_survivors": plan.deadline_capture_survivor_count,
        "required_capture_survivors": plan.required_capture_survivor_count,
        "remaining_hp": plan.target_forecasts.iter().map(|t| t.remaining_hp).sum::<u32>(),
        "targets": plan.target_forecasts.iter().map(|t| json!({
            "entity": t.entity.map(|e| e.to_bits()), "unit": t.unit_type.as_str(),
            "initial_hp": t.initial_hp, "remaining_hp": t.remaining_hp, "destroyed": t.destroyed_turn,
        })).collect::<Vec<_>>(),
    })
}

/// 継続案が評価経路へ渡る直前の入力を保存する。破棄される継続案も診断時だけ再評価する。
pub(super) fn snapshot(
    input: &ProductionInputs<'_>,
    operation: &Operation,
    combat: &RollingPlanInput,
    previous: Option<&plan_revision::PlanContinuation>,
) -> Option<Value> {
    let turn = std::env::var("OPENWARS_PLAN_SELECTION_TRACE_TURN")
        .ok()?
        .parse::<u32>()
        .ok()?;
    if turn != input.turn {
        return None;
    }
    let fixed =
        previous.map(
            |previous| match evaluate_fixed_package(combat, &previous.purchases) {
                Ok(plan) => json!({"ok": plan_summary(&plan)}),
                Err(error) => json!({"error": format!("{error:?}")}),
            },
        );
    Some(json!({
        "turn": input.turn, "player": input.player_id.0,
        "kind": format!("{:?}", operation.kind),
        "anchor": [operation.anchor.x, operation.anchor.y],
        "island": format!("{:?}", operation.island_id),
        "exact_property_control": combat.exact_property_control,
        "funds": combat.current_funds, "income": combat.income_per_turn,
        "hard_deadline": combat.hard_deadline,
        "hold_window": combat.hold_window, "hold_contestants": combat.hold_contestants,
        "capture_completion_turn": combat.capture_completion_turn,
        "required_capture_survivors": combat.required_capture_survivors,
        "economy_of_force": combat.economy_of_force,
        "frontage": rolling_plan::frontage_diagnostic(combat),
        "positional": combat.positional, "defensive_emplacement": combat.defensive_emplacement,
        "existing": combat.existing_units.iter().map(|u| json!({
            "unit": u.stats.unit_type.as_str(), "position": [u.position.x,u.position.y],
            "hp": u.hp, "available": u.available_turn, "targets": u.engageable_enemy_indices,
        })).collect::<Vec<_>>(),
        "enemies": combat.enemies.iter().map(|u| json!({
            "entity": u.entity.map(|e| e.to_bits()), "unit": u.stats.unit_type.as_str(), "position": [u.position.x,u.position.y],
            "hp": u.hp, "available": u.available_turn,
        })).collect::<Vec<_>>(),
        // 追加購入ゼロでも保持が成立するかを同じ入力で診断する。
        "no_purchase_diagnostic": match evaluate_fixed_package(combat, &[]) {
            Ok(plan) => json!({"ok": plan_summary(&plan)}),
            Err(error) => json!({"error": format!("{error:?}")}),
        },
        "max_option_turn": combat.production_options.iter().map(|o| o.purchase.build_turn).max(),
        // 現行購入だけなら成立するかを分けて測る。診断結果を実際の選択には渡さない。
        "current_only_diagnostic": previous.map(|p| {
            let current = p.purchases.iter().copied().filter(|purchase| purchase.build_turn == 0).collect::<Vec<_>>();
            match evaluate_fixed_package(combat, &current) {
                Ok(plan) => json!({"ok": plan_summary(&plan)}),
                Err(error) => json!({"error": format!("{error:?}")}),
            }
        }),
        "candidate_types": input.scan.available_types.iter()
            .filter(|(kind,_)| matches!(kind, UnitType::Recon | UnitType::MdTank | UnitType::HeavySpGun | UnitType::Artillery))
            .map(|(kind, stats)| json!({"unit":kind.as_str(), "cost":stats.cost,
                "cargo":stats.max_cargo, "movement":format!("{:?}",stats.movement_type),
                "option_count": combat.production_options.iter().filter(|o|o.purchase.unit_type == *kind).count(),
                "producible_facilities": input.scan.free_facilities.iter().filter(|(_,terrain)| input.scan.can_produce(*terrain,*kind)).count(),
            })).collect::<Vec<_>>(),
        "committed_purchases": purchases(&combat.committed_purchases),
        "missing_original_options": previous.map(|p| p.purchases.iter()
            .filter(|purchase| !combat.production_options.iter().any(|o| o.purchase == **purchase))
            .map(|purchase| json!({
                "purchase": purchases(&[*purchase]),
                "same_type_options": purchases(&combat.production_options.iter()
                    .filter(|o| o.purchase.unit_type == purchase.unit_type)
                    .map(|o| o.purchase).collect::<Vec<_>>()),
            })).collect::<Vec<_>>()),
        "previous": previous.map(|p| json!({
            "id": format!("{:?}", p.plan_id), "revision": format!("{:?}", p.revision),
            "production_failed": p.production_failed, "execution_delayed": p.execution_delayed,
            "formation_in_progress": p.formation_in_progress,
            "purchases": purchases(&p.purchases),
        })),
        "fixed_evaluation": fixed,
    }))
}

/// 指定兵種の候補有無と、同兵種を1体・2体買う案の評価を全候補施設で照合する。
/// 混成案を含む本探索の代わりではなく、候補除外と評価差を切り分けるための診断。
fn defense_type_diagnostic(combat: &RollingPlanInput) -> Value {
    let mut rows = Vec::new();
    for kind in [
        UnitType::Recon,
        UnitType::MdTank,
        UnitType::HeavySpGun,
        UnitType::Artillery,
    ] {
        let options = combat
            .production_options
            .iter()
            .filter(|o| o.purchase.unit_type == kind)
            .map(|o| o.purchase)
            .collect::<Vec<_>>();
        let mut best: [Option<rolling_plan::ForcePackagePlan>; 2] = std::array::from_fn(|_| None);
        let mut evaluated = [0_u32; 2];
        let mut feasible = [0_u32; 2];
        let key = |p: &rolling_plan::ForcePackagePlan| {
            (
                !p.feasible,
                p.target_forecasts
                    .iter()
                    .map(|t| t.remaining_hp)
                    .sum::<u32>(),
                p.expected_loss,
                p.first_attack_turn.unwrap_or(u32::MAX),
                p.production_cost,
            )
        };
        let mut consider = |purchases: &[rolling_plan::PlannedPurchase]| {
            let bucket = purchases.len() - 1;
            if let Ok(plan) = evaluate_fixed_package(combat, purchases) {
                evaluated[bucket] += 1;
                feasible[bucket] += u32::from(plan.feasible);
                if best[bucket]
                    .as_ref()
                    .is_none_or(|current| key(&plan) < key(current))
                {
                    best[bucket] = Some(plan);
                }
            }
        };
        for (i, purchase) in options.iter().enumerate() {
            consider(&[*purchase]);
            for other in &options[i + 1..] {
                if purchase.facility != other.facility || purchase.build_turn != other.build_turn {
                    consider(&[*purchase, *other]);
                }
            }
        }
        rows.push(json!({
            "unit": kind.as_str(), "option_count": options.len(),
            "options": purchases(&options),
            "single": {"evaluated": evaluated[0], "feasible": feasible[0], "best":best[0].as_ref().map(plan_summary)},
            "pair": {"evaluated": evaluated[1], "feasible": feasible[1], "best":best[1].as_ref().map(plan_summary)},
        }));
    }
    json!(rows)
}

/// 選択結果を入力と同じ行に記録し、別作戦・別revisionとの混同を避ける。
pub(super) fn record(snapshot: Option<Value>, selected: &SelectedPlan, combat: &RollingPlanInput) {
    let Some(mut row) = snapshot else {
        return;
    };
    // 防衛が選んだ列を何件まで買えば成立するか、同じ入力で前半列だけを再評価する。
    if row["kind"] == "Defense" {
        if std::env::var_os("OPENWARS_DEFENSE_TYPE_DIAGNOSTIC").is_some() {
            row["type_diagnostic"] = defense_type_diagnostic(combat);
        }
        row["selected_prefix_diagnostic"] = json!(
            (0..=selected.plan.purchases.len())
                .map(|count| {
                    let result =
                        match evaluate_fixed_package(combat, &selected.plan.purchases[..count]) {
                            Ok(plan) => json!({"ok": plan_summary(&plan)}),
                            Err(error) => json!({"error": format!("{error:?}")}),
                        };
                    json!({"purchase_count": count, "result": result})
                })
                .collect::<Vec<_>>()
        );
    }
    row["selected"] = json!({
        "id": format!("{:?}", selected.plan_id),
        "revision": format!("{:?}", selected.revision),
        "disposition": format!("{:?}", selected.disposition),
        "reason": format!("{:?}", selected.reason),
        "plan": plan_summary(&selected.plan),
    });
    eprintln!("PLAN_SELECTION {row}");
}
