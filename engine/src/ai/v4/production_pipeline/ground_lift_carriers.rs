//! 陸上輸送（[`crate::ai::v4::ground_lift`]）の装甲車を、作戦枠が使い残した資金で買い足す。

use super::*;

/// 同時に走らせる陸上輸送の装甲車の上限。装甲車は戦力にならないので、往復で
/// 毎手番1〜2体ずつ重歩兵を送れる数に留める。
const GROUND_LIFT_MAX_CARRIERS: usize = 4;

/// 歩兵類を載せて陸上を走る輸送部隊（装甲車）の能力か。
fn is_ground_carrier(stats: &UnitStats) -> bool {
    stats.max_cargo > 0 && stats.movement_type == MovementType::ArmoredCar
}

/// 運ぶべき重歩兵の数に対して足りない装甲車を、残額と空き施設で買う。
///
/// 運ぶべき重歩兵は、同じ陸塊の敵から歩いて2手番より遠い自軍の重歩兵と、
/// この手番に買う重歩兵。両首都が同じ陸塊にある（敵の生産施設と自軍の生産施設が
/// 同じ陸塊にある）盤面だけで買う。
pub(super) fn allocate_ground_lift_carriers(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
) {
    let ProductionInputs {
        scan, player_id, ..
    } = *input;
    let Some((carrier_type, carrier_stats)) = scan
        .available_types
        .iter()
        .find(|(_, stats)| is_ground_carrier(stats))
        .map(|(unit_type, stats)| (*unit_type, stats.clone()))
    else {
        return;
    };
    let island_map = crate::ai::islands::IslandMap::analyze(&scan.map);
    let island_of =
        |position: &GridPosition| island_map.get_island_at(position).map(|island| island.id);
    let Some(home) = scan
        .production_facilities
        .first()
        .and_then(|(position, _)| island_of(position))
    else {
        return;
    };
    if !scan
        .enemy_facilities
        .iter()
        .any(|facility| island_of(&facility.pos) == Some(home))
    {
        return;
    }
    let rear_mechs = scan
        .my_units
        .iter()
        .filter(|unit| unit.stats.unit_type == UnitType::Mech && island_of(&unit.pos) == Some(home))
        .filter(|unit| {
            let walk = unit.stats.max_movement.saturating_mul(2).saturating_add(1);
            scan.enemy_units
                .iter()
                .filter(|enemy| island_of(&enemy.pos) == Some(home))
                .all(|enemy| {
                    scan.map
                        .distance(unit.pos.x, unit.pos.y, enemy.pos.x, enemy.pos.y)
                        > walk
                })
        })
        .count();
    let new_mechs = state
        .commands
        .iter()
        .filter(|planned| planned.command.unit_type == UnitType::Mech)
        .count();
    let carriers = scan
        .my_units
        .iter()
        .filter(|unit| is_ground_carrier(&unit.stats))
        .count();
    let wanted = (rear_mechs + new_mechs)
        .min(GROUND_LIFT_MAX_CARRIERS)
        .saturating_sub(carriers);
    let mut facilities = scan
        .free_facilities
        .iter()
        .filter(|(position, terrain)| {
            !state.allocation.is_facility_used(position) && scan.can_produce(*terrain, carrier_type)
        })
        .map(|(position, _)| *position)
        .collect::<Vec<_>>();
    facilities.sort_unstable_by_key(|position| (position.y, position.x));
    // 装甲車はどの作戦の枠でもないが、生産traceは購入案と1対1で記録する。
    // 最優先の作戦の地点へ、残額からの購入として記録する。
    let Some((operation_kind, operation_anchor)) = state
        .operations
        .first()
        .map(|operation| (operation.kind, operation.anchor))
    else {
        return;
    };
    for facility in facilities.into_iter().take(wanted) {
        let remaining_funds_before = state.allocation.remaining_funds();
        if state
            .allocation
            .try_claim(facility, carrier_stats.cost, u32::MAX)
            .is_err()
        {
            break;
        }
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
            cost: carrier_stats.cost,
            purpose: PurchasePurpose::Mobilization,
            deployment: None,
            capture_intent: None,
        });
    }
}
