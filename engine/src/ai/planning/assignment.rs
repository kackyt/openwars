//! 所属の提案・競合解決・差分確定。
//!
//! - 確定所属と履歴は `UnitOperationRegistry` に保持する。
//! - 編成中の候補は `Squad.roster` だけに保持する。複数Squadへの提案を許し、
//!   `AssignmentProposal::from_roster_view` → resolve → commitで一意化する。
//! - commitは確定所属を記録し、敗者参照をrosterから除く。履歴からmemberを復活させない。
//!
//! データ型は `squad_model`、勝者の優先規則は `candidate` に置く。
//! 本番のmanagerは `SquadEdit` だけを返し、所属変更はledgerの更新口に限定する。
//! テストfixtureも同じrosterを読み、別storeへの同期や実行分岐を持たない。

use self::candidate::{
    SquadAssignmentCandidate, assignment_candidate_key, assignment_mission_rank,
    operation_owner_for_squad,
};
use self::squad_model::{MissionPhase, MissionType, SquadId};
use crate::ai::islands::IslandId;
use crate::ai::operation_assignment::{
    OperationOwner, OperationUnitRole, UnitOperationAssignment, UnitOperationRegistry,
};
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, PlayerId};
use bevy_ecs::prelude::*;
use std::collections::BTreeSet;

pub(crate) mod candidate;
#[cfg(test)]
mod production_tests;
pub mod squad_model;
pub use squad_model::{
    CargoRoster, MemberRoster, Squad, SquadEdit, SquadList, SquadManager, SquadRoster,
    TransportBinding,
};
pub(crate) use squad_model::{ProposalCommit, RosterCommit, SquadListCommit};

/// 正規化所属層（[`UnitOperationRegistry`]）の書込み権。このmodule外では生成できない。
///
/// registryの書込みmethodはこの値を要求するため、本番buildでは所属の書込みが
/// [`OperationLedger`] の確定口に限定される。
#[derive(Clone, Copy)]
pub(crate) struct LedgerKey(());

impl LedgerKey {
    const GRANT: Self = Self(());

    /// テストの盤面組立てだけで使う。
    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        Self::GRANT
    }
}

/// 所属台帳の確定口。正規化所属層（registry）を借用し、提案を解決した差分だけを書き込む。
pub(crate) struct OperationLedger<'a> {
    registry: &'a mut UnitOperationRegistry,
}

impl<'a> OperationLedger<'a> {
    pub(crate) fn open(registry: &'a mut UnitOperationRegistry) -> Self {
        Self { registry }
    }

    /// SquadIdで指す所属提案の更新口を開く。
    pub(crate) fn proposals(manager: &mut SquadManager) -> ProposalCommit<'_> {
        manager.proposal_commit()
    }

    /// Squad一覧の構造変更（解散・退避・復元・手動発番）の確定口を開く。
    pub(crate) fn squad_list(manager: &mut SquadManager) -> SquadListCommit<'_> {
        manager.list_commit()
    }

    /// keyed提案トランザクションまたは一覧外draftのroster adapterを開く。
    pub(crate) fn roster<'s>(squad: &'s mut SquadEdit<'_>) -> RosterCommit<'s> {
        RosterCommit::open(squad)
    }

    /// 盤面から消えたEntityの所属を解放する（正規化の入口）。
    pub(crate) fn retain_live(&mut self, live_entities: &HashSet<Entity>) {
        self.registry
            .apply_retain_live_entities(LedgerKey::GRANT, live_entities);
    }

    /// 撃破イベントで所属を解放する。
    pub(crate) fn release_destroyed(&mut self, entity: Entity) {
        self.registry.apply_release_entity(LedgerKey::GRANT, entity);
    }

    /// 生産直後の作戦意図を、まだownerが無いEntityだけの初期所属として登録する。
    /// 既にownerを持つEntityは、その後の明示的な移管を発注時anchorで巻き戻さない。
    pub(crate) fn seed_produced(&mut self, entity: Entity, owner: OperationOwner, turn: u32) {
        if self.registry.assignment(entity).is_some() {
            return;
        }
        self.registry.apply_assign(
            LedgerKey::GRANT,
            entity,
            UnitOperationAssignment {
                owner,
                squad_id: None,
                role: OperationUnitRole::Member,
                assigned_turn: turn,
            },
        );
    }

    /// 輸送波の手動編成で確定した作戦ownerを、具体Squad未接続（squad_id=None）で登録する。
    /// 順序は呼出し側の輸送波編成順を保つ。
    pub(crate) fn transfer_owners(
        &mut self,
        transfers: impl IntoIterator<Item = (Entity, OperationUnitRole, OperationOwner)>,
        turn: u32,
    ) {
        for (entity, role, owner) in transfers {
            self.registry.apply_assign(
                LedgerKey::GRANT,
                entity,
                UnitOperationAssignment {
                    owner,
                    squad_id: None,
                    role,
                    assigned_turn: turn,
                },
            );
        }
    }

    /// 生産完了Entityの所属提案を、受付順（配備の全件→campaignの全件）に1回だけ確定する。
    ///
    /// 新設Squadは提案順に発番するため、各台帳が直接作成していたときと同じSquadIdになる。
    /// 同じEntityが両台帳から提案された場合も両方を適用し、一意化は正規化resolveに任せる。
    pub(crate) fn commit_produced(
        manager: &mut SquadManager,
        proposal: ProducedAssignmentProposal,
    ) -> ProducedAssignmentDelta {
        let mut squads: Vec<SquadId> = Vec::with_capacity(proposal.joins.len());
        let mut proposals = OperationLedger::proposals(manager);
        for join in proposal.joins {
            let existing = match join.squad {
                ProducedSquad::Existing(id) => Some(id),
                ProducedSquad::Proposed(index) => Some(squads[index]),
                ProducedSquad::Create(_) => None,
            }
            .filter(|id| {
                proposals
                    .squad_mut(*id)
                    .is_some_and(|squad| squad.plan.owner_id == Some(join.player))
            });
            let empty = || (BTreeSet::new(), None, Vec::new());
            let squad_id = match (existing, join.squad) {
                (Some(id), _) => {
                    // 既存Squadのrosterへ所属を提案する。一意化は後段のresolveが行う。
                    proposals.join(id, join.entity);
                    id
                }
                (None, ProducedSquad::Create(mission)) => {
                    let (members, transport, cargo) = empty();
                    let id = proposals
                        .create_owned(mission, join.player, members, transport, cargo)
                        .id;
                    proposals.join(id, join.entity);
                    id
                }
                // 不変条件違反: Existing/Proposedは提案時に同playerのSquadとして検証済みで、
                // 提案から確定までにSquadを消す経路は無い。正常なCreateとは区別して検知し、
                // release buildでは所属を捨てずslotの任務種別で新設する（従来経路の非到達分岐）。
                (None, squad) => {
                    if cfg!(debug_assertions) {
                        panic!(
                            "生産所属の受入Squadが確定時に見つからない: {squad:?} entity={:?}",
                            join.entity
                        );
                    }
                    let (members, transport, cargo) = empty();
                    let id = proposals
                        .create_owned(
                            join.fallback_mission,
                            join.player,
                            members,
                            transport,
                            cargo,
                        )
                        .id;
                    proposals.join(id, join.entity);
                    id
                }
            };
            // 補充は所属だけを増やす。現在任務の目標・出撃状態はSquad自身が保持し、
            // 生産発注時の古い意図で進行中の本隊をFormingへ巻き戻さない。
            if existing.is_none() {
                let mut squad = proposals.squad_mut(squad_id).expect("受入Squad");
                squad.plan_mut().target = join.target;
                squad.plan_mut().target_island = join.target_island;
                squad.plan_mut().phase = join.initial_phase;
            }
            squads.push(squad_id);
        }
        ProducedAssignmentDelta { squads }
    }

    /// portfolio claimの解決結果を確定する。終了作戦の解放→優先順の登録→未claimの解放の順。
    pub(crate) fn commit_claims(&mut self, delta: ClaimDelta) {
        for owner in delta.released_owners {
            self.registry
                .apply_release_operation(LedgerKey::GRANT, owner);
        }
        for op in delta.ops {
            match op {
                ClaimOp::Conflict => self.registry.apply_rejected_conflict(LedgerKey::GRANT),
                ClaimOp::Assign(entity, assignment) => {
                    self.registry
                        .apply_assign(LedgerKey::GRANT, entity, assignment)
                }
            }
        }
        self.registry.apply_release_unclaimed_campaign_entities(
            LedgerKey::GRANT,
            delta.player,
            &delta.claimed,
            &delta.protected,
        );
    }

    /// 正規化の差分を正本へ確定し、その正本から自軍Squadのrosterを投影する。
    ///
    /// 1. 勝者の登録→旧戦術所属の解放→勝者の無いCampaign所属の具体Squad切離し（履歴化）。
    /// 2. 自軍Squadのrosterを、正本の現在binding（`squad_id`・role）と一致するEntityだけに絞る。
    ///    履歴のSquadIdは現在bindingではないため投影しない（旧memberを復活させない）。
    ///    owner/Factionが矛盾するforeign Entityは、どちらのplayerの正規化でも変更しない。
    pub(crate) fn commit(
        &mut self,
        delta: AssignmentDelta,
        manager: &mut SquadManager,
        player: PlayerId,
        all_live: &HashSet<Entity>,
        live: &HashSet<Entity>,
    ) {
        for (entity, assignment) in delta.assigned {
            self.registry
                .apply_assign(LedgerKey::GRANT, entity, assignment);
        }
        for entity in delta.released {
            self.registry.apply_release_entity(LedgerKey::GRANT, entity);
        }
        for entity in delta.detached {
            self.registry.apply_detach_squad(LedgerKey::GRANT, entity);
        }
        for _ in 0..delta.conflicts {
            self.registry.apply_rejected_conflict(LedgerKey::GRANT);
        }
        self.registry
            .apply_reconcile_visits(LedgerKey::GRANT, delta.visits);
        self.project_rosters(manager, player, all_live, live);
    }

    /// 自軍Squadのrosterを正本の現在bindingへ揃える（追加はしない。順序は維持する）。
    fn project_rosters(
        &self,
        manager: &mut SquadManager,
        player: PlayerId,
        all_live: &HashSet<Entity>,
        live: &HashSet<Entity>,
    ) {
        let registry = &*self.registry;
        let bound = |entity: &Entity, squad_id: SquadId, role: Option<OperationUnitRole>| {
            (all_live.contains(entity) && !live.contains(entity))
                || registry.assignment(*entity).is_some_and(|current| {
                    current.squad_id == Some(squad_id)
                        && role.is_none_or(|role| current.role == role)
                })
        };
        let ids = manager
            .proposal_view()
            .squads
            .iter()
            .filter(|squad| squad.plan.owner_id == Some(player))
            .map(|squad| squad.id)
            .collect::<Vec<_>>();
        let mut proposals = Self::proposals(manager);
        for id in ids {
            proposals.project_current(id, |entity, role| bound(entity, id, role));
        }
    }
}

/// 生産完了Entityを受け入れるSquad。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProducedSquad {
    /// 受付前から存在する、同じplayerのSquad。
    Existing(SquadId),
    /// 同じ受付の先行提案（index）が新設・選択したSquad。
    Proposed(usize),
    /// 指定任務で新設するSquad。
    Create(MissionType),
}

/// 生産slotの解決が提案する所属1件。新設時の目標・島・状態も同じ確定で適用する。
#[derive(Debug, Clone)]
pub(crate) struct ProducedJoin {
    pub player: PlayerId,
    pub entity: Entity,
    pub squad: ProducedSquad,
    /// Squadが欠けていた場合に新設する任務（slotの任務種別）。
    pub fallback_mission: MissionType,
    /// 新設する任務の進行状態。補充先の既存Squadには適用しない。
    pub initial_phase: MissionPhase,
    pub target: Option<GridPosition>,
    pub target_island: Option<IslandId>,
}

/// 1回の生産イベント受付で集めた所属提案。台帳ごとの全件を受付順に積む。
#[derive(Debug, Default)]
pub(crate) struct ProducedAssignmentProposal {
    joins: Vec<ProducedJoin>,
}

impl ProducedAssignmentProposal {
    /// 提案を追加し、確定後にSquadIdを引くためのindexを返す。
    pub(crate) fn propose(&mut self, join: ProducedJoin) -> usize {
        self.joins.push(join);
        self.joins.len() - 1
    }
}

/// 生産所属の確定結果。提案indexごとの受入Squad。
#[derive(Debug)]
pub(crate) struct ProducedAssignmentDelta {
    squads: Vec<SquadId>,
}

impl ProducedAssignmentDelta {
    pub(crate) fn squad_for(&self, index: usize) -> SquadId {
        self.squads[index]
    }
}

/// portfolio claimの1件。優先順に並べた列を [`CampaignClaimProposal`] へ渡す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CampaignClaim {
    pub owner: OperationOwner,
    pub entity: Entity,
    pub role: OperationUnitRole,
}

/// portfolio claimの提案。Defend→継続→Expand→Contest/Reinforce→Assault・島id順、
/// 作戦内はTransport→Capture→Combatの順で、最初のclaimだけが成立する。
pub(crate) struct CampaignClaimProposal {
    pub player: PlayerId,
    pub turn: u32,
    /// 今のportfolioに残るcampaign owner。これ以外の自軍campaign所属は先に解放する。
    pub active_owners: HashSet<OperationOwner>,
    pub claims: Vec<CampaignClaim>,
    /// 配備・首都作戦で実Squad接続まで保護するEntity。
    pub protected: HashSet<Entity>,
}

enum ClaimOp {
    Conflict,
    Assign(Entity, UnitOperationAssignment),
}

/// claimの解決結果。registryへの書込み列を従来と同じ順で保持する。
pub(crate) struct ClaimDelta {
    player: PlayerId,
    released_owners: Vec<OperationOwner>,
    ops: Vec<ClaimOp>,
    claimed: HashSet<Entity>,
    protected: HashSet<Entity>,
}

impl CampaignClaimProposal {
    /// registryを変更せずに解決する。終了作戦の解放後に見える旧所属で同owner判定を行う。
    pub(crate) fn resolve(self, registry: &UnitOperationRegistry) -> ClaimDelta {
        let released_owners = registry
            .campaign_owners(self.player)
            .into_iter()
            .filter(|owner| !self.active_owners.contains(owner))
            .collect::<Vec<_>>();
        let released = released_owners.iter().copied().collect::<HashSet<_>>();
        let mut claimed_entities: HashMap<Entity, OperationOwner> = HashMap::default();
        let mut ops = Vec::new();
        for claim in self.claims {
            if let Some(claimed_owner) = claimed_entities.get(&claim.entity) {
                if *claimed_owner != claim.owner {
                    ops.push(ClaimOp::Conflict);
                }
                continue;
            }
            let existing = registry
                .assignment(claim.entity)
                .filter(|current| !released.contains(&current.owner));
            if existing.is_some_and(|current| current.owner == claim.owner) {
                // 実Squadへ接続済みの割当をportfolio上の予約で弱めない。
                claimed_entities.insert(claim.entity, claim.owner);
                continue;
            }
            // portfolio再計画の先頭候補を明示的な移管先とする。旧campaignの逆引きは
            // 確定時のassignが同時に外すため、別途全作戦を走査しない。
            ops.push(ClaimOp::Assign(
                claim.entity,
                UnitOperationAssignment {
                    owner: claim.owner,
                    squad_id: None,
                    role: claim.role,
                    assigned_turn: self.turn,
                },
            ));
            claimed_entities.insert(claim.entity, claim.owner);
        }
        ClaimDelta {
            player: self.player,
            released_owners,
            ops,
            claimed: claimed_entities.keys().copied().collect(),
            protected: self.protected,
        }
    }
}

/// 同じresolveが選んだ勝者と解放対象。呼出し元が差分を書き換えないよう非公開にする。
pub(crate) struct AssignmentDelta {
    assigned: Vec<(Entity, UnitOperationAssignment)>,
    released: Vec<Entity>,
    /// 勝者の無いCampaign所属。作戦ownerは残し、具体Squad bindingを履歴へ移す。
    detached: Vec<Entity>,
    conflicts: usize,
    visits: usize,
}

/// 1回の正規化で観測した候補と旧所属。resolve中は台帳もSquadも変更しない。
pub(crate) struct AssignmentProposal {
    turn: u32,
    candidates: HashMap<Entity, Vec<SquadAssignmentCandidate>>,
    existing: HashMap<Entity, UnitOperationAssignment>,
    visits: usize,
}

impl AssignmentProposal {
    pub(crate) fn new(
        turn: u32,
        candidates: HashMap<Entity, Vec<SquadAssignmentCandidate>>,
        existing: HashMap<Entity, UnitOperationAssignment>,
        visits: usize,
    ) -> Self {
        Self {
            turn,
            candidates,
            existing,
            visits,
        }
    }

    /// 正規化前の実行roster（提案view）から、自軍Squadが主張する所属候補を集める。
    ///
    /// rosterは手番途中の書込みで1体が複数Squadに現れうる提案の集合であり、正本ではない。
    /// 正本（現在binding・履歴）は `existing` とresolve後の [`OperationLedger::commit`] が持ち、
    /// rosterは確定後に正本から投影し直す。所属を正本から読む処理はrosterを逆算しない。
    pub(crate) fn from_roster_view(
        turn: u32,
        manager: &mut SquadManager,
        player: PlayerId,
        live: &HashSet<Entity>,
        existing: HashMap<Entity, UnitOperationAssignment>,
        transported_by: &HashMap<Entity, Entity>,
        arrived_at_campaign: impl Fn(Entity, OperationOwner) -> bool,
    ) -> Self {
        let mut candidates: HashMap<Entity, Vec<SquadAssignmentCandidate>> = HashMap::default();
        let mut visits = 0usize;
        // 一覧内のrosterだけから、一覧順・役割順に候補を集める。
        for squad in manager
            .proposal_view()
            .squads
            .iter()
            .filter(|squad| squad.plan.owner_id == Some(player))
        {
            let owner = operation_owner_for_squad(squad, player);
            let mission_rank = assignment_mission_rank(&squad.plan.mission_type);
            let proposed = &**squad;
            for entity in proposed
                .members
                .iter()
                .filter(|entity| live.contains(entity))
            {
                visits = visits.saturating_add(1);
                let role = if proposed.transport_entity == Some(*entity) {
                    OperationUnitRole::Transport
                } else {
                    OperationUnitRole::Member
                };
                candidates
                    .entry(*entity)
                    .or_default()
                    .push(SquadAssignmentCandidate {
                        squad_id: squad.id,
                        owner,
                        role,
                        physically_loaded: false,
                        arrived_at_campaign: arrived_at_campaign(*entity, owner),
                        mission_rank,
                    });
            }
            for (entities, role) in [
                (&proposed.cargo_entities, OperationUnitRole::Cargo),
                (&proposed.delivered_cargo, OperationUnitRole::DeliveredCargo),
            ] {
                for entity in entities.iter().filter(|entity| live.contains(entity)) {
                    visits = visits.saturating_add(1);
                    let physically_loaded = proposed
                        .transport_entity
                        .is_some_and(|transport| transported_by.get(entity) == Some(&transport));
                    candidates
                        .entry(*entity)
                        .or_default()
                        .push(SquadAssignmentCandidate {
                            squad_id: squad.id,
                            owner,
                            role,
                            physically_loaded,
                            arrived_at_campaign: arrived_at_campaign(*entity, owner),
                            mission_rank,
                        });
                }
            }
        }
        Self::new(turn, candidates, existing, visits)
    }

    pub(crate) fn resolve(self) -> AssignmentDelta {
        let mut winners = HashMap::default();
        let mut conflicts = 0;
        for (entity, candidates) in &self.candidates {
            let prior = self.existing.get(entity).copied();
            let winner = candidates
                .iter()
                .filter_map(|candidate| {
                    assignment_candidate_key(candidate, prior).map(|key| (key, *candidate))
                })
                .min_by_key(|(key, _)| *key)
                .map(|(_, candidate)| candidate);
            if candidates.len() > 1 {
                conflicts += 1;
            }
            if let Some(winner) = winner {
                winners.insert(*entity, winner);
            }
        }
        // 元実装と同じ勝者HashMapの列挙順で登録する。候補の順に登録へ変えない。
        let assigned = winners
            .iter()
            .map(|(entity, winner)| {
                (
                    *entity,
                    UnitOperationAssignment {
                        owner: winner.owner,
                        squad_id: Some(winner.squad_id),
                        role: winner.role,
                        assigned_turn: self.turn,
                    },
                )
            })
            .collect();
        // 勝者の無い旧所属: 戦術Squad所属は解放し、Campaign所属は作戦ownerを残したまま
        // 具体Squad bindingだけを履歴へ移す（campaign間の移管はplannerが明示する）。
        let mut released = Vec::new();
        let mut detached = Vec::new();
        for (entity, assignment) in self.existing {
            if winners.contains_key(&entity) {
                continue;
            }
            if matches!(assignment.owner, OperationOwner::Campaign { .. }) {
                detached.push(entity);
            } else {
                released.push(entity);
            }
        }
        AssignmentDelta {
            assigned,
            released,
            detached,
            conflicts,
            visits: self.visits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::islands::IslandId;
    use crate::ai::planning::assignment::squad_model::{MissionType, SquadId};

    /// 正規化直後の不変条件: 自軍Squadのroster（foreign除く）は正本の現在bindingの写像と一致し、
    /// 現在bindingを持つ自軍Entityは必ずそのSquadのrosterに居る。履歴は写像に含まれない。
    fn assert_projection_matches_current(
        registry: &UnitOperationRegistry,
        manager: &SquadManager,
        player: PlayerId,
        foreign: &HashSet<Entity>,
    ) {
        let bound = registry.bound_entities();
        for squad in manager
            .squads
            .iter()
            .filter(|squad| squad.plan.owner_id == Some(player))
        {
            for entity in squad
                .members
                .iter()
                .chain(squad.cargo_entities.iter())
                .chain(squad.delivered_cargo.iter())
                .filter(|entity| !foreign.contains(entity))
            {
                assert_eq!(
                    registry.assignment(*entity).unwrap().squad_id,
                    Some(squad.id)
                );
                assert!(bound.contains(entity));
            }
        }
        for (entity, current) in registry.player_assignments(player) {
            if let Some(squad_id) = current.squad_id {
                let squad = manager
                    .squads
                    .iter()
                    .find(|squad| squad.id == squad_id)
                    .unwrap();
                assert!(
                    squad.members.contains(&entity)
                        || squad.cargo_entities.contains(&entity)
                        || squad.delivered_cargo.contains(&entity)
                );
            } else {
                assert!(!bound.contains(&entity));
            }
        }
    }

    fn candidate(id: u32, owner: OperationOwner) -> SquadAssignmentCandidate {
        SquadAssignmentCandidate {
            squad_id: SquadId(id),
            owner,
            role: OperationUnitRole::Member,
            physically_loaded: false,
            arrived_at_campaign: false,
            mission_rank: 2,
        }
    }

    fn assignment(candidate: SquadAssignmentCandidate) -> UnitOperationAssignment {
        UnitOperationAssignment {
            owner: candidate.owner,
            squad_id: Some(candidate.squad_id),
            role: candidate.role,
            assigned_turn: 1,
        }
    }

    #[test]
    fn resolve_keeps_loading_arrival_previous_owner_and_id_precedence() {
        let player_id = PlayerId(1);
        let owner = OperationOwner::Campaign {
            player_id,
            island_id: IslandId(0),
        };
        let previous = candidate(9, owner);
        let same_owner = candidate(4, owner);
        let mut arrived = candidate(
            3,
            OperationOwner::Campaign {
                player_id,
                island_id: IslandId(1),
            },
        );
        arrived.arrived_at_campaign = true;
        let mut loaded = candidate(12, arrived.owner);
        loaded.physically_loaded = true;
        loaded.role = OperationUnitRole::Cargo;
        let entity = Entity::from_raw(10);
        for (candidates, old, expected) in [
            (
                vec![previous, same_owner, arrived, loaded],
                Some(previous),
                loaded,
            ),
            (vec![previous, same_owner, arrived], Some(previous), arrived),
            (vec![same_owner, previous], Some(previous), previous),
            (vec![previous, same_owner], None, same_owner),
        ] {
            let old = old
                .map(|old| HashMap::from_iter([(entity, assignment(old))]))
                .unwrap_or_default();
            let proposal =
                AssignmentProposal::new(2, HashMap::from_iter([(entity, candidates)]), old, 4);
            let delta = proposal.resolve();
            assert_eq!(
                delta.assigned,
                vec![(
                    entity,
                    UnitOperationAssignment {
                        assigned_turn: 2,
                        ..assignment(expected)
                    }
                )]
            );
            assert!(delta.released.is_empty());
        }
    }

    #[test]
    fn rejected_campaign_move_preserves_old_owner_but_removes_stale_member() {
        let player = PlayerId(1);
        let entity = Entity::from_raw(10);
        let mut manager = SquadManager::default();
        let mut squad = manager.create_owned_squad(MissionType::Capture, player);
        squad.members.insert(entity);
        let id = squad.id.0;
        let old = candidate(
            id,
            OperationOwner::Campaign {
                player_id: player,
                island_id: IslandId(0),
            },
        );
        let rejected = candidate(
            id,
            OperationOwner::Campaign {
                player_id: player,
                island_id: IslandId(1),
            },
        );
        let mut registry = UnitOperationRegistry::default();
        registry.assign(entity, assignment(old));
        let existing = registry.player_assignments(player);
        let proposal = AssignmentProposal::new(
            2,
            HashMap::from_iter([(entity, vec![rejected])]),
            existing,
            1,
        );
        let delta = proposal.resolve();
        assert!(delta.assigned.is_empty());
        assert!(delta.released.is_empty());
        let live = HashSet::from_iter([entity]);
        OperationLedger::open(&mut registry).commit(delta, &mut manager, player, &live, &live);
        // S1: 作戦ownerは残すが、具体Squadは現在bindingから外して履歴へ分ける。
        assert_eq!(
            registry.assignment(entity),
            Some(UnitOperationAssignment {
                squad_id: None,
                ..assignment(old)
            })
        );
        assert_eq!(registry.previous_squad(entity), Some(SquadId(id)));
        assert!(manager.squads[0].members.is_empty());
        assert_projection_matches_current(&registry, &manager, player, &HashSet::default());

        // 次の正規化では、履歴のSquadIdが「前手番の具体Squad」の同点規則にだけ効く。
        let same_owner_other = candidate(id + 5, old.owner);
        let previous_again = old;
        let proposal = AssignmentProposal::new(
            3,
            HashMap::from_iter([(entity, vec![same_owner_other, previous_again])]),
            registry.resolution_priors(player),
            2,
        );
        let delta = proposal.resolve();
        assert_eq!(delta.assigned[0].1.squad_id, Some(SquadId(id)));
        manager.squads[0].members.insert(entity);
        OperationLedger::open(&mut registry).commit(delta, &mut manager, player, &live, &live);
        // 現在bindingへ戻ったら履歴は持ち越さない。
        assert_eq!(
            registry.assignment(entity).unwrap().squad_id,
            Some(SquadId(id))
        );
        assert_eq!(registry.previous_squad(entity), None);
        assert!(manager.squads[0].members.contains(&entity));
    }

    /// 解放・再割当で履歴は消え、別Entityの履歴には波及しない。
    #[test]
    fn previous_squad_history_is_cleared_by_release_and_assignment() {
        let player = PlayerId(1);
        let [kept, released, reassigned] = [1, 2, 3].map(Entity::from_raw);
        let owner = OperationOwner::Campaign {
            player_id: player,
            island_id: IslandId(0),
        };
        let mut registry = UnitOperationRegistry::default();
        for entity in [kept, released, reassigned] {
            registry.assign(entity, assignment(candidate(7, owner)));
            registry.apply_detach_squad(LedgerKey::fixture(), entity);
        }
        registry.apply_release_entity(LedgerKey::fixture(), released);
        registry.assign(reassigned, assignment(candidate(9, owner)));
        assert_eq!(registry.previous_squad(kept), Some(SquadId(7)));
        assert_eq!(registry.assignment(kept).unwrap().squad_id, None);
        assert_eq!(registry.previous_squad(released), None);
        assert_eq!(registry.previous_squad(reassigned), None);
        assert_eq!(
            registry.resolution_priors(player)[&kept].squad_id,
            Some(SquadId(7))
        );
    }

    #[test]
    fn commit_moves_references_releases_tactical_owner_and_preserves_foreign_squads() {
        let player = PlayerId(1);
        let entity = Entity::from_raw(10);
        let released = Entity::from_raw(11);
        let foreign = Entity::from_raw(12);
        let mut manager = SquadManager::default();
        let old_id = {
            let mut squad = manager.create_owned_squad(MissionType::Attack, player);
            squad.members.extend([entity, released, foreign]);
            squad.id
        };
        let new_id = manager.create_owned_squad(MissionType::Capture, player).id;
        manager.squads[1].members.insert(entity);
        manager
            .create_owned_squad(MissionType::Defense, PlayerId(2))
            .members
            .insert(foreign);
        let old = candidate(
            old_id.0,
            OperationOwner::TacticalSquad {
                player_id: player,
                squad_id: old_id,
            },
        );
        let mut new = candidate(
            new_id.0,
            OperationOwner::Campaign {
                player_id: player,
                island_id: IslandId(0),
            },
        );
        // 到着事実を伴う正式handoffだけが旧具体Squadを上回る。
        new.arrived_at_campaign = true;
        let mut registry = UnitOperationRegistry::default();
        registry.assign(entity, assignment(old));
        registry.assign(released, assignment(old));
        let existing = registry.player_assignments(player);
        let proposal = AssignmentProposal::new(
            2,
            HashMap::from_iter([(entity, vec![old, new])]),
            existing,
            2,
        );
        let delta = proposal.resolve();
        assert_eq!(delta.released, vec![released]);
        // resolveの間に台帳は変化しない。
        assert_eq!(registry.assignment(entity), Some(assignment(old)));
        let live = HashSet::from_iter([entity, released]);
        let all = HashSet::from_iter([entity, released, foreign]);
        OperationLedger::open(&mut registry).commit(delta, &mut manager, player, &all, &live);
        assert_eq!(
            manager.squads[0]
                .members
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![foreign]
        );
        assert_eq!(
            manager.squads[1]
                .members
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![entity]
        );
        assert!(manager.squads[2].members.contains(&foreign));
        assert!(registry.assignment(released).is_none());
        assert_eq!(registry.assignment(entity).unwrap().squad_id, Some(new_id));
        assert_eq!(registry.rejected_conflicts(), 1);
        assert_eq!(registry.last_reconcile_visits(), 2);
        assert_projection_matches_current(
            &registry,
            &manager,
            player,
            &HashSet::from_iter([foreign]),
        );
    }

    /// C1（S1以前）のwinner表による投影の参照実装。
    fn legacy_winner_projection(
        manager: &mut SquadManager,
        winners: &HashMap<Entity, UnitOperationAssignment>,
        player: PlayerId,
        all_live: &HashSet<Entity>,
        live: &HashSet<Entity>,
    ) {
        for squad in manager
            .squads
            .iter_mut()
            .filter(|squad| squad.plan.owner_id == Some(player))
        {
            let id = squad.id;
            let foreign = |entity: &Entity| all_live.contains(entity) && !live.contains(entity);
            squad.members.retain(|entity| {
                foreign(entity) || winners.get(entity).is_some_and(|w| w.squad_id == Some(id))
            });
            for (entities, role) in [
                (&mut *squad.roster.cargo_entities, OperationUnitRole::Cargo),
                (
                    &mut *squad.roster.delivered_cargo,
                    OperationUnitRole::DeliveredCargo,
                ),
            ] {
                entities.retain(|entity| {
                    foreign(entity)
                        || winners
                            .get(entity)
                            .is_some_and(|w| w.squad_id == Some(id) && w.role == role)
                });
            }
            if squad
                .transport_entity
                .is_some_and(|transport| !squad.members.contains(&transport))
            {
                *squad.transport_entity = None;
            }
        }
    }

    /// 旧winner表による投影と、S1の正本現在bindingからの投影は、Cargo/Delivered/Transport・
    /// foreign Entity・ownerなしSquad・勝者無しCampaign（履歴化）を含めて同じrosterになる。
    #[test]
    fn current_binding_projection_matches_legacy_winner_projection() {
        let player = PlayerId(1);
        let other = PlayerId(2);
        let campaign = |island| OperationOwner::Campaign {
            player_id: player,
            island_id: IslandId(island),
        };
        let e = (0..12).map(Entity::from_raw).collect::<Vec<_>>();
        let mut manager = SquadManager::default();
        // 0: 輸送Squad（輸送役e0、積荷e1・e2、降車済みe3、foreign e10）
        let mut transport = manager.create_owned_squad(MissionType::Transport, player);
        transport.plan.target_island = Some(IslandId(1));
        transport.members.extend([e[0], e[10]]);
        *transport.transport_entity = Some(e[0]);
        transport.cargo_entities.extend([e[1], e[2]]);
        transport.delivered_cargo.push(e[3]);
        // 1: 現地Capture（e1を物理搭載外で重複主張、e3も主張、e4）
        let mut capture = manager.create_owned_squad(MissionType::Capture, player);
        capture.plan.target_island = Some(IslandId(1));
        capture.members.extend([e[1], e[3], e[4]]);
        // 2: 戦術Attack（e5、e6は旧Campaign 2から移管を主張→拒否される）
        let mut attack = manager.create_owned_squad(MissionType::Attack, player);
        attack.members.extend([e[5], e[6]]);
        // 3: 別Campaign 2の輸送（e7を輸送役に持つが、e0と同じ輸送役をrosterで主張しない）
        let mut other_transport = manager.create_owned_squad(MissionType::Transport, player);
        other_transport.plan.target_island = Some(IslandId(2));
        other_transport.members.insert(e[7]);
        *other_transport.transport_entity = Some(e[7]);
        // 4: ownerなしSquad（自軍e8）と、5: 他playerのSquad（自軍e9）
        manager
            .create_squad(MissionType::Defense)
            .members
            .insert(e[8]);
        manager
            .create_owned_squad(MissionType::Defense, other)
            .members
            .insert(e[9]);
        let ids = manager
            .squads
            .iter()
            .map(|squad| squad.id)
            .collect::<Vec<_>>();

        let mut registry = UnitOperationRegistry::default();
        for (entity, owner, squad, role) in [
            (e[6], campaign(2), Some(ids[3]), OperationUnitRole::Member),
            (
                e[5],
                OperationOwner::TacticalSquad {
                    player_id: player,
                    squad_id: ids[2],
                },
                Some(ids[2]),
                OperationUnitRole::Member,
            ),
            (e[11], campaign(2), Some(ids[3]), OperationUnitRole::Member),
        ] {
            registry.assign(
                entity,
                UnitOperationAssignment {
                    owner,
                    squad_id: squad,
                    role,
                    assigned_turn: 1,
                },
            );
        }
        let live = e[..10]
            .iter()
            .copied()
            .chain([e[11]])
            .collect::<HashSet<_>>();
        let all_live = live.iter().copied().chain([e[10]]).collect::<HashSet<_>>();
        // e1はe0に物理搭載済み。e3・e4は目的島へ到着済み。
        let transported_by = HashMap::from_iter([(e[1], e[0])]);
        let arrived = |entity: Entity, owner: OperationOwner| {
            [e[3], e[4]].contains(&entity) && owner == campaign(1)
        };
        let proposal = AssignmentProposal::from_roster_view(
            2,
            &mut manager,
            player,
            &live,
            registry.resolution_priors(player),
            &transported_by,
            arrived,
        );
        let delta = proposal.resolve();
        let winners = delta.assigned.iter().copied().collect::<HashMap<_, _>>();

        let mut legacy = manager.clone();
        legacy_winner_projection(&mut legacy, &winners, player, &all_live, &live);
        OperationLedger::open(&mut registry).commit(delta, &mut manager, player, &all_live, &live);

        for (current, old) in manager.squads.iter().zip(&legacy.squads) {
            assert_eq!(current.id, old.id);
            assert_eq!(current.members, *old.members, "{:?}", current.id);
            assert_eq!(
                current.transport_entity, *old.transport_entity,
                "{:?}",
                current.id
            );
            assert_eq!(
                current.cargo_entities, *old.cargo_entities,
                "{:?}",
                current.id
            );
            assert_eq!(
                current.delivered_cargo, *old.delivered_cargo,
                "{:?}",
                current.id
            );
        }
        // 移管を拒否されたe6・Squadを持たないe11は作戦ownerを残して履歴化する。
        assert_eq!(registry.assignment(e[6]).unwrap().squad_id, None);
        assert_eq!(registry.previous_squad(e[6]), Some(ids[3]));
        assert_eq!(registry.assignment(e[11]).unwrap().squad_id, None);
        assert_eq!(registry.previous_squad(e[11]), Some(ids[3]));
        // ownerなし・他playerのSquadは変更しない。foreign e10も輸送Squadに残る。
        assert!(manager.squads[4].members.contains(&e[8]));
        assert!(manager.squads[5].members.contains(&e[9]));
        assert!(manager.squads[0].members.contains(&e[10]));
        assert_projection_matches_current(
            &registry,
            &manager,
            player,
            &HashSet::from_iter([e[10]]),
        );
    }

    #[test]
    fn commit_does_not_project_into_ownerless_or_foreign_owned_squads() {
        // owner_id=NoneのSquadとほかのplayerのSquadは、このplayerの正規化で変更しない。
        let player = PlayerId(1);
        let entity = Entity::from_raw(10);
        let cargo = Entity::from_raw(11);
        let mut manager = SquadManager::default();
        let ownerless = {
            let mut squad = manager.create_squad(MissionType::Transport);
            squad.members.insert(entity);
            squad.cargo_entities.push(cargo);
            *squad.transport_entity = Some(entity);
            squad.id
        };
        manager
            .create_owned_squad(MissionType::Attack, PlayerId(2))
            .members
            .insert(entity);
        let mut registry = UnitOperationRegistry::default();
        let proposal = AssignmentProposal::new(1, HashMap::default(), HashMap::default(), 0);
        let live = HashSet::from_iter([entity, cargo]);
        OperationLedger::open(&mut registry).commit(
            proposal.resolve(),
            &mut manager,
            player,
            &live,
            &live,
        );
        assert_eq!(manager.squads[0].id, ownerless);
        assert!(manager.squads[0].members.contains(&entity));
        assert_eq!(*manager.squads[0].cargo_entities, vec![cargo]);
        assert_eq!(manager.squads[0].transport_entity, Some(entity));
        assert!(manager.squads[1].members.contains(&entity));
        assert!(registry.assignment(entity).is_none());
    }

    fn campaign(player_id: PlayerId, island: usize) -> OperationOwner {
        OperationOwner::Campaign {
            player_id,
            island_id: IslandId(island),
        }
    }

    /// 段階3以前のclaim実装をそのまま再現した参照実装（R比較用）。
    fn legacy_claim(
        registry: &mut UnitOperationRegistry,
        player_id: PlayerId,
        turn: u32,
        active_owners: &HashSet<OperationOwner>,
        claims: &[CampaignClaim],
        protected: &HashSet<Entity>,
    ) {
        for owner in registry.campaign_owners(player_id) {
            if !active_owners.contains(&owner) {
                registry.release_operation(owner);
            }
        }
        let mut claimed_entities = HashMap::default();
        for claim in claims {
            if let Some(claimed_owner) = claimed_entities.get(&claim.entity) {
                if *claimed_owner != claim.owner {
                    registry.apply_rejected_conflict(LedgerKey::fixture());
                }
                continue;
            }
            if let Some(current) = registry.assignment(claim.entity)
                && current.owner == claim.owner
            {
                claimed_entities.insert(claim.entity, claim.owner);
                continue;
            }
            registry.assign(
                claim.entity,
                UnitOperationAssignment {
                    owner: claim.owner,
                    squad_id: None,
                    role: claim.role,
                    assigned_turn: turn,
                },
            );
            claimed_entities.insert(claim.entity, claim.owner);
        }
        let claimed = claimed_entities.keys().copied().collect::<HashSet<_>>();
        registry.release_unclaimed_campaign_entities(player_id, &claimed, protected);
    }

    #[test]
    fn claim_proposal_matches_legacy_first_claim_wins_and_release_order() {
        let player = PlayerId(1);
        let other = PlayerId(2);
        let tactical = OperationOwner::TacticalSquad {
            player_id: player,
            squad_id: SquadId(7),
        };
        let entities = (0..12).map(Entity::from_raw).collect::<Vec<_>>();
        let seed = |registry: &mut UnitOperationRegistry| {
            // 継続中campaign（具体Squad接続済み）、終了campaign、戦術Squad、他player。
            for (index, owner, squad) in [
                (0, campaign(player, 1), Some(SquadId(3))),
                (1, campaign(player, 1), Some(SquadId(3))),
                (2, campaign(player, 9), Some(SquadId(4))),
                (3, campaign(player, 9), None),
                (4, tactical, Some(SquadId(7))),
                (5, campaign(other, 1), None),
                (6, campaign(player, 2), None),
                (7, campaign(player, 2), None),
            ] {
                registry.assign(
                    entities[index],
                    UnitOperationAssignment {
                        owner,
                        squad_id: squad,
                        role: OperationUnitRole::Member,
                        assigned_turn: 1,
                    },
                );
            }
        };
        let claims = [
            (campaign(player, 1), 0, OperationUnitRole::Member),
            (campaign(player, 1), 2, OperationUnitRole::Transport),
            (campaign(player, 1), 4, OperationUnitRole::Cargo),
            (campaign(player, 2), 0, OperationUnitRole::Member),
            (campaign(player, 2), 2, OperationUnitRole::Member),
            (campaign(player, 2), 6, OperationUnitRole::Member),
            (campaign(player, 2), 8, OperationUnitRole::Cargo),
            (campaign(player, 1), 8, OperationUnitRole::Member),
        ]
        .map(|(owner, index, role)| CampaignClaim {
            owner,
            entity: entities[index],
            role,
        });
        let active = HashSet::from_iter([campaign(player, 1), campaign(player, 2)]);
        // entity1は今のportfolioに無いが保護、entity7は保護も無いので解放される。
        let protected = HashSet::from_iter([entities[1]]);

        let mut legacy = UnitOperationRegistry::default();
        seed(&mut legacy);
        legacy_claim(&mut legacy, player, 5, &active, &claims, &protected);

        let mut registry = UnitOperationRegistry::default();
        seed(&mut registry);
        let delta = CampaignClaimProposal {
            player,
            turn: 5,
            active_owners: active,
            claims: claims.to_vec(),
            protected,
        }
        .resolve(&registry);
        // resolveの間は台帳を変更しない。
        assert_eq!(
            registry.assignment(entities[2]).unwrap().owner,
            campaign(player, 9)
        );
        OperationLedger::open(&mut registry).commit_claims(delta);

        for player_id in [player, other] {
            assert_eq!(
                registry.player_assignments(player_id),
                legacy.player_assignments(player_id)
            );
        }
        assert_eq!(registry.rejected_conflicts(), legacy.rejected_conflicts());
        assert_eq!(registry.rejected_conflicts(), 3);
        // 接続済み同ownerは具体Squadを弱めない。
        assert_eq!(
            registry.assignment(entities[0]).unwrap().squad_id,
            Some(SquadId(3))
        );
        // 終了campaignは先に解放され、先頭claimへ移管される。
        assert_eq!(
            registry.assignment(entities[2]).unwrap().owner,
            campaign(player, 1)
        );
        assert!(registry.assignment(entities[1]).is_some());
        assert!(registry.assignment(entities[3]).is_none());
        assert!(registry.assignment(entities[7]).is_none());
        assert_eq!(
            registry.assignment(entities[5]).unwrap().owner,
            campaign(other, 1)
        );
    }

    #[test]
    fn roster_release_and_transport_wave_match_previous_direct_writes() {
        let player = PlayerId(1);
        let [transport, cargo_a, cargo_b, member] = [1, 2, 3, 4].map(Entity::from_raw);
        let mut manager = SquadManager::default();
        let mut squad = manager.create_owned_squad(MissionType::Transport, player);
        squad.members.extend([transport, member]);
        *squad.transport_entity = Some(transport);
        squad.cargo_entities.extend([cargo_b, cargo_a, cargo_b]);
        squad.delivered_cargo.push(cargo_a);

        OperationLedger::roster(&mut squad).release_where(|entity| *entity == cargo_a);
        assert_eq!(*squad.cargo_entities, vec![cargo_b, cargo_b]);
        assert!(squad.delivered_cargo.is_empty());
        assert_eq!(squad.transport_entity, Some(transport));

        let mut roster = OperationLedger::roster(&mut squad);
        roster.sort_dedup_cargo();
        roster.release_where(|entity| *entity == transport);
        assert_eq!(squad.transport_entity, None);
        assert_eq!(*squad.cargo_entities, vec![cargo_b]);
        assert_eq!(
            squad.members.iter().copied().collect::<Vec<_>>(),
            vec![member]
        );

        OperationLedger::roster(&mut squad).rebind_transport_wave(transport, vec![cargo_a]);
        assert_eq!(
            squad.members.iter().copied().collect::<Vec<_>>(),
            vec![transport]
        );
        assert_eq!(squad.transport_entity, Some(transport));
        assert_eq!(*squad.cargo_entities, vec![cargo_a]);
    }

    /// 同じ生産Entityを2つのSquadが提案でき、正規化で現在所属だけへ絞られる。
    #[test]
    fn produced_rosters_are_candidates_until_normalization() {
        let player = PlayerId(1);
        let [entity, legacy] = [Entity::from_raw(10), Entity::from_raw(11)];
        let mut manager = SquadManager::default();
        let existing = manager.create_owned_squad(MissionType::Defense, player).id;
        let mut proposal = ProducedAssignmentProposal::default();
        let first = proposal.propose(ProducedJoin {
            player,
            entity,
            squad: ProducedSquad::Existing(existing),
            fallback_mission: MissionType::Defense,
            initial_phase: MissionPhase::Forming,
            target: None,
            target_island: None,
        });
        let second = proposal.propose(ProducedJoin {
            player,
            entity,
            squad: ProducedSquad::Create(MissionType::Capture),
            fallback_mission: MissionType::Capture,
            initial_phase: MissionPhase::Forming,
            target: None,
            target_island: Some(IslandId(0)),
        });
        let delta = OperationLedger::commit_produced(&mut manager, proposal);
        let created = delta.squad_for(second);
        assert_eq!(delta.squad_for(first), existing);
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|s| s.members.contains(&entity))
                .count(),
            2
        );
        // draftと同じroster更新口の変更が候補収集へ直接見える。
        OperationLedger::roster(&mut manager.edit_each().next().unwrap()).join(legacy);
        let live = HashSet::from_iter([entity, legacy]);
        let proposal = AssignmentProposal::from_roster_view(
            2,
            &mut manager,
            player,
            &live,
            HashMap::default(),
            &HashMap::default(),
            |_, _| false,
        );
        assert_eq!(proposal.candidates[&entity].len(), 2);
        assert_eq!(proposal.candidates[&legacy][0].squad_id, existing);
        let mut registry = UnitOperationRegistry::default();
        OperationLedger::open(&mut registry).commit(
            proposal.resolve(),
            &mut manager,
            player,
            &live,
            &live,
        );
        // 正規化後のrosterには現在bindingだけが残る。
        let winner = registry.assignment(entity).unwrap().squad_id.unwrap();
        let loser = if winner == existing {
            created
        } else {
            existing
        };
        let winners = manager
            .squads
            .iter()
            .filter(|s| s.members.contains(&entity))
            .map(|s| s.id)
            .collect::<Vec<_>>();
        assert_eq!(winners, vec![winner]);
        assert!(
            !manager
                .squads
                .iter()
                .find(|s| s.id == loser)
                .unwrap()
                .members
                .contains(&entity)
        );
    }

    /// 生産所属の確定時に検証済みのExisting/Proposedが欠けるのは不変条件違反であり、
    /// debug buildではpanicで検知する。release buildでは所属を捨てずslotの任務で新設する。
    #[test]
    fn produced_commit_detects_missing_verified_squad() {
        let player = PlayerId(1);
        let entity = Entity::from_raw(10);
        let mut proposal = ProducedAssignmentProposal::default();
        proposal.propose(ProducedJoin {
            player,
            entity,
            squad: ProducedSquad::Existing(SquadId(99)),
            fallback_mission: MissionType::Capture,
            initial_phase: MissionPhase::Forming,
            target: None,
            target_island: None,
        });
        let mut manager = SquadManager::default();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            OperationLedger::commit_produced(&mut manager, proposal)
        }));
        if cfg!(debug_assertions) {
            assert!(result.is_err(), "debug buildでは不変条件違反を検知する");
        } else {
            let delta = result.expect("release buildでは所属を捨てない");
            let squad = &manager.squads[0];
            assert_eq!(delta.squad_for(0), squad.id);
            assert_eq!(squad.plan.mission_type, MissionType::Capture);
            assert_eq!(squad.plan.owner_id, Some(player));
            assert!(squad.members.contains(&entity));
        }
    }
}
