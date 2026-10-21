//! 施設を目的とする戦闘割当の振る舞い。兵種相性・到着・必要数・追撃範囲を固定する。

use super::*;
use crate::components::{Ammo, Faction, Fuel, Health, Property};
use crate::resources::{GridTopology, MovementType, Terrain};

const ME: PlayerId = PlayerId(1);
const ENEMY: PlayerId = PlayerId(2);

fn pos(x: usize, y: usize) -> GridPosition {
    GridPosition { x, y }
}

/// 平地だけの盤面。`objective` に未所有の都市を置く。
fn board(width: usize, height: usize, objective: GridPosition) -> World {
    let mut world = World::new();
    let mut map = Map::new(width, height, Terrain::Plains, GridTopology::Square);
    map.set_terrain(objective.x, objective.y, Terrain::City)
        .unwrap();
    world.spawn((objective, Property::new(Terrain::City, None, 200)));
    world.insert_resource(IslandMap::analyze(&map));
    world.insert_resource(map);
    world.insert_resource(MasterDataRegistry::load().unwrap());
    let mut chart = DamageChart::new();
    for (attacker, defender, damage) in [
        (UnitType::Tank, UnitType::Tank, 55),
        (UnitType::Tank, UnitType::Infantry, 75),
        (UnitType::AntiAir, UnitType::Bcopters, 105),
        (UnitType::AntiAir, UnitType::Tank, 25),
        (UnitType::Bcopters, UnitType::Infantry, 75),
    ] {
        chart.insert_damage(attacker, defender, damage);
        chart.insert_secondary_damage(attacker, defender, damage);
    }
    world.insert_resource(chart);
    world
}

fn unit(world: &mut World, owner: PlayerId, kind: UnitType, at: GridPosition, hp: u32) -> Entity {
    let (movement_type, max_movement) = match kind {
        UnitType::Infantry => (MovementType::Infantry, 3),
        UnitType::Bcopters => (MovementType::Air, 6),
        _ => (MovementType::Tank, 6),
    };
    world
        .spawn((
            at,
            Faction(owner),
            Health {
                current: hp,
                max: 100,
            },
            Fuel {
                current: 99,
                max: 99,
            },
            Ammo {
                ammo1: 9,
                max_ammo1: 9,
                ammo2: 9,
                max_ammo2: 9,
            },
            UnitStats {
                unit_type: kind,
                can_capture: kind == UnitType::Infantry,
                min_range: 1,
                max_range: 1,
                max_movement,
                movement_type,
                max_fuel: 99,
                max_ammo1: 9,
                max_ammo2: 9,
                cost: 7000,
                ..UnitStats::mock()
            },
        ))
        .id()
}

fn island(world: &World, at: GridPosition) -> IslandId {
    world.resource::<IslandMap>().get_island_at(&at).unwrap().id
}

fn allocate(
    world: &World,
    members: &[Entity],
    capturers: &[Entity],
    objective: GridPosition,
) -> Vec<CampaignResponsibility> {
    objective_combat_responsibilities(
        world,
        ME,
        island(world, objective),
        members,
        capturers,
        &[objective],
        &mut TerrainConnectivity::default(),
    )
}

fn responsibility_of(
    responsibilities: &[CampaignResponsibility],
    member: Entity,
) -> &CampaignResponsibility {
    responsibilities
        .iter()
        .find(|responsibility| responsibility.members.contains(&member))
        .expect("全メンバーに任務がある")
}

/// 地上の脅威には対地の有効打を持つ戦車、航空の脅威には対空車を割り当てる。
/// 先に近い敵戦車を処理しても、ヘリにしか当たらない対空車を戦車の相手へ回さない。
#[test]
fn each_threat_gets_a_unit_that_can_actually_damage_it() {
    let objective = pos(15, 1);
    let mut world = board(30, 3, objective);
    let capturer = unit(&mut world, ME, UnitType::Infantry, pos(10, 1), 100);
    let tank = unit(&mut world, ME, UnitType::Tank, pos(11, 0), 100);
    let anti_air = unit(&mut world, ME, UnitType::AntiAir, pos(11, 2), 100);
    // 互いに支援できない距離に置き、別々の脅威として扱わせる。
    let enemy_tank_at = pos(16, 1);
    let enemy_copter_at = pos(25, 1);
    unit(&mut world, ENEMY, UnitType::Tank, enemy_tank_at, 100);
    unit(&mut world, ENEMY, UnitType::Bcopters, enemy_copter_at, 100);

    let responsibilities = allocate(&world, &[tank, anti_air], &[capturer], objective);

    let for_tank = responsibility_of(&responsibilities, tank);
    assert_eq!(for_tank.mission_type, MissionType::Attack);
    assert_eq!(for_tank.target, enemy_tank_at, "戦車は敵戦車へ");
    let for_anti_air = responsibility_of(&responsibilities, anti_air);
    assert_eq!(for_anti_air.mission_type, MissionType::Attack);
    assert_eq!(for_anti_air.target, enemy_copter_at, "対空車はヘリへ");
}

/// 占領完了までに施設へ届かない敵は追わず、部隊は施設の前衛として前進する。
#[test]
fn far_enemy_that_cannot_interfere_is_not_chased() {
    let objective = pos(5, 0);
    let mut world = board(40, 1, objective);
    let capturer = unit(&mut world, ME, UnitType::Infantry, pos(4, 0), 100);
    let tank = unit(&mut world, ME, UnitType::Tank, pos(3, 0), 100);
    unit(&mut world, ENEMY, UnitType::Tank, pos(39, 0), 100);

    let responsibilities = allocate(&world, &[tank], &[capturer], objective);

    let for_tank = responsibility_of(&responsibilities, tank);
    assert_eq!(for_tank.mission_type, MissionType::Defense);
    assert_eq!(for_tank.target, objective, "施設の護衛として前進する");
}

/// 敵のHPを削り切るのに要る数だけ割り当て、残りは施設の前衛にする。
#[test]
fn only_as_many_units_as_needed_are_sent_to_one_enemy() {
    let objective = pos(8, 1);
    let mut world = board(10, 3, objective);
    let capturer = unit(&mut world, ME, UnitType::Infantry, pos(2, 1), 100);
    let tanks =
        [pos(3, 0), pos(3, 1), pos(3, 2)].map(|at| unit(&mut world, ME, UnitType::Tank, at, 100));
    let weak_enemy_at = pos(7, 1);
    unit(&mut world, ENEMY, UnitType::Infantry, weak_enemy_at, 30);

    let responsibilities = allocate(&world, &tanks, &[capturer], objective);

    let attackers = responsibilities
        .iter()
        .filter(|responsibility| responsibility.mission_type == MissionType::Attack)
        .flat_map(|responsibility| responsibility.members.iter())
        .count();
    assert_eq!(attackers, 1, "残りHP30の歩兵に戦車3両を集めない");
    let escorts = responsibilities
        .iter()
        .filter(|responsibility| {
            responsibility.mission_type == MissionType::Defense
                && responsibility.target == objective
        })
        .flat_map(|responsibility| responsibility.members.iter())
        .count();
    assert_eq!(escorts, 2);
}

/// 今の目的施設に急ぎの脅威がなければ、余った戦力は残りの施設（敵工場など）を
/// 守る敵の排除へ向かう。施設を守っていない敵は追わない。
#[test]
fn surplus_engages_enemies_guarding_remaining_properties() {
    let objective = pos(5, 1);
    let mut world = board(60, 3, objective);
    let factory = pos(27, 1);
    world.spawn((factory, Property::new(Terrain::Factory, Some(ENEMY), 200)));
    let capturer = unit(&mut world, ME, UnitType::Infantry, pos(4, 1), 100);
    let tanks = [pos(3, 0), pos(3, 2)].map(|at| unit(&mut world, ME, UnitType::Tank, at, 100));
    let guard_at = pos(26, 1);
    unit(&mut world, ENEMY, UnitType::Tank, guard_at, 100);
    let roaming_at = pos(59, 2);
    unit(&mut world, ENEMY, UnitType::Tank, roaming_at, 100);

    let responsibilities = allocate(&world, &tanks, &[capturer], objective);

    let targets = responsibilities
        .iter()
        .filter(|responsibility| responsibility.mission_type == MissionType::Attack)
        .map(|responsibility| responsibility.target)
        .collect::<Vec<_>>();
    assert!(targets.contains(&guard_at), "敵工場を守る戦車を排除する");
    assert!(
        !targets.contains(&roaming_at),
        "施設を守っていない敵は追わない"
    );
}

/// 互いに支援できる敵の集団には、集団の合計HPに見合う戦力を1つの部隊として送る。
/// 1体分のHPだけを見て小部隊を送り、周囲の敵の反撃で失わないようにする。
#[test]
fn mutually_supporting_enemies_receive_a_concentrated_force() {
    let objective = pos(8, 1);
    let mut world = board(12, 3, objective);
    let capturer = unit(&mut world, ME, UnitType::Infantry, pos(2, 1), 100);
    let tanks = [pos(3, 0), pos(3, 1), pos(3, 2), pos(2, 0), pos(2, 2)]
        .map(|at| unit(&mut world, ME, UnitType::Tank, at, 100));
    let front_at = pos(7, 1);
    for at in [front_at, pos(8, 0), pos(8, 2)] {
        unit(&mut world, ENEMY, UnitType::Tank, at, 100);
    }

    let responsibilities = allocate(&world, &tanks, &[capturer], objective);

    let attack_squads = responsibilities
        .iter()
        .filter(|responsibility| responsibility.mission_type == MissionType::Attack)
        .collect::<Vec<_>>();
    assert_eq!(attack_squads.len(), 1, "支え合う3両を1つの集団として扱う");
    // 3両×HP100に対し戦車1両の1撃は55。1体分のHPなら2両で済むが、集団には全5両を集める。
    assert_eq!(attack_squads[0].members.len(), 5);
}

/// 施設に居座る敵は、占領を始めるために必ず排除対象にする。
#[test]
fn enemy_sitting_on_the_objective_is_always_a_threat() {
    let objective = pos(8, 0);
    let mut world = board(10, 1, objective);
    let tank = unit(&mut world, ME, UnitType::Tank, pos(1, 0), 100);
    unit(&mut world, ENEMY, UnitType::Infantry, objective, 100);

    let responsibilities = allocate(&world, &[tank], &[], objective);

    let for_tank = responsibility_of(&responsibilities, tank);
    assert_eq!(for_tank.mission_type, MissionType::Attack);
    assert_eq!(for_tank.target, objective);
}

/// 実マップを読み込み、`player` の首都がある島を返す。
fn real_map(name: &str, player: PlayerId) -> (World, IslandId) {
    let master_data = MasterDataRegistry::load().unwrap();
    let (world, _schedule) = crate::setup::initialize_world_from_master_data_with_topology(
        &master_data,
        name,
        GridTopology::Square,
    )
    .expect("マップを初期化できる");
    let capital = world
        .iter_entities()
        .find_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.terrain == Terrain::Capital && property.owner_id == Some(player))
                .then_some(*position)
        })
        .expect("自首都");
    let island = world
        .resource::<IslandMap>()
        .get_island_at(&capital)
        .expect("首都の島")
        .id;
    (world, island)
}

/// 進軍路が1本の島では、余った戦闘役を前線一点へ集めず、占領目標への前進に残す。
#[test]
fn single_route_island_keeps_surplus_combat_for_capture_advance() {
    let (world, island) = real_map("map_25", ENEMY);
    let fronts =
        crate::ai::v4::capital_route::topology::same_land_route_fronts(&world, ENEMY, island);
    assert!(fronts.len() < 2, "map_25は進軍路が1本以下: {fronts:?}");
    let map = world.resource::<Map>().clone();
    let members = vec![Entity::from_raw(1), Entity::from_raw(2)];
    let mut remaining = members.clone();

    let responsibilities =
        route_front_responsibilities(&world, &map, ENEMY, island, &mut remaining);

    assert!(responsibilities.is_empty());
    assert_eq!(remaining, members, "配らなかった戦闘役は次の前進責務へ残す");
}

/// 進軍路が2本の島では、余った戦闘役を両方の前線へDefenseとして配り切る。
#[test]
fn two_route_island_sends_surplus_combat_to_each_route_front() {
    let (mut world, island) = real_map("map_6", ENEMY);
    let fronts =
        crate::ai::v4::capital_route::topology::same_land_route_fronts(&world, ENEMY, island);
    assert_eq!(fronts.len(), 2, "map_6は北・南の2本: {fronts:?}");
    // 各前線の近くに戦闘役を置き、距離で両方の前線へ分かれることを確かめる。
    let mut remaining = fronts
        .iter()
        .flat_map(|front| [0, 1].map(|_| world.spawn(*front).id()))
        .collect::<Vec<_>>();
    let map = world.resource::<Map>().clone();

    let responsibilities =
        route_front_responsibilities(&world, &map, ENEMY, island, &mut remaining);

    assert!(remaining.is_empty(), "余った戦闘役を配り切る");
    let targets = responsibilities
        .iter()
        .map(|responsibility| {
            assert_eq!(responsibility.mission_type, MissionType::Defense);
            responsibility.target
        })
        .collect::<HashSet<_>>();
    assert_eq!(targets, fronts.into_iter().collect::<HashSet<_>>());
}

/// 首都ルートDAGがある島では、余った戦闘役を固定の識別点ではなく各進軍路の現在の前線へ配る。
#[test]
fn surplus_combat_follows_current_route_fronts_when_dag_exists() {
    let (mut world, island) = real_map("map_6", ENEMY);
    crate::ai::v4::capital_route::prepare_capital_route_topologies(&mut world, ENEMY);
    let current_fronts =
        crate::ai::v4::capital_route::node_operation::capital_route_current_fronts(
            &world, ENEMY, island,
        )
        .expect("DAGの前線");
    let mut remaining = current_fronts
        .iter()
        .flat_map(|front| [0, 1].map(|_| world.spawn(*front).id()))
        .collect::<Vec<_>>();
    let map = world.resource::<Map>().clone();

    let responsibilities =
        route_front_responsibilities(&world, &map, ENEMY, island, &mut remaining);

    assert!(remaining.is_empty(), "余った戦闘役を配り切る");
    let targets = responsibilities
        .iter()
        .map(|responsibility| responsibility.target)
        .collect::<HashSet<_>>();
    assert!(
        targets.is_subset(&current_fronts.iter().copied().collect()),
        "配属先 {targets:?} は現在の前線 {current_fronts:?} のいずれか"
    );
}
