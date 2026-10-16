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
    assert!(BoardScan::collect(&world, player, None, &queries).is_none());
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
    let scan = BoardScan::collect(&world, player, Some(&prepared), &queries).unwrap();
    assert_eq!(scan.opening_policy, Some(prepared.opening_policy));
    assert_eq!(world.entities().len(), entities_before);
    assert_eq!(
        super::super::capital_route::commitment::capital_route_diagnostics_for_player(
            &world, player
        ),
        before
    );
}
