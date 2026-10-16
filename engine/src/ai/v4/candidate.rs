//! 生産判断の到達性・兵種適合度に基づく候補選定。

use crate::ai::v4::*;

/// 見積に含めてよいのは、同じ永続Planへ実際に配属済みのEntityだけ。
pub(in crate::ai::v4) fn committed_entities_for_plan(
    assignments: Option<&HashMap<Entity, deployment::ActiveTargetAssignment>>,
    continuation_plan_id: Option<plan_revision::PlanId>,
    policy: ProductionPolicy,
) -> HashSet<Entity> {
    assignments
        .into_iter()
        .flat_map(|assignments| assignments.iter())
        .filter_map(|(&entity, assignment)| {
            let belongs_to_continuation =
                assignment.plan_id == continuation_plan_id && assignment.plan_id.is_some();
            let is_operation_bound_immediate_combat =
                assignment.plan_id.is_none() && assignment.slot_kind == SlotKind::Combat;
            (belongs_to_continuation
                || (policy.issues_best_effort() && is_operation_bound_immediate_combat))
                .then_some(entity)
        })
        .collect()
}

/// 生産された戦闘Entityへ、実HPと与ダメージから撃破順を与える。
///
/// 価格は敵の硬さでも攻撃能力でもないため使わない。占領・輸送能力を持つ敵を先にし、
/// 同分類では必要攻撃回数が少ない敵から集中撃破する。
/// `preferred_enemy` が候補内にあればそれだけを先頭へ置く（拡張経路の物件契約用）。
/// `None` なら全候補の先頭keyが同値になり、従来の撃破順と一致する。
pub(in crate::ai::v4) fn planned_deployment(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    slot_kind: SlotKind,
    candidate: &SlotCandidate,
    preferred_enemy: Option<Entity>,
) -> Option<PlannedDeployment> {
    let stats = candidate_stats(scan, candidate);
    let (threats, eligible): (&[ThreatTarget], Vec<usize>) = match slot_kind {
        SlotKind::Intercept => (
            &op.unreachable_threats,
            (0..op.unreachable_threats.len()).collect(),
        ),
        SlotKind::Combat => {
            // Combatは標的位置ではなく射撃圏へ自力展開する。艦船を陸上anchorへ
            // 仮置きすると到達判定が再び失敗するため、生産施設を常に起点とする。
            let origin = candidate.facility;
            (
                &op.reachable_threats,
                reachable_threat_indices(scan, ctx, &op.reachable_threats, origin, stats),
            )
        }
        SlotKind::Capture | SlotKind::Transport => return None,
    };
    let mut targets = eligible
        .into_iter()
        .filter_map(|index| {
            let threat = &threats[index];
            let entity = threat.entity?;
            let damage = best_damage(&scan.damage_chart, stats.unit_type, threat.stats.unit_type);
            let attacks_to_destroy = threat.current_hp.div_ceil(damage.max(1));
            let strategic_class = if threat.stats.can_capture {
                0
            } else if threat.stats.max_cargo > 0 {
                1
            } else {
                2
            };
            let incoming_damage =
                best_damage(&scan.damage_chart, threat.stats.unit_type, stats.unit_type);
            (damage > 0 && threat.current_hp > 0).then_some((
                usize::from(preferred_enemy.is_some_and(|preferred| preferred != entity)),
                std::cmp::Reverse(incoming_damage),
                strategic_class,
                attacks_to_destroy,
                threat.position.x,
                threat.position.y,
                entity.to_bits(),
                entity,
            ))
        })
        .collect::<Vec<_>>();
    targets.sort_unstable_by_key(|target| {
        (
            target.0, target.1, target.2, target.3, target.4, target.5, target.6,
        )
    });
    let priority_enemies = targets.into_iter().map(|target| target.7).collect();
    Some(PlannedDeployment {
        operation_binding: None,
        anchor: op.anchor,
        staging_anchor: op.staging_anchor,
        posture: if op.kind == OperationKind::AssaultCapital {
            deployment::DeploymentPosture::Forming
        } else {
            deployment::DeploymentPosture::Execute
        },
        slot_kind,
        priority_enemies,
        threat_horizon: op.threat_horizon,
        forecast: deployment::DeploymentForecast::default(),
        plan_step: None,
    })
}

/// 候補の能力値を生産可能一覧から復元する。候補は同じ一覧から生成されるため必ず存在する。
pub(in crate::ai::v4) fn candidate_stats<'a>(
    scan: &'a BoardScan,
    candidate: &SlotCandidate,
) -> &'a UnitStats {
    scan.available_types
        .iter()
        .find(|(unit_type, _)| *unit_type == candidate.unit_type)
        .map(|(_, stats)| stats)
        .expect("生産候補は生産可能ユニット一覧に存在する")
}

/// 指定地点から実際に交戦できる脅威の添字を返す。
pub(in crate::ai::v4) fn reachable_threat_indices(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    threats: &[ThreatTarget],
    origin: GridPosition,
    stats: &UnitStats,
) -> Vec<usize> {
    threats
        .iter()
        .enumerate()
        .filter(|(_, threat)| {
            ctx.can_reach_engagement_envelope(
                &scan.map,
                &scan.master_data,
                (origin.x, origin.y),
                (threat.position.x, threat.position.y),
                stats.movement_type,
                stats.max_range,
            )
        })
        .map(|(index, _)| index)
        .collect()
}

pub(in crate::ai::v4) fn threats_have_counter(
    unit: &UnitStats,
    threats: &[ThreatTarget],
    eligible_indices: &[usize],
    chart: &DamageChart,
) -> bool {
    eligible_indices
        .iter()
        .any(|index| best_damage(chart, unit.unit_type, threats[*index].stats.unit_type) > 0)
}

/// 構造枠・迎撃枠の候補を、実能力と購入費のROIで比較する。
/// Combatはこの関数を通らず、RollingPlanが具体的な戦闘scheduleを比較する。
pub(in crate::ai::v4) fn normalized_candidate_fitness(
    kind: SlotKind,
    raw_fitness: f32,
    cost: u32,
    per_slot_budget: u32,
) -> f32 {
    let _ = (kind, per_slot_budget);
    let opportunity_cost = cost.max(1);
    raw_fitness * 1000.0 / opportunity_cost as f32
}

/// 枠の適合度関数。通常／拡張で評価本文が異なるため、候補走査の骨格へ明示的に渡す。
///
/// `ReachCtx` は到達判定cacheを更新するので、closureへcaptureさせず引数で受け取る。
pub(in crate::ai::v4) trait CandidateFitness:
    FnMut(&mut ReachCtx, &GridPosition, &UnitStats) -> Option<f32>
{
}

impl<F> CandidateFitness for F where
    F: FnMut(&mut ReachCtx, &GridPosition, &UnitStats) -> Option<f32>
{
}

/// 施設×兵種を走査し、渡された適合度で最良の候補を選ぶ共有骨格。
///
/// 走査順は施設が外側・兵種が内側。適合度は価格除外より前に評価し、
/// 更新は厳密に良い場合だけなので、同値なら先に見つけた候補が残る。
pub(in crate::ai::v4) fn select_candidate_by(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    kind: SlotKind,
    used_facilities: &HashSet<GridPosition>,
    constraints: CandidateConstraints,
    mut fitness_of: impl CandidateFitness,
) -> Option<SlotCandidate> {
    let mut best: Option<SlotCandidate> = None;
    let mut best_over_budget: Option<SlotCandidate> = None;

    for (facility, terrain) in &scan.free_facilities {
        if used_facilities.contains(facility) {
            continue;
        }
        for (unit_type, stats) in &scan.available_types {
            if !scan.can_produce(*terrain, *unit_type) {
                continue;
            }
            let Some(fitness) = fitness_of(ctx, facility, stats) else {
                continue;
            };
            if stats.cost == 0 || stats.cost > constraints.remaining_funds {
                continue;
            }
            // CombatはRollingPlan専用で、この候補選定へ入らない。残る枠はいずれも
            // 体数・輸送容量なので、同じ実能力なら安い方を選ぶ。
            let count_denominated = true;
            let candidate = SlotCandidate {
                unit_type: *unit_type,
                cost: stats.cost,
                facility: *facility,
                fitness: normalized_candidate_fitness(
                    kind,
                    fitness,
                    stats.cost,
                    constraints.per_slot_budget,
                ),
            };
            let slot = if count_denominated && stats.cost > constraints.per_slot_budget.max(1) {
                &mut best_over_budget
            } else {
                &mut best
            };
            let better = slot.is_none_or(|current| {
                if count_denominated {
                    // 同性能なら安い方が多く揃う
                    (candidate.fitness, current.cost) > (current.fitness, candidate.cost)
                } else {
                    // 枠が制約なので、同性能なら大きい方を投入する
                    (candidate.fitness, candidate.cost) > (current.fitness, current.cost)
                }
            });
            if better {
                *slot = Some(candidate);
            }
        }
    }

    // 1 枠あたり予算に収まる候補を優先し、無ければ予算超過でも買える候補を使う
    best.or(best_over_budget)
}

/// 積荷を目標へ届けられるか。
///
/// 降車の可否はゲームのルール（`can_unload_from_terrain`）が決めており、
/// 艦船は港か浅瀬に接岸しないと積荷を降ろせない。海上に浮いたまま
/// 隣のマスへ降ろすことはできないので、「目標の隣まで行けるか」で
/// 判定しても渡洋作戦の成否とは対応しない。そこで
///   (1) 輸送自身が到達でき、かつ降車が許される揚陸地点があり、
///   (2) その隣接マスから積荷が自力で目標まで行ける
/// マスが存在するかどうかを見る。
///
/// 地形ルールは `can_unload_from_terrain` に、隣接の定義は
/// `map.get_adjacent` に委ねるため、特定のマップやトポロジーに依存しない。
pub(in crate::ai::v4) fn can_deliver_cargo(
    map: &Map,
    registry: &MasterDataRegistry,
    ctx: &mut ReachCtx,
    from: &GridPosition,
    anchor: &GridPosition,
    transport_movement: MovementType,
    cargo_movement: MovementType,
) -> bool {
    let key = DeliveryKey {
        transport_movement,
        cargo_movement,
        start: *from,
        target: *anchor,
    };
    if let Some(cached) = ctx.delivery.get(&key) {
        return *cached;
    }

    let mut result = false;
    'outer: for y in 0..map.height {
        for x in 0..map.width {
            let Some(terrain) = map.get_terrain(x, y) else {
                continue;
            };
            // 揚陸が許される地形か（艦船なら港・浅瀬のみ）
            if !can_unload_from_terrain(Some(transport_movement), Some(terrain)) {
                continue;
            }
            // 輸送自身がその揚陸地点まで行けるか
            if !ctx.is_reachable(map, registry, (from.x, from.y), (x, y), transport_movement) {
                continue;
            }
            // 降ろした先から積荷が目標へ行けるか。
            // `is_reachable` は積荷が進入できない地形を非連結として弾くので、
            // 降車先そのものの通行可否もここで同時に判定される。
            for (ax, ay) in map.get_adjacent(x, y) {
                if ctx.is_reachable(
                    map,
                    registry,
                    (ax, ay),
                    (anchor.x, anchor.y),
                    cargo_movement,
                ) {
                    result = true;
                    break 'outer;
                }
            }
        }
    }

    ctx.delivery.insert(key, result);
    result
}

/// `from` にいる（あるいはそこで生産される）ユニットが、この作戦へ投入できるか。
///
/// 成立するのは次のいずれか。
/// (1) 自力で作戦地点まで到達できる
/// (2) それを積める輸送ユニットを空き施設で生産でき、その輸送が積荷を目標へ揚陸できる
///
/// この関数は **購入候補の採用判定（`slot_fitness`）と既存戦力の計上（台帳）の
/// 両方から呼ばれなければならない**。片側だけ条件を変えると「その枠を埋められるのに
/// 充足としては数えられない」ユニットが生まれ、要求が永久に減らずに同じユニットを
/// 買い続けるラチェットになる。
/// 判定はユニット名ではなく能力（`max_cargo` / `loadable_unit_types`）で行う。
pub(in crate::ai::v4) fn can_join_operation(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    anchor: &GridPosition,
    requires_transport: bool,
    from: &GridPosition,
    stats: &UnitStats,
) -> bool {
    // 自力で作戦地点まで到達できるか
    if ctx.is_reachable(
        &scan.map,
        &scan.master_data,
        (from.x, from.y),
        (anchor.x, anchor.y),
        stats.movement_type,
    ) {
        return true;
    }
    if !requires_transport {
        return false;
    }
    // 空き施設で生産できる輸送ユニットの候補を先に洗い出す（借用を分離するため）
    let carriers: Vec<(GridPosition, MovementType)> = scan
        .free_facilities
        .iter()
        .flat_map(|(facility, terrain)| {
            scan.available_types
                .iter()
                .filter(|(unit_type, carrier)| {
                    carrier.max_cargo > 0
                        && carrier.loadable_unit_types.contains(&stats.unit_type)
                        && scan.can_produce(*terrain, *unit_type)
                })
                .map(move |(_, carrier)| (*facility, carrier.movement_type))
        })
        .collect();

    carriers.into_iter().any(|(facility, movement_type)| {
        can_deliver_cargo(
            &scan.map,
            &scan.master_data,
            ctx,
            &facility,
            anchor,
            movement_type,
            stats.movement_type,
        )
    })
}

/// 占領枠の経路別評価。輸送・迎撃枠の評価は経路で共通なので、Capture枠の
/// 差（到達判定の地点と、参加可能な候補の適合度）だけをここへ切り出す。
pub(in crate::ai::v4) trait CaptureSlotPolicy {
    /// 占領枠の到達判定に使う地点。Capture枠を評価するときだけ呼ばれる。
    fn anchor(&self, op: &Operation) -> GridPosition;
    /// 能力・参加可否を確認済みの占領候補の適合度。
    fn fitness(
        &self,
        scan: &BoardScan,
        ctx: &mut ReachCtx,
        anchor: GridPosition,
        facility: &GridPosition,
        stats: &UnitStats,
    ) -> Option<f32>;
}

/// ユニットが指定枠にどれだけ適合するかを返す。適合しない場合は `None`。
///
/// 評価順は 到達判定 → 能力 → 参加可否 → 占領枠の経路別評価。
pub(in crate::ai::v4) fn slot_fitness(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    kind: SlotKind,
    facility: &GridPosition,
    stats: &UnitStats,
    capture_policy: &impl CaptureSlotPolicy,
) -> Option<f32> {
    let slot_anchor = if kind == SlotKind::Capture {
        capture_policy.anchor(op)
    } else {
        op.anchor
    };
    // 施設から作戦地点まで自力で到達できるか
    let self_deployable = ctx.is_reachable(
        &scan.map,
        &scan.master_data,
        (facility.x, facility.y),
        (slot_anchor.x, slot_anchor.y),
        stats.movement_type,
    );

    match kind {
        SlotKind::Capture => {
            if !stats.can_capture {
                return None;
            }
            // 自力で行けないなら、実際に運べる輸送手段が存在することが前提。
            // 「輸送枠が立っている」だけでは運搬が成立する保証にならない。
            // 台帳（`build_operation` の自軍仕分け）と同じ関数を通すこと。
            if !can_join_operation(
                scan,
                ctx,
                &slot_anchor,
                op.facts.requires_transport,
                facility,
                stats,
            ) {
                return None;
            }
            capture_policy.fitness(scan, ctx, slot_anchor, facility, stats)
        }
        SlotKind::Transport => {
            if stats.max_cargo == 0 {
                return None;
            }
            // 「占領ユニットを積める」だけでは足りない。
            // その積荷を実際に目標へ揚陸できるところまで確かめる。
            // ここを緩めると運用の当てがない輸送を買い続けることになり、
            // 逆に目標マス自体への到達を求めると艦船が永久に候補から外れる。
            let deliverable = scan
                .available_types
                .iter()
                .filter(|(unit_type, cargo)| {
                    cargo.can_capture && stats.loadable_unit_types.contains(unit_type)
                })
                .map(|(_, cargo)| cargo.movement_type)
                .collect::<Vec<_>>()
                .into_iter()
                .any(|cargo_movement| {
                    can_deliver_cargo(
                        &scan.map,
                        &scan.master_data,
                        ctx,
                        facility,
                        &op.anchor,
                        stats.movement_type,
                        cargo_movement,
                    )
                });
            if !deliverable {
                return None;
            }
            Some(stats.max_cargo as f32)
        }
        SlotKind::Intercept => {
            if op.unreachable_threats.is_empty() {
                return None;
            }
            // 迎撃には「その脅威に届く」ことと「自力で現地へ行ける」ことの両方が要る。
            // 対空戦車が海を渡れないために選ばれないのは、この 2 条件の帰結。
            if !self_deployable {
                return None;
            }
            // 価格ではなく、次の一撃で実際に削れるHPを適合度にする。
            let value = op
                .unreachable_threats
                .iter()
                .map(|threat| {
                    best_damage(&scan.damage_chart, stats.unit_type, threat.stats.unit_type)
                        .min(threat.current_hp) as f32
                })
                .sum::<f32>();
            if value <= 0.0 { None } else { Some(value) }
        }
        SlotKind::Combat => None,
    }
}

/// 主武器・副武器のうち有効な方のダメージ。
pub(in crate::ai::v4) fn best_damage(
    chart: &DamageChart,
    attacker: UnitType,
    defender: UnitType,
) -> u32 {
    chart.get_base_damage(attacker, defender).unwrap_or(0).max(
        chart
            .get_base_damage_secondary(attacker, defender)
            .unwrap_or(0),
    )
}

/// 見送り購入（資金を貯めて上位の候補を買う）を行うべきか。
///
/// 見送り購入判定の共有骨格。適合度は呼び出し側の経路別関数を使う。
///
/// 将来候補は全施設×兵種から厳密に良いものを選び、現在候補は兵種ごとに
/// 最初に生産可能な施設の適合度を使う。
pub(in crate::ai::v4) fn should_defer_purchase_by(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    remaining_funds: u32,
    affordable_cost: u32,
    mut fitness_of: impl CandidateFitness,
) -> bool {
    if acquisition_mode(&op.facts) != AcquisitionMode::SquadPackage {
        return false;
    }
    if scan.my_income == 0 {
        return false;
    }
    // 輸送が要る作戦で輸送枠がまだ空いているのに他の枠を先に埋めるのは避ける
    let mut best_future: Option<(f32, u32)> = None;
    for (facility, terrain) in &scan.free_facilities {
        for (unit_type, stats) in &scan.available_types {
            if !scan.can_produce(*terrain, *unit_type) || stats.cost <= remaining_funds {
                continue;
            }
            let Some(fitness) = fitness_of(ctx, facility, stats) else {
                continue;
            };
            let scaled = fitness * 1000.0 / stats.cost as f32;
            if best_future.is_none_or(|(current, _)| scaled > current) {
                best_future = Some((scaled, stats.cost));
            }
        }
    }
    let Some((future_fitness, future_cost)) = best_future else {
        return false;
    };
    // 現在買える候補の適合度
    let current_fitness = scan
        .available_types
        .iter()
        .filter(|(_, stats)| stats.cost <= remaining_funds && stats.cost == affordable_cost)
        .filter_map(|(_, stats)| {
            scan.free_facilities.iter().find_map(|(facility, terrain)| {
                if !scan.can_produce(*terrain, stats.unit_type) {
                    return None;
                }
                fitness_of(ctx, facility, stats).map(|f| f * 1000.0 / stats.cost as f32)
            })
        })
        .fold(0.0f32, f32::max);

    if future_fitness <= current_fitness {
        return false;
    }
    let shortfall = future_cost.saturating_sub(remaining_funds);
    let turns_to_afford = shortfall.div_ceil(scan.my_income.max(1));
    turns_to_afford <= RESERVATION_PATIENCE_TURNS
}

pub(in crate::ai::v4) fn planned_capture_deployment(
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
