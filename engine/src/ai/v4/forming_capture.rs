//! 輸送待ちの占領役による足元の拠点の占領。
//!
//! 輸送Squadの積荷（乗船待ちの占領役）は通常行動から外れ、出発の許可が出るまで
//! 集結地で待機する。集結地が未所有の拠点の上だと、その拠点は誰にも占領されない
//! （同じ拠点を狙う占領役もマスが塞がれて入れない）。島の確保が首都攻略の前提に
//! なっている盤面では、最後の1拠点が残り続けて首都攻略が永久に始まらない。
//! 待機中でも、足元の未所有の拠点は占領させる。

use crate::ai::command::AiCommand;
use crate::ai::squad::{MissionType, SquadManager, TransportPhase};
use crate::collections::HashSet;
use crate::components::{
    ActionCompleted, Faction, GridPosition, HasMoved, PlayerId, Property, Transporting, UnitStats,
};
use bevy_ecs::prelude::*;

/// 乗船待ちの占領役のうち、未所有の拠点の上に立っている1体に占領を命じる。
pub(crate) fn decide_forming_capture(
    world: &mut World,
    player_id: PlayerId,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let waiting: HashSet<Entity> = {
        let manager = world.get_resource::<SquadManager>()?;
        manager
            .squads
            .iter()
            .filter(|squad| {
                squad.plan.owner_id == Some(player_id)
                    && squad.plan.mission_type == MissionType::Transport
                    && !matches!(
                        squad.plan.phase,
                        crate::ai::squad::MissionPhase::Transport(
                            TransportPhase::Transit | TransportPhase::Drop
                        )
                    )
            })
            .flat_map(|squad| {
                squad
                    .cargo_entities
                    .iter()
                    .chain(squad.members.iter())
                    .copied()
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    if waiting.is_empty() {
        return None;
    }
    let unowned: HashSet<GridPosition> = world
        .query::<(&GridPosition, &Property)>()
        .iter(world)
        .filter(|(_, property)| {
            property.owner_id != Some(player_id) && property.max_capture_points > 0
        })
        .map(|(position, _)| *position)
        .collect();
    let mut query = world.query_filtered::<(
        Entity,
        &Faction,
        &GridPosition,
        &UnitStats,
        &HasMoved,
        &ActionCompleted,
    ), Without<Transporting>>();
    query
        .iter(world)
        .filter(|(entity, faction, position, stats, moved, done)| {
            faction.0 == player_id
                && waiting.contains(entity)
                && !skip.contains(entity)
                && !moved.0
                && !done.0
                && stats.can_capture
                && unowned.contains(position)
        })
        .map(|(entity, _, position, ..)| (entity, *position))
        .min_by_key(|(entity, _)| *entity)
        .map(|(entity, position)| {
            (
                entity,
                AiCommand::Capture {
                    target_pos: position,
                },
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::squad::{MissionPhase, SquadManager};
    use crate::ai::v4::maritime::test_support::{naval_world, property, spawn};
    use crate::resources::UnitType;

    #[test]
    fn waiting_capturer_captures_the_property_it_stands_on() {
        let mut world = naval_world(&["H.C."]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 0, 2, 0);
        let on_city = spawn(&mut world, 1, UnitType::Infantry, 2, 0);
        let on_plains = spawn(&mut world, 1, UnitType::Infantry, 1, 0);
        let mut manager = SquadManager::new();
        {
            let mut squad = manager.create_owned_squad(MissionType::Transport, PlayerId(1));
            squad.plan_mut().phase = MissionPhase::Forming;
            squad.members.extend([on_city, on_plains]);
        }
        world.insert_resource(manager);

        let (entity, command) =
            decide_forming_capture(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        assert_eq!(entity, on_city);
        assert!(matches!(
            command,
            AiCommand::Capture { target_pos } if target_pos == GridPosition { x: 2, y: 0 }
        ));
        // 行動済みなら対象外。
        let skip = HashSet::from_iter([on_city]);
        assert!(decide_forming_capture(&mut world, PlayerId(1), &skip).is_none());
    }
}
