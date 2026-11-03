//! 攻守判断の局面再現。期待判断は配分の重点ではなく、各方面の既存戦力で定義する。
//!
//! 戦況更新と到達不能な増援の失敗を、通常の回帰検査として維持する。
//! 実行: cargo test -p engine --lib posture_scenario_matrix -- --nocapture

use super::*;

fn point(x: usize, y: usize) -> GridPosition {
    GridPosition { x, y }
}

fn objective(anchor: GridPosition) -> CampaignPlanningObjective {
    CampaignPlanningObjective {
        island_id: crate::ai::islands::IslandId(0),
        kind: OperationKind::Capture,
        anchor,
        objective_properties: vec![anchor],
        capture_eta: None,
        required_capture_survivors: 1,
        logistics_rank: None,
        forced_target_enemies: HashSet::default(),
        protected_capture_entities: HashSet::default(),
        capture_replacement_pending: false,
        staging_anchor: anchor,
        execution_authorized: true,
    }
}

/// 実マスターの同兵種で比較し、兵種相性を変えずに部隊数・到着済み増援だけを変える。
fn units(at: GridPosition, count: usize, stats: &UnitStats) -> Vec<UnitSnapshot> {
    (0..count)
        .map(|_| UnitSnapshot {
            entity: None,
            pos: at,
            stats: stats.clone(),
            hp: 100,
            free_cargo: 0,
        })
        .collect()
}

struct Scenario {
    name: &'static str,
    own: [usize; 2],
    enemy: [usize; 2],
    remembered: Option<[bool; 2]>,
    /// 守備地点に残っている旧命令と、現在の前進目標が異なる局面。
    next_target: bool,
    expected_defend: [bool; 2],
}

#[test]
fn posture_scenario_matrix() {
    let map = Map::new(
        24,
        14,
        Terrain::Plains,
        crate::resources::GridTopology::Square,
    );
    let master = crate::resources::master_data::MasterDataRegistry::load().unwrap();
    let stats = master
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Tank.as_str().to_owned(),
        ))
        .unwrap();
    let anchors = [point(3, 3), point(20, 10)];
    let guards = [point(2, 3), point(21, 10)];
    let cases = [
        Scenario {
            name: "fresh_both_overwhelming",
            own: [8, 8],
            enemy: [1, 1],
            remembered: None,
            next_target: false,
            expected_defend: [false, false],
        },
        Scenario {
            name: "fresh_north_can_attack_south_outnumbered",
            own: [8, 1],
            enemy: [1, 8],
            remembered: None,
            next_target: false,
            expected_defend: [false, true],
        },
        Scenario {
            name: "fresh_south_can_attack_north_outnumbered",
            own: [1, 8],
            enemy: [8, 1],
            remembered: None,
            next_target: false,
            expected_defend: [true, false],
        },
        Scenario {
            name: "remembered_defense_still_outnumbered",
            own: [8, 1],
            enemy: [1, 8],
            remembered: Some([false, true]),
            next_target: false,
            expected_defend: [false, true],
        },
        Scenario {
            name: "reinforcements_arrived_on_defended_south",
            own: [8, 8],
            enemy: [1, 1],
            remembered: Some([false, true]),
            next_target: false,
            expected_defend: [false, false],
        },
        Scenario {
            name: "enemy_reduced_on_defended_south",
            own: [8, 3],
            enemy: [1, 1],
            remembered: Some([false, true]),
            next_target: false,
            expected_defend: [false, false],
        },
        Scenario {
            name: "enemy_reinforced_on_attacking_north",
            own: [1, 8],
            enemy: [8, 1],
            remembered: Some([false, true]),
            next_target: false,
            expected_defend: [true, false],
        },
        Scenario {
            name: "north_priority_does_not_cancel_south_attack",
            own: [8, 8],
            enemy: [1, 1],
            remembered: Some([false, false]),
            next_target: false,
            expected_defend: [false, false],
        },
        Scenario {
            name: "fresh_after_enemy_reduced",
            own: [8, 3],
            enemy: [1, 1],
            remembered: None,
            next_target: false,
            expected_defend: [false, false],
        },
        Scenario {
            name: "fresh_after_enemy_reinforced",
            own: [1, 8],
            enemy: [8, 1],
            remembered: None,
            next_target: false,
            expected_defend: [true, false],
        },
        Scenario {
            name: "same_route_new_objective_after_capture",
            own: [8, 8],
            enemy: [1, 1],
            remembered: Some([false, true]),
            next_target: true,
            expected_defend: [false, false],
        },
    ];
    let mut failed = Vec::new();
    for case in cases {
        let current_anchors = if case.next_target {
            [point(4, 3), anchors[1]]
        } else {
            anchors
        };
        let friendly = units(current_anchors[0], case.own[0], &stats)
            .into_iter()
            .chain(units(current_anchors[1], case.own[1], &stats))
            .collect::<Vec<_>>();
        let enemies = units(current_anchors[0], case.enemy[0], &stats)
            .into_iter()
            .chain(units(current_anchors[1], case.enemy[1], &stats))
            .collect::<Vec<_>>();
        let remembered = case.remembered.map(|defend| RoutePostureDecision {
            main: Some(anchors[0]),
            regions: vec![],
            orders: (0..2)
                .map(|index| RouteOrder {
                    key_point: if defend[index] {
                        guards[index]
                    } else {
                        anchors[index]
                    },
                    staging: guards[index],
                    defend: defend[index],
                    force: stats.cost,
                })
                .collect(),
        });
        let plan = concentrate_route_efforts(
            &map,
            &master,
            current_anchors.into_iter().map(objective).collect(),
            &friendly,
            &enemies,
            &guards,
            &[],
            (None, None),
            PostureBudgets { own: 0, enemy: 0 },
            PostureValues {
                property: 10_000,
                capital: 200_000,
                own_capital: 200_000,
                capturer: 1_000,
            },
            remembered.as_ref(),
            &mut |a, b| {
                Some(
                    map.distance(a.x, a.y, b.x, b.y)
                        .div_ceil(stats.max_movement.max(1)),
                )
            },
        );
        let actual = plan
            .orders
            .iter()
            .map(|order| order.defend)
            .collect::<Vec<_>>();
        let passed = actual == case.expected_defend;
        println!(
            "{} {} own={:?} enemy={:?} memory={:?} expected_defend={:?} actual_defend={:?} main={:?}",
            if passed { "PASS" } else { "FAIL" },
            case.name,
            case.own,
            case.enemy,
            case.remembered,
            case.expected_defend,
            actual,
            plan.posture
        );
        if !passed {
            failed.push(case.name);
        }
    }
    assert!(failed.is_empty(), "期待判断に反した局面: {failed:?}");
}

/// 展開済み砲台の射撃支援を、目標セルを占位できないという理由で失わない。
#[test]
fn deployed_indirect_support_is_counted_without_occupying_the_target() {
    let map = Map::new(
        20,
        7,
        Terrain::Plains,
        crate::resources::GridTopology::Square,
    );
    let master = crate::resources::master_data::MasterDataRegistry::load().unwrap();
    let stats = master
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Artillery.as_str().to_owned(),
        ))
        .unwrap();
    let unit = units(point(3, 3), 1, &stats).remove(0);
    let target = point(9, 3);
    assert!(
        property_control::capture_arrival_turns(
            &map,
            &master,
            (3, 3),
            (9, 3),
            stats.movement_type,
            stats.max_movement,
            stats.max_fuel,
            PlayerId(1),
            &mut Default::default()
        )
        .is_none(),
        "燃料1の砲台は目標セルへ自走できない"
    );
    assert_eq!(
        RouteTravel::new(&map, &master).arrival(&unit, target),
        Some(0),
        "目標を占位できなくても、展開済み砲台の射撃支援を戦力に含める"
    );
}

#[test]
fn indirect_support_that_must_move_includes_its_setup_turn() {
    let map = Map::new(
        20,
        7,
        Terrain::Plains,
        crate::resources::GridTopology::Square,
    );
    let master = crate::resources::master_data::MasterDataRegistry::load().unwrap();
    let stats = master
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Artillery.as_str().to_owned(),
        ))
        .unwrap();
    let unit = units(point(2, 3), 1, &stats).remove(0);
    assert_eq!(
        RouteTravel::new(&map, &master).arrival(&unit, point(9, 3)),
        Some(2),
        "移動した手番に間接射撃できない遅延を含める"
    );
}

/// 海の向こうの戦車は、直線距離では近くてもこの方面の増援として使えない。
/// 同一盤面の中央に海を置く以外は、平地で両方面が優勢なケースと同じにする。
#[test]
fn posture_must_not_count_unreachable_reinforcements() {
    let mut map = Map::new(
        24,
        14,
        Terrain::Plains,
        crate::resources::GridTopology::Square,
    );
    for y in 0..map.height {
        map.set_terrain(5, y, Terrain::Sea).unwrap();
    }
    let master = crate::resources::master_data::MasterDataRegistry::load().unwrap();
    let stats = master
        .create_unit_stats(&crate::resources::master_data::UnitName(
            UnitType::Tank.as_str().to_owned(),
        ))
        .unwrap();
    let anchors = [point(3, 3), point(20, 10)];
    let friendly = units(point(6, 3), 8, &stats)
        .into_iter()
        .chain(units(anchors[1], 8, &stats))
        .collect::<Vec<_>>();
    let enemies = units(anchors[0], 1, &stats)
        .into_iter()
        .chain(units(anchors[1], 1, &stats))
        .collect::<Vec<_>>();
    // 実際の移動ルールでも、海の列を戦車は通過できない。
    assert!(
        crate::systems::movement::get_valid_movement_cost(
            &master,
            stats.movement_type,
            Terrain::Sea,
        )
        .is_none()
    );
    let plan = concentrate_route_efforts(
        &map,
        &master,
        anchors.into_iter().map(objective).collect(),
        &friendly,
        &enemies,
        &[point(2, 3), point(21, 10)],
        &[],
        (None, None),
        PostureBudgets { own: 0, enemy: 0 },
        PostureValues {
            property: 10_000,
            capital: 200_000,
            own_capital: 200_000,
            capturer: 1_000,
        },
        None,
        &mut |a, b| {
            // この固定盤面では、海の列の左右をまたぐ地上経路は存在しない。
            if (a.x < 5) != (b.x < 5) {
                None
            } else {
                Some(
                    map.distance(a.x, a.y, b.x, b.y)
                        .div_ceil(stats.max_movement.max(1)),
                )
            }
        },
    );
    let actual = plan
        .orders
        .iter()
        .map(|order| order.defend)
        .collect::<Vec<_>>();
    println!("unreachable_reinforcements expected_defend=[true, false] actual_defend={actual:?}");
    assert_eq!(
        actual,
        [true, false],
        "北側に到達できる自軍戦車は存在しない"
    );
}
