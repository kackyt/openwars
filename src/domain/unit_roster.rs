use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitType {
    Infantry,            // 歩兵
    CombatEngineer,      // 戦闘工兵
    TankZ,               // 戦車Z
    TankA,               // 戦車A
    TankB,               // 戦車B
    Artillery,           // 砲台
    SelfPropelledGunA,   // 自走砲A
    SelfPropelledGunB,   // 自走砲B
    AntiAirMissile,      // 対空ミサイル
    AntiAirTank,         // 対空戦車
    RocketLauncher,      // ロケットランチャー
    ArmoredCar,          // 装甲車
    SupplyTruck,         // 補給輸送車
    FighterA,            // 戦闘機A
    FighterB,            // 戦闘機B
    Bomber,              // 爆撃機
    RadarPlane,          // レーダー輸送機
    CombatHelicopter,    // 戦闘ヘリ
    TransportHelicopter, // 輸送ヘリ
    SuperMissile,        // スーパーミサイル
    Battleship,          // 戦艦
    AircraftCarrier,     // 空母
    TransportShip,       // 輸送船
    Submarine,           // 潜水艦
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovementType {
    Foot,         // 歩行
    Vehicle,      // 車両
    Tracked,      // キャタピラ
    Tires,        // タイヤ
    LowAltitude,  // 低空
    HighAltitude, // 高空
    Ship,         // 艦船
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitStats {
    pub unit_type: UnitType,
    pub cost: u32,
    pub max_movement: u32,
    pub movement_type: MovementType,
    pub max_fuel: u32,
    pub max_ammo1: u32,
    pub max_ammo2: u32,
    pub min_range: u32,
    pub max_range: u32,
    pub daily_fuel_consumption: u32,
    pub can_capture: bool,
    pub can_supply: bool, // 補給能力を持つユニット（補給輸送車・空母）
    pub max_cargo: u32,   // 搷載可能数ﾈ0 = 搷載不可ﾉ
    pub loadable_unit_types: Vec<UnitType>, // 搷載可能なユニット種別ﾈ空リスト = 搷載不可ﾉ
}

#[derive(Debug, Clone)]
pub struct Unit {
    pub stats: UnitStats,
    pub hp: u32, // 0 to 100 (10x representation)
    pub fuel: u32,
    pub ammo1: u32,
    pub ammo2: u32,
    pub owner_player_id: u32,
    pub position: (usize, usize),
    pub has_moved: bool,
    pub action_completed: bool,
    pub cargo: Vec<usize>,              // 搷載中ユニットのインデックス一覧
    pub transport_index: Option<usize>, // 自分を違ぶ輸送ユニットのインデックス
}

impl Unit {
    pub fn new(stats: UnitStats, owner_player_id: u32, position: (usize, usize)) -> Self {
        Self {
            fuel: stats.max_fuel,
            ammo1: stats.max_ammo1,
            ammo2: stats.max_ammo2,
            stats,
            hp: 100,
            owner_player_id,
            position,
            has_moved: false,
            action_completed: true, // 生産後は即座に行動できない
            cargo: Vec::new(),
            transport_index: None,
        }
    }

    pub fn take_damage(&mut self, damage: u32) {
        if damage >= self.hp {
            self.hp = 0;
        } else {
            self.hp -= damage;
        }
    }

    pub fn is_destroyed(&self) -> bool {
        self.hp == 0
    }

    pub fn get_display_hp(&self) -> u32 {
        // Simple division, round up appropriately based on rules, e.g limit ceil
        (self.hp as f64 / 10.0).ceil() as u32
    }
}

/// 武器ダメージテーブル。主武器（ammo1）と副武器（ammo2）の両方を保持する。
pub struct DamageChart {
    // 主武器（ammo1）: Attacker → Defender → base damage
    matrix: HashMap<UnitType, HashMap<UnitType, u32>>,
    // 副武器（ammo2）: Attacker → Defender → base damage
    secondary_matrix: HashMap<UnitType, HashMap<UnitType, u32>>,
}

impl DamageChart {
    pub fn new() -> Self {
        Self {
            matrix: HashMap::new(),
            secondary_matrix: HashMap::new(),
        }
    }

    // 主武器のダメージ値を取得
    pub fn get_base_damage(&self, attacker: UnitType, defender: UnitType) -> Option<u32> {
        self.matrix
            .get(&attacker)
            .and_then(|defenders| defenders.get(&defender).copied())
    }

    // 副武器のダメージ値を取得
    pub fn get_base_damage_secondary(&self, attacker: UnitType, defender: UnitType) -> Option<u32> {
        self.secondary_matrix
            .get(&attacker)
            .and_then(|defenders| defenders.get(&defender).copied())
    }

    // 主武器のダメージ値を設定（CSVロード時に使用）
    pub fn insert_damage(&mut self, attacker: UnitType, defender: UnitType, damage: u32) {
        self.matrix
            .entry(attacker)
            .or_default()
            .insert(defender, damage);
    }

    // 副武器のダメージ値を設定
    pub fn insert_damage_secondary(&mut self, attacker: UnitType, defender: UnitType, damage: u32) {
        self.secondary_matrix
            .entry(attacker)
            .or_default()
            .insert(defender, damage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_dummy_infantry() -> UnitStats {
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
            max_cargo: 0,
            loadable_unit_types: vec![],
        }
    }

    #[test]
    fn test_unit_creation_and_damage() {
        let stats = create_dummy_infantry();
        let mut unit = Unit::new(stats, 1, (0, 0));

        assert_eq!(unit.hp, 100);
        assert_eq!(unit.get_display_hp(), 10);
        assert!(!unit.is_destroyed());

        unit.take_damage(25);
        assert_eq!(unit.hp, 75);
        assert_eq!(unit.get_display_hp(), 8); // 7.5 ceil
        assert!(!unit.is_destroyed());

        unit.take_damage(80);
        assert_eq!(unit.hp, 0);
        assert_eq!(unit.get_display_hp(), 0);
        assert!(unit.is_destroyed());
    }
}
