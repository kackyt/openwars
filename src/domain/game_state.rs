use crate::domain::map_grid::{Map, Terrain};
use crate::domain::unit_roster::{DamageChart, Unit, UnitStats};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyState {
    pub owner_id: Option<u32>,
    pub capture_points: u32,
}

impl PropertyState {
    pub fn new(terrain: Terrain, owner_id: Option<u32>) -> Self {
        Self {
            owner_id,
            capture_points: terrain.max_capture_points(),
        }
    }
}

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
    pub properties: HashMap<(usize, usize), PropertyState>,
    pub players: Vec<Player>,
    pub current_turn_number: u32,
    pub active_player_index: usize,
    pub current_phase: Phase,
    pub game_over: Option<GameOverCondition>,
}

impl MatchState {
    pub fn new(
        map: Map,
        players: Vec<Player>,
        properties: HashMap<(usize, usize), PropertyState>,
    ) -> Self {
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

        for (&(x, y), state) in &self.properties {
            if state.owner_id == Some(active_player_id) {
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

        self.current_phase = Phase::Production; // 新プレイヤーのフェーズをリセット

        // 移動・攻撃フラグをリセット
        let active_pid = self.players[self.active_player_index].id;
        for unit in &mut self.units {
            if unit.owner_player_id == active_pid {
                unit.has_moved = false;
                unit.action_completed = false;
            }
        }

        // 拠点による自動補給（ターン開始時に資金を消費して補給）
        self.apply_property_resupply(active_pid);

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
            for (&(x, y), state) in &self.properties {
                if state.owner_id == Some(player.id) {
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

        if self.properties.get(&(x, y)).and_then(|s| s.owner_id) != Some(player_id) {
            return Err("Property is not owned by player");
        }

        let terrain = self.map.get_terrain(x, y).ok_or("Out of bounds")?;
        if terrain != Terrain::City && terrain != Terrain::Airport {
            return Err("Can only produce on City or Airport");
        }

        let mut capital_coord = None;
        for (&(cx, cy), state) in &self.properties {
            if state.owner_id == Some(player_id)
                && self.map.get_terrain(cx, cy) == Some(Terrain::Capital)
            {
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

    pub fn capture_or_repair_property(&mut self, unit_index: usize) -> Result<(), &'static str> {
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
        if !unit.stats.can_capture {
            return Err("Unit cannot capture properties");
        }

        let (x, y) = unit.position;
        let terrain = self.map.get_terrain(x, y).ok_or("Out of bounds")?;
        let max_points = terrain.max_capture_points();

        if max_points == 0 {
            return Err("Not a capturable property");
        }

        let action_power = unit.get_display_hp() * 10;

        let prop = self
            .properties
            .entry((x, y))
            .or_insert_with(|| PropertyState::new(terrain, None));

        if prop.owner_id == Some(active_player_id) {
            // Repair
            prop.capture_points = std::cmp::min(prop.capture_points + action_power, max_points);
        } else {
            // Capture
            if prop.capture_points <= action_power {
                prop.owner_id = Some(active_player_id);
                prop.capture_points = max_points;
            } else {
                prop.capture_points -= action_power;
            }
        }

        self.units[unit_index].action_completed = true;
        self.check_win_conditions();

        Ok(())
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
            unit_mut.has_moved = true; // 移動完了マーク（攻撃はまだ可能）
            Ok(())
        } else {
            Err("Unreachable target")
        }
    }

    /// 指定ユニットで相手ユニットに攻撃する。
    /// 直接攻撃は同時ダメージ計算、間接攻撃は一方的。
    pub fn attack(
        &mut self,
        attacker_index: usize,
        defender_index: usize,
        damage_chart: &DamageChart,
    ) -> Result<(), &'static str> {
        if self.game_over.is_some() {
            return Err("Game is over");
        }
        let active_player_id = self.get_active_player().ok_or("No active player")?.id;
        let attacker = self.units.get(attacker_index).ok_or("Attacker not found")?;
        let defender = self.units.get(defender_index).ok_or("Defender not found")?;

        // 基本チェック
        if attacker.owner_player_id != active_player_id {
            return Err("Not your unit");
        }
        if attacker.action_completed {
            return Err("Attacker already acted");
        }
        if attacker.is_destroyed() {
            return Err("Attacker is destroyed");
        }
        if defender.owner_player_id == active_player_id {
            return Err("Cannot attack own unit");
        }
        if defender.is_destroyed() {
            return Err("Defender is already destroyed");
        }

        // 攻撃者の武器を選択
        let attacker_type = attacker.stats.unit_type;
        let defender_type = defender.stats.unit_type;

        let (ax, ay) = attacker.position;
        let (dx, dy) = defender.position;
        let dist = (ax as i64 - dx as i64).unsigned_abs() as u32
            + (ay as i64 - dy as i64).unsigned_abs() as u32;

        // 主武器・副武器を選択（ダメージ > 0 かつ弾薬ありを優先）
        let attacker_weapon = {
            let a = self.units.get(attacker_index).unwrap();
            Self::select_weapon(a, defender_type, damage_chart)
        };
        let (a_weapon_slot, a_base_damage) =
            attacker_weapon.ok_or("Attacker has no usable weapon against this target")?;

        // 射程チェック
        let attacker_stats = &self.units[attacker_index].stats;
        let (min_r, max_r, is_indirect) = if a_weapon_slot == 1 {
            (
                attacker_stats.min_range,
                attacker_stats.max_range,
                attacker_stats.min_range > 1,
            )
        } else {
            // ammo2 = 副武器は直接攻撃専用（min_range2・ max_range2は定義していないため常に直接扱い）
            (1u32, 1u32, false)
        };
        if dist < min_r || dist > max_r {
            return Err("Target out of weapon range");
        }

        // 間接攻撃・移動後は攻撃不可
        if is_indirect && self.units[attacker_index].has_moved {
            return Err("Indirect attack unit cannot attack after moving");
        }

        // 攻撃ダメージ計算（+5%アドバンテージ）
        let attacker_display_hp = self.units[attacker_index].get_display_hp();
        let a_advantage_damage = (a_base_damage as f64 * 1.05) as u32;
        let a_damage = a_advantage_damage * attacker_display_hp / 10 + Self::random_bonus();

        // 反撃判定（直接攻撃の場合のみ、攻撃前のHPで判断）
        let do_counter = !is_indirect; // 間接攻撃は一方的
        let counter_info = if do_counter {
            Self::select_weapon(&self.units[defender_index], attacker_type, damage_chart)
        } else {
            None
        };

        let d_damage_opt: Option<u32> = if let Some((_, d_base)) = counter_info {
            let defender_display_hp = self.units[defender_index].get_display_hp();
            Some(d_base * defender_display_hp / 10 + Self::random_bonus())
        } else {
            None
        };

        // 弾薬消費（攻撃者）
        match a_weapon_slot {
            1 => self.units[attacker_index].ammo1 -= 1,
            _ => self.units[attacker_index].ammo2 -= 1,
        }

        // 同時ダメージ適用
        self.units[defender_index].take_damage(a_damage);
        if let (Some((d_slot, _)), Some(d_dmg)) = (counter_info, d_damage_opt) {
            // 弾薬消費（防衛者）
            match d_slot {
                1 => self.units[defender_index].ammo1 -= 1,
                _ => self.units[defender_index].ammo2 -= 1,
            }
            self.units[attacker_index].take_damage(d_dmg);
        }

        // 攻撃完了マーク
        self.units[attacker_index].action_completed = true;

        self.check_win_conditions();
        Ok(())
    }

    /// 武器を自動選択する。主武器(ammo1)優先、ダメージ > 0 かつ弾薬ありの場合に使用。
    /// 戻り値: (weapon_slot, base_damage) または None
    fn select_weapon(
        unit: &Unit,
        target_type: crate::domain::unit_roster::UnitType,
        damage_chart: &DamageChart,
    ) -> Option<(u32, u32)> {
        // ammo1（主武器）
        if unit.ammo1 > 0 {
            if let Some(dmg) = damage_chart.get_base_damage(unit.stats.unit_type, target_type) {
                if dmg > 0 {
                    return Some((1, dmg));
                }
            }
        }
        // ammo2（副武器）へフォールバック
        if unit.ammo2 > 0 {
            if let Some(dmg) =
                damage_chart.get_base_damage_secondary(unit.stats.unit_type, target_type)
            {
                if dmg > 0 {
                    return Some((2, dmg));
                }
            }
        }
        None
    }

    /// 0【10 のランダム値を返す。公平性のため、仵要時にシード制御も可能な構造を想定。
    fn random_bonus() -> u32 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        use std::time::SystemTime;
        let mut h = DefaultHasher::new();
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            .hash(&mut h);
        (h.finish() % 11) as u32 // 0..=10
    }

    /// 補給輸送車が隣接する指定の味方ユニット 1 体に補給する。
    /// 燃料・ammo1・ammo2 を最大値まで回復。移動後も実行可能。
    pub fn supply_unit(
        &mut self,
        supplier_index: usize,
        target_index: usize,
    ) -> Result<(), &'static str> {
        if self.game_over.is_some() {
            return Err("Game is over");
        }
        let active_player_id = self.get_active_player().ok_or("No active player")?.id;

        {
            let supplier = self.units.get(supplier_index).ok_or("Supplier not found")?;
            if supplier.owner_player_id != active_player_id {
                return Err("Not your unit");
            }
            if supplier.action_completed {
                return Err("Supplier already acted");
            }
            if !supplier.stats.can_supply {
                return Err("Unit cannot supply");
            }
            if supplier.is_destroyed() {
                return Err("Supplier is destroyed");
            }

            let target = self.units.get(target_index).ok_or("Target not found")?;
            if target.owner_player_id != active_player_id {
                return Err("Cannot supply enemy unit");
            }
            if target.is_destroyed() {
                return Err("Target is destroyed");
            }

            // 距離チェック（マンハッタン距離 = 1 のみ）
            let (sx, sy) = supplier.position;
            let (tx, ty) = target.position;
            let dist = (sx as i64 - tx as i64).unsigned_abs() as u32
                + (sy as i64 - ty as i64).unsigned_abs() as u32;
            if dist != 1 {
                return Err("Target is not adjacent to supplier");
            }
        }

        // 補給実行
        {
            let target = &mut self.units[target_index];
            let max_fuel = target.stats.max_fuel;
            let max_ammo1 = target.stats.max_ammo1;
            let max_ammo2 = target.stats.max_ammo2;
            target.fuel = max_fuel;
            target.ammo1 = max_ammo1;
            target.ammo2 = max_ammo2;
        }
        self.units[supplier_index].action_completed = true;
        Ok(())
    }

    /// 補給輸送車が隣接するすべての味方ユニットに一括補給する（全自動補給）。
    pub fn supply_all_adjacent(&mut self, supplier_index: usize) -> Result<(), &'static str> {
        if self.game_over.is_some() {
            return Err("Game is over");
        }
        let active_player_id = self.get_active_player().ok_or("No active player")?.id;

        {
            let supplier = self.units.get(supplier_index).ok_or("Supplier not found")?;
            if supplier.owner_player_id != active_player_id {
                return Err("Not your unit");
            }
            if supplier.action_completed {
                return Err("Supplier already acted");
            }
            if !supplier.stats.can_supply {
                return Err("Unit cannot supply");
            }
        }

        let (sx, sy) = self.units[supplier_index].position;

        // 補給対象インデックスを収集
        let targets: Vec<usize> = self
            .units
            .iter()
            .enumerate()
            .filter(|(i, u)| {
                if *i == supplier_index || u.is_destroyed() {
                    return false;
                }
                if u.owner_player_id != active_player_id {
                    return false;
                }
                let (tx, ty) = u.position;
                let dist = (sx as i64 - tx as i64).unsigned_abs() as u32
                    + (sy as i64 - ty as i64).unsigned_abs() as u32;
                dist == 1
            })
            .map(|(i, _)| i)
            .collect();

        for target_idx in targets {
            let target = &mut self.units[target_idx];
            target.fuel = target.stats.max_fuel;
            target.ammo1 = target.stats.max_ammo1;
            target.ammo2 = target.stats.max_ammo2;
        }

        self.units[supplier_index].action_completed = true;
        Ok(())
    }

    /// 指定ユニットを補給する内部ヘルパ（拠点補給で使用）。
    /// 戻り値: (ammo_restored, fuel_restored) の量
    fn restore_supplies(unit: &mut Unit) -> (u32, u32) {
        let ammo_diff = (unit.stats.max_ammo1.saturating_sub(unit.ammo1))
            + (unit.stats.max_ammo2.saturating_sub(unit.ammo2));
        let fuel_diff = unit.stats.max_fuel.saturating_sub(unit.fuel);
        unit.fuel = unit.stats.max_fuel;
        unit.ammo1 = unit.stats.max_ammo1;
        unit.ammo2 = unit.stats.max_ammo2;
        (ammo_diff, fuel_diff)
    }

    /// ターン開始時にアクティブプレイヤーが所有する拠点にいる味方ユニットを自動補給する。
    /// 弾薬 1 につき 15G、燃料 1 につき 5G を消費。資金不足なら補給スキップ。
    fn apply_property_resupply(&mut self, player_id: u32) {
        // 補給対象ユニットのインデックスと位置を収集
        let unit_positions: Vec<(usize, (usize, usize))> = self
            .units
            .iter()
            .enumerate()
            .filter(|(_, u)| u.owner_player_id == player_id && !u.is_destroyed())
            .map(|(i, u)| (i, u.position))
            .collect();

        // 自国拠点マスのセットを収集
        let owned_property_positions: std::collections::HashSet<(usize, usize)> = self
            .properties
            .iter()
            .filter(|(_, ps)| ps.owner_id == Some(player_id))
            .filter(|(pos, _)| {
                // 補給対象となる拠点種別（首都・都市・工場・空港・港）
                matches!(
                    self.map.get_terrain(pos.0, pos.1),
                    Some(Terrain::Capital)
                        | Some(Terrain::City)
                        | Some(Terrain::Factory)
                        | Some(Terrain::Airport)
                        | Some(Terrain::Port)
                )
            })
            .map(|(pos, _)| *pos)
            .collect();

        // 各ユニットのコストを計算し、資金が足りれば補給
        for (unit_idx, pos) in unit_positions {
            if !owned_property_positions.contains(&pos) {
                continue;
            }

            let unit = &self.units[unit_idx];
            let ammo_diff = (unit.stats.max_ammo1.saturating_sub(unit.ammo1))
                + (unit.stats.max_ammo2.saturating_sub(unit.ammo2));
            let fuel_diff = unit.stats.max_fuel.saturating_sub(unit.fuel);
            let cost = ammo_diff * 15 + fuel_diff * 5;

            // 資金チェック
            let player_idx = self
                .players
                .iter()
                .position(|p| p.id == player_id)
                .expect("Player not found");

            if self.players[player_idx].funds >= cost {
                self.players[player_idx].funds -= cost;
                let unit = &mut self.units[unit_idx];
                unit.fuel = unit.stats.max_fuel;
                unit.ammo1 = unit.stats.max_ammo1;
                unit.ammo2 = unit.stats.max_ammo2;
            }
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
            can_capture: true,
            can_supply: false,
        }
    }

    #[test]
    fn test_budget_increase() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::City).unwrap();
        map.set_terrain(1, 0, Terrain::Airport).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap(); // P1 capital

        let mut properties = HashMap::new();
        properties.insert((0, 0), PropertyState::new(Terrain::City, Some(1)));
        properties.insert((1, 0), PropertyState::new(Terrain::Airport, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(1)));

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
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());

        let mut game = MatchState::new(map, vec![p1, p2], properties);

        // P1 captures P2's capital
        game.properties
            .insert((4, 4), PropertyState::new(Terrain::Capital, Some(1)));
        game.check_win_conditions();

        assert_eq!(game.game_over, Some(GameOverCondition::Winner(1)));
    }

    #[test]
    fn test_win_condition_annihilation() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));

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
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((2, 0), PropertyState::new(Terrain::City, Some(1)));
        properties.insert((4, 0), PropertyState::new(Terrain::City, Some(1)));

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
        properties.insert((0, 0), PropertyState::new(Terrain::Airport, Some(1)));
        properties.insert((0, 1), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));

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
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));
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
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));

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
        // 移動後は has_moved が true で、action_completed は false のまま（攻撃はまだできる）
        assert!(
            game.units[0].has_moved,
            "has_moved should be true after moving"
        );
        assert!(
            !game.units[0].action_completed,
            "action_completed should stay false after moving"
        );
    }

    #[test]
    fn test_property_capture_and_repair() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap(); // P1 Capital
        map.set_terrain(4, 4, Terrain::Capital).unwrap(); // P2 Capital
        map.set_terrain(2, 2, Terrain::City).unwrap(); // Neutral City

        let mut properties = HashMap::new();
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        let mut inf_stats = dummy_stats();
        inf_stats.can_capture = true;

        let mut tank_stats = dummy_stats();
        tank_stats.unit_type = UnitType::TankZ;
        tank_stats.can_capture = false;

        // u0: P1 Infantry at neutral city (2,2)
        game.units.push(Unit::new(inf_stats.clone(), 1, (2, 2)));
        game.units[0].action_completed = false; // allow immediate action
        game.units[0].hp = 50; // Display HP 5 (does 50 damage)

        // u1: P1 Tank at P2 Capital (4,4)
        game.units.push(Unit::new(tank_stats.clone(), 1, (4, 4)));
        game.units[1].action_completed = false;

        // u2: P2 Infantry to prevent annihilation game over
        game.units.push(Unit::new(inf_stats.clone(), 2, (3, 3)));

        // Test non-capturable unit
        assert!(game.capture_or_repair_property(1).is_err()); // Tank cannot capture

        // Test capturing neutral city
        assert!(game.capture_or_repair_property(0).is_ok());
        assert!(game.units[0].action_completed);

        // Check city points
        let city = game.properties.get(&(2, 2)).unwrap();
        assert_eq!(city.owner_id, None);
        assert_eq!(city.capture_points, 150); // 200 - 50

        // Advance turn so P1 can act again
        game.advance_turn(); // P2
        game.advance_turn(); // P1

        // Test persistent capture damage:
        // move infantry away and back should keep points at 150
        // for ease of test, simply acting again reduces it more:
        game.units[0].action_completed = false;
        game.units[0].hp = 100; // Display HP 10 (does 100 damage)
        game.capture_or_repair_property(0).unwrap();
        let city = game.properties.get(&(2, 2)).unwrap();
        assert_eq!(city.capture_points, 50); // 150 - 100

        // Act again to finish capture
        game.advance_turn();
        game.advance_turn();
        game.units[0].action_completed = false;
        game.capture_or_repair_property(0).unwrap();

        // Ownership transferred, points reset
        let city = game.properties.get(&(2, 2)).unwrap();
        assert_eq!(city.owner_id, Some(1));
        assert_eq!(city.capture_points, 200);

        // Test Repair: City is damaged, we repair it
        game.properties.get_mut(&(2, 2)).unwrap().capture_points = 180;
        game.advance_turn();
        game.advance_turn();
        game.units[0].action_completed = false;
        game.units[0].hp = 100; // 10 hp -> 100 repair
        game.capture_or_repair_property(0).unwrap();

        let city = game.properties.get(&(2, 2)).unwrap();
        assert_eq!(city.capture_points, 200); // capped at max 200

        // Test Capital Capture Victory
        game.units[0].position = (4, 4); // Move infantry to P2 Capital
        game.properties.get_mut(&(4, 4)).unwrap().capture_points = 50; // Almost captured
        game.advance_turn();
        game.advance_turn();
        game.units[0].action_completed = false;

        game.capture_or_repair_property(0).unwrap();
        let enemy_capital = game.properties.get(&(4, 4)).unwrap();
        assert_eq!(enemy_capital.owner_id, Some(1));
        assert_eq!(enemy_capital.capture_points, 400); // capital max

        // Since P2 lost its only capital, game should be over, P1 wins!
        assert_eq!(game.game_over, Some(GameOverCondition::Winner(1)));
    }

    // コンビニエンス関数: テスト用の DamageChart 作成
    fn make_chart_with_damage(attacker: UnitType, defender: UnitType, damage: u32) -> DamageChart {
        let mut chart = DamageChart::new();
        chart.insert_damage(attacker, defender, damage);
        chart
    }

    #[test]
    fn test_attack_direct_basic() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        let mut stats_inf = dummy_stats();
        stats_inf.max_ammo1 = 9;

        // P1 歩兵 @ (2,2), HP 100
        let mut u0 = Unit::new(stats_inf.clone(), 1, (2, 2));
        u0.action_completed = false;
        game.units.push(u0);

        // P2 歩兵 @ (3,2), HP 50 (表示HP5)
        let mut u1 = Unit::new(stats_inf.clone(), 2, (3, 2));
        u1.hp = 50;
        game.units.push(u1);

        // P1 歩兵 (display_hp=10) が P2 歩兵に攻撃。ダメージ = floor(55*1.05*10/10) + random
        let chart = make_chart_with_damage(UnitType::Infantry, UnitType::Infantry, 55);

        let result = game.attack(0, 1, &chart);
        assert!(result.is_ok(), "{result:?}");

        // 攻撃者は action_completed になる
        assert!(game.units[0].action_completed);
        // P2 は ammo1 が 1 減る（反撃時）
        // ダメージが入っていることを確認（hp < 100）
        assert!(game.units[1].hp < 100, "Defender should have taken damage");
    }

    #[test]
    fn test_attack_direct_simultaneous_both_destroyed() {
        // 互いに oneshot できる高ダメージで両者消滅を確認
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        let mut stats = dummy_stats();
        stats.max_ammo1 = 9;

        let mut u0 = Unit::new(stats.clone(), 1, (2, 2));
        u0.action_completed = false;
        u0.hp = 100;
        game.units.push(u0);

        let mut u1 = Unit::new(stats.clone(), 2, (3, 2));
        u1.hp = 100;
        game.units.push(u1);

        // ダメージ100 → floor(100*1.05*10/10)+rand >= 100 → 攻撃側は必ず撃破
        // 反撃も同様に計算されるので両方消滅もありうる
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Infantry, UnitType::Infantry, 100);

        let _ = game.attack(0, 1, &chart);

        // 防衛者は撃破されているはず
        assert!(game.units[1].is_destroyed(), "Defender should be destroyed");
        // 反撃ダメージ（100*10/10 + 0-10 = 100-110）で攻撃者も撃破
        assert!(
            game.units[0].is_destroyed(),
            "Attacker should also be destroyed by counter"
        );
    }

    #[test]
    fn test_attack_no_counter_when_defender_has_no_weapon() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        let mut atk_stats = dummy_stats();
        atk_stats.max_ammo1 = 9;

        // 防衛者は弾なし（ammo0）なので反撃不可
        let def_stats = dummy_stats(); // max_ammo1 = 0

        let mut u0 = Unit::new(atk_stats.clone(), 1, (2, 2));
        u0.action_completed = false;
        game.units.push(u0);

        let u1 = Unit::new(def_stats.clone(), 2, (3, 2));
        game.units.push(u1);

        let chart = make_chart_with_damage(UnitType::Infantry, UnitType::Infantry, 30);

        let attacker_hp_before = game.units[0].hp;
        let result = game.attack(0, 1, &chart);
        assert!(result.is_ok());

        // 攻撃者 HP は変わっていない（反撃なし）
        assert_eq!(
            game.units[0].hp, attacker_hp_before,
            "No counter damage expected"
        );
    }

    #[test]
    fn test_attack_indirect_no_counter_and_no_attack_after_move() {
        let mut map = Map::new(10, 10, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(9, 9, Terrain::Capital).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((9, 9), PropertyState::new(Terrain::Capital, Some(2)));

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        // 間接攻撃ユニット: min_range=2, max_range=4
        let mut art_stats = dummy_stats();
        art_stats.unit_type = UnitType::Artillery;
        art_stats.min_range = 2;
        art_stats.max_range = 4;
        art_stats.max_ammo1 = 9;

        let mut def_stats = dummy_stats();
        def_stats.max_ammo1 = 9;

        // u0: P1 砲台 @ (3,3)
        let mut u0 = Unit::new(art_stats.clone(), 1, (3, 3));
        u0.action_completed = false;
        u0.has_moved = false;
        game.units.push(u0);

        // u1: P2 歩兵 @ (3, 6) → 距離=3 (射程内)
        let u1 = Unit::new(def_stats.clone(), 2, (3, 6));
        game.units.push(u1);

        let chart = make_chart_with_damage(UnitType::Artillery, UnitType::Infantry, 70);
        let def_hp_before = game.units[0].hp;

        // 間接攻撃 → 反撃なし: 攻撃者 HP 変わらず
        let result = game.attack(0, 1, &chart);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            game.units[0].hp, def_hp_before,
            "No counter expected for indirect"
        );

        // 移動後に間接攻撃しようとするには、別ユニットでテスト
        // u0 が行動完了したので advance_turn でリセット
        game.advance_turn(); // P2
        game.advance_turn(); // P1

        game.units[0].has_moved = true; // 移動済みを強制セット
        game.units[0].action_completed = false;

        // u1 を復活させる（advance_turn で撃破判定があるため再設定）
        game.units[1].hp = 100;

        let err_result = game.attack(0, 1, &chart);
        assert!(
            err_result.is_err(),
            "Should fail: indirect attack after move"
        );
        assert!(
            err_result
                .unwrap_err()
                .contains("cannot attack after moving")
        );
    }

    #[test]
    fn test_attack_out_of_range() {
        let mut map = Map::new(10, 10, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(9, 9, Terrain::Capital).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((9, 9), PropertyState::new(Terrain::Capital, Some(2)));

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        let mut inf_stats = dummy_stats();
        inf_stats.max_ammo1 = 9;

        let mut u0 = Unit::new(inf_stats.clone(), 1, (0, 0));
        u0.action_completed = false;
        game.units.push(u0);

        let u1 = Unit::new(inf_stats.clone(), 2, (5, 5)); // 距離10 > max_range1
        game.units.push(u1);

        let chart = make_chart_with_damage(UnitType::Infantry, UnitType::Infantry, 55);
        let result = game.attack(0, 1, &chart);
        assert!(result.is_err(), "Should fail: out of range");
    }

    // テスト用の補給輸送車ステータスを作成
    fn supply_truck_stats() -> UnitStats {
        let mut s = dummy_stats();
        s.unit_type = UnitType::SupplyTruck;
        s.max_fuel = 50;
        s.max_ammo1 = 0;
        s.max_ammo2 = 0;
        s.can_supply = true;
        s
    }

    #[test]
    fn test_supply_unit_basic() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        // u0: 補給輸送車 @(2,2)
        let mut truck = Unit::new(supply_truck_stats(), 1, (2, 2));
        truck.action_completed = false;
        game.units.push(truck);

        // u1: 歩兵 @(3,2) 燃料・弾薬消費済み
        let mut inf_stats = dummy_stats();
        inf_stats.max_fuel = 99;
        inf_stats.max_ammo1 = 9;
        let mut inf = Unit::new(inf_stats.clone(), 1, (3, 2));
        inf.fuel = 10;
        inf.ammo1 = 2;
        game.units.push(inf);

        let result = game.supply_unit(0, 1);
        assert!(result.is_ok(), "{result:?}");
        // 補給後は最大値に回復
        assert_eq!(game.units[1].fuel, 99);
        assert_eq!(game.units[1].ammo1, 9);
        // 補給者は行動完了
        assert!(game.units[0].action_completed);
    }

    #[test]
    fn test_supply_unit_out_of_range_error() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        let mut truck = Unit::new(supply_truck_stats(), 1, (2, 2));
        truck.action_completed = false;
        game.units.push(truck);

        let inf = Unit::new(dummy_stats(), 1, (4, 2)); // 距離2
        game.units.push(inf);

        let result = game.supply_unit(0, 1);
        assert!(result.is_err(), "Should fail: not adjacent");
    }

    #[test]
    fn test_supply_all_adjacent() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));

        let p1 = Player::new(1, "Red".to_string());
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        let mut truck = Unit::new(supply_truck_stats(), 1, (2, 2));
        truck.action_completed = false;
        game.units.push(truck);

        let mut inf_stats = dummy_stats();
        inf_stats.max_fuel = 99;
        inf_stats.max_ammo1 = 9;

        let mut u1 = Unit::new(inf_stats.clone(), 1, (1, 2)); // 左隣
        u1.fuel = 5;
        game.units.push(u1);
        let mut u2 = Unit::new(inf_stats.clone(), 1, (3, 2)); // 右隣
        u2.fuel = 20;
        game.units.push(u2);

        let result = game.supply_all_adjacent(0);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(game.units[1].fuel, 99, "Left unit fully resupplied");
        assert_eq!(game.units[2].fuel, 99, "Right unit fully resupplied");
        assert!(game.units[0].action_completed);
    }

    #[test]
    fn test_property_resupply_deducts_funds() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap();
        map.set_terrain(2, 2, Terrain::City).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));
        properties.insert((2, 2), PropertyState::new(Terrain::City, Some(1)));

        let mut p1 = Player::new(1, "Red".to_string());
        p1.funds = 10000;
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        let mut inf_stats = dummy_stats();
        inf_stats.max_fuel = 99;
        inf_stats.max_ammo1 = 9;
        // 弾薬差=9-0=9, 燃料差=99-0=99 ← 全消費状態
        let mut inf = Unit::new(inf_stats.clone(), 1, (2, 2));
        inf.fuel = 0;
        inf.ammo1 = 0;
        game.units.push(inf);

        let funds_before = game.players[0].funds;
        // advance_turn で P2 → P1 の順でリセット
        // P1 のターン開始時に拠点補給が走る
        game.advance_turn(); // P2
        game.advance_turn(); // P1 (ここで拠点補給)

        let expected_cost = 9 * 15 + 99 * 5; // 135 + 495 = 630G
        // add_budget_for_active_player で都市 1 箇所 → +1000G も加算されているので差分確認
        let funds_after_add_budget = 10000 + 1000; // 都市1個
        assert_eq!(
            game.players[0].funds,
            funds_after_add_budget - expected_cost,
            "Funds should decrease by supply cost"
        );
        assert_eq!(game.units[0].fuel, 99);
        assert_eq!(game.units[0].ammo1, 9);
    }

    #[test]
    fn test_property_resupply_insufficient_funds() {
        let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(4, 4, Terrain::Capital).unwrap();
        map.set_terrain(2, 2, Terrain::City).unwrap();

        let mut properties = HashMap::new();
        properties.insert((0, 0), PropertyState::new(Terrain::Capital, Some(1)));
        properties.insert((4, 4), PropertyState::new(Terrain::Capital, Some(2)));
        properties.insert((2, 2), PropertyState::new(Terrain::City, Some(1)));

        let mut p1 = Player::new(1, "Red".to_string());
        p1.funds = 10; // 資金不足
        let p2 = Player::new(2, "Blue".to_string());
        let mut game = MatchState::new(map, vec![p1, p2], properties);

        let mut inf_stats = dummy_stats();
        inf_stats.max_fuel = 99;
        inf_stats.max_ammo1 = 9;
        let mut inf = Unit::new(inf_stats.clone(), 1, (2, 2));
        inf.fuel = 0;
        inf.ammo1 = 0;
        game.units.push(inf);

        game.advance_turn(); // P2
        game.advance_turn(); // P1

        // 補給コスト > 10G なので補給されない（燃料・弾薬は 0 のまま）
        assert_eq!(
            game.units[0].fuel, 0,
            "Should not be resupplied (insufficient funds)"
        );
        assert_eq!(
            game.units[0].ammo1, 0,
            "Should not be resupplied (insufficient funds)"
        );
    }
}
