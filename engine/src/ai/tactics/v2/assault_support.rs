//! GO中の前衛と間接火力が共有する突破対象。
use super::*;
use crate::components::{ActionCompleted, Faction, HasMoved, Transporting, UnitStats};

/// 前衛から首都へ向かう帯にいる敵から、支援対象を1体選ぶ。
/// 今撃てる敵を優先し、その中では敵砲・首都圏の敵・前衛に近い敵の順で選ぶ。
/// 行動適用後に作り直すため、撃破済みの敵へ攻撃を重ねることはない。
pub(super) fn select_target(
    world: &World,
    board: &TacticalBoard,
    objective: GridPosition,
) -> Option<(Entity, GridPosition)> {
    let mut front = Vec::new();
    let mut fireable = HashSet::default();
    for entity in world.iter_entities() {
        if entity
            .get::<Faction>()
            .is_none_or(|f| f.0 != board.player_id)
            || entity.get::<Transporting>().is_some()
        {
            continue;
        }
        let (Some(stats), Some(position)) =
            (entity.get::<UnitStats>(), entity.get::<GridPosition>())
        else {
            continue;
        };
        if stats.max_range == 0 {
            continue;
        }
        if stats.min_range <= 1 {
            front.push((*position, stats.max_movement));
        } else if entity.get::<ActionCompleted>().is_some_and(|done| !done.0)
            && entity.get::<HasMoved>().is_some_and(|moved| !moved.0)
        {
            // 損害0や弾切れの射撃を「支援可能」として扱わない。
            for target in board
                .action_targets
                .action_targets_at(entity.id(), stats, *position, false)
                .attackable_targets
            {
                if crate::ai::pruning::evaluate_attack_exchange(
                    world,
                    entity.id(),
                    target,
                    *position,
                    &board.damage_chart,
                )
                .is_some_and(|exchange| exchange.expected_damage > 0)
                {
                    fireable.insert(target);
                }
            }
        }
    }
    world
        .iter_entities()
        .filter_map(|enemy| {
            if enemy
                .get::<Faction>()
                .is_none_or(|f| f.0 == board.player_id)
                || enemy.get::<Transporting>().is_some()
            {
                return None;
            }
            let position = *enemy.get::<GridPosition>()?;
            let stats = enemy.get::<UnitStats>()?;
            if stats.max_range == 0 {
                return None;
            }
            let cap_distance = board
                .map
                .distance(position.x, position.y, objective.x, objective.y);
            let front_distance = front
                .iter()
                .filter_map(|(p, movement)| {
                    let distance = board.map.distance(position.x, position.y, p.x, p.y);
                    let own_cap_distance = board.map.distance(p.x, p.y, objective.x, objective.y);
                    // 前衛の進行方向におり、次の接敵帯か、前衛を射程に収める敵。
                    (cap_distance <= own_cap_distance
                        && distance <= movement.saturating_add(stats.max_range))
                    .then_some(distance)
                })
                .min()?;
            let key = (
                !fireable.contains(&enemy.id()),
                stats.min_range <= 1,
                cap_distance > crate::systems::production::PRODUCTION_RANGE as u32,
                front_distance,
                position.y,
                position.x,
            );
            Some((key, enemy.id(), position))
        })
        .min_by_key(|(key, ..)| *key)
        .map(|(_, entity, position)| (entity, position))
}
