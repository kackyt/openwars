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

pub struct MatchState {
    pub players: Vec<Player>,
    pub current_turn_number: u32,
    pub active_player_index: usize,
    pub current_phase: Phase,
}

impl MatchState {
    pub fn new(players: Vec<Player>) -> Self {
        Self {
            players,
            current_turn_number: 1,
            active_player_index: 0,           // Starts with player 0 index
            current_phase: Phase::Production, // Typical sequence
        }
    }

    pub fn get_active_player(&self) -> Option<&Player> {
        self.players.get(self.active_player_index)
    }

    pub fn advance_turn(&mut self) {
        self.active_player_index += 1;

        // Wrap around players
        if self.active_player_index >= self.players.len() {
            self.active_player_index = 0;
            self.current_turn_number += 1; // All players moved, next full turn day
        }

        self.current_phase = Phase::Production; // Reset phase for new player
    }

    pub fn next_phase(&mut self) {
        match self.current_phase {
            Phase::Production => {
                self.current_phase = Phase::MovementAndAttack;
            }
            Phase::MovementAndAttack => {
                self.current_phase = Phase::EndTurn;
                self.advance_turn(); // Automatically advance turn if ending
            }
            Phase::EndTurn => {
                // Usually handled by advance_turn, but just in case
                self.current_phase = Phase::Production;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_state_turn_advancement() {
        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());

        let mut game = MatchState::new(vec![p1.clone(), p2.clone()]);

        assert_eq!(game.current_turn_number, 1);
        assert_eq!(game.get_active_player(), Some(&p1));

        game.advance_turn();

        assert_eq!(game.current_turn_number, 1);
        assert_eq!(game.get_active_player(), Some(&p2));

        game.advance_turn();

        assert_eq!(game.current_turn_number, 2);
        assert_eq!(game.get_active_player(), Some(&p1)); // Back to P1
    }
}
