//! 未所属の輸送役と占領兵を、現在のcampaignへ順序を保って接続する。
use super::*;
use crate::ai::island_campaign::IslandCampaignAssignment;
use crate::ai::islands::{IslandId, IslandMap};
use std::ops::ControlFlow;

struct ReconnectionContext {
    map: Map,
    master: MasterDataRegistry,
    islands: IslandMap,
    player: PlayerId,
}
/// 取り出しと復元の順序を維持する。優先順位の異なる集合へ置き換えない。
struct ReservePool {
    transports: Vec<(Entity, GridPosition, UnitStats)>,
    capturers: Vec<(Entity, GridPosition, UnitStats)>,
}
#[derive(Default)]
struct WaveBatch {
    waves: Vec<assignment::SquadDraft>,
    transfers: Vec<(Entity, OperationUnitRole, OperationOwner)>,
}

fn reconnection_context(world: &World, player_id: PlayerId) -> Option<ReconnectionContext> {
    let map = world.get_resource::<Map>().cloned()?;
    let master = world.get_resource::<MasterDataRegistry>().cloned()?;
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
        .unwrap_or_else(|| crate::ai::islands::IslandMap::analyze(&map));

    Some(ReconnectionContext {
        map,
        master,
        islands: island_map,
        player: player_id,
    })
}

fn collect_reserve_pool(
    world: &World,
    manager: &SquadManager,
    ctx: &ReconnectionContext,
) -> Option<ReservePool> {
    let player_id = ctx.player;
    // 固定点の正規化直後に呼ばれるため、所属は正本の現在bindingから読む。
    let assigned_entities = allocation_unavailable_entities(world, manager, player_id);
    let mut unassigned_entities = world
        .iter_entities()
        .filter_map(|entity_ref| {
            let entity = entity_ref.id();
            let faction = entity_ref.get::<Faction>()?;
            let position = entity_ref.get::<GridPosition>()?;
            let stats = entity_ref.get::<UnitStats>()?;
            (faction.0 == player_id
                && entity_ref
                    .get::<crate::components::Transporting>()
                    .is_none()
                && !assigned_entities.contains(&entity))
            .then_some((entity, *position, stats.clone()))
        })
        .collect::<Vec<_>>();
    unassigned_entities
        .sort_by_key(|(entity, position, _)| (position.y, position.x, entity.to_bits()));
    let reserve_transports = unassigned_entities
        .iter()
        .filter(|(_, _, stats)| {
            matches!(
                stats.unit_type,
                UnitType::TransportHelicopter | UnitType::Lander
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let reserve_capturers = unassigned_entities
        .iter()
        .filter(|(_, _, stats)| stats.can_capture)
        .cloned()
        .collect::<Vec<_>>();
    if reserve_capturers.is_empty() {
        return None;
    }

    Some(ReservePool {
        transports: reserve_transports,
        capturers: reserve_capturers,
    })
}

fn count_island_enemies(world: &mut World, ctx: &ReconnectionContext) -> HashMap<IslandId, usize> {
    let island_map = &ctx.islands;
    let player_id = ctx.player;
    let mut enemy_count_by_island = HashMap::default();
    let mut enemy_query = world.query::<(
        &Faction,
        &GridPosition,
        Option<&crate::components::Transporting>,
    )>();
    for (faction, position, transporting) in enemy_query.iter(world) {
        if faction.0 == player_id || transporting.is_some() {
            continue;
        }
        if let Some(island) = island_map.get_island_at(position) {
            enemy_count_by_island
                .entry(island.id)
                .and_modify(|count: &mut usize| *count = count.saturating_add(1))
                .or_insert(1_usize);
        }
    }

    enemy_count_by_island
}

fn remaining_wave_depth(
    world: &World,
    manager: &SquadManager,
    ctx: &ReconnectionContext,
    assignment: &IslandCampaignAssignment,
    enemy_count_by_island: &HashMap<IslandId, usize>,
) -> usize {
    let player_id = ctx.player;
    let desired_depth = assignment
        .capture_target_positions
        .len()
        .max(1)
        .saturating_add(
            enemy_count_by_island
                .get(&assignment.island_id)
                .copied()
                .unwrap_or(0)
                .min(2),
        );
    let already_executing = manager
        .proposal_view()
        .squads
        .iter()
        .filter(|squad| {
            squad.plan.owner_id == Some(player_id)
                && squad.plan.mission_type != MissionType::Reserve
                && squad.plan.target_island == Some(assignment.island_id)
        })
        .flat_map(|squad| {
            squad
                .members
                .iter()
                .chain(squad.cargo_entities.iter())
                .chain(squad.delivered_cargo.iter())
        })
        .copied()
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|entity| {
            world
                .get::<UnitStats>(*entity)
                .is_some_and(|stats| stats.can_capture)
        })
        .count();
    desired_depth.saturating_sub(already_executing)
}

fn take_wave(
    world: &World,
    ctx: &ReconnectionContext,
    pool: &mut ReservePool,
    assignment: &IslandCampaignAssignment,
    remaining_depth: &mut usize,
    connectivity: &mut TerrainConnectivity,
    batch: &mut WaveBatch,
) -> ControlFlow<()> {
    let map = &ctx.map;
    let master = &ctx.master;
    let island_map = &ctx.islands;
    let player_id = ctx.player;
    let reserve_transports = &mut pool.transports;
    let reserve_capturers = &mut pool.capturers;
    let new_waves = &mut batch.waves;
    let ownership_transfers = &mut batch.transfers;
    let transport_index = reserve_transports.iter().position(|(_, position, stats)| {
        island_map
            .islands
            .iter()
            .find(|island| island.id == assignment.island_id)
            .and_then(|island| {
                get_target_position_for_island(map, master, island, *position, stats.movement_type)
            })
            .is_some()
    });
    let Some(transport_index) = transport_index else {
        return ControlFlow::Break(());
    };
    let (transport, transport_position, transport_stats) =
        reserve_transports.remove(transport_index);
    let capacity = world
        .get::<crate::components::CargoCapacity>(transport)
        .map(|cargo| cargo.max as usize)
        .unwrap_or(0);
    if capacity == 0 {
        return ControlFlow::Continue(());
    }

    let prior_assignments = world
        .get_resource::<UnitOperationRegistry>()
        .map(|registry| registry.player_assignments(player_id))
        .unwrap_or_default();
    let expected_owner = OperationOwner::Campaign {
        player_id,
        island_id: assignment.island_id,
    };
    let mut compatible = reserve_capturers
        .iter()
        .enumerate()
        .filter(|(_, (_, _, stats))| {
            transport_stats
                .loadable_unit_types
                .contains(&stats.unit_type)
        })
        .filter_map(|(index, (entity, _, _))| {
            let owner_rank = u8::from(
                prior_assignments
                    .get(entity)
                    .is_none_or(|prior| prior.owner != expected_owner),
            );
            cargo_pickup_rank(
                world,
                player_id,
                transport_position,
                &transport_stats,
                *entity,
                connectivity,
            )
            .map(|rank| ((owner_rank, rank), index))
        })
        .collect::<Vec<_>>();
    compatible.sort_by_key(|(rank, _)| *rank);
    let mut selected_indices = compatible
        .into_iter()
        .take(capacity.min(*remaining_depth))
        .map(|(_, index)| index)
        .collect::<Vec<_>>();
    selected_indices.sort_unstable_by(|left, right| right.cmp(left));
    let mut cargo_entities = Vec::new();
    for index in selected_indices {
        cargo_entities.push(reserve_capturers.remove(index).0);
    }
    cargo_entities.sort_by_key(|entity| entity.to_bits());
    if cargo_entities.is_empty() {
        return ControlFlow::Continue(());
    }
    let Some(pickup_position) = select_pickup_position(
        world,
        player_id,
        transport_position,
        &transport_stats,
        &cargo_entities,
        connectivity,
    ) else {
        for cargo in cargo_entities {
            if let (Some(position), Some(stats)) = (
                world.get::<GridPosition>(cargo).copied(),
                world.get::<UnitStats>(cargo).cloned(),
            ) {
                reserve_capturers.push((cargo, position, stats));
            }
        }
        return ControlFlow::Continue(());
    };

    *remaining_depth = remaining_depth.saturating_sub(cargo_entities.len());
    ownership_transfers.push((transport, OperationUnitRole::Transport, expected_owner));
    ownership_transfers.extend(
        cargo_entities
            .iter()
            .map(|cargo| (*cargo, OperationUnitRole::Cargo, expected_owner)),
    );
    let mut wave = assignment::SquadDraft::new(MissionType::Transport, MissionPhase::Forming);
    wave.target = Some(assignment.target_position);
    wave.target_island = Some(assignment.island_id);
    wave.phase = MissionPhase::Transport(TransportPhase::Pickup);
    wave.pickup_position = Some(pickup_position);
    wave.allow_partial_departure =
        assignment.decision != crate::ai::island_campaign::IslandCampaignDecision::Assault;
    wave.departure_authorized = assignment.operation_ready
        || assignment.decision != crate::ai::island_campaign::IslandCampaignDecision::Assault;
    wave.members.insert(transport);
    wave.transport_entity = Some(transport);
    wave.cargo_entities = cargo_entities;
    new_waves.push(wave);
    ControlFlow::Continue(())
}

fn attach_stranded_capturers(
    world: &World,
    ctx: &ReconnectionContext,
    pool: &mut ReservePool,
    assignment: &IslandCampaignAssignment,
    remaining_depth: usize,
    batch: &mut WaveBatch,
) {
    let player_id = ctx.player;
    let reserve_capturers = &mut pool.capturers;
    let new_waves = &mut batch.waves;
    let ownership_transfers = &mut batch.transfers;
    if remaining_depth == 0 || reserve_capturers.is_empty() {
        return;
    }

    // 実輸送役が今いなくても、期限超過した既存占領兵をReserveへ戻して同型を
    // 再生産してはならない。自力到達不能な既存Entityを実CampaignのForming cargoへ
    // 接続すると、次回分析がこのcargoを観測して不足する輸送役だけを要求できる。
    let expected_owner = OperationOwner::Campaign {
        player_id,
        island_id: assignment.island_id,
    };
    let prior_assignments = world
        .get_resource::<UnitOperationRegistry>()
        .map(|registry| registry.player_assignments(player_id))
        .unwrap_or_default();
    let mut candidates = reserve_capturers
        .iter()
        .enumerate()
        .filter(|(_, (entity, _, _))| {
            !entity_can_self_deploy_to_assignment(world, assignment, *entity)
        })
        .map(|(index, (entity, position, _))| {
            (
                u8::from(
                    prior_assignments
                        .get(entity)
                        .is_none_or(|prior| prior.owner != expected_owner),
                ),
                position.y,
                position.x,
                entity.to_bits(),
                index,
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    let mut selected_indices = candidates
        .into_iter()
        .take(remaining_depth)
        .map(|candidate| candidate.4)
        .collect::<Vec<_>>();
    selected_indices.sort_unstable_by(|left, right| right.cmp(left));
    let mut forming_cargo = Vec::new();
    for index in selected_indices {
        forming_cargo.push(reserve_capturers.remove(index).0);
    }
    forming_cargo.sort_by_key(|entity| entity.to_bits());
    if forming_cargo.is_empty() {
        return;
    }
    ownership_transfers.extend(
        forming_cargo
            .iter()
            .map(|cargo| (*cargo, OperationUnitRole::Cargo, expected_owner)),
    );
    let mut wave = assignment::SquadDraft::new(MissionType::Transport, MissionPhase::Forming);
    wave.target = Some(assignment.target_position);
    wave.target_island = Some(assignment.island_id);
    wave.phase = MissionPhase::Forming;
    wave.allow_partial_departure =
        assignment.decision != crate::ai::island_campaign::IslandCampaignDecision::Assault;
    wave.departure_authorized = assignment.operation_ready
        || assignment.decision != crate::ai::island_campaign::IslandCampaignDecision::Assault;
    wave.cargo_entities = forming_cargo;
    new_waves.push(wave);
}

fn transfer_leftover_to_assault(
    world: &World,
    ctx: &ReconnectionContext,
    pool: &mut ReservePool,
    campaign_assignments: &[IslandCampaignAssignment],
    batch: &mut WaveBatch,
) {
    let player_id = ctx.player;
    let reserve_capturers = &mut pool.capturers;
    let new_waves = &mut batch.waves;
    let ownership_transfers = &mut batch.transfers;
    // 局地作戦の必要深度を満たした後に残った既存占領兵は、勝利条件へ接続する
    // 首都攻略の後続波へ移管する。到着期限超過を理由に捨てず、兵站gateが開くまでは
    // Assault/Formingとして保持するため、Reserveのまま同型を再生産しない。
    let assault_objective = campaign_assignments
        .iter()
        .filter(|assignment| {
            assignment.decision == crate::ai::island_campaign::IslandCampaignDecision::Assault
        })
        .min_by_key(|assignment| campaign_assignment_priority(assignment))
        .map(|assignment| {
            (
                assignment.island_id,
                assignment.target_position,
                assignment.operation_ready,
            )
        })
        .or_else(|| {
            world
                .get_resource::<crate::ai::v4::victory_roadmap::VictoryRoadmapRegistry>()
                .and_then(|registry| registry.active_capital_objective(player_id))
                // 局地portfolioに現れる前は兵站gate未達なので発進させない。
                .map(|(island_id, anchor)| (island_id, anchor, false))
        });
    if !reserve_capturers.is_empty()
        && let Some((assault_island, assault_target, departure_authorized)) = assault_objective
    {
        let expected_owner = OperationOwner::Campaign {
            player_id,
            island_id: assault_island,
        };
        let mut forming_cargo = reserve_capturers
            .drain(..)
            .map(|(entity, _, _)| entity)
            .collect::<Vec<_>>();
        forming_cargo.sort_by_key(|entity| entity.to_bits());
        ownership_transfers.extend(
            forming_cargo
                .iter()
                .map(|cargo| (*cargo, OperationUnitRole::Cargo, expected_owner)),
        );
        let mut wave = assignment::SquadDraft::new(MissionType::Transport, MissionPhase::Forming);
        wave.target = Some(assault_target);
        wave.target_island = Some(assault_island);
        wave.phase = MissionPhase::Forming;
        wave.allow_partial_departure = false;
        wave.departure_authorized = departure_authorized;
        wave.cargo_entities = forming_cargo;
        new_waves.push(wave);
    }
}

fn commit_transport_waves(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    batch: WaveBatch,
) {
    let WaveBatch {
        waves: new_waves,
        transfers: ownership_transfers,
    } = batch;
    if new_waves.is_empty() {
        return;
    }

    // 輸送波も通常編成と同じ提案→keyed commitで発番する。
    // 手動id・仮Squadのroster・一覧への直接追加を使わない。
    let mut proposal = assignment::AssignmentProposal::new(world, manager, player_id);
    for wave in new_waves {
        proposal.create(wave);
    }
    proposal.commit(manager);

    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let mut operation_registry = world
        .remove_resource::<UnitOperationRegistry>()
        .unwrap_or_default();
    OperationLedger::open(&mut operation_registry).transfer_owners(ownership_transfers, turn);
    world.insert_resource(operation_registry);
}

/// 選択できない輸送役だけを打切りとし、容量なし・積荷なしの場合は次の輸送役を試す。
fn plan_campaign_waves(
    world: &World,
    manager: &SquadManager,
    ctx: &ReconnectionContext,
    pool: &mut ReservePool,
    campaigns: &[IslandCampaignAssignment],
    enemies: &HashMap<IslandId, usize>,
) -> WaveBatch {
    let mut assignments = campaigns.iter().collect::<Vec<_>>();
    assignments.sort_by_key(|assignment| campaign_assignment_priority(assignment));
    let mut batch = WaveBatch::default();
    let mut connectivity = TerrainConnectivity::default();
    for assignment in assignments {
        if pool.capturers.is_empty() {
            break;
        }
        if matches!(
            assignment.decision,
            crate::ai::island_campaign::IslandCampaignDecision::Observe
                | crate::ai::island_campaign::IslandCampaignDecision::Withdraw
        ) {
            continue;
        }
        let mut depth = remaining_wave_depth(world, manager, ctx, assignment, enemies);
        while depth > 0 && !pool.transports.is_empty() && !pool.capturers.is_empty() {
            if take_wave(
                world,
                ctx,
                pool,
                assignment,
                &mut depth,
                &mut connectivity,
                &mut batch,
            )
            .is_break()
            {
                break;
            }
        }
        attach_stranded_capturers(world, ctx, pool, assignment, depth, &mut batch);
    }
    batch
}

pub(super) fn reconnect(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    campaign_assignments: &[IslandCampaignAssignment],
) {
    if campaign_assignments.is_empty() {
        return;
    }
    let Some(ctx) = reconnection_context(world, player_id) else {
        return;
    };
    let Some(mut pool) = collect_reserve_pool(world, manager, &ctx) else {
        return;
    };
    let enemies = count_island_enemies(world, &ctx);
    let mut batch = plan_campaign_waves(
        world,
        manager,
        &ctx,
        &mut pool,
        campaign_assignments,
        &enemies,
    );
    transfer_leftover_to_assault(world, &ctx, &mut pool, campaign_assignments, &mut batch);
    commit_transport_waves(world, manager, player_id, batch);
}
