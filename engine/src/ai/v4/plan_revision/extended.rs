//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

/// 全体撃破へ未到達でも、有限の購入列が敵HPを実際に減らすなら再評価付きtrancheとして実行する。
///
/// beam探索を打ち切った候補は、残敵・必要額とも未確定である。部分的にHPを削れる
/// というだけで計画へ昇格させると、未探索の敵を含む巨額の予約を永続化してしまう。
pub(super) fn executable_plan(_kind: OperationKind, plan: &ForcePackagePlan) -> bool {
    if plan.feasible {
        return true;
    }
    !plan.search_truncated
        && !plan.purchases.is_empty()
        && plan
            .target_forecasts
            .iter()
            .any(|target| target.remaining_hp < target.initial_hp)
}

/// 作戦が予測した最初の交戦までに必要な購入だけが実体化したかを返す。
///
/// `scheduled_turn` がその時点より後のstepは、進軍中に到着する後続増援である。これを
/// 全軍の停止条件にすると、資金と工場が余っていても首都前で待ち続けてしまう。
pub(super) fn first_attack_wave_produced(plan: &StoredPlan) -> bool {
    let first_attack_turn = plan
        .execution
        .snapshot
        .planned_first_attack_turn
        .unwrap_or(plan.last_evaluated_turn);
    plan.steps
        .iter()
        .filter(|step| step.scheduled_turn <= first_attack_turn)
        .all(|step| step.status == PurchaseStatus::Produced)
}

impl V4RollingPlanRegistry {
    pub(crate) fn active_combat_plan_summaries_extended(
        &self,
        player_id: PlayerId,
    ) -> Vec<ActiveCombatPlanSummary> {
        self.active
            .iter()
            .filter(|plan| plan.player_id == player_id)
            .map(|plan| ActiveCombatPlanSummary {
                plan_id: plan.plan_id,
                kind: plan.kind,
                anchor: plan.anchor,
                objective_properties: plan.objective_properties.clone(),
                planned_elimination_turn: plan.execution.snapshot.planned_elimination_turn,
                remaining_target_count: plan.execution.snapshot.remaining_target_count,
            })
            .collect()
    }

    /// 現在盤面で組み直した首都計画が、最初の交戦波を発進できるかを返す。
    ///
    /// 後続増援の予約は戦力目標であって、前方DAGが開いた既存部隊を停止させる理由では
    /// ない。最初に攻撃を開始すると見積もった時点までに必要な購入だけを確認する。
    pub(crate) fn operation_launch_ready_extended(
        &self,
        player_id: PlayerId,
        island_id: IslandId,
    ) -> bool {
        self.active.iter().any(|plan| {
            plan.player_id == player_id
                && plan.kind == OperationKind::AssaultCapital
                && plan.island_id == Some(island_id)
                && plan.execution_authorized
                && plan.execution_ready
                && first_attack_wave_produced(plan)
        })
    }

    /// 最新revisionの敵集合を、既に生産済みの全Entityへ再配布する。
    pub(crate) fn active_deployment_intents_extended(
        &self,
        player_id: PlayerId,
        capital_assault_authorized: bool,
        capital_staging_anchor: Option<GridPosition>,
    ) -> Vec<ActiveDeploymentIntent> {
        self.active
            .iter()
            .filter(|plan| plan.player_id == player_id)
            .map(|plan| {
                let mut priority_enemies = plan.target_enemies.iter().copied().collect::<Vec<_>>();
                priority_enemies.sort_unstable_by_key(|entity| entity.to_bits());
                let first_attack_wave_ready = first_attack_wave_produced(plan);
                let posture = if plan.kind != OperationKind::AssaultCapital
                    || (capital_assault_authorized
                        && first_attack_wave_ready
                        && plan.execution_ready)
                {
                    super::super::deployment::DeploymentPosture::Execute
                } else {
                    super::super::deployment::DeploymentPosture::Forming
                };
                ActiveDeploymentIntent {
                    plan_id: plan.plan_id,
                    anchor: plan.anchor,
                    staging_anchor: capital_staging_anchor.unwrap_or(plan.anchor),
                    priority_enemies,
                    threat_horizon: plan
                        .execution
                        .snapshot
                        .planned_occupation_turn
                        .unwrap_or(plan.last_evaluated_turn)
                        .saturating_sub(plan.last_evaluated_turn)
                        .max(1),
                    posture,
                }
            })
            .collect()
    }

    pub(super) fn matching_index_extended(
        &self,
        player_id: PlayerId,
        kind: OperationKind,
        island_id: Option<IslandId>,
        objective_properties: &[GridPosition],
        _target_enemies: &HashSet<Entity>,
    ) -> Option<usize> {
        self.active
            .iter()
            .enumerate()
            .filter(|(_, plan)| plan.player_id == player_id)
            .find_map(|(index, plan)| {
                if let Some(island_id) = island_id {
                    return (plan.island_id == Some(island_id) && plan.kind == kind)
                        .then_some(index);
                }
                // 敵Entityは移動して別の島へ渡るため、敵の重複をPlan identityに使わない。
                // 同じ目的拠点集合だけを同一作戦のrevisionとして扱い、別島への変質を禁止する。
                (plan.island_id.is_none()
                    && plan.kind == kind
                    && plan.objective_properties.len() == objective_properties.len()
                    && plan
                        .objective_properties
                        .iter()
                        .all(|property| objective_properties.contains(property)))
                .then_some(index)
            })
    }
}

#[cfg(test)]
mod tests {
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
            HashSet::from([enemy]),
            true,
            None,
            feasible_plan(0),
            None,
            HashSet::new(),
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
        registry.reconcile_produced_steps(player, &HashSet::from([step]));

        assert!(registry.operation_launch_ready(player, island));
    }

    #[test]
    fn future_capital_reinforcement_does_not_block_first_attack_wave() {
        let player = PlayerId(1);
        let island = IslandId(7);
        let anchor = GridPosition { x: 20, y: 10 };
        let enemy = Entity::from_raw(10);
        let mut plan = feasible_plan(0);
        plan.purchases.push(PlannedPurchase {
            facility: GridPosition { x: 4, y: 3 },
            unit_type: UnitType::Tank,
            // 最初の攻撃予測（turn 5）より後の増援であり、首都前の部隊を待たせない。
            build_turn: 4,
            cost: 7_000,
        });
        plan.production_cost = 14_500;
        let mut registry = V4RollingPlanRegistry::default();
        let selected = registry.select_for_operation(
            player,
            3,
            OperationKind::AssaultCapital,
            Some(island),
            anchor,
            vec![anchor],
            HashSet::from([enemy]),
            true,
            None,
            plan,
            None,
            HashSet::new(),
        );
        let first_step = registry
            .current_step_ref(
                selected.plan_id.expect("首都Plan"),
                selected.revision.expect("revision"),
                3,
                selected.plan.purchases[0],
            )
            .expect("第一波の当手番step");
        registry.reconcile_produced_steps(player, &HashSet::from([first_step]));

        assert!(
            registry.operation_launch_ready(player, island),
            "最初の攻撃波を揃えた後は、後続Tankの予約で首都作戦をFormingへ戻さない"
        );
        assert!(
            registry.active[0]
                .steps
                .iter()
                .any(|step| step.status != PurchaseStatus::Produced),
            "後続増援は未生産のまま残り、予約自体は維持する"
        );
    }

    #[test]
    fn island_operation_identity_keeps_kind_but_allows_anchor_property_and_enemy_changes() {
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
            HashSet::from([enemy]),
            true,
            None,
            feasible_plan(0),
            None,
            HashSet::new(),
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
                    &HashSet::from([reinforcement]),
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
            HashSet::from([reinforcement]),
            true,
            None,
            feasible_plan(0),
            None,
            HashSet::new(),
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
                &HashSet::from([reinforcement]),
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
                    &HashSet::from([enemy]),
                )
                .is_none(),
            "座標が偶然重なっても別島へPlanを流用しない"
        );
    }
}
