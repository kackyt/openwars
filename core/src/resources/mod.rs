use bevy_ecs::prelude::*;
use rand::{SeedableRng, rngs::StdRng};
use std::collections::HashMap;

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
    /// Hex topology is currently unsupported.
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
        assert!(
            topology != GridTopology::Hex,
            "Hex topology is currently unsupported"
        );
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

    pub fn set_terrain(
        &mut self,
        x: usize,
        y: usize,
        terrain: Terrain,
    ) -> Result<(), &'static str> {
        if x < self.width && y < self.height {
            self.tiles[y * self.width + x] = terrain;
            Ok(())
        } else {
            Err("Out of bounds")
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
                unimplemented!("GridTopology::Hex is currently unsupported");
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
            GridTopology::Hex => unimplemented!("GridTopology::Hex is currently unsupported"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Player {
    pub id: u32,
    pub name: String,
    pub funds: u32,
}

impl Player {
    pub fn new(id: u32, name: String) -> Self {
        Self { id, name, funds: 0 }
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
    Winner(u32),
    Draw,
}

#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct MatchState {
    pub current_turn_number: u32,
    pub active_player_index: usize,
    pub current_phase: Phase,
    pub game_over: Option<GameOverCondition>,
}

impl Default for MatchState {
    fn default() -> Self {
        Self {
            current_turn_number: 1,
            active_player_index: 0,
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

#[derive(Resource)]
pub struct GameRng(pub StdRng);

impl Default for GameRng {
    fn default() -> Self {
        Self(StdRng::from_entropy())
    }
}
