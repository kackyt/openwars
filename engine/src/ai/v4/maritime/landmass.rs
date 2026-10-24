//! 陸塊（地上部隊が陸路で行き来できるセルのまとまり）ごとの作戦需要。
//!
//! 島で隔てられた盤面では、陸塊ごとに「まだ獲る拠点があるか」「敵が居るか」が
//! 異なる。仕事の無い陸塊に居る地上部隊（例: 全拠点を確保済みの本拠島）は、
//! 輸送しない限り戦局に寄与しない。これを「取り残された部隊」と呼ぶ。

use super::theater::Components;
use crate::ai::context::board::{BoardUnit, PlayerBoard};
use crate::collections::HashMap;
use crate::components::{GridPosition, PlayerId};
use crate::resources::{MovementType, Terrain};

/// 本拠へ戦闘役を送り込むのを止める、敵の上陸部隊に対する自軍の戦力の比（%）。
const HOME_DEFENSE_PERCENT: i64 = 150;

/// 1つの陸塊の事実。
#[derive(Debug, Clone, Default)]
pub(crate) struct LandmassInfo {
    /// 自軍が所有していない拠点（中立・敵）の座標。
    pub(crate) unowned: Vec<GridPosition>,
    /// 敵が所有する拠点の数。
    pub(crate) enemy_owned: u32,
    /// 敵首都があるか。
    pub(crate) enemy_capital: bool,
    /// 陸塊上の敵地上部隊の数。
    pub(crate) enemy_ground: u32,
    /// 陸塊上の自軍の占領可能な部隊の数。
    pub(crate) own_capturers: u32,
    /// 陸塊上の自軍の地上部隊の数。
    pub(crate) own_ground: u32,
    /// 自軍の首都があるか。
    pub(crate) own_capital: bool,
    /// 陸塊上の自軍の地上部隊の戦力（生産費×HP）。
    pub(crate) own_strength: i64,
    /// 陸塊上の敵の地上部隊の戦力（生産費×HP）。
    pub(crate) enemy_strength: i64,
}

impl LandmassInfo {
    /// 占領役をまだ受け入れられる数。
    pub(crate) fn capture_need(&self) -> u32 {
        (self.unowned.len() as u32).saturating_sub(self.own_capturers)
    }

    /// 戦闘役を送る理由（敵部隊・敵拠点）があるか。
    pub(crate) fn needs_combat(&self) -> bool {
        self.enemy_ground > 0 || self.enemy_owned > 0
    }

    /// 輸送で戦闘役を送り込む価値があるか。
    ///
    /// 自軍の首都の陸塊（本拠）は、そこに居る自軍の戦力が敵の上陸部隊を
    /// [`HOME_DEFENSE_PERCENT`] まで上回る間は送らない。少数の敵が本拠へ上陸するたびに、
    /// 積んだ部隊を本拠へ降ろし直して往復すると、敵の本土への揚陸が進まない。
    pub(crate) fn wants_combat(&self) -> bool {
        self.needs_combat()
            && !(self.own_capital
                && self.own_strength * 100 >= self.enemy_strength * HOME_DEFENSE_PERCENT)
    }
}

/// 陸塊の塗り分けと、陸塊ごとの事実。
pub(crate) struct Landmasses {
    pub(crate) land: Components,
    pub(crate) info: HashMap<u32, LandmassInfo>,
}

/// 地上を移動する部隊か（航空・艦船を除く）。
pub(crate) fn is_ground(unit: &BoardUnit) -> bool {
    !matches!(
        unit.stats.movement_type,
        MovementType::Air | MovementType::Ship
    )
}

impl Landmasses {
    /// 盤面から陸塊ごとの事実を集める。
    pub(crate) fn observe(board: &PlayerBoard, player_id: PlayerId) -> Self {
        let land = Components::label(board, MovementType::Infantry);
        let mut info: HashMap<u32, LandmassInfo> = HashMap::default();
        for property in &board.properties {
            let Some(landmass) = land.at(property.position) else {
                continue;
            };
            let entry = info.entry(landmass).or_default();
            if property.owner != Some(player_id) {
                entry.unowned.push(property.position);
            }
            if property.owner == Some(player_id) && property.terrain == Terrain::Capital {
                entry.own_capital = true;
            }
            if property.owner.is_some_and(|owner| owner != player_id) {
                entry.enemy_owned += 1;
                if property.terrain == Terrain::Capital {
                    entry.enemy_capital = true;
                }
            }
        }
        for unit in board
            .units
            .iter()
            .filter(|unit| unit.is_deployed() && is_ground(unit))
        {
            let Some(landmass) = land.at(unit.position) else {
                continue;
            };
            let entry = info.entry(landmass).or_default();
            let strength = i64::from(unit.stats.cost) * i64::from(unit.health.unwrap_or(100)) / 100;
            if unit.owner == player_id {
                entry.own_ground += 1;
                entry.own_strength += strength;
                if unit.stats.can_capture {
                    entry.own_capturers += 1;
                }
            } else {
                entry.enemy_ground += 1;
                entry.enemy_strength += strength;
            }
        }
        // 拠点の並びを座標順に固定し、走査順に依存しない判断にする。
        for entry in info.values_mut() {
            entry
                .unowned
                .sort_by_key(|position| (position.y, position.x));
        }
        Self { land, info }
    }

    /// 座標の陸塊の事実。
    pub(crate) fn at(&self, position: GridPosition) -> Option<(u32, &LandmassInfo)> {
        let landmass = self.land.at(position)?;
        self.info.get(&landmass).map(|info| (landmass, info))
    }

    /// 自軍の地上部隊が今の陸塊で仕事を持たないか。
    ///
    /// 占領役は未所有拠点も敵部隊も無い陸塊で、戦闘役は敵部隊も敵拠点も無い陸塊で
    /// 取り残されている。
    pub(crate) fn is_stranded(&self, unit: &BoardUnit) -> bool {
        if !is_ground(unit) || !unit.is_deployed() {
            return false;
        }
        let Some((_, info)) = self.at(unit.position) else {
            return false;
        };
        if unit.stats.can_capture {
            info.unowned.is_empty() && info.enemy_ground == 0
        } else {
            !info.needs_combat()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::context::board::BoardUnit;
    use crate::ai::v4::maritime::test_support::board_from_rows;
    use crate::resources::UnitType;
    use crate::resources::master_data::UnitName;

    fn unit(board: &PlayerBoard, owner: u32, unit_type: UnitType, x: usize, y: usize) -> BoardUnit {
        BoardUnit {
            entity: bevy_ecs::entity::Entity::from_raw((x * 100 + y) as u32),
            owner: PlayerId(owner),
            position: GridPosition { x, y },
            stats: board
                .registry
                .create_unit_stats(&UnitName(unit_type.as_str().to_owned()))
                .unwrap(),
            transported: false,
            cargo: None,
            health: Some(100),
            ammo: None,
            fuel: None,
            action_completed: Some(false),
        }
    }

    #[test]
    fn units_on_a_fully_owned_island_are_stranded() {
        // 左の島は自軍の首都だけ。右の島に中立都市。
        let mut board = board_from_rows(&["H.~~C."], &[(0, 0, 1), (4, 0, 0)]);
        let infantry = unit(&board, 1, UnitType::Infantry, 1, 0);
        let tank = unit(&board, 1, UnitType::Tank, 0, 0);
        board.units = vec![infantry.clone(), tank.clone()];
        let landmasses = Landmasses::observe(&board, PlayerId(1));
        assert!(landmasses.is_stranded(&infantry));
        assert!(landmasses.is_stranded(&tank));
        let city = landmasses.at(GridPosition { x: 4, y: 0 }).unwrap().1;
        assert_eq!(city.capture_need(), 1);
        assert!(!city.needs_combat());
    }

    #[test]
    fn capturer_with_unowned_property_on_its_island_is_not_stranded() {
        let mut board = board_from_rows(&["HC~~C."], &[(0, 0, 1), (1, 0, 0), (4, 0, 0)]);
        let infantry = unit(&board, 1, UnitType::Infantry, 0, 0);
        board.units = vec![infantry.clone()];
        let landmasses = Landmasses::observe(&board, PlayerId(1));
        assert!(!landmasses.is_stranded(&infantry));
    }
}
