//! 手番内の生産判断に必要な補助処理。
use crate::ai::v4::board_scan::scanned_occupants;
use crate::ai::v4::property_control::enemy_is_capturing;

#[allow(unused_imports)]
use crate::ai::planning::decision::AdoptedTurnPortfolio;

#[derive(Debug, Clone, Copy)]
pub(in crate::ai::v4) struct ImmediateCombatEngagement {
    pub(in crate::ai::v4) entity: Entity,
    pub(in crate::ai::v4) current_hp: u32,
    pub(in crate::ai::v4) damage: u32,
    pub(in crate::ai::v4) fitness: f32,
}

#[derive(Debug, Clone)]
pub(in crate::ai::v4) struct ImmediateCombatOption {
    pub(in crate::ai::v4) operation_index: usize,
    pub(in crate::ai::v4) candidate: SlotCandidate,
    pub(in crate::ai::v4) engagements: Vec<ImmediateCombatEngagement>,
}

pub(in crate::ai::v4) fn immediate_combat_options(
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

pub(in crate::ai::v4) fn select_immediate_combat_reinforcement(
    operations: &[Operation],
    options: &[ImmediateCombatOption],
    used_facilities: &HashSet<GridPosition>,
    committed_damage: &HashMap<(usize, Entity), u32>,
    constraints: CandidateConstraints,
    policy: ProductionPolicy,
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
                let candidate_priority = operation_priority_rank(&operations[op_index], policy);
                let current_priority =
                    operation_priority_rank(&operations[*current_op_index], policy);
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

pub(in crate::ai::v4) fn immediate_combat_engagements(
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
use super::*;
pub(super) fn allocate_immediate_reinforcements(
    input: &ProductionInputs<'_>,
    state: &mut ProductionState,
    plan_registry: &V4RollingPlanRegistry,
) -> SurplusBudget {
    let ProductionInputs {
        scan, player_id, ..
    } = *input;
    let ProductionState {
        ctx,
        operations,
        plan_trace,
        allocation,
        rolling_plans,
        commands,
        issued_plan_purchase_cost,
        ..
    } = state;
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
        .saturating_sub(*issued_plan_purchase_cost);
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
    let immediate_combat_options =
        immediate_combat_options(scan, ctx, operations, player_id, &resolved_by_rolling_plan);
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
            operations,
            &immediate_combat_options,
            allocation.used_facilities(),
            &immediately_committed_damage,
            CandidateConstraints {
                remaining_funds: immediate_reinforcement_funds,
                per_slot_budget,
            },
            input.policy,
        ) else {
            break;
        };

        let remaining_funds_before = allocation.remaining_funds();
        let deployment = planned_deployment(
            scan,
            ctx,
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
                operation_priority_rank(&operations[op_index], input.policy),
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
        record_surplus_purchase(
            &operations[op_index],
            player_id,
            plan_trace,
            commands,
            SurplusPurchase {
                candidate,
                role: SlotKind::Combat,
                purpose: PurchasePurpose::ImmediateReinforcement,
                deployment: Some(deployment),
                remaining_funds_before,
            },
        );
    }

    SurplusBudget {
        funds: immediate_reinforcement_funds,
        unissued_plan_reserve,
        contingency_reserve,
        resolved: resolved_by_rolling_plan,
    }
}
