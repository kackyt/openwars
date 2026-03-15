use crate::components::PlayerId;
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
use bevy_ecs::prelude::*;
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("座標 ({x}, {y}) がマップ境界外です")]
    OutOfBounds { x: usize, y: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitType {
    Infantry,
    Mech,
    Recon,
    Tank,
    MdTank,
    TankZ, // Example
    Artillery,
    Rockets,
    AntiAir,
    Missiles,
    Fighter,
    Bomber,
    Bcopters,
    TransportHelicopter,
    Battleship,
    Cruiser,
    Lander,
    Submarine,
    SupplyTruck,
    CombatEngineer, // For repairs/capturing specifically if needed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovementType {
    Foot,
    Vehicle,
    Tracked,
    Tires,
    LowAltitude,
    HighAltitude,
    Ship,
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

impl Terrain {
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

    pub fn defense_stars(&self) -> u32 {
        match self {
            Terrain::Mountain => 4,
            Terrain::City
            | Terrain::Factory
            | Terrain::Airport
            | Terrain::Port
            | Terrain::Capital => 3,
            Terrain::Forest => 2,
            Terrain::Plains => 1,
            _ => 0,
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
    Production,
    MovementAndAttack,
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
            current_phase: Phase::Production,
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
