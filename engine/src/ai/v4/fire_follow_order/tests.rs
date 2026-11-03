//! 撃破→進入の実行順序の単体テスト（固定盤面）。

#[path = "entry_tests.rs"]
mod entry_tests;

use super::*;
use crate::ai::squad::MissionType;
use crate::ai::v4::capital_route::model::{
    CapitalRouteCommitment, CapitalRouteId, CapitalRouteNodeObjective, CapitalRouteNodeScope,
};
use crate::ai::v4::test_support::{assign_route_squad, flat_map, pos};
use crate::components::{ActionCompleted, Ammo, Fuel, HasMoved};
use crate::resources::UnitType;

/// 対象セル。DAGの前進区間の実行目標として塞がれている。
const CELL: GridPosition = GridPosition { x: 6, y: 4 };

fn spawn(world: &mut World, player: u32, kind: UnitType, at: GridPosition, hp: u32) -> Entity {
    let stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            kind.as_str().to_owned(),
        ))
        .unwrap();
    world
        .spawn((
            Faction(PlayerId(player)),
            at,
            Health {
                current: hp,
                max: 100,
            },
            Ammo {
                ammo1: stats.max_ammo1,
                max_ammo1: stats.max_ammo1,
                ammo2: stats.max_ammo2,
                max_ammo2: stats.max_ammo2,
            },
            Fuel {
                current: stats.max_fuel,
                max: stats.max_fuel,
            },
            HasMoved(false),
            ActionCompleted(false),
            stats,
        ))
        .id()
}

/// 敵歩兵1体がCELLを塞ぎ、`phase` の前進区間がCELLを実行目標とする盤面。
fn world_with_route(phase: CapitalRouteExecutionPhase) -> World {
    let (mut world, _) = crate::setup::create_world();
    let map = flat_map(13, 9);
    world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
    world.insert_resource(map);
    world.insert_resource(MasterDataRegistry::load().unwrap());
    let mut chart = crate::resources::DamageChart::new();
    chart.insert_damage(UnitType::Infantry, UnitType::Infantry, 7);
    chart.insert_damage(UnitType::Mech, UnitType::Infantry, 12);
    chart.insert_damage(UnitType::HeavySpGun, UnitType::Infantry, 30);
    chart.insert_damage(UnitType::Tank, UnitType::Infantry, 75);
    world.insert_resource(chart);
    let mut settings = crate::ai::PlayerAiSettings::new();
    settings.set_version(PlayerId(1), crate::ai::AiVersion::V4);
    world.insert_resource(settings);
    let squad = assign_route_squad(&mut world, PlayerId(1), MissionType::Attack, &[]);
    world
        .resource_mut::<crate::ai::squad::SquadManager>()
        .squads[0]
        .plan
        .target = Some(CELL);
    world.insert_resource(CapitalRoutePathRegistry {
        commitments: crate::collections::HashMap::from_iter([(
            squad,
            CapitalRouteCommitment {
                player_id: PlayerId(1),
                island_id: crate::ai::islands::IslandId(0),
                route: CapitalRouteId(0),
                target: CELL,
                target_anchor: CELL,
                objective: CapitalRouteNodeObjective::Control,
                scope: CapitalRouteNodeScope::Point,
                control_area: vec![CELL],
                capture_targets: Vec::new(),
                crossing_exit: None,
                crossed: false,
                execution_target: CELL,
                frontline_capacity: 1,
                path: vec![CELL],
                phase,
            },
        )]),
        ..Default::default()
    });
    world
}

/// T20の再現に使う3体の撃破担当（歩兵・重歩兵・重自走砲）。
fn spawn_three_shooters(world: &mut World) -> [Entity; 3] {
    let infantry = spawn(world, 1, UnitType::Infantry, pos(5, 4), 100);
    let mech = spawn(world, 1, UnitType::Mech, pos(7, 4), 100);
    let gun_range = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::HeavySpGun.as_str().to_owned(),
        ))
        .unwrap()
        .min_range
        .max(2) as usize;
    let gun = spawn(world, 1, UnitType::HeavySpGun, pos(6, 4 - gun_range), 100);
    [infantry, mech, gun]
}

/// unitを前進区間のSquad（DAG指令の持ち主）へ所属させる。
fn join_route_squad(world: &mut World, entity: Entity) {
    let squad_id = {
        let mut manager = world.resource_mut::<crate::ai::squad::SquadManager>();
        manager.squads[0].members.insert(entity);
        manager.squads[0].id
    };
    world
        .resource_mut::<crate::ai::operation_assignment::UnitOperationRegistry>()
        .assign(
            entity,
            crate::ai::operation_assignment::UnitOperationAssignment {
                owner: crate::ai::operation_assignment::OperationOwner::TacticalSquad {
                    player_id: PlayerId(1),
                    squad_id,
                },
                squad_id: Some(squad_id),
                role: crate::ai::operation_assignment::OperationUnitRole::Member,
                assigned_turn: 0,
            },
        );
}

fn expected(world: &World, shooter: Entity, enemy: Entity) -> u32 {
    let at = *world.get::<GridPosition>(shooter).unwrap();
    crate::ai::pruning::evaluate_attack_exchange(
        world,
        shooter,
        enemy,
        at,
        world.resource::<crate::resources::DamageChart>(),
    )
    .unwrap()
    .expected_damage
}

/// 射撃を盤面へ反映する（期待ダメージ分だけ削り、撃ち手を行動済みにする）。
fn apply_fire(world: &mut World, shooter: Entity, enemy: Entity, skip: &mut HashSet<Entity>) {
    let damage = expected(world, shooter, enemy);
    world.get_mut::<ActionCompleted>(shooter).unwrap().0 = true;
    skip.insert(shooter);
    let remaining = world
        .get::<Health>(enemy)
        .unwrap()
        .current
        .saturating_sub(damage);
    if remaining == 0 {
        world.despawn(enemy);
    } else {
        world.get_mut::<Health>(enemy).unwrap().current = remaining;
    }
}

fn active_entries(world: &World) -> Vec<Entity> {
    world
        .get_resource::<FireFollowOrderRegistry>()
        .map(|registry| registry.active_entries(PlayerId(1)).collect())
        .unwrap_or_default()
}

/// 1. T20の再現: 3体分の火力が必要な敵に対し、撃破担当3体が先に撃ち、
///    別の進入役は撃破まで予約され、撃破後に同じ手番でセルへ入れる。
#[test]
fn three_shooters_fire_first_and_reserved_entry_enters_after_removal() {
    let mut world = world_with_route(CapitalRouteExecutionPhase::Advance);
    let shooters = spawn_three_shooters(&mut world);
    let entry = spawn(&mut world, 1, UnitType::Tank, pos(6, 6), 100);
    join_route_squad(&mut world, entry);
    let probe_enemy = spawn(&mut world, 2, UnitType::Infantry, CELL, 100);
    let damages = shooters.map(|shooter| expected(&world, shooter, probe_enemy));
    world.despawn(probe_enemy);
    // 3体すべての期待ダメージを合わせて初めて倒せるHPにする。
    let hp = damages.iter().sum::<u32>();
    assert!(hp <= 100 && damages.iter().all(|d| *d > 0), "{damages:?}");
    let enemy = spawn(&mut world, 2, UnitType::Infantry, CELL, hp);
    let mut order_of_fire = shooters.to_vec();
    order_of_fire.sort_by_key(|entity| {
        let index = shooters.iter().position(|s| s == entity).unwrap();
        (std::cmp::Reverse(damages[index]), entity.to_bits())
    });

    let mut skip = HashSet::default();
    for shooter in &order_of_fire {
        let decision = decide_fire_follow_action(&mut world, PlayerId(1), &skip);
        assert!(
            matches!(decision, Some((actor, AiCommand::Attack { target_pos, target_entity }))
                if actor == *shooter && target_entity == enemy
                    && target_pos == *world.get::<GridPosition>(*shooter).unwrap()),
            "撃破担当が期待ダメージ降順に静止射撃する: {decision:?}"
        );
        assert_eq!(
            active_entries(&world),
            vec![entry],
            "撃破まで進入役を予約する"
        );
        apply_fire(&mut world, *shooter, enemy, &mut skip);
    }
    assert!(
        world.get::<GridPosition>(enemy).is_none(),
        "3発で排除される"
    );
    let decision = decide_fire_follow_action(&mut world, PlayerId(1), &skip);
    assert!(
        active_entries(&world).is_empty(),
        "排除後は予約を消費し、進入役自身へ命令を渡す"
    );
    assert!(
        matches!(decision, Some((actor, AiCommand::Wait { target_pos })) if actor == entry && target_pos == CELL),
        "進入役が同じ手番に空いたセルへ入る: {decision:?}"
    );
}

/// 2. 進入役が居ない場合は注文を作らず、従来の最良行動に委ねる。
///    （撃てるのは間接火力だけで、セルへ入れる直接戦闘・占領役が居ない盤面）
#[test]
fn no_order_without_an_entry_unit() {
    let mut world = world_with_route(CapitalRouteExecutionPhase::Advance);
    let [infantry, mech, gun] = spawn_three_shooters(&mut world);
    world.despawn(infantry);
    world.despawn(mech);
    spawn(&mut world, 2, UnitType::Infantry, CELL, 10);
    assert!(decide_fire_follow_action(&mut world, PlayerId(1), &HashSet::default()).is_none());
    assert!(active_entries(&world).is_empty());
    let decision =
        crate::ai::tactics::v2::decide_action(&mut world, PlayerId(1), &HashSet::default());
    assert!(
        decision.as_ref().is_some_and(|(actor, _)| *actor == gun),
        "通常行動は従来どおり選ばれる: {decision:?}"
    );
}

/// 3. 撃破担当が行動済みで火力が足りなければ注文を破棄し、進入役を通常評価へ戻す。
#[test]
fn acted_shooters_drop_the_order_and_release_the_entry() {
    let mut world = world_with_route(CapitalRouteExecutionPhase::Advance);
    let shooters = spawn_three_shooters(&mut world);
    let entry = spawn(&mut world, 1, UnitType::Tank, pos(6, 6), 100);
    spawn(&mut world, 2, UnitType::Infantry, CELL, 100);
    let skip = HashSet::from_iter(shooters);
    for shooter in shooters {
        world.get_mut::<ActionCompleted>(shooter).unwrap().0 = true;
    }
    assert!(decide_fire_follow_action(&mut world, PlayerId(1), &skip).is_none());
    assert!(active_entries(&world).is_empty(), "待機で詰まらない");
    let decision = crate::ai::tactics::v2::decide_action(&mut world, PlayerId(1), &skip);
    assert!(
        decision.as_ref().is_some_and(|(actor, _)| *actor == entry),
        "進入役は通常行動に戻る: {decision:?}"
    );
}

/// 4. 1体で倒せる敵: 撃破担当は1体だけ、進入役は別unitとして1手番分の行動を残す。
#[test]
fn single_shooter_suffices_and_entry_is_a_different_unit() {
    let mut world = world_with_route(CapitalRouteExecutionPhase::Advance);
    let shooters = spawn_three_shooters(&mut world);
    let entry = spawn(&mut world, 1, UnitType::Tank, pos(6, 6), 100);
    let enemy = spawn(&mut world, 2, UnitType::Infantry, CELL, 5);
    let orders = observe(&mut world, PlayerId(1), &HashSet::default());
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].target, enemy);
    assert_eq!(orders[0].fire_tasks.len(), 1, "{:?}", orders[0]);
    assert!(shooters.contains(&orders[0].fire_tasks[0].0));
    // 撃破担当でない最寄りの進入可能unit（隣接する歩兵・重歩兵の残りも候補）。
    assert_ne!(orders[0].entry, orders[0].fire_tasks[0].0);
    assert!(orders[0].entry == entry || shooters.contains(&orders[0].entry));
}

/// 5. DAGの前進区間の外（地域外）の敵は注文対象にしない。
#[test]
fn enemies_outside_advance_route_cells_get_no_order() {
    let mut world = world_with_route(CapitalRouteExecutionPhase::Advance);
    spawn_three_shooters(&mut world);
    spawn(&mut world, 1, UnitType::Tank, pos(6, 7), 100);
    // CELLから外れた位置の敵（射程内でも対象外）。
    spawn(&mut world, 2, UnitType::Infantry, pos(5, 5), 5);
    assert!(observe(&mut world, PlayerId(1), &HashSet::default()).is_empty());

    // 前進区間でない（Stage）なら同じセルの敵も対象外。
    let mut staged = world_with_route(CapitalRouteExecutionPhase::Stage);
    spawn_three_shooters(&mut staged);
    spawn(&mut staged, 1, UnitType::Tank, pos(6, 6), 100);
    spawn(&mut staged, 2, UnitType::Infantry, CELL, 5);
    assert!(observe(&mut staged, PlayerId(1), &HashSet::default()).is_empty());
}

/// 通常行動の除外集合（輸送Squad・砲台フェリー等）にいるunitは撃破担当・進入役にしない。
#[test]
fn skipped_units_are_neither_shooters_nor_entries() {
    let mut world = world_with_route(CapitalRouteExecutionPhase::Advance);
    let shooters = spawn_three_shooters(&mut world);
    let entry = spawn(&mut world, 1, UnitType::Tank, pos(6, 6), 100);
    spawn(&mut world, 2, UnitType::Infantry, CELL, 5);
    let skip = HashSet::from_iter([entry]);
    let orders = observe(&mut world, PlayerId(1), &skip);
    assert!(orders.iter().all(|order| order.entry != entry));
    let skip = HashSet::from_iter(shooters);
    assert!(observe(&mut world, PlayerId(1), &skip).is_empty());
}
