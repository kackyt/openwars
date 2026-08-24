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
    #[error("ROMシナリオデータが不正です: {0}")]
    InvalidRomScenarioData(String),
    #[error("不明なマスターデータ読み込みエラー")]
    Unknown,
}

// include the generated maps from build.rs
include!(concat!(env!("OUT_DIR"), "/generated_maps.rs"));

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

/// ROMシナリオとマップCSVを対応付けるマップ名のNewtype。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MapName(pub String);

impl MapName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// ROMシナリオ値の採取元を表すNewtype。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomScenarioSource(pub String);

impl RomScenarioSource {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

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
    #[serde(rename = "日毎燃料消費量")]
    pub daily_fuel: u32,
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

/// Game Boy Wars TurboのROMから抽出した、マップごとのAI入力値。
///
/// `map_name` はマップCSVのファイル名（拡張子なし）と対応する。AI実装にマップ名や
/// シナリオ値を埋め込まず、マスターデータとして差し替えられるように保持する。
#[derive(Debug, Clone)]
pub struct RomScenarioRecord {
    pub map_name: MapName,
    /// `rom`はROM実測値、`generated:model`は未知マップを意味モデルから生成した値。
    pub source: RomScenarioSource,
    pub restricted_radius: u32,
    pub opening_limit: u32,
    pub recon_uses_mission_three: bool,
    pub strategic_objectives: [[(u8, u8); 2]; 2],
    pub unit_value_profiles: [usize; 2],
    pub production_limits: [[u8; 24]; 4],
}

#[derive(Debug, Deserialize)]
struct RomScenarioCsvRecord {
    map_name: String,
    source: String,
    restricted_radius: u32,
    opening_limit: u32,
    recon_uses_mission_three: bool,
    player1_objective1_x: u8,
    player1_objective1_y: u8,
    player1_objective2_x: u8,
    player1_objective2_y: u8,
    player2_objective1_x: u8,
    player2_objective1_y: u8,
    player2_objective2_x: u8,
    player2_objective2_y: u8,
    player1_unit_value_profile: usize,
    player2_unit_value_profile: usize,
    opening_production_limits: String,
    disadvantage_production_limits: String,
    advantage_production_limits: String,
    draw_production_limits: String,
}

impl TryFrom<RomScenarioCsvRecord> for RomScenarioRecord {
    type Error = MasterDataError;

    fn try_from(record: RomScenarioCsvRecord) -> Result<Self, Self::Error> {
        if record.source != "rom" && !record.source.starts_with("generated:") {
            return Err(MasterDataError::InvalidRomScenarioData(format!(
                "{}: source must be rom or generated:<kind>",
                record.map_name
            )));
        }
        if record.player1_unit_value_profile > 3 || record.player2_unit_value_profile > 3 {
            return Err(MasterDataError::InvalidRomScenarioData(format!(
                "{}: player unit value profiles must be 0..=3",
                record.map_name
            )));
        }

        Ok(Self {
            map_name: MapName(record.map_name.clone()),
            source: RomScenarioSource(record.source),
            restricted_radius: record.restricted_radius,
            opening_limit: record.opening_limit,
            recon_uses_mission_three: record.recon_uses_mission_three,
            strategic_objectives: [
                [
                    (record.player1_objective1_x, record.player1_objective1_y),
                    (record.player1_objective2_x, record.player1_objective2_y),
                ],
                [
                    (record.player2_objective1_x, record.player2_objective1_y),
                    (record.player2_objective2_x, record.player2_objective2_y),
                ],
            ],
            unit_value_profiles: [
                record.player1_unit_value_profile,
                record.player2_unit_value_profile,
            ],
            production_limits: [
                parse_rom_production_limits(&record.map_name, &record.opening_production_limits)?,
                parse_rom_production_limits(
                    &record.map_name,
                    &record.disadvantage_production_limits,
                )?,
                parse_rom_production_limits(&record.map_name, &record.advantage_production_limits)?,
                parse_rom_production_limits(&record.map_name, &record.draw_production_limits)?,
            ],
        })
    }
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
    pub unit_order: Vec<UnitName>,
    pub weapons: HashMap<UnitName, WeaponRecord>,
    pub movements: HashMap<crate::resources::MovementType, MovementRecord>,
    pub loads: HashMap<String, Vec<LoadRecord>>,
    pub categories: HashMap<String, Vec<crate::resources::UnitType>>,
    pub maps: HashMap<String, MapData>,
    pub rom_scenarios: HashMap<String, RomScenarioRecord>,
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
            registry.unit_order.push(record.name.clone());
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
        for (name, content) in MAPS {
            let map = parse_map(content)?;
            registry.maps.insert(name.to_string(), map);
        }

        // 8. ROM由来AIシナリオデータ読み込み
        // マップ名、戦略目標、生産上限などをAI実装から分離して登録します。
        let rom_scenario_csv = include_str!("master_data/rom_scenario.csv");
        let mut rdr = csv::Reader::from_reader(rom_scenario_csv.as_bytes());
        for result in rdr.deserialize::<RomScenarioCsvRecord>() {
            let record = RomScenarioRecord::try_from(result?)?;
            let map_name = record.map_name.0.clone();
            if !registry.maps.contains_key(&map_name) {
                return Err(MasterDataError::InvalidRomScenarioData(format!(
                    "{}: corresponding map CSV is missing",
                    map_name
                )));
            }
            if registry
                .rom_scenarios
                .insert(map_name.clone(), record)
                .is_some()
            {
                return Err(MasterDataError::InvalidRomScenarioData(format!(
                    "duplicate map_name: {map_name}"
                )));
            }
        }

        // 9. 整合性バリデーション
        // 地形(Landscape)の補給タイプ(supply_type)が、存在するカテゴリまたはユニット種別であるか検証します。
        for landscape in registry.landscapes.values() {
            if let Some(supply_type) = &landscape.supply_type
                && !registry.categories.contains_key(supply_type)
                && crate::resources::UnitType::from_str(supply_type).is_none()
            {
                return Err(MasterDataError::InvalidCategoryName(supply_type.clone()));
            }
        }

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

    /// 組み込み済みマップ名を数値順で返す。CLI・MCPの選択肢で共通利用する。
    pub fn map_names(&self) -> Vec<String> {
        let mut names = self.maps.keys().cloned().collect::<Vec<_>>();
        names.sort_by(|left, right| map_name_sort_key(left).cmp(&map_name_sort_key(right)));
        names
    }

    /// ROM互換AIが利用するマップ固有のシナリオ入力を返す。
    pub fn get_rom_scenario(&self, map_name: &str) -> Option<&RomScenarioRecord> {
        self.rom_scenarios.get(map_name)
    }

    /// 地形名からターンごとの収入を返す（マスターデータのincomeフィールドを参照）
    /// 収入フィールドがない地形（道路・平地など）は 0 を返す
    pub fn landscape_income(&self, name: &str) -> u32 {
        self.get_landscape_by_name(name)
            .and_then(|l| l.income)
            .unwrap_or(0)
    }

    /// 地形名から地形の耐久度（占領ポイント最大値）を返す
    pub fn landscape_durability(&self, name: &str) -> u32 {
        self.get_landscape_by_name(name)
            .map(|l| l.durability)
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

    /// 地形から地形効果(防御ボーナス)を返す
    pub fn get_terrain_defense_bonus(&self, terrain: crate::resources::Terrain) -> u32 {
        self.get_landscape_by_name(terrain.as_str())
            .map(|l| l.defense_bonus)
            .unwrap_or(0)
    }

    /// 施設（地形）でそのユニットを補給・回復できるか判定する
    pub fn can_repair_on_terrain(
        &self,
        unit_type: crate::resources::UnitType,
        terrain: crate::resources::Terrain,
    ) -> bool {
        self.can_produce_unit(terrain.as_str(), unit_type)
    }

    /// ユニット名(UnitName)からコンポーネントとしての UnitStats を構築して返す。
    /// マスターデータに不備がある場合は MasterDataError を返す。
    pub fn create_unit_stats(
        &self,
        name: &UnitName,
    ) -> Result<crate::components::UnitStats, MasterDataError> {
        let record = self
            .get_unit(name)
            .ok_or_else(|| MasterDataError::InvalidUnitName(name.0.clone()))?;
        let u_type = self.unit_type_for_name(&name.0)?;

        let mut min_range = 0;
        let mut max_range = 0;

        let w1 = record
            .weapon1
            .as_ref()
            .map(|w| {
                self.weapons
                    .get(&UnitName(w.clone()))
                    .ok_or_else(|| MasterDataError::InvalidUnitName(w.clone()))
            })
            .transpose()?;

        let w2 = record
            .weapon2
            .as_ref()
            .map(|w| {
                self.weapons
                    .get(&UnitName(w.clone()))
                    .ok_or_else(|| MasterDataError::InvalidUnitName(w.clone()))
            })
            .transpose()?;

        if let Some(w) = w1 {
            min_range = w.range_min;
            max_range = w.range_max;
        } else if let Some(w) = w2 {
            min_range = w.range_min;
            max_range = w.range_max;
        }

        let can_capture = u_type == crate::resources::UnitType::Infantry
            || u_type == crate::resources::UnitType::Mech;
        let can_supply = u_type == crate::resources::UnitType::SupplyTruck;

        let mut max_cargo = 0;
        let mut loadable = Vec::new();
        if let Some(loads) = self.loads.get(&name.0) {
            for load_record in loads {
                max_cargo = max_cargo.max(load_record.capacity);
                let expanded = self.expand_target(&load_record.target)?;
                loadable.extend(expanded);
            }
        }
        Ok(crate::components::UnitStats {
            unit_type: u_type,
            cost: record.cost,
            max_movement: record.movement,
            movement_type: record.movement_type,
            max_fuel: record.fuel,
            max_ammo1: w1.map(|w| w.ammo).unwrap_or(0),
            ammo1_cost: w1.map(|w| w.supply_cost).unwrap_or(0),
            max_ammo2: w2.map(|w| w.ammo).unwrap_or(0),
            ammo2_cost: w2.map(|w| w.supply_cost).unwrap_or(0),
            min_range,
            max_range,
            daily_fuel_consumption: record.daily_fuel,
            can_capture,
            can_supply,
            max_cargo,
            loadable_unit_types: loadable,
        })
    }
}

/// `map_10` を `map_2` より後に並べるための自然順キー。
fn map_name_sort_key(map_name: &str) -> (u32, &str) {
    let number = map_name
        .strip_prefix("map_")
        .and_then(|suffix| suffix.parse::<u32>().ok())
        .unwrap_or(u32::MAX);
    (number, map_name)
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

/// セミコロン区切りのROM兵種24枠を、ROM上の順序を保ってパースする。
fn parse_rom_production_limits(map_name: &str, values: &str) -> Result<[u8; 24], MasterDataError> {
    let parsed = values
        .split(';')
        .map(str::trim)
        .map(|value| {
            value.parse::<u8>().map_err(|error| {
                MasterDataError::InvalidRomScenarioData(format!(
                    "{map_name}: invalid production limit '{value}': {error}"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    parsed.try_into().map_err(|values: Vec<u8>| {
        MasterDataError::InvalidRomScenarioData(format!(
            "{map_name}: production limit requires 24 values, got {}",
            values.len()
        ))
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
        // map_1 の確認
        let map1 = registry.get_map("map_1").expect("map_1 not found");
        assert_eq!(map1.width, 10);
        assert_eq!(map1.height, 14);

        // map_3 の確認
        let map3 = registry.get_map("map_3").expect("map_3 not found");
        assert!(map3.width > 0);
        assert!(map3.height > 0);

        // map_4 の確認（画像解析により追加・調整された 777 マップ）
        let map4 = registry.get_map("map_4").expect("map_4 not found");
        assert_eq!(map4.width, 28);
        assert_eq!(map4.height, 18);
        // P1首都 (x=4, y=3) -> 101
        let cell_p1_cap = map4.get_cell(4, 3).unwrap();
        assert_eq!(cell_p1_cap.player_id, 1);
        assert_eq!(cell_p1_cap.terrain_id, LandscapeId(1));
        // P2首都 (x=23, y=12) -> 201
        let cell_p2_cap = map4.get_cell(23, 12).unwrap();
        assert_eq!(cell_p2_cap.player_id, 2);
        assert_eq!(cell_p2_cap.terrain_id, LandscapeId(1));
        // P1空港 (x=2, y=2) -> 104
        let cell_p1_air = map4.get_cell(2, 2).unwrap();
        assert_eq!(cell_p1_air.player_id, 1);
        assert_eq!(cell_p1_air.terrain_id, LandscapeId(4));

        // map_5 の確認（画像解析により追加・調整された 双子島 / 橋マップ）
        let map5 = registry.get_map("map_5").expect("map_5 not found");
        assert_eq!(map5.width, 10);
        assert_eq!(map5.height, 10);
        // P1首都 (x=0, y=5) -> 101
        let cell_p1_cap5 = map5.get_cell(0, 5).unwrap();
        assert_eq!(cell_p1_cap5.player_id, 1);
        assert_eq!(cell_p1_cap5.terrain_id, LandscapeId(1));
        // P2首都 (x=9, y=5) -> 201
        let cell_p2_cap5 = map5.get_cell(9, 5).unwrap();
        assert_eq!(cell_p2_cap5.player_id, 2);
        assert_eq!(cell_p2_cap5.terrain_id, LandscapeId(1));
        // 橋 (x=2, y=5) -> 7
        let cell_bridge = map5.get_cell(2, 5).unwrap();
        assert_eq!(cell_bridge.player_id, 0);
        assert_eq!(cell_bridge.terrain_id, LandscapeId(7));

        // map_6 の確認（画像解析により追加・調整された シルクロード マップ）
        let map6 = registry.get_map("map_6").expect("map_6 not found");
        assert_eq!(map6.width, 30);
        assert_eq!(map6.height, 22);
        // P1首都 (x=4, y=16) -> 101
        let cell_p1_cap6 = map6.get_cell(4, 16).unwrap();
        assert_eq!(cell_p1_cap6.player_id, 1);
        assert_eq!(cell_p1_cap6.terrain_id, LandscapeId(1));
        // P2首都 (x=25, y=5) -> 201
        let cell_p2_cap6 = map6.get_cell(25, 5).unwrap();
        assert_eq!(cell_p2_cap6.player_id, 2);
        assert_eq!(cell_p2_cap6.terrain_id, LandscapeId(1));
        // 中立空港 (x=14, y=9) -> 4
        let cell_air6 = map6.get_cell(14, 9).unwrap();
        assert_eq!(cell_air6.player_id, 0);
        assert_eq!(cell_air6.terrain_id, LandscapeId(4));

        // map_7 の確認（画像解析により追加・調整された 鬼ヶ島 / クレーター マップ）
        let map7 = registry.get_map("map_7").expect("map_7 not found");
        assert_eq!(map7.width, 20);
        assert_eq!(map7.height, 19);
        // P1首都 (x=1, y=8) -> 101
        let cell_p1_cap7 = map7.get_cell(1, 8).unwrap();
        assert_eq!(cell_p1_cap7.player_id, 1);
        assert_eq!(cell_p1_cap7.terrain_id, LandscapeId(1));
        // P2首都 (x=16, y=7) -> 201
        let cell_p2_cap7 = map7.get_cell(16, 7).unwrap();
        assert_eq!(cell_p2_cap7.player_id, 2);
        assert_eq!(cell_p2_cap7.terrain_id, LandscapeId(1));
        // 中立空港 (x=8, y=8) -> 4
        let cell_air7 = map7.get_cell(8, 8).unwrap();
        assert_eq!(cell_air7.player_id, 0);
        assert_eq!(cell_air7.terrain_id, LandscapeId(4));
        // 上の橋 (x=6, y=3) -> 7
        let cell_bridge7 = map7.get_cell(6, 3).unwrap();
        assert_eq!(cell_bridge7.player_id, 0);
        assert_eq!(cell_bridge7.terrain_id, LandscapeId(7));

        // map_8 の確認（画像解析により追加・調整された ナガレジマ マップ）
        let map8 = registry.get_map("map_8").expect("map_8 not found");
        assert_eq!(map8.width, 22);
        assert_eq!(map8.height, 19);
        // P1首都 (x=4, y=9) -> 101
        let cell_p1_cap8 = map8.get_cell(4, 9).unwrap();
        assert_eq!(cell_p1_cap8.player_id, 1);
        assert_eq!(cell_p1_cap8.terrain_id, LandscapeId(1));
        // P2首都 (x=16, y=10) -> 201
        let cell_p2_cap8 = map8.get_cell(16, 10).unwrap();
        assert_eq!(cell_p2_cap8.player_id, 2);
        assert_eq!(cell_p2_cap8.terrain_id, LandscapeId(1));
        // 上の橋 (x=11, y=4) -> 7
        let cell_bridge8_top = map8.get_cell(11, 4).unwrap();
        assert_eq!(cell_bridge8_top.player_id, 0);
        assert_eq!(cell_bridge8_top.terrain_id, LandscapeId(7));
        // 下の橋 (x=10, y=14) -> 7
        let cell_bridge8_bottom = map8.get_cell(10, 14).unwrap();
        assert_eq!(cell_bridge8_bottom.player_id, 0);
        assert_eq!(cell_bridge8_bottom.terrain_id, LandscapeId(7));

        // map_9 の確認（画像解析により追加・調整された インヨウジマ マップ）
        let map9 = registry.get_map("map_9").expect("map_9 not found");
        assert_eq!(map9.width, 26);
        assert_eq!(map9.height, 26);
        // P1首都 (x=6, y=4) -> 101
        let cell_p1_cap9 = map9.get_cell(6, 4).unwrap();
        assert_eq!(cell_p1_cap9.player_id, 1);
        assert_eq!(cell_p1_cap9.terrain_id, LandscapeId(1));

        // map_10 の確認（画像解析により追加された タマゴジマ / クレーター マップ）
        let map10 = registry.get_map("map_10").expect("map_10 not found");
        assert_eq!(map10.width, 30);
        assert_eq!(map10.height, 30);
        // P1首都 (x=16, y=13) -> 101
        let cell_p1_cap10 = map10.get_cell(16, 13).unwrap();
        assert_eq!(cell_p1_cap10.player_id, 1);
        assert_eq!(cell_p1_cap10.terrain_id, LandscapeId(1));
        // P2首都 (x=26, y=26) -> 201
        let cell_p2_cap10 = map10.get_cell(26, 26).unwrap();
        assert_eq!(cell_p2_cap10.player_id, 2);
        assert_eq!(cell_p2_cap10.terrain_id, LandscapeId(1));

        // map_11 の確認（画像解析により追加された チエノワ マップ）
        let map11 = registry.get_map("map_11").expect("map_11 not found");
        assert_eq!(map11.width, 30);
        assert_eq!(map11.height, 30);
        // P1首都 (x=6, y=6) -> 101
        let cell_p1_cap11 = map11.get_cell(6, 6).unwrap();
        assert_eq!(cell_p1_cap11.player_id, 1);
        assert_eq!(cell_p1_cap11.terrain_id, LandscapeId(1));
        // P2首都 (x=23, y=23) -> 201
        let cell_p2_cap11 = map11.get_cell(23, 23).unwrap();
        assert_eq!(cell_p2_cap11.player_id, 2);
        assert_eq!(cell_p2_cap11.terrain_id, LandscapeId(1));

        // Check decoding at specific known coordinates from the csv output we saw
        // Cell (0, 0) was '12' -> player 0, terrain 12 (海)
        let cell_0_0 = map1.get_cell(0, 0).unwrap();
        assert_eq!(cell_0_0.player_id, 0);
        assert_eq!(cell_0_0.terrain_id, LandscapeId(12));

        // Cell (1, 7) was '202' -> player 2, terrain 2 (都市)
        // Wait, cell (1, 7) meaning y=7 (row 8), x=1
        // Let's verify (1, 7)
        let cell = map1.get_cell(1, 7).unwrap();
        assert_eq!(cell.player_id, 2);
        assert_eq!(cell.terrain_id, LandscapeId(2));

        // Cell (3, 11) is (x=3, y=11) -> 201 -> player 2, terrain 1 (首都)
        let cell_capital = map1.get_cell(3, 11).unwrap();
        assert_eq!(cell_capital.player_id, 2);
        assert_eq!(cell_capital.terrain_id, LandscapeId(1));
    }

    #[test]
    fn test_load_rom_scenarios_from_csv_with_map_keys() {
        let registry = MasterDataRegistry::load().unwrap();

        // マップ名はAIコードではなくrom_scenario.csvのmap_name列で対応付ける。
        assert_eq!(registry.rom_scenarios.len(), 53);
        let map_names = registry.map_names();
        assert_eq!(map_names.len(), 53);
        assert_eq!(map_names.first().map(String::as_str), Some("map_1"));
        assert_eq!(map_names.last().map(String::as_str), Some("map_53"));
        for map_number in 1..=53 {
            let map_name = format!("map_{map_number}");
            let scenario = registry
                .get_rom_scenario(&map_name)
                .unwrap_or_else(|| panic!("ROM scenario for {map_name} is missing"));
            assert_eq!(scenario.map_name.as_str(), map_name);
            assert!(registry.get_map(&map_name).is_some());
            if matches!(map_number, 9 | 10) {
                assert_eq!(scenario.source.as_str(), "generated:model");
            } else {
                assert_eq!(scenario.source.as_str(), "rom");
            }
        }
    }

    #[test]
    fn invalid_production_limit_identifies_its_map_and_value() {
        let error = parse_rom_production_limits("map_test", "1;not-a-number").unwrap_err();
        let message = error.to_string();

        assert!(message.contains("map_test"));
        assert!(message.contains("not-a-number"));
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

    #[test]
    fn test_master_data_completeness() {
        use crate::resources::*;
        let registry = MasterDataRegistry::load().unwrap();

        // 1. unit.csv に定義されているすべてのユニットが、割り当てられた武器を weapon.csv に持っているか確認
        for (unit_name, unit_rec) in &registry.units {
            if let Some(w1_name) = &unit_rec.weapon1 {
                assert!(
                    registry.weapons.contains_key(&UnitName(w1_name.clone())),
                    "Unit '{}' has weapon1 '{}' which is missing from weapon.csv",
                    unit_name.0,
                    w1_name
                );
            }
            if let Some(w2_name) = &unit_rec.weapon2 {
                assert!(
                    registry.weapons.contains_key(&UnitName(w2_name.clone())),
                    "Unit '{}' has weapon2 '{}' which is missing from weapon.csv",
                    unit_name.0,
                    w2_name
                );
            }
        }

        // 2. UnitType Enum のすべてのバリアントが unit.csv に存在するか確認
        for &(unit_type, name) in UNIT_TYPE_MAP {
            assert!(
                registry.units.contains_key(&UnitName(name.to_string())),
                "unit.csv is missing record for UnitType::{:?} ({})",
                unit_type,
                name
            );
        }

        // 3. Terrain Enum のすべてのバリアントが landscape.csv に存在するか確認
        for &(terrain, name) in TERRAIN_MAP {
            assert!(
                registry.get_landscape_by_name(name).is_some(),
                "landscape.csv is missing record for Terrain::{:?} ({})",
                terrain,
                name
            );
        }
    }

    #[test]
    fn test_can_repair_on_terrain() {
        let registry = MasterDataRegistry::load().unwrap();
        use crate::resources::{Terrain, UnitType};

        // 首都（地上部隊補給可能）で歩兵は修理可能
        assert!(registry.can_repair_on_terrain(UnitType::Infantry, Terrain::Capital));
        // 首都で重戦車も修理可能（地上部隊カテゴリに含まれるため）
        assert!(registry.can_repair_on_terrain(UnitType::TankZ, Terrain::Capital));

        // 首都で戦闘機は修理不可（地上部隊カテゴリに含まれない）
        assert!(!registry.can_repair_on_terrain(UnitType::Fighter, Terrain::Capital));

        // 道路では歩兵は修理不可
        assert!(!registry.can_repair_on_terrain(UnitType::Infantry, Terrain::Road));

        // 現在の実装が can_produce_unit に委譲していることを確認
        assert_eq!(
            registry.can_repair_on_terrain(UnitType::Infantry, Terrain::Capital),
            registry.can_produce_unit(Terrain::Capital.as_str(), UnitType::Infantry)
        );
    }
}
