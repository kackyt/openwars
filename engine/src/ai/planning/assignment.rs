//! 所属の提案・競合解決・差分確定。
//!
//! 所属台帳（OperationLedger）は、次の正本と読み出し用の投影を区別する。
//!
//! - 現在binding＋履歴: [`UnitOperationRegistry`]。Entityごとの唯一の作戦ownerと現在の
//!   `squad_id` を持つ。勝者の無いCampaignはownerを残して `squad_id=None` とし、過去の
//!   SquadIdを `previous_squad` へ分離する。履歴は同点規則だけが読み、行動へ復活させない。
//! - 所属提案: managerが持つ `ProposalStore`。SquadId→roster、Entity→複数候補の索引を
//!   keyed確定口で更新する。編成中の読み手は `proposal_view()` とstoreを明示して読む。
//!   [`AssignmentProposal::from_roster_view`] はstoreから候補を集め、純粋なresolveがdeltaを作る。
//! - Squadの所属field: storeの読取り互換view。提案commit時は複数候補を含みうる。
//!   [`OperationLedger::commit`] は正規化所属を確定してから、その現在bindingだけへ投影する。
//!   一意な実所属を照会する読み手はregistryを使い、途中viewを実所属と見なさない。
//!
//! 本番ではSquadのrosterからstoreを逆算する同期を行わない。構造変更時の明示的な候補登録と、
//! `ProposalCommit::edit` の作業値だけがroster adapterを使う。直接書込みを許すテストfixtureは
//! test専用の同期で取り込む。
//!
//! 境界（何を型で保証し、何を保証しないか）:
//!
//! - 本番buildで所属fieldへの代入・swap・take・集合変更は外部から行えない。
//!   fieldの実装はroster moduleに閉じ、提案store更新口と内部draft adapterが投影を更新する。
//!   Squad一覧fieldも外部から置換できず、registryの書込みは `LedgerKey` を要求する。
//! - crate内の `squads_mut()` は一般の可変sliceであり、sort/swap/丸ごとの置換は型だけでは
//!   防げない。一覧変更は `SquadListCommit` を使い、典型形の直接置換をsource監査で検出する。
//!   監査はRust parserではなく近似である。確定口のowner対応はcallerの責務であり、
//!   World Resourceの全置換などECS一般の特権を閉じるセキュリティ境界ではない。

use crate::ai::islands::IslandId;
use crate::ai::operation_assignment::{
    OperationOwner, OperationUnitRole, UnitOperationAssignment, UnitOperationRegistry,
};
use crate::ai::squad::{
    MissionPhase, MissionType, SquadAssignmentCandidate, SquadId, assignment_candidate_key,
    assignment_mission_rank, operation_owner_for_squad,
};
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, PlayerId};
use bevy_ecs::prelude::*;
use std::collections::BTreeSet;

mod roster;
pub use roster::{
    CargoRoster, MemberRoster, Squad, SquadList, SquadManager, SquadRoster, TransportBinding,
};
pub(crate) use roster::{ProposalCommit, RosterCommit, SquadListCommit};

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

    /// SquadIdで指す所属提案の確定口を開く（提案storeとSquad側rosterへ同時に適用する）。
    pub(crate) fn proposals(manager: &mut SquadManager) -> ProposalCommit<'_> {
        manager.proposal_commit()
    }

    /// Squad一覧の構造変更（解散・退避・復元・手動発番）の確定口を開く。
    pub(crate) fn squad_list(manager: &mut SquadManager) -> SquadListCommit<'_> {
        manager.list_commit()
    }

    /// keyed提案トランザクションまたは一覧外draftのroster adapterを開く。
    pub(crate) fn roster(squad: &mut Squad) -> RosterCommit<'_> {
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
                    .is_some_and(|squad| squad.owner_id == Some(join.player))
            });
            let empty = || (BTreeSet::new(), None, Vec::new());
            let squad_id = match (existing, join.squad) {
                (Some(id), _) => {
                    // 既存Squadへの所属提案は、提案storeとSquad側rosterへ同時に適用する。
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
            let squad = proposals.squad_mut(squad_id).expect("受入Squad");
            squad.target = join.target;
            squad.target_island = join.target_island;
            squad.phase = MissionPhase::Forming;
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
            .filter(|squad| squad.owner_id == Some(player))
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

/// 生産slotの解決が提案する所属1件。目標・島・Forming化も同じ確定で適用する。
#[derive(Debug, Clone)]
pub(crate) struct ProducedJoin {
    pub player: PlayerId,
    pub entity: Entity,
    pub squad: ProducedSquad,
    /// Squadが欠けていた場合に新設する任務（slotの任務種別）。
    pub fallback_mission: MissionType,
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
        // 提案の正本を読む。テストfixture以外にSquad→storeの逆算経路は無い。
        #[cfg(test)]
        manager.sync_proposals();
        let store = manager.synced_proposals();
        for squad in manager
            .proposal_view()
            .squads
            .iter()
            .filter(|squad| squad.owner_id == Some(player))
        {
            let owner = operation_owner_for_squad(squad, player);
            let mission_rank = assignment_mission_rank(&squad.mission_type);
            let proposed = store.proposed(squad.id).expect("同期済みの提案");
            for entity in proposed
                .members
                .iter()
                .filter(|entity| live.contains(entity))
            {
                visits = visits.saturating_add(1);
                let role = if proposed.transport == Some(*entity) {
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
                (&proposed.cargo, OperationUnitRole::Cargo),
                (&proposed.delivered, OperationUnitRole::DeliveredCargo),
            ] {
                for entity in entities.iter().filter(|entity| live.contains(entity)) {
                    visits = visits.saturating_add(1);
                    let physically_loaded = proposed
                        .transport
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
    use crate::ai::squad::{MissionType, SquadId};

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
            .filter(|squad| squad.owner_id == Some(player))
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
        let squad = manager.create_owned_squad(MissionType::Capture, player);
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
            let squad = manager.create_owned_squad(MissionType::Attack, player);
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
            .filter(|squad| squad.owner_id == Some(player))
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
        let transport = manager.create_owned_squad(MissionType::Transport, player);
        transport.target_island = Some(IslandId(1));
        transport.members.extend([e[0], e[10]]);
        *transport.transport_entity = Some(e[0]);
        transport.cargo_entities.extend([e[1], e[2]]);
        transport.delivered_cargo.push(e[3]);
        // 1: 現地Capture（e1を物理搭載外で重複主張、e3も主張、e4）
        let capture = manager.create_owned_squad(MissionType::Capture, player);
        capture.target_island = Some(IslandId(1));
        capture.members.extend([e[1], e[3], e[4]]);
        // 2: 戦術Attack（e5、e6は旧Campaign 2から移管を主張→拒否される）
        let attack = manager.create_owned_squad(MissionType::Attack, player);
        attack.members.extend([e[5], e[6]]);
        // 3: 別Campaign 2の輸送（e7を輸送役に持つが、e0と同じ輸送役をrosterで主張しない）
        let other_transport = manager.create_owned_squad(MissionType::Transport, player);
        other_transport.target_island = Some(IslandId(2));
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
            let squad = manager.create_squad(MissionType::Transport);
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
        let squad = manager.create_owned_squad(MissionType::Transport, player);
        squad.members.extend([transport, member]);
        *squad.transport_entity = Some(transport);
        squad.cargo_entities.extend([cargo_b, cargo_a, cargo_b]);
        squad.delivered_cargo.push(cargo_a);

        OperationLedger::roster(squad).release_where(|entity| *entity == cargo_a);
        assert_eq!(*squad.cargo_entities, vec![cargo_b, cargo_b]);
        assert!(squad.delivered_cargo.is_empty());
        assert_eq!(squad.transport_entity, Some(transport));

        let mut roster = OperationLedger::roster(squad);
        roster.sort_dedup_cargo();
        roster.release_where(|entity| *entity == transport);
        assert_eq!(squad.transport_entity, None);
        assert_eq!(*squad.cargo_entities, vec![cargo_b]);
        assert_eq!(
            squad.members.iter().copied().collect::<Vec<_>>(),
            vec![member]
        );

        OperationLedger::roster(squad).rebind_transport_wave(transport, vec![cargo_a]);
        assert_eq!(
            squad.members.iter().copied().collect::<Vec<_>>(),
            vec![transport]
        );
        assert_eq!(squad.transport_entity, Some(transport));
        assert_eq!(*squad.cargo_entities, vec![cargo_a]);
    }

    /// `&mut Squad` 経由のSquad丸ごと置換は型で防げないため、`engine/src/ai` 配下の本番codeに
    /// 典型形の直接置換が無いことをsource走査で固定する（型保証ではない）。
    ///
    /// - 対象: `*_tests.rs`・`*_tests/`・`tests.rs` を除く `engine/src/ai` 配下の全 `.rs`。
    ///   各fileから `#[cfg(test)]` 直後のitem（`mod tests {..}` やtest用fn/impl）だけを
    ///   括弧対応で取り除き、それ以降の本番codeも検査する（v4/extended.rsのようにtest module
    ///   の後に本番codeが続くfileの盲点を塞ぐ）。文字列・文字・comment内の括弧は読み飛ばす。
    ///   raw文字列（`r#"..."#`）と `#[cfg(all(test, ..))]` 等の複合条件は扱わない（近似）。
    ///   CRLFはLFへ正規化してから処理する。
    ///   行commentを除き、空白・改行を詰めてから検査するので改行を跨ぐ式も検出する。
    /// - 検出: `squads_mut()[<index>] =`（`==` は除く）と、識別子に続かない `*squad =`。
    /// - 検出できない: 別名の可変参照経由の置換、`slice.swap/sort/reverse` 等の一般操作。
    #[test]
    fn production_code_does_not_replace_whole_squads_directly() {
        fn production_sources(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            let mut entries = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            entries.sort();
            for path in entries {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                if path.is_dir() {
                    if !name.ends_with("_tests") {
                        production_sources(&path, out);
                    }
                } else if name.ends_with(".rs")
                    && !name.ends_with("_tests.rs")
                    && name != "tests.rs"
                {
                    let source = std::fs::read_to_string(&path).unwrap();
                    out.push((path.display().to_string(), strip_test_items(&source)));
                }
            }
        }

        /// `pos` から始まるitemの終端（`;` か対応する `}` の直後）を返す。
        /// 文字列・文字literal・commentの中の括弧は数えない。
        fn item_end(source: &str, pos: usize) -> usize {
            let bytes = source.as_bytes();
            let mut index = pos;
            let mut depth = 0usize;
            let mut parens = 0usize;
            let mut brackets = 0usize;
            while index < bytes.len() {
                match bytes[index] {
                    b'/' if bytes.get(index + 1) == Some(&b'/') => {
                        while index < bytes.len() && bytes[index] != b'\n' {
                            index += 1;
                        }
                    }
                    b'/' if bytes.get(index + 1) == Some(&b'*') => {
                        index += 2;
                        while index + 1 < bytes.len()
                            && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                        {
                            index += 1;
                        }
                        index += 1;
                    }
                    b'"' => {
                        index += 1;
                        while index < bytes.len() && bytes[index] != b'"' {
                            if bytes[index] == b'\\' {
                                index += 1;
                            }
                            index += 1;
                        }
                    }
                    // 文字literal（'{' や '\''）。lifetime（'a）は閉じ引用符が無いので読み飛ばさない。
                    b'\'' if bytes.get(index + 2) == Some(&b'\'') => index += 2,
                    b'\''
                        if bytes.get(index + 1) == Some(&b'\\')
                            && bytes.get(index + 3) == Some(&b'\'') =>
                    {
                        index += 3
                    }
                    // cfg(test)はitemだけでなくfield・initializerにも付く。
                    // 関数の引数や配列の中のcommaでは区切らない。
                    b'(' => parens += 1,
                    b')' => parens = parens.saturating_sub(1),
                    b'[' => brackets += 1,
                    b']' => brackets = brackets.saturating_sub(1),
                    b';' | b',' if depth == 0 && parens == 0 && brackets == 0 => return index + 1,
                    b'{' => depth += 1,
                    b'}' => {
                        assert!(
                            depth > 0,
                            "cfg(test)の終端を判定できない: {}",
                            &source[pos..index]
                        );
                        depth -= 1;
                        if depth == 0 {
                            return index + 1;
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            bytes.len()
        }

        /// `#[cfg(test)]` 直後のitemを取り除いた本番source。
        fn strip_test_items(source: &str) -> String {
            let marker = "#[cfg(test)]";
            // CRLFのfileでもcomment終端と改行跨ぎの検出を同じにするため、LFへ正規化する。
            let source = source.replace("\r\n", "\n");
            let mut out = String::with_capacity(source.len());
            let mut rest = source.as_str();
            while let Some(start) = rest.find(marker) {
                out.push_str(&rest[..start]);
                let after = start + marker.len();
                rest = &rest[item_end(rest, after)..];
            }
            out.push_str(rest);
            out
        }

        /// 行comment（文字列中の `//` は近似で扱う）を除き、空白を詰める。
        fn compact(source: &str) -> String {
            source
                .lines()
                .map(|line| line.split("//").next().unwrap_or(""))
                .collect::<String>()
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect()
        }

        fn indexed_assignments(code: &str) -> usize {
            let needle = "squads_mut()[";
            let mut found = 0;
            let mut rest = code;
            while let Some(start) = rest.find(needle) {
                let after = &rest[start + needle.len()..];
                let mut depth = 1;
                let mut close = None;
                for (offset, ch) in after.char_indices() {
                    match ch {
                        '[' => depth += 1,
                        ']' => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some(offset);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(close) = close {
                    let tail = &after[close + 1..];
                    if tail.starts_with('=') && !tail.starts_with("==") {
                        found += 1;
                    }
                }
                rest = after;
            }
            found
        }

        fn deref_assignments(code: &str) -> usize {
            code.match_indices("*squad=")
                .filter(|(index, _)| {
                    let before = code[..*index].chars().next_back();
                    let after = &code[index + "*squad=".len()..];
                    !after.starts_with('=')
                        && before.is_none_or(|ch| !(ch.is_alphanumeric() || ch == '_' || ch == ')'))
                })
                .count()
        }

        // test item除去の契約: test moduleの後ろの本番codeは残し、文字列中の括弧で誤らない。
        let stripped = strip_test_items(
            "fn a() {}\n#[cfg(test)]\nmod tests {\n fn t() { let s = \"}\"; let c = '{'; }\n}\nfn after() { x.squads_mut()[0] = y; }\n#[cfg(test)]\nuse foo;\nfn last() {}",
        );
        assert!(stripped.contains("fn a()"));
        assert!(!stripped.contains("fn t()"));
        assert!(stripped.contains("fn after()"));
        assert!(!stripped.contains("use foo"));
        assert!(stripped.contains("fn last()"));
        assert_eq!(indexed_assignments(&compact(&stripped)), 1);
        // CRLFのfile: test module後の本番関数の置換と、comment内の括弧・置換風文字列を正しく扱う。
        let crlf_source = [
            "fn a() {}",
            "#[cfg(test)]",
            "mod tests {",
            " // }",
            " fn t() {}",
            "}",
            "fn after() {",
            " // squads_mut()[0] = z;",
            " x.squads_mut()[1]",
            " = y;",
            "}",
        ]
        .join("\r\n");
        assert!(crlf_source.contains('\r'));
        let crlf = strip_test_items(&crlf_source);
        assert!(!crlf.contains('\r'));
        assert!(!crlf.contains("fn t()"));
        assert!(crlf.contains("fn after()"));
        assert_eq!(indexed_assignments(&compact(&crlf)), 1);

        // 検出器自体の契約（改行・空白・入れ子index・比較との区別）。
        assert_eq!(
            indexed_assignments(&compact("m.squads_mut()[i]\n    = s;")),
            1
        );
        assert_eq!(
            indexed_assignments(&compact("m.squads_mut()[ix[0]] = s;")),
            1
        );
        assert_eq!(
            indexed_assignments(&compact("m.squads_mut()[i].phase = p;")),
            0
        );
        assert_eq!(indexed_assignments(&compact("m.squads_mut()[i] == s")), 0);
        assert_eq!(deref_assignments(&compact("*squad\n = other;")), 1);
        assert_eq!(deref_assignments(&compact("*squad == other")), 0);
        assert_eq!(deref_assignments(&compact("let n = a * squad=")), 0);

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ai");
        let fields = strip_test_items(
            "struct S { #[cfg(test)] dirty: bool, live: bool }\nfn make() { S { #[cfg(test)] dirty: false, live: true }; }\nfn after() { m.squads_mut()[0] = z; }",
        );
        assert!(!fields.contains("dirty:"));
        assert!(fields.contains("live: bool"));
        assert!(fields.contains("live: true"));
        assert!(fields.contains("fn after()"));
        assert_eq!(indexed_assignments(&compact(&fields)), 1);

        let mut sources = Vec::new();
        production_sources(&root, &mut sources);
        assert!(sources.len() > 50, "本番sourceを走査できていない");
        // test moduleの後ろに本番codeが続くfileも、後ろの本番codeまで走査対象に残る。
        let extended = &sources
            .iter()
            .find(|(path, _)| path.replace('\\', "/").ends_with("ai/v4/extended.rs"))
            .expect("v4/extended.rsを走査する")
            .1;
        assert!(extended.contains("fn refresh_capital_route_path_commitments"));
        assert!(extended.contains("fn apportion_route_force_slots"));
        assert!(!extended.contains("fn frontline_capacity_follows_scanned_node_geometry"));
        assert!(!extended.contains("fn same_land_armored_entry_shortfall"));
        for (path, source) in sources {
            let code = compact(&source);
            assert_eq!(
                indexed_assignments(&code) + deref_assignments(&code),
                0,
                "{path}: Squadを確定口を通さず丸ごと置換している"
            );
        }
    }

    /// S1b: 生産所属の確定はkeyed確定口から提案storeへ同時に書く。同じEntityを
    /// 2つのSquadが提案した場合も、索引は両方の候補を持つ（一意化は正規化の責務）。
    /// 正規化の候補収集は提案storeを読み、未移行writerの差分は同期で取り込む。
    #[test]
    fn produced_commit_writes_proposal_store_and_normalization_reads_it() {
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
            target: None,
            target_island: None,
        });
        let second = proposal.propose(ProducedJoin {
            player,
            entity,
            squad: ProducedSquad::Create(MissionType::Capture),
            fallback_mission: MissionType::Capture,
            target: None,
            target_island: Some(IslandId(0)),
        });
        let delta = OperationLedger::commit_produced(&mut manager, proposal);
        assert!(!manager.has_unsynced_rosters());
        let created = delta.squad_for(second);
        assert_eq!(delta.squad_for(first), existing);
        {
            let store = manager.synced_proposals();
            assert_eq!(
                store.candidates(entity),
                &[
                    (existing, OperationUnitRole::Member),
                    (created, OperationUnitRole::Member)
                ]
            );
            assert_eq!(
                store.proposed(created).unwrap().members,
                BTreeSet::from([entity])
            );
        }
        // 未移行writer（RosterCommit）の差分は、正規化が提案storeを読む前に同期で取り込まれる。
        OperationLedger::roster(&mut manager.squads_mut()[0]).join(legacy);
        assert!(manager.has_unsynced_rosters());
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
        assert!(!manager.has_unsynced_rosters());
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
        // 正規化後の提案storeは現在bindingの写像（敗者Squadから外れた状態）に揃う。
        manager.sync_proposals();
        let winner = registry.assignment(entity).unwrap().squad_id.unwrap();
        let loser = if winner == existing {
            created
        } else {
            existing
        };
        let store = manager.synced_proposals();
        assert_eq!(
            store.candidates(entity),
            &[(winner, OperationUnitRole::Member)]
        );
        assert!(!store.proposed(loser).unwrap().members.contains(&entity));
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
            assert_eq!(squad.mission_type, MissionType::Capture);
            assert_eq!(squad.owner_id, Some(player));
            assert!(squad.members.contains(&entity));
        }
    }
}
