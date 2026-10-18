//! 拡張経路（小規模マップ戦略）で検証する計画revisionのテスト。

use super::*;

fn feasible_plan(build_turn: u32) -> ForcePackagePlan {
    ForcePackagePlan {
        purchases: vec![PlannedPurchase {
            facility: GridPosition { x: 2, y: 3 },
            unit_type: UnitType::Bcopters,
            build_turn,
            cost: 7_500,
        }],
        capture_purchases: Vec::new(),
        combat_purchases: Vec::new(),
        target_forecasts: Vec::new(),
        turn_forecasts: Vec::new(),
        feasible: true,
        first_attack_turn: Some(build_turn.saturating_add(2)),
        front_breakthrough_turn: Some(build_turn.saturating_add(2)),
        deadline_target_first_attack_turn: None,
        target_first_attack_turns: Vec::new(),
        elimination_turn: Some(build_turn.saturating_add(3)),
        occupation_turn: Some(build_turn.saturating_add(5)),
        production_cost: 7_500,
        expected_loss: 1_000,
        blocked_next_turn_production_slots: 0,
        direct_fire_lane_count: 0,
        direct_front_shortfall: 0,
        direct_enemy_remaining_hp: Vec::new(),
        direct_damage_by_target: Vec::new(),
        existing_indirect_damage_by_target: Vec::new(),
        produced_indirect_damage_by_target: Vec::new(),
        surviving_combat_value: 6_500,
        required_overmatch_value: 0,
        overmatch_ready: true,
        protected_unit_count: 0,
        protected_survivor_count: 0,
        deadline_capture_survivor_count: 0,
        required_capture_survivor_count: 0,
        candidates_considered: 1,
        candidates_pruned: 0,
        search_truncated: false,
    }
}

#[test]
fn capital_launch_gate_opens_after_first_attack_wave_is_produced() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let island = IslandId(7);
    let anchor = GridPosition { x: 20, y: 10 };
    let enemy = Entity::from_raw(10);
    let mut registry = V4RollingPlanRegistry::default();
    let selected = registry.select_for_operation(
        player,
        3,
        OperationKind::AssaultCapital,
        Some(island),
        anchor,
        vec![anchor],
        HashSet::from_iter([enemy]),
        true,
        None,
        feasible_plan(0),
        None,
        HashSet::default(),
    );
    assert!(!registry.operation_launch_ready(player, island));

    let step = registry
        .current_step_ref(
            selected.plan_id.expect("首都Plan"),
            selected.revision.expect("revision"),
            3,
            selected.plan.purchases[0],
        )
        .expect("当手番の生産step");
    registry.reconcile_produced_steps(player, &HashSet::from_iter([step]));

    assert!(registry.operation_launch_ready(player, island));
}

/// 初撃波だけ完成し後続増援が未完成のとき、首都Planはどちらの経路でも出撃する。
/// 全購入の完成まで集結を続けると、生産済みの戦力が集結地点で滞留する。
#[test]
fn capital_launch_posture_departs_with_the_first_attack_wave_on_both_paths() {
    use super::super::deployment::DeploymentPosture;

    let player = PlayerId(1);
    let island = IslandId(7);
    let anchor = GridPosition { x: 20, y: 10 };
    let mut plan = feasible_plan(0);
    // 初撃（turn 2）より後に到着する後続増援。
    plan.purchases.push(PlannedPurchase {
        facility: GridPosition { x: 3, y: 3 },
        unit_type: UnitType::Bcopters,
        build_turn: 4,
        cost: 7_500,
    });
    for (path, expected) in [
        (
            crate::ai::code_path::CodePath::MainCompatible,
            DeploymentPosture::Execute,
        ),
        (
            crate::ai::code_path::CodePath::Extended,
            DeploymentPosture::Execute,
        ),
    ] {
        let _guard = crate::ai::code_path::enter(path);
        let mut registry = V4RollingPlanRegistry::default();
        let selected = registry.select_for_operation(
            player,
            3,
            OperationKind::AssaultCapital,
            Some(island),
            anchor,
            vec![anchor],
            HashSet::from_iter([Entity::from_raw(10)]),
            true,
            None,
            plan.clone(),
            None,
            HashSet::default(),
        );
        let first_wave = registry
            .current_step_ref(
                selected.plan_id.expect("首都Plan"),
                selected.revision.expect("revision"),
                3,
                selected.plan.purchases[0],
            )
            .expect("当手番の生産step");
        registry.reconcile_produced_steps(player, &HashSet::from_iter([first_wave]));

        let intents = registry.active_deployment_intents(player, true, None);
        assert_eq!(intents.len(), 1, "{path:?}");
        assert_eq!(intents[0].posture, expected, "{path:?}");
    }
}

#[test]
fn island_operation_identity_keeps_kind_but_allows_anchor_property_and_enemy_changes() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let player = PlayerId(1);
    let island = IslandId(3);
    let first_anchor = GridPosition { x: 14, y: 15 };
    let remaining_city = GridPosition { x: 12, y: 16 };
    let other_island = IslandId(4);
    let enemy = Entity::from_raw(10);
    let reinforcement = Entity::from_raw(11);
    let mut registry = V4RollingPlanRegistry::default();

    let created = registry.select_for_operation(
        player,
        3,
        OperationKind::Capture,
        Some(island),
        first_anchor,
        vec![first_anchor, remaining_city],
        HashSet::from_iter([enemy]),
        true,
        None,
        feasible_plan(0),
        None,
        HashSet::default(),
    );
    let plan_id = created.plan_id.expect("島campaignは永続Planを持つ");

    assert!(
        registry
            .continuation_for_operation(
                player,
                4,
                OperationKind::Defense,
                Some(island),
                &[remaining_city],
                &HashSet::from_iter([reinforcement]),
            )
            .is_none(),
        "同じ島でもDefenseへCaptureのPlanIdを流用しない"
    );
    let defense = registry.select_for_operation(
        player,
        4,
        OperationKind::Defense,
        Some(island),
        remaining_city,
        vec![remaining_city],
        HashSet::from_iter([reinforcement]),
        true,
        None,
        feasible_plan(0),
        None,
        HashSet::default(),
    );
    assert_ne!(
        defense.plan_id,
        Some(plan_id),
        "同一島の別作戦は独立したPlanIdを持つ"
    );
    assert_eq!(registry.active.len(), 2);
    let continuation = registry
        .continuation_for_operation(
            player,
            4,
            OperationKind::Capture,
            Some(island),
            &[remaining_city],
            &HashSet::from_iter([reinforcement]),
        )
        .expect("同じ島・同じ作戦ならanchor・残施設・敵が変わっても継続する");
    assert_eq!(continuation.plan_id, plan_id);
    assert!(
        registry
            .continuation_for_operation(
                player,
                4,
                OperationKind::Capture,
                Some(other_island),
                &[remaining_city],
                &HashSet::from_iter([enemy]),
            )
            .is_none(),
        "座標が偶然重なっても別島へPlanを流用しない"
    );
}
