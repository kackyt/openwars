//! 生産判断のローリング戦闘計画への入力構築。

use crate::ai::v4::*;

/// 観測敵と到着しうる増援を排除できる混成生産列を、探索期間の全生産slotから計画する。
///
/// `combat_plan_required`は呼び出し条件にだけ残し、候補数・生産停止・完了判定には使わない。
/// 既存unitと同じ手番に発注済みのunitを初期編成へ入れ、敵EntityのHPが0になるまで
/// ターン単位で攻撃を進めた結果から必要な購入だけを返す。
#[derive(Clone, Copy)]
struct CombatInputs<'a> {
    scan: &'a BoardScan,
    op: &'a Operation,
    used_facilities: &'a HashSet<GridPosition>,
    committed_combat_entities: &'a HashSet<Entity>,
    remaining_funds: u32,
    require_self_deployment: bool,
}

/// 構築途中の入力。対象・既存戦力・購入候補をこの順で確定する。
struct CombatDraft {
    enemies: Vec<EnemyPlanUnit>,
    hard_deadline: Option<u32>,
    capture_completion_turn: Option<u32>,
    required_capture_survivors: usize,
    existing_units: Vec<FriendlyPlanUnit>,
    protected_units: Vec<FriendlyPlanUnit>,
    options: Vec<rolling_plan::ProductionPlanOption>,
    production_attack_projections: Vec<Vec<Option<ProductionAttackProjection>>>,
}
#[allow(clippy::too_many_arguments)]
pub(in crate::ai::v4) fn combat_plan_input(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    used_facilities: &HashSet<GridPosition>,
    committed_combat_entities: &HashSet<Entity>,
    remaining_funds: u32,
    require_self_deployment: bool,
) -> Option<RollingPlanInput> {
    let input = CombatInputs {
        scan,
        op,
        used_facilities,
        committed_combat_entities,
        remaining_funds,
        require_self_deployment,
    };
    let mut draft = collect_combat_targets(&input, ctx)?;
    collect_existing_force(&input, ctx, &mut draft);
    collect_production_options(&input, ctx, &mut draft);
    finish_combat_input(&input, draft)
}

fn collect_combat_targets(input: &CombatInputs<'_>, _ctx: &mut ReachCtx) -> Option<CombatDraft> {
    let CombatInputs { scan, op, .. } = *input;

    let enemies: Vec<_> = op
        .reachable_threats
        .iter()
        .filter(|threat| threat.current_hp > 0)
        .map(|threat| {
            let terrain = scan
                .map
                .get_terrain(threat.position.x, threat.position.y)
                .unwrap_or(Terrain::Plains);
            EnemyPlanUnit {
                entity: threat.entity,
                stats: threat.stats.clone(),
                position: threat.position,
                hp: threat.current_hp,
                defense_bonus: scan.master_data.get_terrain_defense_bonus(terrain),
                available_turn: threat.available_turn,
            }
        })
        .collect();
    let hard_deadline =
        if op.kind == OperationKind::Defense && op.facts.enemy_contact_eta != u32::MAX {
            Some(op.facts.enemy_contact_eta.max(1))
        } else {
            None
        };
    let campaign_objective = scan.campaign_objectives.iter().find(|objective| {
        objective.anchor == op.anchor || op.objective_properties.contains(&objective.anchor)
    });
    let capture_completion_turn = campaign_objective.and_then(|objective| objective.capture_eta);
    let required_capture_survivors = campaign_objective.map_or_else(
        || {
            op.objective_properties
                .iter()
                .filter(|property| scan.open_properties.contains(property))
                .count()
        },
        |objective| objective.required_capture_survivors,
    );
    let mut enemies = enemies;
    enemies.extend(op.unavoidable_reinforcements.iter().cloned());
    if enemies.is_empty() {
        return None;
    }

    Some(CombatDraft {
        enemies,
        hard_deadline,
        capture_completion_turn,
        required_capture_survivors,
        existing_units: Vec::new(),
        protected_units: Vec::new(),
        options: Vec::new(),
        production_attack_projections: Vec::new(),
    })
}

fn collect_existing_force(input: &CombatInputs<'_>, ctx: &mut ReachCtx, draft: &mut CombatDraft) {
    let CombatInputs {
        scan,
        op,
        committed_combat_entities,
        ..
    } = *input;
    let enemies = &draft.enemies;
    let mut capture_completion_turn = draft.capture_completion_turn;
    let required_capture_survivors = draft.required_capture_survivors;
    let mut existing_units = Vec::new();
    for unit in &scan.my_units {
        if unit.stats.max_cargo > 0 {
            continue;
        }
        if !unit
            .entity
            .is_some_and(|entity| committed_combat_entities.contains(&entity))
        {
            continue;
        }
        let engageable_enemy_indices = enemies
            .iter()
            .enumerate()
            .filter_map(|(index, enemy)| {
                (best_damage(
                    &scan.damage_chart,
                    unit.stats.unit_type,
                    enemy.stats.unit_type,
                ) > 0
                    && ctx.can_reach_engagement_envelope(
                        &scan.map,
                        &scan.master_data,
                        (unit.pos.x, unit.pos.y),
                        (enemy.position.x, enemy.position.y),
                        unit.stats.movement_type,
                        unit.stats.max_range,
                    ))
                .then_some(index)
            })
            .collect::<Vec<_>>();
        if engageable_enemy_indices.is_empty() {
            continue;
        }
        existing_units.push(FriendlyPlanUnit {
            protection_completion_turn: None,
            stats: unit.stats.clone(),
            position: unit.pos,
            hp: unit.hp,
            available_turn: 0,
            engageable_enemy_indices,
        });
    }
    let mut protected_units = scan
        .my_units
        .iter()
        .filter(|unit| {
            unit.entity
                .is_some_and(|entity| op.protected_capture_entities.contains(&entity))
        })
        .map(|unit| FriendlyPlanUnit {
            protection_completion_turn: None,
            stats: unit.stats.clone(),
            position: unit.pos,
            hp: unit.hp,
            available_turn: 0,
            // 保護対象は攻撃要員として二重計上しないため空にする。
            engageable_enemy_indices: Vec::new(),
        })
        .collect::<Vec<_>>();

    // この生産判断内で先に発注した専任CaptureだけはまだEntityになっていないため、
    // 実行中の保護対象へ投影する。将来手番の仮想Captureはここでは予約しない。
    // Combat枠で選ぶ歩兵・重歩兵も生存占領能力へ数えるため、将来費用を先取りせず
    // 現在の空き施設・現金を前線戦力へ使える。
    let missing_protected_units = required_capture_survivors.saturating_sub(protected_units.len());
    let newly_ordered_capture_units = usize::try_from(op.filled.capture_units)
        .unwrap_or(usize::MAX)
        .saturating_sub(protected_units.len())
        .min(missing_protected_units);
    let reference_capture = scan.reference_capture_unit().cloned();
    if let Some(reference_capture) = reference_capture.as_ref() {
        let targets = if op.objective_properties.is_empty() {
            std::slice::from_ref(&op.anchor)
        } else {
            op.objective_properties.as_slice()
        };
        for index in 0..newly_ordered_capture_units {
            let target = targets[index % targets.len()];
            let movement_turns = if op.facts.requires_transport {
                op.facts.deploy_lead_time.max(1)
            } else {
                scan.production_facilities
                    .iter()
                    .map(|(facility, _)| {
                        eta_turns(&scan.map, facility, &target, reference_capture.max_movement)
                    })
                    .min()
                    .unwrap_or(op.facts.deploy_lead_time)
            };
            protected_units.push(FriendlyPlanUnit {
                protection_completion_turn: None,
                stats: reference_capture.clone(),
                position: target,
                hp: 100,
                available_turn: 1_u32.saturating_add(movement_turns),
                engageable_enemy_indices: Vec::new(),
            });
        }
        if capture_completion_turn.is_none()
            && required_capture_survivors > 0
            && protected_units.len() >= required_capture_survivors
        {
            capture_completion_turn = protected_units
                .iter()
                .map(|unit| unit.available_turn)
                .max()
                .map(|arrival| arrival.saturating_add(CAPTURE_COMPLETION_TURNS));
        }
    }

    draft.existing_units = existing_units;
    draft.protected_units = protected_units;
    draft.capture_completion_turn = capture_completion_turn;
}

fn collect_production_options(
    input: &CombatInputs<'_>,
    ctx: &mut ReachCtx,
    draft: &mut CombatDraft,
) {
    let CombatInputs {
        scan,
        op,
        used_facilities,
        require_self_deployment,
        ..
    } = *input;
    let enemies = &draft.enemies;
    let hard_deadline = draft.hard_deadline;
    let capture_completion_turn = draft.capture_completion_turn;
    // 局地Captureで首都戦用の12手番すべての施設×兵種を展開すると、敵が数体
    // 現れただけでbeam候補が急増する。局地パッケージは占領完了ETAまでに投入可能な
    // 生産枠だけを比較し、長期の全軍編成はAssaultCapital側へ分離する。
    let planning_horizon = match op.kind {
        OperationKind::Capture => capture_completion_turn
            .unwrap_or(op.threat_horizon)
            // 弾薬1の航空機などは複数の生産波が必要になる。局地ETAが短くても
            // 6手番までは候補を残し、それ以降の全軍編成だけを首都作戦へ分離する。
            .clamp(6, DEFAULT_SEARCH_TURNS),
        _ => hard_deadline.unwrap_or(DEFAULT_SEARCH_TURNS).max(1),
    };
    let mut options = production_options(
        &scan.free_facilities,
        &scan.production_facilities,
        &scan.available_types,
        &scan.master_data,
        planning_horizon,
        |facility, stats| {
            let can_engage = enemies.iter().any(|enemy| {
                best_damage(&scan.damage_chart, stats.unit_type, enemy.stats.unit_type) > 0
                    && ctx.can_reach_engagement_envelope(
                        &scan.map,
                        &scan.master_data,
                        (facility.x, facility.y),
                        (enemy.position.x, enemy.position.y),
                        stats.movement_type,
                        stats.max_range,
                    )
            });
            if require_self_deployment {
                return can_engage;
            }
            can_engage
                || can_join_operation(
                    scan,
                    ctx,
                    &op.anchor,
                    op.facts.requires_transport,
                    &facility,
                    stats,
                )
        },
    );
    // この手番に既に使った施設は将来手番には再利用できるが、build_turn=0では使えない。
    options.retain(|option| {
        option.purchase.build_turn > 0 || !used_facilities.contains(&option.purchase.facility)
    });
    for option in &mut options {
        option.engageable_enemy_indices = enemies
            .iter()
            .enumerate()
            .filter_map(|(index, enemy)| {
                (best_damage(
                    &scan.damage_chart,
                    option.stats.unit_type,
                    enemy.stats.unit_type,
                ) > 0
                    && ctx.can_reach_engagement_envelope(
                        &scan.map,
                        &scan.master_data,
                        (option.purchase.facility.x, option.purchase.facility.y),
                        (enemy.position.x, enemy.position.y),
                        option.stats.movement_type,
                        option.stats.max_range,
                    ))
                .then_some(index)
            })
            .collect();
    }

    draft.options = options;
}

fn finish_combat_input(input: &CombatInputs<'_>, draft: CombatDraft) -> Option<RollingPlanInput> {
    let CombatInputs {
        scan,
        op,
        remaining_funds,
        ..
    } = *input;
    let CombatDraft {
        enemies,
        hard_deadline,
        capture_completion_turn,
        required_capture_survivors,
        existing_units,
        protected_units,
        options,
        production_attack_projections,
    } = draft;
    Some(RollingPlanInput {
        current_turn: 0,
        opening_policy: rolling_plan::OpeningProductionPolicy::Standard,
        production_attack_projections,
        deadline_target_index: None,
        interdiction_deadlines: Vec::new(),
        prioritize_deadline_interdiction: false,
        exact_property_control: false,
        map: scan.map.clone(),
        master_data: scan.master_data.clone(),
        damage_chart: scan.damage_chart.clone(),
        existing_units,
        protected_units,
        enemies,
        production_options: options,
        current_funds: remaining_funds,
        income_per_turn: scan.my_income,
        hard_deadline,
        capture_completion_turn,
        required_capture_survivors,
        delay_cost_per_turn: op.facts.target_property_count.max(1).saturating_mul(1_000),
        search_beam_width: if op.kind == OperationKind::Capture {
            32
        } else {
            rolling_plan::SEARCH_BEAM_WIDTH
        },
    })
}
