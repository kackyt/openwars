use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;

/// マスターデータ読み込み時の専用エラー型
#[derive(thiserror::Error, Debug)]
pub enum MasterDataError {
    #[error("CSVパーサーエラー: {0}")]
    CsvError(#[from] csv::Error),
    #[error("数値パースエラー: {0}")]
    ParseError(#[from] std::num::ParseIntError),
    #[error("マップCSVの列数が一致しません: expected {expected}, actual {actual}")]
    InvalidMapWidth { expected: usize, actual: usize },
    #[error("不明な地形ID: {0:?}")]
    UnknownTerrainId(LandscapeId),
    #[error("不正な地形名: {0}")]
    InvalidTerrainName(String),
    #[error("不明なユニット名: {0}")]
    InvalidUnitName(String),
    #[error("不明なカテゴリ名: {0}")]
    InvalidCategoryName(String),
    #[error("不明な移動タイプ: {0}")]
    InvalidMovementType(String),
    #[error("不明なマスターデータ読み込みエラー")]
    Unknown,
}

pub mod supply_types {
    pub const GROUND: &str = "地上部隊";
    pub const AIR: &str = "航空部隊";
    pub const NAVY: &str = "艦船部隊";
}

pub mod movement_types {
    pub const INFANTRY: &str = "歩兵";
    pub const TANK: &str = "戦車";
    pub const ARTILLERY: &str = "砲台";
    pub const ARMORED_CAR: &str = "装甲車";
    pub const AIR: &str = "航空";
    pub const NAVY: &str = "艦船";
}

/// ユニットや武器などを識別するための名前のNewtype
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct UnitName(pub String);

/// 地形を識別するためのIDのNewtype
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
pub struct LandscapeId(pub u32);

#[derive(Debug, Clone, Deserialize)]
pub struct LandscapeRecord {
    #[serde(rename = "ID")]
    pub id: LandscapeId,
    #[serde(rename = "名前")]
    pub name: String,
    #[serde(rename = "耐久度")]
    pub durability: u32,
    #[serde(rename = "地形効果")]
    pub defense_bonus: u32,
    #[serde(rename = "補給補充")]
    pub supply_type: Option<String>,
    #[serde(rename = "収入")]
    pub income: Option<u32>,
}

fn deserialize_movement_type<'de, D>(
    deserializer: D,
) -> Result<crate::resources::MovementType, D::Error>
where
    D: Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;
    crate::resources::MovementType::from_str(&s)
        .ok_or_else(|| serde::de::Error::custom(format!("Unknown movement type: {}", s)))
}

#[derive(Debug, Clone, Deserialize)]
pub struct UnitRecord {
    #[serde(rename = "名前")]
    pub name: UnitName,
    #[serde(rename = "コスト")]
    pub cost: u32,
    #[serde(rename = "移動力")]
    pub movement: u32,
    #[serde(rename = "移動タイプ")]
    #[serde(deserialize_with = "deserialize_movement_type")]
    pub movement_type: crate::resources::MovementType,
    #[serde(rename = "燃料")]
    pub fuel: u32,
    #[serde(rename = "武器1")]
    pub weapon1: Option<String>,
    #[serde(rename = "武器2")]
    pub weapon2: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WeaponRecord {
    pub name: UnitName,
    pub ammo: u32,
    pub supply_cost: u32,
    pub range_min: u32,
    pub range_max: u32,
    pub damages: HashMap<String, u32>,
}

#[derive(Debug, Clone)]
pub struct MovementRecord {
    pub movement_type: crate::resources::MovementType,
    pub terrain_costs: HashMap<String, u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CategoryRecord {
    #[serde(rename = "ユニット名")]
    pub unit_name: String,
    #[serde(rename = "カテゴリ")]
    pub category: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoadRecord {
    #[serde(rename = "輸送ユニット")]
    pub transport: String,
    #[serde(rename = "搭載可能ユニット")]
    pub target: String,
    #[serde(rename = "最大搭載数")]
    pub capacity: u32,
}

#[derive(Debug, Clone)]
pub struct MapCell {
    pub player_id: u32,
    pub terrain_id: LandscapeId,
}

#[derive(Debug, Clone)]
pub struct MapData {
    pub width: usize,
    pub height: usize,
    pub cells: Vec<Vec<u32>>,
}

impl MapData {
    pub fn get_cell(&self, x: usize, y: usize) -> Option<MapCell> {
        if y < self.height && x < self.width {
            let val = self.cells[y][x];
            Some(MapCell {
                player_id: val / 100,
                terrain_id: LandscapeId(val % 100),
            })
        } else {
            None
        }
    }
}

#[derive(Resource, Debug, Clone, Default)]
pub struct MasterDataRegistry {
    pub landscapes: HashMap<LandscapeId, LandscapeRecord>,
    pub landscapes_by_name: HashMap<String, LandscapeId>,
    pub units: HashMap<UnitName, UnitRecord>,
    pub weapons: HashMap<UnitName, WeaponRecord>,
    pub movements: HashMap<crate::resources::MovementType, MovementRecord>,
    pub loads: HashMap<String, Vec<LoadRecord>>,
    pub categories: HashMap<String, Vec<crate::resources::UnitType>>,
    pub maps: HashMap<String, MapData>,
}

impl MasterDataRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load() -> Result<Self, MasterDataError> {
        let mut registry = Self::default();

        // 1. 地形(Landscape)データ読み込み
        // マップセルや効果計算に使用される地形の基本パラメータを登録します。
        let landscape_csv = include_str!("master_data/landscape.csv");
        let mut rdr = csv::Reader::from_reader(landscape_csv.as_bytes());
        for result in rdr.deserialize() {
            let record: LandscapeRecord = result?;
            registry
                .landscapes_by_name
                .insert(record.name.clone(), record.id);
            registry.landscapes.insert(record.id, record);
        }

        // 2. ユニット(Unit)データ読み込み
        // ユニットのコスト、移動力、搭載武器などの基礎特性を登録します。
        let unit_csv = include_str!("master_data/unit.csv");
        let mut rdr = csv::Reader::from_reader(unit_csv.as_bytes());
        for result in rdr.deserialize() {
            let record: UnitRecord = result?;
            registry.units.insert(record.name.clone(), record);
        }

        // 3. 武器(Weapon)・ダメージデータ読み込み
        // 武器毎のベーススタッツと、各防御ユニットへの可変長ダメージテーブルを解析します。
        // csvクレートの #[serde(flatten)] サポート制約を回避するため手動でパースします。
        let weapon_csv = include_str!("master_data/weapon.csv");
        let mut rdr = csv::Reader::from_reader(weapon_csv.as_bytes());
        let headers = rdr.headers()?.clone();
        for result in rdr.records() {
            let record = result?;
            let mut damages = HashMap::new();
            for (i, field) in record.iter().enumerate().skip(5) {
                if let Some(header) = headers.get(i)
                    && !header.is_empty()
                {
                    let trimmed = field.trim();
                    if trimmed != "-" && !trimmed.is_empty() {
                        damages.insert(header.to_string(), trimmed.parse()?);
                    }
                }
            }
            let weapon = WeaponRecord {
                name: UnitName(record.get(0).unwrap_or("").to_string()),
                ammo: record
                    .get(1)
                    .ok_or(MasterDataError::Unknown)?
                    .trim()
                    .parse()?,
                supply_cost: record
                    .get(2)
                    .ok_or(MasterDataError::Unknown)?
                    .trim()
                    .parse()?,
                range_min: record
                    .get(3)
                    .ok_or(MasterDataError::Unknown)?
                    .trim()
                    .parse()?,
                range_max: record
                    .get(4)
                    .ok_or(MasterDataError::Unknown)?
                    .trim()
                    .parse()?,
                damages,
            };
            registry.weapons.insert(weapon.name.clone(), weapon);
        }

        // 4. 移動コスト(Movement)データ読み込み
        // 移動タイプごとの地形進入コストを抽出します。
        let movement_csv = include_str!("master_data/movement.csv");
        let mut rdr = csv::Reader::from_reader(movement_csv.as_bytes());
        let headers = rdr.headers()?.clone();
        for result in rdr.records() {
            let record = result?;
            let mut terrain_costs = HashMap::new();
            for (i, field) in record.iter().enumerate().skip(1) {
                if let Some(header) = headers.get(i)
                    && !header.is_empty()
                {
                    let trimmed = field.trim();
                    if trimmed != "-" && !trimmed.is_empty() {
                        terrain_costs.insert(header.to_string(), trimmed.parse()?);
                    }
                }
            }
            let m_str = record.get(0).unwrap_or("");
            let m_type = crate::resources::MovementType::from_str(m_str)
                .ok_or_else(|| MasterDataError::InvalidMovementType(m_str.to_string()))?;

            let movement = MovementRecord {
                movement_type: m_type,
                terrain_costs,
            };
            registry.movements.insert(m_type, movement);
        }

        // 5. 搭載(Load)データ読み込み
        // どの輸送ユニットがどのユニットを何体搭載できるかの制約を登録します。
        let load_csv = include_str!("master_data/load.csv");
        let mut rdr = csv::Reader::from_reader(load_csv.as_bytes());
        for result in rdr.deserialize() {
            let record: LoadRecord = result?;
            registry
                .loads
                .entry(record.transport.clone())
                .or_default()
                .push(record);
        }

        // 6. カテゴリ(Category)データ読み込み
        // ユニットの属性グループ（「歩兵」「地上部隊」など）を登録します。
        let category_csv = include_str!("master_data/category.csv");
        let mut rdr = csv::Reader::from_reader(category_csv.as_bytes());
        for result in rdr.deserialize() {
            let record: CategoryRecord = result?;
            let u_type = crate::resources::UnitType::from_str(&record.unit_name)
                .ok_or_else(|| MasterDataError::InvalidUnitName(record.unit_name.clone()))?;

            registry
                .categories
                .entry(record.category)
                .or_default()
                .push(u_type);
        }

        // 7. マップ初期配置データ読み込み
        // プレイヤーIDと地形IDが結合された数値を MapData としてパースします。
        let map_1_csv = include_str!("master_data/map/map_1.csv");
        registry
            .maps
            .insert("map_1".to_string(), parse_map(map_1_csv)?);

        Ok(registry)
    }

    pub fn expand_target(
        &self,
        target: &str,
    ) -> Result<Vec<crate::resources::UnitType>, MasterDataError> {
        if let Some(units) = self.categories.get(target) {
            Ok(units.clone())
        } else if let Some(u_type) = crate::resources::UnitType::from_str(target) {
            Ok(vec![u_type])
        } else {
            Err(MasterDataError::InvalidCategoryName(target.to_string()))
        }
    }

    pub fn unit_type_for_name(
        &self,
        name: &str,
    ) -> Result<crate::resources::UnitType, MasterDataError> {
        crate::resources::UnitType::from_str(name)
            .ok_or_else(|| MasterDataError::InvalidUnitName(name.to_string()))
    }

    pub fn get_unit(&self, name: &UnitName) -> Option<&UnitRecord> {
        self.units.get(name)
    }

    pub fn get_landscape(&self, id: LandscapeId) -> Option<&LandscapeRecord> {
        self.landscapes.get(&id)
    }

    pub fn terrain_from_id(
        &self,
        terrain_id: LandscapeId,
    ) -> Result<crate::resources::Terrain, MasterDataError> {
        let landscape = self
            .get_landscape(terrain_id)
            .ok_or(MasterDataError::UnknownTerrainId(terrain_id))?;
        crate::resources::Terrain::from_str(&landscape.name)
            .ok_or_else(|| MasterDataError::InvalidTerrainName(landscape.name.clone()))
    }

    pub fn get_landscape_by_name(&self, name: &str) -> Option<&LandscapeRecord> {
        let id = self.landscapes_by_name.get(name)?;
        self.landscapes.get(id)
    }

    pub fn get_movement_cost(
        &self,
        target_movement_type: crate::resources::MovementType,
        terrain_name: &str,
    ) -> Option<u32> {
        let movement = self.movements.get(&target_movement_type)?;
        movement.terrain_costs.get(terrain_name).copied()
    }

    pub fn get_damage(&self, weapon_name: &UnitName, defender_name: &str) -> Option<u32> {
        let weapon = self.weapons.get(weapon_name)?;
        weapon.damages.get(defender_name).copied()
    }

    pub fn get_map(&self, map_name: &str) -> Option<&MapData> {
        self.maps.get(map_name)
    }

    /// 地形名からターンごとの収入を返す（マスターデータのincomeフィールドを参照）
    /// 収入フィールドがない地形（道路・平地など）は 0 を返す
    pub fn landscape_income(&self, name: &str) -> u32 {
        self.get_landscape_by_name(name)
            .and_then(|l| l.income)
            .unwrap_or(0)
    }

    /// 地形名から「生産施設かどうか」を判定する
    /// 補給補充フィールド（supply_type）が存在する地形を生産施設とみなす
    pub fn is_production_facility(&self, name: &str) -> bool {
        self.get_landscape_by_name(name)
            .map(|l| l.supply_type.is_some())
            .unwrap_or(false)
    }

    /// 施設（地形名）でその移動タイプのユニットを生産できるか判定する
    /// 施設の supply_type と unit の movement_type を照合する:
    ///   - 地上部隊: 歩兵・戦車・砲台・装甲車 移動タイプ
    ///   - 航空部隊: 航空 移動タイプ
    ///   - 艦船部隊: 艦船 移動タイプ
    pub fn can_produce_unit(
        &self,
        landscape_name: &str,
        unit_type: crate::resources::UnitType,
    ) -> bool {
        let Some(landscape) = self.get_landscape_by_name(landscape_name) else {
            return false;
        };
        let Some(supply_type) = &landscape.supply_type else {
            return false;
        };

        if let Some(allowed_units) = self.categories.get(supply_type) {
            allowed_units.contains(&unit_type)
        } else {
            // カテゴリ名でない場合は、直接のユニット名として一致するか確認
            if let Some(target_type) = crate::resources::UnitType::from_str(supply_type) {
                target_type == unit_type
            } else {
                false
            }
        }
    }
}

fn parse_map(csv_data: &str) -> Result<MapData, MasterDataError> {
    let mut cells = Vec::new();
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(csv_data.as_bytes());

    let mut width = 0;
    for result in rdr.records() {
        let record = result?;
        let mut row = Vec::new();
        for field in record.iter() {
            let val: u32 = field.trim().parse()?;
            row.push(val);
        }
        if width == 0 {
            width = row.len();
        } else if row.len() != width {
            return Err(MasterDataError::InvalidMapWidth {
                expected: width,
                actual: row.len(),
            });
        }
        cells.push(row);
    }

    Ok(MapData {
        width,
        height: cells.len(),
        cells,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_landscape() {
        let registry = MasterDataRegistry::load().expect("Failed to load master data");

        let capital = registry
            .get_landscape_by_name("首都")
            .expect("Capital not found");
        assert_eq!(capital.id, LandscapeId(1));
        assert_eq!(capital.durability, 400);
        assert_eq!(capital.defense_bonus, 50);
        assert_eq!(capital.supply_type.as_deref(), Some("地上部隊"));
        assert_eq!(capital.income, Some(4000));

        let road = registry
            .get_landscape_by_name("道路")
            .expect("Road not found");
        assert_eq!(road.income, None);
    }

    #[test]
    fn test_load_unit() {
        let registry = MasterDataRegistry::load().unwrap();

        let tank = registry
            .get_unit(&UnitName("重戦車".to_string()))
            .expect("Heavy Tank not found");
        assert_eq!(tank.cost, 28000);
        assert_eq!(tank.movement, 4);
        assert_eq!(tank.movement_type, crate::resources::MovementType::Tank);
        assert_eq!(tank.weapon1.as_deref(), Some("戦車砲S"));
        assert_eq!(tank.weapon2.as_deref(), Some("機銃S"));
    }

    #[test]
    fn test_load_weapon_and_damage() {
        let registry = MasterDataRegistry::load().unwrap();

        let dmg = registry.get_damage(&UnitName("戦車砲S".to_string()), "重戦車");
        assert_eq!(dmg, Some(47));

        let cant_atk = registry.get_damage(&UnitName("地対空ミサイルA".to_string()), "軽歩兵");
        assert_eq!(cant_atk, None);
    }

    #[test]
    fn test_load_movement() {
        let registry = MasterDataRegistry::load().unwrap();

        // 歩兵 in 森 should be 2
        assert_eq!(
            registry.get_movement_cost(crate::resources::MovementType::Infantry, "森"),
            Some(2)
        );
        // 戦車 in 山 should be 99
        assert_eq!(
            registry.get_movement_cost(crate::resources::MovementType::Tank, "山"),
            Some(99)
        );
    }

    #[test]
    fn test_load_map() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = registry.get_map("map_1").expect("map_1 not found");

        assert_eq!(map.width, 10);
        assert_eq!(map.height, 14);

        // Check decoding at specific known coordinates from the csv output we saw
        // Cell (0, 0) was '12' -> player 0, terrain 12 (海)
        let cell_0_0 = map.get_cell(0, 0).unwrap();
        assert_eq!(cell_0_0.player_id, 0);
        assert_eq!(cell_0_0.terrain_id, LandscapeId(12));

        // Cell (1, 7) was '202' -> player 2, terrain 2 (都市)
        // Wait, cell (1, 7) meaning y=7 (row 8), x=1
        // Let's verify (1, 7)
        let cell = map.get_cell(1, 7).unwrap();
        assert_eq!(cell.player_id, 2);
        assert_eq!(cell.terrain_id, LandscapeId(2));

        // Cell (3, 11) is (x=3, y=11) -> 201 -> player 2, terrain 1 (首都)
        let cell_capital = map.get_cell(3, 11).unwrap();
        assert_eq!(cell_capital.player_id, 2);
        assert_eq!(cell_capital.terrain_id, LandscapeId(1));
    }

    #[test]
    fn test_load_loads() {
        let registry = MasterDataRegistry::load().unwrap();
        let inf_loads = registry
            .loads
            .get("輸送ヘリ")
            .expect("輸送ヘリ loads not found");
        assert!(!inf_loads.is_empty());
        assert_eq!(inf_loads[0].transport, "輸送ヘリ");
        assert_eq!(inf_loads[0].target, "歩兵");
        assert_eq!(inf_loads[0].capacity, 2);
    }

    #[test]
    fn test_landscape_income() {
        let registry = MasterDataRegistry::load().unwrap();
        // 首都は収入4000
        assert_eq!(registry.landscape_income("首都"), 4000);
        // 道路は収入なし
        assert_eq!(registry.landscape_income("道路"), 0);
        // 存在しない地形
        assert_eq!(registry.landscape_income("存在しない"), 0);
    }

    #[test]
    fn test_is_production_facility() {
        let registry = MasterDataRegistry::load().unwrap();
        // 首都は生産施設（supply_typeあり）
        assert!(registry.is_production_facility("首都"));
        // 道路は生産施設ではない
        assert!(!registry.is_production_facility("道路"));
    }

    #[test]
    fn test_can_produce_unit() {
        let registry = MasterDataRegistry::load().unwrap();
        // 首都（地上部隊）で歩兵生産可能
        assert!(registry.can_produce_unit("首都", crate::resources::UnitType::Infantry));
        // 首都で航空は生産不可
        assert!(!registry.can_produce_unit("首都", crate::resources::UnitType::Fighter));
    }

    #[test]
    fn test_expand_target() {
        let registry = MasterDataRegistry::load().unwrap();
        // カテゴリ展開
        let units = registry.expand_target("歩兵").unwrap();
        assert!(units.contains(&crate::resources::UnitType::Infantry));
        assert!(units.contains(&crate::resources::UnitType::Mech));
        assert_eq!(units.len(), 2);

        // 個別名称
        let units = registry.expand_target("軽歩兵").unwrap();
        assert_eq!(units, vec![crate::resources::UnitType::Infantry]);

        // 不明な名称
        let units = registry.expand_target("存在しないユニット");
        assert!(units.is_err());
    }
}
