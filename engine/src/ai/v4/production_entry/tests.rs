//! 発行直前の検証で取り消した購入案の意図解放（D-5）の契約。
use super::*;
use crate::ai::islands::IslandId;
use crate::ai::production::commit::IssueAudit;
use crate::resources::{MatchState, Player, Players, UnitRegistry};
use campaign_execution::{
    CampaignFormingReservation, CampaignProductionIntent, CampaignProductionRole,
    V4CampaignExecutionRegistry,
};
use deployment::{DeploymentForecast, DeploymentPosture, PendingDeployment, V4DeploymentRegistry};

const PLAYER: PlayerId = PlayerId(1);
const ISLAND: IslandId = IslandId(4);
const TURN: u32 = 1;

/// 歩兵1,000だけを登録し、資金で発行可否を切り替えるWorld。施設規則の検証は省く。
fn world_with_funds(funds: u32) -> World {
    let mut world = World::new();
    world.insert_resource(Players(vec![Player {
        id: PLAYER,
        name: "P1".to_string(),
        funds,
    }]));
    world.insert_resource(UnitRegistry(crate::collections::HashMap::from_iter([(
        UnitType::Infantry,
        UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1_000,
            ..UnitStats::mock()
        },
    )])));
    world.insert_resource(MatchState::default());
    world
}

fn command(x: usize) -> ProduceUnitCommand {
    ProduceUnitCommand {
        player_id: PLAYER,
        target_x: x,
        target_y: 0,
        unit_type: UnitType::Infantry,
    }
}

fn capture_intent(x: usize) -> CampaignProductionIntent {
    CampaignProductionIntent {
        command: command(x),
        island_id: ISLAND,
        role: CampaignProductionRole::Capture,
        mission_target: Some(GridPosition { x: 9, y: 0 }),
    }
}

fn pending(x: usize, order: u32) -> PendingDeployment {
    PendingDeployment {
        player_id: PLAYER,
        turn: TURN,
        order,
        facility: GridPosition { x, y: 0 },
        unit_type: UnitType::Infantry,
        anchor: GridPosition { x: 8, y: 0 },
        staging_anchor: GridPosition { x: 7, y: 0 },
        posture: DeploymentPosture::Execute,
        slot_kind: SlotKind::Combat,
        priority_enemies: Vec::new(),
        threat_horizon: 1,
        forecast: DeploymentForecast::default(),
        plan_step: None,
        forming_slot: None,
        operation_binding: None,
    }
}

fn planned_combat(x: usize) -> PlannedProduction {
    PlannedProduction {
        command: command(x),
        role: SlotKind::Combat,
        cost: 1_000,
        purpose: PurchasePurpose::OperationSlot,
        deployment: Some(PlannedDeployment {
            anchor: GridPosition { x: 8, y: 0 },
            staging_anchor: GridPosition { x: 7, y: 0 },
            posture: DeploymentPosture::Execute,
            slot_kind: SlotKind::Combat,
            priority_enemies: Vec::new(),
            threat_horizon: 1,
            forecast: DeploymentForecast::default(),
            plan_step: None,
            operation_binding: None,
        }),
        capture_intent: None,
    }
}

fn planned_capture(x: usize) -> PlannedProduction {
    PlannedProduction {
        command: command(x),
        role: SlotKind::Capture,
        cost: 1_000,
        purpose: PurchasePurpose::OperationSlot,
        deployment: None,
        capture_intent: Some(capture_intent(x)),
    }
}

/// `commit_turn_production` が発行前に保存する配備意図と占領意図を同じ形で登録する。
fn register_intents(world: &mut World) {
    let mut deployments = V4DeploymentRegistry::default();
    deployments.replace_turn_orders(PLAYER, TURN, vec![pending(0, 0)]);
    world.insert_resource(deployments);
    let mut campaign = V4CampaignExecutionRegistry::default();
    campaign.append_turn_intents(PLAYER, TURN, &[capture_intent(1)]);
    world.insert_resource(campaign);
}

#[test]
fn rejected_generic_purchases_release_their_deployment_and_capture_intents() {
    let mut world = world_with_funds(500);
    register_intents(&mut world);
    let mut queue = VecDeque::from(vec![planned_combat(0), planned_capture(1)]);

    let issued = issue_next_planned_production(&mut world, TURN, &mut queue);

    assert!(issued.is_empty());
    assert_eq!(world.resource::<IssueAudit>().rejected().len(), 2);
    // 発行しなかった購入の配備・占領意図は、同じ手番のうちに解放する。
    assert_eq!(
        world
            .resource::<V4DeploymentRegistry>()
            .pending_count(PLAYER),
        0
    );
    assert!(
        world
            .resource::<V4CampaignExecutionRegistry>()
            .records_for(PLAYER, ISLAND)
            .is_empty()
    );
}

#[test]
fn issued_generic_purchase_keeps_its_intents() {
    let mut world = world_with_funds(5_000);
    register_intents(&mut world);
    let mut queue = VecDeque::from(vec![planned_combat(0), planned_capture(1)]);

    let issued = issue_next_planned_production(&mut world, TURN, &mut queue);

    assert_eq!(
        issued
            .iter()
            .map(|command| (command.target_x, command.unit_type))
            .collect::<Vec<_>>(),
        vec![(0, UnitType::Infantry)]
    );
    assert!(!world.contains_resource::<IssueAudit>());
    // 発行した購入と、まだ検証していない後続購入の意図は保持する。
    assert_eq!(
        world
            .resource::<V4DeploymentRegistry>()
            .pending_count(PLAYER),
        1
    );
    assert_eq!(
        world
            .resource::<V4CampaignExecutionRegistry>()
            .records_for(PLAYER, ISLAND)
            .len(),
        1
    );
}

#[test]
fn rejected_campaign_command_releases_its_planned_record() {
    let mut world = world_with_funds(500);
    let mut campaign = V4CampaignExecutionRegistry::default();
    campaign.replace_turn_intents(
        PLAYER,
        TURN,
        &[capture_intent(1)],
        CampaignFormingReservation::Immediate,
    );
    world.insert_resource(campaign);
    let mut state = ProductionTurnState::default();
    state.set_campaign_production_plan(PLAYER, TURN, vec![command(1)], true);
    world.insert_resource(state);

    let control =
        decide_campaign_production_v4(&mut world, PLAYER, ProductionPolicy::for_current_path());

    assert!(!matches!(control, CampaignProductionControl::Command(_)));
    assert_eq!(world.resource::<IssueAudit>().rejected().len(), 1);
    assert!(
        world
            .resource::<V4CampaignExecutionRegistry>()
            .records_for(PLAYER, ISLAND)
            .is_empty()
    );
}
