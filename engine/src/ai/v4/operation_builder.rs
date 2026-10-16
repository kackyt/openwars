//! 生産判断の作戦候補と観測脅威の構築。

use crate::ai::v4::*;

/// 距離と移動力から到達ターン数を見積もる。
pub(in crate::ai::v4) fn eta_turns(
    map: &Map,
    from: &GridPosition,
    to: &GridPosition,
    movement: u32,
) -> u32 {
    let distance = map.distance(from.x, from.y, to.x, to.y);
    distance.div_ceil(movement.max(1))
}

/// 盤面から作戦の一覧を組み立てる。
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
}
struct OperationAssembly<'a, 'b> {
    scan: &'a BoardScan,
    reference: &'a UnitStats,
    active_objectives: &'a [ActivePlanObjective],
    sources: &'a OperationSources<'b>,
    layout: &'a OperationLayout,
}
pub(in crate::ai::v4) fn build_operations(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    active_objectives: &[ActivePlanObjective],
) -> Vec<Operation> {
    let Some(reference) = scan.reference_capture_unit().cloned() else {
        return Vec::new();
    };

    // V4の作戦源は、島campaignが選んだCapture/Defenseと、既に実行を始めた
    // Combat planだけに限定する。全未所有拠点を距離で束ねる第二の目標選定器を
    // 併用すると、勝利ロードマップ外の前線へ生産予算が流れるためである。
    let (raw, sources) = collect_operation_sources(scan, active_objectives);
    let scored = score_operation_candidates(scan, active_objectives, &reference, raw, &sources);
    let layout = prepare_operation_layout(scan, &reference, &scored, &sources);
    let input = OperationAssembly {
        scan,
        reference: &reference,
        active_objectives,
        sources: &sources,
        layout: &layout,
    };
    scored
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| assemble_operation(&input, ctx, index, candidate))
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
    raw: Vec<(OperationKind, Vec<GridPosition>)>,
    sources: &OperationSources<'_>,
) -> Vec<OperationCandidate> {
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

fn prepare_operation_layout(
    scan: &BoardScan,
    _reference: &UnitStats,
    scored: &[OperationCandidate],
    sources: &OperationSources<'_>,
) -> OperationLayout {
    let anchors: Vec<GridPosition> = scored
        .iter()
        .map(|candidate| {
            sources
                .campaign_for_cluster(candidate.kind, &candidate.cluster)
                .map_or_else(
                    || anchor_of(&candidate.cluster, scan),
                    |objective| objective.anchor,
                )
        })
        .collect();
    let horizons: Vec<u32> = scored
        .iter()
        .map(|candidate| operation_threat_horizon(candidate.kind, candidate.lead))
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
    }
}

fn assemble_operation(
    input: &OperationAssembly<'_, '_>,
    ctx: &mut ReachCtx,
    index: usize,
    candidate: OperationCandidate,
) -> Operation {
    let OperationAssembly {
        scan,
        reference,
        active_objectives,
        sources,
        layout,
    } = *input;
    let OperationLayout {
        anchors,
        horizons,
        assignment_enabled,
    } = layout;
    let empty_entity_set = HashSet::default();
    let OperationCandidate {
        lead,
        kind,
        cluster,
        ..
    } = candidate;
    let anchor = anchors[index];
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
        forced_target_enemies,
        protected_capture_entities,
        lead,
    );
    if let Some(objective) = planning_objective {
        operation.island_id = Some(objective.island_id);
        operation.staging_anchor = objective.staging_anchor;
        operation.execution_authorized = objective.execution_authorized;
        operation.protected_capture_entities = objective.protected_capture_entities.clone();
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

/// クラスタの代表地点。自軍生産施設に最も近い拠点を選ぶ。
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

/// 生産施設から代表地点までの展開リードタイム（最短）。
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

pub(super) fn operation_threat_horizon(kind: OperationKind, deploy_lead_time: u32) -> u32 {
    let capital_search_turns =
        if crate::ai::code_path::is_extended_at("v4/mod.rs::operation_threat_horizon") {
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

/// 敵が期限内に自力到着できる作戦だけを比較し、その中で最短の1件へ帰属させる。
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

/// 敵を、地形的に到達できる最寄りの作戦へ一意に帰属させる。
///
/// 作戦の期限は毎ターン変化する予測値であり、敵が局地目標から遠いという理由で
/// Combat計画の入力から消してはならない。期限は防衛案の比較にだけ用いる。
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

/// 敵施設も期限内に到着できる最寄り作戦へ一意に割り当て、全収入の重複計上を防ぐ。
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

/// 1 つの作戦について観測量を集め、枠を導出する。
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
        } else if unit.stats.max_cargo == 0 {
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

    let enemy_reinforcement_funds = anchor_index.map_or(0, |index| {
        projected_enemy_reinforcement_funds(scan, ctx, anchors, horizons, index)
    });

    // 輸送 1 往復にかかるターン数（片道リードタイムの 2 倍）
    let transport_round_trip_turns = deploy_lead_time.saturating_mul(2).max(1);

    let facts = OperationFacts {
        target_property_count: cluster.len() as u32,
        friendly_capture_units_committed,
        enemy_combat_units,
        friendly_combat_units_committed,
        enemy_reinforcement_funds,
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
    // 必要数と完了判定はrolling plannerのHPシミュレーションが決める。
    slots.combat_plan_required = u32::from(
        !reachable_threats.is_empty()
            || (kind == OperationKind::AssaultCapital && enemy_reinforcement_funds > 0),
    );
    // AssaultCapitalも局地Captureと同じ作戦内で複数施設を前進目標として持つ。
    // 構造生産を島campaignが予約済みの手番は`allow_structural_slots`側で一括して
    // Capture/Transportを無効化するため、ここで恒久的に0へ落としてはならない。

    let mut operation = Operation {
        operation_id: None,
        capture_replacement_pending: false,
        property_control: None,
        property_controls: Vec::new(),
        capture_lane_targets: Vec::new(),
        planned_capture_units: Vec::new(),
        expected_reinforcements: Vec::new(),
        stress_reinforcement_funds: 0,
        kind,
        island_id: None,
        anchor,
        staging_anchor: anchor,
        execution_authorized: true,
        protected_capture_entities: HashSet::default(),
        objective_properties: cluster.to_vec(),
        threat_horizon: anchor_index
            .and_then(|index| horizons.get(index).copied())
            .unwrap_or(0),
        slots,
        facts,
        filled: OperationSlots::default(),
        unreachable_threats,
        reachable_threats,
        unavoidable_reinforcements: Vec::new(),
        reinforcement_contingencies: Vec::new(),
        contingency_reserve_funds: 0,
    };
    let assessment =
        enemy_reinforcement_assessment(scan, ctx, &operation, operation.threat_horizon.max(1));
    operation.unavoidable_reinforcements = assessment.unavoidable;
    operation.reinforcement_contingencies = assessment.contingencies;
    operation.contingency_reserve_funds =
        contingency_reserve_now(&operation.reinforcement_contingencies, scan.my_income);
    if !operation.unavoidable_reinforcements.is_empty() {
        operation.slots.combat_plan_required = 1;
    }
    operation
}

/// 敵増援ごとに接触turnと、観測後に生産する最速counterの接触turnを比較する。
/// counterが間に合う仮説は現在の撃破対象へ混ぜず、条件付き予約として保持する。
pub(in crate::ai::v4) fn enemy_reinforcement_assessment(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    horizon: u32,
) -> ReinforcementAssessment {
    let scenario_budget = op.facts.enemy_reinforcement_funds;
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

    for build_turn in 1..horizon {
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
                    let available_turn = build_turn.saturating_add(1).saturating_add(eta);
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
                    can_be_engaged.then_some((
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
                        observed_enemy_types.contains(&stats.unit_type)
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
            // 占領可能増援は、見てからcounterを一体ずつ足す方式では数の圧力に
            // 一手遅れる。前線の所有権を直接変え得るため、現在の作戦packageへ
            // 必ず含める。特殊兵種だけを観測後counterの条件付き計画に残す。
            if stats.can_capture || observed_enemy_types.contains(&stats.unit_type) {
                assessment.unavoidable.push(reinforcement);
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
                assessment.unavoidable.push(reinforcement);
            }
        }
    }
    assessment
}

/// 観測した敵1体へ、生産slot・移動・与ダメージを満たす最速counterを返す。
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

/// 将来収入で賄えない累積counter費用だけを、現在残高から予約する。
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
