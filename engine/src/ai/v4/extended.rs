//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

use super::board_scan::{campaign_scope_audit_enabled, scanned_occupants};
use super::property_control::PlannedCaptureUnit;
use super::*;
use crate::ai::v4::capital_route::model::{
    AdvancingCapitalRouteEntities, CapitalRoutePathRegistry,
};
use crate::ai::v4::enemy_forecast::{
    EXPECTED_REINFORCEMENT_WAVES, EnemyReinforcementEnvelope, ReinforcementScenario,
    observe_enemy_production, projected_enemy_reinforcement_envelope,
};
use crate::ai::v4::property_control::{
    EARLY_PROPERTY_CONTROL_HORIZON, PropertyControlApproach, capture_race_behind,
    committed_capture_arrival, enemy_capture_completion_offset, enemy_is_capturing,
    next_capture_target, player_order, produced_attack_turn_along_capture_route,
    property_durability, select_property_capture_candidate, select_property_control_plan,
};

#[allow(unused_imports)]
use crate::ai::planning::decision::AdoptedTurnPortfolio;

/// V4 の生産意思決定エントリポイント。
///
/// `decide_production` から `AiVersion::uses_operation_driven_production()` が
/// true のときだけ委譲される。V1/V2/V3 の経路には一切影響しない。
pub(crate) fn decide_production_v4(
    world: &mut World,
    player_id: PlayerId,
) -> Vec<ProduceUnitCommand> {
    if !crate::ai::code_path::ext_enabled("v4/mod.rs::decide_production_v4") {
        return super::decide_production_v4(world, player_id);
    }
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    // 島嶼キャンペーン生産だけでreturnする手番も含め、作戦の実績は毎ターン観測する。
    // 生産APIは施設ごとに呼ばれるため、同じ手番の2体目以降では再走査しない。
    let already_observed = world
        .get_resource::<V4ProductionObservationCache>()
        .is_some_and(|cache| cache.player_id == Some(player_id) && cache.turn == turn);
    if !already_observed {
        observe_plan_execution(world, player_id, turn);
        world.insert_resource(V4ProductionObservationCache {
            player_id: Some(player_id),
            turn,
        });
    }

    let campaign_surplus = match decide_campaign_production_v4(world, player_id) {
        CampaignProductionControl::Command(command) => return vec![command],
        CampaignProductionControl::BlockGeneric => return Vec::new(),
        CampaignProductionControl::ContinueWithSurplus(budget) => Some(budget),
        CampaignProductionControl::Continue => None,
    };

    let turn_plan = world
        .remove_resource::<V4ProductionTurnPlan>()
        .unwrap_or_default();
    if turn_plan.player_id == Some(player_id) && turn_plan.turn == turn {
        return issue_from_turn_plan(world, turn, turn_plan);
    }

    let Some(mut scan) = BoardScan::collect(world, player_id) else {
        world.insert_resource(turn_plan);
        return Vec::new();
    };
    // 敵の初期配置は学習せず、前回走査後に現れた実Entityだけで次手番を予測する。
    let enemy_production_forecast = observe_enemy_production(world, player_id, turn, &scan);
    scan.enemy_production_forecast = enemy_production_forecast;
    if let Some(budget) = campaign_surplus {
        scan.funds = scan.funds.min(budget);
    }
    // 既存戦力として見積へ入れるのは、実際にV4 Combat任務へ接続済みのEntityだけ。
    // 占領・輸送など別任務中のunitを「倒せるはず」と二重計上しない。
    let committed_combat_assignments = world
        .get_resource::<deployment::V4DeploymentRegistry>()
        .map(|registry| registry.active_target_assignments(player_id))
        .unwrap_or_default();
    // Squad計画でDAGへ束縛した前進部隊を、首都作戦の既存戦力として同じ手番の
    // 生産plannerへ渡す。PlanIdの有無ではなく、親戦役の未完了区間へ実際に進む
    // という作戦状態を正本にする。
    let advancing_route_entities = world
        .get_resource::<CapitalRoutePathRegistry>()
        .zip(world.get_resource::<crate::ai::squad::SquadManager>())
        .map(|(registry, manager)| registry.advancing_combat_entities(player_id, manager))
        .unwrap_or_default();
    let produced_plan_steps = world
        .get_resource::<deployment::V4DeploymentRegistry>()
        .map(|registry| registry.produced_plan_steps(player_id))
        .unwrap_or_default();
    let mut rolling_registry = world
        .remove_resource::<V4RollingPlanRegistry>()
        .unwrap_or_default();
    let mut contract_registry = world
        .remove_resource::<plan_contract::PlanContractRegistry>()
        .unwrap_or_default();
    rolling_registry.reconcile_produced_steps(player_id, &produced_plan_steps);
    let (planned, mut plan_trace) = plan_production_with_registry(
        &scan,
        player_id,
        campaign_surplus.is_none(),
        &committed_combat_assignments,
        &advancing_route_entities,
        turn,
        &mut rolling_registry,
        &mut contract_registry,
    );
    plan_trace.enemy_production_forecast = enemy_production_forecast;
    world.insert_resource(contract_registry);
    commit_turn_production(
        world,
        player_id,
        turn,
        &scan,
        planned,
        plan_trace,
        rolling_registry,
        turn_plan,
    )
}

/// 盤面から作戦の一覧を組み立てる。
/// 分析関数として副作用を持たず、契約レジストリの同期は呼び出し元で行う。
pub(super) fn build_operations(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    active_objectives: &[ActivePlanObjective],
    player_id: PlayerId,
) -> Vec<Operation> {
    let Some(reference) = scan.reference_capture_unit().cloned() else {
        return Vec::new();
    };

    // V4の作戦源は、島campaignが選んだCapture/Defenseと、既に実行を始めた
    // Combat planだけに限定する。全未所有拠点を距離で束ねる第二の目標選定器を
    // 併用すると、勝利ロードマップ外の前線へ生産予算が流れるためである。
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

    let campaign_for_cluster = |kind: OperationKind, cluster: &[GridPosition]| {
        campaign_clusters
            .iter()
            .find_map(|(objective, properties)| {
                (objective.kind == kind
                    && cluster.iter().any(|property| properties.contains(property)))
                .then_some(*objective)
            })
            .or_else(|| {
                // Capture→Defense/Contestの遷移でも、島そのものが同じなら同一campaign。
                campaign_clusters.iter().find_map(|(objective, _)| {
                    cluster
                        .iter()
                        .any(|property| {
                            island_map
                                .get_island_at(property)
                                .is_some_and(|island| island.id == objective.island_id)
                        })
                        .then_some(*objective)
                })
            })
    };

    // 継続中Planのsnapshotと当ターンRoadmapは、同じ島・同じ目的のOperationを
    // 同時に提示し得る。両方を別候補として残すと同じAssaultCapitalがCapture枠を
    // 二重要求し、戦闘生産を毎ターン2体分の歩兵で圧迫する。Plan継続性は後段の
    // `continuing` で合流できるため、campaign identityごとに一件へ正規化する。
    let mut seen_campaign_operations = HashSet::default();
    raw.retain(|(kind, cluster)| {
        campaign_for_cluster(*kind, cluster).is_none_or(|objective| {
            seen_campaign_operations.insert((objective.kind.priority_rank(), objective.island_id))
        })
    });

    // 上位campaignと永続Planが所有する作戦を必須として識別し、追加の局地候補だけを
    // 同時実行容量へ収める。固定4件で第5の戦略作戦を消してはならない。
    let mut scored: Vec<OperationCandidate> = raw
        .into_iter()
        .filter(|(_, cluster)| !cluster.is_empty())
        .map(|(kind, cluster)| {
            let campaign = campaign_for_cluster(kind, &cluster);
            let anchor =
                campaign.map_or_else(|| anchor_of(&cluster, scan), |objective| objective.anchor);
            let lead = facility_lead_time(scan, &anchor, reference.max_movement);
            let continuing = active_objectives.iter().any(|objective| {
                campaign_for_cluster(kind, &cluster)
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
        let logistics_rank = campaign_for_cluster(candidate.kind, &candidate.cluster)
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
    let scored = select_operation_candidates(scored);

    // Capture clusterは代表点一つで評価せず、敵占領役ごとに成立する物件制御を選ぶ。
    // これにより中央が間に合わない場合も、左右の合法なInterdictを候補に残す。
    let neutral_race_owner = scored
        .iter()
        .position(|candidate| candidate.kind == OperationKind::Capture);
    let property_control_plans = scored
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
        .collect::<Vec<_>>();
    let anchors: Vec<GridPosition> = scored
        .iter()
        .zip(&property_control_plans)
        .map(|(candidate, property_control_plan)| {
            property_control_plan.as_ref().map_or_else(
                || {
                    campaign_for_cluster(candidate.kind, &candidate.cluster).map_or_else(
                        || anchor_of(&candidate.cluster, scan),
                        |objective| objective.staging_anchor,
                    )
                },
                |plan| plan.primary.property,
            )
        })
        .collect();
    let operation_leads = anchors
        .iter()
        .map(|anchor| facility_lead_time(scan, anchor, reference.max_movement))
        .collect::<Vec<_>>();
    let horizons: Vec<u32> = scored
        .iter()
        .enumerate()
        .map(|(index, candidate)| operation_threat_horizon(candidate.kind, operation_leads[index]))
        .collect();
    let assignment_enabled = scored
        .iter()
        .map(|candidate| {
            campaign_for_cluster(candidate.kind, &candidate.cluster).is_none_or(|objective| {
                objective.kind != OperationKind::AssaultCapital || objective.execution_authorized
            })
        })
        .collect::<Vec<_>>();
    let empty_entity_set = HashSet::default();

    scored
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| {
            let OperationCandidate { kind, cluster, .. } = candidate;
            let anchor = anchors[index];
            let property_control_plan = property_control_plans[index].clone();
            let property_control = property_control_plan.as_ref().map(|plan| plan.primary);
            let lead = operation_leads[index];
            let planning_objective = campaign_for_cluster(kind, &cluster);
            let forced_target_enemies = planning_objective
                .map(|objective| &objective.forced_target_enemies)
                .or_else(|| {
                    active_objectives
                        .iter()
                        .find(|objective| {
                            planning_objective.is_some_and(|campaign| {
                                objective.island_id == Some(campaign.island_id)
                            }) || (objective.island_id.is_none()
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
                &reference,
                kind,
                anchor,
                &anchors,
                &horizons,
                &assignment_enabled,
                &cluster,
                &local_forced_target_enemies,
                protected_capture_entities,
                lead,
                player_id,
            );
            operation.property_control = property_control;
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
                let is_anchor_open =
                    scan.open_properties.contains(&anchor) && cluster.contains(&anchor);
                let mut targets = Vec::new();
                if is_anchor_open {
                    targets.push(anchor);
                }
                let mut rest: Vec<GridPosition> = cluster
                    .iter()
                    .copied()
                    .filter(|p| {
                        scan.open_properties.contains(p) && (!is_anchor_open || *p != anchor)
                    })
                    .collect();
                rest.sort_unstable_by_key(|p| {
                    (scan.map.distance(anchor.x, anchor.y, p.x, p.y), p.y, p.x)
                });
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

            // NOTE: V4中央作戦契約との同期は呼び出し元（plan_production_with_registry）で実行する
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
                operation.slots.combat_plan_required =
                    u32::from(!operation.reachable_threats.is_empty());
            }
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
            if let Some(objective) = planning_objective {
                operation.island_id = Some(objective.island_id);
                operation.staging_anchor = objective.staging_anchor;
                operation.execution_authorized = objective.execution_authorized;
                operation.protected_capture_entities = objective.protected_capture_entities.clone();
                operation.capture_replacement_pending = objective.capture_replacement_pending;
            } else if let Some(active) = active_objectives
                .iter()
                .find(|objective| objective.kind == kind && objective.properties == cluster)
            {
                // portfolioがObserveへ一時遷移しても、active Plan由来のOperationは
                // 島identityを失わない。敵やanchorの変化で新Planへ分裂させない。
                operation.island_id = active.island_id;
            }
            operation
        })
        .collect()
}

pub(super) use super::operation_threat_horizon;

/// 1 つの作戦について観測量を集め、枠を導出する。
#[allow(clippy::too_many_arguments)]
pub(super) fn build_operation(
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
) -> Operation {
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
                <= horizons
                    .get(anchor_index.unwrap_or(usize::MAX))
                    .copied()
                    .unwrap_or(0)
        {
            // 具体Entityを追わせず、到着後にだけ攻撃可能な増援として保持する。
            let mut incoming = ThreatTarget::from_snapshot(enemy);
            incoming.entity = None;
            incoming.position = anchor;
            incoming.available_turn = arrival_eta.max(1);
            reachable_threats.push(incoming);
        }
    }
    let enemy_combat_units = u32::try_from(reachable_threats.len()).unwrap_or(u32::MAX);
    let unreachable_threat_units = u32::try_from(unreachable_threats.len()).unwrap_or(u32::MAX);
    // --- 自軍戦力の仕分け ---
    // 敵の仕分けが済んでから数える。
    //
    // 台帳（既存戦力の計上）は、必ず `slot_fitness` の採用条件と同じ判定を使う。
    // 両者がずれると「その枠を埋められるのに、その枠の充足としては数えられない」
    // ユニットが生まれ、枠の要求が永久に減らずに同じユニットを買い続けるラチェットになる。
    // そのため排他に振り分けず、埋められる枠すべてに計上する。
    // （1 体の対空ユニットが航空脅威の抑止と地上の頭数を兼ねるのは実態としても正しい）
    //
    // ここで「最寄りの作戦か」で仕分けてはならない。自軍ユニットは自陣の生産施設で
    // 生まれるので、位置で排他に割り振ると母港に近い作戦が全部を吸い、渡洋作戦側の
    // 台帳は永久に 0 のままになる。要求が一切減らないので同じユニットを毎ターン
    // 買い続けるラチェットになる（実測で歩兵 56 体、揚陸艇 7 隻、対空 20 両）。
    // 上限（`MAX_CAPTURE_SLOTS` 等）はあくまで 1 波の規模であって、
    // 「既に持っている分」を差し引く役割は担っていない。差し引きはこの台帳の仕事。
    let mut friendly_capture_units_committed = 0u32;
    let mut friendly_combat_units_committed = 0u32;
    let mut friendly_intercept_units_committed = 0u32;
    let mut available_free_cargo_slots = 0u32;
    for unit in &scan.my_units {
        if unit.stats.can_capture {
            // Attack任務の歩兵までCapture充足へ数えると、実際の担当が全滅しても
            // 補充が止まる。排他割当でこの作戦のCapture役になっているEntityだけを
            // 構造枠へ計上し、Combat側では同じ歩兵の戦闘・生存能力を別途利用する。
            if unit
                .entity
                .is_some_and(|entity| protected_capture_entities.contains(&entity))
                && can_join_operation(
                    scan,
                    ctx,
                    &anchor,
                    requires_transport,
                    &unit.pos,
                    &unit.stats,
                )
            {
                friendly_capture_units_committed += 1;
            }
        }
        // 搭載能力ではなく、実在脅威への有効打と到達性で戦闘参加を判定する。
        // 装甲車のような武装carrierを輸送台帳だけへ隔離しない。
        {
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
                    &unreachable_threats,
                    &(0..unreachable_threats.len()).collect::<Vec<_>>(),
                    &scan.damage_chart,
                )
            {
                friendly_intercept_units_committed =
                    friendly_intercept_units_committed.saturating_add(1);
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
                continue;
            }
            let origin = if self_deployable { unit.pos } else { anchor };
            let engageable =
                reachable_threat_indices(scan, ctx, &reachable_threats, origin, &unit.stats);
            let combat_eligible = reachable_threats.is_empty()
                || threats_have_counter(
                    &unit.stats,
                    &reachable_threats,
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
                    continue;
                }
                friendly_combat_units_committed = friendly_combat_units_committed.saturating_add(1);
            }
        }
    }

    // 輸送台帳も同じ原則で数える。輸送枠の採用条件は「その積荷をこの作戦地点へ
    // 届けられるか」なので、台帳も同じく `can_deliver_cargo` で数える。
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

    let enemy_reinforcement = anchor_index
        .map_or_else(EnemyReinforcementEnvelope::default, |index| {
            projected_enemy_reinforcement_envelope(scan, ctx, anchors, horizons, index)
        });

    // 輸送 1 往復にかかるターン数（片道リードタイムの 2 倍）
    let transport_round_trip_turns = deploy_lead_time.saturating_mul(2).max(1);

    let facts = OperationFacts {
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
    };

    let mut slots = derive_slots(&facts);
    // Combat枠は金額の差分ではなく、観測敵が残っていることだけで計画器を起動する。
    // 敵の将来生産余力は再評価用の監視情報であり、必要数と完了判定に混ぜない。
    slots.combat_plan_required = u32::from(!reachable_threats.is_empty());
    // AssaultCapitalも局地Captureと同じ作戦内で複数施設を前進目標として持つ。
    // 構造生産を島campaignが予約済みの手番は`allow_structural_slots`側で一括して
    // Capture/Transportを無効化するため、ここで恒久的に0へ落としてはならない。

    let enemy_capture_completion = enemy_capture_completion_offset(scan, ctx, &anchor, player_id);
    if kind == OperationKind::Capture
        && capture_race_behind(deploy_lead_time, enemy_capture_completion, player_id)
    {
        // 敵が先に占領完了する物件へ、占領枠を複数方向へ広げない。
        // 遅い占領兵を増やすだけの投資を止め、予算を妨害・奪還（Combat）側へ残す。
        // 最小 1 枠は、敵排除後に物件へ入れる奪還要員として残す。
        slots.capture_units = slots.capture_units.min(1);
    }

    let mut operation = Operation {
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
    };
    let horizon = operation.threat_horizon.max(1);
    let expected_assessment = enemy_reinforcement_assessment(
        scan,
        ctx,
        &operation,
        horizon,
        enemy_reinforcement.expected_funds,
        ReinforcementScenario::Expected,
    );
    let stress_assessment = enemy_reinforcement_assessment(
        scan,
        ctx,
        &operation,
        horizon,
        enemy_reinforcement.stress_funds,
        ReinforcementScenario::Stress,
    );
    // Expectedは現在のRollingPlanへ仮想敵として渡す。Go判定とは独立しているため、
    // 最低限の戦力で進撃を開始しつつ、到達時刻付きの次波へ備えた生産を続けられる。
    operation.expected_reinforcements = expected_assessment.reinforcements;
    // 可視敵がいなくても、期限内に前線へ到着するExpectedがあれば継続生産を開始する。
    // これは戦闘開始のGoではなく、到着予定の敵に対する編成開始である。
    operation.slots.combat_plan_required = u32::from(
        !operation.reachable_threats.is_empty() || !operation.expected_reinforcements.is_empty(),
    );
    operation.reinforcement_contingencies = stress_assessment.contingencies;
    // Stressは現在のGoや現金を固定しない。観測時に実兵種・実HPで再計画するための
    // counter候補として残し、Expected生産を予約資金で止めない。
    operation.contingency_reserve_funds = 0;
    operation
}

#[allow(clippy::too_many_arguments)]
pub(super) fn plan_production_with_registry(
    scan: &BoardScan,
    player_id: PlayerId,
    allow_structural_slots: bool,
    committed_combat_assignments: &HashMap<Entity, deployment::ActiveTargetAssignment>,
    advancing_route_entities: &AdvancingCapitalRouteEntities,
    turn: u32,
    plan_registry: &mut V4RollingPlanRegistry,
    contract_registry: &mut plan_contract::PlanContractRegistry,
) -> (Vec<PlannedProduction>, ProductionPlanTrace) {
    let mut ctx = ReachCtx::default();
    let active_objectives = plan_registry.active_objectives(player_id);
    let mut operations = build_operations(scan, &mut ctx, &active_objectives, player_id);

    // V4中央作戦契約との同期（build_operationsの純粋性を保つため、呼び出し元で実行）
    for operation in &mut operations {
        if let Some(control) = operation.property_control
            && let Some(&target_entity) = scan.property_entities.get(&control.property)
        {
            let op_id = contract_registry.maintain_or_replan(target_entity, control.property, None);
            let approach = match control.approach {
                PropertyControlApproach::DirectCapture => {
                    plan_contract::OperationApproach::DirectCapture
                }
                PropertyControlApproach::Interdict => plan_contract::OperationApproach::Interdict,
                PropertyControlApproach::Recapture => plan_contract::OperationApproach::Recapture,
            };
            contract_registry.update_approach_and_target(
                op_id,
                Some(approach),
                Some(control.property),
            );
            operation.operation_id = Some(op_id);
        } else if let Some(objective) = scan
            .campaign_objectives
            .iter()
            .find(|obj| operation.objective_properties.contains(&obj.anchor))
            && let Some(&target_entity) = scan.property_entities.get(&objective.anchor)
        {
            let op_id = contract_registry.maintain_or_replan(target_entity, objective.anchor, None);
            contract_registry.update_approach_and_target(op_id, None, Some(objective.anchor));
            operation.operation_id = Some(op_id);
        }
    }
    let mut plan_trace =
        ProductionPlanTrace::new(player_id, scan.funds, scan.free_facilities.len());

    if operations.is_empty() {
        plan_trace.fallback = true;
        // 作戦に接続されない汎用戦闘生産は行わない。戦力が必要なら、兵站確保・
        // 防衛・首都攻略のいずれかを先にOperationとして成立させる。
        return (Vec::new(), plan_trace);
    }

    // campaignと切り離した汎用clusterが、固定兵站工程のCombat予算を先取りしない。
    // 防衛を最上位に保ちつつ、Capture同士では兵站工程順、その後に敵接触ETAで並べる。
    operations.sort_by_key(|op| {
        let logistics_rank = scan
            .campaign_objectives
            .iter()
            .find(|objective| objective.anchor == op.anchor)
            .and_then(|objective| objective.logistics_rank)
            .unwrap_or(u32::MAX);
        (
            operation_priority_rank(op),
            logistics_rank,
            op.facts.enemy_contact_eta,
        )
    });
    // 要求（op.slots）は配分中に書き換えず、未充足の枠は理由付きで台帳へ記録する。
    let mut ledger = SlotLedger::new();
    if !allow_structural_slots {
        // 占領要員と輸送役は島嶼キャンペーン側で予約済み。余剰予算を同じ役割へ
        // 二重投入せず、観測済みの敵に対する迎撃・護衛・撃破だけへ使う。
        // ただし陸続きの前線はcampaign専用生産の対象外なので、別の渡洋作戦が
        // 同時に予約中でも構造枠を見送ってはならない。
        for (index, operation) in operations.iter().enumerate() {
            if operation.facts.requires_transport {
                ledger.close(index, SlotKind::Capture, UnfilledReason::CoveredByCampaign);
                ledger.close(
                    index,
                    SlotKind::Transport,
                    UnfilledReason::CoveredByCampaign,
                );
            }
        }
    }
    defer_unapproved_capital_structure_for_active_segment(&operations, &mut ledger);

    plan_trace.operations = operations
        .iter()
        .enumerate()
        .map(|(index, op)| ProductionOperationTrace {
            kind: op.kind,
            anchor: op.anchor,
            slots: effective_slots(op, index, &ledger),
            capture_replacement_pending: op.capture_replacement_pending,
            requires_transport: op.facts.requires_transport,
            enemy_combat_units: op.facts.enemy_combat_units,
            enemy_reinforcement_funds: op.facts.enemy_reinforcement_funds,
            enemy_stress_reinforcement_funds: op.stress_reinforcement_funds,
            contingency_reserve_funds: op.contingency_reserve_funds,
            reinforcement_contingencies: op
                .reinforcement_contingencies
                .iter()
                .map(|contingency| ReinforcementContingencyTrace {
                    enemy_type: contingency.enemy_type,
                    enemy_contact_turn: contingency.enemy_contact_turn,
                    counter_type: contingency.counter_type,
                    counter_facility: contingency.counter_facility,
                    counter_build_turn: contingency.counter_build_turn,
                    counter_contact_turn: contingency.counter_contact_turn,
                    attacks_required: contingency.attacks_required,
                    reserve_cost: contingency.reserve_cost,
                })
                .collect(),
            deploy_lead_time: op.facts.deploy_lead_time,
        })
        .collect();

    // 1体は1手番に1作戦しか遂行できない。過去のpriority enemyが移動して複数作戦へ
    // 分散しても、現在位置から最も近い1作戦だけへ既存Combat Entityを帰属させる。
    let mut committed_by_operation: HashMap<
        usize,
        HashMap<Entity, deployment::ActiveTargetAssignment>,
    > = HashMap::default();
    for (&entity, assignment) in committed_combat_assignments {
        let Some(unit) = scan
            .my_units
            .iter()
            .find(|unit| unit.entity == Some(entity))
        else {
            continue;
        };
        let assigned_operation = operations
            .iter()
            .enumerate()
            .filter(|(_, operation)| {
                operation.reachable_threats.iter().any(|threat| {
                    threat
                        .entity
                        .is_some_and(|enemy| assignment.targets.contains(&enemy))
                })
            })
            .min_by_key(|(_, operation)| {
                scan.map.distance(
                    unit.pos.x,
                    unit.pos.y,
                    operation.anchor.x,
                    operation.anchor.y,
                )
            })
            .map(|(index, _)| index);
        if let Some(index) = assigned_operation {
            committed_by_operation
                .entry(index)
                .or_default()
                .insert(entity, assignment.clone());
        }
    }

    // 資金と施設占有は配分状態に集約する。施設を確保した作戦の優先度も保持し、
    // 占領作戦同士の競合を「上位防衛による中断」と誤認しない。
    let mut allocation = AllocationState::new(scan.funds);
    // rolling plannerは1作戦につき1回だけ実行し、その混成パッケージの当手番分を
    // 施設ごとに順次消費する。施設を1つ埋めるたびに同じbeam searchをやり直さない。
    let mut rolling_plans: HashMap<usize, SelectedPlan> = HashMap::default();
    let mut seen_plan_ids = HashSet::default();
    let empty_committed_entities = HashSet::default();
    // この手番に発注済みの永続Plan購入。Registry上は生産完了を観測するまで
    // Plannedのまま残るため、余剰予算の計算では二重に予約しない。
    let mut issued_plan_purchase_cost = 0_u32;
    let mut commands = Vec::new();

    loop {
        let free_slots =
            allocation.free_facility_count(scan.free_facilities.iter().map(|(pos, _)| pos));
        if free_slots == 0 {
            break;
        }
        // 最も不足している枠を持つ作戦から順に見ていく
        let Some((op_index, slot_kind)) = most_starved_slot(&operations, &ledger) else {
            break;
        };

        // 先に並ぶ作戦の観測後counterを発動できる現金は、下位作戦へ流さない。
        // 自作戦の予約は現在の確定脅威を処理するために使ってよい。
        let higher_priority_contingency_reserve = operations[..op_index]
            .iter()
            .map(|operation| operation.contingency_reserve_funds)
            .fold(0_u32, u32::saturating_add);
        let spendable_funds = allocation.spendable_after(higher_priority_contingency_reserve);
        // 1 枠あたり予算。高価なユニットで枠を食い潰さないためのソフト上限。
        let per_slot_budget = spendable_funds / free_slots as u32;

        // トレース用に、選定前の未充足率と作戦の識別情報を控えておく。
        let operation_kind = operations[op_index].kind;
        let operation_anchor = operations[op_index].anchor;
        let deficit_before = allocation_slots(&operations[op_index], op_index, &ledger)
            .deficit_ratio(slot_kind, &operations[op_index].filled);
        let remaining_funds_before = allocation.remaining_funds();

        if slot_kind == SlotKind::Combat && !rolling_plans.contains_key(&op_index) {
            let target_enemies = operations[op_index]
                .reachable_threats
                .iter()
                .filter_map(|threat| threat.entity)
                .collect::<HashSet<_>>();
            let continuation = plan_registry.continuation_for_operation(
                player_id,
                turn,
                operation_kind,
                operations[op_index].island_id,
                &operations[op_index].objective_properties,
                &target_enemies,
            );
            // 既存戦力は、PlanIdが一致する発注済み部隊だけでなく、親戦役DAGの
            // 未完了区間へ前進中の部隊から導く。ただし首都作戦と「次の区間を確保する」
            // Captureが同じ戦力を同時に数えることはない。前段では区間目標に一致する
            // Captureだけが受け取り、首都戦闘へ移るのは実行許可後である。
            // 別のPlan由来の部隊は借りず、即時Combat増援は実在敵への任務を持つ場合だけ
            // 入れるため、防衛・輸送・回復部隊を見かけの首都戦力にしない。
            let continuation_plan_id = continuation.as_ref().map(|plan| plan.plan_id);
            let mut committed_for_plan = committed_entities_for_plan(
                committed_by_operation.get(&op_index),
                continuation_plan_id,
            );
            // Stageは前衛ではないが、Roadmapが同じDAG区間へ予約した実Entityである。
            // 到着予定付きの既存戦力としてRollingPlanへ渡し、人数だけを根拠に
            // 追加生産を禁止する第二の判定器にはしない。
            let mut staged_for_plan = HashSet::default();
            if let Some(island_id) = operations[op_index].island_id {
                match operation_kind {
                    OperationKind::Capture => {
                        for objective in &operations[op_index].objective_properties {
                            if let Some(route_entities) = advancing_route_entities
                                .by_target
                                .get(&(island_id, *objective))
                            {
                                committed_for_plan.extend(route_entities.iter().copied());
                            }
                            if let Some(route_entities) = advancing_route_entities
                                .staged_by_target
                                .get(&(island_id, *objective))
                            {
                                staged_for_plan.extend(route_entities.iter().copied());
                            }
                        }
                    }
                    OperationKind::AssaultCapital if operations[op_index].execution_authorized => {
                        if let Some(route_entities) =
                            advancing_route_entities.by_island.get(&island_id)
                        {
                            committed_for_plan.extend(route_entities.iter().copied());
                        }
                        for ((staged_island, _), route_entities) in
                            &advancing_route_entities.staged_by_target
                        {
                            if *staged_island == island_id {
                                staged_for_plan.extend(route_entities.iter().copied());
                            }
                        }
                    }
                    OperationKind::Defense | OperationKind::AssaultCapital => {}
                }
            }
            let rolling_input = combat_plan_input(
                scan,
                &mut ctx,
                &operations[op_index],
                allocation.used_facilities(),
                if committed_for_plan.is_empty() {
                    &empty_committed_entities
                } else {
                    &committed_for_plan
                },
                &staged_for_plan,
                spendable_funds,
                !allow_structural_slots,
                player_id,
            );
            if let Some(input) = rolling_input {
                // 首都攻略の編成中は、固定購入列が今も実行可能ならその再評価だけを行う。
                // 継続すると分かっている案の全beam searchを先に実行して捨てない。
                // ただし物件制御の前線作戦では毎ターンの動的最新盤面から合同探索を行う。
                let evaluated_continuation = if input.exact_property_control {
                    None
                } else {
                    continuation.map(|previous| {
                        let evaluated = evaluate_fixed_package(&input, &previous.purchases);
                        (previous, evaluated)
                    })
                };
                let reusable_candidate =
                    evaluated_continuation
                        .as_ref()
                        .and_then(|(previous, evaluated)| {
                            previous
                                .can_reuse_without_search(
                                    operation_kind,
                                    operations[op_index].execution_authorized,
                                )
                                .then(|| evaluated.as_ref().ok().cloned())
                                .flatten()
                        });
                let Some(candidate_plan) =
                    reusable_candidate.or_else(|| plan_force_package(&input))
                else {
                    ledger.close(op_index, SlotKind::Combat, UnfilledReason::NoForcePackage);
                    continue;
                };
                let conflicted_facilities = evaluated_continuation
                    .as_ref()
                    .map(|(previous, evaluated)| {
                        if evaluated.is_ok() {
                            return HashSet::default();
                        }
                        let mut due = previous
                            .purchases
                            .iter()
                            .filter(|purchase| purchase.build_turn == 0)
                            .copied()
                            .collect::<Vec<_>>();
                        due.sort_unstable_by_key(|purchase| {
                            (purchase.facility.y, purchase.facility.x, purchase.cost)
                        });
                        let mut affordable_funds = input.current_funds;
                        let mut conflicts = HashSet::default();
                        for purchase in due {
                            let claimed_by_higher_priority =
                                allocation.facility_owner(&purchase.facility).is_some_and(
                                    |owner| owner < operation_priority_rank(&operations[op_index]),
                                );
                            let claimed_by_campaign = scan
                                .production_facilities
                                .iter()
                                .any(|(facility, _)| *facility == purchase.facility)
                                && !scan
                                    .free_facilities
                                    .iter()
                                    .any(|(facility, _)| *facility == purchase.facility);
                            if claimed_by_higher_priority || claimed_by_campaign {
                                conflicts.insert(purchase.facility);
                            } else if operation_kind == OperationKind::AssaultCapital {
                                if purchase.cost <= affordable_funds {
                                    affordable_funds =
                                        affordable_funds.saturating_sub(purchase.cost);
                                } else {
                                    // 上位作戦が当手番の現金を使った場合も、失敗ではなく
                                    // 当該施設の首都編成列を次手番以降へ繰り下げる。
                                    conflicts.insert(purchase.facility);
                                }
                            }
                        }
                        conflicts
                    })
                    .unwrap_or_default();
                let selected = plan_registry.select_for_operation(
                    player_id,
                    turn,
                    operation_kind,
                    operations[op_index].island_id,
                    operation_anchor,
                    operations[op_index].objective_properties.clone(),
                    target_enemies,
                    operations[op_index].execution_authorized,
                    evaluated_continuation,
                    candidate_plan,
                    input.hard_deadline,
                    conflicted_facilities,
                );
                if let Some(plan_id) = selected.plan_id {
                    seen_plan_ids.insert(plan_id);
                }
                if input.exact_property_control {
                    // 物件数から歩兵数を先決めしない。施設・資金・敵相性を同時に列挙して
                    // 選ばれた支配パッケージのCapture割当だけを、この手番の必要数とする。
                    // 元の要求を保持し、従来の上書き量を実効要求として台帳へ記録する。
                    // 以前の実効要求に対する未充足の取消タイミングは変えない。
                    ledger.set_effective(
                        op_index,
                        SlotKind::Capture,
                        u32::try_from(selected.plan.capture_purchases.len()).unwrap_or(u32::MAX),
                        EffectiveDemandReason::JointPackageSelected,
                    );
                    ledger.reopen(op_index, SlotKind::Capture);
                }
                let plan = &selected.plan;
                plan_trace.rolling_combat_plans.retain(|current| {
                    current.operation_kind != operation_kind || current.anchor != operation_anchor
                });
                plan_trace
                    .rolling_combat_plans
                    .push(RollingCombatPlanTrace {
                        plan_id: selected.plan_id,
                        revision: selected.revision,
                        disposition: selected.disposition,
                        replan_reason: selected.reason,
                        operation_kind,
                        anchor: operation_anchor,
                        feasible: plan.feasible,
                        purchases: plan
                            .purchases
                            .iter()
                            .copied()
                            .map(|purchase| RollingPurchaseTrace {
                                unit_type: purchase.unit_type,
                                facility: purchase.facility,
                                build_turn: purchase.build_turn,
                                cost: purchase.cost,
                            })
                            .collect(),
                        targets: plan
                            .target_forecasts
                            .iter()
                            .map(|target| RollingTargetTrace {
                                entity: target.entity,
                                unit_type: target.unit_type,
                                available_turn: target.available_turn,
                                initial_hp: target.initial_hp,
                                remaining_hp: target.remaining_hp,
                                destroyed_turn: target.destroyed_turn,
                            })
                            .collect(),
                        turn_forecasts: plan
                            .turn_forecasts
                            .iter()
                            .map(|forecast| CampaignTurnForecastTrace {
                                turn: forecast.turn,
                                enemy_arrival_hp: forecast.enemy_arrival_hp,
                                enemy_hp_removed: forecast.enemy_hp_removed,
                                friendly_hp_lost: forecast.friendly_hp_lost,
                                attack_count: forecast.attack_count,
                            })
                            .collect(),
                        first_attack_turn: plan.first_attack_turn,
                        elimination_turn: plan.elimination_turn,
                        occupation_turn: plan.occupation_turn,
                        production_cost: plan.production_cost,
                        expected_loss: plan.expected_loss,
                        surviving_combat_value: plan.surviving_combat_value,
                        required_overmatch_value: plan.required_overmatch_value,
                        overmatch_ready: plan.overmatch_ready,
                        protected_unit_count: plan.protected_unit_count,
                        protected_survivor_count: plan.protected_survivor_count,
                        required_capture_survivor_count: plan.required_capture_survivor_count,
                        candidates_considered: plan.candidates_considered,
                        candidates_pruned: plan.candidates_pruned,
                        search_truncated: plan.search_truncated,
                    });
                if selected.plan_id.is_none() {
                    // 長期Expectedを今の資金で全滅できない場合でも、best-effortは
                    // 当手番に到達可能なscreenを返している。これを捨てると「敵増援を
                    // 多く見積もるほど1体も生産しない」逆転が起きる。
                    // PlanIdなしの購入は永続計画に固定せず、次手番の実敵・資金・
                    // 生産結果で必ず再評価する即時Combatとして発行する。
                    rolling_plans.insert(op_index, selected);
                    continue;
                }
                rolling_plans.insert(op_index, selected);
            }
        }
        let rolling_plan = rolling_plans.get(&op_index);
        let mut planned_purchase = None;
        let candidate = if slot_kind == SlotKind::Combat {
            rolling_plan.and_then(|plan| {
                plan.plan
                    .current_purchases()
                    .find(|purchase| {
                        !allocation.is_facility_used(&purchase.facility)
                            && purchase.cost <= spendable_funds
                    })
                    .map(|purchase| {
                        planned_purchase = Some(purchase);
                        SlotCandidate {
                            unit_type: purchase.unit_type,
                            cost: purchase.cost,
                            facility: purchase.facility,
                            fitness: 1.0,
                        }
                    })
            })
        } else {
            select_candidate(
                scan,
                &mut ctx,
                &operations[op_index],
                slot_kind,
                allocation.used_facilities(),
                CandidateConstraints {
                    remaining_funds: spendable_funds,
                    per_slot_budget,
                },
                player_id,
            )
        };

        let Some(candidate) = candidate else {
            // この枠を満たせる候補が無い場合は、要求を残したまま当手番は見送り、次を探す
            ledger.close(op_index, slot_kind, UnfilledReason::NoCandidate);
            let reserved = (slot_kind == SlotKind::Combat)
                .then_some(rolling_plan)
                .flatten()
                .filter(|plan| plan.plan_id.is_some())
                .and_then(|plan| {
                    plan.plan
                        .purchases
                        .iter()
                        .filter(|purchase| purchase.build_turn > 0)
                        .min_by_key(|purchase| purchase.build_turn)
                });
            plan_trace.steps.push(ProductionStepTrace {
                operation_kind,
                operation_anchor,
                slot_kind,
                deficit_before,
                deficit_after: deficit_before,
                remaining_funds_before,
                decision: reserved.map_or(
                    ProductionDecision::Unfilled {
                        reason: UnfilledReason::NoCandidate,
                    },
                    |purchase| ProductionDecision::Reserved {
                        unit_type: purchase.unit_type,
                        cost: purchase.cost,
                        build_turn: purchase.build_turn,
                    },
                ),
            });
            continue;
        };

        // 見送り購入: 一括編成が必要な作戦で、今買える範囲に適合候補が無く、
        // 数ターン待てばより適合する候補が買えるなら、資金を貯める。
        if slot_kind != SlotKind::Combat
            && operations[op_index].property_controls.is_empty()
            && should_defer_purchase(
                scan,
                &mut ctx,
                &operations[op_index],
                slot_kind,
                spendable_funds,
                candidate.cost,
                player_id,
            )
        {
            plan_trace.steps.push(ProductionStepTrace {
                operation_kind,
                operation_anchor,
                slot_kind,
                deficit_before,
                deficit_after: deficit_before,
                remaining_funds_before,
                decision: ProductionDecision::Deferred {
                    unit_type: candidate.unit_type,
                    cost: candidate.cost,
                },
            });
            break;
        }

        let joint_capture_target = (slot_kind == SlotKind::Combat)
            .then_some(rolling_plan)
            .flatten()
            .zip(planned_purchase)
            .and_then(|(plan, purchase)| plan.plan.capture_target_for(purchase));
        // 共同計画のCapture割当を優先し、通常Capture枠だけ従来の次レーンを使う。
        let capture_mission_target = joint_capture_target.or_else(|| {
            (slot_kind == SlotKind::Capture).then(|| next_capture_target(&operations[op_index]))
        });
        let effective_slot_kind = if capture_mission_target.is_some() {
            SlotKind::Capture
        } else {
            slot_kind
        };
        let planned_combat_target = (effective_slot_kind == SlotKind::Combat)
            .then_some(rolling_plan)
            .flatten()
            .zip(planned_purchase)
            .and_then(|(plan, purchase)| plan.plan.combat_target_for(purchase));
        if allocation
            .try_claim(
                candidate.facility,
                candidate.cost,
                operation_priority_rank(&operations[op_index]),
            )
            .is_err()
        {
            // 候補評価後に競合した予約は、後続の意図や工程へ反映せず配分を止める。
            break;
        }
        if planned_purchase.is_some() {
            issued_plan_purchase_cost = issued_plan_purchase_cost.saturating_add(candidate.cost);
        }
        let mut deployment = if effective_slot_kind == SlotKind::Capture {
            let target = capture_mission_target.unwrap_or(operations[op_index].anchor);
            Some(planned_capture_deployment(&operations[op_index], target))
        } else {
            planned_deployment(
                scan,
                &mut ctx,
                &operations[op_index],
                effective_slot_kind,
                &candidate,
                planned_combat_target,
            )
        };
        if effective_slot_kind == SlotKind::Combat
            && let (Some(deployment), Some(plan)) = (deployment.as_mut(), rolling_plan)
        {
            deployment.forecast = deployment::DeploymentForecast {
                first_attack_turn: plan.plan.first_attack_turn,
                elimination_turn: plan.plan.elimination_turn,
                occupation_turn: plan.plan.occupation_turn,
                package_cost: plan.plan.production_cost,
                package_size: u32::try_from(plan.plan.purchases.len()).unwrap_or(u32::MAX),
            };
            deployment.plan_step = plan
                .plan_id
                .zip(plan.revision)
                .zip(planned_purchase)
                .and_then(|((plan_id, revision), purchase)| {
                    plan_registry.current_step_ref(plan_id, revision, turn, purchase)
                });
            if let (Some(plan_id), Some(op_id)) = (plan.plan_id, operations[op_index].operation_id)
            {
                contract_registry.link_plan_to_operation(plan_id, op_id);
            }
        }
        if let Some(op_id) = operations[op_index].operation_id
            && let Some(deployment) = deployment.as_mut()
        {
            let role = match effective_slot_kind {
                SlotKind::Capture => victory_roadmap::OperationEntityRole::Capture,
                SlotKind::Transport => victory_roadmap::OperationEntityRole::Transport,
                SlotKind::Combat | SlotKind::Intercept => {
                    victory_roadmap::OperationEntityRole::Combat
                }
            };
            let revision = contract_registry
                .contract_revision(op_id)
                .unwrap_or(plan_revision::PlanRevision(1));
            deployment.operation_binding = Some(plan_contract::OperationBinding {
                operation_id: op_id,
                plan_step: deployment.plan_step,
                role,
                revision,
            });
        }
        // Combatは同じパッケージの未使用current purchaseを次の反復で選ぶ。
        // 全て消費した後は候補なしとなり、この手番のCombat枠を完了する。
        if effective_slot_kind == SlotKind::Capture {
            let target = capture_mission_target.expect("Capture購入には任務先がある");
            operations[op_index]
                .planned_capture_units
                .push(PlannedCaptureUnit {
                    stats: candidate_stats(scan, &candidate).clone(),
                    facility: candidate.facility,
                    target,
                });
            operations[op_index].filled.capture_units =
                operations[op_index].filled.capture_units.saturating_add(1);
        } else if effective_slot_kind != SlotKind::Combat {
            record_fill(
                scan,
                &mut ctx,
                &mut operations[op_index],
                effective_slot_kind,
                &candidate,
            );
        }
        plan_trace.steps.push(ProductionStepTrace {
            operation_kind,
            operation_anchor,
            slot_kind: effective_slot_kind,
            deficit_before,
            // 購入を反映した後の未充足率。ここが下がらない枠が同一ユニットを買い続ける。
            deficit_after: allocation_slots(&operations[op_index], op_index, &ledger)
                .deficit_ratio(effective_slot_kind, &operations[op_index].filled),
            remaining_funds_before,
            decision: ProductionDecision::Produced {
                unit_type: candidate.unit_type,
                cost: candidate.cost,
                facility: candidate.facility,
            },
        });
        let capture_intent = (effective_slot_kind == SlotKind::Capture)
            .then(|| {
                operations[op_index].island_id.map(|island_id| {
                    campaign_execution::CampaignProductionIntent {
                        command: ProduceUnitCommand {
                            player_id,
                            target_x: candidate.facility.x,
                            target_y: candidate.facility.y,
                            unit_type: candidate.unit_type,
                        },
                        island_id,
                        role: campaign_execution::CampaignProductionRole::Capture,
                        mission_target: capture_mission_target,
                    }
                })
            })
            .flatten();
        let purpose = plan_step_purpose(deployment.as_ref(), PurchasePurpose::OperationSlot);
        commands.push(PlannedProduction {
            command: ProduceUnitCommand {
                player_id,
                target_x: candidate.facility.x,
                target_y: candidate.facility.y,
                unit_type: candidate.unit_type,
            },
            role: effective_slot_kind,
            cost: candidate.cost,
            purpose,
            deployment,
            capture_intent,
        });
    }

    // 永続Planの予約は「最低限ここまでは作る」というMustであり、手番の生産停止条件
    // ではない。未発行のMustと観測後counterだけを残し、残額・空き施設があれば
    // 実在する前線の敵へ交戦可能な増援を追加する。仮想増援や未接敵の敵はここに
    // 入れないので、敵見積の不確実さで汎用兵を量産することもない。
    let contingency_reserve = operations
        .iter()
        .map(|operation| operation.contingency_reserve_funds)
        .fold(0_u32, u32::saturating_add);
    let unissued_plan_reserve = plan_registry
        .reserved_purchase_cost(player_id)
        .saturating_sub(issued_plan_purchase_cost);
    let mut immediate_reinforcement_funds =
        allocation.spendable_after(unissued_plan_reserve.saturating_add(contingency_reserve));
    // 到達性と対敵有効度は、残額を工場へ配るたびに変わらない。候補表をここで一度だけ
    // 作り、以下のループでは資金・使用済み施設だけを更新する。
    // 待機線の有無だけで余剰Combatを一律停止しない。Stage戦力は既に
    // RollingPlanの到着予定付き既存戦力へ入っているため、不足が残るときだけ
    // ここへ到達する。実在敵への有効打・到着性・HP上限による既存の枝刈りで、
    // 同一出口への無目的な量産は防ぐ。
    // Rolling Planがこの手番の具体的な攻撃列で排除済みとした実Entityは、残額を
    // 使い切るためだけの追加Combatの標的にしない。次手番は実盤面を再観測するため、
    // ここで仮想的な追撃を買う必要はない。
    let resolved_by_rolling_plan = rolling_plans
        .values()
        .flat_map(|selected| selected.plan.target_forecasts.iter())
        .filter(|target| target.remaining_hp == 0)
        .filter_map(|target| target.entity)
        .collect::<HashSet<_>>();
    let immediate_combat_options = immediate_combat_options(
        scan,
        &mut ctx,
        &operations,
        player_id,
        &resolved_by_rolling_plan,
    );
    // 1手番に1体が与えられる攻撃は1回だけなので、同じ敵HPを複数の工場枠で
    // 仮想的に何度も消費しない。ここは経路探索済みの攻撃表への割当だけであり、
    // 「1体追加ごとの将来戦闘シミュレーション」にはしない。
    let mut immediately_committed_damage = HashMap::<(usize, Entity), u32>::default();
    while immediate_reinforcement_funds > 0 {
        let free_slots =
            allocation.free_facility_count(scan.free_facilities.iter().map(|(pos, _)| pos));
        if free_slots == 0 {
            break;
        }
        let per_slot_budget = immediate_reinforcement_funds / free_slots as u32;
        let Some((op_index, candidate, engagement)) = select_immediate_combat_reinforcement(
            &operations,
            &immediate_combat_options,
            allocation.used_facilities(),
            &immediately_committed_damage,
            CandidateConstraints {
                remaining_funds: immediate_reinforcement_funds,
                per_slot_budget,
            },
        ) else {
            break;
        };

        let operation_kind = operations[op_index].kind;
        let operation_anchor = operations[op_index].anchor;
        let remaining_funds_before = allocation.remaining_funds();
        let deployment = planned_deployment(
            scan,
            &mut ctx,
            &operations[op_index],
            SlotKind::Combat,
            &candidate,
            Some(engagement.entity),
        );
        // 選定時に有効な実Entityを少なくとも1体確認している。ここで任務が作れない
        // なら命令を出さず停止し、任務なしCombat Entityを生まない。
        let Some(deployment) =
            deployment.filter(|deployment| !deployment.priority_enemies.is_empty())
        else {
            break;
        };

        if allocation
            .try_claim(
                candidate.facility,
                candidate.cost,
                operation_priority_rank(&operations[op_index]),
            )
            .is_err()
        {
            // 候補評価後に競合した予約は、後続の意図や工程へ反映せず配分を止める。
            break;
        }
        immediate_reinforcement_funds =
            immediate_reinforcement_funds.saturating_sub(candidate.cost);
        let committed_damage = immediately_committed_damage
            .entry((op_index, engagement.entity))
            .or_default();
        *committed_damage = committed_damage.saturating_add(engagement.damage);
        plan_trace.steps.push(ProductionStepTrace {
            operation_kind,
            operation_anchor,
            slot_kind: SlotKind::Combat,
            // これはMust枠を埋める購入ではない。残額で前線圧を足すため、枠不足率は
            // 0のまま記録し、rolling planとの混同を避ける。
            deficit_before: 0.0,
            deficit_after: 0.0,
            remaining_funds_before,
            decision: ProductionDecision::ProducedImmediateReinforcement {
                unit_type: candidate.unit_type,
                cost: candidate.cost,
                facility: candidate.facility,
            },
        });
        commands.push(PlannedProduction {
            command: ProduceUnitCommand {
                player_id,
                target_x: candidate.facility.x,
                target_y: candidate.facility.y,
                unit_type: candidate.unit_type,
            },
            role: SlotKind::Combat,
            cost: candidate.cost,
            purpose: PurchasePurpose::ImmediateReinforcement,
            deployment: Some(deployment),
            capture_intent: None,
        });
    }

    // 空き施設があり、Must予約・予備費を除いた余剰資金が残っている場合は、
    // 工場枠の遊休化を防ぎ前線圧・戦線維持を継続するため、前線作戦へ向けて動員生産を行う。
    while immediate_reinforcement_funds >= 1000 {
        let free_slots =
            allocation.free_facility_count(scan.free_facilities.iter().map(|(pos, _)| pos));
        if free_slots == 0 {
            break;
        }
        let Some((op_index, op)) = operations
            .iter()
            .enumerate()
            .filter(|(_, op)| {
                op.kind == OperationKind::Capture || op.kind == OperationKind::AssaultCapital
            })
            .min_by_key(|(_, op)| operation_priority_rank(op))
        else {
            break;
        };
        // Rolling Planで排除済みなら、未所有物件へ向かえる占領役だけを追加する。
        // 交戦相手がいないのに航空・戦車まで残額で埋めると、次手番に必要となる
        // 相性counterの資金と生産口を失う。輸送未成立の前線には単独の地上兵も出さない。
        if op.facts.requires_transport {
            break;
        }
        let has_unresolved_live_threat = op.reachable_threats.iter().any(|threat| {
            threat.available_turn == 0
                && threat
                    .entity
                    .is_some_and(|entity| !resolved_by_rolling_plan.contains(&entity))
        });

        let is_small_map = scan.opening_policy == Some(OpeningProductionPolicy::SmallMapExpansion);
        let per_slot_budget = immediate_reinforcement_funds / free_slots as u32;
        let mut best_mobilization: Option<SlotCandidate> = None;
        for (facility, terrain) in &scan.free_facilities {
            if allocation.is_facility_used(facility) {
                continue;
            }
            for (unit_type, stats) in &scan.available_types {
                let allow_combat_unit = has_unresolved_live_threat || is_small_map;
                if stats.cost == 0
                    || stats.cost > immediate_reinforcement_funds
                    || (!allow_combat_unit && !stats.can_capture)
                    || !scan.can_produce(*terrain, *unit_type)
                    || !can_join_operation(
                        scan,
                        &mut ctx,
                        &op.anchor,
                        op.facts.requires_transport,
                        facility,
                        stats,
                    )
                {
                    continue;
                }
                let candidate = SlotCandidate {
                    unit_type: *unit_type,
                    cost: stats.cost,
                    facility: *facility,
                    fitness: 1.0,
                };
                let fits_budget = if has_unresolved_live_threat || is_small_map {
                    stats.cost <= immediate_reinforcement_funds
                } else {
                    stats.cost <= per_slot_budget.max(1000)
                };
                let damage_potential = if has_unresolved_live_threat {
                    op.reachable_threats
                        .iter()
                        .map(|threat| {
                            best_damage(&scan.damage_chart, stats.unit_type, threat.stats.unit_type)
                        })
                        .max()
                        .unwrap_or(0)
                } else {
                    0
                };
                let has_effective_damage = !has_unresolved_live_threat || damage_potential > 0;
                let movement_rank = if is_small_map {
                    std::cmp::Reverse(stats.max_movement)
                } else {
                    std::cmp::Reverse(0)
                };
                let key = (
                    !has_effective_damage,
                    !fits_budget,
                    std::cmp::Reverse(damage_potential),
                    movement_rank,
                    stats.cost,
                    facility.y,
                    facility.x,
                );
                if best_mobilization.as_ref().is_none_or(|current| {
                    let current_fits = if has_unresolved_live_threat || is_small_map {
                        current.cost <= immediate_reinforcement_funds
                    } else {
                        current.cost <= per_slot_budget.max(1000)
                    };
                    let current_damage = if has_unresolved_live_threat {
                        op.reachable_threats
                            .iter()
                            .map(|threat| {
                                best_damage(
                                    &scan.damage_chart,
                                    current.unit_type,
                                    threat.stats.unit_type,
                                )
                            })
                            .max()
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    let current_has_damage = !has_unresolved_live_threat || current_damage > 0;
                    let current_stats = scan
                        .available_types
                        .iter()
                        .find(|(t, _)| *t == current.unit_type)
                        .map(|(_, s)| s);
                    let current_movement_rank = if is_small_map {
                        std::cmp::Reverse(current_stats.map_or(0, |s| s.max_movement))
                    } else {
                        std::cmp::Reverse(0)
                    };
                    let current_key = (
                        !current_has_damage,
                        !current_fits,
                        std::cmp::Reverse(current_damage),
                        current_movement_rank,
                        current.cost,
                        current.facility.y,
                        current.facility.x,
                    );
                    key < current_key
                }) {
                    best_mobilization = Some(candidate);
                }
            }
        }

        let Some(candidate) = best_mobilization else {
            break;
        };
        // 動員購入の役割は、価格ではなく選定理由から購入案に一度だけ決める。
        // 未解決の実在脅威へ有効打を持つことを理由に選んだ兵はCombat、
        // それ以外で占領能力を持つ兵は前線物件へ向かうCapture、残りはCombat。
        let role = mobilization_role(
            candidate_stats(scan, &candidate),
            has_unresolved_live_threat,
            op,
            &scan.damage_chart,
        );

        let deployment = planned_deployment(
            scan,
            &mut ctx,
            &operations[op_index],
            role,
            &candidate,
            None,
        );

        if allocation
            .try_claim(
                candidate.facility,
                candidate.cost,
                operation_priority_rank(&operations[op_index]),
            )
            .is_err()
        {
            // 候補評価後に競合した予約は、後続の意図や工程へ反映せず配分を止める。
            break;
        }
        immediate_reinforcement_funds =
            immediate_reinforcement_funds.saturating_sub(candidate.cost);

        plan_trace.steps.push(ProductionStepTrace {
            operation_kind: operations[op_index].kind,
            operation_anchor: operations[op_index].anchor,
            slot_kind: role,
            deficit_before: 0.0,
            deficit_after: 0.0,
            remaining_funds_before: allocation.remaining_funds().saturating_add(candidate.cost),
            decision: ProductionDecision::ProducedImmediateReinforcement {
                unit_type: candidate.unit_type,
                cost: candidate.cost,
                facility: candidate.facility,
            },
        });

        commands.push(PlannedProduction {
            command: ProduceUnitCommand {
                player_id,
                target_x: candidate.facility.x,
                target_y: candidate.facility.y,
                unit_type: candidate.unit_type,
            },
            role,
            cost: candidate.cost,
            purpose: PurchasePurpose::Mobilization,
            deployment,
            capture_intent: None,
        });
    }

    production_finalize::finalize(scan, &mut commands, &mut plan_trace, plan_registry, true);

    plan_registry.reconcile_unseen_plans(player_id, turn, &seen_plan_ids);
    let remaining_funds = allocation.remaining_funds();
    plan_trace.leftover_funds = remaining_funds;
    plan_trace.unfilled_slots = unfilled_slot_traces(&operations, &ledger);
    plan_trace.demands = production_demand_traces(&operations, &ledger);
    plan_trace.reserved_funds =
        remaining_funds.min(unissued_plan_reserve.saturating_add(contingency_reserve));
    plan_trace.uncommitted_funds = remaining_funds.saturating_sub(plan_trace.reserved_funds);
    (commands, plan_trace)
}

/// 動員購入の役割を、兵種の能力と選定理由から決める。
///
/// 未解決の実在脅威があるときの動員は与ダメージ順に選ぶため、その脅威へ有効打を
/// 持つ兵は交戦役（Combat）として配備意図を付ける。脅威が無い、または有効打が無い
/// 占領能力兵は前線物件の占領役（Capture）とする。
pub(super) fn mobilization_role(
    stats: &UnitStats,
    has_unresolved_live_threat: bool,
    op: &Operation,
    damage_chart: &DamageChart,
) -> SlotKind {
    let selected_for_engagement = has_unresolved_live_threat
        && op
            .reachable_threats
            .iter()
            .any(|threat| best_damage(damage_chart, stats.unit_type, threat.stats.unit_type) > 0);
    if stats.can_capture && !selected_for_engagement {
        SlotKind::Capture
    } else {
        SlotKind::Combat
    }
}

/// 見積に含めてよいのは、同じ永続Planへ実際に配属済みのEntityだけ。
pub(super) fn committed_entities_for_plan(
    assignments: Option<&HashMap<Entity, deployment::ActiveTargetAssignment>>,
    continuation_plan_id: Option<plan_revision::PlanId>,
) -> HashSet<Entity> {
    assignments
        .into_iter()
        .flat_map(|assignments| assignments.iter())
        .filter_map(|(&entity, assignment)| {
            let belongs_to_continuation =
                assignment.plan_id == continuation_plan_id && assignment.plan_id.is_some();
            let is_operation_bound_immediate_combat =
                assignment.plan_id.is_none() && assignment.slot_kind == SlotKind::Combat;
            (belongs_to_continuation || is_operation_bound_immediate_combat).then_some(entity)
        })
        .collect()
}

/// 敵増援ごとに接触turnと、観測後に生産する最速counterの接触turnを比較する。
/// counterが間に合う仮説は現在の撃破対象へ混ぜず、条件付き予約として保持する。
pub(super) fn enemy_reinforcement_assessment(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    horizon: u32,
    scenario_budget: u32,
    scenario: ReinforcementScenario,
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
    if let Some(unit_type) = scan.enemy_production_forecast.dominant_unit_type {
        expected_enemy_types.insert(unit_type);
    }

    let last_build_turn = match scenario {
        ReinforcementScenario::Expected => {
            EXPECTED_REINFORCEMENT_WAVES.min(horizon.saturating_sub(1))
        }
        ReinforcementScenario::Stress => horizon.saturating_sub(1),
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
                        && (scenario == ReinforcementScenario::Stress
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
                    let available_turn = property_control::produced_action_offset(build_turn, eta);
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
                    (scenario == ReinforcementScenario::Expected || can_be_engaged).then_some((
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
            if scenario == ReinforcementScenario::Expected
                || stats.can_capture
                || observed_enemy_types.contains(&stats.unit_type)
            {
                assessment.reinforcements.push(reinforcement);
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
                assessment.reinforcements.push(reinforcement);
            }
        }
    }
    assessment
}

/// 観測敵と到着しうる増援を排除できる混成生産列を、探索期間の全生産slotから計画する。
///
/// `combat_plan_required`は呼び出し条件にだけ残し、候補数・生産停止・完了判定には使わない。
/// 既存unitと同じ手番に発注済みのunitを初期編成へ入れ、敵EntityのHPが0になるまで
/// ターン単位で攻撃を進めた結果から必要な購入だけを返す。
#[allow(clippy::too_many_arguments)]
pub(super) fn combat_plan_input(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    used_facilities: &HashSet<GridPosition>,
    committed_combat_entities: &HashSet<Entity>,
    staged_combat_entities: &HashSet<Entity>,
    remaining_funds: u32,
    require_self_deployment: bool,
    player_id: PlayerId,
) -> Option<RollingPlanInput> {
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
    let mut hard_deadline =
        if op.kind == OperationKind::Defense && op.facts.enemy_contact_eta != u32::MAX {
            Some(op.facts.enemy_contact_eta.max(1))
        } else if op.kind == OperationKind::Capture {
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
        };
    let campaign_objective = scan.campaign_objectives.iter().find(|objective| {
        objective.anchor == op.anchor || op.objective_properties.contains(&objective.anchor)
    });
    let disaggregated_control = op.property_control.filter(|control| control.disaggregated);
    let mut capture_completion_turn = if op.property_controls.is_empty() {
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
    let required_capture_survivors = campaign_objective
        .map_or(local_capture_requirement, |objective| {
            local_capture_requirement.max(objective.required_capture_survivors)
        });
    if campaign_scope_audit_enabled() {
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
    let mut enemies = enemies;
    if enemies.is_empty() && !op.property_controls.is_empty() {
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
    if op.kind != OperationKind::Capture {
        enemies.extend(op.expected_reinforcements.iter().cloned());
    }
    if enemies.is_empty() {
        return None;
    }

    let mut existing_units = Vec::new();
    for unit in &scan.my_units {
        let Some(entity) = unit.entity else {
            continue;
        };
        let committed = committed_combat_entities.contains(&entity);
        let staged = staged_combat_entities.contains(&entity);
        let protected = op.protected_capture_entities.contains(&entity);
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
    if !op.property_controls.is_empty() {
        // 同じ施設・資金について、戦闘役だけでなく各物件の占領役として使う合法手も
        // 同じ探索空間へ入れる。先に最安歩兵を予約せず、相性を含む全編成で比較する。
        for target in &op.capture_lane_targets {
            let durability = property_durability(scan, target);
            for (facility, terrain) in &scan.free_facilities {
                if used_facilities.contains(facility) {
                    continue;
                }
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
    // この手番に既に使った施設は将来手番には再利用できるが、build_turn=0では使えない。
    options.retain(|option| {
        option.purchase.build_turn > 0 || !used_facilities.contains(&option.purchase.facility)
    });
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
    if !op.property_controls.is_empty() {
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
    let next_occupancy = next_activation_occupants(scan, op, player_id);
    let projected_target_cells = enemies
        .iter()
        .map(|enemy| {
            scan.enemy_units
                .iter()
                .find(|unit| unit.entity == enemy.entity)
                .map_or_else(
                    || vec![enemy.position],
                    |observed| projected_enemy_front_cells(scan, op, observed, player_id),
                )
        })
        .collect::<Vec<_>>();
    let projected_occupancies = enemies
        .iter()
        .map(|enemy| {
            let mut occupancy = next_occupancy.clone();
            if scan
                .enemy_units
                .iter()
                .any(|unit| unit.entity == enemy.entity)
            {
                // 対象自身の現在セルは、一手先包絡へ進んだ後の経路障害にしない。
                occupancy.remove(&(enemy.position.x, enemy.position.y));
            }
            occupancy
        })
        .collect::<Vec<_>>();
    // occupancyは敵ごとに異なるため、占有をkeyに持たない既存cacheを共有しない。
    let mut projected_action_caches = (0..enemies.len())
        .map(|_| ActionTurnDistanceCache::default())
        .collect::<Vec<_>>();
    let production_attack_projections = options
        .iter()
        .map(|option| {
            enemies
                .iter()
                .enumerate()
                .map(|(enemy_index, enemy)| {
                    if best_damage(
                        &scan.damage_chart,
                        option.stats.unit_type,
                        enemy.stats.unit_type,
                    ) == 0
                    {
                        return None;
                    }
                    let has_only_one_move_of_fuel = option.stats.min_range > 1
                        && option.stats.max_fuel > 0
                        && option.stats.max_fuel <= 1;
                    let has_spare_production_capacity = scan.free_facilities.len() > 1;
                    let capital_side_defense = op.kind == OperationKind::Defense
                        && scan.capital_position.is_some_and(|capital| {
                            scan.map
                                .distance(capital.x, capital.y, op.anchor.x, op.anchor.y)
                                <= option.stats.max_range
                        });
                    if has_only_one_move_of_fuel
                        && !capital_side_defense
                        && !has_spare_production_capacity
                    {
                        // 燃料が一マス分しかない砲台は、前線へ展開しても再配置できない。
                        // 首都側の防衛でない限り、唯一の生産口を長期占有する案は候補に
                        // しない。兵種名や攻撃力ではなく、マスターデータ上の燃料と実際の
                        // 空き生産slotだけから判定する。
                        return None;
                    }
                    if option.stats.min_range <= 1
                        && let Some(capture_target) = option.capture_target
                    {
                        if option.capture_blocking_enemy_index != Some(enemy_index) {
                            return None;
                        }
                        let control = op.property_controls.iter().find(|control| {
                            control.property == capture_target
                                && enemy.entity == Some(control.enemy_entity)
                        })?;
                        let observed_enemy = scan
                            .enemy_units
                            .iter()
                            .find(|unit| unit.entity == enemy.entity)?;
                        // 占領担当を戦闘用cloneへ分けず、同じ物件へ進む敵との経路上で
                        // 合法に交戦できる場合だけ初撃edgeを与える。
                        return produced_attack_turn_along_capture_route(
                            scan,
                            ctx,
                            &option.purchase.facility,
                            &option.stats,
                            observed_enemy,
                            &capture_target,
                            control.enemy_arrival_turn,
                            player_id,
                        );
                    }
                    if option.stats.min_range <= 1
                        && let Some(control) = op
                            .property_controls
                            .iter()
                            .find(|control| enemy.entity == Some(control.enemy_entity))
                        && let Some(observed_enemy) = scan
                            .enemy_units
                            .iter()
                            .find(|unit| unit.entity == Some(control.enemy_entity))
                    {
                        // 中立へのInterdictだけでなく、敵所有前線のRecaptureでも敵を
                        // 初期位置へ固定しない。敵の実最短経路上で接触できる快速screenを
                        // 評価し、経路上に合法な交戦点が無い場合だけ現在位置を追う。
                        if let Some(projection) = produced_attack_turn_along_capture_route(
                            scan,
                            ctx,
                            &option.purchase.facility,
                            &option.stats,
                            observed_enemy,
                            &control.property,
                            control.enemy_arrival_turn,
                            player_id,
                        ) {
                            return Some(projection);
                        }
                    }
                    let effective_fuel = if option.stats.max_fuel == 0 {
                        u32::MAX
                    } else {
                        option.stats.max_fuel
                    };
                    projected_target_cells[enemy_index]
                        .iter()
                        .filter_map(|target| {
                            let distance = if option.stats.min_range > 1 {
                                // 間接unitは生産口の上から即射撃できても、その一手で次手番の
                                // 生産枠を失う。退避してから射撃可能になるETAだけを戦力として
                                // 数え、砲台を含めた高火力の見かけ上の即応性を使わない。
                                calculate_action_distance_to_range_after_leaving_start(
                                    &scan.map,
                                    &scan.master_data,
                                    &projected_occupancies[enemy_index],
                                    (option.purchase.facility.x, option.purchase.facility.y),
                                    (target.x, target.y),
                                    option.stats.movement_type,
                                    option.stats.max_movement,
                                    effective_fuel,
                                    option.stats.min_range,
                                    option.stats.max_range,
                                    player_id,
                                    &mut projected_action_caches[enemy_index],
                                )?
                            } else {
                                calculate_action_distance_to_range(
                                    &scan.map,
                                    &scan.master_data,
                                    &projected_occupancies[enemy_index],
                                    (option.purchase.facility.x, option.purchase.facility.y),
                                    (target.x, target.y),
                                    option.stats.movement_type,
                                    option.stats.max_movement,
                                    effective_fuel,
                                    option.stats.min_range,
                                    option.stats.max_range,
                                    player_id,
                                    &mut projected_action_caches[enemy_index],
                                )?
                            };
                            Some((
                                property_control::produced_action_offset(
                                    option.purchase.build_turn,
                                    distance.turns,
                                ),
                                distance,
                            ))
                        })
                        .min_by_key(|(ready_turn, distance)| {
                            (
                                *ready_turn,
                                distance.used_fuel,
                                distance.used_mp,
                                distance.firing_position.y,
                                distance.firing_position.x,
                            )
                        })
                        .map(|(ready_turn, distance)| ProductionAttackProjection {
                            ready_turn,
                            firing_position: distance.firing_position,
                            requires_movement: distance.requires_movement,
                        })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let deadline_target_index = disaggregated_control
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
            control.disaggregated && control.approach == PropertyControlApproach::Interdict
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
    if !op.property_controls.is_empty() {
        hard_deadline = Some(
            hard_deadline
                .unwrap_or(0)
                .max(EARLY_PROPERTY_CONTROL_HORIZON),
        );
    }
    Some(RollingPlanInput {
        map: scan.map.clone(),
        master_data: scan.master_data.clone(),
        damage_chart: scan.damage_chart.clone(),
        current_turn: scan.current_turn,
        opening_policy: scan
            .opening_policy
            .unwrap_or(OpeningProductionPolicy::Standard),
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
        prioritize_deadline_interdiction: op.property_controls.iter().any(|control| {
            control.disaggregated && control.approach == PropertyControlApproach::Interdict
        }),
        exact_property_control: !op.property_controls.is_empty(),
    })
}

/// 指定の枠を満たす最良の候補を選ぶ。
///
/// 物件制御付きCaptureだけは専用選定を使い、それ以外は親の共有骨格へ
/// 拡張の適合度を渡す。
pub(super) fn select_candidate(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    kind: SlotKind,
    used_facilities: &HashSet<GridPosition>,
    constraints: CandidateConstraints,
    player_id: PlayerId,
) -> Option<SlotCandidate> {
    if kind == SlotKind::Capture && !op.property_controls.is_empty() {
        return select_property_capture_candidate(
            scan,
            ctx,
            op,
            used_facilities,
            constraints.remaining_funds,
            player_id,
        );
    }
    select_candidate_by(
        scan,
        ctx,
        kind,
        used_facilities,
        constraints,
        |ctx, facility, stats| {
            slot_fitness(
                scan,
                ctx,
                op,
                kind,
                facility,
                stats,
                &CaptureArrivalPolicy { player_id },
            )
        },
    )
}

/// 拡張経路の占領枠評価。占領レーンの次目標へ、何フェーズで到着し既存の
/// 占領役より速いかで比較する。
pub(super) struct CaptureArrivalPolicy {
    pub(super) player_id: PlayerId,
}

impl CaptureSlotPolicy for CaptureArrivalPolicy {
    fn anchor(&self, op: &Operation) -> GridPosition {
        next_capture_target(op)
    }

    fn fitness(
        &self,
        scan: &BoardScan,
        ctx: &mut ReachCtx,
        anchor: GridPosition,
        facility: &GridPosition,
        stats: &UnitStats,
    ) -> Option<f32> {
        // 兵種名ではなく「物件へ何フェーズで到着し、既存の占領役より速いか」で比較する。
        // 速い兵種でも、既に割り当て済みの占領役より到着が遅い・同等なら直取り目的では
        // 買い増さない（`docs/07-project-management/plans/v4-v200-small-map-expansion-analysis.md`）。
        let candidate_arrival = property_control::capture_arrival_turns(
            &scan.map,
            &scan.master_data,
            (facility.x, facility.y),
            (anchor.x, anchor.y),
            stats.movement_type,
            stats.max_movement,
            stats.max_fuel,
            self.player_id,
            &mut ctx.action_turns,
        )
        .map(|turns| property_control::produced_action_offset(0, turns))
        .unwrap_or(u32::MAX);
        let committed_best = committed_capture_arrival(scan, ctx, anchor, self.player_id);
        property_control::capture_speed_fitness(candidate_arrival, committed_best)
    }
}

/// 見送り購入（資金を貯めて上位の候補を買う）を行うべきか。
///
/// 判定骨格は親と共有し、適合度だけ拡張の評価を使う。
pub(super) fn should_defer_purchase(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    kind: SlotKind,
    remaining_funds: u32,
    affordable_cost: u32,
    player_id: PlayerId,
) -> bool {
    should_defer_purchase_by(
        scan,
        ctx,
        op,
        remaining_funds,
        affordable_cost,
        |ctx, facility, stats| {
            slot_fitness(
                scan,
                ctx,
                op,
                kind,
                facility,
                stats,
                &CaptureArrivalPolicy { player_id },
            )
        },
    )
}

/// 余剰Combat生産について、あらかじめ計算した「1回の攻撃」での前線への寄与。
///
/// 1体は同一手番に複数の敵を攻撃できない。候補ごとの経路探索を終えた後は、この
/// 小さな表だけを使って工場枠と敵HPを対応付ける。工場ごとに戦闘を再探索しない。
#[derive(Debug, Clone, Copy)]
pub(super) struct ImmediateCombatEngagement {
    pub(in crate::ai::v4) entity: Entity,
    pub(in crate::ai::v4) current_hp: u32,
    pub(in crate::ai::v4) damage: u32,
    pub(in crate::ai::v4) fitness: f32,
}

/// 余剰Combat候補と、到達可能な実在前線Entityへの攻撃表。
#[derive(Debug, Clone)]
pub(super) struct ImmediateCombatOption {
    pub(in crate::ai::v4) operation_index: usize,
    pub(in crate::ai::v4) candidate: SlotCandidate,
    pub(in crate::ai::v4) engagements: Vec<ImmediateCombatEngagement>,
}

/// 同一手番に発注する複数ユニットを、それぞれの目標地点への到着所要ターン数・距離の
/// 総和が最小になるよう生産施設へ最適配分する。
///
/// 探索深さや施設の列挙順の都合で快速部隊が遠方の工場へ追いやられたり、占領兵が遠回り
/// することを防ぎ、盤面の移動力と幾何距離に基づく純粋な二部マッチング最適化を行う。
pub(super) fn optimize_production_facility_assignments(
    scan: &BoardScan,
    commands: &mut [PlannedProduction],
) {
    if commands.len() <= 1 {
        return;
    }
    let stats_by_type: HashMap<UnitType, &UnitStats> =
        scan.available_types.iter().map(|(t, s)| (*t, s)).collect();

    let mut improved = true;
    let mut iterations = 0;
    while improved && iterations < 32 {
        improved = false;
        iterations += 1;
        for i in 0..commands.len() {
            for j in (i + 1)..commands.len() {
                let target_i = commands[i]
                    .capture_intent
                    .as_ref()
                    .and_then(|intent| intent.mission_target)
                    .or_else(|| commands[i].deployment.as_ref().map(|d| d.staging_anchor));
                let target_j = commands[j]
                    .capture_intent
                    .as_ref()
                    .and_then(|intent| intent.mission_target)
                    .or_else(|| commands[j].deployment.as_ref().map(|d| d.staging_anchor));

                let (Some(target_i), Some(target_j)) = (target_i, target_j) else {
                    continue;
                };

                let pos_i = GridPosition {
                    x: commands[i].command.target_x,
                    y: commands[i].command.target_y,
                };
                let pos_j = GridPosition {
                    x: commands[j].command.target_x,
                    y: commands[j].command.target_y,
                };
                if pos_i == pos_j {
                    continue;
                }

                let terrain_i = scan
                    .map
                    .get_terrain(pos_i.x, pos_i.y)
                    .unwrap_or(Terrain::Factory);
                let terrain_j = scan
                    .map
                    .get_terrain(pos_j.x, pos_j.y)
                    .unwrap_or(Terrain::Factory);
                let unit_i = commands[i].command.unit_type;
                let unit_j = commands[j].command.unit_type;

                // 地形生産制約の確認
                if !scan.can_produce(terrain_j, unit_i) || !scan.can_produce(terrain_i, unit_j) {
                    continue;
                }

                let speed_i = stats_by_type
                    .get(&unit_i)
                    .map_or(1, |s| s.max_movement)
                    .max(1);
                let speed_j = stats_by_type
                    .get(&unit_j)
                    .map_or(1, |s| s.max_movement)
                    .max(1);

                let dist_i_curr = scan.map.distance(pos_i.x, pos_i.y, target_i.x, target_i.y);
                let dist_j_curr = scan.map.distance(pos_j.x, pos_j.y, target_j.x, target_j.y);
                let eta_i_curr = dist_i_curr.div_ceil(speed_i);
                let eta_j_curr = dist_j_curr.div_ceil(speed_j);
                let curr_key = (
                    eta_i_curr.saturating_add(eta_j_curr),
                    dist_i_curr.saturating_add(dist_j_curr),
                );

                let dist_i_swap = scan.map.distance(pos_j.x, pos_j.y, target_i.x, target_i.y);
                let dist_j_swap = scan.map.distance(pos_i.x, pos_i.y, target_j.x, target_j.y);
                let eta_i_swap = dist_i_swap.div_ceil(speed_i);
                let eta_j_swap = dist_j_swap.div_ceil(speed_j);
                let swap_key = (
                    eta_i_swap.saturating_add(eta_j_swap),
                    dist_i_swap.saturating_add(dist_j_swap),
                );

                if swap_key < curr_key {
                    commands[i].command.target_x = pos_j.x;
                    commands[i].command.target_y = pos_j.y;

                    commands[j].command.target_x = pos_i.x;
                    commands[j].command.target_y = pos_i.y;
                    improved = true;
                }
            }
        }
    }
}

/// 未確保のDAG区間が実在敵と交戦中なら、未許可の首都親作戦の構造枠を保留する。
///
/// 親の予約は「将来の首都占領」を保証するものであり、現在の区間を突破するCombat
/// 増援より優先するMustではない。ここで親の構造枠だけを保留すると、残額・空き工場は
/// 下段の即時増援選択へ渡り、到達性・与ダメージ・被害交換で現在前線へ投資される。
pub(super) fn defer_unapproved_capital_structure_for_active_segment(
    operations: &[Operation],
    ledger: &mut SlotLedger,
) {
    let active_segment_under_pressure = operations.iter().any(|operation| {
        operation.kind == OperationKind::Capture && operation.facts.enemy_combat_units > 0
    });
    if !active_segment_under_pressure {
        return;
    }
    for (index, operation) in operations.iter().enumerate() {
        if operation.kind == OperationKind::AssaultCapital && !operation.execution_authorized {
            ledger.close(
                index,
                SlotKind::Capture,
                UnfilledReason::DeferredBehindActiveSegment,
            );
            ledger.close(
                index,
                SlotKind::Transport,
                UnfilledReason::DeferredBehindActiveSegment,
            );
        }
    }
}

/// 占領スロット専用の計画配備状態を生成する。
pub(super) fn planned_capture_deployment(
    op: &Operation,
    target: GridPosition,
) -> PlannedDeployment {
    PlannedDeployment {
        anchor: target,
        staging_anchor: op.staging_anchor,
        posture: deployment::DeploymentPosture::Execute,
        slot_kind: SlotKind::Capture,
        priority_enemies: Vec::new(),
        threat_horizon: op.threat_horizon,
        forecast: deployment::DeploymentForecast::default(),
        plan_step: None,
        operation_binding: None,
    }
}

/// 観測済み前線へ届くCombat増援の候補表を、各手番に一度だけ作る。
///
/// RollingPlanのように候補ごとに将来ターンを探索し直すのではなく、各候補について
/// (1) 実Entityへ交戦圏まで到達できるか、(2) 前線全体の残HPをどれだけ削れるか、
/// (3) その敵が占領・輸送を行えるか、を一度に集約して比較する。予約済みの将来敵は
/// 含めないため、推測量を勝利用の余剰生産へ取り違えない。
pub(super) fn immediate_combat_options(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    operations: &[Operation],
    player_id: PlayerId,
    resolved_by_rolling_plan: &HashSet<Entity>,
) -> Vec<ImmediateCombatOption> {
    let mut options = Vec::new();
    let unit_positions = scanned_occupants(scan, player_id);

    for (op_index, operation) in operations.iter().enumerate() {
        // 実Entityが現在の前線へ出ていない作戦は、余剰Combatの送り先にしない。
        if !operation.reachable_threats.iter().any(|threat| {
            threat
                .entity
                .is_some_and(|entity| !resolved_by_rolling_plan.contains(&entity))
                && threat.available_turn == 0
        }) {
            continue;
        }
        for (facility, terrain) in &scan.free_facilities {
            for (unit_type, stats) in &scan.available_types {
                // 支援砲は前衛を含むRollingPlanで購入する。余剰の単体補充でその検証を
                // 迂回すると、前衛が失われた局面でも砲だけを追加してしまう。
                if stats.min_range > 1 || !scan.can_produce(*terrain, *unit_type) || stats.cost == 0
                {
                    continue;
                }
                let engagements = immediate_combat_engagements(
                    scan,
                    ctx,
                    operation,
                    facility,
                    stats,
                    player_id,
                    &unit_positions,
                )
                .into_iter()
                .filter(|engagement| !resolved_by_rolling_plan.contains(&engagement.entity))
                .collect::<Vec<_>>();
                if engagements.is_empty() {
                    continue;
                }
                options.push(ImmediateCombatOption {
                    operation_index: op_index,
                    candidate: SlotCandidate {
                        unit_type: *unit_type,
                        cost: stats.cost,
                        facility: *facility,
                        // 即時増援では資金より工場枠が先に尽きる。価格あたりのダメージで
                        // 正規化すると、余剰資金を抱えたまま歩兵だけを量産するため、
                        // 同じ1枠が前線全体へ与える実効ダメージを主評価にする。
                        fitness: 0.0,
                    },
                    engagements,
                });
            }
        }
    }

    options
}

/// Mustを確保した後の残額を、作成済み候補表から最も有効なCombat増援へ割り当てる。
///
/// 残額と使用済み施設だけが購入ごとに変わる。到達性・敵HP・対敵ダメージは候補表へ
/// 固定済みなので、工場数に比例して経路探索を繰り返さない。
pub(super) fn select_immediate_combat_reinforcement(
    operations: &[Operation],
    options: &[ImmediateCombatOption],
    used_facilities: &HashSet<GridPosition>,
    committed_damage: &HashMap<(usize, Entity), u32>,
    constraints: CandidateConstraints,
) -> Option<(usize, SlotCandidate, ImmediateCombatEngagement)> {
    // ここへ来る前に占領等のMust枠は確保済み。前衛の補充は頭数ではなく実効戦力を選ぶ。
    // 工場数で割った仮予算を超えても、残額で買えるなら同じ候補集合で比較する。
    let mut best: Option<(usize, SlotCandidate, ImmediateCombatEngagement)> = None;
    for option in options {
        let op_index = option.operation_index;
        let candidate = option.candidate;
        if used_facilities.contains(&candidate.facility)
            || candidate.cost > constraints.remaining_funds
        {
            continue;
        }
        // 候補ごとに「まだ割り当てられていない実敵HP」への最大の1攻撃を選ぶ。
        // ここで対象を一つに絞ることで、単体が前線全員を同時に攻撃できるかのような
        // 合算スコアを防ぎ、同手番の複数工場も別の敵へ自然に分散できる。
        let Some(engagement) = option
            .engagements
            .iter()
            .filter_map(|engagement| {
                let already_committed = committed_damage
                    .get(&(op_index, engagement.entity))
                    .copied()
                    .unwrap_or_default();
                let remaining_hp = engagement.current_hp.saturating_sub(already_committed);
                (remaining_hp > 0).then_some((*engagement, remaining_hp))
            })
            .map(|(engagement, remaining_hp)| {
                let usable_damage = engagement.damage.min(remaining_hp);
                let marginal_fitness =
                    engagement.fitness * usable_damage as f32 / engagement.damage.max(1) as f32;
                (engagement, marginal_fitness)
            })
            .max_by(|(left, left_fitness), (right, right_fitness)| {
                left_fitness
                    .total_cmp(right_fitness)
                    .then_with(|| right.entity.to_bits().cmp(&left.entity.to_bits()))
            })
            .map(|(engagement, _)| engagement)
        else {
            continue;
        };
        let slot = &mut best;
        let better = slot
            .as_ref()
            .is_none_or(|(current_op_index, current, current_engagement)| {
                let candidate_priority = operation_priority_rank(&operations[op_index]);
                let current_priority = operation_priority_rank(&operations[*current_op_index]);
                candidate_priority < current_priority
                    || (candidate_priority == current_priority
                        && (engagement
                            .fitness
                            .total_cmp(&current_engagement.fitness)
                            .is_gt()
                            || (engagement
                                .fitness
                                .total_cmp(&current_engagement.fitness)
                                .is_eq()
                                && candidate.cost < current.cost)))
            });
        if better {
            *slot = Some((op_index, candidate, engagement));
        }
    }

    best
}

/// 実在前線全体に対する、1体の増援候補の集約有効度。
///
/// 同じ価格なら早く射撃でき、敵の反撃より大きく削れる候補を優先する。また、資金が
/// 潤沢な局面で「安いが遅く脆い歩兵」を工場枠の節約として誤採用しないよう、価格は
/// ここでの主評価に入れない。資金制約は呼び出し側の残額・工場あたり予算で扱う。
pub(super) fn immediate_combat_engagements(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    operation: &Operation,
    facility: &GridPosition,
    stats: &UnitStats,
    player_id: PlayerId,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
) -> Vec<ImmediateCombatEngagement> {
    let mut engagements = Vec::new();
    for threat in &operation.reachable_threats {
        // available_turnが正の敵は観測済みでもまだ局地前線にいない。Mustの再評価対象に
        // 留め、余剰の即時投入先にしてはならない。
        let Some(entity) = threat.entity else {
            continue;
        };
        if threat.available_turn != 0 || threat.current_hp == 0 {
            continue;
        }
        let Some(action_distance) = calculate_action_distance_to_range(
            &scan.map,
            &scan.master_data,
            unit_positions,
            (facility.x, facility.y),
            (threat.position.x, threat.position.y),
            stats.movement_type,
            stats.max_movement,
            stats.max_fuel,
            stats.min_range,
            stats.max_range,
            player_id,
            &mut ctx.action_turns,
        ) else {
            continue;
        };
        let damage = best_damage(&scan.damage_chart, stats.unit_type, threat.stats.unit_type);
        if damage == 0 {
            continue;
        }
        // 占領・輸送役を止める価値は、単なる射撃ユニットより高い。価格やユニット名で
        // はなく、その敵が盤面へ与えられる行動能力だけから重みを導く。
        // 今まさに未所有拠点の上で占領を進めている敵は、占領完了を遅らせる直接の
        // `Interdict` 対象であり、まだ到達途中の占領可能敵より優先して攻撃を向ける。
        let strategic_weight = if enemy_is_capturing(scan, threat) {
            6.0
        } else if threat.stats.can_capture {
            2.0
        } else if threat.stats.max_cargo > 0 {
            1.5
        } else {
            1.0
        };
        // 新規生産unitはこの手番には撃てないが、距離1なら次フェーズの移動後に直接攻撃
        // できる。`calculate_action_distance_to_range` が含む移動フェーズを二重加算しない。
        let arrival_turns = property_control::produced_action_offset(0, action_distance.turns);
        let arrival_weight = 1.0 / (arrival_turns.saturating_add(1) as f32);
        let received_damage =
            best_damage(&scan.damage_chart, threat.stats.unit_type, stats.unit_type);
        // 被ダメージを無視して高火力unitだけを並べると、前線へ着いた直後の悪い交換を
        // 選んでしまう。1回の交戦で相手HPへ与えられる割合と、相互ダメージ比を掛ける。
        let attack_fraction = damage.min(threat.current_hp) as f32 / threat.current_hp as f32;
        let exchange_ratio = damage as f32 / damage.saturating_add(received_damage).max(1) as f32;
        engagements.push(ImmediateCombatEngagement {
            entity,
            current_hp: threat.current_hp,
            damage,
            fitness: strategic_weight * arrival_weight * attack_fraction * exchange_ratio,
        });
    }
    engagements
}

/// 次の自軍手番まで残ると確定している占有だけを、生産unitの配備ETAへ渡す。
///
/// 現在の味方前衛を永久障害物にすると、その前衛が先に退いた後の接敵セルを新造戦車が
/// 使えない。一方、占領完了まで保護中のunitと敵unitは残し、無条件の全解放にもしない。
pub(super) fn next_activation_occupants(
    scan: &BoardScan,
    operation: &Operation,
    player_id: PlayerId,
) -> HashMap<(usize, usize), OccupantInfo> {
    let opposing_player = player_id.opposite();
    scan.my_units
        .iter()
        .filter(|unit| {
            unit.entity
                .is_some_and(|entity| operation.protected_capture_entities.contains(&entity))
        })
        .map(|unit| (unit, player_id))
        .chain(scan.enemy_units.iter().map(|unit| (unit, opposing_player)))
        .map(|(unit, owner)| {
            (
                (unit.pos.x, unit.pos.y),
                OccupantInfo {
                    player_id: owner,
                    is_transport: unit.stats.max_cargo > 0,
                    unit_type: unit.stats.unit_type,
                    loadable_types: unit.stats.loadable_unit_types.clone(),
                    free_slots: unit.free_cargo,
                },
            )
        })
        .collect()
}

/// 後手の生産判断では、先手の直接攻撃unitが次手番に作戦正面へ進む一手だけを投影する。
/// 全敵手番の分岐は列挙せず、実移動ルールで到達できるセルのうち作戦anchorへ最接近する
/// 包絡だけを使う。先手側の判断と移動後射撃不可の間接unitは現在位置のまま扱う。
pub(super) fn projected_enemy_front_cells(
    scan: &BoardScan,
    operation: &Operation,
    enemy: &UnitSnapshot,
    player_id: PlayerId,
) -> Vec<GridPosition> {
    let enemy_player = player_id.opposite();
    if player_order(player_id) <= player_order(enemy_player)
        || enemy.stats.max_movement == 0
        || enemy.stats.min_range > 1
    {
        return vec![enemy.pos];
    }

    let occupancy = scanned_occupants_for_mover(scan, enemy_player, player_id);
    let fuel = if enemy.stats.max_fuel == 0 {
        u32::MAX
    } else {
        enemy.stats.max_fuel
    };
    let reachable = calculate_reachable_tile_costs(
        &scan.map,
        &occupancy,
        (enemy.pos.x, enemy.pos.y),
        enemy.stats.movement_type,
        enemy.stats.max_movement,
        fuel,
        enemy_player,
        enemy.stats.unit_type,
        &scan.master_data,
    );
    let mut best_distance = u32::MAX;
    let mut cells = Vec::new();
    for ((x, y), _) in reachable {
        // 合流先は元Entityが消える別行動なので、単体敵の位置投影には使わない。
        if (x, y) != (enemy.pos.x, enemy.pos.y) && occupancy.contains_key(&(x, y)) {
            continue;
        }
        let distance = scan
            .map
            .distance(x, y, operation.anchor.x, operation.anchor.y);
        match distance.cmp(&best_distance) {
            std::cmp::Ordering::Less => {
                best_distance = distance;
                cells.clear();
                cells.push(GridPosition { x, y });
            }
            std::cmp::Ordering::Equal => cells.push(GridPosition { x, y }),
            std::cmp::Ordering::Greater => {}
        }
    }
    if cells.is_empty() {
        vec![enemy.pos]
    } else {
        cells.sort_unstable_by_key(|cell| (cell.y, cell.x));
        cells
    }
}

/// `BoardScan`の自軍・敵軍を、任意の移動側から見た所有関係へ組み直す。
pub(super) fn scanned_occupants_for_mover(
    scan: &BoardScan,
    moving_player: PlayerId,
    opposing_player: PlayerId,
) -> HashMap<(usize, usize), OccupantInfo> {
    scan.enemy_units
        .iter()
        .map(|unit| (unit, moving_player))
        .chain(scan.my_units.iter().map(|unit| (unit, opposing_player)))
        .map(|(unit, owner)| {
            (
                (unit.pos.x, unit.pos.y),
                OccupantInfo {
                    player_id: owner,
                    is_transport: unit.stats.max_cargo > 0,
                    unit_type: unit.stats.unit_type,
                    loadable_types: unit.stats.loadable_unit_types.clone(),
                    free_slots: unit.free_cargo,
                },
            )
        })
        .collect()
}
