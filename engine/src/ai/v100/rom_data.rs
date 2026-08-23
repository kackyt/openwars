//! Game Boy Wars Turboのシナリオレコードから抽出したAI用読み取り専用データ。
//!
//! Bank 0 `0B11`はシナリオレコード+`0x9A`にある4×24兵種の保有上限を、
//! Bank 2 `5152`はシナリオレコード+`0x13`が選ぶ兵種価値表を参照する。
//! マップ固有の行動を記述するものではなく、原作AIが入力として読むマスターデータである。

use super::rom_logic::{GbUnitKind, ProductionStrategy};
use crate::components::{GridPosition, PlayerId};
use crate::resources::{Map, MasterDataRegistry, UnitType};

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

#[derive(Clone, Copy)]
pub(crate) struct RomScenarioData {
    pub(crate) opening_limit: u32,
    /// ROMシナリオレコード+0x12。0以外なら偵察車の任務状態を3へ固定する。
    pub(crate) recon_uses_mission_three: bool,
    strategic_objectives: [[(u8, u8); 2]; 2],
    unit_values: &'static [u8; 24],
    production_limits: [[u8; 24]; 4],
}

impl RomScenarioData {
    pub(crate) fn unit_value(self, unit_type: UnitType) -> u32 {
        let index = usize::from(GbUnitKind::production_order(unit_type) / 2);
        u32::from(self.unit_values[index])
    }

    pub(crate) fn production_limit(self, strategy: ProductionStrategy, unit_type: UnitType) -> u32 {
        let table = &self.production_limits[strategy.index()];
        let index = usize::from(GbUnitKind::production_order(unit_type) / 2);
        // OpenWarsに存在しない兵種の枠は、似た兵種へ合算せずROM表上で捨てる。
        u32::from(table[index])
    }

    /// ROM 0AE9がシナリオレコード+0x90〜+0x97から読む陣営別の固定目標。
    /// ROM座標は1始まりなので、OpenWarsの0始まり座標へ変換して返す。
    pub(crate) fn strategic_objective(
        self,
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

const MAP_1: RomScenarioData = RomScenarioData {
    opening_limit: 3,
    recon_uses_mission_three: false,
    strategic_objectives: [[(3, 8), (7, 12)], [(8, 8), (5, 5)]],
    unit_values: &UNIT_VALUES_4,
    production_limits: [
        [
            11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
        [
            14, 1, 0, 2, 2, 1, 1, 0, 0, 0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
        [
            15, 0, 0, 1, 3, 0, 1, 0, 0, 0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
        [
            7, 5, 0, 0, 3, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
    ],
};

const MAP_2: RomScenarioData = RomScenarioData {
    opening_limit: 2,
    recon_uses_mission_three: false,
    strategic_objectives: [[(9, 9), (11, 7)], [(3, 8), (5, 6)]],
    unit_values: &UNIT_VALUES_7,
    production_limits: [
        [
            8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0,
        ],
        [
            13, 5, 0, 2, 3, 1, 3, 0, 1, 1, 2, 3, 0, 0, 1, 1, 0, 2, 1, 0, 1, 0, 0, 0,
        ],
        [
            16, 2, 0, 1, 4, 1, 3, 1, 1, 2, 2, 2, 0, 0, 1, 1, 0, 2, 1, 0, 0, 0, 0, 0,
        ],
        [
            10, 7, 0, 0, 4, 1, 1, 1, 1, 2, 1, 3, 0, 0, 0, 0, 0, 2, 1, 0, 1, 1, 0, 0,
        ],
    ],
};

const MAP_3: RomScenarioData = RomScenarioData {
    opening_limit: 4,
    recon_uses_mission_three: false,
    strategic_objectives: [[(22, 12), (7, 19)], [(7, 19), (22, 12)]],
    unit_values: &UNIT_VALUES_7,
    production_limits: [
        [
            11, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 3, 3, 0, 0, 0, 0, 0,
        ],
        [
            9, 4, 0, 3, 0, 1, 2, 0, 1, 1, 0, 0, 0, 2, 0, 4, 0, 3, 3, 0, 4, 0, 2, 1,
        ],
        [
            16, 0, 0, 1, 0, 1, 1, 0, 1, 1, 0, 1, 0, 1, 3, 3, 0, 3, 4, 0, 2, 0, 1, 1,
        ],
        [
            8, 7, 0, 1, 4, 2, 2, 0, 1, 3, 2, 2, 0, 1, 1, 1, 0, 2, 1, 0, 1, 1, 0, 0,
        ],
    ],
};

const MAP_4: RomScenarioData = RomScenarioData {
    opening_limit: 3,
    recon_uses_mission_three: false,
    strategic_objectives: [[(21, 4), (14, 14)], [(13, 4), (6, 12)]],
    unit_values: &UNIT_VALUES_7,
    production_limits: [
        [
            16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 0, 0, 0, 3, 0,
        ],
        [
            13, 4, 0, 2, 0, 1, 2, 0, 2, 0, 1, 1, 0, 2, 0, 3, 0, 1, 2, 0, 3, 0, 2, 1,
        ],
        [
            16, 2, 0, 1, 0, 1, 1, 0, 1, 1, 1, 1, 0, 1, 3, 3, 0, 1, 2, 0, 2, 0, 2, 1,
        ],
        [
            10, 7, 0, 1, 3, 1, 2, 0, 1, 2, 1, 1, 0, 1, 1, 1, 0, 2, 2, 0, 1, 1, 1, 1,
        ],
    ],
};

const MAP_5: RomScenarioData = RomScenarioData {
    opening_limit: 2,
    recon_uses_mission_three: true,
    strategic_objectives: [[(9, 4), (9, 6)], [(2, 4), (2, 6)]],
    unit_values: &UNIT_VALUES_4,
    production_limits: [
        [
            3, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
        [
            6, 0, 0, 3, 0, 2, 2, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
        [
            7, 0, 0, 1, 1, 2, 1, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
        [
            4, 3, 0, 1, 2, 0, 1, 1, 0, 0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
    ],
};

const MAP_6: RomScenarioData = RomScenarioData {
    opening_limit: 3,
    recon_uses_mission_three: true,
    strategic_objectives: [[(17, 7), (23, 17)], [(15, 15), (8, 8)]],
    unit_values: &UNIT_VALUES_5,
    production_limits: [
        [
            16, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 3, 3, 0, 0, 0, 0, 0,
        ],
        [
            13, 2, 0, 5, 0, 1, 3, 0, 2, 1, 2, 2, 0, 2, 0, 4, 0, 2, 1, 0, 0, 0, 0, 0,
        ],
        [
            17, 0, 0, 2, 0, 1, 2, 0, 2, 2, 1, 2, 0, 1, 2, 3, 0, 3, 2, 0, 0, 0, 0, 0,
        ],
        [
            8, 7, 0, 2, 2, 2, 2, 0, 1, 4, 2, 2, 0, 1, 1, 2, 0, 3, 1, 0, 0, 0, 0, 0,
        ],
    ],
};

const MAP_7: RomScenarioData = RomScenarioData {
    opening_limit: 2,
    recon_uses_mission_three: false,
    strategic_objectives: [[(9, 4), (11, 14)], [(9, 14), (9, 4)]],
    unit_values: &UNIT_VALUES_7,
    production_limits: [
        [
            9, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0,
        ],
        [
            14, 0, 0, 4, 0, 1, 3, 0, 2, 2, 0, 2, 0, 0, 2, 2, 0, 2, 2, 0, 2, 0, 1, 1,
        ],
        [
            18, 0, 0, 1, 2, 1, 3, 0, 1, 2, 1, 2, 0, 0, 1, 2, 0, 2, 2, 0, 1, 0, 0, 1,
        ],
        [
            9, 7, 0, 0, 4, 1, 1, 0, 1, 3, 1, 3, 0, 0, 1, 1, 0, 2, 0, 0, 2, 1, 0, 0,
        ],
    ],
};

const MAP_8: RomScenarioData = RomScenarioData {
    opening_limit: 3,
    recon_uses_mission_three: false,
    strategic_objectives: [[(17, 5), (15, 14)], [(5, 5), (7, 13)]],
    unit_values: &UNIT_VALUES_6,
    production_limits: [
        [
            20, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
        [
            20, 0, 0, 4, 0, 2, 5, 0, 0, 0, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 1,
        ],
        [
            21, 0, 0, 2, 3, 3, 4, 0, 0, 0, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 1,
        ],
        [
            13, 9, 0, 2, 4, 1, 2, 2, 0, 0, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 1,
        ],
    ],
};

const SCENARIOS: [(&str, RomScenarioData); 8] = [
    ("map_1", MAP_1),
    ("map_2", MAP_2),
    ("map_3", MAP_3),
    ("map_4", MAP_4),
    ("map_5", MAP_5),
    ("map_6", MAP_6),
    ("map_7", MAP_7),
    ("map_8", MAP_8),
];

/// 現在盤面をマスターデータの地形配列と照合し、対応するROMシナリオを返す。
pub(crate) fn identify_scenario(
    map: &Map,
    master_data: &MasterDataRegistry,
) -> Option<RomScenarioData> {
    SCENARIOS.iter().find_map(|(name, scenario)| {
        let source = master_data.get_map(name)?;
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
        matches.then_some(*scenario)
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
        assert_eq!(scenario.unit_value(UnitType::Bcopters), 30);
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

        // シナリオ表は地形配置で識別し、距離と到達判定だけをMapのtopologyへ委譲する。
        let scenario = identify_scenario(world.resource::<Map>(), &master_data).unwrap();
        assert_eq!(scenario.opening_limit, 4);
        assert_eq!(
            scenario.production_limit(ProductionStrategy::Opening, UnitType::TransportHelicopter),
            3
        );
    }
}
