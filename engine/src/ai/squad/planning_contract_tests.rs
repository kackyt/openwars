//! F07の通常／拡張hookと共通オーケストレーションの境界を固定する。

use crate::ai::planning::decision::AdoptedTurnPortfolio;
use super::*;
use crate::ai::ai_version::{AiVersion, PlayerAiSettings};
use crate::ai::island_campaign::{
    IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignPortfolio,
    IslandCampaignRequirement,
};
use crate::ai::islands::{IslandId, IslandMap};
use crate::ai::strategy_profile::{V4StrategyProfile, V4StrategyProfileRegistry};
use crate::resources::MovementType;
use std::borrow::Cow;

fn assignment(target: GridPosition) -> IslandCampaignAssignment {
    let requirement = IslandCampaignRequirement {
        preferred_transport: None,
        transport_slots: 0,
        capture_units: 0,
        ground_combat_units: 0,
        combat_units: 0,
        total_budget: 0,
    };
    IslandCampaignAssignment {
        island_id: IslandId(0),
        decision: IslandCampaignDecision::Secure,
        target_position: target,
        capture_target_positions: vec![target],
        priority_enemy_types: Vec::new(),
        requirement: requirement.clone(),
        purchase_shortfall: requirement,
        allocated_budget: 0,
        transport_entities: Vec::new(),
        capture_entities: Vec::new(),
        combat_entities: Vec::new(),
        operation_ready: true,
        continued_from_existing_squad: false,
    }
}

#[test]
fn capture_hooks_keep_main_input_order_and_extended_distance_order() {
    let mut world = super::tests::setup_test_world();
    let player = PlayerId(1);
    world.insert_resource(V4StrategyProfileRegistry::with_profile(
        player,
        V4StrategyProfile::SmallMapExpansion,
    ));
    let map = world.resource::<Map>().clone();
    let registry = world.resource::<MasterDataRegistry>().clone();
    let islands = world.resource::<IslandMap>().clone();
    let stats = UnitStats {
        movement_type: MovementType::Infantry,
        ..UnitStats::mock()
    };
    let infantry = vec![
        (
            Entity::from_raw(9),
            GridPosition { x: 0, y: 0 },
            stats.clone(),
        ),
        (
            Entity::from_raw(8),
            GridPosition { x: 7, y: 0 },
            stats.clone(),
        ),
        (Entity::from_raw(2), GridPosition { x: 7, y: 0 }, stats),
    ];
    let context = CapturePlanningContext {
        map: &map,
        registry: &registry,
        island_map: &islands,
        infantry: &infantry,
        capital: Some(GridPosition { x: 0, y: 0 }),
        is_v3: false,
    };
    let target = GridPosition { x: 9, y: 0 };
    assert_eq!(
        MainPlanningHooks::select_capture_infantry(&context, IslandId(0), &target),
        Some(0)
    );
    assert_eq!(
        extended::ExtendedPlanningHooks::select_capture_infantry(&context, IslandId(0), &target),
        Some(2)
    );
    let original = vec![
        GridPosition { x: 5, y: 0 },
        GridPosition { x: 3, y: 0 },
        GridPosition { x: 8, y: 0 },
    ];
    // 目標順位の差は歩兵が自首都付近だけにいる盤面で検査する。
    let sorting_context = CapturePlanningContext {
        infantry: &infantry[..1],
        ..context
    };
    let mut main_targets = original.clone();
    MainPlanningHooks::sort_capture_targets(
        &mut world,
        player,
        &mut main_targets,
        &sorting_context,
    );
    assert_eq!(
        main_targets.iter().map(|p| p.x).collect::<Vec<_>>(),
        vec![3, 5, 8]
    );
    let mut extended_targets = original.clone();
    extended::ExtendedPlanningHooks::sort_capture_targets(
        &mut world,
        player,
        &mut extended_targets,
        &sorting_context,
    );
    assert_eq!(
        extended_targets.iter().map(|p| p.x).collect::<Vec<_>>(),
        vec![8, 5, 3]
    );
    let no_capital = CapturePlanningContext {
        capital: None,
        ..sorting_context
    };
    extended_targets = original.clone();
    extended::ExtendedPlanningHooks::sort_capture_targets(
        &mut world,
        player,
        &mut extended_targets,
        &no_capital,
    );
    assert_eq!(
        extended_targets, original,
        "小規模でも自首都なしでは入力順を保持"
    );
    world.insert_resource(V4StrategyProfileRegistry::with_profile(
        player,
        V4StrategyProfile::Standard,
    ));
    extended::ExtendedPlanningHooks::sort_capture_targets(
        &mut world,
        player,
        &mut extended_targets,
        &no_capital,
    );
    assert_eq!(
        extended_targets, main_targets,
        "標準profileなら拡張hookも通常sortを使う"
    );
}

#[test]
fn portfolio_and_capital_defense_hooks_keep_model_specific_behavior() {
    let mut world = super::tests::setup_test_world();
    let manager = SquadManager::new();
    let portfolio = IslandCampaignPortfolio::default();
    assert!(matches!(
        MainPlanningHooks::portfolio_for_planning(
            &mut world,
            PlayerId(1),
            &manager,
            &portfolio,
            true
        ),
        Cow::Borrowed(_)
    ));
    assert!(matches!(
        extended::ExtendedPlanningHooks::portfolio_for_planning(
            &mut world,
            PlayerId(1),
            &manager,
            &portfolio,
            false
        ),
        Cow::Owned(_)
    ));
    for is_v4 in [false, true] {
        assert!(MainPlanningHooks::uses_legacy_capital_defense(is_v4));
        assert_eq!(
            extended::ExtendedPlanningHooks::uses_legacy_capital_defense(is_v4),
            !is_v4
        );
    }
}

#[test]
fn generic_transport_uses_source_portfolio_and_preserves_property_fallback() {
    let player = PlayerId(1);
    let source_target = GridPosition { x: 2, y: 0 };
    let adopted_target = GridPosition { x: 4, y: 0 };
    let source = IslandCampaignPortfolio {
        active_offensives: vec![assignment(source_target)],
        ..Default::default()
    };
    let adopted = IslandCampaignPortfolio {
        active_offensives: vec![assignment(adopted_target)],
        ..Default::default()
    };
    let properties = HashMap::from_iter([(IslandId(0), vec![adopted_target, source_target])]);
    let ownership = HashMap::from_iter([(source_target, Some(player))]);
    assert_ne!(source, adopted);
    assert_eq!(
        resolve_generic_transport_target(&source, IslandId(0), player, &properties, &ownership),
        Some(source_target)
    );
    assert_eq!(
        resolve_generic_transport_target(
            &IslandCampaignPortfolio::default(),
            IslandId(0),
            player,
            &properties,
            &ownership
        ),
        Some(adopted_target)
    );
}

#[derive(Resource, Default)]
struct Stages(Vec<&'static str>);
#[derive(Resource, Clone)]
struct SelectedPortfolio(IslandCampaignPortfolio);
#[derive(Resource)]
struct OriginalPortfolio(IslandCampaignPortfolio);
struct RecordingHooks;
impl SquadPlanningHooks for RecordingHooks {
    fn portfolio_for_planning<'a>(
        world: &mut World,
        _player: PlayerId,
        _manager: &SquadManager,
        portfolio: &'a IslandCampaignPortfolio,
        _is_v4: bool,
    ) -> Cow<'a, IslandCampaignPortfolio> {
        assert!(!world.contains_resource::<SquadManager>());
        world.resource_mut::<Stages>().0.push("portfolio");
        world.insert_resource(OriginalPortfolio(portfolio.clone()));
        Cow::Owned(world.resource::<SelectedPortfolio>().0.clone())
    }
    fn before_final_reconcile(
        world: &mut World,
        _player: PlayerId,
        _portfolio: &IslandCampaignPortfolio,
        _manager: &SquadManager,
        _is_v4: bool,
    ) {
        assert!(!world.contains_resource::<SquadManager>());
        world.resource_mut::<Stages>().0.push("before_reconcile");
        world
            .resource_mut::<UnitOperationRegistry>()
            .note_reconcile_visits(usize::MAX);
    }
    fn after_final_reconcile(
        world: &mut World,
        _player: PlayerId,
        _portfolio: &IslandCampaignPortfolio,
        _manager: &mut SquadManager,
        _is_v4: bool,
    ) {
        assert!(!world.contains_resource::<SquadManager>());
        assert_ne!(
            world
                .resource::<UnitOperationRegistry>()
                .last_reconcile_visits(),
            usize::MAX
        );
        world.resource_mut::<Stages>().0.push("after_reconcile");
    }
}

#[test]
fn shared_planning_keeps_manager_lifetime_and_adopted_portfolio_cache() {
    let mut world = super::tests::setup_test_world();
    let player = PlayerId(1);
    let mut settings = PlayerAiSettings::new();
    settings.set_version(player, AiVersion::V3);
    world.insert_resource(settings);
    let selected = IslandCampaignPortfolio {
        active_offensives: vec![assignment(GridPosition { x: 2, y: 0 })],
        ..Default::default()
    };
    world.insert_resource(SelectedPortfolio(selected.clone()));
    world.insert_resource(Stages::default());
    plan_squads_with_hooks::<RecordingHooks>(&mut world, player);
    assert_eq!(
        world.resource::<Stages>().0,
        vec![
            "portfolio",
            "before_reconcile",
            "after_reconcile"
        ]
    );
    assert!(world.contains_resource::<SquadManager>());
    assert_ne!(world.resource::<OriginalPortfolio>().0, selected);
    assert_eq!(
        world
            .resource::<AdoptedTurnPortfolio>()
            .campaign_portfolio(player),
        Some(&selected)
    );
}

fn v4_small_world() -> World {
    let mut world = super::tests::setup_test_world();
    let mut settings = PlayerAiSettings::new();
    settings.set_version(PlayerId(1), AiVersion::V4);
    world.insert_resource(settings);
    world.insert_resource(V4StrategyProfileRegistry::with_profile(
        PlayerId(1),
        V4StrategyProfile::SmallMapExpansion,
    ));
    world
}

/// 計画入口と局地割当は、実行中の経路（または拡張hookの直接呼び出し）だけで
/// 通常／拡張の実装が切り替わる。委譲点ごとの無効化スイッチは段階7で廃止した。
#[test]
fn planning_entry_and_local_assignment_follow_code_path() {
    for path in [
        crate::ai::code_path::CodePath::MainCompatible,
        crate::ai::code_path::CodePath::Extended,
    ] {
        for direct_extended in [false, true] {
            let _guard = crate::ai::code_path::enter(path);
            let mut world = v4_small_world();
            if direct_extended {
                extended::plan_squads(&mut world, PlayerId(1));
            } else {
                plan_squads(&mut world, PlayerId(1));
            }
            let expects_extended =
                direct_extended || path == crate::ai::code_path::CodePath::Extended;
            assert_eq!(
                world.contains_resource::<crate::ai::v4::CapitalRoutePathRegistry>(),
                expects_extended,
                "plan入口: path={path:?}, direct={direct_extended}"
            );

            // Secureの戦闘責務追加は拡張の局地割当だけが行う。
            let mut world = v4_small_world();
            let registry = world.resource::<MasterDataRegistry>();
            let combat_stats = registry
                .create_unit_stats(&crate::resources::master_data::UnitName(
                    UnitType::Bcopters.as_str().to_owned(),
                ))
                .unwrap();
            let enemy_stats = registry
                .create_unit_stats(&crate::resources::master_data::UnitName(
                    UnitType::Infantry.as_str().to_owned(),
                ))
                .unwrap();
            let combat = world
                .spawn((
                    Faction(PlayerId(1)),
                    GridPosition { x: 1, y: 0 },
                    combat_stats,
                    Health {
                        current: 100,
                        max: 100,
                    },
                ))
                .id();
            world.spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 3, y: 0 },
                enemy_stats,
                Health {
                    current: 100,
                    max: 100,
                },
            ));
            let mut local = assignment(GridPosition { x: 3, y: 0 });
            local.combat_entities.push(combat);
            let mut manager = SquadManager::new();
            let mut connectivity = TerrainConnectivity::default();
            if direct_extended {
                extended::ExtendedPlanningHooks::prepare_local_assignment(
                    &world,
                    &mut manager,
                    PlayerId(1),
                    &local,
                    &mut connectivity,
                );
            } else {
                MainPlanningHooks::prepare_local_assignment(
                    &world,
                    &mut manager,
                    PlayerId(1),
                    &local,
                    &mut connectivity,
                );
            }
            let expects_combat =
                direct_extended || path == crate::ai::code_path::CodePath::Extended;
            assert_eq!(
                manager.squads.iter().any(|s| s.members.contains(&combat)),
                expects_combat,
                "局地割当: path={path:?}, direct={direct_extended}"
            );
        }
    }
}
