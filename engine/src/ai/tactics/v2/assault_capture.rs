//! GO中も歩兵は占領担当を維持する。戦闘担当とは目標と攻撃許可を分ける。
use super::*;
use crate::components::{ActionCompleted, Faction, HasMoved, Transporting, UnitStats};

/// 占領中の物件を最優先し、次に元の割当目標を維持する。
pub(super) fn retained_target(board: &TacticalBoard, state: &UnitState) -> Option<GridPosition> {
    let available = |target: GridPosition| {
        board
            .properties
            .iter()
            .any(|(p, _, owner)| *p == target && *owner != Some(board.player_id))
    };
    if available(state.position) {
        Some(state.position)
    } else {
        board
            .squads
            .target(state.entity)
            .filter(|target| available(*target))
    }
}

/// 元の担当を持たない歩兵だけに新しい占領先を割り当てる。
/// 敵の有効な生産口を優先し、残りは首都方向の未確保物件を実経路距離で分担する。
pub(super) fn assign_unclaimed(
    board: &TacticalBoard,
    unit: &mut UnitTactics,
    blockade: Option<&EnemyProductionBlockade>,
    claimed: &mut HashSet<GridPosition>,
    distances: &mut ApproachDistances,
) {
    let Some(objective) = unit.assault_objective else {
        return;
    };
    if !unit.state.stats.can_capture {
        return;
    }
    if let Some(target) = unit.squad_target {
        claimed.insert(target);
        return;
    }
    let own_distance = board.map.distance(
        unit.state.position.x,
        unit.state.position.y,
        objective.x,
        objective.y,
    );
    let sites = board
        .properties
        .iter()
        .filter(|(p, _, owner)| {
            *owner != Some(board.player_id)
                && !claimed.contains(p)
                && board.map.distance(p.x, p.y, objective.x, objective.y) <= own_distance
        })
        .map(|(p, _, _)| *p)
        .collect::<Vec<_>>();
    let production = sites
        .iter()
        .copied()
        .filter(|p| blockade.is_some_and(|b| b.sites.contains(p)))
        .collect::<Vec<_>>();
    let mut choose = |targets: Vec<GridPosition>| {
        targets
            .into_iter()
            .filter_map(|target| {
                let turns = distances.turns(&unit.state.stats, unit.state.position, target, 0);
                (turns.used_mp != u32::MAX).then_some((turns, target))
            })
            .min_by_key(|(turns, p)| (*turns, p.y, p.x))
    };
    let nearest = choose(production).or_else(|| choose(sites));
    if let Some((_, target)) = nearest {
        unit.squad_target = Some(target);
        unit.is_solo = false;
        claimed.insert(target);
    }
}

/// 歩兵の攻撃は担当物件を直接塞ぐ敵への補助に限定する。
/// 同じ手番に戦車・間接兵器で有効な攻撃ができるなら、その攻撃を先に任せる。
pub(super) fn needs_attack(
    world: &World,
    board: &TacticalBoard,
    unit: &UnitTactics,
    target: Entity,
) -> bool {
    if unit.assault_objective.is_none() || !unit.state.stats.can_capture {
        return true;
    }
    let Some(at) = world.get::<GridPosition>(target).copied() else {
        return false;
    };
    if unit.squad_target != Some(at) {
        return false;
    }
    for entity in world.iter_entities() {
        if entity
            .get::<Faction>()
            .is_none_or(|f| f.0 != board.player_id)
            || entity.get::<Transporting>().is_some()
            || entity.get::<ActionCompleted>().is_none_or(|d| d.0)
            || entity.get::<HasMoved>().is_none_or(|m| m.0)
        {
            continue;
        }
        let (Some(stats), Some(position), Some(fuel)) = (
            entity.get::<UnitStats>(),
            entity.get::<GridPosition>(),
            entity.get::<crate::components::Fuel>(),
        ) else {
            continue;
        };
        if stats.can_capture || stats.max_range == 0 {
            continue;
        }
        let positions = if stats.min_range > 1 {
            std::collections::BTreeSet::from([(position.x, position.y)])
        } else {
            calculate_reachable_tiles(
                &board.map,
                &board.occupancy,
                (position.x, position.y),
                stats.movement_type,
                stats.max_movement,
                fuel.current,
                board.player_id,
                stats.unit_type,
                &board.registry,
            )
        };
        for (x, y) in positions {
            let from = GridPosition { x, y };
            if board
                .action_targets
                .action_targets_at(entity.id(), stats, from, from != *position)
                .attackable_targets
                .contains(&target)
                && crate::ai::pruning::evaluate_attack_exchange(
                    world,
                    entity.id(),
                    target,
                    from,
                    &board.damage_chart,
                )
                .is_some_and(|exchange| exchange.expected_damage > 0)
            {
                return false;
            }
        }
    }
    true
}
