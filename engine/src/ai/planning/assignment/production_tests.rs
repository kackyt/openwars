use super::*;

/// 補充は人数だけを追加し、発注時の古い目標で本隊の任務を巻き戻さない。
#[test]
fn reinforcement_preserves_the_receiving_squads_mission() {
    let player = PlayerId(1);
    let mut manager = SquadManager::default();
    let target = GridPosition { x: 9, y: 9 };
    let squad_id = {
        let mut proposals = OperationLedger::proposals(&mut manager);
        let mut squad = proposals.create_owned(
            MissionType::Attack,
            player,
            BTreeSet::new(),
            None,
            Vec::new(),
        );
        squad.plan_mut().target = Some(target);
        squad.plan_mut().target_island = Some(IslandId(2));
        squad.plan_mut().phase = MissionPhase::Executing;
        squad.plan_mut().departure_authorized = true;
        squad.id
    };
    let reinforcement = Entity::from_raw(100);
    let mut proposal = ProducedAssignmentProposal::default();
    proposal.propose(ProducedJoin {
        player,
        entity: reinforcement,
        squad: ProducedSquad::Existing(squad_id),
        fallback_mission: MissionType::Defense,
        initial_phase: MissionPhase::Forming,
        target: Some(GridPosition { x: 1, y: 1 }),
        target_island: Some(IslandId(0)),
    });

    OperationLedger::commit_produced(&mut manager, proposal);

    let squad = manager
        .squads
        .iter()
        .find(|squad| squad.id == squad_id)
        .unwrap();
    assert!(squad.members.contains(&reinforcement));
    assert_eq!(squad.plan.target, Some(target));
    assert_eq!(squad.plan.target_island, Some(IslandId(2)));
    assert_eq!(squad.plan.phase, MissionPhase::Executing);
    assert!(squad.plan.departure_authorized);
}
