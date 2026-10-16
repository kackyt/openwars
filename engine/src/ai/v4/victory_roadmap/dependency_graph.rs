//! 勝利Roadmapのdependency_graph責務。

use super::strategic_selection::purpose_for;
use super::{
    GridPosition, HashMap, HashSet, IslandCampaignAssessment, IslandCampaignAssignment,
    IslandCampaignDecision, IslandCampaignPortfolio, IslandId, OperationIssueKind, OperationKind,
    OperationPhase, OperationRecoveryKind, PlayerId, StepExecutionTotals, StrategicOperation,
    StrategicOperationId, StrategicPurpose, VictoryRoadmapId, VictoryRoadmapRegistry,
};

/// 同じ陸塊でも、局地作戦と勝利条件の首都作戦は別のライフサイクルを持つ。
/// Capture/Defense間の変更は同じ局地作戦のrevisionとして履歴を引き継ぐ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum StrategicOperationScope {
    Regional,
    Capital,
}

pub(super) fn operation_scope(purpose: StrategicPurpose) -> StrategicOperationScope {
    match purpose {
        StrategicPurpose::CaptureIsland | StrategicPurpose::DefendIsland => {
            StrategicOperationScope::Regional
        }
        StrategicPurpose::AssaultCapital => StrategicOperationScope::Capital,
    }
}

/// Roadmap上の子作戦とRolling Combat Planの種別を一意に対応付ける。
///
/// 同じ首都島でも、DAGの未確保区間を取るCaptureと終端のAssaultCapitalは別の
/// 予実台帳を持つ。島IDだけで結び付けると、前段の計画を首都作戦が横取りする。
pub(super) fn rolling_operation_kind(purpose: StrategicPurpose) -> OperationKind {
    match purpose {
        StrategicPurpose::CaptureIsland => OperationKind::Capture,
        StrategicPurpose::DefendIsland => OperationKind::Defense,
        StrategicPurpose::AssaultCapital => OperationKind::AssaultCapital,
    }
}

/// Roadmap Nodeを盤面へ投影した現在の進行状態。
///
/// `OperationPhase` はSquad・輸送の工程、こちらは前提Nodeを解放できるかという
/// 戦略上の状態を表す。両者を一つのenumへ混ぜると、例えば輸送中であることと
/// 局地戦で優勢であることを同じ軸で比較してしまう。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoadmapNodeState {
    Locked,
    Ready,
    Contested,
    Dominant,
    Capturing,
    Secured,
    Blocked,
}

/// Roadmap上のOperation間にある、実行順序を持つ依存辺。
///
/// `Logistics` は島間の兵站経路、`CapitalRoute` は敵首都島で前段Milestoneを
/// 確保してから首都強襲へ移る関係を表す。いずれも前提が `Dominant` または
/// `Secured` になった時だけ後続Nodeを解放する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoadmapDependencyKind {
    Logistics,
    CapitalRoute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoadmapDependency {
    pub predecessor: StrategicOperationId,
    pub successor: StrategicOperationId,
    pub kind: RoadmapDependencyKind,
}
impl VictoryRoadmapRegistry {
    /// 局地作戦で占領担当を失い、その後のCapture再発注がまだ記録されていないか。
    ///
    /// 現在の割当集合は撃破時に空になるため、そこだけでは初回編成と再補充を区別できない。
    /// 永続する作戦履歴の最新時刻を比較し、価格や兵種名に依存しない再契約条件を返す。
    pub(crate) fn capture_replacement_pending(
        &self,
        player_id: PlayerId,
        island_id: IslandId,
    ) -> bool {
        self.operations.values().any(|operation| {
            if operation.player_id != player_id
                || operation.island_id != island_id
                || !operation.active
                || operation.purpose == StrategicPurpose::AssaultCapital
            {
                return false;
            }
            let latest_loss = operation
                .issue_history
                .iter()
                .filter(|issue| {
                    matches!(
                        issue.kind,
                        OperationIssueKind::CapturerDestroyed
                            | OperationIssueKind::CargoLostWithTransport
                    )
                })
                .map(|issue| issue.detected_turn)
                .max();
            let latest_replacement = operation
                .recovery_history
                .iter()
                .filter(|recovery| {
                    recovery.kind == OperationRecoveryKind::RequestReplacement
                        && matches!(
                            recovery.cause,
                            OperationIssueKind::CapturerDestroyed
                                | OperationIssueKind::CargoLostWithTransport
                        )
                })
                .map(|recovery| recovery.completed_turn)
                .max();
            latest_loss.is_some_and(|loss_turn| {
                latest_replacement.is_none_or(|replacement_turn| replacement_turn < loss_turn)
            })
        })
    }

    /// Regional/CapitalのOperationを同じ島・scopeごとに一度だけ作成する。
    ///
    /// 最初に全島評価から観測Nodeを作り、その後に実行assignmentが同じIDを更新する。
    /// これにより「行動候補に選ばれなかったため、島自体のNodeが消える」ことを防ぐ。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn ensure_operation(
        &mut self,
        roadmap_id: VictoryRoadmapId,
        player_id: PlayerId,
        turn: u32,
        island_id: IslandId,
        purpose: StrategicPurpose,
        tactical_anchor: GridPosition,
        objective_properties: Vec<GridPosition>,
    ) -> StrategicOperationId {
        let key = (player_id, island_id, operation_scope(purpose));
        if let Some(operation_id) = self.operation_keys.get(&key).copied() {
            return operation_id;
        }
        self.next_operation_id = self.next_operation_id.saturating_add(1);
        let operation_id = StrategicOperationId(self.next_operation_id);
        self.operation_keys.insert(key, operation_id);
        self.operations.insert(
            operation_id,
            StrategicOperation {
                id: operation_id,
                roadmap_id,
                player_id,
                island_id,
                purpose,
                created_turn: turn,
                last_observed_turn: turn,
                tactical_anchor,
                objective_properties,
                owned_objective_count: 0,
                node_state: RoadmapNodeState::Locked,
                phase: OperationPhase::Forming,
                planned_completion_turn: None,
                actual_completion_turn: None,
                assigned_transports: HashSet::default(),
                assigned_capturers: HashSet::default(),
                assigned_combat: HashSet::default(),
                combat_plan_ids: HashSet::default(),
                planned_suppression_turn: None,
                execution: StepExecutionTotals::default(),
                last_step: None,
                last_progress_turn: None,
                blocked_reason: None,
                current_issues: Vec::new(),
                issue_history: Vec::new(),
                recovery_history: Vec::new(),
                replan_count: 0,
                last_replan_turn: None,
                active: false,
            },
        );
        if let Some(roadmap) = self.roadmaps.get_mut(&player_id) {
            roadmap.operation_ids.push(operation_id);
        }
        operation_id
    }

    /// allocatorの実行候補にならない島も、Roadmap上では観測Nodeとして残す。
    ///
    /// 観測NodeはEntityを所有せず、未確保ならLocked、全施設を所有していれば
    /// Securedとなる。実行候補が同じ島に現れた場合は後続の
    /// `reconcile_assignment` が同じOperation IDをactiveな作戦へ更新する。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn reconcile_observed_island(
        &mut self,
        roadmap_id: VictoryRoadmapId,
        player_id: PlayerId,
        turn: u32,
        assessment: &IslandCampaignAssessment,
        tactical_anchor: GridPosition,
        objective_properties: Vec<GridPosition>,
        owned_properties: &HashSet<GridPosition>,
    ) {
        let purpose = if assessment.decision == IslandCampaignDecision::Defend {
            StrategicPurpose::DefendIsland
        } else {
            StrategicPurpose::CaptureIsland
        };
        let operation_id = self.ensure_operation(
            roadmap_id,
            player_id,
            turn,
            assessment.island_id,
            purpose,
            tactical_anchor,
            objective_properties.clone(),
        );
        let operation = self
            .operations
            .get_mut(&operation_id)
            .expect("作成済み観測Node");
        operation.purpose = purpose;
        operation.last_observed_turn = turn;
        operation.tactical_anchor = tactical_anchor;
        operation.objective_properties = objective_properties;
        operation.owned_objective_count = operation
            .objective_properties
            .iter()
            .filter(|position| owned_properties.contains(position))
            .count();
        operation.active = false;
        operation.planned_completion_turn = None;
        operation.combat_plan_ids.clear();
        operation.planned_suppression_turn = None;
        operation.blocked_reason = None;
        if !operation.objective_properties.is_empty()
            && operation.owned_objective_count == operation.objective_properties.len()
        {
            operation.phase = OperationPhase::Completed;
            operation.actual_completion_turn.get_or_insert(turn);
        } else {
            operation.phase = OperationPhase::Forming;
            operation.actual_completion_turn = None;
        }
    }

    /// 施設所有の変化を、現在portfolioに現れない完了済みNodeにも反映する。
    ///
    /// 兵站島を確保した後にportfolioから外れても、そのNodeは後続の前提として
    /// `Secured` のまま残る。一方で敵に取り返された場合は完了実績を取り消し、
    /// 当ターンに再提案されていれば `Ready`、されていなければ `Locked` へ戻す。
    pub(super) fn refresh_objective_ownership(
        &mut self,
        player_id: PlayerId,
        owned_properties: &HashSet<GridPosition>,
    ) {
        for operation in self
            .operations
            .values_mut()
            .filter(|operation| operation.player_id == player_id)
        {
            operation.owned_objective_count = operation
                .objective_properties
                .iter()
                .filter(|position| owned_properties.contains(position))
                .count();
            let objectives_secured = !operation.objective_properties.is_empty()
                && operation.owned_objective_count == operation.objective_properties.len();
            if !objectives_secured && operation.phase == OperationPhase::Completed {
                operation.phase = OperationPhase::Forming;
                operation.actual_completion_turn = None;
            }
        }
    }

    /// IslandCampaignと敵首都島の地上DAGを、実際の兵站経路順で接続する。
    ///
    /// `V4LogisticsPlan::selected_islands` は自軍側から敵首都側へ並んでいる。各島の
    /// Regional Operationをその順に結び、最後の兵站島と敵首都島の前段Regional
    /// Operationを `AssaultCapital` の前提にする。未割当の島はOperationを捏造せず、
    /// 次回analyzerが公開するまで辺から外す。
    pub(super) fn rebuild_dependencies(
        &mut self,
        player_id: PlayerId,
        capital_island: Option<IslandId>,
        logistics_plan: Option<&crate::ai::v4::logistics_plan::V4LogisticsPlan>,
    ) {
        let regional_for = |island_id| {
            self.operation_keys
                .get(&(player_id, island_id, StrategicOperationScope::Regional))
                .copied()
        };
        let capital_operation = capital_island.and_then(|island_id| {
            self.operation_keys
                .get(&(player_id, island_id, StrategicOperationScope::Capital))
                .copied()
        });
        let mut dependencies = HashSet::default();
        let mut previous = None;
        if let Some(plan) = logistics_plan {
            for island_id in &plan.selected_islands {
                let Some(current) = regional_for(*island_id) else {
                    continue;
                };
                if let Some(predecessor) = previous
                    && predecessor != current
                {
                    dependencies.insert(RoadmapDependency {
                        predecessor,
                        successor: current,
                        kind: RoadmapDependencyKind::Logistics,
                    });
                }
                previous = Some(current);
            }
        }
        if let (Some(capital_island), Some(capital_operation)) = (capital_island, capital_operation)
        {
            if let Some(predecessor) = previous
                && predecessor != capital_operation
            {
                dependencies.insert(RoadmapDependency {
                    predecessor,
                    successor: capital_operation,
                    kind: RoadmapDependencyKind::Logistics,
                });
            }
            if let Some(predecessor) = regional_for(capital_island)
                && predecessor != capital_operation
            {
                dependencies.insert(RoadmapDependency {
                    predecessor,
                    successor: capital_operation,
                    kind: RoadmapDependencyKind::CapitalRoute,
                });
            }
        }
        let mut dependencies = dependencies.into_iter().collect::<Vec<_>>();
        dependencies.sort_unstable_by_key(|edge| {
            (
                edge.predecessor.0,
                edge.successor.0,
                match edge.kind {
                    RoadmapDependencyKind::Logistics => 0_u8,
                    RoadmapDependencyKind::CapitalRoute => 1_u8,
                },
            )
        });
        if let Some(roadmap) = self.roadmaps.get_mut(&player_id) {
            roadmap.dependencies = dependencies;
        }
    }

    /// Operation自身の進捗だけから得られる状態を返す。
    ///
    /// 首都作戦の「まだscheduleがない」は、前提Nodeの未解放を意味し得る待機理由で
    /// あって失敗ではない。実行stepの失敗や到達不能だけを `Blocked` として扱う。
    pub(super) fn local_node_state(operation: &StrategicOperation) -> RoadmapNodeState {
        let objectives_secured = !operation.objective_properties.is_empty()
            && operation.owned_objective_count == operation.objective_properties.len();
        if operation.phase == OperationPhase::Completed || objectives_secured {
            return RoadmapNodeState::Secured;
        }
        if !operation.active {
            return RoadmapNodeState::Locked;
        }
        let waiting_for_capital_schedule = operation.purpose == StrategicPurpose::AssaultCapital
            && operation.phase == OperationPhase::Forming
            && operation.blocked_reason.as_deref()
                == Some("no executable capital assault schedule has been selected");
        if operation.phase == OperationPhase::Blocked
            || (operation.blocked_reason.is_some() && !waiting_for_capital_schedule)
        {
            return RoadmapNodeState::Blocked;
        }
        match operation.phase {
            OperationPhase::Capture => RoadmapNodeState::Capturing,
            OperationPhase::Suppress => {
                if !operation.assigned_combat.is_empty()
                    && operation.planned_suppression_turn.is_some()
                {
                    RoadmapNodeState::Dominant
                } else {
                    RoadmapNodeState::Contested
                }
            }
            OperationPhase::Forming
            | OperationPhase::Pickup
            | OperationPhase::Transit
            | OperationPhase::Drop
            | OperationPhase::Hold => RoadmapNodeState::Ready,
            OperationPhase::Completed | OperationPhase::Blocked => {
                unreachable!("完了・阻害状態は先に処理済み")
            }
        }
    }

    /// 前提Nodeの状態を適用して、実際に公開する Roadmap Node 状態を更新する。
    ///
    /// すべての前提が `Dominant` または `Secured` であるときだけ後続は局地状態を
    /// 公開できる。分岐の合流は安全側に全前提必須とし、複数前提の意味づけを
    /// 暗黙に「どれか一つでよい」としない。
    pub(super) fn refresh_node_states(&mut self, player_id: PlayerId) {
        let mut states = self
            .operations
            .iter()
            .filter(|(_, operation)| operation.player_id == player_id)
            .map(|(id, operation)| (*id, Self::local_node_state(operation)))
            .collect::<HashMap<_, _>>();
        let dependencies = self
            .roadmaps
            .get(&player_id)
            .map(|roadmap| roadmap.dependencies.clone())
            .unwrap_or_default();
        let mut predecessors =
            HashMap::<StrategicOperationId, Vec<StrategicOperationId>>::default();
        for edge in dependencies {
            predecessors
                .entry(edge.successor)
                .or_default()
                .push(edge.predecessor);
        }
        for (successor, requirements) in predecessors {
            let Some(current) = states.get(&successor).copied() else {
                continue;
            };
            if matches!(
                current,
                RoadmapNodeState::Secured | RoadmapNodeState::Blocked
            ) {
                continue;
            }
            let predecessors_ready = requirements.iter().all(|predecessor| {
                states.get(predecessor).is_some_and(|state| {
                    matches!(
                        state,
                        RoadmapNodeState::Dominant | RoadmapNodeState::Secured
                    )
                })
            });
            if !predecessors_ready {
                states.insert(successor, RoadmapNodeState::Locked);
            }
        }
        for (operation_id, state) in states {
            if let Some(operation) = self.operations.get_mut(&operation_id) {
                operation.node_state = state;
            }
        }
    }

    /// 前提未達のNodeを除いた、SquadReconcilerへ渡す当ターンのPortfolioを作る。
    ///
    /// analyzerは将来の島作戦を同時に提案できるが、ここでLocked assignmentを落とす
    /// ことで、前提Nodeが優勢になる前に後続Squadや輸送便が組まれるのを防ぐ。
    pub(super) fn approved_portfolio(
        &self,
        player_id: PlayerId,
        enemy_capital: Option<GridPosition>,
        portfolio: &IslandCampaignPortfolio,
    ) -> IslandCampaignPortfolio {
        let assignment_is_unlocked = |assignment: &IslandCampaignAssignment| {
            let purpose = purpose_for(assignment, enemy_capital);
            self.operation_keys
                .get(&(player_id, assignment.island_id, operation_scope(purpose)))
                .and_then(|operation_id| self.operations.get(operation_id))
                .is_none_or(|operation| operation.node_state != RoadmapNodeState::Locked)
        };
        let mut approved = portfolio.clone();
        approved
            .defenses
            .retain(|assignment| assignment_is_unlocked(assignment));
        approved
            .active_offensives
            .retain(|assignment| assignment_is_unlocked(assignment));
        approved
    }
}
