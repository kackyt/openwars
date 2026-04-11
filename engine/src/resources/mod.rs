pub mod master_data;
pub use master_data::MasterDataRegistry;

use crate::components::PlayerId;
use bevy_ecs::prelude::*;

pub fn init_master_data(world: &mut World) -> Result<(), master_data::MasterDataError> {
    let registry = master_data::MasterDataRegistry::load()?;
    world.insert_resource(registry);
    Ok(())
}
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::SystemTime;

#[derive(Resource, Debug, Clone)]
pub struct GameRng {
    pub seed: u64,
}

impl Default for GameRng {
    fn default() -> Self {
        let mut h = DefaultHasher::new();
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            .hash(&mut h);
        Self { seed: h.finish() }
    }
}

impl GameRng {
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }
    pub fn next_bonus(&mut self) -> u32 {
        self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        ((self.seed >> 33) % 11) as u32
    }
}
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("座標 ({x}, {y}) がマップ境界外です")]
    OutOfBounds { x: usize, y: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UnitType {
    #[default]
    Infantry,
    Mech,
    Recon,
    Tank,
    MdTank,
    TankZ,
    Artillery,
    LightSpGun,
    HeavySpGun,
    Rockets,
    AntiAir,
    Missiles,
    Fighter,
    HeavyFighter,
    Bomber,
    Bcopters,
    TransportHelicopter,
    Battleship,
    Carrier,
    Lander,
    SupplyTruck,
}

const UNIT_TYPE_MAP: &[(UnitType, &str)] = &[
    (UnitType::Infantry, "軽歩兵"),
    (UnitType::Mech, "重歩兵"),
    (UnitType::Recon, "装甲車"),
    (UnitType::Tank, "軽戦車"),
    (UnitType::MdTank, "中戦車"),
    (UnitType::TankZ, "重戦車"),
    (UnitType::Artillery, "砲台"),
    (UnitType::LightSpGun, "軽自走砲"),
    (UnitType::HeavySpGun, "重自走砲"),
    (UnitType::Rockets, "ロケットランチャー"),
    (UnitType::AntiAir, "対空戦車"),
    (UnitType::Missiles, "対空ミサイル"),
    (UnitType::Fighter, "軽戦闘機"),
    (UnitType::HeavyFighter, "重戦闘機"),
    (UnitType::Bomber, "爆撃機"),
    (UnitType::Bcopters, "戦闘ヘリ"),
    (UnitType::TransportHelicopter, "輸送ヘリ"),
    (UnitType::Battleship, "戦艦"),
    (UnitType::Carrier, "空母"),
    (UnitType::Lander, "輸送船"),
    (UnitType::SupplyTruck, "補給輸送車"),
];

impl UnitType {
    pub fn as_str(&self) -> &'static str {
        UNIT_TYPE_MAP
            .iter()
            .find(|(t, _)| t == self)
            .map(|(_, s)| *s)
            .unwrap_or("不明")
    }

    pub fn symbol(&self) -> &'static str {
        use UnitType::*;
        match self {
            Infantry => "i",
            Mech => "I",
            Recon => "R",
            Tank => "T",
            MdTank => "M",
            TankZ => "Z",
            Artillery => "a",
            LightSpGun => "g",
            HeavySpGun => "G",
            Rockets => "r",
            AntiAir => "A",
            Missiles => "m",
            Fighter => "F",
            HeavyFighter => "H",
            Bomber => "B",
            Bcopters => "b",
            TransportHelicopter => "h",
            Battleship => "S",
            Carrier => "C",
            Lander => "l",
            SupplyTruck => "t",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        UNIT_TYPE_MAP
            .iter()
            .find(|(_, name)| *name == s)
            .map(|(t, _)| *t)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MovementType {
    #[default]
    Infantry,
    Tank,
    Artillery,
    ArmoredCar,
    Air,
    Ship,
}

const MOVEMENT_TYPE_MAP: &[(MovementType, &str)] = &[
    (MovementType::Infantry, "歩兵"),
    (MovementType::Tank, "戦車"),
    (MovementType::Artillery, "砲台"),
    (MovementType::ArmoredCar, "装甲車"),
    (MovementType::Air, "航空"),
    (MovementType::Ship, "艦船"),
];

impl MovementType {
    pub fn as_str(&self) -> &'static str {
        MOVEMENT_TYPE_MAP
            .iter()
            .find(|(t, _)| t == self)
            .map(|(_, s)| *s)
            .unwrap_or("不明")
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        MOVEMENT_TYPE_MAP
            .iter()
            .find(|(_, name)| *name == s)
            .map(|(t, _)| *t)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terrain {
    Plains,
    Road,
    River,
    Bridge,
    Mountain,
    Forest,
    Sea,
    Shoal,
    City,
    Factory,
    Airport,
    Port,
    Capital,
}

const TERRAIN_MAP: &[(Terrain, &str)] = &[
    (Terrain::Plains, "平地"),
    (Terrain::Road, "道路"),
    (Terrain::River, "川"),
    (Terrain::Bridge, "橋"),
    (Terrain::Mountain, "山"),
    (Terrain::Forest, "森"),
    (Terrain::Sea, "海"),
    (Terrain::Shoal, "浅瀬"),
    (Terrain::City, "都市"),
    (Terrain::Factory, "工場"),
    (Terrain::Airport, "空港"),
    (Terrain::Port, "港"),
    (Terrain::Capital, "首都"),
];

impl Terrain {
    pub fn as_str(&self) -> &'static str {
        TERRAIN_MAP
            .iter()
            .find(|(t, _)| t == self)
            .map(|(_, s)| *s)
            .unwrap_or("不明")
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        TERRAIN_MAP
            .iter()
            .find(|(_, name)| *name == s)
            .map(|(t, _)| *t)
    }

    pub fn max_capture_points(&self) -> u32 {
        match self {
            Terrain::City
            | Terrain::Factory
            | Terrain::Airport
            | Terrain::Port
            | Terrain::Capital => 200,
            _ => 0,
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            Terrain::Plains => ".",
            Terrain::Road => "=",
            Terrain::River => "~",
            Terrain::Bridge => "=",
            Terrain::Mountain => "^",
            Terrain::Forest => "\"",
            Terrain::Sea => "≈",
            Terrain::Shoal => ",",
            Terrain::City => "C",
            Terrain::Factory => "F",
            Terrain::Airport => "A",
            Terrain::Port => "P",
            Terrain::Capital => "H",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridTopology {
    Square,
    Hex,
}

#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct Map {
    pub width: usize,
    pub height: usize,
    pub tiles: Vec<Terrain>,
    pub topology: GridTopology,
}

impl Map {
    pub fn new(
        width: usize,
        height: usize,
        default_terrain: Terrain,
        topology: GridTopology,
    ) -> Self {
        if topology == GridTopology::Hex {
            unimplemented!("GridTopology::Hex is not currently supported");
        }
        Self {
            width,
            height,
            tiles: vec![default_terrain; width * height],
            topology,
        }
    }

    pub fn get_terrain(&self, x: usize, y: usize) -> Option<Terrain> {
        if x < self.width && y < self.height {
            Some(self.tiles[y * self.width + x])
        } else {
            None
        }
    }

    pub fn set_terrain(&mut self, x: usize, y: usize, terrain: Terrain) -> Result<(), DomainError> {
        if x < self.width && y < self.height {
            self.tiles[y * self.width + x] = terrain;
            Ok(())
        } else {
            Err(DomainError::OutOfBounds { x, y })
        }
    }

    pub fn get_adjacent(&self, x: usize, y: usize) -> Vec<(usize, usize)> {
        let mut adj = Vec::new();
        match self.topology {
            GridTopology::Square => {
                if x > 0 {
                    adj.push((x - 1, y));
                }
                if x + 1 < self.width {
                    adj.push((x + 1, y));
                }
                if y > 0 {
                    adj.push((x, y - 1));
                }
                if y + 1 < self.height {
                    adj.push((x, y + 1));
                }
            }
            GridTopology::Hex => {
                // Implementation depends on hex orientation. Keep simple for now or implement if needed.
            }
        }
        adj
    }

    pub fn distance(&self, x1: usize, y1: usize, x2: usize, y2: usize) -> Option<u32> {
        match self.topology {
            GridTopology::Square => {
                let dx = (x1 as i32 - x2 as i32).abs();
                let dy = (y1 as i32 - y2 as i32).abs();
                Some((dx + dy) as u32)
            }
            GridTopology::Hex => None, // Needs implementation if used
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Player {
    pub id: PlayerId,
    pub name: String,
    pub funds: u32,
}

impl Player {
    pub fn new(id: u32, name: String) -> Self {
        Self {
            id: PlayerId(id),
            name,
            funds: 0,
        }
    }
}

#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct Players(pub Vec<Player>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Main,
    EndTurn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameOverCondition {
    Winner(PlayerId),
    Draw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnNumber(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerIndex(pub usize);

#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct MatchState {
    pub current_turn_number: TurnNumber,
    pub active_player_index: PlayerIndex,
    pub current_phase: Phase,
    pub game_over: Option<GameOverCondition>,
}

impl Default for MatchState {
    fn default() -> Self {
        Self {
            current_turn_number: TurnNumber(1),
            active_player_index: PlayerIndex(0),
            current_phase: Phase::Main,
            game_over: None,
        }
    }
}

#[derive(Resource, Debug, Clone, Default)]
pub struct DamageChart {
    // attacker -> defender -> base damage
    table: HashMap<UnitType, HashMap<UnitType, u32>>,
    secondary_table: HashMap<UnitType, HashMap<UnitType, u32>>,
}

impl DamageChart {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_damage(&mut self, attacker: UnitType, defender: UnitType, damage: u32) {
        self.table
            .entry(attacker)
            .or_default()
            .insert(defender, damage);
    }

    pub fn insert_secondary_damage(&mut self, attacker: UnitType, defender: UnitType, damage: u32) {
        self.secondary_table
            .entry(attacker)
            .or_default()
            .insert(defender, damage);
    }

    pub fn get_base_damage(&self, attacker: UnitType, defender: UnitType) -> Option<u32> {
        self.table
            .get(&attacker)
            .and_then(|row| row.get(&defender))
            .copied()
    }

    pub fn get_base_damage_secondary(&self, attacker: UnitType, defender: UnitType) -> Option<u32> {
        self.secondary_table
            .get(&attacker)
            .and_then(|row| row.get(&defender))
            .copied()
    }
}

#[derive(Resource, Debug, Clone)]
pub struct UnitRegistry(pub std::collections::HashMap<UnitType, crate::components::UnitStats>);

impl UnitRegistry {
    pub fn get_stats(&self, unit_type: UnitType) -> Option<&crate::components::UnitStats> {
        self.0.get(&unit_type)
    }
}
