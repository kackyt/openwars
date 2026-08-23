//! V100/V200をOpenWarsへ対応付ける能力ベースの規則。
//!
//! 実装間でユニット名・編成が一致しないため、名称ではなく占領、補給、輸送、
//! 間接射撃というゲーム上の能力から評価する。

use crate::components::UnitStats;

/// GB版の輸送分岐51BF/553Dに対応するOpenWars側の輸送能力かを判定する。
/// ROM兵種表では装甲車0x16も搭載数1を持ち、530Cの接近対象に明記されている。
pub(crate) fn is_gbw_transport(stats: &UnitStats) -> bool {
    stats.max_cargo > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::master_data::UnitName;
    use crate::resources::{MasterDataRegistry, MovementType, UnitType};

    #[test]
    fn gb_transport_mapping_includes_the_rom_armored_car_capacity() {
        let master_data = MasterDataRegistry::load().unwrap();
        let recon = master_data
            .create_unit_stats(&UnitName(UnitType::Recon.as_str().to_owned()))
            .unwrap();
        let transport_helicopter = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();

        assert!(recon.max_cargo > 0);
        assert!(is_gbw_transport(&recon));
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
