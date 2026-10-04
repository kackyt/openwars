//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

/// 全島評価を保持したまま、行動可能な候補だけへ共有資源を予約する。
///
/// `IslandCampaignPortfolio.islands` は作戦候補一覧ではなく盤面の全島評価である。
/// そのため、目標座標を持たないSecured/Observed島をallocatorから外しても、
/// その評価自体は捨てない。
pub(crate) fn allocate_campaign_portfolio_with_assessments(
    candidates: Vec<IslandCampaignCandidate>,
    mut pool: CampaignResourcePool,
    mut assessments: Vec<IslandCampaignAssessment>,
) -> IslandCampaignPortfolio {
    let logistics_prerequisite_pending = candidates
        .iter()
        .any(|candidate| candidate.logistics_prerequisite);
    let has_expansion_candidate = candidates
        .iter()
        .any(|candidate| candidate.assessment.decision == IslandCampaignDecision::Expand);
    let catalog: HashMap<_, _> = pool
        .units
        .iter()
        .cloned()
        .map(|unit| (unit.entity, unit))
        .collect();
    // 【一時的な診断専用】allocator入口のpoolを記録する。
    if crate::ai::island_campaign_analysis::pool_audit_enabled() {
        for unit in &pool.units {
            eprintln!(
                "POOL_AUDIT stage=allocator_entry entity={:?} type={:?} can_capture={} can_secure_local={} island={:?} assigned_island={:?} transporting={}",
                unit.entity,
                unit.unit_type,
                unit.can_capture,
                unit.can_secure_local_property,
                unit.island_id,
                unit.assigned_island,
                unit.is_transporting,
            );
        }
        for candidate in &candidates {
            eprintln!(
                "POOL_AUDIT stage=candidate island={:?} decision={:?} capture_units={} transport_slots={} ground_combat_units={} target={:?} existing_op={}",
                candidate.assessment.island_id,
                candidate.assessment.decision,
                candidate.requirement.capture_units,
                candidate.requirement.transport_slots,
                candidate.requirement.ground_combat_units,
                candidate.target_position,
                candidate.existing_operation.is_some(),
            );
        }
    }
    let mut contest_protections = protect_contest_assets(&candidates, &mut pool);
    reserve_secure_capture_units(
        &candidates
            .iter()
            .filter(|candidate| candidate.requirement.capture_units == 0)
            .cloned()
            .collect::<Vec<_>>(),
        &mut pool,
    );
    // 【一時的な診断専用】先行予約（Contest保護・Secure現地要員）の適用後を記録する。
    if crate::ai::island_campaign_analysis::pool_audit_enabled() {
        let remaining: Vec<_> = pool
            .units
            .iter()
            .map(|unit| (unit.entity, unit.can_capture, unit.assigned_island))
            .collect();
        eprintln!(
            "POOL_AUDIT stage=after_prereservation contest_protected={:?} remaining={:?}",
            contest_protections, remaining
        );
    }
    // V4の兵站計画などが候補の評価を更新している場合は、同じ島の全島評価へ
    // 最新の判断を反映する。候補化されなかった島はここで上書きされない。
    for candidate in &candidates {
        if let Some(assessment) = assessments
            .iter_mut()
            .find(|assessment| assessment.island_id == candidate.assessment.island_id)
        {
            *assessment = candidate.assessment.clone();
        } else {
            assessments.push(candidate.assessment.clone());
        }
    }
    let mut offenses: Vec<_> = candidates
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.assessment.decision,
                IslandCampaignDecision::Expand
                    | IslandCampaignDecision::Secure
                    | IslandCampaignDecision::Contest
                    | IslandCampaignDecision::Reinforce
                    | IslandCampaignDecision::Assault
            ) && (candidate.assessment.decision != IslandCampaignDecision::Secure
                || candidate.requirement.capture_units > 0
                || candidate.existing_operation.is_some())
        })
        .cloned()
        .collect();
    offenses.sort_by_key(offensive_priority_key);
    let mut active_offensives = Vec::new();
    let mut active_offensive_count = 0_usize;
    for candidate in offenses {
        if logistics_prerequisite_pending
            && !candidate.logistics_prerequisite
            && candidate.existing_operation.is_none()
            && candidate.assessment.decision == IslandCampaignDecision::Assault
        {
            mark_unallocated(
                &mut assessments,
                candidate.assessment.island_id,
                IslandCampaignDecision::Observe,
                "前進兵站拠点の確保まで新規強襲を待機する",
                None,
            );
            continue;
        }
        let consumes_offensive_slot =
            candidate.assessment.decision != IslandCampaignDecision::Secure;
        if consumes_offensive_slot && active_offensive_count == MAX_ACTIVE_OFFENSIVES {
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                restore_contest_assets(
                    candidate.assessment.island_id,
                    &mut pool,
                    &mut contest_protections,
                );
            }
            mark_unallocated(
                &mut assessments,
                candidate.assessment.island_id,
                IslandCampaignDecision::Observe,
                "同時攻勢上限のため監視する",
                None,
            );
            continue;
        }
        // 最上位の新規Assaultは全額がまだ無くても作戦として保持し、現在資金を
        // 下位Expandへ流さず複数ターンで完全パッケージを調達する。
        let allow_future_budget_reservation = matches!(
            candidate.assessment.decision,
            IslandCampaignDecision::Secure
                | IslandCampaignDecision::Contest
                | IslandCampaignDecision::Reinforce
        ) || candidate
            .existing_operation
            .as_ref()
            .is_some_and(|operation| operation.is_forming)
            || active_offensives.is_empty()
                && (candidate.assessment.decision == IslandCampaignDecision::Assault
                    || candidate.logistics_prerequisite);
        if let Some((assignment, provisional)) =
            reserve_candidate(&candidate, &pool, &catalog, allow_future_budget_reservation)
        {
            pool = provisional;
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                contest_protections.remove(&candidate.assessment.island_id);
            }
            update_assessment_for_assignment(&mut assessments, &assignment);
            active_offensives.push(assignment);
            active_offensive_count =
                active_offensive_count.saturating_add(usize::from(consumes_offensive_slot));
        } else {
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                restore_contest_assets(
                    candidate.assessment.island_id,
                    &mut pool,
                    &mut contest_protections,
                );
            }
            let has_active_expansion = active_offensives
                .iter()
                .any(|assignment| assignment.decision == IslandCampaignDecision::Expand);
            let (decision, reason) = if candidate.assessment.decision
                == IslandCampaignDecision::Reinforce
                && (has_active_expansion || has_expansion_candidate)
            {
                (
                    IslandCampaignDecision::Withdraw,
                    "より投資回収効率のよい中立島作戦を優先する",
                )
            } else if candidate.assessment.decision == IslandCampaignDecision::Reinforce {
                (
                    IslandCampaignDecision::Contest,
                    "完全増援を予約できないため現地作戦を継続する",
                )
            } else {
                (
                    IslandCampaignDecision::Observe,
                    "完全編成を予約できないため監視する",
                )
            };
            mark_unallocated(
                &mut assessments,
                candidate.assessment.island_id,
                decision,
                reason,
                None,
            );
        }
    }

    let mut defense_candidates: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| candidate.assessment.decision == IslandCampaignDecision::Defend)
        .collect();
    defense_candidates.sort_by_key(defense_priority_key);
    let mut defenses = Vec::new();
    for candidate in defense_candidates {
        loop {
            if let Some((assignment, provisional)) =
                reserve_candidate(&candidate, &pool, &catalog, false)
            {
                pool = provisional;
                update_assessment_for_assignment(&mut assessments, &assignment);
                defenses.push(assignment);
                break;
            }
            let Some(released) = active_offensives.pop() else {
                mark_unallocated(
                    &mut assessments,
                    candidate.assessment.island_id,
                    IslandCampaignDecision::Observe,
                    "完全な防衛編成を予約できないため監視する",
                    None,
                );
                break;
            };
            release_assignment(&released, &mut pool, &catalog);
            mark_unallocated(
                &mut assessments,
                released.island_id,
                IslandCampaignDecision::Observe,
                "Threatened島防衛のため攻勢を一時停止",
                Some(IslandCampaignPauseCause::DefensePreemption),
            );
        }
    }

    assessments.sort_by_key(|assessment| assessment.island_id.0);
    IslandCampaignPortfolio {
        islands: assessments,
        active_offensives,
        defenses,
    }
}
