//! 行動選択で使う1体のunitの状態（版によらない部分）。

use super::board::TacticalBoard;
use crate::components::{GridPosition, Health, UnitStats};
use bevy_ecs::prelude::*;

/// 敵の脅威を「近い」とみなす格子距離（敵の移動+射程に余裕を持たせた半径）。
const THREAT_PROXIMITY_RADIUS: u32 = 8;

pub(crate) struct UnitState {
    pub(crate) entity: Entity,
    pub(crate) stats: UnitStats,
    pub(crate) position: GridPosition,
    pub(crate) fuel: u32,
    pub(crate) hp: u32,
    pub(crate) ammo: (u32, u32),
    /// HP70未満、または主兵装の弾切れ。
    pub(crate) combat_ineffective: bool,
    /// 敵が近くにいる（防御地形への退避を意味のある局面に限定するためのゲート）。
    pub(crate) threat_nearby: bool,
}

impl UnitState {
    /// 必要な状態（性能・位置・燃料・HP）が欠けているunitはNone。
    pub(crate) fn observe(world: &World, board: &TacticalBoard, entity: Entity) -> Option<Self> {
        let stats = world.get::<UnitStats>(entity)?.clone();
        let position = *world.get::<GridPosition>(entity)?;
        let fuel = world.get::<crate::components::Fuel>(entity)?.current;
        let hp = world.get::<Health>(entity)?.current;
        let ammo = world
            .get::<crate::components::Ammo>(entity)
            .map(|ammo| (ammo.ammo1, ammo.ammo2))
            .unwrap_or((99, 99));
        let combat_ineffective = hp < 70 || (stats.max_ammo1 > 0 && ammo.0 == 0);
        let threat_nearby = board.enemy_positions().any(|enemy| {
            (enemy.x.abs_diff(position.x) + enemy.y.abs_diff(position.y)) as u32
                <= THREAT_PROXIMITY_RADIUS
        });
        Some(Self {
            entity,
            stats,
            position,
            fuel,
            hp,
            ammo,
            combat_ineffective,
            threat_nearby,
        })
    }

    pub(crate) fn is_infantry(&self) -> bool {
        matches!(
            self.stats.unit_type,
            crate::resources::UnitType::Infantry | crate::resources::UnitType::Mech
        )
    }
}
