//! 砲台の運搬（[`crate::ai::v4::artillery_ferry`]）に使う補給輸送車を、砲台の数に合わせて買う。
//!
//! 計画は砲台を「補給輸送車で運ばれる」前提で、輸送車との組の値段で評価して買う。
//! 砲台は工場の上から隣の輸送車へ直接乗るのが最も早いので、この手番に買った砲台には、
//! 組で確保済みの資金で隣の施設に輸送車を付ける。作戦枠の前に呼べば、運ぶ手段の無い
//! 既存の砲台へ残額で輸送車を付ける。

use super::*;
use crate::ai::v4::artillery_ferry::{
    GUNS_FOR_ATTACK_ROUTES, is_artillery_carrier, is_ferried_gun,
};

/// 砲台に対して足りない補給輸送車を、残額と空き施設で買う。新しく買う砲台の隣の施設を
/// 優先し、次の手番にすぐ積み込めるようにする。
pub(super) fn allocate_artillery_carriers(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
) {
    let ProductionInputs {
        scan, player_id, ..
    } = *input;
    let Some((carrier_type, carrier_stats)) = scan
        .available_types
        .iter()
        .find(|(_, stats)| is_artillery_carrier(stats))
        .map(|(unit_type, stats)| (*unit_type, stats.clone()))
    else {
        return;
    };
    let is_gun_type = |unit_type: UnitType| {
        scan.available_types
            .iter()
            .any(|(candidate, stats)| *candidate == unit_type && is_ferried_gun(stats))
    };
    // 輸送中の部隊は盤面に居ないので、満載の補給輸送車は砲台も1基数える。
    let carriers = scan
        .my_units
        .iter()
        .filter(|unit| is_artillery_carrier(&unit.stats))
        .collect::<Vec<_>>();
    let guns = scan
        .my_units
        .iter()
        .filter(|unit| is_ferried_gun(&unit.stats))
        .map(|unit| unit.pos)
        .collect::<Vec<_>>();
    // 近くに補給輸送車が居ない孤立した砲台（護衛を失って前線に取り残された砲台）。
    let unescorted_guns = scan
        .my_units
        .iter()
        .filter(|unit| {
            is_ferried_gun(&unit.stats)
                && !carriers
                    .iter()
                    .any(|c| scan.map.distance(c.pos.x, c.pos.y, unit.pos.x, unit.pos.y) <= 2)
        })
        .map(|unit| unit.pos)
        .collect::<Vec<_>>();
    place_new_guns_for_fastest_emplacement(
        input,
        state,
        (carrier_type, &carrier_stats),
        &is_gun_type,
    );
    let new_guns = state
        .commands
        .iter()
        .filter(|planned| is_gun_type(planned.command.unit_type))
        .map(|planned| GridPosition {
            x: planned.command.target_x,
            y: planned.command.target_y,
        })
        .collect::<Vec<_>>();
    // 計画は砲台を補給輸送車との組の値段で買い、輸送車の分の資金を既に確保している。
    let gun_cost = |unit_type: UnitType| {
        scan.available_types
            .iter()
            .find(|(candidate, _)| *candidate == unit_type)
            .map_or(0, |(_, stats)| stats.cost)
    };
    let mut prepaid = state
        .commands
        .iter()
        .filter(|planned| {
            is_gun_type(planned.command.unit_type)
                && planned.cost
                    >= gun_cost(planned.command.unit_type).saturating_add(carrier_stats.cost)
        })
        .count();
    let new_carriers = state
        .commands
        .iter()
        .filter(|planned| planned.command.unit_type == carrier_type)
        .count();
    let gun_count = guns.len()
        + new_guns.len()
        + carriers
            .iter()
            .filter(|carrier| carrier.free_cargo == 0)
            .count();
    let wanted = gun_count.saturating_sub(carriers.len() + new_carriers);
    if wanted == 0 {
        return;
    }
    let Some((operation_kind, operation_anchor)) = state
        .operations
        .first()
        .map(|operation| (operation.kind, operation.anchor))
    else {
        return;
    };
    for _ in 0..wanted {
        let allocation = &state.allocation;
        // 新しい砲台の隣、なければ既存の砲台に近い空き施設。
        let facility = scan
            .free_facilities
            .iter()
            .filter(|(position, terrain)| {
                !allocation.is_facility_used(position) && scan.can_produce(*terrain, carrier_type)
            })
            .map(|(position, _)| *position)
            .min_by_key(|position| {
                let to_new = new_guns
                    .iter()
                    .map(|gun| scan.map.distance(position.x, position.y, gun.x, gun.y))
                    .min()
                    .unwrap_or(u32::MAX);
                let to_unescorted = unescorted_guns
                    .iter()
                    .map(|gun| scan.map.distance(position.x, position.y, gun.x, gun.y))
                    .min()
                    .unwrap_or(u32::MAX);
                let to_existing = guns
                    .iter()
                    .map(|gun| scan.map.distance(position.x, position.y, gun.x, gun.y))
                    .min()
                    .unwrap_or(u32::MAX);
                // 組で買った砲台の輸送車はその砲台の隣に置き、既存の孤立砲台があればそれに近い施設を優先。
                let nearest = if prepaid > 0 {
                    to_new
                } else if !unescorted_guns.is_empty() {
                    to_unescorted
                } else {
                    to_new.min(to_existing)
                };
                (nearest, position.y, position.x)
            });
        let Some(facility) = facility else {
            return;
        };
        let remaining_funds_before = state.allocation.remaining_funds();
        // 組で確保済みの輸送車は、配分から重ねて差し引かない。
        let claimed_cost = if prepaid > 0 {
            prepaid -= 1;
            0
        } else {
            carrier_stats.cost
        };
        if state
            .allocation
            .try_claim(facility, claimed_cost, 0)
            .is_err()
        {
            return;
        }
        // 生産traceは購入案と1対1で記録する。補給輸送車はどの作戦の枠でもないので、
        // 最優先の作戦の地点へ記録する。
        state.plan_trace.steps.push(ProductionStepTrace {
            operation_kind,
            operation_anchor,
            slot_kind: SlotKind::Transport,
            deficit_before: 0.0,
            deficit_after: 0.0,
            remaining_funds_before,
            decision: ProductionDecision::ProducedImmediateReinforcement {
                unit_type: carrier_type,
                cost: carrier_stats.cost,
                facility,
            },
        });
        state.commands.push(PlannedProduction {
            command: ProduceUnitCommand {
                player_id,
                target_x: facility.x,
                target_y: facility.y,
                unit_type: carrier_type,
            },
            role: SlotKind::Transport,
            cost: claimed_cost,
            purpose: PurchasePurpose::Mobilization,
            deployment: None,
            capture_intent: None,
        });
    }
}

/// この手番に買う砲台の生産施設を、作戦地点へ据えられるまでの手番が最も早い施設へ移す。
///
/// 砲台は生産の次の手番に隣の補給輸送車へ乗り、輸送車の足で作戦地点まで運ばれる。
/// 前線へ据える手番は、どの施設で作るかと隣に輸送車を作れるかで決まるので、
/// 「輸送車が作戦地点へ着く手番 + 隣に輸送車を作れなければ迎えを待つ1手番」で比べる。
/// 空いている施設へはそのまま移し、他の購入（砲台・輸送車以外）が使う施設とは入れ替える。
fn place_new_guns_for_fastest_emplacement(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
    carrier: (UnitType, &UnitStats),
    is_gun_type: &dyn Fn(UnitType) -> bool,
) {
    let scan = input.scan;
    let (carrier_type, carrier_stats) = carrier;
    let facility_of = |planned: &PlannedProduction| GridPosition {
        x: planned.command.target_x,
        y: planned.command.target_y,
    };
    let terrain_at = |position: GridPosition| {
        scan.production_facilities
            .iter()
            .find(|(facility, _)| *facility == position)
            .map(|(_, terrain)| *terrain)
    };
    // 購入とtraceは1対1で積まれるので、購入の順に作戦地点を引く。
    let anchors = state
        .plan_trace
        .steps
        .iter()
        .filter(|step| {
            matches!(
                step.decision,
                ProductionDecision::Produced { .. }
                    | ProductionDecision::ProducedImmediateReinforcement { .. }
            )
        })
        .map(|step| step.operation_anchor)
        .collect::<Vec<_>>();
    if anchors.len() != state.commands.len() {
        return;
    }
    for (gun_index, &anchor) in anchors.iter().enumerate() {
        let gun_type = state.commands[gun_index].command.unit_type;
        if !is_gun_type(gun_type) {
            continue;
        }
        let current = facility_of(&state.commands[gun_index]);
        // 候補施設と、そこを使っている別の購入（空き施設ならNone）。
        let mut candidates = vec![(current, None)];
        candidates.extend(
            scan.free_facilities
                .iter()
                .filter(|(facility, terrain)| {
                    !state.allocation.is_facility_used(facility)
                        && scan.can_produce(*terrain, gun_type)
                })
                .map(|(facility, _)| (*facility, None)),
        );
        candidates.extend(
            (0..state.commands.len())
                .filter(|other| *other != gun_index)
                .filter(|other| {
                    let planned = &state.commands[*other];
                    let facility = facility_of(planned);
                    !is_gun_type(planned.command.unit_type)
                        && planned.command.unit_type != carrier_type
                        && terrain_at(facility)
                            .is_some_and(|terrain| scan.can_produce(terrain, gun_type))
                        && terrain_at(current).is_some_and(|terrain| {
                            scan.can_produce(terrain, planned.command.unit_type)
                        })
                })
                .map(|other| (facility_of(&state.commands[other]), Some(other))),
        );
        let allocation = &state.allocation;
        let action_turns = &mut state.ctx.action_turns;
        let best = candidates
            .into_iter()
            .filter_map(|(facility, occupant)| {
                let arrival = property_control::capture_arrival_turns(
                    &scan.map,
                    &scan.master_data,
                    (facility.x, facility.y),
                    (anchor.x, anchor.y),
                    carrier_stats.movement_type,
                    carrier_stats.max_movement,
                    carrier_stats.max_fuel,
                    input.player_id,
                    action_turns,
                )?;
                // 移った後に空く施設（空き施設へ移るなら今の施設）も輸送車の生産に使える。
                let vacated = if occupant.is_none() && facility != current {
                    Some(current)
                } else {
                    None
                };
                let truck_beside = scan.free_facilities.iter().any(|(neighbor, terrain)| {
                    (!allocation.is_facility_used(neighbor) || vacated == Some(*neighbor))
                        && *neighbor != facility
                        && scan.can_produce(*terrain, carrier_type)
                        && scan
                            .map
                            .distance(neighbor.x, neighbor.y, facility.x, facility.y)
                            == 1
                });
                let emplaced = arrival.saturating_add(u32::from(!truck_beside));
                let distance = scan
                    .map
                    .distance(facility.x, facility.y, anchor.x, anchor.y);
                // 同じ手番なら今の施設を保ち、無用な入れ替えをしない。
                Some((
                    (
                        emplaced,
                        u32::from(facility != current),
                        distance,
                        facility.y,
                        facility.x,
                    ),
                    facility,
                    occupant,
                ))
            })
            .min_by_key(|(key, _, _)| *key);
        let Some((_, facility, occupant)) = best else {
            continue;
        };
        if facility == current {
            continue;
        }
        match occupant {
            Some(other) => {
                set_facility(&mut state.commands[other], current);
            }
            None => {
                if state.allocation.move_claim(current, facility).is_err() {
                    continue;
                }
            }
        }
        set_facility(&mut state.commands[gun_index], facility);
    }
}

/// 購入の生産施設を書き換える。占領の意図も同じ施設を指す。
fn set_facility(planned: &mut PlannedProduction, facility: GridPosition) {
    planned.command.target_x = facility.x;
    planned.command.target_y = facility.y;
    if let Some(intent) = planned.capture_intent.as_mut() {
        intent.command.target_x = facility.x;
        intent.command.target_y = facility.y;
    }
}

/// 攻める進軍路（配分案の守りでない作戦）に回っている砲台（[`BoardScan::attack_route_guns`]:
/// 割り当て済みと、これから割り当てる新しい砲台）が、合計 [`GUNS_FOR_ATTACK_ROUTES`] 基に
/// 満たなければ、攻めの要衝に最も近い空き施設で砲台を買う。既に居る砲台（右辺の守りなど）は
/// 回さないので、その分を引き抜かずに足す。作戦の枠より先に資金を確保する。輸送車との組の
/// 値段に届けば組で確保し（[`allocate_artillery_carriers`] が隣に輸送車を付ける）、届かなければ
/// 砲台だけを買う（輸送車は次の手番以降に、運ぶ手段の無い砲台へ付ける）。
pub(super) fn allocate_attack_route_guns(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
) {
    let ProductionInputs {
        scan, player_id, ..
    } = *input;
    let Some(posture) = scan.route_posture.as_ref() else {
        return;
    };
    let attack_points = posture
        .orders
        .iter()
        .filter(|order| !order.defend)
        .map(|order| order.key_point)
        .collect::<Vec<_>>();
    if attack_points.is_empty() {
        return;
    }
    let Some((gun_type, gun_stats)) = scan
        .available_types
        .iter()
        .find(|(_, stats)| is_ferried_gun(stats))
        .map(|(unit_type, stats)| (*unit_type, stats.clone()))
    else {
        return;
    };
    let Some(carrier_cost) = scan
        .available_types
        .iter()
        .find(|(_, stats)| is_artillery_carrier(stats))
        .map(|(_, stats)| stats.cost)
    else {
        return;
    };
    let distance = |a: GridPosition, b: GridPosition| scan.map.distance(a.x, a.y, b.x, b.y);
    let bought = state
        .commands
        .iter()
        .filter(|planned| planned.command.unit_type == gun_type)
        .count();
    let mut wanted = GUNS_FOR_ATTACK_ROUTES.saturating_sub(scan.attack_route_guns + bought);
    let pair_cost = gun_stats.cost.saturating_add(carrier_cost);
    // 攻めの要衝を順に回り、それぞれの要衝に最も近い空き施設で買う。
    let mut points = attack_points.iter().cycle();
    while wanted > 0 {
        let Some(point) = points.next().copied() else {
            return;
        };
        let Some(operation_kind) = state
            .operations
            .iter()
            .min_by_key(|operation| distance(operation.anchor, point))
            .map(|operation| operation.kind)
        else {
            return;
        };
        let allocation = &state.allocation;
        let facility = scan
            .free_facilities
            .iter()
            .filter(|(position, terrain)| {
                !allocation.is_facility_used(position) && scan.can_produce(*terrain, gun_type)
            })
            .map(|(position, _)| *position)
            .min_by_key(|position| (distance(*position, point), position.y, position.x));
        let Some(facility) = facility else {
            return;
        };
        let remaining_funds_before = state.allocation.remaining_funds();
        let cost = if remaining_funds_before >= pair_cost {
            pair_cost
        } else {
            gun_stats.cost
        };
        if state.allocation.try_claim(facility, cost, 0).is_err() {
            return;
        }
        // 施設の付け替え（[`place_new_guns_for_fastest_emplacement`]）が要衝への到着で
        // 比べるよう、作戦地点には要衝を記録する。
        state.plan_trace.steps.push(ProductionStepTrace {
            operation_kind,
            operation_anchor: point,
            slot_kind: SlotKind::Combat,
            deficit_before: 0.0,
            deficit_after: 0.0,
            remaining_funds_before,
            decision: ProductionDecision::ProducedImmediateReinforcement {
                unit_type: gun_type,
                cost,
                facility,
            },
        });
        state.commands.push(PlannedProduction {
            command: ProduceUnitCommand {
                player_id,
                target_x: facility.x,
                target_y: facility.y,
                unit_type: gun_type,
            },
            role: SlotKind::Combat,
            cost,
            purpose: PurchasePurpose::Mobilization,
            deployment: None,
            capture_intent: None,
        });
        wanted -= 1;
    }
}
