//! 局地作戦の担当候補を組み立て、所属を確定する。
use super::capture_responsibilities::campaign_assignment_capture_responsibilities;
use super::combat_responsibilities::campaign_combat_responsibilities;
use super::{
    CampaignAssignmentScope, CampaignResponsibility, Entity, HashSet, MissionType, PlayerId,
    SquadManager, TerrainConnectivity, World, cargo_is_landed_on_assignment_island,
    entity_can_self_deploy_to_assignment_with_connectivity,
};

pub(super) fn prepare_campaign_local_assignment_with_connectivity(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    connectivity: &mut TerrainConnectivity,
) {
    prepare_campaign_local_assignment_scoped(
        world,
        manager,
        player_id,
        assignment,
        connectivity,
        CampaignAssignmentScope::Full,
    );
}

/// 方策を選ぶ入口だけを保ち、具体的な配属順を決定別の段へ渡す。
pub(super) fn prepare_campaign_local_assignment_scoped(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    connectivity: &mut TerrainConnectivity,
    scope: CampaignAssignmentScope<'_>,
) {
    use crate::ai::island_campaign::IslandCampaignDecision;
    match assignment.decision {
        IslandCampaignDecision::Defend => {
            assign_defense(world, manager, player_id, assignment, connectivity, scope)
        }
        IslandCampaignDecision::Secure => {
            assign_secure(world, manager, player_id, assignment, connectivity, scope)
        }
        IslandCampaignDecision::Expand
        | IslandCampaignDecision::Assault
        | IslandCampaignDecision::Contest
        | IslandCampaignDecision::Reinforce => {
            assign_offense(world, manager, player_id, assignment, connectivity, scope)
        }
        _ => {}
    }
    scope.synchronize_departure(world, manager, player_id, assignment, connectivity);
}

/// 占領責務を先に固定し、残る戦力で敵排除または主要施設防衛を担う。
fn assign_defense(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    connectivity: &mut TerrainConnectivity,
    scope: CampaignAssignmentScope<'_>,
) {
    // 島が脅威下でも未所有施設の占領は止めない。占領可能Entityだけを
    // Capture責務へ固定し、残りを局地敵の排除または拠点防衛へ分割する。
    let capture = local_entities(world, assignment, &assignment.capture_entities);
    let capture_responsibilities = campaign_assignment_capture_responsibilities(
        world,
        manager,
        player_id,
        assignment,
        &capture,
        connectivity,
    );
    let assigned_capture: HashSet<_> = capture_responsibilities
        .iter()
        .flat_map(|responsibility| responsibility.members.iter().copied())
        .collect();
    scope.assign(
        world,
        manager,
        player_id,
        assignment.island_id,
        capture_responsibilities,
        &[MissionType::Capture],
    );
    let combat = defense_combat_members(world, assignment, &assigned_capture, connectivity);
    let mut combat_responsibilities =
        campaign_combat_responsibilities(world, player_id, assignment, &combat, connectivity);
    if !combat.is_empty()
        && combat_responsibilities
            .iter()
            .all(|responsibility| responsibility.mission_type == MissionType::Defense)
        && crate::ai::v4::capital_route::topology::same_land_route_fronts(
            world,
            player_id,
            assignment.island_id,
        )
        .len()
            < 2
    {
        // 接近中でまだ島内に敵がいない場合は、防衛対象をばらけさせず
        // assignmentの主要施設へ集結させる。
        combat_responsibilities = vec![CampaignResponsibility {
            mission_type: MissionType::Defense,
            target: assignment.target_position,
            members: combat,
        }];
    }
    scope.assign(
        world,
        manager,
        player_id,
        assignment.island_id,
        combat_responsibilities,
        &[MissionType::Attack, MissionType::Defense],
    );
}

/// 現地に上陸済みの占領役と戦闘役だけで確保を引き継ぐ。
fn assign_secure(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    connectivity: &mut TerrainConnectivity,
    scope: CampaignAssignmentScope<'_>,
) {
    let capture = local_entities(world, assignment, &assignment.capture_entities);
    let responsibilities = campaign_assignment_capture_responsibilities(
        world,
        manager,
        player_id,
        assignment,
        &capture,
        connectivity,
    );
    scope.assign(
        world,
        manager,
        player_id,
        assignment.island_id,
        responsibilities,
        &[MissionType::Capture],
    );
    let combat = local_entities(world, assignment, &assignment.combat_entities);
    let combat_responsibilities =
        campaign_combat_responsibilities(world, player_id, assignment, &combat, connectivity);
    scope.assign(
        world,
        manager,
        player_id,
        assignment.island_id,
        combat_responsibilities,
        &[MissionType::Attack, MissionType::Defense],
    );
}

/// 占領責務を先に確定し、島外から自力展開できる戦闘役も攻勢へ配属する。
fn assign_offense(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    connectivity: &mut TerrainConnectivity,
    scope: CampaignAssignmentScope<'_>,
) {
    let capture = local_entities(world, assignment, &assignment.capture_entities);
    let capture_responsibilities = campaign_assignment_capture_responsibilities(
        world,
        manager,
        player_id,
        assignment,
        &capture,
        connectivity,
    );
    scope.assign(
        world,
        manager,
        player_id,
        assignment.island_id,
        capture_responsibilities,
        &[MissionType::Capture],
    );
    // 航空戦力など作戦島へ自力展開できる戦力は、島外にいても輸送cargoへ
    // 入れず、直接Attack/Defense責務を与える。
    let combat = assignment
        .combat_entities
        .iter()
        .copied()
        .filter(|entity| {
            cargo_is_landed_on_assignment_island(world, assignment, *entity)
                || entity_can_self_deploy_to_assignment_with_connectivity(
                    world,
                    assignment,
                    *entity,
                    connectivity,
                )
        })
        .collect::<Vec<_>>();
    let responsibilities =
        campaign_combat_responsibilities(world, player_id, assignment, &combat, connectivity);
    scope.assign(
        world,
        manager,
        player_id,
        assignment.island_id,
        responsibilities,
        &[MissionType::Attack, MissionType::Defense],
    );
}

/// 上陸済みの現地Entityだけを入力順で抽出する。
fn local_entities(
    world: &World,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    entities: &[Entity],
) -> Vec<Entity> {
    entities
        .iter()
        .copied()
        .filter(|entity| cargo_is_landed_on_assignment_island(world, assignment, *entity))
        .collect()
}

/// 占領担当を除き、自力展開可能な防衛戦力を重複なく抽出する。
fn defense_combat_members(
    world: &World,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    assigned_capture: &HashSet<Entity>,
    connectivity: &mut TerrainConnectivity,
) -> Vec<Entity> {
    let mut combat: Vec<_> = assignment
        .combat_entities
        .iter()
        .chain(assignment.capture_entities.iter())
        .copied()
        .filter(|entity| !assigned_capture.contains(entity))
        .filter(|entity| {
            world
                .get::<crate::components::Transporting>(*entity)
                .is_none()
                && (cargo_is_landed_on_assignment_island(world, assignment, *entity)
                    || entity_can_self_deploy_to_assignment_with_connectivity(
                        world,
                        assignment,
                        *entity,
                        connectivity,
                    ))
        })
        .collect();
    combat.sort_by_key(|entity| entity.to_bits());
    combat.dedup();
    combat
}
