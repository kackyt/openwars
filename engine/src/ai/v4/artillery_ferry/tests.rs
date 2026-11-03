use super::*;
use crate::ai::v4::maritime::test_support::{load, naval_world, property, spawn};
use crate::components::ActionCompleted;
use crate::resources::Map;
use crate::resources::master_data::{MasterDataRegistry, UnitName};

/// 1列の陸地。左端が敵首都、右端が自軍首都。砲台と重自走砲の撃ち合いだけを入れる。
fn line_world() -> World {
    let mut world = naval_world(&["H..............H"]);
    property(&mut world, 2, 0, 0);
    property(&mut world, 1, 15, 0);
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Artillery, UnitType::HeavySpGun, 88);
    chart.insert_damage(UnitType::HeavySpGun, UnitType::Artillery, 88);
    world.insert_resource(chart);
    world
}

fn decide(world: &mut World) -> Option<(Entity, AiCommand)> {
    decide_artillery_ferry(world, PlayerId(1), &HashSet::default())
}

#[test]
fn truck_drops_the_gun_where_it_outranges_the_enemy_gun() {
    let mut world = line_world();
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 12, 0);
    let gun = load(&mut world, truck, UnitType::Artillery, 1);
    spawn(&mut world, 2, UnitType::HeavySpGun, 2, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, truck);
    match command {
        AiCommand::Drop {
            cargo_drop_pos,
            cargo_entity,
            ..
        } => {
            assert_eq!(cargo_entity, gun);
            // x=6 は重自走砲（射程3〜5）に撃ち返される。射程6の x=8 から一方的に撃つ。
            assert_eq!(cargo_drop_pos, GridPosition { x: 8, y: 0 });
        }
        other => panic!("撃てる位置へ降ろすはず: {other:?}"),
    }
}

#[test]
fn gun_fires_without_moving_when_an_enemy_is_in_range() {
    let mut world = line_world();
    let gun = spawn(&mut world, 1, UnitType::Artillery, 8, 0);
    let enemy = spawn(&mut world, 2, UnitType::HeavySpGun, 2, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, gun);
    assert!(matches!(
        command,
        AiCommand::Attack { target_pos, target_entity }
            if target_pos == GridPosition { x: 8, y: 0 } && target_entity == enemy
    ));
}

#[test]
fn gun_just_out_of_range_boards_an_adjacent_truck_instead_of_waiting() {
    let mut world = line_world();
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 11, 0);
    world.get_mut::<ActionCompleted>(truck).unwrap().0 = true;
    let gun = spawn(&mut world, 1, UnitType::Artillery, 10, 0);
    // 重自走砲は射程(6)のすぐ外。待つより運んで撃てる位置へ据える。
    spawn(&mut world, 2, UnitType::HeavySpGun, 2, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, gun);
    assert!(matches!(
        command,
        AiCommand::Load { transport_entity, .. } if transport_entity == truck
    ));
}

#[test]
fn idle_gun_boards_the_truck_that_came_next_to_it() {
    let mut world = line_world();
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 13, 0);
    world.get_mut::<ActionCompleted>(truck).unwrap().0 = true;
    let gun = spawn(&mut world, 1, UnitType::Artillery, 14, 0);
    // 敵は射程のすぐ外より遠い。
    spawn(&mut world, 2, UnitType::HeavySpGun, 4, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, gun);
    assert!(matches!(
        command,
        AiCommand::Load { target_pos, transport_entity }
            if target_pos == GridPosition { x: 13, y: 0 } && transport_entity == truck
    ));
}

#[test]
fn empty_truck_goes_next_to_an_idle_gun() {
    let mut world = line_world();
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 9, 0);
    spawn(&mut world, 1, UnitType::Artillery, 14, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 1, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, truck);
    let AiCommand::Wait { target_pos } = command else {
        panic!("砲台の隣へ迎えに行くはず: {command:?}");
    };
    assert_eq!(world.resource::<Map>().distance(target_pos.x, 0, 14, 0), 1);
}

#[test]
fn landers_are_not_artillery_carriers() {
    let registry = MasterDataRegistry::load().unwrap();
    let stats = |unit_type: UnitType| {
        registry
            .create_unit_stats(&UnitName(unit_type.as_str().to_owned()))
            .unwrap()
    };
    assert!(is_artillery_carrier(&stats(UnitType::SupplyTruck)));
    assert!(!is_artillery_carrier(&stats(UnitType::Lander)));
    assert!(is_ferried_gun(&stats(UnitType::Artillery)));
    assert!(!is_ferried_gun(&stats(UnitType::HeavySpGun)));
}

#[test]
fn truck_resupplies_the_adjacent_gun_before_it_fires() {
    let mut world = line_world();
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 9, 0);
    let gun = spawn(&mut world, 1, UnitType::Artillery, 8, 0);
    world.get_mut::<crate::components::Ammo>(gun).unwrap().ammo1 = 1;
    spawn(&mut world, 2, UnitType::HeavySpGun, 2, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, truck);
    assert!(matches!(
        command,
        AiCommand::Supply { target_pos, target_entity }
            if target_pos == GridPosition { x: 9, y: 0 } && target_entity == gun
    ));
}

#[test]
fn truck_next_to_an_engaged_gun_stays_instead_of_fetching_another() {
    let mut world = line_world();
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 9, 0);
    let gun = spawn(&mut world, 1, UnitType::Artillery, 8, 0);
    world.get_mut::<ActionCompleted>(gun).unwrap().0 = true;
    // 後方に運ぶべき砲台が居ても、交戦中の砲台の補給役は動かない。
    spawn(&mut world, 1, UnitType::Artillery, 14, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 2, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, truck);
    assert!(matches!(
        command,
        AiCommand::Wait { target_pos } if target_pos == GridPosition { x: 9, y: 0 }
    ));
}

/// 敵を射程に収める陣地へ運べるなら、手前の自軍都市で降ろさずに前へ運ぶ。
/// 砲台は射程に多くの敵を収めて初めて役に立つので、撃てない都市に据えて待たない。
#[test]
fn truck_carries_the_gun_past_an_idle_city_toward_a_covering_site() {
    let mut world = line_world();
    // x=9 が自軍の都市。そこからは敵の重自走砲(x=1)を射程に捉えられない。
    world
        .resource_mut::<Map>()
        .set_terrain(9, 0, Terrain::City)
        .unwrap();
    property(&mut world, 1, 9, 0);
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 14, 0);
    load(&mut world, truck, UnitType::Artillery, 1);
    spawn(&mut world, 2, UnitType::HeavySpGun, 1, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, truck);
    match command {
        AiCommand::Wait { target_pos } => assert!(target_pos.x < 14, "前へ運ぶ: {target_pos:?}"),
        other => panic!("撃てない都市で降ろさず前へ運ぶはず: {other:?}"),
    }
}

/// 射程に収める敵が多い陣地を、手前の少ない陣地より優先する。
#[test]
fn truck_drops_where_more_enemies_are_in_range() {
    let mut world = naval_world(&["H..............H"]);
    property(&mut world, 2, 0, 0);
    property(&mut world, 1, 15, 0);
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Artillery, UnitType::Infantry, 95);
    world.insert_resource(chart);
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 13, 0);
    let gun = load(&mut world, truck, UnitType::Artillery, 1);
    // x=1 と x=4 に敵歩兵。輸送車に近い x=8〜9 からは x=4 の1体しか射程（3〜6）に入らない。
    spawn(&mut world, 2, UnitType::Infantry, 1, 0);
    spawn(&mut world, 2, UnitType::Infantry, 4, 0);
    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, truck);
    let AiCommand::Drop {
        cargo_drop_pos,
        cargo_entity,
        ..
    } = command
    else {
        panic!("降ろすはず: {command:?}");
    };
    assert_eq!(cargo_entity, gun);
    // 砲台の射程は3〜6。x=7 なら x=1(距離6) と x=4(距離3) の両方を収める。
    let map = world.resource::<Map>();
    let covered = [1, 4]
        .iter()
        .filter(|x| (3..=6).contains(&map.distance(cargo_drop_pos.x, 0, **x, 0)))
        .count();
    assert_eq!(
        covered, 2,
        "2体とも射程に収める位置へ降ろす: {cargo_drop_pos:?}"
    );
}

#[test]
fn gun_waiting_for_a_ride_stays_put_to_keep_its_fuel() {
    let mut world = line_world();
    let gun = spawn(&mut world, 1, UnitType::Artillery, 14, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 1, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, gun);
    assert!(matches!(
        command,
        AiCommand::Wait { target_pos } if target_pos == GridPosition { x: 14, y: 0 }
    ));
}

#[test]
fn truck_refuels_a_stranded_gun_before_it_boards() {
    let mut world = line_world();
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 13, 0);
    let gun = spawn(&mut world, 1, UnitType::Artillery, 14, 0);
    world
        .get_mut::<crate::components::Fuel>(gun)
        .unwrap()
        .current = 0;
    spawn(&mut world, 2, UnitType::HeavySpGun, 1, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, truck);
    assert!(matches!(
        command,
        AiCommand::Supply { target_entity, .. } if target_entity == gun
    ));
}

#[test]
fn truck_does_not_drop_the_gun_where_it_could_never_board_again() {
    let mut world = line_world();
    // x=9 の自軍の都市は両隣が森。輸送車は森へ入れるが砲台は入れないので、ここへ降ろすと
    // 二度と輸送車へ乗れない。
    world
        .resource_mut::<Map>()
        .set_terrain(9, 0, Terrain::City)
        .unwrap();
    for x in [8, 10] {
        world
            .resource_mut::<Map>()
            .set_terrain(x, 0, Terrain::Forest)
            .unwrap();
    }
    property(&mut world, 1, 9, 0);
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 14, 0);
    let gun = load(&mut world, truck, UnitType::Artillery, 1);
    spawn(&mut world, 2, UnitType::HeavySpGun, 1, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, truck);
    assert!(
        !matches!(
            command,
            AiCommand::Drop { cargo_drop_pos, cargo_entity, .. }
                if cargo_drop_pos == GridPosition { x: 9, y: 0 } && cargo_entity == gun
        ),
        "乗り直せない都市へ降ろしてはいけない: {command:?}"
    );
}

#[test]
fn truck_tending_an_engaged_gun_from_a_forest_moves_where_the_gun_can_board() {
    let mut world = line_world();
    // 砲台(x=8)は重自走砲(x=2)を撃ち終えた。付き添いの輸送車は森(x=7)に居て、砲台はそこへ
    // 乗り込めない。撃つ敵が居なくなったらすぐ積めるよう、平地(x=9)へ移る。
    world
        .resource_mut::<Map>()
        .set_terrain(7, 0, Terrain::Forest)
        .unwrap();
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 7, 0);
    let gun = spawn(&mut world, 1, UnitType::Artillery, 8, 0);
    world.get_mut::<ActionCompleted>(gun).unwrap().0 = true;
    spawn(&mut world, 2, UnitType::HeavySpGun, 2, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, truck);
    assert!(
        matches!(
            command,
            AiCommand::Wait { target_pos } if target_pos == GridPosition { x: 9, y: 0 }
        ),
        "砲台の乗り込めるマスへ移るはず: {command:?}"
    );
}

#[test]
fn gun_with_a_target_keeps_firing_instead_of_moving_to_a_better_site() {
    let mut world = line_world();
    world
        .resource_mut::<DamageChart>()
        .insert_damage(UnitType::Artillery, UnitType::Infantry, 95);
    // 砲台(x=12)は射程の端の歩兵(x=6)しか撃てず、中戦車(x=7)の後ろ(x=8)からなら重自走砲(x=2)を
    // 撃てる。それでも撃てる敵が居る間は動かさない（着く頃には敵が動いていて、また移すことになる）。
    let gun = spawn(&mut world, 1, UnitType::Artillery, 12, 0);
    spawn(&mut world, 1, UnitType::SupplyTruck, 13, 0);
    spawn(&mut world, 1, UnitType::MdTank, 7, 0);
    let infantry = spawn(&mut world, 2, UnitType::Infantry, 6, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 2, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, gun);
    assert!(
        matches!(
            command,
            AiCommand::Attack { target_entity, .. } if target_entity == infantry
        ),
        "その場で撃ち続けるはず: {command:?}"
    );
}

#[test]
fn carried_gun_heads_for_the_site_on_its_own_front_rather_than_a_distant_city() {
    let mut world = naval_world(&["H.............................H"]);
    property(&mut world, 2, 0, 0);
    property(&mut world, 1, 30, 0);
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Artillery, UnitType::HeavySpGun, 88);
    world.insert_resource(chart);
    // 輸送車(x=20)の届く範囲からは、どこへ降ろしても敵を射程に捉えられない。中戦車(x=5)の
    // 後ろから重自走砲(x=1)を撃てる陣地は左にある。右の自軍の都市(x=27)は敵の歩兵(x=29)に
    // 最も近い「前線の都市」だが、そこへは向かわず、撃てる陣地（左）へ向かう。
    world
        .resource_mut::<Map>()
        .set_terrain(27, 0, Terrain::City)
        .unwrap();
    property(&mut world, 1, 27, 0);
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 20, 0);
    load(&mut world, truck, UnitType::Artillery, 1);
    spawn(&mut world, 1, UnitType::MdTank, 5, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 1, 0);
    spawn(&mut world, 2, UnitType::Infantry, 29, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, truck);
    match command {
        AiCommand::Wait { target_pos } => assert!(
            target_pos.x < 20,
            "今の戦線の陣地（左）へ向かうはず: {target_pos:?}"
        ),
        other => panic!("陣地へ向かうはず: {other:?}"),
    }
}

#[test]
fn second_truck_next_to_an_engaged_gun_fetches_an_idle_gun() {
    let mut world = line_world();
    // 交戦中の砲台(x=8)の両隣に輸送車が2両(x=7, x=9)。補給は1両で足りるので、もう1両は
    // 後方の撃てない砲台(x=14)を迎えに行く。
    let gun = spawn(&mut world, 1, UnitType::Artillery, 8, 0);
    world.get_mut::<ActionCompleted>(gun).unwrap().0 = true;
    spawn(&mut world, 1, UnitType::SupplyTruck, 7, 0);
    let second = spawn(&mut world, 1, UnitType::SupplyTruck, 9, 0);
    spawn(&mut world, 1, UnitType::Artillery, 14, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 2, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, second);
    assert!(
        matches!(
            command,
            AiCommand::Wait { target_pos } if target_pos == GridPosition { x: 13, y: 0 }
        ),
        "後方の砲台の隣へ迎えに行くはず: {command:?}"
    );
}

/// 攻めの作戦（要衝 `attack`）と守りの作戦（要衝 `defend`）を持つ配分案を置く。
fn set_route_posture(world: &mut World, attack: GridPosition, defend: GridPosition) {
    use crate::ai::v4::capital_route::force_demand::{
        RouteOrder, RoutePostureDecision, RoutePostureMemory,
    };
    let order = |key_point: GridPosition, defend: bool| RouteOrder {
        key_point,
        defend,
        staging: key_point,
        force: 0,
    };
    let mut memory = RoutePostureMemory::default();
    memory.by_player.insert(
        PlayerId(1),
        RoutePostureDecision {
            main: Some(attack),
            regions: Vec::new(),
            orders: vec![order(attack, false), order(defend, true)],
        },
    );
    world.insert_resource(memory);
}

/// 前の手番の割り当ての記録を置く。`seen` は前の手番までに居た砲台（それ以外は新しい砲台）。
fn set_previous_guns(world: &mut World, seen: &[Entity]) {
    let mut memory = GunRouteAssignment::default();
    memory.by_player.insert(
        PlayerId(1),
        GunRoutes {
            regions: HashMap::default(),
            turn: u32::MAX,
            routes: HashMap::default(),
            seen: seen.iter().copied().collect(),
        },
    );
    world.insert_resource(memory);
}

#[test]
fn gun_away_from_its_attack_route_fires_at_a_target_in_range() {
    let mut world = naval_world(&["H.............................H"]);
    property(&mut world, 2, 0, 0);
    property(&mut world, 1, 30, 0);
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Artillery, UnitType::HeavySpGun, 88);
    world.insert_resource(chart);
    // 攻めの進軍路の要衝は x=3、守りの進軍路の要衝は x=29。新しく作った砲台(x=16)は
    // 攻めの進軍路へ割り当てられていても、目の前の重自走砲(x=21)を先に撃つ。
    set_route_posture(
        &mut world,
        GridPosition { x: 3, y: 0 },
        GridPosition { x: 29, y: 0 },
    );
    set_previous_guns(&mut world, &[]);
    let gun = spawn(&mut world, 1, UnitType::Artillery, 16, 0);
    let _truck = spawn(&mut world, 1, UnitType::SupplyTruck, 15, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 21, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 0, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, gun);
    assert!(
        matches!(command, AiCommand::Attack { .. }),
        "射程内の敵へ先に射撃する: {command:?}"
    );
}

#[test]
fn existing_gun_is_not_pulled_to_the_attack_route() {
    let mut world = naval_world(&["H.............................H"]);
    property(&mut world, 2, 0, 0);
    property(&mut world, 1, 30, 0);
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Artillery, UnitType::HeavySpGun, 88);
    world.insert_resource(chart);
    // 前の手番から居た砲台(x=16)は、攻めの砲台が足りなくても割り当てず（右辺の守りなどを
    // 引き抜かない）、守りの要衝(x=18)の近くで目の前の重自走砲(x=21)を撃つ。
    set_route_posture(
        &mut world,
        GridPosition { x: 3, y: 0 },
        GridPosition { x: 18, y: 0 },
    );
    let gun = spawn(&mut world, 1, UnitType::Artillery, 16, 0);
    spawn(&mut world, 1, UnitType::SupplyTruck, 15, 0);
    let enemy = spawn(&mut world, 2, UnitType::HeavySpGun, 21, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 0, 0);
    set_previous_guns(&mut world, &[gun]);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, gun);
    assert!(
        matches!(
            command,
            AiCommand::Attack { target_entity, .. } if target_entity == enemy
        ),
        "前から居た砲台はその場で撃つはず: {command:?}"
    );
}

#[test]
fn new_guns_join_the_attack_route_without_a_fixed_quota() {
    let mut world = naval_world(&["H.............................H"]);
    property(&mut world, 2, 0, 0);
    property(&mut world, 1, 30, 0);
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Artillery, UnitType::HeavySpGun, 88);
    world.insert_resource(chart);
    set_route_posture(
        &mut world,
        GridPosition { x: 3, y: 0 },
        GridPosition { x: 27, y: 0 },
    );
    // 前の手番から居た2基は割り当てず、新しく作った4基は、固定の上限なしに攻めの要衝へ付ける。
    let old = [22, 24].map(|x| spawn(&mut world, 1, UnitType::Artillery, x, 0));
    let new = [8, 10, 12, 14].map(|x| spawn(&mut world, 1, UnitType::Artillery, x, 0));
    spawn(&mut world, 2, UnitType::HeavySpGun, 0, 0);
    set_previous_guns(&mut world, &old);
    decide(&mut world);
    let routes = world
        .resource::<GunRouteAssignment>()
        .by_player
        .get(&PlayerId(1))
        .unwrap()
        .routes
        .clone();
    for gun in &new {
        assert_eq!(routes.get(gun), Some(&GridPosition { x: 3, y: 0 }));
    }
    for gun in &old {
        assert_eq!(routes.get(gun), None);
    }
}

#[test]
fn new_gun_assigned_to_defend_route_by_squad_manager_is_not_hijacked_to_attack() {
    let mut world = naval_world(&["H.............................H"]);
    property(&mut world, 2, 0, 0);
    property(&mut world, 1, 30, 0);
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Artillery, UnitType::HeavySpGun, 88);
    world.insert_resource(chart);
    let attack = GridPosition { x: 3, y: 0 };
    let defend = GridPosition { x: 24, y: 0 };
    set_route_posture(&mut world, attack, defend);
    set_previous_guns(&mut world, &[]);
    let defend_gun = spawn(&mut world, 1, UnitType::Artillery, 25, 0);
    let attack_gun = spawn(&mut world, 1, UnitType::Artillery, 15, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 0, 0);
    let mut manager = crate::ai::squad::SquadManager::default();
    manager.route_operation_members.insert(defend_gun, defend);
    manager.route_operation_members.insert(attack_gun, attack);
    world.insert_resource(manager);

    decide(&mut world);

    let routes = world
        .resource::<GunRouteAssignment>()
        .by_player
        .get(&PlayerId(1))
        .unwrap()
        .routes
        .clone();
    assert_eq!(routes.get(&attack_gun), Some(&attack));
    assert_eq!(routes.get(&defend_gun), None);
}

#[test]
fn gun_assigned_to_a_route_that_turned_defensive_keeps_its_post() {
    let mut world = line_world();
    // 前の手番に要衝 x=12 の攻めの作戦へ割り当てた砲台。今の配分案では x=12 付近は守りの作戦に
    // なり、攻めの作戦は反対側(x=2)。配分案の揺れで戦線の間を運び直さないよう、割り当ては
    // 外さず、前の要衝(x=12)のまま受け持ちを続ける。
    set_route_posture(
        &mut world,
        GridPosition { x: 2, y: 0 },
        GridPosition { x: 13, y: 0 },
    );
    let gun = spawn(&mut world, 1, UnitType::Artillery, 14, 0);
    let mut memory = GunRouteAssignment::default();
    memory.by_player.insert(
        PlayerId(1),
        GunRoutes {
            regions: HashMap::default(),
            turn: u32::MAX,
            routes: [(gun, GridPosition { x: 12, y: 0 })].into_iter().collect(),
            seen: [gun].into_iter().collect(),
        },
    );
    world.insert_resource(memory);
    decide(&mut world);
    let routes = &world
        .resource::<GunRouteAssignment>()
        .by_player
        .get(&PlayerId(1))
        .unwrap()
        .routes;
    assert_eq!(routes.get(&gun), Some(&GridPosition { x: 12, y: 0 }));
}

#[test]
fn defending_gun_beyond_range_of_its_post_fires_before_returning() {
    let mut world = naval_world(&["H.............................H"]);
    property(&mut world, 2, 0, 0);
    property(&mut world, 1, 30, 0);
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Artillery, UnitType::HeavySpGun, 88);
    world.insert_resource(chart);
    // 守りの要衝(x=25)から離れた砲台も、射程内の敵(x=12)を先に撃つ。
    set_route_posture(
        &mut world,
        GridPosition { x: 3, y: 0 },
        GridPosition { x: 25, y: 0 },
    );
    let gun = spawn(&mut world, 1, UnitType::Artillery, 17, 0);
    let _truck = spawn(&mut world, 1, UnitType::SupplyTruck, 18, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 12, 0);
    set_previous_guns(&mut world, &[gun]);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, gun);
    assert!(
        matches!(command, AiCommand::Attack { .. }),
        "守備位置へ戻る前に射撃する: {command:?}"
    );
}

#[test]
fn truck_does_not_drop_the_gun_on_an_own_factory() {
    let mut world = line_world();
    // x=8 は自軍の工場。重自走砲(x=2)を射程の端から撃てる位置だが、工場の上に据わると
    // その工場で生産できなくなるので降ろさない。
    world
        .resource_mut::<Map>()
        .set_terrain(8, 0, Terrain::Factory)
        .unwrap();
    property(&mut world, 1, 8, 0);
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 12, 0);
    load(&mut world, truck, UnitType::Artillery, 1);
    spawn(&mut world, 2, UnitType::HeavySpGun, 2, 0);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, truck);
    assert!(
        !matches!(
            command,
            AiCommand::Drop { cargo_drop_pos, .. } if cargo_drop_pos == GridPosition { x: 8, y: 0 }
        ),
        "工場の上へ降ろしてはいけない: {command:?}"
    );
}

#[test]
fn defending_gun_near_its_post_waits_without_a_target() {
    let mut world = naval_world(&["H.............................H"]);
    property(&mut world, 2, 0, 0);
    property(&mut world, 1, 30, 0);
    let mut chart = DamageChart::new();
    chart.insert_damage(UnitType::Artillery, UnitType::HeavySpGun, 88);
    world.insert_resource(chart);
    // 守りの要衝(x=25)の近く(x=24)に据わった守りの砲台は、撃てる敵が居なくても隣の輸送車へ
    // 乗らずに待つ（積んで同じ場所へ降ろし直すだけになる）。
    set_route_posture(
        &mut world,
        GridPosition { x: 3, y: 0 },
        GridPosition { x: 25, y: 0 },
    );
    let gun = spawn(&mut world, 1, UnitType::Artillery, 24, 0);
    spawn(&mut world, 1, UnitType::SupplyTruck, 23, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 10, 0);
    set_previous_guns(&mut world, &[gun]);

    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, gun);
    assert!(
        matches!(
            command,
            AiCommand::Wait { target_pos } if target_pos == GridPosition { x: 24, y: 0 }
        ),
        "その場で待つはず: {command:?}"
    );
}

#[test]
fn ferry_defers_boarding_until_after_normal_actions() {
    let mut world = line_world();
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 11, 0);
    world.get_mut::<ActionCompleted>(truck).unwrap().0 = true;
    let gun = spawn(&mut world, 1, UnitType::Artillery, 10, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 2, 0);
    let skip = HashSet::default();
    assert!(decide_artillery_ferry_phase(&mut world, PlayerId(1), &skip, false).is_none());
    assert!(
        matches!(decide_artillery_ferry_phase(&mut world, PlayerId(1), &skip, true),
        Some((entity, AiCommand::Load { transport_entity, .. })) if entity == gun && transport_entity == truck)
    );
}

#[test]
fn unassigned_deployed_gun_without_a_destination_does_not_board() {
    let mut world = naval_world(&["........."]);
    let gun = spawn(&mut world, 1, UnitType::Artillery, 4, 0);
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 5, 0);
    world.get_mut::<ActionCompleted>(truck).unwrap().0 = true;
    let action = decide(&mut world);
    assert!(
        !matches!(action, Some((entity, AiCommand::Load { .. })) if entity == gun),
        "目標も展開先もない砲台を積み直さない: {action:?}"
    );
}

/// 砲台も、小隊へ確定したDAG地域を運搬先の正本として使う。
#[test]
fn forward_gun_transport_uses_committed_region_including_cached_and_loaded_steps() {
    use crate::ai::v4::capital_route::model::{
        CapitalRouteCommitment, CapitalRouteExecutionPhase, CapitalRouteId,
        CapitalRouteNodeObjective, CapitalRouteNodeScope, CapitalRoutePathRegistry,
    };
    let mut world = line_world();
    let player = PlayerId(1);
    let gun = spawn(&mut world, 1, UnitType::Artillery, 12, 0);
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 13, 0);
    spawn(&mut world, 2, UnitType::HeavySpGun, 0, 0);
    let squad = crate::ai::v4::test_support::assign_route_squad(
        &mut world,
        player,
        crate::ai::squad::MissionType::Attack,
        &[gun],
    );
    let goal = GridPosition { x: 1, y: 0 };
    world.insert_resource(CapitalRoutePathRegistry {
        commitments: HashMap::from_iter([(
            squad,
            CapitalRouteCommitment {
                player_id: player,
                island_id: crate::ai::islands::IslandId(0),
                route: CapitalRouteId(0),
                target: goal,
                target_anchor: goal,
                objective: CapitalRouteNodeObjective::Control,
                scope: CapitalRouteNodeScope::Area,
                control_area: vec![goal],
                capture_targets: Vec::new(),
                crossing_exit: None,
                crossed: false,
                execution_target: goal,
                frontline_capacity: 1,
                path: vec![GridPosition { x: 12, y: 0 }, goal],
                phase: CapitalRouteExecutionPhase::Advance,
            },
        )]),
        ..Default::default()
    });
    set_previous_guns(&mut world, &[gun]);
    let previous = world
        .resource_mut::<GunRouteAssignment>()
        .into_inner()
        .by_player
        .get_mut(&player)
        .unwrap();
    previous.turn = 1;
    previous.routes.insert(gun, GridPosition { x: 12, y: 0 });
    assert_eq!(
        update_gun_routes(&mut world, player, 1)[&gun],
        goal,
        "キャッシュした後方要衝で地域指示を上書きしない"
    );
    assert_eq!(assigned_attack_route(&world, player, gun), Some(goal));
    let decision = decide(&mut world);
    assert!(
        matches!(decision,Some((actor,AiCommand::Load{transport_entity,..})) if actor==gun && transport_entity==truck),
        "前進先の敵を撃つために実際に搭載する: {decision:?}"
    );
    let next = GridPosition { x: 2, y: 0 };
    world
        .resource_mut::<CapitalRoutePathRegistry>()
        .commitments
        .get_mut(&squad)
        .unwrap()
        .execution_target = next;
    assert_eq!(
        update_gun_routes(&mut world, player, 1)[&gun],
        next,
        "同手番の新しい確定指示も配送へ渡す"
    );
    world.entity_mut(gun).insert(Transporting(truck));
    world
        .resource_mut::<CapitalRoutePathRegistry>()
        .commitments
        .clear();
    set_route_posture(
        &mut world,
        GridPosition { x: 12, y: 0 },
        GridPosition { x: 14, y: 0 },
    );
    assert_eq!(
        update_gun_routes(&mut world, player, 2)[&gun],
        next,
        "輸送中に地上DAGから外れても配送先を巻き戻さない"
    );
    assert_eq!(
        world.resource::<GunRouteAssignment>().by_player[&player].regions[&gun],
        vec![goal],
        "搭載中の据え付け先・射撃対象も同じ制圧地域を使う"
    );
}

/// 専用射撃が通常戦術を横取りしても、地域の障害より別地域の価格を優先しない。
#[test]
fn ferry_fire_and_transport_keep_the_committed_regional_objective() {
    use crate::ai::v4::capital_route::model::{
        CapitalRouteCommitment, CapitalRouteExecutionPhase, CapitalRouteId,
        CapitalRouteNodeObjective, CapitalRouteNodeScope, CapitalRoutePathRegistry,
    };
    let mut world = line_world();
    let player = PlayerId(1);
    let gun = spawn(&mut world, 1, UnitType::Artillery, 8, 0);
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 9, 0);
    world.get_mut::<ActionCompleted>(truck).unwrap().0 = true;
    let blocker = spawn(&mut world, 2, UnitType::Infantry, 12, 0);
    let unrelated = spawn(&mut world, 2, UnitType::HeavySpGun, 2, 0);
    world
        .resource_mut::<DamageChart>()
        .insert_damage(UnitType::Artillery, UnitType::Infantry, 95);
    let squad = crate::ai::v4::test_support::assign_route_squad(
        &mut world,
        player,
        crate::ai::squad::MissionType::Attack,
        &[gun],
    );
    let goal = GridPosition { x: 12, y: 0 };
    world.insert_resource(CapitalRoutePathRegistry {
        commitments: HashMap::from_iter([(
            squad,
            CapitalRouteCommitment {
                player_id: player,
                island_id: crate::ai::islands::IslandId(0),
                route: CapitalRouteId(0),
                target: goal,
                target_anchor: goal,
                objective: CapitalRouteNodeObjective::Control,
                scope: CapitalRouteNodeScope::Area,
                control_area: vec![goal],
                capture_targets: Vec::new(),
                crossing_exit: None,
                crossed: false,
                execution_target: goal,
                frontline_capacity: 1,
                path: vec![GridPosition { x: 8, y: 0 }, goal],
                phase: CapitalRouteExecutionPhase::Advance,
            },
        )]),
        ..Default::default()
    });
    let decision = decide(&mut world);
    assert!(
        matches!(decision, Some((actor, AiCommand::Attack { target_entity, .. })) if actor==gun && target_entity==blocker),
        "専用の射撃も価格の高い別地域の敵より制圧対象を撃つ: {decision:?}"
    );
    // 同手番の地域変更も射撃・配送へ渡す。射程外なら無関係な敵を撃って搭載不能にしない。
    let next = GridPosition { x: 15, y: 0 };
    world.entity_mut(blocker).insert(next);
    let mut paths = world.resource_mut::<CapitalRoutePathRegistry>();
    let order = paths.commitments.get_mut(&squad).unwrap();
    order.control_area = vec![next];
    order.target_anchor = next;
    order.execution_target = next;
    world.despawn(truck);
    let decision = decide(&mut world);
    assert!(
        matches!(decision, Some((actor, AiCommand::Attack { target_entity, .. })) if actor==gun && target_entity==unrelated),
        "まだ搭載できない手番は地域外の射撃も継続する: {decision:?}"
    );
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 9, 0);
    world.get_mut::<ActionCompleted>(truck).unwrap().0 = true;
    let decision = decide(&mut world);
    assert!(
        matches!(decision, Some((actor, AiCommand::Load { transport_entity, .. })) if actor==gun && transport_entity==truck),
        "地域内の敵へ届くため、周辺射撃で輸送を止めず実際に搭載する: {decision:?}"
    );
    world.despawn(blocker);
    let decision = decide(&mut world);
    assert!(
        matches!(decision, Some((actor, AiCommand::Attack { target_entity, .. })) if actor==gun && target_entity==unrelated),
        "地域を掃討した後は周囲の有利な射撃も残す: {decision:?}"
    );
}

fn injured_tank(world: &mut World, x: usize, hp: u32) -> Entity {
    let tank = spawn(world, 1, UnitType::Tank, x, 0);
    world
        .get_mut::<crate::components::Health>(tank)
        .unwrap()
        .current = hp;
    tank
}

fn board_with_cities(world: &mut World, cities: &[usize]) {
    for x in cities {
        world
            .resource_mut::<Map>()
            .set_terrain(*x, 0, Terrain::City)
            .unwrap();
        property(world, 1, *x, 0);
    }
}

/// 都市が1つだけなら、傷んだ直接部隊がいても砲台のために空けない。
#[test]
fn a_single_city_is_not_reserved_for_repair() {
    let mut world = line_world();
    board_with_cities(&mut world, &[9]);
    injured_tank(&mut world, 10, 40);
    let board = PlayerBoard::observe(&mut world, PlayerId(1));
    assert!(repair_cities(&board).is_empty());
}

/// 都市が複数あるときは、傷んだ直接部隊に近い都市を回復場所として空け、砲台の分は残す。
#[test]
fn damaged_direct_units_keep_nearby_cities_open_but_one_city_is_left_for_guns() {
    let mut world = line_world();
    board_with_cities(&mut world, &[6, 9, 12]);
    injured_tank(&mut world, 10, 40);
    injured_tank(&mut world, 5, 30);
    injured_tank(&mut world, 13, 20);
    let board = PlayerBoard::observe(&mut world, PlayerId(1));
    let reserved = repair_cities(&board);
    assert_eq!(
        reserved.len(),
        2,
        "都市3つのうち、砲台用に1つ残す: {reserved:?}"
    );
    // 損傷の大きい順（x=13のHP20、x=5のHP30）に、近い都市（x=12、x=6）を空ける。
    assert!(reserved.contains(&(12, 0)) && reserved.contains(&(6, 0)));
}

/// 回復の効果が1手番の回復量に満たない損傷では、都市を空けない。
#[test]
fn light_damage_does_not_reserve_a_city() {
    let mut world = line_world();
    board_with_cities(&mut world, &[6, 9]);
    injured_tank(
        &mut world,
        10,
        100 - crate::systems::turn_management::PROPERTY_REPAIR_HP + 1,
    );
    let board = PlayerBoard::observe(&mut world, PlayerId(1));
    assert!(repair_cities(&board).is_empty());
}

/// 回復場所として空けた都市には砲台を降ろさず、別の都市へ降ろす。
#[test]
fn guns_are_not_dropped_on_a_city_kept_open_for_repair() {
    let mut world = line_world();
    board_with_cities(&mut world, &[7, 9]);
    injured_tank(&mut world, 6, 40);
    let truck = spawn(&mut world, 1, UnitType::SupplyTruck, 8, 0);
    let gun = load(&mut world, truck, UnitType::Artillery, 1);
    spawn(&mut world, 2, UnitType::HeavySpGun, 3, 0);
    let (entity, command) = decide(&mut world).unwrap();
    assert_eq!(entity, truck);
    if let AiCommand::Drop {
        cargo_drop_pos,
        cargo_entity,
        ..
    } = command
    {
        assert_eq!(cargo_entity, gun);
        assert_ne!(
            cargo_drop_pos,
            GridPosition { x: 7, y: 0 },
            "傷んだ戦車に近い都市は空ける"
        );
    }
}
