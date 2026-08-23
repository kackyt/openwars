//! Game Boy Wars TurboのROMから抽出したAI用読み取り専用データ。
//!
//! シナリオ固有値は`resources/master_data/rom_scenario.csv`に置き、ここでは
//! ROM共通の兵種価値表と、マスターデータをAI入力へ変換する処理だけを保持する。

use super::rom_logic::{GbUnitKind, ProductionStrategy, RomEvaluationMode};
use crate::components::{GridPosition, PlayerId};
use crate::resources::master_data::RomScenarioRecord;
use crate::resources::{Map, MasterDataRegistry, UnitType};

// Bank 2 `68AC`のポインタ表が指す24兵種×8組の値をそのまま保持する。
// 表0〜3はC6A6が立つ首都防衛時、表4〜7は通常時に選ばれる。
const UNIT_VALUES_0: [u8; 24] = [
    11, 29, 57, 45, 32, 58, 43, 24, 0, 0, 23, 18, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
const UNIT_VALUES_1: [u8; 24] = [
    11, 29, 57, 45, 32, 58, 43, 24, 40, 33, 23, 18, 0, 32, 24, 42, 0, 30, 13, 100, 0, 0, 0, 0,
];
const UNIT_VALUES_2: [u8; 24] = [
    11, 29, 57, 45, 32, 58, 43, 24, 0, 0, 23, 18, 0, 0, 0, 0, 0, 0, 0, 0, 76, 0, 26, 40,
];
const UNIT_VALUES_3: [u8; 24] = [
    11, 29, 57, 45, 32, 58, 43, 24, 37, 31, 23, 18, 0, 32, 24, 42, 0, 30, 16, 100, 76, 24, 26, 40,
];
const UNIT_VALUES_4: [u8; 24] = [
    11, 29, 57, 45, 32, 58, 43, 24, 0, 0, 23, 18, 0, 0, 0, 42, 0, 30, 16, 100, 76, 0, 26, 0,
];
const UNIT_VALUES_5: [u8; 24] = [
    11, 29, 57, 45, 32, 58, 43, 24, 37, 31, 23, 18, 0, 32, 24, 42, 0, 30, 16, 100, 76, 24, 26, 0,
];
const UNIT_VALUES_6: [u8; 24] = [
    11, 29, 57, 45, 32, 58, 43, 24, 0, 0, 23, 18, 0, 0, 0, 42, 0, 30, 16, 100, 76, 0, 26, 40,
];
const UNIT_VALUES_7: [u8; 24] = [
    11, 29, 57, 45, 32, 58, 43, 24, 37, 31, 23, 18, 0, 32, 24, 42, 0, 30, 16, 100, 76, 24, 26, 40,
];

const UNIT_VALUES: [[u8; 24]; 8] = [
    UNIT_VALUES_0,
    UNIT_VALUES_1,
    UNIT_VALUES_2,
    UNIT_VALUES_3,
    UNIT_VALUES_4,
    UNIT_VALUES_5,
    UNIT_VALUES_6,
    UNIT_VALUES_7,
];

#[derive(Clone, Copy)]
pub(crate) struct RomScenarioData {
    /// ROMシナリオレコード+0x10。敵が自首都からこの距離以内ならC6A6を立てる。
    pub(crate) restricted_radius: u32,
    /// ROMシナリオレコード+0x11。序盤生産戦略を継続するターン数。
    pub(crate) opening_limit: u32,
    /// ROMシナリオレコード+0x12。0以外なら偵察車の任務状態を3へ固定する。
    pub(crate) recon_uses_mission_three: bool,
    strategic_objectives: [[(u8, u8); 2]; 2],
    /// ROMシナリオレコード+0x13の陣営別nibble。値域は0〜3。
    unit_value_profiles: [usize; 2],
    production_limits: [[u8; 24]; 4],
}

impl From<&RomScenarioRecord> for RomScenarioData {
    fn from(record: &RomScenarioRecord) -> Self {
        Self {
            restricted_radius: record.restricted_radius,
            opening_limit: record.opening_limit,
            recon_uses_mission_three: record.recon_uses_mission_three,
            strategic_objectives: record.strategic_objectives,
            unit_value_profiles: record.unit_value_profiles,
            production_limits: record.production_limits,
        }
    }
}

impl RomScenarioData {
    pub(crate) fn unit_value(
        &self,
        player_id: PlayerId,
        unit_type: UnitType,
        mode: RomEvaluationMode,
    ) -> u32 {
        let index = usize::from(GbUnitKind::production_order(unit_type) / 2);
        let table_offset = match mode {
            RomEvaluationMode::Restricted => 0,
            RomEvaluationMode::Normal => 4,
        };
        let side = usize::try_from(player_id.0.saturating_sub(1)).unwrap_or_default();
        let profile = self
            .unit_value_profiles
            .get(side)
            .copied()
            .unwrap_or_default();
        u32::from(UNIT_VALUES[profile + table_offset][index])
    }

    pub(crate) fn production_limit(
        &self,
        strategy: ProductionStrategy,
        unit_type: UnitType,
    ) -> u32 {
        let table = &self.production_limits[strategy.index()];
        let index = usize::from(GbUnitKind::production_order(unit_type) / 2);
        // OpenWarsに存在しない兵種の枠は、似た兵種へ合算せずROM表上で捨てる。
        u32::from(table[index])
    }

    /// ROM 0AE9がシナリオレコード+0x90〜+0x97から読む陣営別の固定目標。
    /// ROM座標は1始まりなので、OpenWarsの0始まり座標へ変換して返す。
    pub(crate) fn strategic_objective(
        &self,
        player_id: PlayerId,
        mission_state: u8,
    ) -> Option<GridPosition> {
        let side = match player_id.0 {
            1 => 0,
            2 => 1,
            _ => return None,
        };
        let objective = match mission_state {
            1 => self.strategic_objectives[side][0],
            2 => self.strategic_objectives[side][1],
            _ => return None,
        };
        Some(GridPosition {
            x: usize::from(objective.0.saturating_sub(1)),
            y: usize::from(objective.1.saturating_sub(1)),
        })
    }
}

/// 現在盤面をマスターデータの地形配列と照合し、対応するROMシナリオを返す。
/// シナリオ名と値はCSV側にあり、ここでは盤面がそのシナリオと一致するかだけを判定する。
pub(crate) fn identify_scenario(
    map: &Map,
    master_data: &MasterDataRegistry,
) -> Option<RomScenarioData> {
    master_data.rom_scenarios.values().find_map(|record| {
        let source = master_data.get_map(record.map_name.as_str())?;
        if source.width != map.width || source.height != map.height {
            return None;
        }
        let matches = (0..source.height).all(|y| {
            (0..source.width).all(|x| {
                source
                    .get_cell(x, y)
                    .and_then(|cell| master_data.terrain_from_id(cell.terrain_id).ok())
                    == map.get_terrain(x, y)
            })
        });
        matches.then(|| RomScenarioData::from(record))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::GridTopology;

    #[test]
    fn map3_uses_the_extracted_opening_table() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_3",
            GridTopology::Hex,
        )
        .unwrap();
        let scenario = identify_scenario(world.resource::<Map>(), &master_data).unwrap();
        assert_eq!(scenario.restricted_radius, 3);
        assert_eq!(scenario.opening_limit, 4);
        assert_eq!(
            scenario.production_limit(ProductionStrategy::Opening, UnitType::Infantry),
            11
        );
        assert_eq!(
            scenario.production_limit(ProductionStrategy::Opening, UnitType::TransportHelicopter),
            3
        );
        // 潜水艦の上限1は戦艦へ合算せず、戦艦自身の上限4だけを使う。
        assert_eq!(
            scenario.production_limit(ProductionStrategy::Advantage, UnitType::Battleship),
            4
        );
        assert_eq!(
            scenario.unit_value(PlayerId(1), UnitType::Bcopters, RomEvaluationMode::Normal),
            30
        );
    }

    #[test]
    fn restricted_tables_match_rom_profiles_zero_through_three() {
        let master_data = MasterDataRegistry::load().unwrap();
        let scenario_for = |name| {
            let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
                &master_data,
                name,
                GridTopology::Hex,
            )
            .unwrap();
            identify_scenario(world.resource::<Map>(), &master_data).unwrap()
        };
        let map1 = scenario_for("map_1");
        assert_eq!(map1.restricted_radius, 2);
        assert_eq!(map1.opening_limit, 3);
        assert_eq!(
            map1.unit_value(PlayerId(1), UnitType::Bomber, RomEvaluationMode::Restricted),
            0
        );
        assert_eq!(
            map1.unit_value(PlayerId(1), UnitType::Bomber, RomEvaluationMode::Normal),
            42
        );
        let map6 = scenario_for("map_6");
        assert_eq!(
            map6.unit_value(
                PlayerId(1),
                UnitType::Missiles,
                RomEvaluationMode::Restricted
            ),
            40
        );
        assert_eq!(
            map6.unit_value(PlayerId(1), UnitType::Missiles, RomEvaluationMode::Normal),
            37
        );
        assert_eq!(
            map6.unit_value(
                PlayerId(1),
                UnitType::TransportHelicopter,
                RomEvaluationMode::Restricted,
            ),
            13
        );
        let map8 = scenario_for("map_8");
        assert_eq!(
            map8.unit_value(
                PlayerId(1),
                UnitType::Battleship,
                RomEvaluationMode::Restricted
            ),
            76
        );
        assert_eq!(
            map8.unit_value(PlayerId(1), UnitType::Bomber, RomEvaluationMode::Restricted),
            0
        );
        // プロファイル3はROM上で表3と表7が同じアドレスを共有する。
        let map3 = scenario_for("map_3");
        assert_eq!(
            map3.unit_value(
                PlayerId(1),
                UnitType::Bcopters,
                RomEvaluationMode::Restricted
            ),
            map3.unit_value(PlayerId(1), UnitType::Bcopters, RomEvaluationMode::Normal)
        );
    }

    #[test]
    fn square_topology_uses_the_same_rom_scenario_data() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_3",
            GridTopology::Square,
        )
        .unwrap();
        let scenario = identify_scenario(world.resource::<Map>(), &master_data).unwrap();
        assert_eq!(scenario.opening_limit, 4);
        assert_eq!(
            scenario.production_limit(ProductionStrategy::Opening, UnitType::TransportHelicopter),
            3
        );
    }

    #[test]
    fn generated_maps_and_ulysses_activate_rom_scenario_logic() {
        let master_data = MasterDataRegistry::load().unwrap();
        for (map_name, opening_limit, opening_infantry) in
            [("map_9", 3, 12), ("map_10", 3, 12), ("map_11", 2, 6)]
        {
            let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
                &master_data,
                map_name,
                GridTopology::Hex,
            )
            .unwrap();
            let scenario = identify_scenario(world.resource::<Map>(), &master_data)
                .unwrap_or_else(|| panic!("ROM scenario for {map_name} was not identified"));

            assert_eq!(scenario.restricted_radius, 3);
            assert_eq!(scenario.opening_limit, opening_limit);
            assert_eq!(
                scenario.production_limit(ProductionStrategy::Opening, UnitType::Infantry),
                opening_infantry
            );
        }
    }

    #[test]
    fn asymmetric_rom_profiles_are_kept_per_player() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_52",
            GridTopology::Hex,
        )
        .unwrap();
        let scenario = identify_scenario(world.resource::<Map>(), &master_data).unwrap();

        assert_eq!(
            scenario.unit_value(PlayerId(1), UnitType::Missiles, RomEvaluationMode::Normal),
            0
        );
        assert_eq!(
            scenario.unit_value(PlayerId(2), UnitType::Missiles, RomEvaluationMode::Normal),
            37
        );
    }

    #[test]
    fn every_master_map_activates_its_scenario_data() {
        let master_data = MasterDataRegistry::load().unwrap();
        for map_number in 1..=53 {
            let map_name = format!("map_{map_number}");
            let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
                &master_data,
                &map_name,
                GridTopology::Hex,
            )
            .unwrap();
            assert!(
                identify_scenario(world.resource::<Map>(), &master_data).is_some(),
                "ROM scenario for {map_name} was not identified"
            );
        }
    }
}
