use super::*;
use crate::ai::v4::capital_route::model::CapitalRouteTopologyRegistry;
use crate::resources::GridTopology;
use bevy_ecs::prelude::Component;

/// sparse markerはarchetypeだけを分け、dense Queryが読むtableは共有する。
#[derive(Component)]
#[component(storage = "SparseSet")]
struct SparseMarker;

#[test]
fn readonly_rows_keep_dense_query_order_across_sparse_archetypes() {
    let mut world = World::new();
    let first = world
        .spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::City, None, 100),
            Faction(PlayerId(1)),
            UnitStats::mock(),
        ))
        .id();
    world.spawn((
        GridPosition { x: 1, y: 0 },
        Property::new(Terrain::City, None, 100),
        Faction(PlayerId(2)),
        UnitStats::mock(),
    ));
    // 異なるtableと同一table内の別archetypeを両方含める。
    world.spawn((
        GridPosition { x: 2, y: 0 },
        Property::new(Terrain::Factory, None, 100),
    ));
    world.entity_mut(first).insert(SparseMarker);
    // 多数のtableがあるとcomponent indexの順序がtable ID順と一致しなくなる。
    for mask in 0_u32..16 {
        let mut entity = world.spawn((
            GridPosition {
                x: mask as usize + 3,
                y: 0,
            },
            Property::new(Terrain::City, None, 100),
            Faction(PlayerId(1)),
            UnitStats::mock(),
        ));
        if mask & 1 != 0 {
            entity.insert(Health {
                current: 100,
                max: 100,
            });
        }
        if mask & 2 != 0 {
            entity.insert(CargoCapacity {
                max: 0,
                loaded: Vec::new(),
            });
        }
        if mask & 4 != 0 {
            entity.insert(crate::components::HasMoved(false));
        }
        if mask & 8 != 0 {
            entity.insert(crate::components::ActionCompleted(false));
        }
    }
    let expected_properties = world
        .query::<(Entity, &GridPosition, &Property)>()
        .iter(&world)
        .map(|(entity, _, _)| entity)
        .collect::<Vec<_>>();
    let expected_units = world
        .query::<(
            Entity,
            &GridPosition,
            &Faction,
            &UnitStats,
            Option<&Health>,
            Option<&CargoCapacity>,
            Option<&Transporting>,
        )>()
        .iter(&world)
        .map(|(entity, ..)| entity)
        .collect::<Vec<_>>();
    let queries = BoardScanQueries::prepare(&mut world);
    assert_eq!(
        property_rows(&world, &queries)
            .map(|row| row.entity)
            .collect::<Vec<_>>(),
        expected_properties
    );
    assert_eq!(
        unit_rows(&world, &queries)
            .map(|row| row.entity)
            .collect::<Vec<_>>(),
        expected_units
    );
}

#[test]
fn small_map_snapshot_requires_preparation_and_never_initializes_strategy() {
    let _path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let registry = MasterDataRegistry::load().unwrap();
    let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
        &registry,
        "map_1",
        GridTopology::Hex,
    )
    .unwrap();
    let player = PlayerId(1);
    let mut settings = crate::ai::PlayerAiSettings::new();
    settings.set_version(player, crate::ai::AiVersion::V4);
    world.insert_resource(settings);
    assert!(!world.contains_resource::<CapitalRouteTopologyRegistry>());
    // 準備なしのsnapshotが隠れた解析・DAG準備を実行しない。
    let queries = BoardScanQueries::prepare(&mut world);
    assert!(
        BoardScan::collect(
            &world,
            player,
            None,
            &queries,
            ProductionPolicy::for_current_path()
        )
        .is_none()
    );
    assert!(!world.contains_resource::<CapitalRouteTopologyRegistry>());
    let prepared = strategy_pipeline::prepare_production_scan(&mut world, player).unwrap();
    assert!(
        prepared.logistics_plan.is_none(),
        "解析前に未作成だった兵站計画を今回の観測として保持する"
    );
    assert!(world.contains_resource::<logistics_plan::V4LogisticsPlanRegistry>());
    let before = super::super::capital_route::commitment::capital_route_diagnostics_for_player(
        &world, player,
    );
    let entities_before = world.entities().len();
    let queries = BoardScanQueries::prepare(&mut world);
    let scan = BoardScan::collect(
        &world,
        player,
        Some(&prepared),
        &queries,
        ProductionPolicy::for_current_path(),
    )
    .unwrap();
    assert_eq!(scan.opening_policy, Some(prepared.opening_policy));
    assert_eq!(world.entities().len(), entities_before);
    assert_eq!(
        super::super::capital_route::commitment::capital_route_diagnostics_for_player(
            &world, player
        ),
        before
    );
}

#[test]
fn capital_assault_authorization_pure_logic_verifies_geography_and_logistics() {
    let map = Map::new(10, 10, Terrain::Plains, GridTopology::Square);
    let island_map = crate::ai::islands::IslandMap::analyze(&map);
    let home_pos = GridPosition { x: 0, y: 0 };
    let enemy_capital_pos = GridPosition { x: 8, y: 0 };
    let home_island = island_map.get_island_at(&home_pos).unwrap().id;
    let route_dist = map.distance(
        home_pos.x,
        home_pos.y,
        enemy_capital_pos.x,
        enemy_capital_pos.y,
    );
    assert_eq!(route_dist, 8);

    // 1. 同一島: 所有数が劣勢または同数のときは認可されない
    let owned = [GridPosition { x: 4, y: 0 }];
    let enemy = [GridPosition { x: 6, y: 0 }, GridPosition { x: 7, y: 0 }];
    assert!(!is_capital_assault_authorized(
        CapitalAssaultGeography {
            home_island: Some(home_island),
            capital_island: home_island,
            home_pos: Some(home_pos),
            enemy_capital_pos: Some(enemy_capital_pos),
        },
        &map,
        &island_map,
        owned.iter(),
        enemy.iter(),
        false,
    ));

    // 2. 同一島: 所有数は優位だが、戦線が中間地点まで進出していないときは認可されない
    // forward_dist = 6 (4マス離れているが首都からは6), 6 * 2 = 12 > 8
    let owned_back = [GridPosition { x: 2, y: 0 }, GridPosition { x: 1, y: 0 }];
    let enemy_single = [GridPosition { x: 7, y: 0 }];
    assert!(!is_capital_assault_authorized(
        CapitalAssaultGeography {
            home_island: Some(home_island),
            capital_island: home_island,
            home_pos: Some(home_pos),
            enemy_capital_pos: Some(enemy_capital_pos),
        },
        &map,
        &island_map,
        owned_back.iter(),
        enemy_single.iter(),
        false,
    ));

    // 3. 同一島: 所有数優位 かつ 前線が中間地点（forward_dist * 2 <= 8）まで進出しているときは認可される
    // forward_dist = 4 (x=4の位置), 4 * 2 = 8 <= 8
    let owned_front = [GridPosition { x: 4, y: 0 }, GridPosition { x: 1, y: 0 }];
    assert!(is_capital_assault_authorized(
        CapitalAssaultGeography {
            home_island: Some(home_island),
            capital_island: home_island,
            home_pos: Some(home_pos),
            enemy_capital_pos: Some(enemy_capital_pos),
        },
        &map,
        &island_map,
        owned_front.iter(),
        enemy_single.iter(),
        false,
    ));

    // 4. 別島（渡海）: 兵站路未完了なら認可されない
    let other_island = IslandId(2);
    assert!(!is_capital_assault_authorized(
        CapitalAssaultGeography {
            home_island: Some(home_island),
            capital_island: other_island,
            home_pos: Some(home_pos),
            enemy_capital_pos: Some(enemy_capital_pos),
        },
        &map,
        &island_map,
        owned_front.iter(),
        enemy_single.iter(),
        false,
    ));

    // 5. 別島（渡海）: 兵站路完了なら認可される
    assert!(is_capital_assault_authorized(
        CapitalAssaultGeography {
            home_island: Some(home_island),
            capital_island: other_island,
            home_pos: Some(home_pos),
            enemy_capital_pos: Some(enemy_capital_pos),
        },
        &map,
        &island_map,
        owned_front.iter(),
        enemy_single.iter(),
        true,
    ));
}
