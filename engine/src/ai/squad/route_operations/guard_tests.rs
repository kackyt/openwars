//! 実ユニットを置いた、守備役と前進役の分離再現。
use super::*;

fn at(x: usize, y: usize) -> GridPosition {
    GridPosition { x, y }
}

fn spawn(
    world: &mut World,
    player: u32,
    kind: crate::resources::UnitType,
    at: GridPosition,
) -> Entity {
    let stats = world
        .resource::<crate::resources::master_data::MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            kind.as_str().to_owned(),
        ))
        .unwrap();
    world
        .spawn((
            Faction(PlayerId(player)),
            at,
            Health {
                current: 100,
                max: 100,
            },
            crate::components::Fuel {
                current: stats.max_fuel,
                max: stats.max_fuel,
            },
            crate::components::Ammo {
                ammo1: stats.max_ammo1,
                max_ammo1: stats.max_ammo1,
                ammo2: stats.max_ammo2,
                max_ammo2: stats.max_ammo2,
            },
            crate::components::HasMoved(false),
            crate::components::ActionCompleted(false),
            stats,
        ))
        .id()
}

fn fixture() -> (World, Vec<Entity>, RouteOrder) {
    let master = crate::resources::master_data::MasterDataRegistry::load().unwrap();
    let (mut world, _) = crate::setup::initialize_world_from_master_data(&master, "map_1").unwrap();
    world.clear_entities();
    let mut map = Map::new(
        20,
        7,
        crate::resources::Terrain::Plains,
        crate::resources::GridTopology::Square,
    );
    map.set_terrain(3, 3, crate::resources::Terrain::City)
        .unwrap();
    world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
    world.insert_resource(map);
    world.spawn((
        at(3, 3),
        Property::new(crate::resources::Terrain::City, Some(PlayerId(1)), 200),
    ));
    world.spawn((
        at(14, 3),
        Property::new(crate::resources::Terrain::City, Some(PlayerId(2)), 200),
    ));
    let own = [at(3, 3), at(4, 3), at(13, 3)]
        .into_iter()
        .map(|p| spawn(&mut world, 1, crate::resources::UnitType::MdTank, p))
        .collect::<Vec<_>>();
    spawn(&mut world, 2, crate::resources::UnitType::MdTank, at(14, 3));
    let order = RouteOrder {
        key_point: at(14, 3),
        staging: at(3, 3),
        defend: false,
        force: 54_000,
    };
    (world, own, order)
}

#[test]
fn rear_threat_keeps_a_guard_but_does_not_cancel_the_front_group() {
    let (mut world, members, order) = fixture();
    spawn(
        &mut world,
        2,
        crate::resources::UnitType::Infantry,
        at(2, 3),
    );
    let map = world.resource::<Map>();
    let (duties, retained) = responsibilities(
        &world,
        PlayerId(1),
        crate::ai::islands::IslandId(0),
        &[order],
        RouteOperationPlan {
            members: vec![members.clone()],
            free: vec![],
            launched: vec![true],
        },
        &mut Default::default(),
    );
    let guards = duties
        .iter()
        .filter(|duty| duty.mission_type == MissionType::Defense && duty.target == order.staging)
        .flat_map(|duty| duty.members.iter().copied())
        .collect::<crate::collections::HashSet<_>>();
    assert!(!guards.is_empty(), "後方都市への侵入に対処する部隊を残す");
    assert!(
        !guards.contains(&members[2]),
        "前線到達済みの部隊を後方へ呼び戻さない"
    );
    assert!(
        duties
            .iter()
            .any(|duty| duty.mission_type == MissionType::Attack
                && duty.members.contains(&members[2])
                && map.distance(duty.target.x, duty.target.y, 14, 3) <= 3),
        "守備を残して同じ作戦の前線部隊は攻勢を継続する"
    );
    let assigned = duties
        .iter()
        .flat_map(|duty| duty.members.iter().copied())
        .collect::<Vec<_>>();
    assert_eq!(assigned.len(), members.len(), "二重配属・未配属を作らない");
    assert!(members.iter().all(|entity| {
        assigned
            .iter()
            .filter(|assigned| *assigned == entity)
            .count()
            == 1
    }));
    assert_eq!(retained[0], members, "守備役も同じ進軍路の所属に残す");
}

#[test]
fn split_guard_and_front_orders_reach_normal_tactics_and_execution() {
    let (mut world, members, order) = fixture();
    spawn(
        &mut world,
        2,
        crate::resources::UnitType::Infantry,
        at(2, 3),
    );
    let front_enemy = world
        .iter_entities()
        .find(|entity| {
            entity.get::<GridPosition>() == Some(&order.key_point)
                && entity.get::<UnitStats>().is_some()
        })
        .unwrap()
        .id();
    let (duties, _) = responsibilities(
        &world,
        PlayerId(1),
        crate::ai::islands::IslandId(0),
        &[order],
        RouteOperationPlan {
            members: vec![members.clone()],
            free: vec![],
            launched: vec![true],
        },
        &mut Default::default(),
    );
    let guards = duties
        .iter()
        .filter(|duty| duty.mission_type == MissionType::Defense && duty.target == order.staging)
        .flat_map(|duty| duty.members.iter().copied())
        .collect::<Vec<_>>();
    let mut manager = SquadManager::default();
    crate::ai::squad::assign_campaign_responsibilities_scoped(
        &world,
        &mut manager,
        PlayerId(1),
        crate::ai::islands::IslandId(0),
        duties,
        &[MissionType::Attack, MissionType::Defense],
        crate::ai::squad::CampaignAssignmentScope::Full,
    );
    authorize_departures(&mut manager, PlayerId(1), &[members]);
    world.insert_resource(manager);
    let mut actions = Schedule::default();
    actions.add_systems(
        (
            crate::systems::movement::move_unit_system,
            crate::systems::combat::attack_unit_system,
            crate::systems::turn_management::wait_unit_system,
        )
            .chain(),
    );
    let mut skip = crate::collections::HashSet::default();
    let mut front_attacked = false;
    for _ in 0..3 {
        let Some((actor, command)) =
            crate::ai::tactics::v2::decide_action(&mut world, PlayerId(1), &skip)
        else {
            break;
        };
        if matches!(&command, crate::ai::command::AiCommand::Attack { target_entity, .. } if *target_entity == front_enemy)
        {
            front_attacked = true;
        }
        crate::ai::runtime::issue::execute_ai_command(&mut world, actor, command);
        actions.run(&mut world);
        skip.insert(actor);
    }
    assert!(front_attacked, "前進役の攻撃命令が通常戦術まで届く");
    assert!(
        world
            .get::<Health>(front_enemy)
            .is_none_or(|health| health.current < 100),
        "命令が発行されるだけでなく前線の敵へ実ダメージを与える"
    );
    assert!(!guards.is_empty());
    for guard in guards {
        let position = world.get::<GridPosition>(guard).unwrap();
        assert!(position.x <= 4, "守備役を前線へ引き抜かない: {position:?}");
    }
}

#[test]
fn no_rear_threat_does_not_invent_a_permanent_garrison() {
    let (world, members, order) = fixture();
    let duties = guard::duties(
        &world,
        world.resource::<Map>(),
        PlayerId(1),
        crate::ai::islands::IslandId(0),
        &order,
        &members,
        &mut Default::default(),
    );
    assert!(duties.is_empty(), "脅威のない都市に固定守備枠を設けない");
}

#[test]
fn an_unanswerable_rear_threat_does_not_release_the_guard_force() {
    let (mut world, _, order) = fixture();
    // 武器を持たない輸送車では、侵入する歩兵への守備を引き受けられない。
    let truck = spawn(
        &mut world,
        1,
        crate::resources::UnitType::SupplyTruck,
        at(3, 4),
    );
    spawn(
        &mut world,
        2,
        crate::resources::UnitType::Infantry,
        at(2, 3),
    );
    let duties = guard::duties(
        &world,
        world.resource::<Map>(),
        PlayerId(1),
        crate::ai::islands::IslandId(0),
        &order,
        &[truck],
        &mut Default::default(),
    );
    assert_eq!(duties.len(), 1);
    assert_eq!(duties[0].members, vec![truck]);
    assert_eq!(duties[0].mission_type, MissionType::Defense);
}
