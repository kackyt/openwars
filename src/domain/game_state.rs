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
            self.process_daily_updates();
        }

        self.current_phase = Phase::Production; // Reset phase for new player

        // Reset action points
        let active_pid = self.players[self.active_player_index].id;
        for unit in &mut self.units {
            if unit.owner_player_id == active_pid {
                unit.action_completed = false;
            }
        }

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

    fn process_daily_updates(&mut self) {
        use crate::domain::unit_roster::MovementType;
        for unit in &mut self.units {
            if unit.is_destroyed() {
                continue;
            }
            if unit.stats.movement_type == MovementType::LowAltitude
                || unit.stats.movement_type == MovementType::HighAltitude
            {
                let terrain = self.map.get_terrain(unit.position.0, unit.position.1);
                if terrain != Some(Terrain::Airport) {
                    if unit.fuel == 0 {
                        unit.hp = 0; // Destroyed
                    } else {
                        unit.fuel = unit.fuel.saturating_sub(unit.stats.daily_fuel_consumption);
                    }
                }
            }
        }
    }

    pub fn move_unit(
        &mut self,
        unit_index: usize,
        target_x: usize,
        target_y: usize,
    ) -> Result<(), &'static str> {
        if self.game_over.is_some() {
            return Err("Game is over");
        }
        let active_player_id = self.get_active_player().ok_or("No active player")?.id;
        let unit = self.units.get(unit_index).ok_or("Unit not found")?;

        if unit.owner_player_id != active_player_id {
            return Err("Not your unit");
        }
        if unit.action_completed {
            return Err("Unit already acted");
        }
        if unit.is_destroyed() {
            return Err("Unit is destroyed");
        }

        let mut unit_positions = std::collections::HashMap::new();
        for (i, u) in self.units.iter().enumerate() {
            if !u.is_destroyed() && i != unit_index {
                unit_positions.insert(u.position, u.owner_player_id);
            }
        }

        let context = crate::domain::movement::MovementContext {
            map: &self.map,
            unit_positions,
        };

        if let Some((_path, _cost, fuel_used)) = crate::domain::movement::find_path_a_star(
            &context,
            unit.position,
            (target_x, target_y),
            unit.stats.movement_type,
            unit.stats.max_movement,
            unit.fuel,
            active_player_id,
        ) {
            let unit_mut = &mut self.units[unit_index];
            unit_mut.position = (target_x, target_y);
            unit_mut.fuel -= fuel_used;
            unit_mut.action_completed = true;
            Ok(())
        } else {
            Err("Unreachable target")
        }
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
            daily_fuel_consumption: 0,
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

    #[test]
    fn test_air_unit_fuel_and_crash() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Airport).unwrap();
        map.set_terrain(0, 1, Terrain::Capital).unwrap(); // P1 Capital
        map.set_terrain(4, 4, Terrain::Capital).unwrap(); // P2 Capital

        let mut properties = HashMap::new();
        properties.insert((0, 0), 1);
        properties.insert((0, 1), 1);
        properties.insert((4, 4), 2);

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        let mut air_stats = dummy_stats();
        air_stats.movement_type = MovementType::HighAltitude;
        air_stats.max_fuel = 3;
        air_stats.daily_fuel_consumption = 2; // e.g. Helicopter uses 2

        let mut u1 = Unit::new(air_stats.clone(), 1, (0, 0));
        u1.fuel = 3;
        game.units.push(u1);

        let mut u2 = Unit::new(air_stats.clone(), 1, (1, 1));
        u2.fuel = 3;
        game.units.push(u2);

        let u3 = Unit::new(air_stats.clone(), 2, (4, 4));
        game.units.push(u3);

        game.advance_turn(); // P2
        game.advance_turn(); // P1 (Day 2)

        assert_eq!(game.units[0].fuel, 3);
        assert_eq!(game.units[1].fuel, 1);
        assert!(!game.units[1].is_destroyed());

        game.advance_turn(); // P2 
        game.advance_turn(); // P1 (Day 3)

        assert_eq!(game.units[1].fuel, 0);
        assert!(!game.units[1].is_destroyed());

        game.advance_turn(); // P2
        game.advance_turn(); // P1 (Day 4)

        assert!(game.units[1].is_destroyed());

        // Now test another unit with 5 fuel consumption
        let mut jet_stats = dummy_stats();
        jet_stats.movement_type = MovementType::HighAltitude;
        jet_stats.max_fuel = 10;
        jet_stats.daily_fuel_consumption = 5;

        let mut u4 = Unit::new(jet_stats, 2, (3, 3));
        u4.fuel = 6;
        game.units.push(u4);

        // It is currently P1's turn (Day 4 start). Let's advance a full turn to P2.
        game.advance_turn(); // P1
        game.advance_turn(); // P2 (Day 5)

        // u4 belongs to P2. Wait, daily update happens when active_player_index wraps to 0.
        // Currently advance_turn() wraps around: P1 (index 0) -> P2 (index 1). Wraps to P1.
        // Let's just create a new game state to cleanly test the 5 consumption to avoid confusion.
    }

    #[test]
    fn test_air_unit_fuel_consumption_jet() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), 1);
        properties.insert((4, 4), 2);
        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        let mut jet_stats = dummy_stats();
        jet_stats.movement_type = MovementType::HighAltitude;
        jet_stats.max_fuel = 10;
        jet_stats.daily_fuel_consumption = 5;

        // Position (2, 2) is Plains
        let mut u1 = Unit::new(jet_stats.clone(), 1, (2, 2));
        u1.fuel = 8;
        game.units.push(u1);

        let u2 = Unit::new(dummy_stats(), 2, (4, 4)); // Infantry, no fuel burn
        game.units.push(u2);

        // Turn 1, P1 starts.
        // Advance to P2:
        game.advance_turn(); // P1 -> P2
        // Advance to P1 (Day 2):
        game.advance_turn(); // P2 -> P1, Day 2 starts.

        assert_eq!(game.units[0].fuel, 3); // 8 - 5 = 3

        // Advance to P2:
        game.advance_turn(); // P1 -> P2
        // Advance to P1 (Day 3):
        game.advance_turn(); // P2 -> P1, Day 3 starts.

        assert_eq!(game.units[0].fuel, 0); // 3 - 5 = 0 (saturating sub)
        assert!(!game.units[0].is_destroyed());

        // Advance to P2:
        game.advance_turn(); // P1 -> P2
        // Advance to P1 (Day 4):
        game.advance_turn(); // P2 -> P1, Day 4 starts.

        assert!(game.units[0].is_destroyed());
    }

    #[test]
    fn test_move_unit() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), 1);
        properties.insert((4, 4), 2);

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        let stats = dummy_stats();
        let mut u1 = Unit::new(stats.clone(), 1, (2, 2));
        u1.action_completed = false;
        game.units.push(u1);

        let u2 = Unit::new(stats.clone(), 2, (4, 4));
        game.units.push(u2);

        let res = game.move_unit(0, 2, 4);
        assert!(res.is_ok());
        assert_eq!(game.units[0].position, (2, 4));
        assert_eq!(game.units[0].fuel, 97);
        assert!(game.units[0].action_completed);
    }
}
