//! 作戦再編で、進軍済みの異なる方面を多数決で一つの小隊へ押し込まない。
use super::*;

pub(super) fn partition(
    world: &World,
    player: PlayerId,
    duties: Vec<CampaignResponsibility>,
) -> Vec<CampaignResponsibility> {
    let mut partitioned = Vec::new();
    for duty in duties {
        if !matches!(
            duty.mission_type,
            MissionType::Attack | MissionType::Defense
        ) {
            partitioned.push(duty);
            continue;
        }
        let mut groups = std::collections::BTreeMap::<Option<(usize, usize)>, Vec<Entity>>::new();
        for member in duty.members {
            let axis = crate::ai::v4::capital_route::commitment::carried_axis_for_entity(
                world, player, member,
            )
            .map(|at| (at.x, at.y));
            groups.entry(axis).or_default().push(member);
        }
        for members in groups.into_values() {
            partitioned.push(CampaignResponsibility {
                mission_type: duty.mission_type.clone(),
                target: duty.target,
                members,
            });
        }
    }
    partitioned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_regrouping_preserves_both_progressed_axes_and_keeps_new_units_free() {
        use crate::ai::v4::capital_route::{carried_tests, commitment};
        let (mut world, south, _) = carried_tests::fixture();
        let stats = world.get::<UnitStats>(south).unwrap().clone();
        let north = world
            .spawn((
                Faction(PlayerId(1)),
                GridPosition { x: 19, y: 6 },
                stats.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let mut manager = world.remove_resource::<SquadManager>().unwrap();
        let mut squad = manager.create_owned_squad(MissionType::Attack, PlayerId(1));
        squad.members.insert(north);
        squad.plan_mut().target = Some(GridPosition { x: 20, y: 3 });
        world.insert_resource(manager);
        carried_tests::refresh(&mut world);
        assert_eq!(
            commitment::carried_axis_for_entity(&world, PlayerId(1), north),
            Some(GridPosition { x: 18, y: 6 })
        );
        let fresh = world
            .spawn((
                Faction(PlayerId(1)),
                GridPosition { x: 11, y: 4 },
                stats,
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let mut manager = world.remove_resource::<SquadManager>().unwrap();
        assign_campaign_responsibilities_scoped(
            &world,
            &mut manager,
            PlayerId(1),
            crate::ai::islands::IslandId(0),
            vec![CampaignResponsibility {
                mission_type: MissionType::Attack,
                target: GridPosition { x: 21, y: 16 },
                members: vec![south, north, fresh],
            }],
            &[MissionType::Attack, MissionType::Defense],
            CampaignAssignmentScope::Full,
        );
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|s| s.members.iter().any(|e| [south, north, fresh].contains(e)))
                .count(),
            3,
            "方面の異なる前進部隊と新規部隊を多数決で混ぜない"
        );
        world.insert_resource(manager);
        carried_tests::refresh(&mut world);
        let diagnostic = commitment::capital_route_diagnostics_for_player(&world, PlayerId(1));
        let graph = &diagnostic["routes"][0];
        for (entity, axis) in [
            (south, GridPosition { x: 8, y: 18 }),
            (north, GridPosition { x: 18, y: 6 }),
        ] {
            let unit = graph["ground_units"]
                .as_array()
                .unwrap()
                .iter()
                .find(|unit| unit["entity_id"].as_u64() == Some(entity.to_bits()))
                .unwrap();
            let route = unit["route_index"].as_u64().unwrap() as usize;
            assert_eq!(
                graph["routes"][route]["front_x"].as_u64(),
                Some(axis.x as u64)
            );
            assert_eq!(
                graph["routes"][route]["front_y"].as_u64(),
                Some(axis.y as u64)
            );
        }
        assert!(commitment::carried_axis_for_entity(&world, PlayerId(1), fresh).is_none());
    }
}
