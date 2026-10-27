use crate::ai::v4::enemy_forecast::{
    EXPECTED_REINFORCEMENT_WAVES, EnemyReinforcementEnvelope, ReinforcementScenario,
    projected_enemy_reinforcement_envelope,
};
use crate::ai::v4::property_control::{
    EARLY_PROPERTY_CONTROL_HORIZON, capture_race_behind, enemy_capture_completion_offset,
    select_property_control_plan,
};
use crate::ai::v4::trace::campaign_scope_audit_enabled;
use crate::ai::v4::*;

#[allow(unused_imports)]
use crate::ai::planning::decision::AdoptedTurnPortfolio;

type CampaignClusters<'a> = Vec<(&'a CampaignPlanningObjective, Vec<GridPosition>)>;
struct OperationSources<'a> {
    campaign_clusters: CampaignClusters<'a>,
    island_map: crate::ai::islands::IslandMap,
}
impl<'a> OperationSources<'a> {
    fn campaign_for_cluster(
        &self,
        kind: OperationKind,
        cluster: &[GridPosition],
    ) -> Option<&'a CampaignPlanningObjective> {
        self.campaign_clusters
            .iter()
            .find_map(|(objective, properties)| {
                (objective.kind == kind
                    && cluster.iter().any(|property| properties.contains(property)))
                .then_some(*objective)
            })
            .or_else(|| {
                // Capture→Defense/Contestの遷移でも、島そのものが同じなら同一campaign。
                self.campaign_clusters.iter().find_map(|(objective, _)| {
                    cluster
                        .iter()
                        .any(|property| {
                            self.island_map
                                .get_island_at(property)
                                .is_some_and(|island| island.id == objective.island_id)
                        })
                        .then_some(*objective)
                })
            })
    }
}
struct OperationLayout {
    anchors: Vec<GridPosition>,
    horizons: Vec<u32>,
    assignment_enabled: Vec<bool>,
    operation_leads: Vec<u32>,
}
struct OperationAssembly<'a, 'b> {
    reference: &'a UnitStats,
    policy: ProductionPolicy,
    scan: &'a BoardScan,
    active_objectives: &'a [ActivePlanObjective],
    sources: &'a OperationSources<'b>,
    layout: &'a OperationLayout,
    property_control_plans: &'a [Option<property_control::PropertyControlPlan>],
}
pub(in crate::ai::v4) fn build_operations(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    active_objectives: &[ActivePlanObjective],
    player_id: PlayerId,
    policy: ProductionPolicy,
) -> Vec<Operation> {
    let Some(reference) = scan.reference_capture_unit().cloned() else {
        return Vec::new();
    };

    // V4の作戦源は、島campaignが選んだCapture/Defenseと、既に実行を始めた
    // Combat planだけに限定する。全未所有拠点を距離で束ねる第二の目標選定器を
    // 併用すると、勝利ロードマップ外の前線へ生産予算が流れるためである。
    let (raw, sources) = collect_operation_sources(scan, active_objectives);
    let scored =
        score_operation_candidates(scan, active_objectives, &reference, raw, &sources, policy);
    let property_control_plans = if policy.reports_property_control() {
        select_property_controls(scan, ctx, &scored, player_id)
    } else {
        vec![None; scored.len()]
    };
    let layout = prepare_operation_layout(
        scan,
        &reference,
        &scored,
        &sources,
        &property_control_plans,
        policy,
    );
    let input = OperationAssembly {
        policy,
        scan,
        reference: &reference,
        active_objectives,
        sources: &sources,
        layout: &layout,
        property_control_plans: &property_control_plans,
    };
    scored
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| assemble_operation(&input, ctx, index, candidate, player_id))
        .collect()
}

fn collect_operation_sources<'a>(
    scan: &'a BoardScan,
    active_objectives: &[ActivePlanObjective],
) -> (
    Vec<(OperationKind, Vec<GridPosition>)>,
    OperationSources<'a>,
) {
    let mut raw = active_objectives
        .iter()
        .filter(|objective| !objective.properties.is_empty())
        .map(|objective| (objective.kind, objective.properties.clone()))
        .collect::<Vec<_>>();

    let island_map = crate::ai::islands::IslandMap::analyze(&scan.map);
    let mut campaign_clusters = scan
        .campaign_objectives
        .iter()
        .map(|objective| {
            let mut properties = if objective.kind == OperationKind::Defense {
                vec![objective.anchor]
            } else if !objective.objective_properties.is_empty() {
                objective.objective_properties.clone()
            } else {
                scan.open_properties
                    .iter()
                    .filter(|position| {
                        island_map
                            .get_island_at(position)
                            .is_some_and(|island| island.id == objective.island_id)
                    })
                    .copied()
                    .collect::<Vec<_>>()
            };
            if properties.is_empty() {
                // 全施設取得後のContest/Reinforceも、残敵への保持・掃討作戦として残す。
                properties.push(objective.anchor);
            }
            properties.sort_unstable_by_key(|position| (position.y, position.x));
            (objective, properties)
        })
        .collect::<Vec<_>>();
    // 勝利目標としての首都作戦と、現在の局地Captureは同じ島でも別Operationにする。
    // 単一大陸全体を一つの完全編成待ちへ畳むと、裸の占領兵だけが先行するためである。
    campaign_clusters.sort_by_key(|(objective, _)| {
        (
            objective.island_id.0,
            u8::from(objective.kind != OperationKind::AssaultCapital),
            objective.kind.priority_rank(),
        )
    });
    raw.retain(|(_, cluster)| {
        !campaign_clusters.iter().any(|(objective, _)| {
            objective.kind != OperationKind::AssaultCapital
                && cluster
                    .iter()
                    .any(|property| objective.objective_properties.contains(property))
        })
    });
    raw.extend(
        campaign_clusters
            .iter()
            .map(|(objective, cluster)| (objective.kind, cluster.clone())),
    );

    (
        raw,
        OperationSources {
            campaign_clusters,
            island_map,
        },
    )
}

fn score_operation_candidates(
    scan: &BoardScan,
    active_objectives: &[ActivePlanObjective],
    reference: &UnitStats,
    mut raw: Vec<(OperationKind, Vec<GridPosition>)>,
    sources: &OperationSources<'_>,
    policy: ProductionPolicy,
) -> Vec<OperationCandidate> {
    // 継続中Planのsnapshotと当ターンRoadmapは、同じ島・同じ目的のOperationを
    // 同時に提示し得る。両方を別候補として残すと同じAssaultCapitalがCapture枠を
    // 二重要求し、戦闘生産を毎ターン2体分の歩兵で圧迫する。Plan継続性は後段の
    // `continuing` で合流できるため、campaign identityごとに一件へ正規化する。
    if policy.uses_joint_capture() {
        let mut seen_campaign_operations = HashSet::default();
        raw.retain(|(kind, cluster)| {
            sources
                .campaign_for_cluster(*kind, cluster)
                .is_none_or(|objective| {
                    seen_campaign_operations
                        .insert((objective.kind.priority_rank(), objective.island_id))
                })
        });
    }
    // 上位campaignと永続Planが所有する作戦を必須として識別し、追加の局地候補だけを
    // 同時実行容量へ収める。固定4件で第5の戦略作戦を消してはならない。
    let mut scored: Vec<OperationCandidate> = raw
        .into_iter()
        .filter(|(_, cluster)| !cluster.is_empty())
        .map(|(kind, cluster)| {
            let campaign = sources.campaign_for_cluster(kind, &cluster);
            let anchor =
                campaign.map_or_else(|| anchor_of(&cluster, scan), |objective| objective.anchor);
            let lead = facility_lead_time(scan, &anchor, reference.max_movement);
            let continuing = active_objectives.iter().any(|objective| {
                sources
                    .campaign_for_cluster(kind, &cluster)
                    .is_some_and(|campaign| objective.island_id == Some(campaign.island_id))
                    || (objective.island_id.is_none()
                        && objective.kind == kind
                        && cluster
                            .iter()
                            .any(|property| objective.properties.contains(property)))
            });
            OperationCandidate {
                continuing,
                lead,
                kind,
                cluster,
                required: continuing || campaign.is_some(),
            }
        })
        .collect();
    scored.sort_by_key(|candidate| {
        let logistics_rank = sources
            .campaign_for_cluster(candidate.kind, &candidate.cluster)
            .and_then(|objective| objective.logistics_rank)
            .unwrap_or(u32::MAX);
        (
            candidate.kind.priority_rank(),
            logistics_rank,
            !candidate.continuing,
            candidate.lead,
            // 同条件なら拠点数の多い（面が広い）作戦を優先
            usize::MAX - candidate.cluster.len(),
        )
    });

    select_operation_candidates(scored)
}

fn select_property_controls(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    scored: &[OperationCandidate],
    player_id: PlayerId,
) -> Vec<Option<property_control::PropertyControlPlan>> {
    // Capture clusterは代表点一つで評価せず、敵占領役ごとに成立する物件制御を選ぶ。
    // これにより中央が間に合わない場合も、左右の合法なInterdictを候補に残す。
    let neutral_race_owner = scored
        .iter()
        .position(|candidate| candidate.kind == OperationKind::Capture);

    scored
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            if candidate.kind != OperationKind::Capture {
                return None;
            }
            // 初動の中立物件レースはDAG区間をまたいで同時に進むため、最優先Captureが
            // 全中立物件を一度だけ共同評価する。各物件は下段の実capture completion期限で
            // 除外されるので、遠方目標を固定数で抱え込まない。後続Captureは自身の区間だけ扱う。
            let properties =
                if neutral_race_owner == Some(index) && !scan.neutral_properties.is_empty() {
                    // 中立が残る局面では中立物件レースだけを共同評価する。敵後方物件を
                    // 混ぜると、直取り3件を奪還5件へ水増ししてしまう。
                    // ただし、Campaign作戦目標が認めていない遠隔物件（未突破Gate向こうなど）
                    // を再投入して戦闘予算を枯渇させないよう、campaign_objectivesが認めた物件に限定する。
                    let campaign_allowed_properties = scan
                        .campaign_objectives
                        .iter()
                        .flat_map(|obj| obj.objective_properties.iter().copied())
                        .collect::<HashSet<_>>();
                    let filtered_neutrals: Vec<_> = scan
                        .neutral_properties
                        .iter()
                        .copied()
                        .filter(|pos| campaign_allowed_properties.contains(pos))
                        .collect();
                    if filtered_neutrals.is_empty() {
                        candidate.cluster.clone()
                    } else {
                        filtered_neutrals
                    }
                } else {
                    // 中立が無い初期分割マップでは、Campaignが選んだ前線物件を使う。
                    // 首都は別Operationなのでここには入らない。
                    candidate.cluster.clone()
                };
            select_property_control_plan(
                scan,
                ctx,
                &properties,
                // 小規模前線は「基準歩兵が最寄り物件へ着くまで」ではなく、T6までに
                // 実際に支配へ変えられる全候補を合同比較する。短い歩兵ETAで切ると
                // 快速攻撃役で前線を開き、後続占領役が取る行動列そのものが消える。
                EARLY_PROPERTY_CONTROL_HORIZON,
                player_id,
            )
        })
        .collect::<Vec<_>>()
}

fn prepare_operation_layout(
    scan: &BoardScan,
    reference: &UnitStats,
    scored: &[OperationCandidate],
    sources: &OperationSources<'_>,
    property_control_plans: &[Option<property_control::PropertyControlPlan>],
    policy: ProductionPolicy,
) -> OperationLayout {
    let anchors: Vec<GridPosition> = scored
        .iter()
        .zip(property_control_plans)
        .map(|(candidate, property_control_plan)| {
            property_control_plan.as_ref().map_or_else(
                || {
                    sources
                        .campaign_for_cluster(candidate.kind, &candidate.cluster)
                        .map_or_else(
                            || anchor_of(&candidate.cluster, scan),
                            |objective| {
                                if policy.uses_joint_capture() {
                                    objective.staging_anchor
                                } else {
                                    objective.anchor
                                }
                            },
                        )
                },
                |plan| plan.primary.property,
            )
        })
        .collect();
    let operation_leads = anchors
        .iter()
        .enumerate()
        .map(|(index, anchor)| {
            if policy.uses_joint_capture() {
                facility_lead_time(scan, anchor, reference.max_movement)
            } else {
                scored[index].lead
            }
        })
        .collect::<Vec<_>>();
    let horizons: Vec<u32> = scored
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            operation_threat_horizon(candidate.kind, operation_leads[index], policy)
        })
        .collect();
    let assignment_enabled = scored
        .iter()
        .map(|candidate| {
            sources
                .campaign_for_cluster(candidate.kind, &candidate.cluster)
                .is_none_or(|objective| {
                    objective.kind != OperationKind::AssaultCapital
                        || objective.execution_authorized
                })
        })
        .collect::<Vec<_>>();
    OperationLayout {
        anchors,
        horizons,
        assignment_enabled,
        operation_leads,
    }
}

fn assemble_operation(
    input: &OperationAssembly<'_, '_>,
    ctx: &mut ReachCtx,
    index: usize,
    candidate: OperationCandidate,
    player_id: PlayerId,
) -> Operation {
    let OperationAssembly {
        policy,
        scan,
        reference,
        active_objectives,
        sources,
        layout,
        property_control_plans,
    } = *input;
    let OperationLayout {
        anchors,
        horizons,
        assignment_enabled,
        operation_leads,
    } = layout;
    let empty_entity_set = HashSet::default();
    let OperationCandidate { kind, cluster, .. } = candidate;
    let anchor = anchors[index];
    let property_control_plan = property_control_plans[index].clone();
    let _property_control = property_control_plan.as_ref().map(|plan| plan.primary);
    let lead = operation_leads[index];
    let planning_objective = sources.campaign_for_cluster(kind, &cluster);
    let forced_target_enemies = planning_objective
        .map(|objective| &objective.forced_target_enemies)
        .or_else(|| {
            active_objectives
                .iter()
                .find(|objective| {
                    planning_objective
                        .is_some_and(|campaign| objective.island_id == Some(campaign.island_id))
                        || (objective.island_id.is_none()
                            && objective.kind == kind
                            && objective.properties == cluster)
                })
                .map(|objective| &objective.target_enemies)
        })
        .unwrap_or(&empty_entity_set);
    let protected_capture_entities = planning_objective
        .map(|objective| &objective.protected_capture_entities)
        .unwrap_or(&empty_entity_set);
    let mut local_forced_target_enemies = forced_target_enemies.clone();
    if let Some(plan) = property_control_plan.as_ref() {
        local_forced_target_enemies.extend(
            plan.controls
                .iter()
                .filter(|control| control.disaggregated)
                .map(|control| control.enemy_entity),
        );
    }
    let mut operation = build_operation(
        scan,
        ctx,
        reference,
        kind,
        anchor,
        anchors,
        horizons,
        assignment_enabled,
        &cluster,
        &local_forced_target_enemies,
        protected_capture_entities,
        lead,
        player_id,
        policy,
    );
    apply_property_control(
        input,
        &mut operation,
        &cluster,
        property_control_plan,
        planning_objective,
    );
    if let Some(objective) = planning_objective {
        operation.island_id = Some(objective.island_id);
        operation.staging_anchor = objective.staging_anchor;
        operation.execution_authorized = objective.execution_authorized;
        operation.protected_capture_entities = objective.protected_capture_entities.clone();
        operation.capture_replacement_pending =
            policy.reports_property_control() && objective.capture_replacement_pending;
    } else if let Some(active) = active_objectives
        .iter()
        .find(|objective| objective.kind == kind && objective.properties == cluster)
    {
        // portfolioがObserveへ一時遷移しても、active Plan由来のOperationは
        // 島identityを失わない。敵やanchorの変化で新Planへ分裂させない。
        operation.island_id = active.island_id;
    }
    operation
}

pub(in crate::ai::v4) fn eta_turns(
    map: &Map,
    from: &GridPosition,
    to: &GridPosition,
    movement: u32,
) -> u32 {
    let distance = map.distance(from.x, from.y, to.x, to.y);
    distance.div_ceil(movement.max(1))
}

pub(in crate::ai::v4) fn anchor_of(cluster: &[GridPosition], scan: &BoardScan) -> GridPosition {
    cluster
        .iter()
        .copied()
        .min_by_key(|pos| {
            scan.free_facilities
                .iter()
                .map(|(f, _)| scan.map.distance(f.x, f.y, pos.x, pos.y))
                .min()
                .unwrap_or(u32::MAX)
        })
        .unwrap_or(cluster[0])
}

pub(in crate::ai::v4) fn facility_lead_time(
    scan: &BoardScan,
    anchor: &GridPosition,
    movement: u32,
) -> u32 {
    scan.free_facilities
        .iter()
        .map(|(f, _)| eta_turns(&scan.map, f, anchor, movement))
        .min()
        .unwrap_or(u32::MAX)
}

pub(super) fn operation_threat_horizon(
    kind: OperationKind,
    deploy_lead_time: u32,
    policy: ProductionPolicy,
) -> u32 {
    let capital_search_turns = if policy.uses_route_commitments() {
        CAPTURE_COMPLETION_TURNS
    } else {
        rolling_plan::DEFAULT_SEARCH_TURNS
    };
    match kind {
        OperationKind::Defense => DEFENSE_THREAT_ETA,
        OperationKind::Capture => deploy_lead_time.saturating_add(CAPTURE_COMPLETION_TURNS),
        OperationKind::AssaultCapital => deploy_lead_time.saturating_add(capital_search_turns),
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::ai::v4) fn nearest_relevant_anchor_index(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    pos: &GridPosition,
    movement: MovementType,
    max_movement: u32,
    anchors: &[GridPosition],
    horizons: &[u32],
    assignment_enabled: &[bool],
) -> Option<usize> {
    anchors
        .iter()
        .enumerate()
        .filter(|(index, _)| assignment_enabled.get(*index).copied().unwrap_or(true))
        .filter_map(|(index, anchor)| {
            if !ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (pos.x, pos.y),
                (anchor.x, anchor.y),
                movement,
            ) {
                return None;
            }
            let eta = eta_turns(&scan.map, pos, anchor, max_movement);
            (eta <= horizons.get(index).copied().unwrap_or(0)).then_some((eta, index))
        })
        .min()
        .map(|(_, index)| index)
}

pub(in crate::ai::v4) fn nearest_reachable_anchor_index(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    pos: &GridPosition,
    movement: MovementType,
    max_movement: u32,
    anchors: &[GridPosition],
    assignment_enabled: &[bool],
) -> Option<usize> {
    anchors
        .iter()
        .enumerate()
        .filter(|(index, _)| assignment_enabled.get(*index).copied().unwrap_or(true))
        .filter_map(|(index, anchor)| {
            ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (pos.x, pos.y),
                (anchor.x, anchor.y),
                movement,
            )
            .then_some((eta_turns(&scan.map, pos, anchor, max_movement), index))
        })
        .min()
        .map(|(_, index)| index)
}

pub(in crate::ai::v4) fn enemy_facility_arrival(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    facility: EnemyFacilitySnapshot,
    anchor: GridPosition,
) -> Option<(u32, u32)> {
    scan.available_types
        .iter()
        .filter(|(unit_type, stats)| {
            stats.max_cargo == 0 && scan.can_produce(facility.terrain, *unit_type)
        })
        .filter_map(|(_, stats)| {
            ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (facility.pos.x, facility.pos.y),
                (anchor.x, anchor.y),
                stats.movement_type,
            )
            .then_some((
                eta_turns(&scan.map, &facility.pos, &anchor, stats.max_movement),
                stats.cost,
            ))
        })
        .min_by_key(|(eta, cost)| (*eta, *cost))
}

pub(in crate::ai::v4) fn projected_enemy_reinforcement_funds(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    anchors: &[GridPosition],
    horizons: &[u32],
    anchor_index: usize,
) -> u32 {
    if scan.enemy_production_slots == 0 {
        return 0;
    }
    let mut local_slots = 0_u32;
    let mut production_capacity = 0_u32;
    for facility in &scan.enemy_facilities {
        let assignment = anchors
            .iter()
            .enumerate()
            .filter_map(|(index, anchor)| {
                // 編成中でまだ前進を許可していない首都攻略は、敵増援の配分先ではない。
                // ここへ距離0で全枠を吸わせると、実際に占領中の中央島を「増援なし」と
                // 誤認して占領兵だけを送り続けるため、実行中の局地作戦を先に評価する。
                let is_unauthorized_capital_formation = scan
                    .campaign_objectives
                    .iter()
                    .find(|objective| objective.anchor == *anchor)
                    .is_some_and(|objective| {
                        objective.kind == OperationKind::AssaultCapital
                            && !objective.execution_authorized
                    });
                if is_unauthorized_capital_formation {
                    return None;
                }
                let (eta, cost) = enemy_facility_arrival(scan, ctx, *facility, *anchor)?;
                (eta <= horizons.get(index).copied().unwrap_or(0)).then_some((eta, index, cost))
            })
            .min_by_key(|(eta, index, _)| (*eta, *index));
        let Some((eta, assigned_index, unit_cost)) = assignment else {
            continue;
        };
        if assigned_index != anchor_index {
            continue;
        }
        let production_turns = horizons[anchor_index].saturating_sub(eta);
        if production_turns == 0 {
            continue;
        }
        local_slots = local_slots.saturating_add(1);
        production_capacity =
            production_capacity.saturating_add(unit_cost.saturating_mul(production_turns));
    }
    if local_slots == 0 {
        return 0;
    }
    let allocated_income_per_turn = u64::from(scan.enemy_income)
        .saturating_mul(u64::from(local_slots))
        / u64::from(scan.enemy_production_slots.max(1));
    let income_capacity =
        allocated_income_per_turn.saturating_mul(u64::from(horizons[anchor_index]));
    production_capacity.min(u32::try_from(income_capacity).unwrap_or(u32::MAX))
}

pub(in crate::ai::v4) fn fastest_observed_counter(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    anchor: GridPosition,
    enemy: &UnitStats,
    counter_build_turn: u32,
    enemy_contact_turn: u32,
    reserved_slots: &HashSet<(u32, GridPosition)>,
) -> Option<ReinforcementContingency> {
    scan.production_facilities
        .iter()
        .flat_map(|(facility, terrain)| {
            scan.available_types
                .iter()
                .filter(move |(unit_type, stats)| {
                    !stats.can_capture
                        && stats.max_cargo == 0
                        && scan.can_produce(*terrain, *unit_type)
                })
                .map(move |(_, stats)| (*facility, stats))
        })
        .filter(|(facility, stats)| {
            !reserved_slots.contains(&(counter_build_turn, *facility))
                && best_damage(&scan.damage_chart, stats.unit_type, enemy.unit_type) > 0
                && ctx.is_reachable(
                    &scan.map,
                    &scan.master_data,
                    (facility.x, facility.y),
                    (anchor.x, anchor.y),
                    stats.movement_type,
                )
        })
        .filter_map(|(facility, stats)| {
            let damage = best_damage(&scan.damage_chart, stats.unit_type, enemy.unit_type);
            let attacks_required = 100_u32.div_ceil(damage.max(1));
            let counter_contact_turn = counter_build_turn
                .saturating_add(1)
                .saturating_add(eta_turns(&scan.map, &facility, &anchor, stats.max_movement));
            (counter_contact_turn <= enemy_contact_turn).then_some((
                counter_contact_turn,
                attacks_required,
                stats.cost,
                facility,
                stats,
            ))
        })
        .min_by_key(|(contact, attacks, cost, facility, _)| {
            (*contact, *attacks, *cost, facility.y, facility.x)
        })
        .map(
            |(counter_contact_turn, attacks_required, reserve_cost, facility, stats)| {
                ReinforcementContingency {
                    enemy_type: enemy.unit_type,
                    enemy_contact_turn,
                    counter_type: stats.unit_type,
                    counter_facility: facility,
                    counter_build_turn,
                    counter_contact_turn,
                    attacks_required,
                    reserve_cost,
                }
            },
        )
}

pub(in crate::ai::v4) fn contingency_reserve_now(
    contingencies: &[ReinforcementContingency],
    income_per_turn: u32,
) -> u32 {
    let mut due_by_turn = HashMap::<u32, u32>::default();
    for contingency in contingencies {
        due_by_turn
            .entry(contingency.counter_build_turn)
            .and_modify(|cost| *cost = cost.saturating_add(contingency.reserve_cost))
            .or_insert(contingency.reserve_cost);
    }
    let mut turns = due_by_turn.keys().copied().collect::<Vec<_>>();
    turns.sort_unstable();
    let mut cumulative_cost = 0_u32;
    turns.into_iter().fold(0_u32, |required_now, turn| {
        cumulative_cost = cumulative_cost.saturating_add(due_by_turn[&turn]);
        required_now.max(cumulative_cost.saturating_sub(income_per_turn.saturating_mul(turn)))
    })
}

#[allow(clippy::too_many_arguments)]
struct OperationObservation<'a> {
    scan: &'a BoardScan,
    kind: OperationKind,
    anchor: GridPosition,
    anchors: &'a [GridPosition],
    horizons: &'a [u32],
    assignment_enabled: &'a [bool],
    cluster: &'a [GridPosition],
    forced_target_enemies: &'a HashSet<Entity>,
    protected_capture_entities: &'a HashSet<Entity>,
    deploy_lead_time: u32,
    player_id: PlayerId,
    policy: ProductionPolicy,
}
#[allow(clippy::too_many_arguments)]
pub(in crate::ai::v4) fn build_operation(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    reference: &UnitStats,
    kind: OperationKind,
    anchor: GridPosition,
    anchors: &[GridPosition],
    horizons: &[u32],
    assignment_enabled: &[bool],
    cluster: &[GridPosition],
    forced_target_enemies: &HashSet<Entity>,
    protected_capture_entities: &HashSet<Entity>,
    deploy_lead_time: u32,
    player_id: PlayerId,
    policy: ProductionPolicy,
) -> Operation {
    let input = OperationObservation {
        scan,
        kind,
        anchor,
        anchors,
        horizons,
        assignment_enabled,
        cluster,
        forced_target_enemies,
        protected_capture_entities,
        deploy_lead_time,
        player_id,
        policy,
    };

    // この作戦を「最寄りの作戦」とするユニットだけを、この作戦の担当として数える。
    // これにより 1 体のユニットが複数作戦に二重計上されない。
    let anchor_index = anchors.iter().position(|candidate| *candidate == anchor);

    // 基準占領ユニットが自力で到達できるかどうかで輸送要否が決まる。
    // 「島だから輸送が要る」ではなく「地形的に繋がっていないから要る」と判定する。
    let requires_transport = !scan.free_facilities.iter().any(|(f, _)| {
        ctx.is_reachable(
            &scan.map,
            &scan.master_data,
            (f.x, f.y),
            (anchor.x, anchor.y),
            reference.movement_type,
        )
    });

    // 敵戦力は、現在の局地敵と将来到着する増援に分離する。
    // 別島の敵Entityを具体的な撃破目標へ入れると、生産Entityがanchorを離れて
    // 敵初期空港まで追跡し、輸送・占領工程を前進させない。別島から期限内に
    // 到着できる敵はanchor到着時点の仮想増援として計画へ入れ、現在の任務対象にしない。
    //
    // (1) を基準占領ユニット（歩兵）の足で判定してはならない。
    // 海の向こうで拠点を取り続ける敵の占領部隊は、歩兵の足では届かないというだけで
    // 脅威の集計から丸ごと消え、撃破枠が立たず、対抗候補の採点対象にもならなくなる。
    // 実際にはヘリ・艦船・航空機で届くのだから、生産しうる移動タイプ全体で問う。
    // 「制空で応じるか対空で応じるか」が思想ではなく到達可能性の問題であるのと同じで、
    // 「敵の占領部隊を潰しに行けるか」もまた到達可能性の問題でしかない。
    let threats = observe_operation_threats(&input, ctx, anchor_index);
    let force = count_committed_force(&input, ctx, requires_transport, &threats);
    let enemy_reinforcement = if policy.reports_property_control() {
        anchor_index.map_or_else(EnemyReinforcementEnvelope::default, |index| {
            projected_enemy_reinforcement_envelope(scan, ctx, anchors, horizons, index)
        })
    } else {
        EnemyReinforcementEnvelope {
            expected_funds: anchor_index.map_or(0, |index| {
                projected_enemy_reinforcement_funds(scan, ctx, anchors, horizons, index)
            }),
            ..Default::default()
        }
    };
    let facts = observed_operation_facts(
        &input,
        requires_transport,
        &threats,
        &force,
        &enemy_reinforcement,
    );
    let mut operation = initialize_observed_operation(
        &input,
        ctx,
        anchor_index,
        threats,
        facts,
        &enemy_reinforcement,
    );
    assess_operation_reinforcements(&input, ctx, &mut operation, &enemy_reinforcement);
    operation
}

struct ThreatObservation {
    reachable_threats: Vec<ThreatTarget>,
    unreachable_threats: Vec<ThreatTarget>,
    enemy_contact_eta: u32,
}
struct CommittedForce {
    friendly_capture_units_committed: u32,
    friendly_combat_units_committed: u32,
    friendly_intercept_units_committed: u32,
    available_free_cargo_slots: u32,
}

fn observe_operation_threats(
    input: &OperationObservation<'_>,
    ctx: &mut ReachCtx,
    anchor_index: Option<usize>,
) -> ThreatObservation {
    let OperationObservation {
        scan,
        kind,
        anchor,
        anchors,
        horizons,
        assignment_enabled,
        forced_target_enemies,
        ..
    } = *input;
    let producible_movement_types: Vec<MovementType> = {
        let mut seen = HashSet::default();
        scan.available_types
            .iter()
            .map(|(_, stats)| stats.movement_type)
            .filter(|movement_type| seen.insert(*movement_type))
            .collect()
    };

    let mut reachable_threats = Vec::new();
    let mut unreachable_threats = Vec::new();
    let mut enemy_contact_eta = u32::MAX;
    let island_map = crate::ai::islands::IslandMap::analyze(&scan.map);
    let anchor_island = island_map.get_island_at(&anchor).map(|island| island.id);
    for enemy in &scan.enemy_units {
        let forced_target = enemy
            .entity
            .is_some_and(|entity| forced_target_enemies.contains(&entity));
        if !forced_target
            && nearest_reachable_anchor_index(
                scan,
                ctx,
                &enemy.pos,
                enemy.stats.movement_type,
                enemy.stats.max_movement,
                anchors,
                assignment_enabled,
            ) != anchor_index
        {
            continue;
        }
        let i_can_reach = producible_movement_types.iter().any(|movement_type| {
            ctx.is_reachable(
                &scan.map,
                &scan.master_data,
                (anchor.x, anchor.y),
                (enemy.pos.x, enemy.pos.y),
                *movement_type,
            )
        });
        let it_can_reach_me = ctx.is_reachable(
            &scan.map,
            &scan.master_data,
            (enemy.pos.x, enemy.pos.y),
            (anchor.x, anchor.y),
            enemy.stats.movement_type,
        );
        debug_assert!(
            forced_target || it_can_reach_me,
            "到達可能な最寄り作戦へ敵を帰属済み"
        );
        let arrival_eta = eta_turns(&scan.map, &enemy.pos, &anchor, enemy.stats.max_movement);
        let local_horizon = anchor_index
            .and_then(|index| horizons.get(index).copied())
            .unwrap_or(0);
        if kind == OperationKind::Capture && !forced_target && arrival_eta > local_horizon {
            // 同じ大陸の遠方敵をすべて局地護衛へ入れない。占領完了までにこの前線へ
            // 接触できない敵は、前線更新後または首都作戦Go後に別パッケージで扱う。
            continue;
        }
        if it_can_reach_me {
            enemy_contact_eta = enemy_contact_eta.min(arrival_eta);
        }
        let enemy_island = island_map.get_island_at(&enemy.pos).map(|island| island.id);
        let local_contact = enemy_island == anchor_island || arrival_eta == 0;
        record_enemy_contact(
            &mut reachable_threats,
            &mut unreachable_threats,
            EnemyContact {
                enemy,
                i_can_reach,
                it_can_reach_me,
                local_contact,
                arrival_eta,
                local_horizon,
                anchor,
            },
        );
    }
    ThreatObservation {
        reachable_threats,
        unreachable_threats,
        enemy_contact_eta,
    }
}

fn count_committed_force(
    input: &OperationObservation<'_>,
    ctx: &mut ReachCtx,
    requires_transport: bool,
    threats: &ThreatObservation,
) -> CommittedForce {
    let OperationObservation { scan, anchor, .. } = *input;
    let ThreatObservation { .. } = threats;

    let mut available_free_cargo_slots = 0u32;
    let mut force = CommittedForce {
        friendly_capture_units_committed: 0,
        friendly_combat_units_committed: 0,
        friendly_intercept_units_committed: 0,
        available_free_cargo_slots: 0,
    };
    for unit in &scan.my_units {
        record_committed_unit(input, ctx, requires_transport, threats, unit, &mut force);
    }
    let cargo_movements: Vec<MovementType> = {
        let mut seen = HashSet::default();
        scan.available_types
            .iter()
            .filter(|(_, stats)| stats.can_capture)
            .map(|(_, stats)| stats.movement_type)
            .filter(|movement_type| seen.insert(*movement_type))
            .collect()
    };
    for unit in &scan.my_units {
        if unit.free_cargo == 0 {
            continue;
        }
        let deliverable = cargo_movements.iter().any(|cargo_movement| {
            can_deliver_cargo(
                &scan.map,
                &scan.master_data,
                ctx,
                &unit.pos,
                &anchor,
                unit.stats.movement_type,
                *cargo_movement,
            )
        });
        if deliverable {
            available_free_cargo_slots = available_free_cargo_slots.saturating_add(unit.free_cargo);
        }
    }

    force.available_free_cargo_slots = available_free_cargo_slots;
    force
}

fn observed_operation_facts(
    input: &OperationObservation<'_>,
    requires_transport: bool,
    threats: &ThreatObservation,
    force: &CommittedForce,
    enemy_reinforcement: &EnemyReinforcementEnvelope,
) -> OperationFacts {
    let OperationObservation {
        scan,
        kind,
        cluster,
        deploy_lead_time,
        policy,
        ..
    } = *input;
    let enemy_combat_units = u32::try_from(threats.reachable_threats.len()).unwrap_or(u32::MAX);
    let unreachable_threat_units =
        u32::try_from(threats.unreachable_threats.len()).unwrap_or(u32::MAX);
    let enemy_contact_eta = threats.enemy_contact_eta;
    let CommittedForce {
        friendly_capture_units_committed,
        friendly_combat_units_committed,
        friendly_intercept_units_committed,
        available_free_cargo_slots,
    } = *force;
    // 輸送 1 往復にかかるターン数（片道リードタイムの 2 倍）
    let transport_round_trip_turns = deploy_lead_time.saturating_mul(2).max(1);

    OperationFacts {
        target_property_count: cluster.len() as u32,
        friendly_capture_units_committed,
        enemy_combat_units,
        friendly_combat_units_committed,
        // OperationFactsの既存フィールドはExpectedを保持する。Go判定には使わず、
        // RollingPlanへ渡す通常の継続生産scenarioだけに使う。
        enemy_reinforcement_funds: enemy_reinforcement.expected_funds,
        friendly_intercept_units_committed,
        deploy_lead_time,
        enemy_contact_eta: if enemy_contact_eta == u32::MAX {
            u32::MAX
        } else {
            enemy_contact_eta
        },
        requires_transport,
        transport_round_trip_turns,
        available_free_cargo_slots,
        unreachable_threat_units,
        // 序盤は、先に歩兵を敵の拠点へ送り込めた方が翼の拠点を取る。後手が1手番目に
        // 見えている敵の歩兵へ中戦車などで対抗すると、歩兵の数で負けて占領競争に
        // 出遅れる（map_20の後手で相手の翼を取れなかった）。
        opening_turn: policy == ProductionPolicy::Standard
            && kind == OperationKind::Capture
            && scan.game_turn <= 1,
    }
}

fn initialize_observed_operation(
    input: &OperationObservation<'_>,
    ctx: &mut ReachCtx,
    anchor_index: Option<usize>,
    threats: ThreatObservation,
    facts: OperationFacts,
    enemy_reinforcement: &EnemyReinforcementEnvelope,
) -> Operation {
    let OperationObservation {
        scan,
        kind,
        anchor,
        horizons,
        cluster,
        deploy_lead_time,
        player_id,
        policy,
        ..
    } = *input;
    let ThreatObservation {
        reachable_threats,
        unreachable_threats,
        ..
    } = threats;
    let mut slots = derive_slots(&facts);
    // Combat枠は金額の差分ではなく、観測敵が残っていることだけで計画器を起動する。
    // 敵の将来生産余力は再評価用の監視情報であり、必要数と完了判定に混ぜない。
    slots.combat_plan_required = u32::from(
        !reachable_threats.is_empty()
            || (policy == ProductionPolicy::Standard
                && kind == OperationKind::AssaultCapital
                && enemy_reinforcement.expected_funds > 0),
    );
    // AssaultCapitalも局地Captureと同じ作戦内で複数施設を前進目標として持つ。
    // 構造生産を島campaignが予約済みの手番は`allow_structural_slots`側で一括して
    // Capture/Transportを無効化するため、ここで恒久的に0へ落としてはならない。

    if policy.uses_joint_capture() {
        let enemy_capture_completion =
            enemy_capture_completion_offset(scan, ctx, &anchor, player_id);
        if kind == OperationKind::Capture
            && capture_race_behind(deploy_lead_time, enemy_capture_completion, player_id)
        {
            // 敵が先に占領完了する物件へ、占領枠を複数方向へ広げない。
            // 遅い占領兵を増やすだけの投資を止め、予算を妨害・奪還（Combat）側へ残す。
            // 最小 1 枠は、敵排除後に物件へ入れる奪還要員として残す。
            slots.capture_units = slots.capture_units.min(1);
        }
    }

    Operation {
        kind,
        island_id: None,
        operation_id: None,
        anchor,
        staging_anchor: anchor,
        execution_authorized: true,
        protected_capture_entities: HashSet::default(),
        capture_replacement_pending: false,
        objective_properties: cluster.to_vec(),
        property_control: None,
        property_controls: Vec::new(),
        capture_lane_targets: Vec::new(),
        planned_capture_units: Vec::new(),
        threat_horizon: anchor_index
            .and_then(|index| horizons.get(index).copied())
            .unwrap_or(0),
        slots,
        facts,
        filled: OperationSlots::default(),
        unreachable_threats,
        reachable_threats,
        expected_reinforcements: Vec::new(),
        unavoidable_reinforcements: Vec::new(),
        reinforcement_contingencies: Vec::new(),
        stress_reinforcement_funds: enemy_reinforcement.stress_funds,
        contingency_reserve_funds: 0,
    }
}

fn assess_operation_reinforcements(
    input: &OperationObservation<'_>,
    ctx: &mut ReachCtx,
    operation: &mut Operation,
    enemy_reinforcement: &EnemyReinforcementEnvelope,
) {
    let OperationObservation { scan, policy, .. } = *input;
    if policy.reports_property_control() {
        let horizon = operation.threat_horizon.max(1);
        let expected_assessment = enemy_reinforcement_assessment(
            scan,
            ctx,
            operation,
            horizon,
            enemy_reinforcement.expected_funds,
            ReinforcementScenario::Expected,
            policy,
        );
        let stress_assessment = enemy_reinforcement_assessment(
            scan,
            ctx,
            operation,
            horizon,
            enemy_reinforcement.stress_funds,
            ReinforcementScenario::Stress,
            policy,
        );
        // Expectedは現在のRollingPlanへ仮想敵として渡す。Go判定とは独立しているため、
        // 最低限の戦力で進撃を開始しつつ、到達時刻付きの次波へ備えた生産を続けられる。
        operation.expected_reinforcements = expected_assessment.reinforcements;
        // 可視敵がいなくても、期限内に前線へ到着するExpectedがあれば継続生産を開始する。
        // これは戦闘開始のGoではなく、到着予定の敵に対する編成開始である。
        operation.slots.combat_plan_required = u32::from(
            !operation.reachable_threats.is_empty()
                || !operation.expected_reinforcements.is_empty(),
        );
        operation.reinforcement_contingencies = stress_assessment.contingencies;
        // Stressは現在のGoや現金を固定しない。観測時に実兵種・実HPで再計画するための
        // counter候補として残し、Expected生産を予約資金で止めない。
        operation.contingency_reserve_funds = 0;
    } else {
        let assessment = enemy_reinforcement_assessment(
            scan,
            ctx,
            operation,
            operation.threat_horizon.max(1),
            operation.facts.enemy_reinforcement_funds,
            ReinforcementScenario::Stress,
            policy,
        );
        operation.unavoidable_reinforcements = assessment.unavoidable;
        operation.reinforcement_contingencies = assessment.contingencies;
        operation.contingency_reserve_funds =
            contingency_reserve_now(&operation.reinforcement_contingencies, scan.my_income);
        if !operation.unavoidable_reinforcements.is_empty() {
            operation.slots.combat_plan_required = 1;
        }
    }
}

pub(in crate::ai::v4) fn enemy_reinforcement_assessment(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    horizon: u32,
    scenario_budget: u32,
    scenario: ReinforcementScenario,
    policy: ProductionPolicy,
) -> ReinforcementAssessment {
    if scenario_budget == 0 {
        return ReinforcementAssessment::default();
    }
    let mut budget = 0_u32;
    let mut funded = 0_u32;
    let mut assessment = ReinforcementAssessment::default();
    let mut reserved_counter_slots = HashSet::default();
    let friendly_combat_types = scan
        .available_types
        .iter()
        .filter(|(_, stats)| !stats.can_capture && stats.max_cargo == 0)
        .map(|(_, stats)| stats)
        .collect::<Vec<_>>();
    let observed_enemy_types = op
        .reachable_threats
        .iter()
        .map(|threat| threat.stats.unit_type)
        .collect::<HashSet<_>>();
    // 観測済みの局地兵種と直近生産の主兵種をExpectedの候補集合にする。
    // dominant一種類だけへ絞ると、敵の航空・地上混成を一方の相性だけで見積もり、
    // 対空や占領阻止を後追いにするため、少なくとも観測兵種の混成を維持する。
    let mut expected_enemy_types = observed_enemy_types.clone();
    if policy.reports_property_control()
        && let Some(unit_type) = scan.enemy_production_forecast.dominant_unit_type
    {
        expected_enemy_types.insert(unit_type);
    }

    let last_build_turn = match (policy, scenario) {
        (ProductionPolicy::SmallMap, ReinforcementScenario::Expected) => {
            EXPECTED_REINFORCEMENT_WAVES.min(horizon.saturating_sub(1))
        }
        _ => horizon.saturating_sub(1),
    };
    for build_turn in 1..=last_build_turn {
        let mut projected_types = HashSet::default();
        let income = scan
            .enemy_income
            .min(scenario_budget.saturating_sub(funded));
        budget = budget.saturating_add(income);
        funded = funded.saturating_add(income);
        for facility in &scan.enemy_facilities {
            let mut candidates = scan
                .available_types
                .iter()
                .filter(|(unit_type, stats)| {
                    stats.max_cargo == 0
                        && stats.cost > 0
                        && stats.cost <= budget
                        // Expectedは観測済み混成と生産履歴の主兵種を交互に見積もる。
                        // 観測が無い場合だけ、到達可能な兵種から通常の前線圧を選ぶ。
                        && (policy == ProductionPolicy::Standard || scenario == ReinforcementScenario::Stress
                            || expected_enemy_types.is_empty()
                            || expected_enemy_types.contains(&stats.unit_type)
                            || stats.can_capture)
                        && scan.can_produce(facility.terrain, *unit_type)
                        && ctx.is_reachable(
                            &scan.map,
                            &scan.master_data,
                            (facility.pos.x, facility.pos.y),
                            (op.anchor.x, op.anchor.y),
                            stats.movement_type,
                        )
                })
                .filter_map(|(_, stats)| {
                    let eta = eta_turns(&scan.map, &facility.pos, &op.anchor, stats.max_movement);
                    let available_turn = if policy == ProductionPolicy::SmallMap { property_control::produced_action_offset(build_turn, eta) } else { build_turn.saturating_add(1).saturating_add(eta) };
                    if available_turn > horizon {
                        return None;
                    }
                    let counter_damage = friendly_combat_types
                        .iter()
                        .map(|friendly| {
                            best_damage(&scan.damage_chart, stats.unit_type, friendly.unit_type)
                        })
                        .max()
                        .unwrap_or_default();
                    let can_be_engaged = friendly_combat_types.iter().any(|friendly| {
                        best_damage(&scan.damage_chart, friendly.unit_type, stats.unit_type) > 0
                    });
                    // Expectedでは、現在の自軍カタログに対抗兵種が無いこと自体が
                    // 生産計画で解くべき不足である。ここで敵候補を捨てると
                    // 「まだcounterを持たないから敵もいない」と誤認してしまう。
                    ((policy == ProductionPolicy::SmallMap && scenario == ReinforcementScenario::Expected) || can_be_engaged).then_some((
                        std::cmp::Reverse(counter_damage),
                        std::cmp::Reverse(u32::from(stats.can_capture)),
                        std::cmp::Reverse(stats.cost),
                        available_turn,
                        stats,
                    ))
                })
                .collect::<Vec<_>>();
            // 前線争奪では施設を奪える増援の頭数を最初にstress testする。
            // 最大単発火力を先にすると、高価な一体だけを仮定して安価な占領兵の
            // 連続生産を見落とすため、占領能力→対友軍火力→費用の順にする。
            candidates.sort_by_key(|candidate| {
                let stats = candidate.4;
                (
                    std::cmp::Reverse(
                        expected_enemy_types.contains(&stats.unit_type)
                            && !projected_types.contains(&stats.unit_type),
                    ),
                    candidate.1,
                    candidate.0,
                    candidate.2,
                )
            });
            let Some((_, _, _, available_turn, stats)) = candidates.into_iter().next() else {
                continue;
            };
            projected_types.insert(stats.unit_type);
            budget = budget.saturating_sub(stats.cost);
            let reinforcement = EnemyPlanUnit {
                entity: None,
                stats: stats.clone(),
                // ETA到達後は作戦anchorに接触するものとして交戦時間を見積もる。
                position: op.anchor,
                hp: 100,
                defense_bonus: scan
                    .map
                    .get_terrain(op.anchor.x, op.anchor.y)
                    .map_or(0, |terrain| {
                        scan.master_data.get_terrain_defense_bonus(terrain)
                    }),
                available_turn,
            };
            // Expectedは通常想定される到着戦力をすべてRollingPlanへ渡す。これが
            // 「最低Goは満たしたから生産終了」を防ぐ継続戦力の基準になる。
            // Stressだけは観測後に間に合うcounterを条件付き計画として残す。
            if (policy == ProductionPolicy::SmallMap && scenario == ReinforcementScenario::Expected)
                || stats.can_capture
                || observed_enemy_types.contains(&stats.unit_type)
            {
                if policy == ProductionPolicy::SmallMap {
                    assessment.reinforcements.push(reinforcement);
                } else {
                    assessment.unavoidable.push(reinforcement);
                }
            } else if let Some(contingency) = fastest_observed_counter(
                scan,
                ctx,
                op.anchor,
                stats,
                build_turn,
                available_turn,
                &reserved_counter_slots,
            ) {
                reserved_counter_slots
                    .insert((contingency.counter_build_turn, contingency.counter_facility));
                assessment.contingencies.push(contingency);
            } else {
                if policy == ProductionPolicy::SmallMap {
                    assessment.reinforcements.push(reinforcement);
                } else {
                    assessment.unavoidable.push(reinforcement);
                }
            }
        }
    }
    assessment
}

fn apply_property_control(
    input: &OperationAssembly<'_, '_>,
    operation: &mut Operation,
    cluster: &[GridPosition],
    property_control_plan: Option<property_control::PropertyControlPlan>,
    planning_objective: Option<&CampaignPlanningObjective>,
) {
    let OperationAssembly { scan, policy, .. } = *input;
    let property_control = property_control_plan.as_ref().map(|plan| plan.primary);
    if policy.reports_property_control() {
        operation.property_control = property_control;
        configure_capture_lanes(scan, operation, cluster, property_control_plan);
        // NOTE: V4中央作戦契約との同期は呼び出し元（plan_production_with_registry）で実行する
        project_property_control_threats(scan, operation);
        if campaign_scope_audit_enabled() {
            eprintln!(
                "CAMPAIGN_SCOPE_AUDIT stage=binding turn={} op_kind={:?} op_anchor={:?} op_properties={:?} bound={:?}",
                scan.current_turn,
                operation.kind,
                operation.anchor,
                operation.objective_properties,
                planning_objective.map(|objective| (
                    objective.island_id,
                    objective.kind,
                    objective.anchor,
                    objective.required_capture_survivors,
                    objective.protected_capture_entities.len(),
                )),
            );
        }
    }
}

fn record_committed_unit(
    input: &OperationObservation<'_>,
    ctx: &mut ReachCtx,
    requires_transport: bool,
    threats: &ThreatObservation,
    unit: &UnitSnapshot,
    force: &mut CommittedForce,
) {
    let OperationObservation {
        scan,
        anchor,
        anchors,
        horizons,
        assignment_enabled,
        policy,
        ..
    } = *input;
    let anchor_index = anchors.iter().position(|candidate| *candidate == anchor);
    let ThreatObservation {
        reachable_threats,
        unreachable_threats,
        ..
    } = threats;

    if is_assigned_capture(input, ctx, unit, requires_transport) {
        force.friendly_capture_units_committed += 1;
    }
    // 小規模では武装carrierも戦闘へ参加し、標準では従来の排他仕分けを保つ。
    if policy.counts_armed_carriers() || (!unit.stats.can_capture && unit.stats.max_cargo == 0) {
        // 迎撃枠の条件：到達できない脅威へ有効打を持ち、かつ自力で現地へ行ける
        let self_deployable = ctx.is_reachable(
            &scan.map,
            &scan.master_data,
            (unit.pos.x, unit.pos.y),
            (anchor.x, anchor.y),
            unit.stats.movement_type,
        );
        if self_deployable
            && threats_have_counter(
                &unit.stats,
                unreachable_threats,
                &(0..unreachable_threats.len()).collect::<Vec<_>>(),
                &scan.damage_chart,
            )
        {
            force.friendly_intercept_units_committed =
                force.friendly_intercept_units_committed.saturating_add(1);
        }
        // 撃破枠の条件：現地へ行けて、自分が実際に届く敵に対して有効打を持つ。
        // 敵が観測できない段階では誰でも採用されうるので、台帳側も同様に全員を数える。
        if !can_join_operation(
            scan,
            ctx,
            &anchor,
            requires_transport,
            &unit.pos,
            &unit.stats,
        ) {
            return;
        }
        let origin = if self_deployable { unit.pos } else { anchor };
        let engageable =
            reachable_threat_indices(scan, ctx, reachable_threats, origin, &unit.stats);
        let combat_eligible = reachable_threats.is_empty()
            || threats_have_counter(
                &unit.stats,
                reachable_threats,
                &engageable,
                &scan.damage_chart,
            );
        if combat_eligible {
            // 1体が同じ手番に攻撃できる作戦は1つだけである。unit価格による
            // 戦力価値もsortie体数も、期限内に到着できる最寄り作戦へ排他的に
            // 帰属させ、全前線で同じ1体を重複控除しない。
            let belongs_to_control_operation = nearest_relevant_anchor_index(
                scan,
                ctx,
                &unit.pos,
                unit.stats.movement_type,
                unit.stats.max_movement,
                anchors,
                horizons,
                assignment_enabled,
            ) == anchor_index;
            if !belongs_to_control_operation {
                return;
            }
            force.friendly_combat_units_committed =
                force.friendly_combat_units_committed.saturating_add(1);
        }
    }
}

struct EnemyContact<'a> {
    enemy: &'a UnitSnapshot,
    i_can_reach: bool,
    it_can_reach_me: bool,
    local_contact: bool,
    arrival_eta: u32,
    local_horizon: u32,
    anchor: GridPosition,
}
fn record_enemy_contact(
    reachable_threats: &mut Vec<ThreatTarget>,
    unreachable_threats: &mut Vec<ThreatTarget>,
    contact: EnemyContact<'_>,
) {
    let EnemyContact {
        enemy,
        i_can_reach,
        it_can_reach_me,
        local_contact,
        arrival_eta,
        local_horizon,
        anchor,
    } = contact;
    if i_can_reach && local_contact {
        reachable_threats.push(ThreatTarget::from_snapshot(enemy));
    } else if !i_can_reach && local_contact {
        unreachable_threats.push(ThreatTarget::from_snapshot(enemy));
    } else if it_can_reach_me
        // 空の輸送unitは単独では占領も攻撃もできない。全ての中立前線で
        // 「将来来る輸送ヘリ」として対空購入を発生させず、実cargoを搭載して
        // 作戦を開始した時点から到着scenarioへ含める。
        && (enemy.stats.max_cargo == 0 || enemy.free_cargo < enemy.stats.max_cargo)
        && arrival_eta
            <= local_horizon
    {
        // 具体Entityを追わせず、到着後にだけ攻撃可能な増援として保持する。
        let mut incoming = ThreatTarget::from_snapshot(enemy);
        incoming.entity = None;
        incoming.position = anchor;
        incoming.available_turn = arrival_eta.max(1);
        reachable_threats.push(incoming);
    }
}

fn is_assigned_capture(
    input: &OperationObservation<'_>,
    ctx: &mut ReachCtx,
    unit: &UnitSnapshot,
    requires_transport: bool,
) -> bool {
    unit.stats.can_capture
        && unit
            .entity
            .is_some_and(|entity| input.protected_capture_entities.contains(&entity))
        && can_join_operation(
            input.scan,
            ctx,
            &input.anchor,
            requires_transport,
            &unit.pos,
            &unit.stats,
        )
}

fn configure_capture_lanes(
    scan: &BoardScan,
    operation: &mut Operation,
    cluster: &[GridPosition],
    property_control_plan: Option<property_control::PropertyControlPlan>,
) {
    let kind = operation.kind;
    let anchor = operation.anchor;
    if let Some(plan) = property_control_plan {
        operation.property_controls = plan.controls;
        let current_capture_facility_capacity = scan
            .free_facilities
            .iter()
            .filter(|(_, terrain)| {
                scan.available_types.iter().any(|(unit_type, stats)| {
                    stats.can_capture && scan.can_produce(*terrain, *unit_type)
                })
            })
            .count();
        // 既にこの作戦へ確保済みの占領役は先頭レーンを担当済みとみなし、
        // 今回新規に必要な任務先だけを残す。今手番に占領役を作れない施設数を
        // 超えて仮想保護対象を要求せず、作戦自身の対象クラスタの未所有拠点数および
        // 最大波サイズに従う。別前線の遠隔物件まで先取りして戦闘予算を枯渇させない。
        let cluster_target_cap = cluster
            .iter()
            .filter(|p| scan.open_properties.contains(p))
            .count();
        let is_small_map_expansion =
            scan.opening_policy == Some(OpeningProductionPolicy::SmallMapExpansion);
        let mut capture_lanes = plan.capture_lanes;
        if is_small_map_expansion && let Some(my_cap) = scan.capital_position {
            let enemy_cap = scan
                .enemy_facilities
                .iter()
                .find_map(|f| (f.terrain == Terrain::Capital).then_some(f.pos));
            capture_lanes.sort_by(|a, b| {
                let d_my_a = scan.map.distance(a.x, a.y, my_cap.x, my_cap.y);
                let d_my_b = scan.map.distance(b.x, b.y, my_cap.x, my_cap.y);
                let d_en_a = enemy_cap
                    .map(|ec| scan.map.distance(a.x, a.y, ec.x, ec.y))
                    .unwrap_or(0);
                let d_en_b = enemy_cap
                    .map(|ec| scan.map.distance(b.x, b.y, ec.x, ec.y))
                    .unwrap_or(0);
                d_my_b
                    .cmp(&d_my_a)
                    .then_with(|| d_en_a.cmp(&d_en_b))
                    .then_with(|| (a.y, a.x).cmp(&(b.y, b.x)))
            });
        }
        operation.capture_lane_targets = capture_lanes
            .into_iter()
            .skip(operation.facts.friendly_capture_units_committed as usize)
            .take(current_capture_facility_capacity)
            .take(cluster_target_cap.max(1))
            .take(MAX_CAPTURE_SLOTS as usize)
            .collect();
        operation.slots.capture_units =
            u32::try_from(operation.capture_lane_targets.len()).unwrap_or(u32::MAX);
    } else if kind == OperationKind::Capture {
        // 敵競合が存在せず物件制御計画が成立しない平時Capture作戦でも、
        // cluster内の未取得拠点を重複なく割り当て、同一anchorへの二重配備を防ぐ。
        let is_anchor_open = scan.open_properties.contains(&anchor) && cluster.contains(&anchor);
        let mut targets = Vec::new();
        if is_anchor_open {
            targets.push(anchor);
        }
        let mut rest: Vec<GridPosition> = cluster
            .iter()
            .copied()
            .filter(|p| scan.open_properties.contains(p) && (!is_anchor_open || *p != anchor))
            .collect();
        rest.sort_unstable_by_key(|p| (scan.map.distance(anchor.x, anchor.y, p.x, p.y), p.y, p.x));
        rest.dedup();
        targets.extend(rest);
        operation.capture_lane_targets = targets
            .into_iter()
            .take(MAX_CAPTURE_SLOTS as usize)
            .collect();
        operation.slots.capture_units = operation
            .slots
            .capture_units
            .min(u32::try_from(operation.capture_lane_targets.len()).unwrap_or(u32::MAX));
    }
}

fn project_property_control_threats(scan: &BoardScan, operation: &mut Operation) {
    if operation
        .property_controls
        .iter()
        .any(|control| control.disaggregated)
    {
        // 他の可視脅威を消さず、全ての物件別期限対象を戦闘計画へ含める。
        for control in &operation.property_controls {
            if !operation
                .reachable_threats
                .iter()
                .any(|threat| threat.entity == Some(control.enemy_entity))
                && let Some(enemy) = scan
                    .enemy_units
                    .iter()
                    .find(|enemy| enemy.entity == Some(control.enemy_entity))
            {
                operation
                    .reachable_threats
                    .push(ThreatTarget::from_snapshot(enemy));
            }
        }
        let priority_entities = operation
            .property_controls
            .iter()
            .map(|control| control.enemy_entity)
            .collect::<Vec<_>>();
        operation.reachable_threats.sort_by_key(|threat| {
            priority_entities
                .iter()
                .position(|entity| threat.entity == Some(*entity))
                .unwrap_or(usize::MAX)
        });
        operation.slots.combat_plan_required = u32::from(!operation.reachable_threats.is_empty());
    }
}
