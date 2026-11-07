//! 当手番に実行予定の購入を、補助輸送車の先取りから守る。
//! 比率・兵種名ではなく、有効な継続計画の実費と施設から保護量を導く。
use super::*;

#[derive(Default)]
pub(super) struct DuePurchaseReserve {
    pub(super) funds: u32,
    facilities: HashSet<GridPosition>,
}

impl DuePurchaseReserve {
    /// 実行可能な当手番購入だけを、作戦優先順に一度だけ数える。
    fn collect(
        purchases: impl IntoIterator<Item = rolling_plan::PlannedPurchase>,
        funds: u32,
        available: impl Fn(GridPosition, UnitType) -> bool,
    ) -> Self {
        let mut reserve = Self::default();
        for purchase in purchases {
            if purchase.build_turn != 0
                || purchase.cost == 0
                || reserve.facilities.contains(&purchase.facility)
                || !available(purchase.facility, purchase.unit_type)
                || purchase.cost > funds.saturating_sub(reserve.funds)
            {
                continue;
            }
            reserve.funds += purchase.cost;
            reserve.facilities.insert(purchase.facility);
        }
        reserve
    }

    pub(super) fn permits(&self, facility: GridPosition, cost: u32, funds: u32) -> bool {
        !self.facilities.contains(&facility) && funds.saturating_sub(self.funds) >= cost
    }
}

pub(super) fn protect_due_purchases(
    input: &ProductionInputs<'_>,
    state: &ProductionState,
    registry: &V4RollingPlanRegistry,
) -> DuePurchaseReserve {
    let mut due = Vec::new();
    for operation in &state.operations {
        let enemies = operation
            .reachable_threats
            .iter()
            .filter_map(|threat| threat.entity)
            .collect();
        let Some(previous) = registry.continuation_for_operation(
            input.player_id,
            input.turn,
            operation.kind,
            operation.island_id,
            &operation.objective_properties,
            &enemies,
        ) else {
            continue;
        };
        // 失敗・遅延で再計画すべき古い購入は保護せず、予約だけで資金を止めない。
        if previous.production_failed || previous.execution_delayed {
            continue;
        }
        due.extend(previous.purchases);
    }
    let reserve =
        DuePurchaseReserve::collect(
            due.iter().copied(),
            state.allocation.remaining_funds(),
            |facility, kind| {
                !state.allocation.is_facility_used(&facility)
                    && input.scan.free_facilities.iter().any(|(p, terrain)| {
                        *p == facility && input.scan.can_produce(*terrain, kind)
                    })
            },
        );
    // 診断を明示した局面試験だけ、予定購入と実際の保護量を照合できるようにする。
    if std::env::var_os("OPENWARS_DUE_BUDGET_TRACE").is_some() {
        eprintln!(
            "DUE_BUDGET {}",
            serde_json::json!({
                "turn": input.turn, "player": input.player_id.0,
                "funds": state.allocation.remaining_funds(), "reserved": reserve.funds,
                "reserved_facilities": reserve.facilities.iter().map(|p| [p.x, p.y]).collect::<Vec<_>>(),
                "purchases": due.iter().map(|p| serde_json::json!({
                    "unit": p.unit_type.as_str(), "cost": p.cost, "build_turn": p.build_turn,
                    "facility": [p.facility.x, p.facility.y],
                })).collect::<Vec<_>>(),
            })
        );
    }
    reserve
}

#[cfg(test)]
mod tests {
    use super::*;
    use rolling_plan::PlannedPurchase;

    fn purchase(cost: u32, build_turn: u32) -> PlannedPurchase {
        PlannedPurchase {
            facility: GridPosition { x: 1, y: 1 },
            unit_type: UnitType::TankZ,
            build_turn,
            cost,
        }
    }

    #[test]
    fn due_purchase_is_not_displaced_by_a_support_truck() {
        let reserve = DuePurchaseReserve::collect([purchase(28_000, 0)], 30_000, |_, _| true);
        assert!(!reserve.permits(GridPosition { x: 2, y: 1 }, 3_500, 30_000));
        assert!(reserve.permits(GridPosition { x: 2, y: 1 }, 3_500, 31_500));
        assert!(!reserve.permits(GridPosition { x: 1, y: 1 }, 3_500, 100_000));
    }

    #[test]
    fn future_unaffordable_or_invalid_purchase_does_not_freeze_funds() {
        assert_eq!(
            DuePurchaseReserve::collect([purchase(28_000, 1)], 30_000, |_, _| true).funds,
            0
        );
        assert_eq!(
            DuePurchaseReserve::collect([purchase(28_000, 0)], 20_000, |_, _| true).funds,
            0
        );
        assert_eq!(
            DuePurchaseReserve::collect([purchase(28_000, 0)], 30_000, |_, _| false).funds,
            0
        );
    }

    #[test]
    fn same_facility_is_not_reserved_twice() {
        assert_eq!(
            DuePurchaseReserve::collect(
                [purchase(28_000, 0), purchase(28_000, 0)],
                60_000,
                |_, _| true
            )
            .funds,
            28_000
        );
    }

    #[test]
    fn early_artillery_purchase_gets_the_same_protection() {
        let gun = PlannedPurchase {
            unit_type: UnitType::Artillery,
            ..purchase(23_500, 0)
        };
        let reserve = DuePurchaseReserve::collect([gun], 26_000, |_, _| true);
        assert!(!reserve.permits(GridPosition { x: 2, y: 1 }, 3_500, 26_000));
    }
}
