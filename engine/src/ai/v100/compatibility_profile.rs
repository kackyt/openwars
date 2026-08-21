//! V100/V200をOpenWarsへ対応付ける能力ベースの規則。
//!
//! 実装間でユニット名・編成が一致しないため、名称ではなく占領、補給、輸送、
//! 間接射撃というゲーム上の能力から評価する。

use crate::components::UnitStats;
use crate::resources::MovementType;

/// GB版の能力レコードから対応付けた、初期生産の役割。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProductionRole {
    Capturer,
    FastGround,
}

/// GB版の陣営別ID 0/1（兵種0・歩兵）と22/23（兵種11・装甲車）を、名称ではなく
/// OpenWars上の能力へ対応付ける。20/21（兵種10・ロケットランチャー）とは区別する。
pub(crate) fn production_role(stats: &UnitStats) -> Option<ProductionRole> {
    if stats.can_capture {
        Some(ProductionRole::Capturer)
    } else if stats.movement_type == MovementType::ArmoredCar && stats.max_movement >= 6 {
        Some(ProductionRole::FastGround)
    } else {
        None
    }
}

/// GB版の輸送分岐530C/5675に対応するOpenWars側の輸送能力かを判定する。
/// OpenWarsの偵察車は歩兵を搭載できるが、対応するGB兵種22/23は直射戦闘部隊なので除外する。
pub(crate) fn is_gbw_transport(stats: &UnitStats) -> bool {
    stats.max_cargo > 0 && matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
}

/// GB版の初期配備で観測した「高速地上系、歩兵系、歩兵系」の周期を返す。
///
/// OpenWarsは同一フェーズの生産命令を先にキューへ積むため、現在の盤上数ではなく
/// 発行済み生産スロット数を用いて周期を安定させる。
pub(crate) fn preferred_production_role(production_slot: usize) -> ProductionRole {
    if production_slot.is_multiple_of(3) {
        ProductionRole::FastGround
    } else {
        ProductionRole::Capturer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::master_data::UnitName;
    use crate::resources::{MasterDataRegistry, UnitType};

    fn stats() -> UnitStats {
        UnitStats {
            unit_type: UnitType::Infantry,
            movement_type: MovementType::Infantry,
            ..UnitStats::mock()
        }
    }

    #[test]
    fn production_roles_use_capabilities_not_unit_names() {
        let mut capturer = stats();
        capturer.can_capture = true;
        let mut fast_ground = stats();
        fast_ground.movement_type = MovementType::ArmoredCar;
        fast_ground.max_movement = 6;

        assert!(
            production_role(&capturer) == Some(ProductionRole::Capturer)
                && production_role(&fast_ground) == Some(ProductionRole::FastGround)
        );
        assert_eq!(preferred_production_role(0), ProductionRole::FastGround);
        assert_eq!(preferred_production_role(1), ProductionRole::Capturer);
        assert_eq!(preferred_production_role(2), ProductionRole::Capturer);
        assert_eq!(preferred_production_role(3), ProductionRole::FastGround);
    }

    #[test]
    fn gb_transport_mapping_excludes_openwars_recon_extra_ability() {
        let master_data = MasterDataRegistry::load().unwrap();
        let recon = master_data
            .create_unit_stats(&UnitName(UnitType::Recon.as_str().to_owned()))
            .unwrap();
        let transport_helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();

        assert!(recon.max_cargo > 0);
        assert!(!is_gbw_transport(&recon));
        assert!(is_gbw_transport(&transport_helicopter));
    }

    #[test]
    fn map1_observed_gb_unit_ids_match_openwars_capabilities() {
        let master_data = MasterDataRegistry::load().unwrap();
        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let armored_car = master_data
            .create_unit_stats(&UnitName(UnitType::Recon.as_str().to_owned()))
            .unwrap();

        // GB 0/1: 歩兵。GB 22/23: 装甲車（兵種番号11）。
        assert_eq!(
            (
                infantry.cost,
                infantry.max_movement,
                infantry.movement_type,
                infantry.max_fuel,
                infantry.max_ammo1,
                infantry.min_range,
                infantry.max_range,
            ),
            (1_000, 3, MovementType::Infantry, 99, 9, 1, 1)
        );
        assert_eq!(
            (
                armored_car.cost,
                armored_car.max_movement,
                armored_car.movement_type,
                armored_car.max_fuel,
                armored_car.max_ammo1,
                armored_car.min_range,
                armored_car.max_range,
            ),
            (4_200, 6, MovementType::ArmoredCar, 70, 9, 1, 1)
        );
    }

    #[test]
    fn gb_rocket_launcher_profile_is_not_used_for_kind_22_or_23() {
        let master_data = MasterDataRegistry::load().unwrap();
        let rockets = master_data
            .create_unit_stats(&UnitName(UnitType::Rockets.as_str().to_owned()))
            .unwrap();

        // GB 20/21（兵種番号10）の能力。22/23の移動6・射程1とは一致しない。
        assert_eq!(
            (
                rockets.cost,
                rockets.max_movement,
                rockets.movement_type,
                rockets.max_fuel,
                rockets.max_ammo1,
                rockets.min_range,
                rockets.max_range,
            ),
            (6_200, 4, MovementType::Tank, 50, 4, 2, 3)
        );
    }
}
