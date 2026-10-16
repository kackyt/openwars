//! 購入列の最終施設を、命令・工程・意図・診断へ一度に確定する。
//!
//! 施設交換の判断は従来の拡張経路にだけ適用する。交換前の購入対応を先に保存し、
//! 同型の購入や施設の相互交換でも、診断を別の購入へ付け替えない。

use super::*;

/// 配分時の対応を保存してから施設を確定する。予測や交換条件は変更しない。
pub(super) fn finalize(
    scan: &BoardScan,
    commands: &mut [PlannedProduction],
    trace: &mut ProductionPlanTrace,
    registry: &mut V4RollingPlanRegistry,
    optimize: bool,
) {
    let produced_steps = trace
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| {
            matches!(
                step.decision,
                ProductionDecision::Produced { .. }
                    | ProductionDecision::ProducedImmediateReinforcement { .. }
            )
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(
        produced_steps.len(),
        commands.len(),
        "購入案とtraceの件数が異なる"
    );
    // 添字はこの購入列の確定中だけ有効。工程参照と交換前の施設を併用し、兵種だけで照合しない。
    let rolling_links = commands
        .iter()
        .zip(&produced_steps)
        .map(|(planned, &step_index)| {
            let step = &trace.steps[step_index];
            trace
                .rolling_combat_plans
                .iter()
                .enumerate()
                .filter(|(_, plan)| {
                    plan.operation_kind == step.operation_kind
                        && plan.anchor == step.operation_anchor
                })
                .flat_map(|(plan_index, plan)| {
                    plan.purchases.iter().enumerate().filter_map(
                        move |(purchase_index, purchase)| {
                            let same_purchase = purchase.build_turn == 0
                                && purchase.unit_type == planned.command.unit_type
                                && purchase.cost == planned.cost
                                && purchase.facility == facility(planned);
                            let same_plan = match planned.purpose {
                                PurchasePurpose::PlanStep(reference) => {
                                    plan.plan_id == Some(reference.plan_id)
                                        && plan.revision == Some(reference.revision)
                                }
                                _ => plan.plan_id.is_none(),
                            };
                            (same_purchase && same_plan).then_some((plan_index, purchase_index))
                        },
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    if optimize {
        optimize_production_facility_assignments(scan, commands);
    }
    commit_facilities(commands, trace, registry, &produced_steps, &rolling_links);
}

fn facility(planned: &PlannedProduction) -> GridPosition {
    GridPosition {
        x: planned.command.target_x,
        y: planned.command.target_y,
    }
}

/// 命令を正本にして同じ購入の投影を更新する。pending配備はこの後の共通commitで生成する。
fn commit_facilities(
    commands: &mut [PlannedProduction],
    trace: &mut ProductionPlanTrace,
    registry: &mut V4RollingPlanRegistry,
    produced_steps: &[usize],
    rolling_links: &[Vec<(usize, usize)>],
) {
    for ((planned, &step_index), links) in
        commands.iter_mut().zip(produced_steps).zip(rolling_links)
    {
        let selected = facility(planned);
        if let Some(intent) = planned.capture_intent.as_mut() {
            intent.command.target_x = selected.x;
            intent.command.target_y = selected.y;
        }
        if let PurchasePurpose::PlanStep(reference) = planned.purpose {
            registry.retarget_planned_step(reference, selected);
        }
        match &mut trace.steps[step_index].decision {
            ProductionDecision::Produced { facility, .. }
            | ProductionDecision::ProducedImmediateReinforcement { facility, .. } => {
                *facility = selected;
            }
            _ => unreachable!("購入trace以外を施設確定へ渡した"),
        }
        for &(plan_index, purchase_index) in links {
            trace.rolling_combat_plans[plan_index].purchases[purchase_index].facility = selected;
        }
    }
}

pub(in crate::ai::v4) fn optimize_production_facility_assignments(
    scan: &BoardScan,
    commands: &mut [PlannedProduction],
) {
    if commands.len() <= 1 {
        return;
    }
    let stats_by_type: HashMap<UnitType, &UnitStats> =
        scan.available_types.iter().map(|(t, s)| (*t, s)).collect();

    let mut improved = true;
    let mut iterations = 0;
    while improved && iterations < 32 {
        improved = false;
        iterations += 1;
        for i in 0..commands.len() {
            for j in (i + 1)..commands.len() {
                let target_i = commands[i]
                    .capture_intent
                    .as_ref()
                    .and_then(|intent| intent.mission_target)
                    .or_else(|| commands[i].deployment.as_ref().map(|d| d.staging_anchor));
                let target_j = commands[j]
                    .capture_intent
                    .as_ref()
                    .and_then(|intent| intent.mission_target)
                    .or_else(|| commands[j].deployment.as_ref().map(|d| d.staging_anchor));

                let (Some(target_i), Some(target_j)) = (target_i, target_j) else {
                    continue;
                };

                let pos_i = GridPosition {
                    x: commands[i].command.target_x,
                    y: commands[i].command.target_y,
                };
                let pos_j = GridPosition {
                    x: commands[j].command.target_x,
                    y: commands[j].command.target_y,
                };
                if pos_i == pos_j {
                    continue;
                }

                let terrain_i = scan
                    .map
                    .get_terrain(pos_i.x, pos_i.y)
                    .unwrap_or(Terrain::Factory);
                let terrain_j = scan
                    .map
                    .get_terrain(pos_j.x, pos_j.y)
                    .unwrap_or(Terrain::Factory);
                let unit_i = commands[i].command.unit_type;
                let unit_j = commands[j].command.unit_type;

                // 地形生産制約の確認
                if !scan.can_produce(terrain_j, unit_i) || !scan.can_produce(terrain_i, unit_j) {
                    continue;
                }

                let speed_i = stats_by_type
                    .get(&unit_i)
                    .map_or(1, |s| s.max_movement)
                    .max(1);
                let speed_j = stats_by_type
                    .get(&unit_j)
                    .map_or(1, |s| s.max_movement)
                    .max(1);

                let dist_i_curr = scan.map.distance(pos_i.x, pos_i.y, target_i.x, target_i.y);
                let dist_j_curr = scan.map.distance(pos_j.x, pos_j.y, target_j.x, target_j.y);
                let eta_i_curr = dist_i_curr.div_ceil(speed_i);
                let eta_j_curr = dist_j_curr.div_ceil(speed_j);
                let curr_key = (
                    eta_i_curr.saturating_add(eta_j_curr),
                    dist_i_curr.saturating_add(dist_j_curr),
                );

                let dist_i_swap = scan.map.distance(pos_j.x, pos_j.y, target_i.x, target_i.y);
                let dist_j_swap = scan.map.distance(pos_i.x, pos_i.y, target_j.x, target_j.y);
                let eta_i_swap = dist_i_swap.div_ceil(speed_i);
                let eta_j_swap = dist_j_swap.div_ceil(speed_j);
                let swap_key = (
                    eta_i_swap.saturating_add(eta_j_swap),
                    dist_i_swap.saturating_add(dist_j_swap),
                );

                if swap_key < curr_key {
                    commands[i].command.target_x = pos_j.x;
                    commands[i].command.target_y = pos_j.y;

                    commands[j].command.target_x = pos_i.x;
                    commands[j].command.target_y = pos_i.y;
                    improved = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(position: GridPosition) -> PlannedProduction {
        PlannedProduction {
            command: ProduceUnitCommand {
                player_id: PlayerId(0),
                target_x: position.x,
                target_y: position.y,
                unit_type: UnitType::Infantry,
            },
            role: SlotKind::Capture,
            cost: 1000,
            purpose: PurchasePurpose::OperationSlot,
            deployment: None,
            capture_intent: None,
        }
    }

    #[test]
    fn same_type_swaps_update_each_purchase_trace_in_original_order() {
        let left = GridPosition { x: 0, y: 0 };
        let right = GridPosition { x: 4, y: 0 };
        let mut commands = vec![command(right), command(left)];
        let mut trace = ProductionPlanTrace::new(PlayerId(0), 2000, 2);
        for position in [left, right] {
            trace.steps.push(ProductionStepTrace {
                operation_kind: OperationKind::Capture,
                operation_anchor: position,
                slot_kind: SlotKind::Capture,
                deficit_before: 1.0,
                deficit_after: 0.0,
                remaining_funds_before: 2000,
                decision: ProductionDecision::Produced {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    facility: position,
                },
            });
        }
        commit_facilities(
            &mut commands,
            &mut trace,
            &mut V4RollingPlanRegistry::default(),
            &[0, 1],
            &[Vec::new(), Vec::new()],
        );
        for (step, expected) in trace.steps.iter().zip([right, left]) {
            let ProductionDecision::Produced { facility, .. } = step.decision else {
                panic!("購入traceが失われた");
            };
            assert_eq!(facility, expected);
        }
    }
}
