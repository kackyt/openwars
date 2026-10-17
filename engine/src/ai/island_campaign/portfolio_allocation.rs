//! 作戦候補の優先順を維持した共有資源の配分。
use super::{
    CampaignResourcePool, CampaignUnitCandidate, ContestProtection, Entity, HashMap,
    IslandCampaignAssessment, IslandCampaignAssignment, IslandCampaignCandidate,
    IslandCampaignDecision, IslandCampaignPauseCause, IslandCampaignPortfolio,
    MAX_ACTIVE_OFFENSIVES, defense_priority_key, mark_unallocated, offensive_priority_key,
    protect_contest_assets, release_assignment, reserve_candidate, reserve_secure_capture_units,
    restore_contest_assets, update_assessment_for_assignment,
};

/// 全島評価を保持し、攻勢予約の後に必要な防衛へ資源を再配分する。
pub(crate) fn allocate_campaign_portfolio_with_assessments(
    candidates: Vec<IslandCampaignCandidate>,
    pool: CampaignResourcePool,
    assessments: Vec<IslandCampaignAssessment>,
) -> IslandCampaignPortfolio {
    let mut allocation = CampaignAllocation::prepare(&candidates, pool, assessments);
    allocation.allocate_offenses(&candidates);
    let defenses = allocation.allocate_defenses(candidates);
    allocation.finish(defenses)
}

struct CampaignAllocation {
    pool: CampaignResourcePool,
    catalog: HashMap<Entity, CampaignUnitCandidate>,
    assessments: Vec<IslandCampaignAssessment>,
    contest_protections: ContestProtection,
    active_offensives: Vec<IslandCampaignAssignment>,
    active_offensive_count: usize,
    logistics_prerequisite_pending: bool,
    has_expansion_candidate: bool,
}

impl CampaignAllocation {
    /// 候補観測→Contest保護→Secure先行予約→評価反映をこの順序で用意する。
    fn prepare(
        candidates: &[IslandCampaignCandidate],
        mut pool: CampaignResourcePool,
        assessments: Vec<IslandCampaignAssessment>,
    ) -> Self {
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
        audit_entry(&pool, candidates);
        let contest_protections = protect_contest_assets(candidates, &mut pool);
        reserve_secure_capture_units(
            &candidates
                .iter()
                .filter(|candidate| candidate.requirement.capture_units == 0)
                .cloned()
                .collect::<Vec<_>>(),
            &mut pool,
        );
        audit_prereservation(&pool, &contest_protections);
        let mut allocation = Self {
            pool,
            catalog,
            assessments,
            contest_protections,
            active_offensives: Vec::new(),
            active_offensive_count: 0,
            logistics_prerequisite_pending,
            has_expansion_candidate,
        };
        allocation.synchronize_assessments(candidates);
        allocation
    }

    /// 候補の最新判断を全島評価へ反映し、非候補島も保持する。
    fn synchronize_assessments(&mut self, candidates: &[IslandCampaignCandidate]) {
        for candidate in candidates {
            if let Some(assessment) = self
                .assessments
                .iter_mut()
                .find(|assessment| assessment.island_id == candidate.assessment.island_id)
            {
                *assessment = candidate.assessment.clone();
            } else {
                self.assessments.push(candidate.assessment.clone());
            }
        }
    }

    /// 攻勢優先順で一候補ずつ予約する。
    fn allocate_offenses(&mut self, candidates: &[IslandCampaignCandidate]) {
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
        for candidate in offenses {
            self.allocate_offense(&candidate);
        }
    }

    /// 兵站前提と同時攻勢上限による見送りを理由付きで記録する。
    fn skip_offense(&mut self, candidate: &IslandCampaignCandidate) -> bool {
        if self.logistics_prerequisite_pending
            && !candidate.logistics_prerequisite
            && candidate.existing_operation.is_none()
            && candidate.assessment.decision == IslandCampaignDecision::Assault
        {
            mark_unallocated(
                &mut self.assessments,
                candidate.assessment.island_id,
                IslandCampaignDecision::Observe,
                "前進兵站拠点の確保まで新規強襲を待機する",
                None,
            );
            return true;
        }
        let consumes_offensive_slot =
            candidate.assessment.decision != IslandCampaignDecision::Secure;
        if consumes_offensive_slot && self.active_offensive_count == MAX_ACTIVE_OFFENSIVES {
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                restore_contest_assets(
                    candidate.assessment.island_id,
                    &mut self.pool,
                    &mut self.contest_protections,
                );
            }
            mark_unallocated(
                &mut self.assessments,
                candidate.assessment.island_id,
                IslandCampaignDecision::Observe,
                "同時攻勢上限のため監視する",
                None,
            );
            return true;
        }
        false
    }

    /// 初回強襲と継続中の完全編成に限り将来予算を予約できる。
    fn allows_future_budget(&self, candidate: &IslandCampaignCandidate) -> bool {
        (matches!(
            candidate.assessment.decision,
            IslandCampaignDecision::Secure
                | IslandCampaignDecision::Contest
                | IslandCampaignDecision::Reinforce
        ) || candidate
            .existing_operation
            .as_ref()
            .is_some_and(|operation| operation.is_forming)
            || self.active_offensives.is_empty()
                && (candidate.assessment.decision == IslandCampaignDecision::Assault
                    || candidate.logistics_prerequisite))
    }

    /// 予約成功時だけpool・Contest保護・採用案・攻勢枠を更新する。
    fn allocate_offense(&mut self, candidate: &IslandCampaignCandidate) {
        if self.skip_offense(candidate) {
            return;
        }
        let consumes_offensive_slot =
            candidate.assessment.decision != IslandCampaignDecision::Secure;
        let allow_future_budget_reservation = self.allows_future_budget(candidate);
        if let Some((assignment, provisional)) = reserve_candidate(
            candidate,
            &self.pool,
            &self.catalog,
            allow_future_budget_reservation,
        ) {
            self.pool = provisional;
            if candidate.assessment.decision == IslandCampaignDecision::Contest {
                self.contest_protections
                    .remove(&candidate.assessment.island_id);
            }
            update_assessment_for_assignment(&mut self.assessments, &assignment);
            self.active_offensives.push(assignment);
            self.active_offensive_count = self
                .active_offensive_count
                .saturating_add(usize::from(consumes_offensive_slot));
        } else {
            self.record_unfunded_offense(candidate);
        }
    }

    /// 予約できない案の保護資源を戻し、継続・撤退の判断を記録する。
    fn record_unfunded_offense(&mut self, candidate: &IslandCampaignCandidate) {
        if candidate.assessment.decision == IslandCampaignDecision::Contest {
            restore_contest_assets(
                candidate.assessment.island_id,
                &mut self.pool,
                &mut self.contest_protections,
            );
        }
        let has_active_expansion = self
            .active_offensives
            .iter()
            .any(|assignment| assignment.decision == IslandCampaignDecision::Expand);
        let (decision, reason) = if candidate.assessment.decision
            == IslandCampaignDecision::Reinforce
            && (has_active_expansion || self.has_expansion_candidate)
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
            &mut self.assessments,
            candidate.assessment.island_id,
            decision,
            reason,
            None,
        );
    }

    /// 防衛優先順を保ち、必要なら末尾の攻勢予約から解放する。
    fn allocate_defenses(
        &mut self,
        candidates: Vec<IslandCampaignCandidate>,
    ) -> Vec<IslandCampaignAssignment> {
        let mut defense_candidates: Vec<_> = candidates
            .into_iter()
            .filter(|candidate| candidate.assessment.decision == IslandCampaignDecision::Defend)
            .collect();
        defense_candidates.sort_by_key(defense_priority_key);
        let mut defenses = Vec::new();
        for candidate in defense_candidates {
            if let Some(assignment) = self.allocate_defense(&candidate) {
                defenses.push(assignment);
            }
        }
        defenses
    }

    /// 一防衛候補を満たすまで攻勢を一件ずつ解き、資源を取り直す。
    fn allocate_defense(
        &mut self,
        candidate: &IslandCampaignCandidate,
    ) -> Option<IslandCampaignAssignment> {
        loop {
            if let Some((assignment, provisional)) =
                reserve_candidate(candidate, &self.pool, &self.catalog, false)
            {
                self.pool = provisional;
                update_assessment_for_assignment(&mut self.assessments, &assignment);
                return Some(assignment);
            }
            let Some(released) = self.active_offensives.pop() else {
                mark_unallocated(
                    &mut self.assessments,
                    candidate.assessment.island_id,
                    IslandCampaignDecision::Observe,
                    "完全な防衛編成を予約できないため監視する",
                    None,
                );
                return None;
            };
            release_assignment(&released, &mut self.pool, &self.catalog);
            mark_unallocated(
                &mut self.assessments,
                released.island_id,
                IslandCampaignDecision::Observe,
                "Threatened島防衛のため攻勢を一時停止",
                Some(IslandCampaignPauseCause::DefensePreemption),
            );
        }
    }

    /// 全島評価の順を確定して採用portfolioを返す。
    fn finish(mut self, defenses: Vec<IslandCampaignAssignment>) -> IslandCampaignPortfolio {
        self.assessments
            .sort_by_key(|assessment| assessment.island_id.0);
        IslandCampaignPortfolio {
            islands: self.assessments,
            active_offensives: self.active_offensives,
            defenses,
        }
    }
}

/// 診断は選択に戻さず、旧入口と同じ順序で記録する。
fn audit_entry(pool: &CampaignResourcePool, candidates: &[IslandCampaignCandidate]) {
    if crate::ai::island_campaign_analysis::pool_audit_enabled() {
        audit_units(pool);
        audit_candidates(candidates);
    }
}
/// 入口の共有資源を記録する。
fn audit_units(pool: &CampaignResourcePool) {
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
}

/// 入口の候補順と要求を記録する。
fn audit_candidates(candidates: &[IslandCampaignCandidate]) {
    for candidate in candidates {
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

/// Contest保護とSecure要員の先行予約結果を記録する。
fn audit_prereservation(pool: &CampaignResourcePool, contest_protections: &ContestProtection) {
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
}
