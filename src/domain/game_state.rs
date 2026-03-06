use crate::domain::map_grid::{Map, Terrain};
use crate::domain::unit_roster::{Unit, UnitStats};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Player {
    pub id: u32,
    pub name: String,
    pub funds: u32, // Production money
}

impl Player {
    pub fn new(id: u32, name: String) -> Self {
        Self { id, name, funds: 0 }
    }

    pub fn add_funds(&mut self, amount: u32) {
        self.funds += amount;
    }

    pub fn spend_funds(&mut self, amount: u32) -> Result<(), &'static str> {
        if self.funds >= amount {
            self.funds -= amount;
            Ok(())
        } else {
            Err("Insufficient funds")
        }
    }
}

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

pub struct MatchState {
    pub map: Map,
    pub units: Vec<Unit>,
    pub properties: HashMap<(usize, usize), u32>,
    pub players: Vec<Player>,
    pub current_turn_number: u32,
    pub active_player_index: usize,
    pub current_phase: Phase,
    pub game_over: Option<GameOverCondition>,
}

impl MatchState {
    pub fn new(map: Map, players: Vec<Player>, properties: HashMap<(usize, usize), u32>) -> Self {
        Self {
            map,
            units: Vec::new(),
            properties,
            players,
            current_turn_number: 1,
            active_player_index: 0,           // Starts with player 0 index
            current_phase: Phase::Production, // Typical sequence
            game_over: None,
        }
    }

    pub fn get_active_player(&self) -> Option<&Player> {
        self.players.get(self.active_player_index)
    }

    pub fn add_budget_for_active_player(&mut self) {
        if self.game_over.is_some() {
            return;
        }

        let active_player_id = self.players[self.active_player_index].id;
        let mut property_count = 0;

        for (&(x, y), &owner_id) in &self.properties {
            if owner_id == active_player_id {
                let terrain = self.map.get_terrain(x, y);
                if terrain == Some(Terrain::City) || terrain == Some(Terrain::Airport) {
                    property_count += 1;
                }
            }
        }

        let budget_increase = property_count * 1000;
        self.players[self.active_player_index].add_funds(budget_increase);
    }

    pub fn advance_turn(&mut self) {
        if self.game_over.is_some() {
            return;
        }

        self.active_player_index += 1;

        // Wrap around players
        if self.active_player_index >= self.players.len() {
            self.active_player_index = 0;
            self.current_turn_number += 1; // All players moved, next full turn day
        }

        self.current_phase = Phase::Production; // Reset phase for new player

        self.add_budget_for_active_player();
        self.check_win_conditions();
    }

    pub fn next_phase(&mut self) {
        if self.game_over.is_some() {
            return;
        }

        match self.current_phase {
            Phase::Production => {
                self.current_phase = Phase::MovementAndAttack;
            }
            Phase::MovementAndAttack => {
                self.current_phase = Phase::EndTurn;
                self.check_win_conditions();
                if self.game_over.is_none() {
                    self.advance_turn(); // Automatically advance turn if ending
                }
            }
            Phase::EndTurn => {
                // Usually handled by advance_turn, but just in case
                self.current_phase = Phase::Production;
            }
        }
    }

    pub fn check_win_conditions(&mut self) {
        if self.game_over.is_some() {
            return;
        }

        let mut alive_players = Vec::new();

        for player in &self.players {
            let mut has_capital = false;
            for (&(x, y), &owner_id) in &self.properties {
                if owner_id == player.id {
                    if let Some(Terrain::Capital) = self.map.get_terrain(x, y) {
                        has_capital = true;
                        break;
                    }
                }
            }

            let has_units = self
                .units
                .iter()
                .any(|u| u.owner_player_id == player.id && !u.is_destroyed());
            let is_annihilated = self.current_turn_number > 1 && !has_units;

            if has_capital && !is_annihilated {
                alive_players.push(player.id);
            }
        }

        if alive_players.len() == 1 {
            self.game_over = Some(GameOverCondition::Winner(alive_players[0]));
        } else if alive_players.is_empty() {
            self.game_over = Some(GameOverCondition::Draw);
        }
    }

    pub fn produce_unit(
        &mut self,
        player_id: u32,
        x: usize,
        y: usize,
        unit_stats: UnitStats,
    ) -> Result<(), &'static str> {
        if self.game_over.is_some() {
            return Err("Game is over");
        }

        if player_id != self.get_active_player().ok_or("No active player")?.id {
            return Err("Not active player's turn to produce");
        }

        if self.properties.get(&(x, y)) != Some(&player_id) {
            return Err("Property is not owned by player");
        }

        let terrain = self.map.get_terrain(x, y).ok_or("Out of bounds")?;
        if terrain != Terrain::City && terrain != Terrain::Airport {
            return Err("Can only produce on City or Airport");
        }

        let mut capital_coord = None;
        for (&(cx, cy), &owner_id) in &self.properties {
            if owner_id == player_id && self.map.get_terrain(cx, cy) == Some(Terrain::Capital) {
                capital_coord = Some((cx, cy));
                break;
            }
        }

        let (cx, cy) = capital_coord.ok_or("Player has no capital")?;

        let distance = self
            .map
            .distance(x, y, cx, cy)
            .ok_or("Invalid distance calculation")?;
        if distance > 3 {
            return Err("Distance from capital is greater than 3");
        }

        let player = self.players.iter_mut().find(|p| p.id == player_id).unwrap();
        if player.funds < unit_stats.cost {
            return Err("Insufficient funds");
        }

        player.spend_funds(unit_stats.cost)?;

        let unit = Unit::new(unit_stats, player_id, (x, y));
        self.units.push(unit);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::map_grid::GridTopology;
    use crate::domain::unit_roster::{MovementType, UnitType};

    fn dummy_stats() -> UnitStats {
        UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1000,
            max_movement: 3,
            movement_type: MovementType::Foot,
            max_fuel: 99,
            max_ammo1: 0,
            max_ammo2: 0,
            min_range: 1,
            max_range: 1,
        }
    }

    #[test]
    fn test_budget_increase() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::City).unwrap();
        map.set_terrain(1, 0, Terrain::Airport).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap(); // P1 capital

        let mut properties = HashMap::new();
        properties.insert((0, 0), 1);
        properties.insert((1, 0), 1);
        properties.insert((4, 4), 1);

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());

        let mut game = MatchState::new(map, vec![p1, p2], properties);

        assert_eq!(game.players[0].funds, 0);
        game.add_budget_for_active_player();
        assert_eq!(game.players[0].funds, 2000);
    }

    #[test]
    fn test_win_condition_capital() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap(); // P1 capital
        map.set_terrain(4, 4, Terrain::Capital).unwrap(); // P2 capital

        let mut properties = HashMap::new();
        properties.insert((0, 0), 1);
        properties.insert((4, 4), 2);

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());

        let mut game = MatchState::new(map, vec![p1, p2], properties);

        // P1 captures P2's capital
        game.properties.insert((4, 4), 1);
        game.check_win_conditions();

        assert_eq!(game.game_over, Some(GameOverCondition::Winner(1)));
    }

    #[test]
    fn test_win_condition_annihilation() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), 1);
        properties.insert((4, 4), 2);

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        // Turn 1: nobody has units. But annihilation is only checked > turn 1.
        game.check_win_conditions();
        assert_eq!(game.game_over, None);

        // Turn 2: P1 has a unit, P2 has none
        game.current_turn_number = 2;
        game.units.push(Unit::new(dummy_stats(), 1, (1, 1)));
        game.check_win_conditions();

        assert_eq!(game.game_over, Some(GameOverCondition::Winner(1)));
    }

    #[test]
    fn test_production() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(2, 0, Terrain::City).unwrap(); // distance 2
        map.set_terrain(4, 0, Terrain::City).unwrap(); // distance 4

        let mut properties = HashMap::new();
        properties.insert((0, 0), 1);
        properties.insert((2, 0), 1);
        properties.insert((4, 0), 1);

        let mut p1 = Player::new(1, "Red".to_string());
        p1.funds = 1500;
        let p2 = Player::new(2, "Blue".to_string());

        let mut game = MatchState::new(map, vec![p1, p2], properties);

        // Fail: insufficient funds
        let stats = dummy_stats();
        let expensive_stats = UnitStats {
            cost: 2000,
            ..stats.clone()
        };
        let res = game.produce_unit(1, 2, 0, expensive_stats);
        assert!(res.is_err());

        // Fail: distance > 3
        let res = game.produce_unit(1, 4, 0, stats.clone());
        assert!(res.is_err());

        // Success
        let res = game.produce_unit(1, 2, 0, stats.clone());
        assert!(res.is_ok());
        assert_eq!(game.players[0].funds, 500);
        assert_eq!(game.units.len(), 1);
        assert_eq!(game.units[0].position, (2, 0));
    }
}
