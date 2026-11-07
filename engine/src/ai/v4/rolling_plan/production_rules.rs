//! 比較試作用の候補役割と予測期間。兵種の評価点や作戦の購入期限は変更しない。
use crate::components::UnitStats;

/// 輸送役と戦闘役を区別する。既存経路の条件を比較基準として保持する。
pub(in crate::ai::v4) fn allows_combat_role(stats: &UnitStats, armed_carriers: bool) -> bool {
    // 搭載能力があっても武装していれば戦闘候補とする。敵への有効性は入力側で判定する。
    stats.max_cargo == 0 || (armed_carriers && stats.max_range > 0)
}

/// 予測期間と購入・敵排除期限を混同せず、戦闘を計算する期間だけを返す。
pub(super) fn forecast_turns(deadline: Option<u32>, hold: Option<u32>, through_hold: bool) -> u32 {
    let original = deadline.unwrap_or(super::DEFAULT_SEARCH_TURNS).max(1);
    if through_hold {
        original.max(hold.unwrap_or(1))
    } else {
        original
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::UnitType;
    use crate::resources::master_data::{MasterDataRegistry, UnitName};

    fn stats(kind: UnitType) -> UnitStats {
        MasterDataRegistry::load()
            .unwrap()
            .create_unit_stats(&UnitName(kind.as_str().to_owned()))
            .unwrap()
    }

    #[test]
    fn armed_carrier_is_eligible_without_admitting_supply_trucks() {
        let recon = stats(UnitType::Recon);
        assert!(recon.max_cargo > 0 && recon.max_range > 0);
        assert!(allows_combat_role(&recon, true));
        assert!(!allows_combat_role(&stats(UnitType::SupplyTruck), true));
    }

    #[test]
    fn ordinary_combat_units_keep_their_candidate_role() {
        for kind in [UnitType::MdTank, UnitType::HeavySpGun, UnitType::Artillery] {
            assert!(allows_combat_role(&stats(kind), true));
            assert!(allows_combat_role(&stats(kind), false));
        }
    }

    #[test]
    fn disabled_prototype_preserves_the_original_candidate_filter() {
        assert!(!allows_combat_role(&stats(UnitType::Recon), false));
        assert!(!allows_combat_role(&stats(UnitType::SupplyTruck), false));
    }

    #[test]
    fn forecast_reaches_hold_window_without_shortening_the_deadline() {
        assert_eq!(forecast_turns(Some(2), Some(4), true), 4);
        assert_eq!(forecast_turns(Some(7), Some(4), true), 7);
        assert_eq!(
            forecast_turns(None, Some(4), true),
            super::super::DEFAULT_SEARCH_TURNS
        );
    }

    #[test]
    fn absent_hold_and_disabled_prototype_preserve_the_original_horizon() {
        assert_eq!(forecast_turns(Some(2), Some(4), false), 2);
        assert_eq!(forecast_turns(Some(2), None, true), 2);
        assert_eq!(forecast_turns(Some(0), Some(0), true), 1);
    }
}
