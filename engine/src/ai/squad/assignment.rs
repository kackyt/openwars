//! 汎用編成の提案（propose）と確定（commit）。
//!
//! 汎用編成の各段は [`SquadManager`] を直接書き換えず、「Squadの新設」と「既存Squadへの追加」を
//! [`AssignmentProposal`] へ順に積む。段の判断が参照するSquadの状態（変更可否・任務・目標・人数）は
//! 提案の中の要約に反映されるため、後の段は前の段の提案を確定前に見られる。
//! 最後に [`AssignmentProposal::commit`] が1回だけSquadManagerへ適用する。
//!
//! 新設Squadの `SquadId` は確定時に提案順で発番するため、直接作成していたときと同じ値になる。

use super::*;
use crate::ai::islands::IslandId;

/// 提案の中のSquadの位置。既存Squadは `SquadManager::squads` の添字、新設Squadは
/// 既存数＋提案順で、確定後の添字と一致する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SquadSlot(usize);

/// 新設するSquadの内容。所有者は提案のplayer。
#[derive(Debug, Clone)]
pub(super) struct SquadDraft {
    pub mission_type: MissionType,
    pub members: BTreeSet<Entity>,
    pub target: Option<GridPosition>,
    pub target_island: Option<IslandId>,
    pub phase: MissionPhase,
    pub transport_entity: Option<Entity>,
    pub cargo_entities: Vec<Entity>,
    pub pickup_position: Option<GridPosition>,
    pub allow_partial_departure: bool,
    pub departure_authorized: bool,
}

impl SquadDraft {
    /// 既定値（`SquadManager::create_squad` と同じ）で下書きを作る。
    pub fn new(mission_type: MissionType, phase: MissionPhase) -> Self {
        Self {
            mission_type,
            members: BTreeSet::new(),
            target: None,
            target_island: None,
            phase,
            transport_entity: None,
            cargo_entities: Vec::new(),
            pickup_position: None,
            allow_partial_departure: false,
            departure_authorized: true,
        }
    }

    /// 輸送役1体の輸送Squadの下書き。
    pub fn transport(
        transport: Entity,
        cargo_entities: Vec<Entity>,
        phase: TransportPhase,
    ) -> Self {
        let mut draft = Self::new(MissionType::Transport, MissionPhase::Transport(phase));
        draft.members.insert(transport);
        draft.transport_entity = Some(transport);
        draft.cargo_entities = cargo_entities;
        draft
    }
}

/// 編成判断が参照するSquadの要約。
#[derive(Debug, Clone)]
pub(super) struct SquadView {
    /// 提案のplayerが変更できるか（[`squad_is_mutable_by_player`]）。
    pub mutable: bool,
    pub mission_type: MissionType,
    pub target: Option<GridPosition>,
    /// 提案を含めた人数。
    pub members: usize,
}

#[derive(Debug, Clone)]
enum AssignmentOp {
    Create(SquadDraft),
    Join { slot: SquadSlot, entity: Entity },
}

/// 確定した変更の要約。
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct AssignmentDelta {
    /// 新設したSquad（発番順）。
    pub created: Vec<SquadId>,
    /// 既存・新設Squadへ追加したメンバー数。
    pub joined: usize,
}

/// 汎用編成の提案。
pub(super) struct AssignmentProposal {
    player: PlayerId,
    /// 提案を作った時点の既存Squad数。
    existing: usize,
    views: Vec<SquadView>,
    ops: Vec<AssignmentOp>,
}

impl AssignmentProposal {
    /// 現在のSquadの要約から空の提案を作る。
    ///
    /// 汎用編成は自軍のfree unitだけを追加するため、既存Squadの変更可否は提案の間変わらない。
    /// 新設Squadは提案のplayerが所有し自軍unitだけを持つので、常に変更できる。
    pub fn new(world: &World, manager: &SquadManager, player: PlayerId) -> Self {
        let views = manager
            .squads
            .iter()
            .map(|squad| SquadView {
                mutable: squad_is_mutable_by_player(world, squad, player),
                mission_type: squad.mission_type.clone(),
                target: squad.target,
                members: squad.members.len(),
            })
            .collect();
        Self {
            player,
            existing: manager.squads.len(),
            views,
            ops: Vec::new(),
        }
    }

    /// 既存・新設のSquad（確定後の並び順）。
    pub fn squads(&self) -> impl Iterator<Item = (SquadSlot, &SquadView)> {
        self.views
            .iter()
            .enumerate()
            .map(|(index, view)| (SquadSlot(index), view))
    }

    /// 変更できる、指定任務・目標のSquad（新設提案を含む）があるか。
    pub fn has_squad(&self, mission_type: MissionType, target: GridPosition) -> bool {
        self.views.iter().any(|view| {
            view.mutable && view.mission_type == mission_type && view.target == Some(target)
        })
    }

    /// Squadの新設を提案する。
    pub fn create(&mut self, draft: SquadDraft) -> SquadSlot {
        let slot = SquadSlot(self.views.len());
        self.views.push(SquadView {
            mutable: true,
            mission_type: draft.mission_type.clone(),
            target: draft.target,
            members: draft.members.len(),
        });
        self.ops.push(AssignmentOp::Create(draft));
        slot
    }

    /// 既存または新設提案済みのSquadへのメンバー追加を提案する。
    pub fn join(&mut self, slot: SquadSlot, entity: Entity) {
        self.views[slot.0].members += 1;
        self.ops.push(AssignmentOp::Join { slot, entity });
    }

    /// 提案をSquadManagerへ提案順に適用する。汎用編成の唯一の更新口。
    ///
    /// 提案後にSquadManagerのSquad数が変わっていれば、添字が一致しないためpanicする。
    pub fn commit(self, manager: &mut SquadManager) -> AssignmentDelta {
        assert_eq!(
            manager.squads.len(),
            self.existing,
            "提案の作成後にSquadManagerが変更された"
        );
        debug_assert!(self.has_unique_entities(), "同じEntityを2回割り当てる提案");
        let mut delta = AssignmentDelta::default();
        for op in self.ops {
            match op {
                AssignmentOp::Create(draft) => {
                    let squad = manager.create_owned_squad(draft.mission_type, self.player);
                    squad.members = draft.members;
                    squad.target = draft.target;
                    squad.target_island = draft.target_island;
                    squad.phase = draft.phase;
                    squad.transport_entity = draft.transport_entity;
                    squad.cargo_entities = draft.cargo_entities;
                    squad.pickup_position = draft.pickup_position;
                    squad.allow_partial_departure = draft.allow_partial_departure;
                    squad.departure_authorized = draft.departure_authorized;
                    delta.created.push(squad.id);
                }
                AssignmentOp::Join { slot, entity } => {
                    manager.squads[slot.0].members.insert(entity);
                    delta.joined += 1;
                }
            }
        }
        delta
    }

    /// 提案の中で、同じEntityがメンバー・積荷として2回以上現れないか。
    fn has_unique_entities(&self) -> bool {
        let mut seen = HashSet::default();
        self.ops.iter().all(|op| match op {
            AssignmentOp::Create(draft) => draft
                .members
                .iter()
                .chain(draft.cargo_entities.iter())
                .all(|entity| seen.insert(*entity)),
            AssignmentOp::Join { entity, .. } => seen.insert(*entity),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn_unit(world: &mut World, player: PlayerId) -> Entity {
        world.spawn((Faction(player), UnitStats::mock())).id()
    }

    #[test]
    fn commit_assigns_squad_ids_in_proposal_order_and_applies_joins() {
        let player = PlayerId(1);
        let mut world = World::new();
        let mut manager = SquadManager::default();
        let existing = manager.create_owned_squad(MissionType::Attack, player).id;
        let first = spawn_unit(&mut world, player);
        let second = spawn_unit(&mut world, player);
        let third = spawn_unit(&mut world, player);

        let mut proposal = AssignmentProposal::new(&world, &manager, player);
        let mut defense = SquadDraft::new(MissionType::Defense, MissionPhase::Forming);
        defense.target = Some(GridPosition { x: 3, y: 4 });
        defense.members.insert(first);
        let created = proposal.create(defense);
        proposal.join(created, second);
        proposal.join(SquadSlot(0), third);
        // 確定前でも、後の段は新設提案と追加人数を参照できる。
        assert!(proposal.has_squad(MissionType::Defense, GridPosition { x: 3, y: 4 }));
        let views: Vec<_> = proposal.squads().map(|(_, view)| view.members).collect();
        assert_eq!(views, vec![1, 2]);

        let delta = proposal.commit(&mut manager);

        assert_eq!(delta.created, vec![SquadId(existing.0 + 1)]);
        assert_eq!(delta.joined, 2);
        assert_eq!(manager.squads[0].members, BTreeSet::from([third]));
        let squad = &manager.squads[1];
        assert_eq!(squad.owner_id, Some(player));
        assert_eq!(squad.members, BTreeSet::from([first, second]));
        assert_eq!(squad.target, Some(GridPosition { x: 3, y: 4 }));
    }

    #[test]
    fn foreign_squads_are_not_reported_as_existing_targets() {
        let player = PlayerId(1);
        let world = World::new();
        let mut manager = SquadManager::default();
        let foreign = manager.create_owned_squad(MissionType::Capture, PlayerId(2));
        foreign.target = Some(GridPosition { x: 1, y: 1 });

        let proposal = AssignmentProposal::new(&world, &manager, player);
        assert!(!proposal.has_squad(MissionType::Capture, GridPosition { x: 1, y: 1 }));
    }

    #[test]
    #[should_panic(expected = "提案の作成後にSquadManagerが変更された")]
    fn commit_rejects_manager_changed_after_proposal() {
        let player = PlayerId(1);
        let world = World::new();
        let mut manager = SquadManager::default();
        let mut proposal = AssignmentProposal::new(&world, &manager, player);
        proposal.create(SquadDraft::new(MissionType::Attack, MissionPhase::Forming));
        manager.create_owned_squad(MissionType::Attack, player);

        proposal.commit(&mut manager);
    }
}
