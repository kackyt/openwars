//! 次の敵手番に、自軍の艦船・輸送部隊があるセルで受ける期待損害。
//!
//! 移動後の攻撃は隣接（距離1）からしかできない。直接攻撃の脅威は「敵がそのセルの
//! 隣へ移動して止まれるか」で数える。距離（移動力+1）だけで数えると、海へ入れない
//! 陸上部隊が沖合の艦船を脅かすことになり、戦艦や輸送船が海岸へ近づけなくなる。
//! 間接射撃は移動しないで届く射程内だけを数える。

use super::theater::cells_within_ring;
use crate::ai::context::board::{BoardUnit, PlayerBoard};
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, PlayerId};
use crate::resources::DamageChart;
use crate::systems::combat::get_expected_damage;
use crate::systems::movement::{OccupantInfo, calculate_reachable_tile_costs};
use bevy_ecs::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// 盤面のセル座標。
type Cell = (usize, usize);

/// 敵部隊ごとの直接攻撃の届くセルを、必要になった敵の分だけ求めて保持する。
pub(crate) struct ThreatMap<'a> {
    board: &'a PlayerBoard,
    chart: &'a DamageChart,
    occupancy: &'a HashMap<Cell, OccupantInfo>,
    enemies: Vec<&'a BoardUnit>,
    /// 敵部隊が次の手番に隣接して直接攻撃できるセル。
    direct_cells: RefCell<HashMap<Entity, Rc<HashSet<Cell>>>>,
}

impl<'a> ThreatMap<'a> {
    pub(crate) fn new(
        board: &'a PlayerBoard,
        chart: &'a DamageChart,
        occupancy: &'a HashMap<Cell, OccupantInfo>,
        player_id: PlayerId,
    ) -> Self {
        let enemies = board
            .units
            .iter()
            .filter(|unit| unit.owner != player_id && unit.is_deployed())
            .collect();
        Self {
            board,
            chart,
            occupancy,
            enemies,
            direct_cells: RefCell::default(),
        }
    }

    /// 自軍の部隊 `target` がセル `cell` に止まった場合に、次の敵手番に受ける期待損害
    /// （HP、100満点）。全ての敵が同じ部隊を狙う上限見積もりで、現在のHPで頭打ちにする。
    pub(crate) fn damage_hp(&self, target: &BoardUnit, cell: Cell) -> u32 {
        let position = GridPosition {
            x: cell.0,
            y: cell.1,
        };
        let defense = self
            .board
            .map
            .get_terrain(cell.0, cell.1)
            .map(|terrain| self.board.registry.get_terrain_defense_bonus(terrain))
            .unwrap_or(0);
        let total: u32 = self
            .enemies
            .iter()
            .map(|enemy| {
                let attack = |distance: u32| {
                    get_expected_damage(
                        &enemy.stats,
                        enemy.health.unwrap_or(100),
                        enemy
                            .ammo
                            .unwrap_or((enemy.stats.max_ammo1, enemy.stats.max_ammo2)),
                        &target.stats,
                        defense,
                        distance,
                        &self.board.registry,
                        self.chart,
                        false,
                    )
                };
                let distance = self.board.map.distance(
                    position.x,
                    position.y,
                    enemy.position.x,
                    enemy.position.y,
                );
                // 移動せずに撃つ間接射撃。
                let indirect = if distance >= 2 { attack(distance) } else { 0 };
                // 隣へ移動してから撃つ直接攻撃。移動力+1より遠い敵は経路を調べない。
                let direct = if distance <= enemy.stats.max_movement + 1
                    && self.can_strike_directly(enemy, cell)
                {
                    attack(1)
                } else {
                    0
                };
                direct.max(indirect)
            })
            .sum();
        total.min(target.health.unwrap_or(100))
    }

    /// 敵 `enemy` が次の手番に、セル `cell` の隣へ移動して止まれるか。
    fn can_strike_directly(&self, enemy: &BoardUnit, cell: Cell) -> bool {
        self.direct_cells_of(enemy).contains(&cell)
    }

    /// 敵 `enemy` が次の手番に隣接して直接攻撃できるセルの集合。
    fn direct_cells_of(&self, enemy: &BoardUnit) -> Rc<HashSet<Cell>> {
        if let Some(cached) = self.direct_cells.borrow().get(&enemy.entity) {
            return Rc::clone(cached);
        }
        let start = (enemy.position.x, enemy.position.y);
        let reachable = calculate_reachable_tile_costs(
            &self.board.map,
            self.occupancy,
            start,
            enemy.stats.movement_type,
            enemy.stats.max_movement,
            enemy.fuel.unwrap_or(enemy.stats.max_fuel),
            enemy.owner,
            enemy.stats.unit_type,
            &self.board.registry,
        );
        let mut cells = HashSet::default();
        // 他の部隊が居るセルには止まれない（自身の現在地は止まれる）。
        for stop in reachable
            .keys()
            .filter(|stop| **stop == start || !self.occupancy.contains_key(*stop))
        {
            cells.extend(cells_within_ring(
                &self.board.map,
                GridPosition {
                    x: stop.0,
                    y: stop.1,
                },
                1,
                1,
            ));
        }
        let cells = Rc::new(cells);
        self.direct_cells
            .borrow_mut()
            .insert(enemy.entity, Rc::clone(&cells));
        cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::v4::maritime::test_support::{naval_world, spawn};
    use crate::resources::UnitType;

    fn threat_at(world: &mut World, target: Entity, cell: Cell) -> u32 {
        let board = PlayerBoard::observe(world, PlayerId(1));
        let chart = world
            .get_resource::<DamageChart>()
            .cloned()
            .unwrap_or_default();
        let occupancy = board.occupancy();
        let threat = ThreatMap::new(&board, &chart, &occupancy, PlayerId(1));
        let unit = board
            .units
            .iter()
            .find(|unit| unit.entity == target)
            .unwrap();
        threat.damage_hp(unit, cell)
    }

    #[test]
    fn land_units_threaten_only_cells_next_to_the_shore() {
        // 陸地(x=0..2)と海(x=3..)。戦車は海へ入れないので、海岸の隣の海だけを撃てる。
        let mut world = naval_world(&["...~~~~~"]);
        let lander = spawn(&mut world, 1, UnitType::Lander, 7, 0);
        spawn(&mut world, 2, UnitType::Tank, 0, 0);

        assert!(
            threat_at(&mut world, lander, (3, 0)) > 0,
            "海岸の隣は撃てる"
        );
        // 移動力+1以内（距離5）でも、隣接できない沖合は脅威ではない。
        assert_eq!(threat_at(&mut world, lander, (5, 0)), 0);
    }
}
