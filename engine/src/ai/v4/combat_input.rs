use crate::ai::v4::property_control::{
    EARLY_PROPERTY_CONTROL_HORIZON, PropertyControlApproach, enemy_capture_completion_offset,
    player_order, property_durability,
};
use crate::ai::v4::trace::campaign_scope_audit_enabled;
use crate::ai::v4::*;

#[allow(unused_imports)]
use crate::ai::planning::decision::AdoptedTurnPortfolio;

#[derive(Clone, Copy)]
struct CombatInputs<'a> {
    policy: ProductionPolicy,
    scan: &'a BoardScan,
    op: &'a Operation,
    used_facilities: &'a HashSet<GridPosition>,
    committed_combat_entities: &'a HashSet<Entity>,
    staged_combat_entities: &'a HashSet<Entity>,
    remaining_funds: u32,
    require_self_deployment: bool,
    player_id: PlayerId,
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
    staged_combat_entities: &HashSet<Entity>,
    remaining_funds: u32,
    require_self_deployment: bool,
    player_id: PlayerId,
    policy: ProductionPolicy,
) -> Option<RollingPlanInput> {
    let input = CombatInputs {
        policy,
        scan,
        op,
        used_facilities,
        committed_combat_entities,
        staged_combat_entities,
        remaining_funds,
        require_self_deployment,
        player_id,
    };
    let mut draft = collect_combat_targets(&input, ctx)?;
    collect_existing_force(&input, ctx, &mut draft);
    collect_production_options(&input, ctx, &mut draft);
    if policy.reports_property_control() {
        draft.production_attack_projections = project_production_attacks(&input, ctx, &draft);
    }
    finish_combat_input(&input, draft)
}

fn collect_combat_targets(input: &CombatInputs<'_>, ctx: &mut ReachCtx) -> Option<CombatDraft> {
    let CombatInputs { scan, op, .. } = *input;

    let mut enemies = observed_combat_targets(scan, op);
    let (hard_deadline, capture_completion_turn, required_capture_survivors) =
        combat_capture_requirements(input, ctx);
    if enemies.is_empty() {
        enemies = projected_capture_enemies(input);
    }
    match input.policy {
        ProductionPolicy::SmallMap if op.kind != OperationKind::Capture => {
            enemies.extend(op.expected_reinforcements.iter().cloned())
        }
        ProductionPolicy::Standard => enemies.extend(op.unavoidable_reinforcements.iter().cloned()),
        _ => {}
    }
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
        staged_combat_entities,
        ..
    } = *input;
    let enemies = &draft.enemies;
    let hard_deadline = draft.hard_deadline;
    let capture_completion_turn = draft.capture_completion_turn;
    let required_capture_survivors = draft.required_capture_survivors;
    let mut existing_units = Vec::new();
    for unit in &scan.my_units {
        if input.policy == ProductionPolicy::Standard && unit.stats.max_cargo > 0 {
            continue;
        }
        let Some(entity) = unit.entity else {
            continue;
        };
        let committed = committed_combat_entities.contains(&entity);
        let staged =
            input.policy.uses_route_commitments() && staged_combat_entities.contains(&entity);
        let protected =
            input.policy.uses_joint_capture() && op.protected_capture_entities.contains(&entity);
        if !committed && !staged && !protected {
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
        if engageable_enemy_indices.is_empty() && !protected {
            continue;
        }
        existing_units.push(FriendlyPlanUnit {
            stats: unit.stats.clone(),
            position: unit.pos,
            hp: unit.hp,
            // Stageは前線にいることを仮定しない。最短移動ETAの後にだけ戦闘へ
            // 寄与する既存戦力として評価し、Advance部隊と同じ手番の火力にはしない。
            available_turn: if staged {
                eta_turns(&scan.map, &unit.pos, &op.anchor, unit.stats.max_movement).max(1)
            } else {
                0
            },
            engageable_enemy_indices,
            protection_completion_turn: protected.then_some(
                capture_completion_turn
                    .or(hard_deadline)
                    .unwrap_or(op.threat_horizon.max(1)),
            ),
        });
    }
    let (protected_units, capture_completion_turn) = match input.policy {
        ProductionPolicy::Standard => standard_protected_units(
            scan,
            op,
            required_capture_survivors,
            capture_completion_turn,
        ),
        ProductionPolicy::SmallMap => small_map_protected_units(
            input,
            ctx,
            &existing_units,
            required_capture_survivors,
            capture_completion_turn,
        ),
    };
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
            // 計画内のどの敵にも損害を与えられない兵種（地上敵しかいない盤面の対空戦車等）は、
            // 作戦地点へ合流できても戦闘に寄与せず、敵の攻撃を吸うだけの駒になる。
            // 輸送ユニットは積荷の運搬が役割なので、武装の有無では除外しない。
            if stats.max_cargo == 0
                && !enemies.iter().any(|enemy| {
                    best_damage(&scan.damage_chart, stats.unit_type, enemy.stats.unit_type) > 0
                })
            {
                return false;
            }
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
    append_joint_capture_options(input, ctx, enemies, &mut options);
    // この手番に既に使った施設は将来手番には再利用できるが、build_turn=0では使えない。
    options.retain(|option| {
        option.purchase.build_turn > 0 || !used_facilities.contains(&option.purchase.facility)
    });
    // 1手番目の戦闘購入は、占領もできる歩兵類に限る。後手は先手の生産を見てから決めるが、
    // 相手の歩兵はまだ数手番先にいるので、1手番目から高価な中戦車などを作るのは早計で、
    // 序盤の資金は先に翼の拠点へ届く歩兵の数に回す。対抗の戦車・砲兵は次手番以降の予定
    // として計画に残し、2手番目からの計画が作る。
    if input.policy == ProductionPolicy::Standard && op.facts.opening_turn {
        options.retain(|option| option.purchase.build_turn > 0 || option.stats.can_capture);
    }
    for option in &mut options {
        option.engageable_enemy_indices = enemies
            .iter()
            .enumerate()
            .filter_map(|(index, enemy)| {
                if option.capture_target.is_some()
                    && option.capture_blocking_enemy_index != Some(index)
                {
                    return None;
                }
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
    if input.policy.uses_joint_capture() && !op.property_controls.is_empty() {
        // 物件前線の合同探索では、Capture契約を持たない候補は実在敵へ合法に
        // 攻撃できる場合、または敵未接触・小規模マップで前線へ自力展開できる
        // 武装戦闘役（快速screen等）の場合だけCombat枠へ残す。非武装輸送や
        // 未接敵兵を「敵の攻撃を吸うだけの戦闘役」として評価することを防ぐ。
        let is_small_map = scan.opening_policy == Some(OpeningProductionPolicy::SmallMapExpansion);
        options.retain(|option| {
            option.capture_target.is_some()
                || !option.engageable_enemy_indices.is_empty()
                || ((enemies.is_empty() || is_small_map) && option.stats.max_range > 0)
        });
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
    let (deadline_target_index, interdiction_deadlines, hard_deadline) =
        combat_interdiction_contracts(input, &enemies, hard_deadline);
    Some(RollingPlanInput {
        map: scan.map.clone(),
        master_data: scan.master_data.clone(),
        damage_chart: scan.damage_chart.clone(),
        current_turn: if input.policy == ProductionPolicy::SmallMap {
            scan.current_turn
        } else {
            0
        },
        opening_policy: if input.policy == ProductionPolicy::SmallMap {
            scan.opening_policy
                .unwrap_or(OpeningProductionPolicy::Standard)
        } else {
            OpeningProductionPolicy::Standard
        },
        existing_units,
        protected_units,
        enemies,
        production_options: options,
        production_attack_projections,
        deadline_target_index,
        interdiction_deadlines,
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
        // 初撃だけで成立するのは、敵ごとの期限を分離できたInterdict契約だけである。
        // 通常Captureのhard deadlineは敵排除期限なので、同じfallbackへ流さない。
        prioritize_deadline_interdiction: input.policy.uses_joint_capture()
            && op.property_controls.iter().any(|control| {
                control.disaggregated && control.approach == PropertyControlApproach::Interdict
            }),
        exact_property_control: input.policy.uses_joint_capture()
            && !op.property_controls.is_empty(),
    })
}

fn standard_protected_units(
    scan: &BoardScan,
    op: &Operation,
    required_capture_survivors: usize,
    mut capture_completion_turn: Option<u32>,
) -> (Vec<FriendlyPlanUnit>, Option<u32>) {
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

    (protected_units, capture_completion_turn)
}

fn small_map_protected_units(
    input: &CombatInputs<'_>,
    ctx: &mut ReachCtx,
    existing_units: &[FriendlyPlanUnit],
    required_capture_survivors: usize,
    mut capture_completion_turn: Option<u32>,
) -> (Vec<FriendlyPlanUnit>, Option<u32>) {
    let CombatInputs {
        scan,
        op,
        player_id,
        ..
    } = *input;
    // 実Entityの占領担当は上の一巡でexisting_unitsへ一度だけ登録した。
    // ここには、この生産判断中に先行発注済みでまだEntity化していないunitだけを置く。
    let mut protected_units = Vec::new();
    let existing_protected_count = existing_units
        .iter()
        .filter(|unit| unit.protection_completion_turn.is_some())
        .count();
    if campaign_scope_audit_enabled() {
        eprintln!(
            "CAMPAIGN_SCOPE_AUDIT stage=supply turn={} op_kind={:?} op_anchor={:?} required={} existing_units={} existing_protected={} planned_capture_units={} my_units={}",
            scan.current_turn,
            op.kind,
            op.anchor,
            required_capture_survivors,
            existing_units.len(),
            existing_protected_count,
            op.planned_capture_units.len(),
            scan.my_units.len(),
        );
    }

    // この生産判断内で先に発注した専任CaptureだけはまだEntityになっていないため、
    // 実行中の保護対象へ投影する。将来手番の仮想Captureはここでは予約しない。
    // Combat枠で選ぶ歩兵・重歩兵も生存占領能力へ数えるため、将来費用を先取りせず
    // 現在の空き施設・現金を前線戦力へ使える。
    let missing_protected_units = required_capture_survivors
        .saturating_sub(existing_protected_count.saturating_add(protected_units.len()));
    for planned in op
        .planned_capture_units
        .iter()
        .take(missing_protected_units)
    {
        let movement_turns = if op.facts.requires_transport {
            op.facts.deploy_lead_time.max(1)
        } else {
            property_control::capture_arrival_turns(
                &scan.map,
                &scan.master_data,
                (planned.facility.x, planned.facility.y),
                (planned.target.x, planned.target.y),
                planned.stats.movement_type,
                planned.stats.max_movement,
                planned.stats.max_fuel,
                player_id,
                &mut ctx.action_turns,
            )
            .unwrap_or(op.facts.deploy_lead_time)
        };
        protected_units.push(FriendlyPlanUnit {
            stats: planned.stats.clone(),
            position: planned.target,
            hp: 100,
            // 距離1は次フェーズに到着する。生産待機を移動フェーズへ二重加算しない。
            available_turn: property_control::produced_action_offset(0, movement_turns),
            engageable_enemy_indices: Vec::new(),
            protection_completion_turn: None,
        });
    }
    if capture_completion_turn.is_none()
        && required_capture_survivors > 0
        && existing_protected_count.saturating_add(protected_units.len())
            >= required_capture_survivors
    {
        capture_completion_turn = existing_units
            .iter()
            .filter(|unit| unit.protection_completion_turn.is_some())
            .chain(protected_units.iter())
            .map(|unit| unit.available_turn)
            .max()
            .map(|arrival| arrival.saturating_add(CAPTURE_COMPLETION_TURNS));
    }

    (protected_units, capture_completion_turn)
}

pub(in crate::ai::v4) mod projection;
use projection::*;

fn observed_combat_targets(scan: &BoardScan, op: &Operation) -> Vec<EnemyPlanUnit> {
    let enemies: Vec<_> = op
        .reachable_threats
        .iter()
        .filter(|threat| threat.current_hp > 0)
        .map(|threat| {
            // 期限対象も仮想的に物件上へテレポートさせず、現在観測した実Entity位置から
            // 追う。配置側もEntity IDへ追随するため、移動中の占領役を迎撃できる。
            let position = threat.position;
            let available_turn = threat.available_turn;
            let terrain = scan
                .map
                .get_terrain(position.x, position.y)
                .unwrap_or(Terrain::Plains);
            EnemyPlanUnit {
                entity: threat.entity,
                stats: threat.stats.clone(),
                position,
                hp: threat.current_hp,
                defense_bonus: scan.master_data.get_terrain_defense_bonus(terrain),
                available_turn,
            }
        })
        .collect();
    enemies
}

fn combat_capture_requirements(
    input: &CombatInputs<'_>,
    ctx: &mut ReachCtx,
) -> (Option<u32>, Option<u32>, usize) {
    let CombatInputs { scan, op, .. } = *input;
    let hard_deadline = combat_attack_deadline(input, ctx);
    let campaign_objective = scan.campaign_objectives.iter().find(|objective| {
        objective.anchor == op.anchor || op.objective_properties.contains(&objective.anchor)
    });
    let capture_completion_turn =
        if input.policy == ProductionPolicy::Standard || op.property_controls.is_empty() {
            campaign_objective.and_then(|objective| objective.capture_eta)
        } else {
            None
        };
    let local_capture_requirement = if !op.capture_lane_targets.is_empty() {
        op.capture_lane_targets.len()
    } else {
        op.objective_properties
            .iter()
            .filter(|property| scan.open_properties.contains(property))
            .count()
    };
    // Campaign側の0は「追加の生存占領役を予約していない」という意味であり、
    // 目前の未所有物件を占領しなくてよい、ではない。局地レーンとcampaign要求の
    // 大きい方をRolling Planへ渡し、歩兵を余剰生産へ漏らさない。
    let required_capture_survivors = if input.policy == ProductionPolicy::SmallMap {
        campaign_objective.map_or(local_capture_requirement, |objective| {
            local_capture_requirement.max(objective.required_capture_survivors)
        })
    } else {
        campaign_objective.map_or_else(
            || {
                op.objective_properties
                    .iter()
                    .filter(|property| scan.open_properties.contains(property))
                    .count()
            },
            |objective| objective.required_capture_survivors,
        )
    };
    if input.policy.reports_property_control() && campaign_scope_audit_enabled() {
        eprintln!(
            "CAMPAIGN_SCOPE_AUDIT stage=quota turn={} op_kind={:?} op_anchor={:?} op_island={:?} op_properties={:?} op_protected={:?} lane_targets={:?} local_requirement={} found={:?} required={}",
            scan.current_turn,
            op.kind,
            op.anchor,
            op.island_id,
            op.objective_properties,
            op.protected_capture_entities,
            op.capture_lane_targets,
            local_capture_requirement,
            campaign_objective.map(|objective| (
                objective.island_id,
                objective.kind,
                objective.anchor,
                objective.required_capture_survivors,
                objective.protected_capture_entities.len(),
            )),
            required_capture_survivors,
        );
    }
    (
        hard_deadline,
        capture_completion_turn,
        required_capture_survivors,
    )
}

fn projected_capture_enemies(input: &CombatInputs<'_>) -> Vec<EnemyPlanUnit> {
    let CombatInputs { scan, op, .. } = *input;
    let mut enemies = Vec::new();
    if input.policy.uses_joint_capture() && enemies.is_empty() && !op.property_controls.is_empty() {
        let reference_infantry = scan
            .available_types
            .iter()
            .find(|(unit_type, stats)| *unit_type == UnitType::Infantry && stats.can_capture)
            .map(|(_, stats)| stats.clone());
        let fallback_enemy_units: Vec<UnitSnapshot> = if scan.enemy_units.is_empty()
            && let Some(infantry_stats) = reference_infantry
        {
            scan.enemy_facilities
                .iter()
                .enumerate()
                .map(|(index, facility)| UnitSnapshot {
                    entity: Some(Entity::from_raw(900_000 + index as u32)),
                    pos: facility.pos,
                    stats: infantry_stats.clone(),
                    hp: 100,
                    free_cargo: 0,
                })
                .collect()
        } else {
            Vec::new()
        };
        for control in &op.property_controls {
            let Some(enemy) = scan
                .enemy_units
                .iter()
                .find(|unit| unit.entity == Some(control.enemy_entity))
                .or_else(|| {
                    fallback_enemy_units
                        .iter()
                        .find(|unit| unit.entity == Some(control.enemy_entity))
                })
            else {
                continue;
            };
            let position = enemy.pos;
            let terrain = scan
                .map
                .get_terrain(position.x, position.y)
                .unwrap_or(Terrain::Plains);
            enemies.push(EnemyPlanUnit {
                entity: Some(control.enemy_entity),
                stats: enemy.stats.clone(),
                position,
                hp: enemy.hp,
                defense_bonus: scan.master_data.get_terrain_defense_bonus(terrain),
                available_turn: control.enemy_arrival_turn,
            });
        }
    }
    // Capture作戦のCombat計画は「今の物件レース」を解くもので、将来生産される
    // 増援は対象にしない。将来増援まで含めると全滅が実行不能になり、快速の妨害が
    // 出せなくなる。将来増援への備えはDefense/AssaultCapital側が担う。
    enemies
}

fn append_joint_capture_options(
    input: &CombatInputs<'_>,
    ctx: &mut ReachCtx,
    enemies: &[EnemyPlanUnit],
    options: &mut Vec<rolling_plan::ProductionPlanOption>,
) {
    let CombatInputs { scan, op, .. } = *input;
    if input.policy.uses_joint_capture() && !op.property_controls.is_empty() {
        // 同じ施設・資金について、戦闘役だけでなく各物件の占領役として使う合法手も
        // 同じ探索空間へ入れる。先に最安歩兵を予約せず、相性を含む全編成で比較する。
        for target in &op.capture_lane_targets {
            let target = CaptureOptionTarget {
                property: *target,
                durability: property_durability(scan, target),
            };
            append_capture_options_for_target(input, ctx, enemies, options, &target);
        }
        options.sort_by_key(|option| {
            (
                option.purchase.build_turn,
                option.purchase.facility.y,
                option.purchase.facility.x,
                option.capture_target.map(|target| (target.y, target.x)),
                option.purchase.cost,
                option.purchase.unit_type.as_str(),
            )
        });
    }
}

fn combat_attack_deadline(input: &CombatInputs<'_>, ctx: &mut ReachCtx) -> Option<u32> {
    let CombatInputs {
        scan,
        op,
        player_id,
        ..
    } = *input;

    if op.kind == OperationKind::Defense && op.facts.enemy_contact_eta != u32::MAX {
        Some(op.facts.enemy_contact_eta.max(1))
    } else if input.policy.uses_joint_capture() && op.kind == OperationKind::Capture {
        if let Some(control) = op.property_control.filter(|control| control.disaggregated) {
            if control.approach == PropertyControlApproach::Interdict {
                // 同一roundなら先手だけが敵占領完了前に行動できる。後手は一つ前の
                // roundを最終期限にし、turn整数化で同着を成功扱いしない。
                Some(
                    if player_order(player_id) < player_order(player_id.opposite()) {
                        control.enemy_completion_turn
                    } else {
                        control.enemy_completion_turn.saturating_sub(1)
                    },
                )
                .filter(|deadline| *deadline > 0)
            } else {
                None
            }
        } else {
            // 直取り競争では、敵が占領完了する前の撃破が唯一の勝ち筋。
            // 敵占領役の占領完了フェーズを「撃破のハード期限」にし、総ダメージではなく
            // 「期限前に敵を倒せるか」を編成選択の最優先にする。
            // ただし完了が1フェーズ以内（＝敵が既に物件上で占領中）なら、この手番の
            // 生産では妨害が物理的に間に合わないため、期限を立てず通常の撃破計画へ委ねる。
            enemy_capture_completion_offset(scan, ctx, &op.anchor, player_id)
                .filter(|deadline| *deadline > 1)
        }
    } else {
        None
    }
}

fn combat_interdiction_contracts(
    input: &CombatInputs<'_>,
    enemies: &[EnemyPlanUnit],
    mut hard_deadline: Option<u32>,
) -> (Option<usize>, Vec<(usize, u32)>, Option<u32>) {
    let CombatInputs { op, player_id, .. } = *input;
    let disaggregated_control = op.property_control.filter(|control| control.disaggregated);
    let deadline_target_index = disaggregated_control
        .filter(|_| input.policy.uses_joint_capture())
        .filter(|control| control.approach == PropertyControlApproach::Interdict)
        .and_then(|control| {
            enemies
                .iter()
                .position(|enemy| enemy.entity == Some(control.enemy_entity))
        });
    let interdiction_deadlines = op
        .property_controls
        .iter()
        .filter(|control| {
            input.policy.uses_joint_capture()
                && control.disaggregated
                && control.approach == PropertyControlApproach::Interdict
        })
        .filter_map(|control| {
            let index = enemies
                .iter()
                .position(|enemy| enemy.entity == Some(control.enemy_entity))?;
            // 同roundなら先手だけが占領完了前に行動できる。後手は直前roundを期限にする。
            let deadline = if player_order(player_id) < player_order(player_id.opposite()) {
                control.enemy_completion_turn
            } else {
                control.enemy_completion_turn.saturating_sub(1)
            };
            (deadline > 0).then_some((index, deadline))
        })
        .collect::<Vec<_>>();
    if let Some(latest_deadline) = interdiction_deadlines
        .iter()
        .map(|(_, deadline)| *deadline)
        .max()
    {
        hard_deadline = Some(latest_deadline);
    }
    if input.policy.uses_joint_capture() && !op.property_controls.is_empty() {
        hard_deadline = Some(
            hard_deadline
                .unwrap_or(0)
                .max(EARLY_PROPERTY_CONTROL_HORIZON),
        );
    }
    (deadline_target_index, interdiction_deadlines, hard_deadline)
}

/// この候補表の一つの物件と、その時点で観測した占領耐久。
struct CaptureOptionTarget {
    property: GridPosition,
    durability: u32,
}
fn append_capture_options_for_target(
    input: &CombatInputs<'_>,
    ctx: &mut ReachCtx,
    enemies: &[EnemyPlanUnit],
    options: &mut Vec<rolling_plan::ProductionPlanOption>,
    target: &CaptureOptionTarget,
) {
    let CombatInputs {
        scan,
        used_facilities,
        ..
    } = *input;
    for (facility, terrain) in &scan.free_facilities {
        if used_facilities.contains(facility) {
            continue;
        }
        append_capture_options_at_facility(input, ctx, enemies, options, target, facility, terrain);
    }
}

fn append_capture_options_at_facility(
    input: &CombatInputs<'_>,
    ctx: &mut ReachCtx,
    enemies: &[EnemyPlanUnit],
    options: &mut Vec<rolling_plan::ProductionPlanOption>,
    target: &CaptureOptionTarget,
    facility: &GridPosition,
    terrain: &Terrain,
) {
    let CombatInputs {
        scan,
        op,
        player_id,
        ..
    } = *input;
    let durability = target.durability;
    let target = &target.property;
    for (unit_type, stats) in &scan.available_types {
        if !stats.can_capture
            || stats.cost == 0
            || !scan.can_produce(*terrain, *unit_type)
            || !can_join_operation(
                scan,
                ctx,
                target,
                op.facts.requires_transport,
                facility,
                stats,
            )
        {
            continue;
        }
        let Some(arrival) = property_control::capture_arrival_turns(
            &scan.map,
            &scan.master_data,
            (facility.x, facility.y),
            (target.x, target.y),
            stats.movement_type,
            stats.max_movement,
            stats.max_fuel,
            player_id,
            &mut ctx.action_turns,
        )
        .map(|turns| property_control::produced_action_offset(0, turns)) else {
            continue;
        };
        let completion = property_control::completion_offset(
            arrival,
            durability,
            property_control::capture_power_from_hp(100),
        );
        let blocking_enemy_index = op
            .property_controls
            .iter()
            .find(|control| control.property == *target)
            .and_then(|control| {
                enemies
                    .iter()
                    .position(|enemy| enemy.entity == Some(control.enemy_entity))
            });
        options.push(rolling_plan::ProductionPlanOption {
            purchase: rolling_plan::PlannedPurchase {
                facility: *facility,
                unit_type: *unit_type,
                build_turn: 0,
                cost: stats.cost,
            },
            stats: stats.clone(),
            engageable_enemy_indices: Vec::new(),
            capture_target: Some(*target),
            capture_arrival_turn: Some(arrival),
            capture_completion_turn: Some(completion),
            capture_durability: Some(durability),
            capture_blocking_enemy_index: blocking_enemy_index,
        });
    }
}
