use super::{UnifiedBoardExtras, plan_unified_production};
use crate::ai::v4::operation::SlotKind;
use crate::ai::v4::test_support::{pos, strait_map};
use crate::ai::v4::trace::ProductionDecision;
use crate::ai::v4::{BoardScan, EnemyProductionForecastTrace, UnitSnapshot};
use crate::collections::HashMap;
use crate::components::{GridPosition, PlayerId, UnitStats};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{
    DamageChart, GridTopology, Map, MovementType, Terrain, UnitRegistry, UnitType,
};

/// マスターデータと実 `DamageChart` を備えたテスト用ワールド部品を構築する。
fn load_full_combat_resources(
    map_name: &str,
) -> (
    Map,
    MasterDataRegistry,
    DamageChart,
    Vec<(UnitType, UnitStats)>,
) {
    let md = MasterDataRegistry::load().unwrap();
    let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
        &md,
        map_name,
        GridTopology::Hex,
    )
    .unwrap();
    let map = world.resource::<Map>().clone();
    let damage_chart = world.resource::<DamageChart>().clone();
    let unit_reg = world.resource::<UnitRegistry>();
    let mut available_types: Vec<(UnitType, UnitStats)> = unit_reg
        .0
        .iter()
        .map(|(ut, st)| (*ut, st.clone()))
        .collect();
    available_types.sort_unstable_by_key(|(ut, _)| ut.as_str());
    (map, md, damage_chart, available_types)
}

fn unit_snap(
    available_types: &[(UnitType, UnitStats)],
    ut: UnitType,
    x: usize,
    y: usize,
    hp: u32,
) -> UnitSnapshot {
    let stats = available_types
        .iter()
        .find(|(t, _)| *t == ut)
        .map(|(_, s)| s.clone())
        .expect("マスターデータに兵種が存在する");
    let free_cargo = stats.max_cargo;
    UnitSnapshot {
        entity: None,
        pos: pos(x, y),
        stats,
        hp,
        free_cargo,
    }
}

fn empty_scan(
    map: Map,
    master_data: MasterDataRegistry,
    damage_chart: DamageChart,
    available_types: Vec<(UnitType, UnitStats)>,
    funds: u32,
) -> BoardScan {
    BoardScan {
        map: map.into(),
        master_data: master_data.into(),
        damage_chart: damage_chart.into(),
        current_turn: 1,
        owned_cities: Vec::new(),
        economy_anchors: Vec::new(),
        route_posture: None,
        opening_policy: None,
        funds,
        capital_position: None,
        free_facilities: Vec::new(),
        production_facilities: Vec::new(),
        available_types,
        my_units: Vec::new(),
        ferried_units: Vec::new(),
        enemy_units: Vec::new(),
        owned_airport_count: 0,
        property_entities: HashMap::default(),
        open_properties: Vec::new(),
        neutral_properties: Vec::new(),
        enemy_income: 1000,
        enemy_production_slots: 0,
        enemy_facilities: Vec::new(),
        my_income: 1000,
        campaign_objectives: Vec::new(),
        capital_assault_authorized: false,
        capital_staging_anchor: None,
        enemy_production_forecast: EnemyProductionForecastTrace::default(),
    }
}

/// 平地マップでの小規模テスト盤面を組み立てる。
fn flat_combat_scan(
    width: usize,
    height: usize,
    funds: u32,
) -> (BoardScan, Vec<(UnitType, UnitStats)>) {
    let (_, md, damage_chart, available_types) = load_full_combat_resources("map_1");
    let map = Map {
        width,
        height,
        tiles: vec![Terrain::Plains; width * height],
        topology: GridTopology::Hex,
    };
    let scan = empty_scan(map, md, damage_chart, available_types.clone(), funds);
    (scan, available_types)
}

#[test]
fn captures_uncovered_neutral_cities_then_stops_when_saturated() {
    let (mut scan, avail) = flat_combat_scan(10, 8, 5000);
    scan.capital_position = Some(pos(1, 1));
    let extras = UnifiedBoardExtras {
        friendly_properties: vec![
            (pos(1, 1), Terrain::Capital),
            (pos(2, 1), Terrain::Factory),
            (pos(1, 2), Terrain::Factory),
        ],
        ..Default::default()
    };
    scan.production_facilities = vec![(pos(2, 1), Terrain::Factory), (pos(1, 2), Terrain::Factory)];
    scan.free_facilities = scan.production_facilities.clone();
    // 未占領中立都市が 2 つあり、敵戦力はまだいない状態
    scan.open_properties = vec![pos(4, 1), pos(1, 4)];
    scan.neutral_properties = scan.open_properties.clone();

    let (planned, trace) = plan_unified_production(&scan, &extras, PlayerId(1), 1, true, &[]);
    assert_eq!(
        planned.len(),
        2,
        "未占領拠点 2 に対して 2 枠分の占領兵を買うべき"
    );
    assert!(
        planned.iter().all(|p| p.role == SlotKind::Capture),
        "すべて占領役割として買われるべき: {:?}",
        planned
            .iter()
            .map(|p| (p.command.unit_type, p.role))
            .collect::<Vec<_>>()
    );
    assert_eq!(trace.steps.len(), planned.len());

    // 全未占領都市の直上に既に占領兵が立ち、敵もおらずスクリーン不要な状態
    scan.open_properties = vec![pos(4, 1), pos(1, 4)];
    scan.my_units = vec![
        unit_snap(&avail, UnitType::Infantry, 4, 1, 100),
        unit_snap(&avail, UnitType::Infantry, 1, 4, 100),
        unit_snap(&avail, UnitType::Infantry, 3, 2, 100),
        unit_snap(&avail, UnitType::Infantry, 3, 3, 100),
    ];
    let (saturated, _) = plan_unified_production(&scan, &extras, PlayerId(1), 2, true, &[]);
    let extra_capturers = saturated
        .iter()
        .filter(|p| p.role == SlotKind::Capture)
        .count();
    assert_eq!(
        extra_capturers, 0,
        "全物件がカバーされた状態では占領枠を追加購入しない"
    );
}

#[test]
fn buys_anti_air_against_air_threat_without_overbuying() {
    let (mut scan, avail) = flat_combat_scan(12, 10, 25000);
    scan.capital_position = Some(pos(1, 1));
    let extras = UnifiedBoardExtras {
        friendly_properties: vec![
            (pos(1, 1), Terrain::Capital),
            (pos(2, 1), Terrain::Factory),
            (pos(1, 2), Terrain::Factory),
            (pos(2, 2), Terrain::Factory),
        ],
        ..Default::default()
    };
    scan.production_facilities = vec![
        (pos(2, 1), Terrain::Factory),
        (pos(1, 2), Terrain::Factory),
        (pos(2, 2), Terrain::Factory),
    ];
    scan.free_facilities = scan.production_facilities.clone();
    // 敵の戦闘ヘリ・爆撃機が接近中、自軍の対空がゼロ
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Bcopters, 6, 3, 100),
        unit_snap(&avail, UnitType::Bomber, 7, 3, 100),
    ];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 4, true, &[]);
    let aa_or_missiles = planned
        .iter()
        .filter(|p| {
            matches!(
                p.command.unit_type,
                UnitType::AntiAir | UnitType::Missiles | UnitType::Fighter
            )
        })
        .count();
    assert!(
        aa_or_missiles >= 1,
        "敵航空脅威に対して対空・迎撃兵器を購入するべき: {:?}",
        planned
            .iter()
            .map(|p| p.command.unit_type)
            .collect::<Vec<_>>()
    );
    assert!(
        aa_or_missiles <= 2,
        "少数の航空脅威に対空を過剰に3体買わないべき: {:?}",
        planned
            .iter()
            .map(|p| p.command.unit_type)
            .collect::<Vec<_>>()
    );
}

#[test]
fn selects_indirect_on_safe_supply_but_avoids_expensive_indirect_under_2t_breach() {
    let (mut scan, avail) = flat_combat_scan(14, 10, 22000);
    scan.capital_position = Some(pos(2, 8));
    let extras = UnifiedBoardExtras {
        friendly_properties: vec![
            (pos(2, 8), Terrain::Capital),
            (pos(3, 7), Terrain::City),
            (pos(4, 7), Terrain::City),
            (pos(3, 8), Terrain::Factory),
            (pos(4, 8), Terrain::Factory),
        ],
        ..Default::default()
    };
    scan.production_facilities = vec![(pos(3, 8), Terrain::Factory), (pos(4, 8), Terrain::Factory)];
    scan.free_facilities = scan.production_facilities.clone();

    // 厚い味方前衛（戦車・軽歩兵）の背後、射程内に高脅威車両がいる安全局面
    scan.my_units = vec![
        unit_snap(&avail, UnitType::Tank, 4, 6, 100),
        unit_snap(&avail, UnitType::Tank, 5, 6, 100),
        unit_snap(&avail, UnitType::Infantry, 3, 6, 100),
        unit_snap(&avail, UnitType::Infantry, 6, 6, 100),
    ];
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Tank, 4, 3, 100),
        unit_snap(&avail, UnitType::MdTank, 5, 3, 100),
        unit_snap(&avail, UnitType::Rockets, 4, 2, 100),
    ];

    let (safe_plan, _) = plan_unified_production(&scan, &extras, PlayerId(1), 5, true, &[]);
    let bought_heavy_safe = safe_plan
        .iter()
        .any(|p| p.command.unit_type == UnitType::HeavySpGun);
    assert!(
        bought_heavy_safe,
        "前衛が厚く安全補給できる敵重戦車面前線では重自走砲が選ばれるべき: {:?}",
        safe_plan
            .iter()
            .map(|p| p.command.unit_type)
            .collect::<Vec<_>>()
    );

    // 次に、前衛に薄い歩兵1体しかおらず、敵戦車2両が工場至近(2T圏内)に迫る危険局面
    scan.funds = 16800;
    scan.my_units = vec![unit_snap(&avail, UnitType::Infantry, 4, 7, 100)];
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Tank, 4, 4, 100),
        unit_snap(&avail, UnitType::Tank, 5, 4, 100),
        unit_snap(&avail, UnitType::Infantry, 4, 5, 100),
    ];
    let (danger_plan, _) = plan_unified_production(&scan, &extras, PlayerId(1), 4, true, &[]);
    let bought_heavy_danger = danger_plan
        .iter()
        .any(|p| p.command.unit_type == UnitType::HeavySpGun);
    assert!(
        !bought_heavy_danger,
        "薄い前衛の背後で2T突破の危険がある盤面では高価な重自走砲を避けるべき: {:?}",
        danger_plan
            .iter()
            .map(|p| p.command.unit_type)
            .collect::<Vec<_>>()
    );
}

#[test]
fn avoids_spawning_ground_units_on_isolated_island() {
    let (_, md, damage_chart, avail) = load_full_combat_resources("map_3");
    let map = strait_map(Some(Terrain::Shoal));
    let mut scan = empty_scan(map, md, damage_chart, avail.clone(), 20000);
    // 左岸に工場 (0, 1) と港 (1, 1) があり、海を挟んで対岸 (8, 1) に敵と物件のみがある孤島構造
    if let Some(m) = std::sync::Arc::get_mut(&mut scan.map) {
        m.set_terrain(0, 1, Terrain::Factory).unwrap();
        m.set_terrain(1, 1, Terrain::Port).unwrap();
    }
    let extras = UnifiedBoardExtras {
        friendly_properties: vec![(pos(0, 1), Terrain::Factory), (pos(1, 1), Terrain::Port)],
        ..Default::default()
    };
    scan.production_facilities = vec![(pos(0, 1), Terrain::Factory), (pos(1, 1), Terrain::Port)];
    scan.free_facilities = scan.production_facilities.clone();
    scan.open_properties = vec![pos(8, 1)];
    scan.enemy_units = vec![unit_snap(&avail, UnitType::Tank, 8, 1, 100)];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 3, true, &[]);
    let bought_ground = planned.iter().any(|p| {
        scan.available_types
            .iter()
            .find(|(ut, _)| *ut == p.command.unit_type)
            .is_some_and(|(_, s)| {
                !matches!(s.movement_type, MovementType::Air | MovementType::Ship)
            })
    });
    assert!(
        !bought_ground,
        "海で隔てられ敵にも物件にも到達できない孤島工場で地上ユニットを誤生産しないべき: {:?}",
        planned
            .iter()
            .map(|p| p.command.unit_type)
            .collect::<Vec<_>>()
    );
}

#[test]
fn maintains_budget_unique_facilities_and_trace_consistency() {
    let (mut scan, avail) = flat_combat_scan(12, 10, 18500);
    scan.capital_position = Some(pos(1, 1));
    let extras = UnifiedBoardExtras {
        friendly_properties: vec![
            (pos(1, 1), Terrain::Capital),
            (pos(2, 1), Terrain::Factory),
            (pos(1, 2), Terrain::Factory),
            (pos(2, 2), Terrain::Factory),
        ],
        ..Default::default()
    };
    scan.production_facilities = vec![
        (pos(2, 1), Terrain::Factory),
        (pos(1, 2), Terrain::Factory),
        (pos(2, 2), Terrain::Factory),
    ];
    scan.free_facilities = scan.production_facilities.clone();
    scan.open_properties = vec![pos(5, 1), pos(1, 5)];
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Infantry, 7, 3, 100),
        unit_snap(&avail, UnitType::Tank, 8, 4, 100),
    ];

    let (planned, trace) = plan_unified_production(&scan, &extras, PlayerId(1), 3, true, &[]);
    let total_cost: u32 = planned.iter().map(|p| p.cost).sum();
    assert!(
        total_cost <= scan.funds,
        "合計コスト {total_cost} が所持金 {} を超えている",
        scan.funds
    );

    let mut used_facs = std::collections::HashSet::<GridPosition>::new();
    for p in &planned {
        let fac = GridPosition {
            x: p.command.target_x,
            y: p.command.target_y,
        };
        assert!(
            used_facs.insert(fac),
            "同一施設 {:?} に重複発注している",
            fac
        );
        match p.role {
            SlotKind::Capture => assert!(p.capture_intent.is_some()),
            SlotKind::Combat => assert!(p.deployment.is_some()),
            other => panic!("想定外の role {other:?}"),
        }
    }

    let produced_trace_cnt = trace
        .steps
        .iter()
        .filter(|s| matches!(s.decision, ProductionDecision::Produced { .. }))
        .count();
    assert_eq!(
        produced_trace_cnt,
        planned.len(),
        "trace.steps の Produced 件数とコマンド数が一致しなければならない"
    );
}

#[test]
fn assigns_distinct_capture_targets_across_multiple_factories() {
    let (mut scan, avail) = flat_combat_scan(12, 10, 6000);
    scan.capital_position = Some(pos(1, 1));
    let extras = UnifiedBoardExtras {
        friendly_properties: vec![
            (pos(1, 1), Terrain::Capital),
            (pos(2, 1), Terrain::Factory),
            (pos(1, 2), Terrain::Factory),
            (pos(2, 2), Terrain::Factory),
        ],
        ..Default::default()
    };
    scan.production_facilities = vec![
        (pos(2, 1), Terrain::Factory),
        (pos(1, 2), Terrain::Factory),
        (pos(2, 2), Terrain::Factory),
    ];
    scan.free_facilities = scan.production_facilities.clone();
    scan.open_properties = vec![pos(5, 1), pos(1, 5), pos(5, 5)];
    scan.neutral_properties = scan.open_properties.clone();

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 1, true, &[]);
    let cap_targets: Vec<GridPosition> = planned
        .iter()
        .filter(|p| p.role == SlotKind::Capture)
        .filter_map(|p| p.capture_intent.as_ref()?.mission_target)
        .collect();
    assert_eq!(
        cap_targets.len(),
        3,
        "3工場・未占領都市3つに対し 3つの占領スロットが発行されるべき: {:?}",
        planned
            .iter()
            .map(|p| (p.command.unit_type, p.role))
            .collect::<Vec<_>>()
    );
    let unique_targets: std::collections::HashSet<GridPosition> =
        cap_targets.iter().copied().collect();
    assert_eq!(
        unique_targets.len(),
        3,
        "各占領兵へ重複しない異なる都市が割り当てられるべき: {cap_targets:?}"
    );
    for expected in &scan.open_properties {
        assert!(
            unique_targets.contains(expected),
            "未占領都市 {expected:?} が占領割当に含まれていない: {unique_targets:?}"
        );
    }

    // すでに全3都市の直上に歩兵がいる盤面では追加占領を購入しない
    scan.my_units = vec![
        unit_snap(&avail, UnitType::Infantry, 5, 1, 100),
        unit_snap(&avail, UnitType::Infantry, 1, 5, 100),
        unit_snap(&avail, UnitType::Infantry, 5, 5, 100),
        unit_snap(&avail, UnitType::Infantry, 3, 3, 100),
    ];
    let (saturated, _) = plan_unified_production(&scan, &extras, PlayerId(1), 2, true, &[]);
    let extra_cap = saturated
        .iter()
        .filter(|p| p.role == SlotKind::Capture)
        .count();
    assert_eq!(
        extra_cap, 0,
        "全3都市がカバーされた状態では占領スロットを追加購入しないべき"
    );
}

#[test]
fn excludes_transport_units_from_direct_combat_candidates() {
    let (mut scan, avail) = flat_combat_scan(10, 8, 12000);
    scan.capital_position = Some(pos(1, 1));
    let extras = UnifiedBoardExtras {
        friendly_properties: vec![
            (pos(1, 1), Terrain::Capital),
            (pos(2, 1), Terrain::Airport),
            (pos(1, 2), Terrain::Factory),
        ],
        ..Default::default()
    };
    scan.production_facilities = vec![(pos(2, 1), Terrain::Airport), (pos(1, 2), Terrain::Factory)];
    scan.free_facilities = scan.production_facilities.clone();
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Infantry, 6, 3, 100),
        unit_snap(&avail, UnitType::Infantry, 7, 3, 100),
        unit_snap(&avail, UnitType::Infantry, 6, 4, 100),
    ];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 4, true, &[]);
    let bought_transport = planned.iter().any(|p| {
        scan.available_types
            .iter()
            .find(|(ut, _)| *ut == p.command.unit_type)
            .is_some_and(|(ut, s)| {
                s.max_cargo > 0
                    && (matches!(s.movement_type, MovementType::Air | MovementType::Ship)
                        || super::threats::max_base_damage(
                            &scan.damage_chart,
                            *ut,
                            UnitType::Infantry,
                        ) == 0)
            })
    });
    assert!(
        !bought_transport,
        "輸送ユニット（輸送ヘリ等）を直接戦闘要員として購入しないべき: {:?}",
        planned
            .iter()
            .map(|p| p.command.unit_type)
            .collect::<Vec<_>>()
    );
}

#[test]
fn does_not_spam_anti_air_when_existing_aa_already_covers_enemy_air() {
    let (mut scan, avail) = flat_combat_scan(12, 10, 18000);
    scan.capital_position = Some(pos(1, 1));
    let extras = UnifiedBoardExtras {
        friendly_properties: vec![
            (pos(1, 1), Terrain::Capital),
            (pos(2, 1), Terrain::Factory),
            (pos(1, 2), Terrain::Factory),
            (pos(2, 2), Terrain::Factory),
            (pos(3, 1), Terrain::Factory),
        ],
        ..Default::default()
    };
    scan.production_facilities = vec![
        (pos(2, 1), Terrain::Factory),
        (pos(1, 2), Terrain::Factory),
        (pos(2, 2), Terrain::Factory),
        (pos(3, 1), Terrain::Factory),
    ];
    scan.free_facilities = scan.production_facilities.clone();
    // 自軍対空戦車2両が既におり、地上には敵戦車と大量の歩兵がいる状態
    scan.my_units = vec![
        unit_snap(&avail, UnitType::AntiAir, 3, 4, 100),
        unit_snap(&avail, UnitType::AntiAir, 5, 4, 100),
        unit_snap(&avail, UnitType::Tank, 4, 4, 100),
        unit_snap(&avail, UnitType::Infantry, 3, 5, 100),
        unit_snap(&avail, UnitType::Infantry, 4, 5, 100),
        unit_snap(&avail, UnitType::Infantry, 5, 5, 100),
    ];
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Bcopters, 8, 7, 100),
        unit_snap(&avail, UnitType::Bcopters, 9, 7, 100),
        unit_snap(&avail, UnitType::Tank, 4, 7, 100),
        unit_snap(&avail, UnitType::Tank, 5, 7, 100),
        unit_snap(&avail, UnitType::Infantry, 3, 7, 100),
        unit_snap(&avail, UnitType::Infantry, 6, 7, 100),
        unit_snap(&avail, UnitType::Infantry, 7, 7, 100),
    ];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 10, true, &[]);
    let aa_bought = planned
        .iter()
        .filter(|p| p.command.unit_type == UnitType::AntiAir)
        .count();
    assert_eq!(
        aa_bought,
        0,
        "既に対空戦車2両で敵ヘリ2機をカバーしている盤面では対空戦車を追加連打せず地上戦力を生産するべき: {:?}",
        planned
            .iter()
            .map(|p| p.command.unit_type)
            .collect::<Vec<_>>()
    );
    assert!(
        !planned.is_empty(),
        "地上戦力（戦車・歩兵等）は生産されるべき"
    );
}

#[test]
fn fills_empty_factories_with_cheap_screen_when_outnumbered_midgame() {
    let (mut scan, avail) = flat_combat_scan(12, 10, 15000);
    scan.capital_position = Some(pos(1, 1));
    scan.production_facilities = vec![
        (pos(2, 1), Terrain::Factory),
        (pos(3, 1), Terrain::Factory),
        (pos(4, 1), Terrain::Factory),
        (pos(1, 2), Terrain::Factory),
        (pos(2, 2), Terrain::Factory),
        (pos(3, 2), Terrain::Factory),
    ];
    let mut extras = UnifiedBoardExtras {
        friendly_properties: scan.production_facilities.clone(),
        ..Default::default()
    };
    extras
        .friendly_properties
        .push((pos(1, 1), Terrain::Capital));
    scan.free_facilities = scan.production_facilities.clone();
    // 未占領都市ゼロ、自軍地上14体 vs 敵地上26体の頭数劣勢中盤
    scan.my_units = (0..14)
        .map(|i| {
            let ut = if i < 2 {
                UnitType::Tank
            } else {
                UnitType::Infantry
            };
            unit_snap(&avail, ut, 2 + (i % 7), 4 + (i / 7), 100)
        })
        .collect();
    scan.enemy_units = (0..26)
        .map(|i| {
            let ut = if i < 4 {
                UnitType::Tank
            } else {
                UnitType::Infantry
            };
            unit_snap(&avail, ut, 1 + (i % 8), 6 + (i / 8), 100)
        })
        .collect();

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 10, true, &[]);
    assert!(
        planned.len() >= 4,
        "中盤の頭数劣勢時に空き6工場から4体以上生産して前線を埋めるべき: count={}, units={:?}",
        planned.len(),
        planned
            .iter()
            .map(|p| p.command.unit_type)
            .collect::<Vec<_>>()
    );
    assert!(
        planned
            .iter()
            .any(|p| p.command.unit_type == UnitType::Infantry),
        "空き工場を埋める廉価前衛（歩兵）が含まれるべき: {:?}",
        planned
            .iter()
            .map(|p| p.command.unit_type)
            .collect::<Vec<_>>()
    );
}

#[test]
fn spends_surplus_funds_on_high_tier_units_when_cash_abundant() {
    let (mut scan, avail) = flat_combat_scan(12, 10, 85000);
    scan.capital_position = Some(pos(1, 1));
    scan.production_facilities = vec![
        (pos(2, 1), Terrain::Factory),
        (pos(1, 2), Terrain::Factory),
        (pos(2, 2), Terrain::Factory),
    ];
    let mut extras = UnifiedBoardExtras {
        friendly_properties: scan.production_facilities.clone(),
        ..Default::default()
    };
    extras
        .friendly_properties
        .push((pos(1, 1), Terrain::Capital));
    scan.free_facilities = scan.production_facilities.clone();

    // 自軍 Infantry×12 + Tank×2（合計14体）
    scan.my_units = (0..14)
        .map(|i| {
            let ut = if i < 2 {
                UnitType::Tank
            } else {
                UnitType::Infantry
            };
            unit_snap(&avail, ut, 2 + (i % 7), 3 + (i / 7), 100)
        })
        .collect();
    // 敵 Tank×3 + MdTank×2 + Infantry×6（合計11体）
    scan.enemy_units = (0..11)
        .map(|i| {
            let ut = if i < 3 {
                UnitType::Tank
            } else if i < 5 {
                UnitType::MdTank
            } else {
                UnitType::Infantry
            };
            unit_snap(&avail, ut, 2 + (i % 6), 6 + (i / 6), 100)
        })
        .collect();

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 16, true, &[]);
    let cost_of = |ut: UnitType| -> u32 {
        avail
            .iter()
            .find(|(t, _)| *t == ut)
            .map(|(_, s)| s.cost)
            .expect("兵種コストが取得できる")
    };
    let total_spent: u32 = planned.iter().map(|p| cost_of(p.command.unit_type)).sum();
    let has_high_tier = planned.iter().any(|p| cost_of(p.command.unit_type) >= 6000);

    assert!(
        total_spent >= 18000,
        "余剰資金85,000G下で歩兵のみ買って貯金せず十分な額を戦力投資するべき: total_spent={}, units={:?}",
        total_spent,
        planned
            .iter()
            .map(|p| p.command.unit_type)
            .collect::<Vec<_>>()
    );
    assert!(
        has_high_tier,
        "余剰資金潤沢時は高価戦闘兵器（cost >= 6000）を少なくとも1体以上生産するべき: units={:?}",
        planned
            .iter()
            .map(|p| (p.command.unit_type, cost_of(p.command.unit_type)))
            .collect::<Vec<_>>()
    );
}

#[test]
fn avoids_sending_bomber_into_enemy_fighter_aa_net() {
    let (mut scan, avail) = flat_combat_scan(12, 10, 30000);
    scan.capital_position = Some(pos(1, 1));
    scan.owned_airport_count = 1;
    scan.production_facilities = vec![
        (pos(1, 2), Terrain::Airport),
        (pos(2, 1), Terrain::Factory),
        (pos(2, 2), Terrain::Factory),
    ];
    let mut extras = UnifiedBoardExtras {
        friendly_properties: scan.production_facilities.clone(),
        ..Default::default()
    };
    extras
        .friendly_properties
        .push((pos(1, 1), Terrain::Capital));
    scan.free_facilities = scan.production_facilities.clone();

    // 自軍前線 Infantry×6 + Tank×2（y=3..4）
    scan.my_units = (0..8)
        .map(|i| {
            let ut = if i < 2 {
                UnitType::Tank
            } else {
                UnitType::Infantry
            };
            unit_snap(&avail, ut, 2 + (i % 4), 3 + (i / 4), 100)
        })
        .collect();

    // 敵 Fighter×2（y=6, 迎撃圏内）+ AntiAir×2（y=5）+ Tank×2 + Infantry×4（y=5..6）
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::AntiAir, 2, 5, 100),
        unit_snap(&avail, UnitType::AntiAir, 3, 5, 100),
        unit_snap(&avail, UnitType::Tank, 4, 5, 100),
        unit_snap(&avail, UnitType::Tank, 5, 5, 100),
        unit_snap(&avail, UnitType::Infantry, 1, 5, 100),
        unit_snap(&avail, UnitType::Infantry, 1, 6, 100),
        unit_snap(&avail, UnitType::Infantry, 2, 6, 100),
        unit_snap(&avail, UnitType::Fighter, 3, 6, 100),
        unit_snap(&avail, UnitType::Fighter, 4, 6, 100),
        unit_snap(&avail, UnitType::Infantry, 5, 6, 100),
    ];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 12, true, &[]);
    let units: Vec<UnitType> = planned.iter().map(|p| p.command.unit_type).collect();
    assert!(
        !units.contains(&UnitType::Bomber),
        "敵Fighter×2+AntiAir×2の迎撃網へ高コストBomberを特攻生産してはならない: {:?}",
        units
    );
    assert!(
        !units.is_empty(),
        "対空・制空・地上戦力など少なくとも1体以上の戦力は生産するべき"
    );
}

#[test]
fn continues_producing_from_rear_factories_when_screen_saturated_and_enemies_far() {
    let (mut scan, avail) = flat_combat_scan(14, 12, 16000);
    scan.capital_position = Some(pos(1, 1));
    scan.production_facilities = vec![
        (pos(2, 1), Terrain::Factory),
        (pos(3, 1), Terrain::Factory),
        (pos(4, 1), Terrain::Factory),
        (pos(5, 1), Terrain::Factory),
        (pos(6, 1), Terrain::Factory),
    ];
    let mut extras = UnifiedBoardExtras {
        friendly_properties: scan.production_facilities.clone(),
        ..Default::default()
    };
    extras
        .friendly_properties
        .push((pos(1, 1), Terrain::Capital));
    scan.free_facilities = scan.production_facilities.clone();

    // 自軍12体（target_screen=11 を超えるスクリーン飽和状態）を中央 y=5..6 に配置
    scan.my_units = (0..12)
        .map(|i| {
            let ut = if i < 2 {
                UnitType::Tank
            } else {
                UnitType::Infantry
            };
            unit_snap(&avail, ut, 2 + (i % 6), 5 + (i / 6), 100)
        })
        .collect();
    // 敵4体（後方工場から距離9〜11の遠方 y=10..11 に配置）
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Tank, 4, 10, 100),
        unit_snap(&avail, UnitType::Tank, 5, 10, 100),
        unit_snap(&avail, UnitType::Infantry, 4, 11, 100),
        unit_snap(&avail, UnitType::Infantry, 5, 11, 100),
    ];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 15, true, &[]);
    assert!(
        planned.len() >= 4,
        "スクリーン飽和かつ敵遠方の盤面でも空き後方工場と資金16,000Gから4体以上生産を継続するべき: count={}, units={:?}",
        planned.len(),
        planned
            .iter()
            .map(|p| p.command.unit_type)
            .collect::<Vec<_>>()
    );
}

#[test]
fn selects_fast_anti_capturer_when_enemy_infantry_rush_neutral_properties() {
    let (mut scan, avail) = flat_combat_scan(12, 10, 10000);
    scan.capital_position = Some(pos(1, 1));
    scan.production_facilities = vec![
        (pos(2, 1), Terrain::Factory),
        (pos(1, 2), Terrain::Factory),
        (pos(2, 2), Terrain::Factory),
    ];
    let mut extras = UnifiedBoardExtras {
        friendly_properties: scan.production_facilities.clone(),
        ..Default::default()
    };
    extras
        .friendly_properties
        .push((pos(1, 1), Terrain::Capital));
    scan.free_facilities = scan.production_facilities.clone();

    // 中立物件群上に敵歩兵が迫る（味方歩兵4体だけでは決定的阻止ができない）盤面
    scan.neutral_properties = vec![pos(5, 3), pos(6, 3), pos(5, 4), pos(6, 4)];
    scan.open_properties = scan.neutral_properties.clone();
    scan.my_units = vec![
        unit_snap(&avail, UnitType::Infantry, 3, 3, 100),
        unit_snap(&avail, UnitType::Infantry, 4, 2, 100),
        unit_snap(&avail, UnitType::Infantry, 4, 3, 100),
        unit_snap(&avail, UnitType::Infantry, 4, 4, 100),
    ];
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Infantry, 5, 3, 100),
        unit_snap(&avail, UnitType::Infantry, 6, 3, 100),
        unit_snap(&avail, UnitType::Infantry, 5, 4, 100),
        unit_snap(&avail, UnitType::Infantry, 6, 4, 100),
    ];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 2, true, &[]);
    let units: Vec<UnitType> = planned.iter().map(|p| p.command.unit_type).collect();
    let has_fast_anti_infantry = planned.iter().any(|p| {
        let Some((_, st)) = avail.iter().find(|(t, _)| *t == p.command.unit_type) else {
            return false;
        };
        let anti_inf_dmg = super::threats::max_base_damage(
            &scan.damage_chart,
            p.command.unit_type,
            UnitType::Infantry,
        );
        st.max_range <= 1 && st.max_movement >= 4 && anti_inf_dmg >= 55
    });
    assert!(
        has_fast_anti_infantry,
        "中立物件上の敵占領ラッシュに対し、高機動かつ対歩兵決定的火力(>=55%)を持つ直接ユニットを採用するべき: {:?}",
        units
    );
}

#[test]
fn avoids_early_unscreened_heavy_indirect_when_outnumbered() {
    let (mut scan, avail) = flat_combat_scan(14, 14, 18000);
    // 敵の装甲車(mp=6)が2T突破しないようにy=7..8に森帯を配置する
    if let Some(m) = std::sync::Arc::get_mut(&mut scan.map) {
        for x in 0..14 {
            m.set_terrain(x, 7, Terrain::Forest).unwrap();
            m.set_terrain(x, 8, Terrain::Forest).unwrap();
        }
    }
    scan.capital_position = Some(pos(1, 1));
    scan.production_facilities = vec![
        (pos(2, 1), Terrain::Factory),
        (pos(3, 1), Terrain::Factory),
        (pos(4, 1), Terrain::Factory),
    ];
    let mut extras = UnifiedBoardExtras {
        friendly_properties: scan.production_facilities.clone(),
        ..Default::default()
    };
    extras
        .friendly_properties
        .push((pos(1, 1), Terrain::Capital));
    scan.free_facilities = scan.production_facilities.clone();

    // 味方歩兵5体 vs 敵10体（装甲車2 + 歩兵8）の序盤劣勢盤面
    scan.my_units = (0..5)
        .map(|i| unit_snap(&avail, UnitType::Infantry, 2 + i, 4, 100))
        .collect();
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Recon, 3, 10, 100),
        unit_snap(&avail, UnitType::Recon, 6, 10, 100),
        unit_snap(&avail, UnitType::Infantry, 2, 7, 100),
        unit_snap(&avail, UnitType::Infantry, 3, 7, 100),
        unit_snap(&avail, UnitType::Infantry, 4, 7, 100),
        unit_snap(&avail, UnitType::Infantry, 5, 7, 100),
        unit_snap(&avail, UnitType::Infantry, 2, 8, 100),
        unit_snap(&avail, UnitType::Infantry, 3, 8, 100),
        unit_snap(&avail, UnitType::Infantry, 4, 8, 100),
        unit_snap(&avail, UnitType::Infantry, 5, 8, 100),
    ];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 2, true, &[]);
    let units: Vec<UnitType> = planned.iter().map(|p| p.command.unit_type).collect();
    assert!(
        !units.contains(&UnitType::HeavySpGun),
        "前衛薄い序盤劣勢盤面で全予算近いHeavySpGunを買わず前衛戦力を厚くするべき: {:?}",
        units
    );
    assert!(
        planned.iter().any(|p| p.role == SlotKind::Combat),
        "前衛戦力増強のためCombatロールを少なくとも1体以上生産するべき: {:?}",
        units
    );
}

#[test]
fn does_not_overbuy_pure_anti_air_when_one_counter_already_exists() {
    let (mut scan, avail) = flat_combat_scan(12, 10, 16000);
    scan.capital_position = Some(pos(1, 1));
    scan.production_facilities = vec![
        (pos(2, 1), Terrain::Factory),
        (pos(1, 2), Terrain::Factory),
        (pos(2, 2), Terrain::Factory),
    ];
    let mut extras = UnifiedBoardExtras {
        friendly_properties: scan.production_facilities.clone(),
        ..Default::default()
    };
    extras
        .friendly_properties
        .push((pos(1, 1), Terrain::Capital));
    scan.free_facilities = scan.production_facilities.clone();

    // 既に味方対空戦車1両 + 歩兵4体が存在する状態で、敵戦闘ヘリ2機 + 地上部隊が接近
    scan.my_units = vec![
        unit_snap(&avail, UnitType::AntiAir, 4, 3, 100),
        unit_snap(&avail, UnitType::Infantry, 3, 3, 100),
        unit_snap(&avail, UnitType::Infantry, 5, 3, 100),
        unit_snap(&avail, UnitType::Infantry, 3, 4, 100),
        unit_snap(&avail, UnitType::Infantry, 4, 4, 100),
    ];
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Bcopters, 6, 5, 100),
        unit_snap(&avail, UnitType::Bcopters, 7, 5, 100),
        unit_snap(&avail, UnitType::Tank, 6, 6, 100),
        unit_snap(&avail, UnitType::Tank, 7, 6, 100),
        unit_snap(&avail, UnitType::Infantry, 5, 6, 100),
        unit_snap(&avail, UnitType::Infantry, 8, 6, 100),
        unit_snap(&avail, UnitType::Infantry, 5, 7, 100),
        unit_snap(&avail, UnitType::Infantry, 7, 7, 100),
    ];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 3, true, &[]);
    let units: Vec<UnitType> = planned.iter().map(|p| p.command.unit_type).collect();
    let aa_count = units.iter().filter(|&&u| u == UnitType::AntiAir).count();
    assert!(
        aa_count <= 1,
        "既に対空戦車1両がある盤面で1ターンにAntiAirを2体以上重複購入してはならない: aa_count={}, units={:?}",
        aa_count,
        units
    );
}

#[test]
fn beam_search_finds_high_roi_mixed_portfolio_over_greedy_expensive_pair() {
    let (mut scan, avail) = flat_combat_scan(14, 12, 10000);
    scan.capital_position = Some(pos(1, 1));
    scan.production_facilities = vec![
        (pos(2, 1), Terrain::Factory),
        (pos(3, 1), Terrain::Factory),
        (pos(4, 1), Terrain::Factory),
        (pos(1, 2), Terrain::Factory),
        (pos(2, 2), Terrain::Factory),
        (pos(3, 2), Terrain::Factory),
    ];
    let mut extras = UnifiedBoardExtras {
        friendly_properties: scan.production_facilities.clone(),
        ..Default::default()
    };
    extras
        .friendly_properties
        .push((pos(1, 1), Terrain::Capital));
    scan.free_facilities = scan.production_facilities.clone();

    // 工場近くに6個の未占領中立都市があり、対岸に中立都市圏内の敵歩兵6体が展開している T1 盤面
    scan.neutral_properties = vec![
        pos(5, 2),
        pos(6, 2),
        pos(2, 5),
        pos(3, 5),
        pos(4, 5),
        pos(5, 5),
    ];
    scan.open_properties = scan.neutral_properties.clone();
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Infantry, 10, 8, 100),
        unit_snap(&avail, UnitType::Infantry, 11, 8, 100),
        unit_snap(&avail, UnitType::Infantry, 12, 8, 100),
        unit_snap(&avail, UnitType::Infantry, 10, 9, 100),
        unit_snap(&avail, UnitType::Infantry, 11, 9, 100),
        unit_snap(&avail, UnitType::Infantry, 12, 9, 100),
    ];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(2), 1, true, &[]);
    let units: Vec<UnitType> = planned.iter().map(|p| p.command.unit_type).collect();
    let cap_count = planned
        .iter()
        .filter(|p| p.role == SlotKind::Capture)
        .count();
    assert!(
        planned.len() >= 5,
        "深さ2でRecon×2のみに枝刈りされず5体以上生産して多工場展開できるべき: count={}, units={:?}",
        planned.len(),
        units
    );
    assert!(
        cap_count >= 3,
        "資金10,000Gの初手展開で少なくとも3体以上のCapture役を同時生産するべき: cap_count={}, units={:?}",
        cap_count,
        units
    );
}

#[test]
fn avoids_wasting_capturers_on_enemy_closer_neutral_properties() {
    let (mut scan, avail) = flat_combat_scan(14, 10, 10000);
    scan.capital_position = Some(pos(0, 4));
    scan.production_facilities = vec![
        (pos(1, 3), Terrain::Factory),
        (pos(1, 4), Terrain::Factory),
        (pos(1, 5), Terrain::Factory),
        (pos(2, 4), Terrain::Factory),
        (pos(2, 5), Terrain::Factory),
    ];
    let mut extras = UnifiedBoardExtras {
        friendly_properties: scan.production_facilities.clone(),
        ..Default::default()
    };
    extras
        .friendly_properties
        .push((pos(0, 4), Terrain::Capital));
    scan.free_facilities = scan.production_facilities.clone();

    // 自軍側中立都市2個 + 敵歩兵が隣接済みの敵陣側中立都市3個
    scan.neutral_properties = vec![pos(4, 4), pos(4, 5), pos(10, 3), pos(10, 4), pos(10, 5)];
    scan.open_properties = scan.neutral_properties.clone();
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Infantry, 11, 3, 100),
        unit_snap(&avail, UnitType::Infantry, 11, 4, 100),
        unit_snap(&avail, UnitType::Infantry, 11, 5, 100),
    ];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(2), 1, true, &[]);
    let units: Vec<UnitType> = planned.iter().map(|p| p.command.unit_type).collect();
    let cap_count = planned
        .iter()
        .filter(|p| p.role == SlotKind::Capture)
        .count();
    let combat_count = planned
        .iter()
        .filter(|p| p.role == SlotKind::Combat)
        .count();
    assert!(
        cap_count <= 3,
        "敵が先着する敵陣側中立都市へ5体分のCapture役を浪費しないべき: cap_count={}, units={:?}",
        cap_count,
        units
    );
    assert!(
        combat_count >= 1,
        "敵先着中立都市を割り引いた結果、余力でCombat前衛（Recon等）を併産するべき: units={:?}",
        units
    );
}

#[test]
fn produces_air_transport_for_unreachable_neutral_city_when_airport_available() {
    let (_, md, damage_chart, avail) = load_full_combat_resources("map_3");
    let mut map = strait_map(None);
    map.set_terrain(0, 0, Terrain::Capital).unwrap();
    map.set_terrain(0, 1, Terrain::Factory).unwrap();
    map.set_terrain(1, 1, Terrain::Airport).unwrap();
    map.set_terrain(8, 1, Terrain::City).unwrap();

    let mut scan = empty_scan(map, md, damage_chart, avail.clone(), 8000);
    scan.capital_position = Some(pos(0, 0));
    scan.owned_airport_count = 1;
    scan.production_facilities = vec![(pos(0, 1), Terrain::Factory), (pos(1, 1), Terrain::Airport)];
    scan.free_facilities = scan.production_facilities.clone();
    let extras = UnifiedBoardExtras {
        friendly_properties: vec![
            (pos(0, 0), Terrain::Capital),
            (pos(0, 1), Terrain::Factory),
            (pos(1, 1), Terrain::Airport),
        ],
        ..Default::default()
    };
    scan.my_units = vec![unit_snap(&avail, UnitType::Infantry, 0, 2, 100)];
    scan.neutral_properties = vec![pos(8, 1)];
    scan.open_properties = vec![pos(8, 1)];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 2, true, &[]);
    let transport_count = planned
        .iter()
        .filter(|p| {
            p.role == SlotKind::Transport
                && p.command.unit_type == UnitType::TransportHelicopter
                && p.capture_intent.as_ref().is_some_and(|ci| {
                    ci.role == crate::ai::v4::campaign_execution::CampaignProductionRole::Transport
                })
        })
        .count();
    assert_eq!(
        transport_count,
        1,
        "海峡で隔てられた中立都市に対し空き空港から輸送ヘリ(Transport役)を1機生産するべき: {:?}",
        planned
            .iter()
            .map(|p| (p.command.unit_type, p.role))
            .collect::<Vec<_>>()
    );
}

#[test]
fn selects_bcopters_from_idle_airport_against_ground_without_aa() {
    let (mut scan, avail) = flat_combat_scan(12, 10, 14000);
    scan.capital_position = Some(pos(1, 1));
    scan.owned_airport_count = 1;
    scan.production_facilities = vec![
        (pos(1, 2), Terrain::Airport),
        (pos(2, 1), Terrain::Factory),
        (pos(2, 2), Terrain::Factory),
    ];
    let mut extras = UnifiedBoardExtras {
        friendly_properties: scan.production_facilities.clone(),
        ..Default::default()
    };
    extras
        .friendly_properties
        .push((pos(1, 1), Terrain::Capital));
    scan.free_facilities = scan.production_facilities.clone();

    scan.my_units = vec![
        unit_snap(&avail, UnitType::Tank, 4, 3, 100),
        unit_snap(&avail, UnitType::Infantry, 4, 4, 100),
        unit_snap(&avail, UnitType::Infantry, 4, 5, 100),
    ];
    // 対空兵器・戦闘機が一切いない敵地上部隊
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Tank, 6, 3, 100),
        unit_snap(&avail, UnitType::Tank, 7, 3, 100),
        unit_snap(&avail, UnitType::Recon, 6, 4, 100),
        unit_snap(&avail, UnitType::Infantry, 5, 4, 100),
        unit_snap(&avail, UnitType::Infantry, 7, 4, 100),
    ];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(1), 4, true, &[]);
    let units: Vec<UnitType> = planned.iter().map(|p| p.command.unit_type).collect();
    assert!(
        units.contains(&UnitType::Bcopters),
        "敵に対空網がなく空き空港がある盤面では対地航空戦力Bcoptersを採用するべき: {:?}",
        units
    );
}

#[test]
fn p2_still_captures_equidistant_center_neutral_city_despite_p1_first_move() {
    let (mut scan, avail) = flat_combat_scan(14, 12, 10000);
    scan.capital_position = Some(pos(1, 5));
    scan.production_facilities = vec![(pos(2, 5), Terrain::Factory), (pos(2, 6), Terrain::Factory)];
    let extras = UnifiedBoardExtras {
        friendly_properties: vec![
            (pos(1, 5), Terrain::Capital),
            (pos(2, 5), Terrain::Factory),
            (pos(2, 6), Terrain::Factory),
        ],
        ..Default::default()
    };
    scan.free_facilities = scan.production_facilities.clone();

    // 等距離にある中央中立都市 (6,5), (6,6) に対し、先攻 P1 歩兵は1手だけ早く出現済み（min_enemy_cap_eta=2 vs my_min_eta=3）
    scan.neutral_properties = vec![pos(6, 5), pos(6, 6)];
    scan.open_properties = scan.neutral_properties.clone();
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Infantry, 10, 5, 100),
        unit_snap(&avail, UnitType::Infantry, 10, 6, 100),
    ];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(2), 1, true, &[]);
    let cap_count = planned
        .iter()
        .filter(|p| p.role == SlotKind::Capture)
        .count();
    assert!(
        cap_count >= 1,
        "先攻と1手差の中央中立都市を後攻が全面放棄せず少なくとも1体以上のCapture役を生産するべき: {:?}",
        planned
            .iter()
            .map(|p| (p.command.unit_type, p.role))
            .collect::<Vec<_>>()
    );
}

#[test]
fn spreads_direct_combat_anchors_across_multiple_frontline_enemies() {
    let (mut scan, avail) = flat_combat_scan(14, 12, 16000);
    scan.capital_position = Some(pos(1, 5));
    scan.production_facilities = vec![
        (pos(2, 2), Terrain::Factory),
        (pos(2, 5), Terrain::Factory),
        (pos(2, 8), Terrain::Factory),
        (pos(1, 5), Terrain::Capital),
    ];
    let extras = UnifiedBoardExtras {
        friendly_properties: scan.production_facilities.clone(),
        ..Default::default()
    };
    scan.free_facilities = scan.production_facilities.clone();

    // 上下2翼に分かれた敵歩兵2体（いずれも1〜2手で到着可能）
    scan.enemy_units = vec![
        unit_snap(&avail, UnitType::Infantry, 5, 2, 100),
        unit_snap(&avail, UnitType::Infantry, 5, 9, 100),
    ];

    let (planned, _) = plan_unified_production(&scan, &extras, PlayerId(2), 2, true, &[]);
    let anchors: std::collections::HashSet<GridPosition> = planned
        .iter()
        .filter(|p| p.role == SlotKind::Combat)
        .filter_map(|p| p.deployment.as_ref().map(|d| d.anchor))
        .collect();
    assert!(
        anchors.len() >= 2,
        "複数生産する直接戦闘部隊は単一の敵へ集中せず複数前線へ分散配備されるべき: {:?}",
        planned
            .iter()
            .map(|p| (
                p.command.unit_type,
                p.role,
                p.deployment.as_ref().map(|d| d.anchor)
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn air_unit_stops_at_enemy_zoc_in_reachable_tiles() {
    let (_, md, _, avail) = load_full_combat_resources("map_1");
    let map = Map {
        width: 9,
        height: 3,
        tiles: vec![Terrain::Plains; 9 * 3],
        topology: GridTopology::Square,
    };
    let mut bcopters = unit_snap(&avail, UnitType::Bcopters, 1, 1, 100);
    bcopters.stats.max_movement = 5;
    // (3, 1) に敵がいるため (2, 1) と (4, 1) は敵 ZOC、さらに (3, 0) と (3, 2) も敵 ZOC
    let occ = super::reach::OccupancyGrid::from_units(&map, [], [pos(3, 1)]);
    let r1 = super::reach::reachable_tiles(
        &map,
        &md,
        &occ,
        pos(1, 1),
        MovementType::Air,
        bcopters.stats.max_movement,
        bcopters.stats.max_fuel,
        super::reach::OCC_FRIENDLY,
        1,
        None,
    );
    assert!(
        r1.contains(pos(2, 1)),
        "敵 ZOC の入口マス (2, 1) 自体は 1T で進入できる"
    );
    assert!(
        !r1.contains(pos(4, 1)),
        "航空ユニットであっても敵 ZOC を素通りして背後の (4, 1) へ 1T で抜けてはならない"
    );
}

#[test]
fn two_turn_reach_does_not_carry_over_leftover_mp_across_turn_boundary() {
    let (_, md, _, avail) = load_full_combat_resources("map_1");
    // 1行マップ: (0,0)平地, (1,0)森(コスト2), (2,0)平地(コスト1), (3,0)森(コスト2), (4,0)平地(コスト1)
    // Tank (mp=4): 平地=1, 森=2。
    // 単一パス max_mp=8 だと合計コスト 1+2+1+2+1=7<=8 で 2T 以内に (4,0) と先まで届くように見えるが、
    // 正しくは 1T 目に 森(2)+平地(1)=3MP しか使えず残り 1MP で 森(3,0)=コスト2 に踏み込めないため 1T 終了は (2,0)。
    // 2T 目に (2,0) から 森(2)+平地(1)=3MP で (4,0) が 2T 到達の限界であり、もし 森(4,0) なら 2T 目も届かない。
    let mut map = Map {
        width: 6,
        height: 1,
        tiles: vec![
            Terrain::Plains,
            Terrain::Forest,
            Terrain::Plains,
            Terrain::Forest,
            Terrain::Forest,
            Terrain::Plains,
        ],
        topology: GridTopology::Square,
    };
    let tank = unit_snap(&avail, UnitType::Tank, 0, 0, 100);
    let r2p = super::reach::reachable_pierce(
        &map,
        &md,
        pos(0, 0),
        tank.stats.movement_type,
        tank.stats.max_movement,
        tank.stats.max_fuel,
        2,
    );
    // 合計MPは 2+1+2+2 = 7 <= 8 だが、1T目で(2,0)止まり(残り1MP<2)、2T目で(3,0)(2MP)止まり(残り2MP=2で(4,0)可)、
    // (5,0)は合計コスト8だが2Tでは絶対に到達不能。
    assert!(r2p.contains(pos(4, 0)), "(4, 0) までは 2T 目で到達できる");
    assert!(
        !r2p.contains(pos(5, 0)),
        "総 MP コストが 8(=4*2) 以下であってもターン境界での端数 MP 切り捨てにより 2T 以内に (5, 0) へ届いてはならない"
    );

    // もう1例: 3ターンかかる経路を unit_turn_distance_field で検証
    map.set_terrain(4, 0, Terrain::Plains).unwrap();
    let df = super::reach::unit_turn_distance_field(
        &map,
        &md,
        pos(0, 0),
        tank.stats.movement_type,
        tank.stats.max_movement,
        tank.stats.max_fuel,
    );
    assert_eq!(df.get(pos(2, 0)), Some(1));
    assert_eq!(df.get(pos(3, 0)), Some(2));
}
