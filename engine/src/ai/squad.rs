#![allow(clippy::collapsible_if)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::manual_while_let_some)]
#![allow(clippy::unnecessary_map_or)]
#[cfg(test)]
use crate::ai::operation_assignment::UnitOperationAssignment;
pub use crate::ai::planning::assignment::squad_model::{
    MissionPhase, MissionType, SquadId, SquadPlan, TransportPhase,
};

use crate::ai::cluster::detect_enemy_clusters;
use crate::ai::operation_assignment::{OperationOwner, OperationUnitRole, UnitOperationRegistry};
pub(crate) use crate::ai::planning::assignment::squad_model::SquadOperationId;
use crate::ai::planning::assignment::{
    AssignmentProposal, CampaignClaim, CampaignClaimProposal, OperationLedger,
};
pub use crate::ai::planning::assignment::{
    CargoRoster, MemberRoster, Squad, SquadEdit, SquadList, SquadManager, SquadRoster,
    TransportBinding,
};
use crate::ai::planning::decision::AdoptedTurnPortfolio;
use crate::ai::runtime::state::AiTurnProgress;
use crate::ai::turn_distance::{
    TerrainConnectivity, TurnDistanceCache, calculate_all_turn_distances, calculate_turn_distance,
    is_terrain_reachable,
};
use crate::collections::{HashMap, HashSet};
use crate::components::{Ammo, Faction, GridPosition, Health, PlayerId, Property, UnitStats};
use crate::resources::{Map, Terrain, UnitType, master_data::MasterDataRegistry};
use crate::systems::movement::calculate_reachable_tiles;
use bevy_ecs::prelude::*;
use std::collections::BTreeSet;
/// 全Squadを手番の境界で一度だけ線形走査し、Entityごとに唯一の作戦・Squadを確定する。
/// 以降の所有者照会はUnitOperationRegistryのHashMapから平均O(1)で行う。
fn reconcile_unique_operation_assignments(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
) {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let (all_live_entities, live_entities) = {
        let mut query = world.query::<(Entity, &Faction)>();
        let all = query.iter(world).collect::<Vec<_>>();
        (
            all.iter()
                .map(|(entity, _)| *entity)
                .collect::<HashSet<_>>(),
            all.iter()
                .filter_map(|(entity, faction)| (faction.0 == player_id).then_some(*entity))
                .collect::<HashSet<_>>(),
        )
    };
    let transported_by = {
        let mut query = world.query::<(Entity, &crate::components::Transporting)>();
        query
            .iter(world)
            .map(|(cargo, transporting)| (cargo, transporting.0))
            .collect::<HashMap<_, _>>()
    };
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned();
    let entity_islands = world
        .iter_entities()
        .filter_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            let island = island_map.as_ref()?.get_island_at(position)?;
            Some((entity.id(), island.id))
        })
        .collect::<HashMap<_, _>>();
    let arrived_at_campaign = |entity: Entity, owner: OperationOwner| {
        let OperationOwner::Campaign { island_id, .. } = owner else {
            return false;
        };
        entity_islands.get(&entity) == Some(&island_id)
    };
    let mut registry = world
        .remove_resource::<UnitOperationRegistry>()
        .unwrap_or_default();
    let mut ledger = OperationLedger::open(&mut registry);
    ledger.retain_live(&all_live_entities);
    // 生産直後の作戦意図は、まだownerが無いEntityだけの初期seedにする。
    // その後の明示的な作戦移管を発注時anchorで巻き戻してはならない。
    if let Some(produced) = world
        .get_resource::<crate::ai::v4::campaign_execution::V4CampaignExecutionRegistry>()
        .map(|execution| execution.produced_entity_assignments(player_id))
    {
        for (entity, island_id) in produced {
            if live_entities.contains(&entity) {
                let owner = OperationOwner::Campaign {
                    player_id,
                    island_id,
                };
                ledger.seed_produced(entity, owner, turn);
            }
        }
    }
    let existing = registry.resolution_priors(player_id);
    // 正規化前の実行rosterは「提案view」であり正本ではない。候補収集は台帳側で行う。
    let proposal = AssignmentProposal::from_roster_view(
        turn,
        manager,
        player_id,
        &live_entities,
        existing,
        &transported_by,
        arrived_at_campaign,
    );
    let delta = proposal.resolve();
    OperationLedger::open(&mut registry).commit(
        delta,
        manager,
        player_id,
        &all_live_entities,
        &live_entities,
    );
    world.insert_resource(registry);
}

/// allocatorの出力を優先度順に正規台帳へ予約する。同じEntityが複数assignmentに
/// 混入しても最初のownerだけが成立し、後段はHashMap照会で排除される。
fn claim_campaign_portfolio_assignments(
    world: &mut World,
    player_id: PlayerId,
    portfolio: &crate::ai::island_campaign::IslandCampaignPortfolio,
) {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let mut assignments = portfolio
        .defenses
        .iter()
        .chain(portfolio.active_offensives.iter())
        .collect::<Vec<_>>();
    assignments.sort_by_key(|assignment| campaign_assignment_priority(assignment));
    let mut protected_entities = world
        .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
        .map(|deployments| deployments.active_entities(player_id))
        .unwrap_or_default();
    // 首都強襲は兵站gateが開く前から形成する。局地portfolioにまだ現れないことだけを
    // 理由に前ターンの実Entity接続を解放すると、毎ターンReserveへ逆戻りするため保護する。
    if let Some(roadmap) =
        world.get_resource::<crate::ai::v4::victory_roadmap::VictoryRoadmapRegistry>()
    {
        protected_entities.extend(roadmap.active_capital_entities(player_id));
    }
    let mut registry = world
        .remove_resource::<UnitOperationRegistry>()
        .unwrap_or_default();
    let active_owners = assignments
        .iter()
        .map(|assignment| OperationOwner::Campaign {
            player_id,
            island_id: assignment.island_id,
        })
        .collect::<HashSet<_>>();
    // 作戦の優先順、作戦内はTransport→Capture→Combatの順にclaimを提案する。
    let claims = assignments
        .iter()
        .flat_map(|assignment| {
            let owner = OperationOwner::Campaign {
                player_id,
                island_id: assignment.island_id,
            };
            [
                (&assignment.transport_entities, OperationUnitRole::Transport),
                (&assignment.capture_entities, OperationUnitRole::Cargo),
                (&assignment.combat_entities, OperationUnitRole::Member),
            ]
            .into_iter()
            .flat_map(move |(entities, role)| {
                entities.iter().map(move |entity| CampaignClaim {
                    owner,
                    entity: *entity,
                    role,
                })
            })
        })
        .collect();
    // 同じ島の作戦が続いていても、今のportfolioから外れたEntityを旧campaign ownerへ
    // 永久固定しない。明示的なdeploymentだけは実行Squadへ接続されるまで保護する。
    let delta = CampaignClaimProposal {
        player: player_id,
        turn,
        active_owners,
        claims,
        protected: protected_entities,
    }
    .resolve(&registry);
    OperationLedger::open(&mut registry).commit_claims(delta);
    world.insert_resource(registry);
}

/// #53 (V3): 敵生産施設への奪取部隊の護衛として適格かを判定する。
/// 「歩兵に随伴できる機動力があり、損傷しておらず、弾薬がある」戦闘ユニットのみ。
/// 鈍足ユニット (砲台等)・損傷ユニット・弾切れユニットを護衛に組み込むと、
/// 部隊の足が揃わない/戦力にならず奪取が成立しないため除外する。
#[allow(clippy::too_many_arguments)]
fn select_nearest_compatible_cargo(
    candidates: &[(Entity, GridPosition, UnitStats)],
    transport_position: GridPosition,
    transport_stats: &UnitStats,
    target_island: crate::ai::islands::IslandId,
    target_position: Option<GridPosition>,
    island_map: &crate::ai::islands::IslandMap,
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), crate::systems::movement::OccupantInfo>,
    player_id: PlayerId,
    turn_cache: &mut TurnDistanceCache,
    connectivity: &mut TerrainConnectivity,
    enemy_types: &[UnitType],
    damage_chart: Option<&crate::resources::DamageChart>,
    require_effective_combat: bool,
) -> Option<usize> {
    let mut best = None;
    for (index, (entity, position, stats)) in candidates.iter().enumerate() {
        if !transport_stats
            .loadable_unit_types
            .contains(&stats.unit_type)
        {
            continue;
        }
        let already_reachable = target_position.is_some_and(|target| {
            connectivity.is_reachable(
                map,
                registry,
                (position.x, position.y),
                (target.x, target.y),
                stats.movement_type,
            )
        });
        if already_reachable
            || target_position.is_none()
                && island_map
                    .get_island_at(position)
                    .is_some_and(|island| island.id == target_island)
        {
            continue;
        }
        let max_damage = damage_chart.map_or(0, |chart| {
            enemy_types
                .iter()
                .map(|enemy_type| {
                    chart
                        .get_base_damage(stats.unit_type, *enemy_type)
                        .or_else(|| chart.get_base_damage_secondary(stats.unit_type, *enemy_type))
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0)
        });
        if require_effective_combat
            && !enemy_types.is_empty()
            && damage_chart.is_some()
            && max_damage == 0
        {
            continue;
        }
        let distance = calculate_turn_distance(
            map,
            registry,
            unit_positions,
            (position.x, position.y),
            (transport_position.x, transport_position.y),
            stats.movement_type,
            stats.max_movement,
            0,
            player_id,
            turn_cache,
        );
        let score = (
            std::cmp::Reverse(max_damage),
            distance,
            position.y,
            position.x,
            entity.to_bits(),
        );
        if best.is_none_or(|(_, best_score)| score < best_score) {
            best = Some((index, score));
        }
    }
    best.map(|(index, _)| index)
}

/// 首都の生産圏内にある自軍生産施設を、ゲーム本体と同じ条件で列挙する。
fn active_production_positions(world: &World, player_id: PlayerId) -> HashSet<(usize, usize)> {
    let (Some(map), Some(registry)) = (
        world.get_resource::<Map>(),
        world.get_resource::<MasterDataRegistry>(),
    ) else {
        return HashSet::default();
    };
    let capital_positions: Vec<_> = world
        .iter_entities()
        .filter_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.owner_id == Some(player_id) && property.terrain == Terrain::Capital)
                .then_some(*position)
        })
        .collect();
    world
        .iter_entities()
        .filter_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.owner_id == Some(player_id)
                && registry.is_production_facility(property.terrain.as_str())
                && crate::systems::production::is_within_production_range(
                    &capital_positions,
                    position.x,
                    position.y,
                    map.topology,
                ))
            .then_some((position.x, position.y))
        })
        .collect()
}

fn select_pickup_position(
    world: &World,
    player_id: PlayerId,
    transport_position: GridPosition,
    transport_stats: &UnitStats,
    cargo_entities: &[Entity],
    connectivity: &mut TerrainConnectivity,
) -> Option<GridPosition> {
    let map = world.resource::<Map>();
    let registry = world.resource::<MasterDataRegistry>();
    let cargo_data: Vec<_> = cargo_entities
        .iter()
        .filter_map(|cargo| {
            Some((
                *world.get::<GridPosition>(*cargo)?,
                world.get::<UnitStats>(*cargo)?.clone(),
            ))
        })
        .collect();
    if cargo_data.len() != cargo_entities.len() {
        return None;
    }
    let occupied_positions: HashSet<_> = world
        .iter_entities()
        .filter(|entity| entity.get::<crate::components::Transporting>().is_none())
        .filter_map(|entity| {
            entity
                .get::<GridPosition>()
                .map(|position| (position.x, position.y))
        })
        .collect();

    // 合流点に生産施設を使うと、積載が終わるまで次の航空・海上戦力を生産できない。
    // 合法な非生産タイルがある限りPickup候補の最後へ回す。
    let production_positions = active_production_positions(world, player_id);

    let mut best = None;
    for y in 0..map.height {
        for x in 0..map.width {
            let Some(terrain) = map.get_terrain(x, y) else {
                continue;
            };
            if crate::systems::movement::get_valid_movement_cost(
                registry,
                transport_stats.movement_type,
                terrain,
            )
            .is_none()
                || transport_stats.movement_type == crate::resources::MovementType::Ship
                    && !matches!(terrain, Terrain::Port | Terrain::Shoal)
                || !connectivity.is_reachable(
                    map,
                    registry,
                    (transport_position.x, transport_position.y),
                    (x, y),
                    transport_stats.movement_type,
                )
            {
                continue;
            }
            // 現在位置以外の占有済み合流点は輸送役が入れないため除外する。
            // cargoだけを見ると、別輸送役が待つ港・空港を選んで永久に接近し続ける。
            if (x, y) != (transport_position.x, transport_position.y)
                && occupied_positions.contains(&(x, y))
            {
                continue;
            }

            let transport_distance = map.distance(transport_position.x, transport_position.y, x, y);
            let transport_turns = transport_distance.div_ceil(transport_stats.max_movement.max(1));
            let mut max_turns = transport_turns;
            let mut total_turns = transport_turns;
            let mut max_distance = transport_distance;
            let mut total_distance = transport_distance;
            let all_cargo_can_board = cargo_data.iter().all(|(position, stats)| {
                let boarding_distance = if terrain == Terrain::Shoal
                    && transport_stats.movement_type == crate::resources::MovementType::Ship
                {
                    map.get_adjacent(x, y)
                        .into_iter()
                        .filter(|adjacent| {
                            map.get_terrain(adjacent.0, adjacent.1)
                                .and_then(|adjacent_terrain| {
                                    crate::systems::movement::get_valid_movement_cost(
                                        registry,
                                        stats.movement_type,
                                        adjacent_terrain,
                                    )
                                })
                                .is_some()
                                && connectivity.is_reachable(
                                    map,
                                    registry,
                                    (position.x, position.y),
                                    *adjacent,
                                    stats.movement_type,
                                )
                        })
                        .map(|adjacent| {
                            map.distance(position.x, position.y, adjacent.0, adjacent.1) + 1
                        })
                        .min()
                } else if connectivity.is_reachable(
                    map,
                    registry,
                    (position.x, position.y),
                    (x, y),
                    stats.movement_type,
                ) {
                    Some(map.distance(position.x, position.y, x, y))
                } else {
                    None
                };
                if let Some(distance) = boarding_distance {
                    let cargo_turns = distance.div_ceil(stats.max_movement.max(1));
                    max_turns = max_turns.max(cargo_turns);
                    total_turns = total_turns.saturating_add(cargo_turns);
                    max_distance = max_distance.max(distance);
                    total_distance += distance;
                    true
                } else {
                    false
                }
            });
            if !all_cargo_can_board {
                continue;
            }
            let production_rank = u8::from(production_positions.contains(&(x, y)));
            let current_rank = if (x, y) == (transport_position.x, transport_position.y) {
                0u8
            } else {
                1u8
            };
            let score = (
                production_rank,
                // 現在地に居座ることより、全員が揃う推定手番を優先する。
                // 輸送ヘリと歩兵では移動力が違うため、生距離だけでなく各自の
                // 移動力で切り上げた最大ETAを便の搭載所要時間として比較する。
                max_turns,
                total_turns,
                current_rank,
                max_distance,
                total_distance,
                y,
                x,
            );
            if best.is_none_or(|(_, best_score)| score < best_score) {
                best = Some((GridPosition { x, y }, score));
            }
        }
    }
    best.map(|(position, _)| position)
}

/// 1体のcargoと輸送役が最適なPickup地点へ合流する推定手番。
///
/// 複数輸送役へcargoを分配するときにEntity ID順を使うと、近いcargoを別便へ渡し、
/// 空の輸送役が遠いcargoを数ターン待つ。最適合流点を単体でも評価し、最大ETA、
/// 合計ETA、距離、Entity IDの順で安定して近い組を作る。
fn cargo_pickup_rank(
    world: &World,
    player_id: PlayerId,
    transport_position: GridPosition,
    transport_stats: &UnitStats,
    cargo: Entity,
    connectivity: &mut TerrainConnectivity,
) -> Option<(u32, u32, u32, u64)> {
    let cargo_position = *world.get::<GridPosition>(cargo)?;
    let cargo_stats = world.get::<UnitStats>(cargo)?;
    let pickup = select_pickup_position(
        world,
        player_id,
        transport_position,
        transport_stats,
        &[cargo],
        connectivity,
    )?;
    let map = world.resource::<Map>();
    let transport_turns = map
        .distance(
            transport_position.x,
            transport_position.y,
            pickup.x,
            pickup.y,
        )
        .div_ceil(transport_stats.max_movement.max(1));
    let cargo_distance = map.distance(cargo_position.x, cargo_position.y, pickup.x, pickup.y);
    let cargo_turns = cargo_distance.div_ceil(cargo_stats.max_movement.max(1));
    Some((
        transport_turns.max(cargo_turns),
        transport_turns.saturating_add(cargo_turns),
        cargo_distance,
        cargo.to_bits(),
    ))
}

/// cargoが現在の手番に輸送役へ移動し、そのままLoadできるかをゲームの移動規則で判定する。
/// 単純距離では地形・占有・燃料を落とすため、実際の到達可能タイルを使用する。
fn cargo_can_board_transport_this_turn(
    world: &mut World,
    cargo: Entity,
    transport: Entity,
) -> bool {
    if world
        .get::<crate::components::Transporting>(cargo)
        .is_some()
        || world
            .get::<crate::components::HasMoved>(cargo)
            .is_none_or(|moved| moved.0)
        || world
            .get::<crate::components::ActionCompleted>(cargo)
            .is_none_or(|action| action.0)
    {
        return false;
    }

    let (cargo_position, cargo_stats, cargo_fuel, cargo_faction) = match (
        world.get::<GridPosition>(cargo).copied(),
        world.get::<UnitStats>(cargo).cloned(),
        world
            .get::<crate::components::Fuel>(cargo)
            .map(|fuel| fuel.current),
        world.get::<Faction>(cargo).map(|faction| faction.0),
    ) {
        (Some(position), Some(stats), Some(fuel), Some(faction)) => {
            (position, stats, fuel, faction)
        }
        _ => return false,
    };
    let (transport_position, transport_stats, transport_faction, has_capacity) = match (
        world.get::<GridPosition>(transport).copied(),
        world.get::<UnitStats>(transport),
        world.get::<Faction>(transport).map(|faction| faction.0),
        world.get::<crate::components::CargoCapacity>(transport),
    ) {
        (Some(position), Some(stats), Some(faction), Some(capacity)) => (
            position,
            stats.clone(),
            faction,
            capacity.loaded.len() < capacity.max as usize,
        ),
        _ => return false,
    };
    if cargo_faction != transport_faction
        || !has_capacity
        || !transport_stats
            .loadable_unit_types
            .contains(&cargo_stats.unit_type)
    {
        return false;
    }
    if cargo_position == transport_position {
        return true;
    }

    let mut unit_positions = HashMap::default();
    let mut query = world.query::<(
        &GridPosition,
        &Faction,
        &UnitStats,
        Option<&crate::components::CargoCapacity>,
        Option<&crate::components::Transporting>,
    )>();
    for (position, faction, stats, capacity, transporting) in query.iter(world) {
        if transporting.is_some() {
            continue;
        }
        unit_positions.insert(
            (position.x, position.y),
            crate::systems::movement::OccupantInfo {
                player_id: faction.0,
                is_transport: stats.max_cargo > 0,
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots: capacity
                    .map(|capacity| capacity.max.saturating_sub(capacity.loaded.len() as u32))
                    .unwrap_or(0),
            },
        );
    }
    let map = world.resource::<Map>();
    let registry = world.resource::<MasterDataRegistry>();
    calculate_reachable_tiles(
        map,
        &unit_positions,
        (cargo_position.x, cargo_position.y),
        cargo_stats.movement_type,
        cargo_stats.max_movement,
        cargo_fuel,
        cargo_faction,
        cargo_stats.unit_type,
        registry,
    )
    .contains(&(transport_position.x, transport_position.y))
}

fn escort_is_eligible(
    stats: &UnitStats,
    infantry_movement: u32,
    hp: u32,
    ammo1: u32,
    ammo2: u32,
) -> bool {
    // 機動力: 歩兵と同等以上 (鈍足ユニットを弾く)
    if stats.max_movement < infantry_movement {
        return false;
    }
    // 損傷: HP < 70 のユニットは前線から抜くべきでない
    if hp < 70 {
        return false;
    }
    // 弾薬: 主武器が弾切れ (副武器も無し) なら戦力にならない
    if stats.max_ammo1 > 0 && ammo1 == 0 && !(stats.max_ammo2 > 0 && ammo2 > 0) {
        return false;
    }
    true
}

fn inferred_squad_owner(world: &World, squad: &Squad) -> Result<Option<PlayerId>, ()> {
    let mut owner = None;
    for entity in squad
        .transport_entity
        .into_iter()
        .chain(squad.members.iter().copied())
        .chain(squad.cargo_entities.iter().copied())
        .chain(squad.delivered_cargo.iter().copied())
    {
        let Some(faction) = world.get::<Faction>(entity) else {
            continue;
        };
        if owner.is_some_and(|owner| owner != faction.0) {
            return Err(());
        }
        owner = Some(faction.0);
    }
    Ok(owner)
}

pub(crate) fn squad_is_mutable_by_player(
    world: &World,
    squad: &Squad,
    player_id: PlayerId,
) -> bool {
    let Ok(inferred_owner) = inferred_squad_owner(world, squad) else {
        return false;
    };
    match squad.plan.owner_id {
        Some(owner) => {
            owner == player_id && inferred_owner.is_none_or(|inferred| inferred == owner)
        }
        // ownerless legacy Squadは既知Factionが一意な場合だけ安全に扱い、空Squadは誰にも開放しない。
        None => inferred_owner == Some(player_id),
    }
}

fn adopt_legacy_squad_owners(world: &World, manager: &mut SquadManager) {
    let mut squads: Vec<_> = manager.edit_each().collect();
    squads.sort_by_key(|squad| squad.id.0);
    for mut squad in squads {
        if squad.plan.owner_id.is_none() {
            if let Ok(Some(owner)) = inferred_squad_owner(world, &squad) {
                // 一意なlive Factionを持つlegacy Squadだけを決定的に移行する。
                squad.plan_mut().owner_id = Some(owner);
            }
        }
    }
}

fn is_purchase_campaign_placeholder(squad: &Squad, player_id: PlayerId) -> bool {
    squad.plan.owner_id == Some(player_id)
        && squad.plan.mission_type == MissionType::Transport
        && squad.plan.phase == MissionPhase::Forming
        && squad.transport_entity.is_none()
        && squad.members.is_empty()
        && squad.cargo_entities.is_empty()
        && squad.delivered_cargo.is_empty()
        && squad.plan.target_island.is_some()
        && squad.plan.target.is_some()
}

/// 輸送ヘリが着陸地点を確保するため、目標島内で攻撃可能な最寄りの軽歩兵を選ぶ。
/// 距離が同じ場合は座標とEntity IDで順序を固定し、再計画時の目標揺れを防ぐ。
fn light_infantry_target_for_transport(
    world: &World,
    transport: Entity,
    player_id: PlayerId,
    target_island: crate::ai::islands::IslandId,
) -> Option<GridPosition> {
    if world
        .get::<UnitStats>(transport)
        .is_none_or(|stats| stats.unit_type != UnitType::TransportHelicopter)
    {
        return None;
    }
    let island_map = world.get_resource::<crate::ai::islands::IslandMap>()?;
    let mut targets: Vec<_> = world
        .iter_entities()
        .filter_map(|entity| {
            let position = entity.get::<GridPosition>().copied()?;
            let faction = entity.get::<Faction>()?;
            let stats = entity.get::<UnitStats>()?;
            if faction.0 == player_id
                || !matches!(stats.unit_type, UnitType::Infantry | UnitType::Mech)
                || island_map
                    .get_island_at(&position)
                    .is_none_or(|island| island.id != target_island)
            {
                return None;
            }
            let distance =
                campaign_member_attack_distance(world, transport, entity.id(), position)?;
            Some((distance, position.y, position.x, entity.id(), position))
        })
        .collect();
    targets.sort_by_key(|target| (target.0, target.1, target.2, target.3.to_bits()));
    targets.first().map(|target| target.4)
}

/// 毎ターンの部隊の再編成と SoloFallback の判定を行います。
pub fn update_squads(world: &mut World, perspective_player: PlayerId) {
    update::update(world, perspective_player);
}

#[derive(Clone)]
struct CampaignTransportCandidate {
    entity: Entity,
    position: GridPosition,
    stats: UnitStats,
    capacity: usize,
    loaded: Vec<Entity>,
}

struct CampaignTransportPartition {
    transport: Entity,
    cargo: Vec<Entity>,
    pickup_position: GridPosition,
    all_loaded: bool,
}

fn search_ready_campaign_transport_partition(
    world: &World,
    player_id: PlayerId,
    transports: &[CampaignTransportCandidate],
    cargo_entities: &[Entity],
    cargo_index: usize,
    assigned: &mut [Vec<Entity>],
) -> Option<Vec<CampaignTransportPartition>> {
    if cargo_index < cargo_entities.len() {
        let cargo = cargo_entities[cargo_index];
        let cargo_type = world.get::<UnitStats>(cargo)?.unit_type;
        for (transport_index, transport) in transports.iter().enumerate() {
            if assigned[transport_index].len() >= transport.capacity
                || !transport.stats.loadable_unit_types.contains(&cargo_type)
            {
                continue;
            }
            assigned[transport_index].push(cargo);
            if let Some(partition) = search_ready_campaign_transport_partition(
                world,
                player_id,
                transports,
                cargo_entities,
                cargo_index + 1,
                assigned,
            ) {
                return Some(partition);
            }
            assigned[transport_index].pop();
        }
        return None;
    }

    // packageが予約した各輸送役を実行Squadへ接続し、空輸送だけの部分発進を避ける。
    if assigned.iter().any(Vec::is_empty) {
        return None;
    }

    let mut partitions = Vec::with_capacity(transports.len());
    let mut connectivity = TerrainConnectivity::default();
    for (transport, cargo) in transports.iter().zip(assigned.iter()) {
        let unloaded: Vec<_> = cargo
            .iter()
            .filter(|entity| !transport.loaded.contains(entity))
            .copied()
            .collect();
        let pickup_position = if unloaded.is_empty() {
            transport.position
        } else {
            select_pickup_position(
                world,
                player_id,
                transport.position,
                &transport.stats,
                &unloaded,
                &mut connectivity,
            )?
        };
        let mut ordered_cargo = cargo.clone();
        ordered_cargo.sort_by_key(|entity| {
            let capture_rank = world
                .get::<UnitStats>(*entity)
                .map_or(1u8, |stats| if stats.can_capture { 0 } else { 1 });
            (capture_rank, entity.to_bits())
        });
        let all_loaded = ordered_cargo
            .iter()
            .all(|entity| transport.loaded.contains(entity));
        partitions.push(CampaignTransportPartition {
            transport: transport.entity,
            cargo: ordered_cargo,
            pickup_position,
            all_loaded,
        });
    }
    Some(partitions)
}

fn build_ready_campaign_transport_partitions(
    world: &World,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    advanced_live_transports: &HashSet<Entity>,
) -> Option<Vec<CampaignTransportPartition>> {
    let mut transport_entities = assignment.transport_entities.clone();
    transport_entities.sort_by_key(|entity| entity.to_bits());
    transport_entities.dedup();
    if transport_entities.is_empty() {
        return None;
    }
    let transport_set: HashSet<_> = transport_entities.iter().copied().collect();

    let island_map = world.get_resource::<crate::ai::islands::IslandMap>();
    let mut requested_cargo: Vec<_> = assignment
        .capture_entities
        .iter()
        .chain(assignment.combat_entities.iter())
        .copied()
        .filter(|cargo| {
            // 既に対象島へ上陸して通常部隊へ引き渡されたEntityは、再輸送候補へ戻さない。
            world
                .get::<crate::components::Transporting>(*cargo)
                .is_some()
                || world.get::<GridPosition>(*cargo).is_none_or(|position| {
                    island_map
                        .and_then(|map| map.get_island_at(position))
                        .is_none_or(|island| island.id != assignment.island_id)
                })
        })
        .collect();
    requested_cargo.sort_by_key(|entity| entity.to_bits());
    requested_cargo.dedup();
    if requested_cargo
        .iter()
        .any(|cargo| transport_set.contains(cargo))
    {
        return None;
    }

    let mut loaded_owner = HashMap::default();
    let mut transports = Vec::with_capacity(transport_entities.len());
    for transport in transport_entities {
        let position = world.get::<GridPosition>(transport).copied()?;
        let stats = world.get::<UnitStats>(transport)?.clone();
        let cargo_capacity = world.get::<crate::components::CargoCapacity>(transport)?;
        let mut loaded = cargo_capacity.loaded.clone();
        loaded.sort_by_key(|entity| entity.to_bits());
        loaded.dedup();
        if loaded.len() > cargo_capacity.max as usize {
            return None;
        }
        // Transit/Drop中の輸送役へ未搭載cargoを追加するとPickupへ逆行するため、実搭載分を上限に固定する。
        let capacity = if advanced_live_transports.contains(&transport) {
            loaded.len()
        } else {
            cargo_capacity.max as usize
        };
        for cargo in &loaded {
            if transport_set.contains(cargo)
                || world
                    .get::<crate::components::Transporting>(*cargo)
                    .is_none_or(|transporting| transporting.0 != transport)
            {
                return None;
            }
            let cargo_type = world.get::<UnitStats>(*cargo)?.unit_type;
            if !stats.loadable_unit_types.contains(&cargo_type)
                || loaded_owner.insert(*cargo, transport).is_some()
            {
                return None;
            }
            if !requested_cargo.contains(cargo) {
                // Forming以前から実搭載されていたcargoも、実際の輸送役との関係を保持する。
                requested_cargo.push(*cargo);
            }
        }
        transports.push(CampaignTransportCandidate {
            entity: transport,
            position,
            stats,
            capacity,
            loaded,
        });
    }

    for cargo in &requested_cargo {
        if let Some(transporting) = world.get::<crate::components::Transporting>(*cargo)
            && loaded_owner.get(cargo) != Some(&transporting.0)
        {
            return None;
        }
    }

    let mut remaining_cargo: Vec<_> = requested_cargo
        .into_iter()
        .filter(|cargo| !loaded_owner.contains_key(cargo))
        .collect();
    // 選択肢の少ないcargoから探索するが、行き止まりでは必ず巻き戻して完全partitionを優先する。
    remaining_cargo.sort_by_key(|cargo| {
        let cargo_stats = world.get::<UnitStats>(*cargo);
        let compatible_count = cargo_stats.map_or(usize::MAX, |cargo_stats| {
            transports
                .iter()
                .filter(|transport| {
                    transport.loaded.len() < transport.capacity
                        && transport
                            .stats
                            .loadable_unit_types
                            .contains(&cargo_stats.unit_type)
                })
                .count()
        });
        let capture_rank = cargo_stats.map_or(1u8, |stats| if stats.can_capture { 0 } else { 1 });
        (compatible_count, capture_rank, cargo.to_bits())
    });
    if remaining_cargo
        .iter()
        .any(|cargo| world.get::<UnitStats>(*cargo).is_none())
    {
        return None;
    }

    let mut assigned: Vec<Vec<Entity>> = transports
        .iter()
        .map(|transport| transport.loaded.clone())
        .collect();
    search_ready_campaign_transport_partition(
        world,
        player_id,
        &transports,
        &remaining_cargo,
        0,
        &mut assigned,
    )
}

/// readyになった既存作戦を、輸送役ごとの実行Squadへ決定的に再調整する。
fn reconcile_ready_forming_campaign_squad(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
) -> bool {
    if !assignment.operation_ready {
        return false;
    }
    let has_target_forming = manager.proposal_view().squads.iter().any(|squad| {
        squad_is_mutable_by_player(world, squad, player_id)
            && squad.plan.mission_type == MissionType::Transport
            && squad.plan.phase == MissionPhase::Forming
            && squad.plan.target_island == Some(assignment.island_id)
    });
    if !has_target_forming {
        // 通常のlive作戦だけになった後は再partitionせず、実行フェーズの進行をそのまま維持する。
        return false;
    }

    let assignment_transports: HashSet<_> = assignment.transport_entities.iter().copied().collect();
    let mut candidate_squads = Vec::new();
    OperationLedger::squad_list(manager).retain(|squad| {
        let is_target_forming = squad_is_mutable_by_player(world, squad, player_id)
            && squad.plan.mission_type == MissionType::Transport
            && squad.plan.phase == MissionPhase::Forming
            && squad.plan.target_island == Some(assignment.island_id);
        let is_target_live = squad_is_mutable_by_player(world, squad, player_id)
            && squad.plan.mission_type == MissionType::Transport
            && squad.plan.target_island == Some(assignment.island_id)
            && squad
                .transport_entity
                .is_some_and(|transport| assignment_transports.contains(&transport))
            && matches!(
                squad.plan.phase,
                MissionPhase::Transport(
                    TransportPhase::Pickup | TransportPhase::Transit | TransportPhase::Drop
                )
            );
        if is_target_forming || is_target_live {
            candidate_squads.push(squad.clone());
        }
        !(is_target_forming || is_target_live)
    });
    if candidate_squads.is_empty() {
        return false;
    }
    candidate_squads.sort_by_key(|squad| squad.id.0);

    // Transit/Dropの輸送役は実搭載cargoだけで作戦を継続し、再計画でPickupへ戻さない。
    let advanced_live_transports: HashSet<_> = candidate_squads
        .iter()
        .filter(|squad| {
            matches!(
                squad.plan.phase,
                MissionPhase::Transport(TransportPhase::Transit | TransportPhase::Drop)
            )
        })
        .filter_map(|squad| *squad.transport_entity)
        .collect();
    let Some(mut partitions) = build_ready_campaign_transport_partitions(
        world,
        player_id,
        assignment,
        &advanced_live_transports,
    ) else {
        // 完全なcapacity/loadability/rendezvousが成立しない場合は元の作戦群を保つ。
        OperationLedger::squad_list(manager).extend(candidate_squads);
        OperationLedger::squad_list(manager).sort_by_id();
        return true;
    };
    partitions.sort_by_key(|partition| partition.transport.to_bits());
    let incorporated_cargo_by_transport: HashMap<_, HashSet<_>> = partitions
        .iter()
        .map(|partition| {
            (
                partition.transport,
                partition.cargo.iter().copied().collect(),
            )
        })
        .collect();

    let has_unincorporated_loaded_state = |squad: &Squad| {
        let transport_loaded_is_unique = squad.transport_entity.is_some_and(|transport| {
            world
                .get::<crate::components::CargoCapacity>(transport)
                .is_some_and(|capacity| {
                    capacity.loaded.iter().any(|cargo| {
                        incorporated_cargo_by_transport
                            .get(&transport)
                            .is_none_or(|incorporated| !incorporated.contains(cargo))
                    })
                })
        });
        transport_loaded_is_unique
            || squad.cargo_entities.iter().any(|cargo| {
                world
                    .get::<crate::components::Transporting>(*cargo)
                    .is_some_and(|transporting| {
                        incorporated_cargo_by_transport
                            .get(&transporting.0)
                            .is_none_or(|incorporated| !incorporated.contains(cargo))
                    })
            })
    };

    let mut owned_entities: HashSet<_> = assignment.transport_entities.iter().copied().collect();
    for partition in &partitions {
        owned_entities.extend(partition.cargo.iter().copied());
    }
    let ids = manager
        .proposal_view()
        .squads
        .iter()
        .filter(|squad| squad_is_mutable_by_player(world, squad, player_id))
        .map(|squad| squad.id)
        .collect::<Vec<_>>();
    {
        let mut proposals = OperationLedger::proposals(manager);
        for id in ids {
            proposals.release_where(id, |entity| owned_entities.contains(entity));
        }
    }

    let live_state_is_compatible = |squad: &Squad, partition: &CampaignTransportPartition| {
        if squad.transport_entity != Some(partition.transport)
            || !squad.members.contains(&partition.transport)
            || squad.cargo_entities.is_empty()
            || !squad
                .cargo_entities
                .iter()
                .all(|cargo| partition.cargo.contains(cargo))
        {
            return false;
        }
        match squad.plan.phase {
            MissionPhase::Transport(TransportPhase::Drop | TransportPhase::Transit) => {
                partition.all_loaded
            }
            MissionPhase::Transport(TransportPhase::Pickup) => {
                !partition.all_loaded
                    && squad.plan.pickup_position == Some(partition.pickup_position)
            }
            _ => false,
        }
    };

    // assignment輸送役との完全一致を最優先し、進行中live、Forming、非互換liveの順で再利用する。
    let mut selected_squads = vec![None; partitions.len()];
    for (partition_index, partition) in partitions.iter().enumerate() {
        let best = candidate_squads
            .iter()
            .enumerate()
            .filter(|(_, squad)| squad.transport_entity == Some(partition.transport))
            .min_by_key(|(_, squad)| {
                let phase_rank = if live_state_is_compatible(squad, partition) {
                    match squad.plan.phase {
                        MissionPhase::Transport(TransportPhase::Drop) => 0u8,
                        MissionPhase::Transport(TransportPhase::Transit) => 1u8,
                        MissionPhase::Transport(TransportPhase::Pickup) => 2u8,
                        _ => 5u8,
                    }
                } else if squad.plan.phase == MissionPhase::Forming {
                    3u8
                } else {
                    4u8
                };
                (phase_rank, squad.id.0)
            })
            .map(|(index, squad)| (index, live_state_is_compatible(squad, partition)));
        if let Some((index, preserve_live_state)) = best {
            selected_squads[partition_index] =
                Some((candidate_squads.remove(index), preserve_live_state));
        }
    }
    for selected in &mut selected_squads {
        if selected.is_none() {
            // transport不一致のfallbackは、固有の実搭載状態を持たないFormingだけに限定する。
            let fallback = candidate_squads.iter().position(|squad| {
                squad.plan.phase == MissionPhase::Forming && !has_unincorporated_loaded_state(squad)
            });
            *selected = if let Some(index) = fallback {
                Some((candidate_squads.remove(index), false))
            } else {
                manager.create_owned_squad(MissionType::Transport, player_id);
                Some((OperationLedger::squad_list(manager).pop().unwrap(), false))
            };
        }
    }

    let mut reconciled = Vec::with_capacity(partitions.len());
    for (partition, selected) in partitions.into_iter().zip(selected_squads) {
        let (mut squad, preserve_live_state) = selected.unwrap();
        OperationLedger::roster(&mut squad.edit())
            .rebind_transport_wave(partition.transport, partition.cargo);
        squad.plan.target_island = Some(assignment.island_id);
        squad.plan.target = Some(assignment.target_position);
        OperationLedger::roster(&mut squad.edit()).clear_delivered();
        if !preserve_live_state {
            squad.plan.allow_partial_departure = matches!(
                assignment.decision,
                crate::ai::island_campaign::IslandCampaignDecision::Expand
                    | crate::ai::island_campaign::IslandCampaignDecision::Secure
                    | crate::ai::island_campaign::IslandCampaignDecision::Contest
                    | crate::ai::island_campaign::IslandCampaignDecision::Reinforce
            );
            squad.plan.pickup_position = Some(partition.pickup_position);
            squad.plan.drop_position = None;
            // 新規またはForming Squadは、自身の全cargoが実搭載済みの場合だけTransitを許可する。
            squad.plan.phase = MissionPhase::Transport(if partition.all_loaded {
                TransportPhase::Transit
            } else {
                TransportPhase::Pickup
            });
        }
        reconciled.push(squad);
    }

    // assignmentへ組み込まれていない実搭載関係を持つFormingだけは残し、live duplicateは残さない。
    let preserved_loaded_squads = candidate_squads
        .into_iter()
        .filter(|squad| squad.plan.phase == MissionPhase::Forming)
        .filter(has_unincorporated_loaded_state);

    // 空の重複候補は破棄し、assignment輸送役ごとに1 Squadだけを残す。
    OperationLedger::squad_list(manager).extend(reconciled);
    OperationLedger::squad_list(manager).extend(preserved_loaded_squads);
    OperationLedger::squad_list(manager).sort_by_id();
    false
}

fn campaign_assignment_priority(
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
) -> (u8, usize) {
    use crate::ai::island_campaign::IslandCampaignDecision;

    let rank = match assignment.decision {
        IslandCampaignDecision::Defend => 0,
        _ if assignment.continued_from_existing_squad => 1,
        IslandCampaignDecision::Expand => 2,
        IslandCampaignDecision::Contest | IslandCampaignDecision::Reinforce => 3,
        IslandCampaignDecision::Assault => 4,
        _ => 5,
    };
    (rank, assignment.island_id.0)
}

fn cargo_is_landed_on_assignment_island(
    world: &World,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    cargo: Entity,
) -> bool {
    if world
        .get::<crate::components::Transporting>(cargo)
        .is_some()
    {
        return false;
    }
    let Some(position) = world.get::<GridPosition>(cargo) else {
        return false;
    };
    world
        .get_resource::<crate::ai::islands::IslandMap>()
        .and_then(|map| map.get_island_at(position))
        .is_some_and(|island| island.id == assignment.island_id)
}

/// 現在地から作戦地点へ自力展開できるEntityかを判定する。
/// 航空戦力まで輸送cargoへ混ぜると、搭載不能なままFormingに滞留するため、
/// 輸送が必要な地上戦力との責務境界として用いる。
fn entity_can_self_deploy_to_assignment(
    world: &World,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    entity: Entity,
) -> bool {
    entity_can_self_deploy_to_assignment_with_connectivity(
        world,
        assignment,
        entity,
        &mut TerrainConnectivity::default(),
    )
}

fn entity_can_self_deploy_to_assignment_with_connectivity(
    world: &World,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    entity: Entity,
    connectivity: &mut TerrainConnectivity,
) -> bool {
    if world
        .get::<crate::components::Transporting>(entity)
        .is_some()
    {
        return false;
    }
    let (Some(position), Some(stats), Some(map), Some(registry)) = (
        world.get::<GridPosition>(entity),
        world.get::<UnitStats>(entity),
        world.get_resource::<Map>(),
        world.get_resource::<MasterDataRegistry>(),
    ) else {
        return false;
    };
    connectivity.is_reachable(
        map,
        registry,
        (position.x, position.y),
        (assignment.target_position.x, assignment.target_position.y),
        stats.movement_type,
    )
}

/// 島全体の必要数が未完成でも、実輸送役と搭載可能cargoが揃った便だけをPickupへ進める。
/// 残りは同じ島のFormingとして保持し、後続生産を止めずに逐次出航させる。
fn promote_partial_campaign_transport_wave(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
) -> bool {
    // Assaultも搭載・集合までは逐次進める。従来は完全編成までFormingに閉じ込め、
    // 輸送役とcargoを通常行動から除外したまま何ターンも停止させていた。
    // 発進可否は`departure_authorized`で別管理し、兵站gate前の逐次突入は防ぐ。
    if assignment.decision == crate::ai::island_campaign::IslandCampaignDecision::Assault
        && !crate::ai::resolve_player_ai_version(world, player_id)
            .uses_operation_driven_production()
    {
        return false;
    }
    let Some(index) = manager
        .proposal_view()
        .squads
        .iter()
        .enumerate()
        .filter(|(_, squad)| {
            squad_is_mutable_by_player(world, squad, player_id)
                && squad.plan.mission_type == MissionType::Transport
                && squad.plan.target_island == Some(assignment.island_id)
                && squad.plan.phase == MissionPhase::Forming
        })
        .min_by_key(|(_, squad)| squad.id.0)
        .map(|(index, _)| index)
    else {
        return false;
    };

    let forming = OperationLedger::squad_list(manager).remove(index);
    let assignment_transports: HashSet<_> = assignment.transport_entities.iter().copied().collect();
    let mut transports: Vec<_> = forming
        .members
        .iter()
        .filter(|entity| assignment_transports.contains(entity))
        .filter(|entity| {
            world
                .get::<crate::components::CargoCapacity>(**entity)
                .is_some()
        })
        .copied()
        .collect();
    transports.sort_by_key(|entity| entity.to_bits());
    transports.dedup();
    let mut remaining_cargo = forming.cargo_entities.clone();
    remaining_cargo.sort_by_key(|entity| entity.to_bits());
    remaining_cargo.dedup();
    if transports.is_empty() || remaining_cargo.is_empty() {
        OperationLedger::squad_list(manager).insert(index, forming);
        return false;
    }

    let mut connectivity = TerrainConnectivity::default();
    let mut launched_transports = HashSet::default();
    let mut waves = Vec::new();
    for transport in transports {
        let (Some(position), Some(stats), Some(capacity)) = (
            world.get::<GridPosition>(transport).copied(),
            world.get::<UnitStats>(transport),
            world.get::<crate::components::CargoCapacity>(transport),
        ) else {
            continue;
        };
        let mut cargo: Vec<_> = capacity
            .loaded
            .iter()
            .filter(|cargo| remaining_cargo.contains(cargo))
            .copied()
            .collect();
        let free_slots = (capacity.max as usize).saturating_sub(cargo.len());
        let already_selected: HashSet<_> = cargo.iter().copied().collect();
        let mut pickup_candidates: Vec<_> = remaining_cargo
            .iter()
            .filter(|candidate| !already_selected.contains(candidate))
            .filter(|candidate| {
                world
                    .get::<UnitStats>(**candidate)
                    .is_some_and(|cargo_stats| {
                        stats.loadable_unit_types.contains(&cargo_stats.unit_type)
                    })
            })
            .filter_map(|candidate| {
                cargo_pickup_rank(
                    world,
                    player_id,
                    position,
                    stats,
                    *candidate,
                    &mut connectivity,
                )
                .map(|rank| (rank, *candidate))
            })
            .collect();
        pickup_candidates.sort_by_key(|(rank, _)| *rank);
        cargo.extend(
            pickup_candidates
                .into_iter()
                .take(free_slots)
                .map(|(_, candidate)| candidate),
        );
        if cargo.is_empty() {
            continue;
        }
        let unloaded: Vec<_> = cargo
            .iter()
            .filter(|cargo| !capacity.loaded.contains(cargo))
            .copied()
            .collect();
        let pickup_position = if unloaded.is_empty() {
            position
        } else if let Some(position) = select_pickup_position(
            world,
            player_id,
            position,
            stats,
            &unloaded,
            &mut connectivity,
        ) {
            position
        } else {
            continue;
        };
        remaining_cargo.retain(|candidate| !cargo.contains(candidate));
        launched_transports.insert(transport);

        let mut wave = if waves.is_empty() {
            forming.clone()
        } else {
            manager.create_owned_squad(MissionType::Transport, player_id);
            OperationLedger::squad_list(manager).pop().unwrap()
        };
        OperationLedger::roster(&mut wave.edit()).rebind_transport_wave(transport, cargo);
        wave.plan.pickup_position = Some(pickup_position);
        // 先頭波はFormingの複製なので、旧降車位置を持ち越さない。
        wave.plan.drop_position = None;
        OperationLedger::roster(&mut wave.edit()).clear_delivered();
        wave.plan.phase = MissionPhase::Transport(if unloaded.is_empty() {
            TransportPhase::Transit
        } else {
            TransportPhase::Pickup
        });
        wave.plan.allow_partial_departure =
            assignment.decision != crate::ai::island_campaign::IslandCampaignDecision::Assault;
        wave.plan.departure_authorized = assignment.operation_ready
            || assignment.decision != crate::ai::island_campaign::IslandCampaignDecision::Assault;
        waves.push(wave);
    }
    if waves.is_empty() {
        OperationLedger::squad_list(manager).insert(index, forming);
        return false;
    }

    let mut remaining_transports: Vec<_> = forming
        .members
        .iter()
        .filter(|transport| !launched_transports.contains(transport))
        .copied()
        .collect();
    remaining_transports.sort_by_key(|entity| entity.to_bits());
    if !remaining_transports.is_empty() || !remaining_cargo.is_empty() {
        let id = manager
            .create_owned_squad(MissionType::Transport, player_id)
            .id;
        OperationLedger::proposals(manager).edit(id, |follow_up| {
            let mut roster = OperationLedger::roster(follow_up);
            roster.join_all(remaining_transports);
            roster.replace_cargo(remaining_cargo);
            let lead_transport = follow_up
                .members
                .iter()
                .min_by_key(|entity| entity.to_bits())
                .copied();
            OperationLedger::roster(follow_up).bind_transport(lead_transport);
            follow_up.plan_mut().target_island = forming.plan.target_island;
            follow_up.plan_mut().target = forming.plan.target;
            follow_up.plan_mut().allow_partial_departure =
                assignment.decision != crate::ai::island_campaign::IslandCampaignDecision::Assault;
            follow_up.plan_mut().departure_authorized = assignment.operation_ready
                || assignment.decision
                    != crate::ai::island_campaign::IslandCampaignDecision::Assault;
            follow_up.plan_mut().phase = MissionPhase::Forming;
        });
    }
    OperationLedger::squad_list(manager).extend(waves);
    OperationLedger::squad_list(manager).sort_by_id();
    true
}

/// portfolio assignmentごとにFormingを復元し、readyなら既存の安全なpartitionへ接続する。
#[cfg(test)]
fn prepare_campaign_transport_assignment(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
) -> bool {
    prepare_campaign_transport_assignment_with_connectivity(
        world,
        manager,
        player_id,
        assignment,
        &mut TerrainConnectivity::default(),
    )
}

fn prepare_campaign_transport_assignment_with_connectivity(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    connectivity: &mut TerrainConnectivity,
) -> bool {
    adopt_legacy_squad_owners(world, manager);
    for mut squad in manager.edit_each().filter(|squad| {
        squad_is_mutable_by_player(world, squad, player_id)
            && squad.plan.mission_type == MissionType::Transport
            && squad.plan.target_island == Some(assignment.island_id)
            && matches!(
                squad.plan.phase,
                MissionPhase::Forming
                    | MissionPhase::Transport(
                        TransportPhase::Pickup | TransportPhase::Transit | TransportPhase::Drop
                    )
            )
    }) {
        // 再分析で兵站施設が特定された場合、進行中便も島の代表座標ではなく
        // 空港・港・都市という具体的な作戦施設へ目標を同期する。
        squad.plan_mut().target = Some(assignment.target_position);
        squad.plan_mut().departure_authorized = assignment.operation_ready
            || assignment.decision != crate::ai::island_campaign::IslandCampaignDecision::Assault;
    }
    let assignment_transports: HashSet<_> = assignment.transport_entities.iter().copied().collect();
    let covered_transports: HashSet<_> = manager
        .proposal_view()
        .squads
        .iter()
        .filter(|squad| squad_is_mutable_by_player(world, squad, player_id))
        .filter(|squad| squad.plan.mission_type == MissionType::Transport)
        .filter(|squad| squad.plan.target_island == Some(assignment.island_id))
        .filter_map(|squad| *squad.transport_entity)
        .filter(|transport| assignment_transports.contains(transport))
        .collect();
    let forming_index = manager
        .proposal_view()
        .squads
        .iter()
        .enumerate()
        .filter(|(_, squad)| {
            squad_is_mutable_by_player(world, squad, player_id)
                && squad.plan.mission_type == MissionType::Transport
                && squad.plan.target_island == Some(assignment.island_id)
                && squad.plan.phase == MissionPhase::Forming
        })
        .min_by_key(|(_, squad)| squad.id.0)
        .map(|(index, _)| index);
    let uncovered_transports: Vec<_> = assignment
        .transport_entities
        .iter()
        .filter(|transport| !covered_transports.contains(transport))
        .copied()
        .collect();
    let already_owned: HashSet<_> = manager
        .proposal_view()
        .squads
        .iter()
        .filter(|squad| squad_is_mutable_by_player(world, squad, player_id))
        .filter(|squad| !squad.plan.return_after_combat)
        .filter(|squad| squad.plan.target_island == Some(assignment.island_id))
        .flat_map(|squad| {
            squad
                .members
                .iter()
                .chain(squad.cargo_entities.iter())
                .chain(squad.delivered_cargo.iter())
                .copied()
        })
        .collect();
    let mut forming_cargo: Vec<_> = assignment
        .capture_entities
        .iter()
        .chain(assignment.combat_entities.iter())
        .copied()
        .filter(|cargo| !already_owned.contains(cargo))
        .filter(|cargo| !cargo_is_landed_on_assignment_island(world, assignment, *cargo))
        .filter(|cargo| {
            !entity_can_self_deploy_to_assignment_with_connectivity(
                world,
                assignment,
                *cargo,
                connectivity,
            )
        })
        .collect();
    for transport in &uncovered_transports {
        if let Some(capacity) = world.get::<crate::components::CargoCapacity>(*transport) {
            forming_cargo.extend(
                capacity
                    .loaded
                    .iter()
                    .filter(|cargo| !already_owned.contains(cargo))
                    .copied(),
            );
        }
    }
    forming_cargo.sort_by_key(|entity| entity.to_bits());
    forming_cargo.dedup();

    // 期限に遅れた既存cargoも、輸送役を再生産するまでReserveへ戻さない。
    // ETA超過は作戦の予実差であり、作戦参加資格を失うハード条件ではない。
    let needs_forming = forming_index.is_some()
        || !assignment.operation_ready
        || !uncovered_transports.is_empty()
        || !forming_cargo.is_empty();

    if needs_forming {
        // 購入待ちで作った空placeholderへ、後のターンに完成した実Entityを必ず追加入隊させる。
        // placeholderが存在するだけで新規Entityを無所属にすると、汎用戦闘や迎撃へ流出する。
        let index = forming_index.unwrap_or_else(|| {
            manager.create_owned_squad(MissionType::Transport, player_id);
            manager.proposal_view().squads.len() - 1
        });
        let id = manager.proposal_view().squads[index].id;
        OperationLedger::proposals(manager).edit(id, |squad| {
            OperationLedger::roster(squad).join_all(uncovered_transports);
            if squad.transport_entity.is_none() {
                let lead_transport = squad
                    .members
                    .iter()
                    .filter(|entity| assignment_transports.contains(entity))
                    .min_by_key(|entity| entity.to_bits())
                    .copied();
                OperationLedger::roster(squad).bind_transport(lead_transport);
            }
            let mut roster = OperationLedger::roster(squad);
            roster.extend_cargo(forming_cargo);
            roster.sort_dedup_cargo();
            squad.plan_mut().target_island = Some(assignment.island_id);
            squad.plan_mut().target = Some(assignment.target_position);
            squad.plan_mut().allow_partial_departure = matches!(
                assignment.decision,
                crate::ai::island_campaign::IslandCampaignDecision::Expand
                    | crate::ai::island_campaign::IslandCampaignDecision::Secure
                    | crate::ai::island_campaign::IslandCampaignDecision::Contest
                    | crate::ai::island_campaign::IslandCampaignDecision::Reinforce
            );
            squad.plan_mut().departure_authorized = assignment.operation_ready
                || assignment.decision
                    != crate::ai::island_campaign::IslandCampaignDecision::Assault;
            squad.plan_mut().phase = MissionPhase::Forming;
        });
    }

    if !assignment.operation_ready
        && promote_partial_campaign_transport_wave(world, manager, player_id, assignment)
    {
        return false;
    }
    if assignment.operation_ready {
        reconcile_ready_forming_campaign_squad(world, manager, player_id, assignment)
    } else {
        false
    }
}

fn nearest_campaign_property_target(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    members: &[Entity],
) -> Option<GridPosition> {
    nearest_campaign_property_target_with_connectivity(
        world,
        player_id,
        island_id,
        members,
        &mut TerrainConnectivity::default(),
    )
}

fn nearest_campaign_property_target_with_connectivity(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    members: &[Entity],
    connectivity: &mut TerrainConnectivity,
) -> Option<GridPosition> {
    let island_map = world.get_resource::<crate::ai::islands::IslandMap>()?;
    let map = world.get_resource::<Map>()?;
    let registry = world.get_resource::<MasterDataRegistry>()?;
    world
        .iter_entities()
        .filter_map(|entity| {
            let target = entity.get::<GridPosition>().copied()?;
            let property = entity.get::<Property>()?;
            if property.owner_id == Some(player_id)
                || island_map
                    .get_island_at(&target)
                    .is_none_or(|island| island.id != island_id)
            {
                return None;
            }
            let distance = members
                .iter()
                .filter_map(|member| {
                    let position = world.get::<GridPosition>(*member)?;
                    let stats = world.get::<UnitStats>(*member)?;
                    connectivity
                        .is_reachable(
                            map,
                            registry,
                            (position.x, position.y),
                            (target.x, target.y),
                            stats.movement_type,
                        )
                        .then_some(position.x.abs_diff(target.x) + position.y.abs_diff(target.y))
                })
                .min()?;
            Some((distance, target.y, target.x, target))
        })
        .min_by_key(|candidate| (candidate.0, candidate.1, candidate.2))
        .map(|candidate| candidate.3)
}

fn campaign_assignment_capture_responsibilities(
    world: &World,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    members: &[Entity],
    connectivity: &mut TerrainConnectivity,
) -> Vec<CampaignResponsibility> {
    let mut targets: Vec<_> = assignment
        .capture_target_positions
        .iter()
        .copied()
        .filter(|target| {
            world.iter_entities().any(|entity| {
                entity.get::<GridPosition>() == Some(target)
                    && entity
                        .get::<Property>()
                        .is_some_and(|property| property.owner_id != Some(player_id))
            })
        })
        .collect();
    if targets.is_empty()
        && let Some(target) = nearest_campaign_property_target_with_connectivity(
            world,
            player_id,
            assignment.island_id,
            members,
            connectivity,
        )
    {
        targets.push(target);
    }

    let mut remaining = members.to_vec();
    remaining.sort_by_key(|entity| entity.to_bits());
    let mut responsibilities = Vec::new();
    for target in targets {
        let Some((index, _)) = remaining
            .iter()
            .enumerate()
            .filter_map(|(index, member)| {
                campaign_member_distance_to_position(world, *member, target, connectivity)
                    .map(|distance| (index, (distance, member.to_bits())))
            })
            .min_by_key(|(_, score)| *score)
        else {
            continue;
        };
        let member = remaining.remove(index);
        responsibilities.push(CampaignResponsibility {
            mission_type: MissionType::Capture,
            target,
            members: vec![member],
        });
        if remaining.is_empty() {
            break;
        }
    }

    // 施設数より占領要員が多い場合も遊兵化させず、島内の到達可能な未所有施設へ予備を送る。
    for member in remaining {
        if let Some(target) = nearest_campaign_property_target_with_connectivity(
            world,
            player_id,
            assignment.island_id,
            &[member],
            connectivity,
        ) {
            responsibilities.push(CampaignResponsibility {
                mission_type: MissionType::Capture,
                target,
                members: vec![member],
            });
        }
    }
    responsibilities
}

fn campaign_member_distance_to_position(
    world: &World,
    member: Entity,
    target: GridPosition,
    connectivity: &mut TerrainConnectivity,
) -> Option<usize> {
    let position = world.get::<GridPosition>(member)?;
    let stats = world.get::<UnitStats>(member)?;
    let map = world.get_resource::<Map>()?;
    let registry = world.get_resource::<MasterDataRegistry>()?;
    connectivity
        .is_reachable(
            map,
            registry,
            (position.x, position.y),
            (target.x, target.y),
            stats.movement_type,
        )
        .then_some(position.x.abs_diff(target.x) + position.y.abs_diff(target.y))
}

fn campaign_member_attack_distance(
    world: &World,
    member: Entity,
    enemy: Entity,
    enemy_position: GridPosition,
) -> Option<usize> {
    campaign_member_attack_distance_with_connectivity(
        world,
        member,
        enemy,
        enemy_position,
        &mut TerrainConnectivity::default(),
    )
}

fn campaign_member_attack_distance_with_connectivity(
    world: &World,
    member: Entity,
    enemy: Entity,
    enemy_position: GridPosition,
    connectivity: &mut TerrainConnectivity,
) -> Option<usize> {
    let member_position = world.get::<GridPosition>(member)?;
    let member_stats = world.get::<UnitStats>(member)?;
    let ammo = world.get::<Ammo>(member)?;
    let enemy_stats = world.get::<UnitStats>(enemy)?;
    let map = world.get_resource::<Map>()?;
    let registry = world.get_resource::<MasterDataRegistry>()?;
    let mut firing_positions = Vec::new();
    for y in 0..map.height {
        for x in 0..map.width {
            let terrain = map.get_terrain(x, y)?;
            if crate::systems::movement::get_valid_movement_cost(
                registry,
                member_stats.movement_type,
                terrain,
            )
            .is_none()
                || !connectivity.is_reachable(
                    map,
                    registry,
                    (member_position.x, member_position.y),
                    (x, y),
                    member_stats.movement_type,
                )
            {
                continue;
            }
            let firing_distance = map.distance(x, y, enemy_position.x, enemy_position.y);
            if crate::systems::combat::select_weapon(
                ammo.ammo1,
                ammo.ammo2,
                member_stats.unit_type.as_str(),
                enemy_stats.unit_type.as_str(),
                firing_distance,
                registry,
            )
            .is_none()
            {
                continue;
            }
            firing_positions.push((
                map.distance(member_position.x, member_position.y, x, y) as usize,
                y,
                x,
            ));
        }
    }
    firing_positions.sort_unstable();
    firing_positions.first().map(|position| position.0)
}

#[derive(Clone)]
pub(super) struct CampaignResponsibility {
    mission_type: MissionType,
    target: GridPosition,
    members: Vec<Entity>,
}

/// `advance_targets` は、敵へ到達できないmemberが前進する先の未所有前線施設（優先順）。
/// 空なら従来どおりmember位置での現地防御にまとめる。
fn campaign_combat_responsibilities(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    members: &[Entity],
    advance_targets: &[GridPosition],
    connectivity: &mut TerrainConnectivity,
) -> Vec<CampaignResponsibility> {
    let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() else {
        return Vec::new();
    };
    let mut enemies: Vec<_> = world
        .iter_entities()
        .filter_map(|entity| {
            let target = entity.get::<GridPosition>().copied()?;
            let faction = entity.get::<Faction>()?;
            (faction.0 != player_id
                && island_map
                    .get_island_at(&target)
                    .is_some_and(|island| island.id == island_id))
            .then_some((entity.id(), target))
        })
        .collect();
    enemies.sort_by_key(|(entity, target)| (target.y, target.x, entity.to_bits()));

    let mut remaining = members.to_vec();
    remaining.sort_by_key(|entity| entity.to_bits());
    remaining.dedup();
    let mut responsibilities = Vec::new();

    while !remaining.is_empty() {
        let mut candidates: Vec<_> = enemies
            .iter()
            .filter_map(|(enemy, target)| {
                let reachable: Vec<_> = remaining
                    .iter()
                    .filter_map(|member| {
                        campaign_member_attack_distance_with_connectivity(
                            world,
                            *member,
                            *enemy,
                            *target,
                            connectivity,
                        )
                        .map(|distance| (*member, distance))
                    })
                    .collect();
                if reachable.is_empty() {
                    return None;
                }
                let total_distance = reachable.iter().fold(0usize, |total, (_, distance)| {
                    total.saturating_add(*distance)
                });
                Some((*enemy, *target, reachable, total_distance))
            })
            .collect();
        if candidates.is_empty() {
            break;
        }
        // 全員が到達できる共通敵を最優先し、無ければ最多memberを受け持つ局所目標を選ぶ。
        candidates.sort_by_key(|(enemy, target, reachable, total_distance)| {
            (
                usize::MAX - reachable.len(),
                *total_distance,
                target.y,
                target.x,
                enemy.to_bits(),
            )
        });
        let (_, target, reachable, _) = candidates.remove(0);
        let assigned: Vec<_> = reachable.iter().map(|(member, _)| *member).collect();
        let assigned_set: HashSet<_> = assigned.iter().copied().collect();
        remaining.retain(|member| !assigned_set.contains(member));
        responsibilities.push(CampaignResponsibility {
            mission_type: MissionType::Attack,
            target,
            members: assigned,
        });
    }

    // 敵へ到達できないmemberを現在地へ留めると、占領兵が先へ出るまで誰も前進せず、
    // 次の拠点が前線として扱われない循環に陥る。前進先の施設へ届くmemberは、
    // その施設の防御（護衛）として先に送り出す。
    responsibilities.extend(campaign_advance_responsibilities(
        world,
        &mut remaining,
        advance_targets,
        connectivity,
    ));

    // 前進先へも届かないmemberは、全員が到達できるmember位置ごとに決定的に現地防御へまとめる。
    while !remaining.is_empty() {
        let mut hold_candidates: Vec<_> = remaining
            .iter()
            .filter_map(|source| {
                let target = world.get::<GridPosition>(*source).copied()?;
                let reachable: Vec<_> = remaining
                    .iter()
                    .filter_map(|member| {
                        campaign_member_distance_to_position(world, *member, target, connectivity)
                            .map(|distance| (*member, distance))
                    })
                    .collect();
                let total_distance = reachable.iter().fold(0usize, |total, (_, distance)| {
                    total.saturating_add(*distance)
                });
                Some((*source, target, reachable, total_distance))
            })
            .collect();
        hold_candidates.sort_by_key(|(source, target, reachable, total_distance)| {
            (
                usize::MAX - reachable.len(),
                *total_distance,
                target.y,
                target.x,
                source.to_bits(),
            )
        });
        let Some((_, target, reachable, _)) = hold_candidates.into_iter().next() else {
            break;
        };
        let assigned: Vec<_> = reachable.iter().map(|(member, _)| *member).collect();
        let assigned_set: HashSet<_> = assigned.iter().copied().collect();
        remaining.retain(|member| !assigned_set.contains(member));
        responsibilities.push(CampaignResponsibility {
            mission_type: MissionType::Defense,
            target,
            members: assigned,
        });
    }

    responsibilities.sort_by_key(|responsibility| {
        let mission_rank = match responsibility.mission_type {
            MissionType::Attack => 0,
            MissionType::Defense => 1,
            MissionType::Capture => 2,
            MissionType::Reserve => 3,
            MissionType::Transport => 4,
        };
        (
            mission_rank,
            responsibility.target.y,
            responsibility.target.x,
            responsibility
                .members
                .iter()
                .map(|entity| entity.to_bits())
                .min()
                .unwrap_or(u64::MAX),
        )
    });
    responsibilities
}

/// 敵へ到達できない戦闘memberを、到達できる最寄りの前進先施設ごとの防御責務へまとめる。
///
/// 各memberは自分から最も近い前進先へ向かう（同距離なら `advance_targets` の優先順）。
/// どの前進先へも到達できないmemberは `remaining` に残す。
fn campaign_advance_responsibilities(
    world: &World,
    remaining: &mut Vec<Entity>,
    advance_targets: &[GridPosition],
    connectivity: &mut TerrainConnectivity,
) -> Vec<CampaignResponsibility> {
    if advance_targets.is_empty() {
        return Vec::new();
    }
    let mut members_by_target: Vec<Vec<Entity>> = vec![Vec::new(); advance_targets.len()];
    remaining.retain(|member| {
        let nearest = advance_targets
            .iter()
            .enumerate()
            .filter_map(|(index, target)| {
                campaign_member_distance_to_position(world, *member, *target, connectivity)
                    .map(|distance| (distance, index))
            })
            .min();
        let Some((_, index)) = nearest else {
            return true;
        };
        members_by_target[index].push(*member);
        false
    });
    advance_targets
        .iter()
        .zip(members_by_target)
        .filter(|(_, members)| !members.is_empty())
        .map(|(target, members)| CampaignResponsibility {
            mission_type: MissionType::Defense,
            target: *target,
            members,
        })
        .collect()
}

/// 通常の再計画と、正本で特定したEntityだけの再接続を分ける。
#[derive(Clone, Copy)]
enum CampaignAssignmentScope<'a> {
    Full,
    Reconnect(&'a HashSet<Entity>),
}
impl CampaignAssignmentScope<'_> {
    /// 再接続するEntityと、現在任務にいる護衛の読取り文脈を混同しない。
    fn synchronize_departure(
        self,
        world: &World,
        manager: &mut SquadManager,
        player: PlayerId,
        assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
        connectivity: &mut TerrainConnectivity,
    ) {
        let Self::Reconnect(pending) = self else {
            synchronize_local_capture_departure(world, manager, player, assignment, connectivity);
            return;
        };
        let owner = OperationOwner::Campaign {
            player_id: player,
            island_id: assignment.island_id,
        };
        let registry = world.get_resource::<UnitOperationRegistry>();
        let mut escorts = HashSet::default();
        for squad in manager.proposal_view().squads.iter().filter(|squad| {
            matches!(
                squad.plan.mission_type,
                MissionType::Attack | MissionType::Defense
            ) && squad_is_mutable_by_player(world, squad, player)
        }) {
            for entity in &squad.members {
                if registry
                    .and_then(|r| r.assignment(*entity))
                    .is_some_and(|current| {
                        current.owner == owner
                            && ((current.squad_id == Some(squad.id)
                                && current.role == OperationUnitRole::Member)
                                || pending.contains(entity))
                    })
                {
                    escorts.insert(*entity);
                }
            }
        }
        let mut context = assignment.clone();
        context.combat_entities = escorts.into_iter().collect();
        context
            .combat_entities
            .sort_by_key(|entity| entity.to_bits());
        synchronize_local_capture_departure(world, manager, player, &context, connectivity);
    }

    fn assign(
        self,
        world: &World,
        manager: &mut SquadManager,
        player: PlayerId,
        island: crate::ai::islands::IslandId,
        responsibilities: Vec<CampaignResponsibility>,
        missions: &[MissionType],
    ) {
        assign_campaign_responsibilities_scoped(
            world,
            manager,
            player,
            island,
            responsibilities,
            missions,
            self,
        );
    }
}

pub(super) fn assign_campaign_responsibilities(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    responsibilities: Vec<CampaignResponsibility>,
    managed_missions: &[MissionType],
) {
    CampaignAssignmentScope::Full.assign(
        world,
        manager,
        player_id,
        island_id,
        responsibilities,
        managed_missions,
    );
}

fn assign_campaign_responsibilities_scoped(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    mut responsibilities: Vec<CampaignResponsibility>,
    managed_missions: &[MissionType],
    scope: CampaignAssignmentScope<'_>,
) {
    // 生産目的が続くSquadはcampaign再編の受入先に転用せず、空になっても解散しない。
    // 後段の配備接続が同じidentityへ差分を戻すため、新しい孤立Entityを生まない。
    let deployment_ids = world
        .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
        .map(|registry| registry.protected_squads(player_id))
        .unwrap_or_default();
    // 他の現在bindingを持つSquadを受入先・解散対象にせず、既存任務を維持する。
    let fixed_ids = match scope {
        CampaignAssignmentScope::Full => HashSet::default(),
        CampaignAssignmentScope::Reconnect(pending) => {
            for responsibility in &mut responsibilities {
                responsibility
                    .members
                    .retain(|entity| pending.contains(entity));
            }
            manager
                .proposal_view()
                .squads
                .iter()
                .filter(|squad| {
                    squad
                        .members
                        .iter()
                        .chain(squad.cargo_entities.iter())
                        .chain(squad.delivered_cargo.iter())
                        .chain(squad.transport_entity.iter())
                        .any(|entity| {
                            !pending.contains(entity)
                                && world
                                    .get_resource::<UnitOperationRegistry>()
                                    .and_then(|registry| registry.assignment(*entity))
                                    .is_some_and(|binding| binding.squad_id == Some(squad.id))
                        })
                })
                .map(|squad| squad.id)
                .collect()
        }
    };
    for responsibility in &mut responsibilities {
        responsibility
            .members
            .sort_by_key(|entity| entity.to_bits());
        responsibility.members.dedup();
    }
    responsibilities.retain(|responsibility| !responsibility.members.is_empty());

    let all_members: HashSet<_> = responsibilities
        .iter()
        .flat_map(|responsibility| responsibility.members.iter().copied())
        .collect();
    let mut selected_ids = Vec::with_capacity(responsibilities.len());
    let mut used_ids = HashSet::default();
    for responsibility in &responsibilities {
        let member_set: HashSet<_> = responsibility.members.iter().copied().collect();
        let existing_id = manager
            .proposal_view()
            .squads
            .iter()
            .filter(|squad| {
                !used_ids.contains(&squad.id)
                    && !deployment_ids.contains(&squad.id)
                    && !fixed_ids.contains(&squad.id)
            })
            .filter(|squad| squad_is_mutable_by_player(world, squad, player_id))
            .filter(|squad| squad.plan.mission_type == responsibility.mission_type)
            .filter(|squad| squad.plan.target_island == Some(island_id))
            .min_by_key(|squad| {
                let compatibility = if squad.plan.target == Some(responsibility.target) {
                    0
                } else if squad
                    .members
                    .iter()
                    .any(|member| member_set.contains(member))
                {
                    1
                } else {
                    2
                };
                (compatibility, squad.id.0)
            })
            .map(|squad| squad.id);
        let selected_id = existing_id.unwrap_or_else(|| {
            manager
                .create_owned_squad(responsibility.mission_type.clone(), player_id)
                .id
        });
        used_ids.insert(selected_id);
        selected_ids.push(selected_id);
    }

    // 一覧順のIDを先に確定し、責務移管をmanager単位の提案storeへ適用する。
    let mutable_ids = manager
        .proposal_view()
        .squads
        .iter()
        .filter(|squad| {
            squad_is_mutable_by_player(world, squad, player_id)
                && !deployment_ids.contains(&squad.id)
        })
        .map(|squad| squad.id)
        .collect::<Vec<_>>();
    {
        let mut proposals = OperationLedger::proposals(manager);
        // 旧参照の除去→各partitionの登録という順序と、輸送役bindingの保持を維持する。
        for id in mutable_ids {
            proposals.remove_mentions(id, &all_members);
        }
        for (responsibility, selected_id) in responsibilities.iter().zip(&selected_ids) {
            proposals.replace_members(
                *selected_id,
                responsibility.members.iter().copied().collect(),
            );
            let mut squad = proposals
                .squad_mut(*selected_id)
                .expect("選択済みの責務Squad");
            squad.plan_mut().mission_type = responsibility.mission_type.clone();
            squad.plan_mut().target_island = Some(island_id);
            squad.plan_mut().target = Some(responsibility.target);
            if squad.plan.phase == MissionPhase::Completed {
                squad.plan_mut().phase = MissionPhase::Forming;
            }
        }
    }
    let selected_set: HashSet<_> = selected_ids.into_iter().collect();
    OperationLedger::squad_list(manager).retain(|squad| {
        selected_set.contains(&squad.id)
            || fixed_ids.contains(&squad.id)
            || deployment_ids.contains(&squad.id)
            || !squad_is_mutable_by_player(world, squad, player_id)
            || squad.plan.return_after_combat
            || squad.plan.target_island != Some(island_id)
            || !managed_missions.contains(&squad.plan.mission_type)
    });
    OperationLedger::squad_list(manager).sort_by_id();
}

fn assign_campaign_members(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    mission_type: MissionType,
    island_id: crate::ai::islands::IslandId,
    target: GridPosition,
    members: Vec<Entity>,
) {
    if members.is_empty() {
        return;
    }
    assign_campaign_responsibilities(
        world,
        manager,
        player_id,
        island_id,
        vec![CampaignResponsibility {
            mission_type: mission_type.clone(),
            target,
            members,
        }],
        &[mission_type],
    );
}

#[cfg(test)]
fn prepare_campaign_local_assignment(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
) {
    prepare_campaign_local_assignment_with_connectivity(
        world,
        manager,
        player_id,
        assignment,
        &mut TerrainConnectivity::default(),
    );
}

fn prepare_campaign_local_assignment_with_connectivity(
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

fn prepare_campaign_local_assignment_scoped(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    connectivity: &mut TerrainConnectivity,
    scope: CampaignAssignmentScope<'_>,
) {
    // 小規模マップのV4は extended 側の実装を使う。
    if crate::ai::code_path::is_extended() {
        return local_assignment::prepare_campaign_local_assignment_scoped(
            world,
            manager,
            player_id,
            assignment,
            connectivity,
            scope,
        );
    }
    use crate::ai::island_campaign::IslandCampaignDecision;

    let local_entities = |entities: &[Entity]| {
        entities
            .iter()
            .copied()
            .filter(|entity| cargo_is_landed_on_assignment_island(world, assignment, *entity))
            .collect::<Vec<_>>()
    };
    match assignment.decision {
        IslandCampaignDecision::Defend => {
            // 島が脅威下でも未所有施設の占領は止めない。占領可能Entityだけを
            // Capture責務へ固定し、残りを局地敵の排除または拠点防衛へ分割する。
            let capture = local_entities(&assignment.capture_entities);
            let capture_responsibilities = campaign_assignment_capture_responsibilities(
                world,
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
            // 防衛島では、敵へ届かない戦力を下の分岐で主要施設へ集結させる。
            let mut combat_responsibilities = campaign_combat_responsibilities(
                world,
                player_id,
                assignment.island_id,
                &combat,
                &[],
                connectivity,
            );
            if !combat.is_empty()
                && combat_responsibilities
                    .iter()
                    .all(|responsibility| responsibility.mission_type == MissionType::Defense)
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
        IslandCampaignDecision::Secure => {
            let capture = local_entities(&assignment.capture_entities);
            let responsibilities = campaign_assignment_capture_responsibilities(
                world,
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
        }
        IslandCampaignDecision::Expand
        | IslandCampaignDecision::Assault
        | IslandCampaignDecision::Contest
        | IslandCampaignDecision::Reinforce => {
            let capture = local_entities(&assignment.capture_entities);
            let capture_responsibilities = campaign_assignment_capture_responsibilities(
                world,
                player_id,
                assignment,
                &capture,
                connectivity,
            );
            // 敵へ届かない戦闘戦力は、占領兵が向かう施設、次に前線帯の未所有施設へ前進させる。
            let mut advance_targets = capture_responsibilities
                .iter()
                .map(|responsibility| responsibility.target)
                .chain(assignment.capture_target_positions.iter().copied())
                .chain(std::iter::once(assignment.target_position))
                .collect::<Vec<_>>();
            // 施設が尽きた島の目標は任意タイルになり得るため、未所有の施設だけを前進先にする。
            let unowned_properties = world
                .iter_entities()
                .filter_map(|entity| {
                    let property = entity.get::<Property>()?;
                    (property.owner_id != Some(player_id))
                        .then(|| entity.get::<GridPosition>().copied())
                        .flatten()
                })
                .collect::<HashSet<_>>();
            let mut seen_targets = HashSet::default();
            advance_targets.retain(|target| {
                unowned_properties.contains(target) && seen_targets.insert(*target)
            });
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
            let responsibilities = campaign_combat_responsibilities(
                world,
                player_id,
                assignment.island_id,
                &combat,
                &advance_targets,
                connectivity,
            );
            scope.assign(
                world,
                manager,
                player_id,
                assignment.island_id,
                responsibilities,
                &[MissionType::Attack, MissionType::Defense],
            );
        }
        _ => {}
    }
    scope.synchronize_departure(world, manager, player_id, assignment, connectivity);
}

/// 局地Captureは首都本隊の完全編成を待たない。ただし占領完了前に接触できる敵が
/// 実在する場合だけ、各敵へ有効打を持つ護衛が占領兵の到着から1手番以内に合流できる
/// ことを出発条件にする。安全な目標は占領兵単独で即時実行する。
fn synchronize_local_capture_departure(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
    connectivity: &mut TerrainConnectivity,
) {
    // 小規模マップ展開の即時出撃（H01の出撃方策）は拡張経路だけで使う。
    let departs_immediately_on_small_map = crate::ai::code_path::is_extended()
        && crate::ai::strategy_profile::current_profile(world, player_id)
            == crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion;
    if crate::ai::resolve_player_ai_version(world, player_id)
        != crate::ai::ai_version::AiVersion::V4
    {
        return;
    }
    let Some(map) = world.get_resource::<Map>() else {
        return;
    };
    let Some(registry) = world.get_resource::<MasterDataRegistry>() else {
        return;
    };
    let Some(damage_chart) = world.get_resource::<crate::resources::DamageChart>() else {
        return;
    };
    let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() else {
        return;
    };
    let enemies = world
        .iter_entities()
        .filter_map(|entity| {
            let faction = entity.get::<Faction>()?;
            let position = entity.get::<GridPosition>()?;
            let stats = entity.get::<UnitStats>()?;
            (faction.0 != player_id
                && entity.get::<crate::components::Transporting>().is_none()
                && island_map
                    .get_island_at(position)
                    .is_some_and(|island| island.id == assignment.island_id))
            .then_some((*position, stats.clone()))
        })
        .collect::<Vec<_>>();
    let property_capture_points = world
        .iter_entities()
        .filter_map(|entity| {
            Some((
                *entity.get::<GridPosition>()?,
                entity.get::<Property>()?.capture_points,
            ))
        })
        .collect::<HashMap<_, _>>();
    let escorts = local_capture_escort_candidates(world, manager, player_id, assignment);

    for mut squad in manager.edit_each() {
        if squad.plan.owner_id != Some(player_id)
            || squad.plan.mission_type != MissionType::Capture
            || squad.plan.target_island != Some(assignment.island_id)
        {
            continue;
        }
        let Some(target) = squad.plan.target else {
            squad.plan_mut().departure_authorized = false;
            continue;
        };
        // 小規模マップの並列展開では、複数の物件レースを同時に開き続ける。
        // 護衛戦闘部隊の完成を待って工場・待機地に居座ると、中立物件を敵に先取され
        // 序盤収入を覆せなくなるため、到達可能な全物件への即時出撃を認める。
        if departs_immediately_on_small_map {
            squad.plan_mut().departure_authorized = true;
            continue;
        }
        squad.plan_mut().departure_authorized = squad.members.iter().all(|capturer| {
            let Some(capturer_position) = world.get::<GridPosition>(*capturer) else {
                return false;
            };
            let Some(capturer_stats) = world.get::<UnitStats>(*capturer) else {
                return false;
            };
            let capturer_hp = world
                .get::<Health>(*capturer)
                .map_or(100, |health| health.current);
            let capture_arrival = map
                .distance(capturer_position.x, capturer_position.y, target.x, target.y)
                .div_ceil(capturer_stats.max_movement.max(1));
            let occupation_turns =
                property_capture_points
                    .get(&target)
                    .map_or(2, |capture_points| {
                        let capture_power = capturer_hp.saturating_add(9) / 10 * 10;
                        capture_points.div_ceil(capture_power.max(1))
                    });
            let capture_completion = capture_arrival.saturating_add(occupation_turns);
            let threats = enemies
                .iter()
                .filter(|(enemy_position, enemy_stats)| {
                    let damage = damage_chart
                        .get_base_damage(enemy_stats.unit_type, capturer_stats.unit_type)
                        .unwrap_or(0)
                        .max(
                            damage_chart
                                .get_base_damage_secondary(
                                    enemy_stats.unit_type,
                                    capturer_stats.unit_type,
                                )
                                .unwrap_or(0),
                        );
                    if damage == 0
                        || !connectivity.is_reachable(
                            map,
                            registry,
                            (enemy_position.x, enemy_position.y),
                            (target.x, target.y),
                            enemy_stats.movement_type,
                        )
                    {
                        return false;
                    }
                    let contact_eta = map
                        .distance(enemy_position.x, enemy_position.y, target.x, target.y)
                        .saturating_sub(enemy_stats.max_range.max(1))
                        .div_ceil(enemy_stats.max_movement.max(1));
                    contact_eta <= capture_completion
                })
                .collect::<Vec<_>>();
            threats.is_empty()
                || threats.iter().all(|(_, enemy_stats)| {
                    escorts.iter().any(|escort| {
                        let (Some(position), Some(stats)) = (
                            world.get::<GridPosition>(*escort),
                            world.get::<UnitStats>(*escort),
                        ) else {
                            return false;
                        };
                        if world
                            .get::<crate::components::Transporting>(*escort)
                            .is_some()
                            || !connectivity.is_reachable(
                                map,
                                registry,
                                (position.x, position.y),
                                (target.x, target.y),
                                stats.movement_type,
                            )
                        {
                            return false;
                        }
                        let damage = damage_chart
                            .get_base_damage(stats.unit_type, enemy_stats.unit_type)
                            .unwrap_or(0)
                            .max(
                                damage_chart
                                    .get_base_damage_secondary(
                                        stats.unit_type,
                                        enemy_stats.unit_type,
                                    )
                                    .unwrap_or(0),
                            );
                        let escort_eta = map
                            .distance(position.x, position.y, target.x, target.y)
                            .div_ceil(stats.max_movement.max(1));
                        damage > 0 && escort_eta <= capture_arrival.saturating_add(1)
                    })
                })
        });
    }
}

/// 局地Captureの出発判定で護衛として数える自軍戦闘Entity。
///
/// campaignへ割り当てた戦闘Entityだけでは、V4が作戦用に生産した配備台帳の戦力や、
/// 既存のAttack/Defense Squadの戦力が護衛から漏れる。盤上に味方の戦闘部隊がいても
/// 「護衛不足」と判定し、占領兵が生産施設前から出発できなくなるため、それらも候補に含める。
/// 到達可否・有効打・合流ETAは呼出側が個別に判定する。
fn local_capture_escort_candidates(
    world: &World,
    manager: &SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
) -> Vec<Entity> {
    let deployment_entities = world
        .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
        .map(|registry| registry.active_entities(player_id))
        .unwrap_or_default();
    let squad_entities = manager
        .proposal_view()
        .squads
        .iter()
        .filter(|squad| {
            squad.plan.owner_id == Some(player_id)
                && matches!(
                    squad.plan.mission_type,
                    MissionType::Attack | MissionType::Defense
                )
        })
        .flat_map(|squad| squad.members.iter().copied());
    // 追加分は占領兵自身を護衛と数えないよう、占領できないEntityに限る。
    let additional = deployment_entities
        .into_iter()
        .chain(squad_entities)
        .filter(|entity| {
            world
                .get::<UnitStats>(*entity)
                .is_some_and(|stats| !stats.can_capture)
        });
    let mut escorts = assignment
        .combat_entities
        .iter()
        .copied()
        .chain(additional)
        .collect::<Vec<_>>();
    escorts.sort_unstable_by_key(|entity| entity.to_bits());
    escorts.dedup();
    escorts
}

fn prepare_secure_local_captures(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    portfolio: &crate::ai::island_campaign::IslandCampaignPortfolio,
) -> HashSet<Entity> {
    use crate::ai::island_campaign::{IslandCampaignDecision, campaign_unit_type_rank};

    let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() else {
        return HashSet::default();
    };
    let mut secure_islands: Vec<_> = portfolio
        .islands
        .iter()
        .filter(|assessment| assessment.decision == IslandCampaignDecision::Secure)
        .filter(|assessment| portfolio.assignment_for(assessment.island_id).is_none())
        .map(|assessment| assessment.island_id)
        .collect();
    secure_islands.sort_by_key(|island| island.0);
    secure_islands.dedup();
    let mut protected = HashSet::default();
    for island_id in secure_islands {
        let mut candidates: Vec<_> = world
            .iter_entities()
            .filter_map(|entity_ref| {
                let entity = entity_ref.id();
                let faction = entity_ref.get::<Faction>()?;
                let position = entity_ref.get::<GridPosition>()?;
                let stats = entity_ref.get::<UnitStats>()?;
                if faction.0 != player_id
                    || !stats.can_capture
                    || entity_ref
                        .get::<crate::components::Transporting>()
                        .is_some()
                    || island_map
                        .get_island_at(position)
                        .is_none_or(|island| island.id != island_id)
                    || manager.proposal_view().squads.iter().any(|squad| {
                        let references_entity = squad.members.contains(&entity)
                            || squad.cargo_entities.contains(&entity)
                            || squad.delivered_cargo.contains(&entity);
                        references_entity && !squad_is_mutable_by_player(world, squad, player_id)
                    })
                {
                    return None;
                }
                let target =
                    nearest_campaign_property_target(world, player_id, island_id, &[entity]);
                Some((
                    stats.cost,
                    campaign_unit_type_rank(stats.unit_type),
                    entity.to_bits(),
                    entity,
                    target,
                ))
            })
            .collect();
        candidates.sort_by_key(|candidate| (candidate.0, candidate.1, candidate.2));
        let selected = candidates
            .iter()
            .find(|candidate| candidate.4.is_some())
            .or_else(|| candidates.first())
            .copied();
        if let Some((_, _, _, entity, target)) = selected {
            // 到達可否とは独立にallocator選択Entityを保護し、他島輸送へ戻さない。
            protected.insert(entity);
            if let Some(target) = target {
                assign_campaign_members(
                    world,
                    manager,
                    player_id,
                    MissionType::Capture,
                    island_id,
                    target,
                    vec![entity],
                );
            } else {
                let ids = manager
                    .proposal_view()
                    .squads
                    .iter()
                    .filter(|squad| squad_is_mutable_by_player(world, squad, player_id))
                    .map(|squad| squad.id)
                    .collect::<Vec<_>>();
                let mut proposals = OperationLedger::proposals(manager);
                for id in ids {
                    proposals.release_where(id, |other| *other == entity);
                }
            }
        }
    }
    protected
}

fn campaign_paused_islands(
    portfolio: &crate::ai::island_campaign::IslandCampaignPortfolio,
) -> HashSet<crate::ai::islands::IslandId> {
    use crate::ai::island_campaign::IslandCampaignDecision;

    portfolio
        .islands
        .iter()
        .filter(|assessment| {
            assessment.decision == IslandCampaignDecision::Withdraw
                || assessment.pause_cause.is_some()
        })
        .map(|assessment| assessment.island_id)
        .collect()
}

fn remove_abandoned_campaign_placeholders(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    active_islands: &HashSet<crate::ai::islands::IslandId>,
) {
    OperationLedger::squad_list(manager).retain(|squad| {
        let is_empty_placeholder = squad.plan.owner_id == Some(player_id)
            && squad.plan.mission_type == MissionType::Transport
            && squad.plan.phase == MissionPhase::Forming
            && squad.transport_entity.is_none()
            && squad.members.is_empty()
            && squad.cargo_entities.is_empty()
            && squad.delivered_cargo.is_empty()
            && squad.plan.target_island.is_some();
        !is_empty_placeholder
            || !squad_is_mutable_by_player(world, squad, player_id)
            || squad
                .plan
                .target_island
                .is_some_and(|island| active_islands.contains(&island))
    });
}

fn apply_campaign_pauses(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    paused_islands: &HashSet<crate::ai::islands::IslandId>,
) {
    if paused_islands.is_empty() {
        return;
    }

    let ids = manager
        .proposal_view()
        .squads
        .iter()
        .map(|squad| squad.id)
        .collect::<Vec<_>>();
    let mut removed = HashSet::default();
    for id in ids {
        let keep = OperationLedger::proposals(manager)
            .edit(id, |squad| {
                if !squad_is_mutable_by_player(world, squad, player_id)
                    || squad.plan.mission_type != MissionType::Transport
                    || squad
                        .plan
                        .target_island
                        .is_none_or(|island| !paused_islands.contains(&island))
                {
                    return true;
                }
                let Some(transport) = *squad.transport_entity else {
                    // 輸送役を特定できないidle Formingは解放し、現地通常部隊には触れない。
                    return squad.plan.phase != MissionPhase::Forming;
                };
                if world
                    .get::<Faction>(transport)
                    .is_none_or(|faction| faction.0 != player_id)
                {
                    return true;
                }

                match squad.plan.phase {
                    MissionPhase::Forming | MissionPhase::Transport(TransportPhase::Pickup) => {
                        let mut loaded = world
                            .get::<crate::components::CargoCapacity>(transport)
                            .map(|capacity| capacity.loaded.clone())
                            .unwrap_or_default();
                        loaded.sort_by_key(|entity| entity.to_bits());
                        loaded.dedup();
                        if loaded.is_empty() {
                            // 未搭載の輸送役・待機unitはSquad所有から解放し、逆向きのPickupを作らない。
                            return false;
                        }
                        // 輸送役bindingは同じ輸送役のまま、搭載済みcargoだけの降車便へ置き換える。
                        OperationLedger::roster(squad).rebind_transport_wave(transport, loaded);
                        squad.plan_mut().target_island = None;
                        squad.plan_mut().target = None;
                        squad.plan_mut().pickup_position = None;
                        squad.plan_mut().drop_position = None;
                        squad.plan_mut().phase = MissionPhase::Transport(TransportPhase::Drop);
                        true
                    }
                    MissionPhase::Transport(TransportPhase::Return) | MissionPhase::Completed => {
                        false
                    }
                    MissionPhase::Transport(TransportPhase::Transit | TransportPhase::Drop) => true,
                    MissionPhase::MovingToTarget | MissionPhase::Executing => true,
                }
            })
            .unwrap_or(true);
        if !keep {
            removed.insert(id);
        }
    }
    OperationLedger::squad_list(manager).retain(|squad| !removed.contains(&squad.id));
}

/// すべての通常作戦を構築した後に、まだ参照されていない実Entityへ明示的な
/// 予備配置を与える。全unit・全Squadを各1回走査し、行動ごとの全なめは行わない。
fn assign_explicit_reserve_squads(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    campaign_assignments: &[crate::ai::island_campaign::IslandCampaignAssignment],
) {
    let Some(map) = world.get_resource::<Map>().cloned() else {
        return;
    };
    let Some(registry) = world.get_resource::<MasterDataRegistry>().cloned() else {
        return;
    };
    let damage_chart = world
        .get_resource::<crate::resources::DamageChart>()
        .cloned();
    // SoloFallback は「Squadを組めなかった」という旧経路の印であり、実行任務の
    // 所有を保証しない。ここで割当済み扱いにすると、Registry ownerだけを持つ
    // EntityがReserveへ再接続されず、行動可能なまま手番を終えてしまう。
    // 固定点の正規化直後に呼ばれるため、所属は正本の現在bindingから読む（rosterを逆算しない）。
    let assigned = allocation_unavailable_entities(world, manager, player_id);

    let mut campaign_by_entity = HashMap::default();
    for assignment in campaign_assignments {
        for entity in assignment
            .transport_entities
            .iter()
            .chain(assignment.capture_entities.iter())
            .chain(assignment.combat_entities.iter())
        {
            campaign_by_entity.insert(*entity, (assignment.island_id, assignment.target_position));
        }
    }
    // deployment等がportfolioの当手番割当から外れていても、正本registryに
    // campaign ownerが残るEntityは同じownerのReserveへ接続する。Tactical Reserveに
    // すると排他正規化がcampaign所有を優先してSquad参照だけを落としてしまう。
    let prior_campaign_assignments = world
        .get_resource::<UnitOperationRegistry>()
        .map(|registry| registry.campaign_entity_assignments(player_id))
        .unwrap_or_default();
    for (entity, island_id) in prior_campaign_assignments {
        campaign_by_entity.entry(entity).or_insert_with(|| {
            let target = campaign_assignments
                .iter()
                .find(|assignment| assignment.island_id == island_id)
                .map(|assignment| assignment.target_position)
                .or_else(|| {
                    world
                        .get_resource::<crate::ai::islands::IslandMap>()
                        .and_then(|islands| {
                            islands.islands.iter().find(|island| island.id == island_id)
                        })
                        .and_then(|island| {
                            island
                                .tiles
                                .iter()
                                .copied()
                                .min_by_key(|position| (position.y, position.x))
                        })
                })
                .unwrap_or(GridPosition { x: 0, y: 0 });
            (island_id, target)
        });
    }

    let mut occupied = HashSet::default();
    let mut units = Vec::new();
    let mut enemies = Vec::new();
    let mut unit_query = world.query::<(
        Entity,
        &Faction,
        &GridPosition,
        &UnitStats,
        Option<&crate::components::Transporting>,
    )>();
    for (entity, faction, position, stats, transporting) in unit_query.iter(world) {
        if transporting.is_none() {
            occupied.insert((position.x, position.y));
        }
        if faction.0 == player_id {
            if transporting.is_none() && !assigned.contains(&entity) {
                units.push((entity, *position, stats.clone()));
            }
        } else if transporting.is_none() {
            enemies.push((*position, stats.clone()));
        }
    }
    units.sort_by_key(|(entity, _, _)| entity.to_bits());

    let mut owned_properties = Vec::new();
    let mut production_positions = HashSet::default();
    let mut property_query = world.query::<(&GridPosition, &Property)>();
    for (position, property) in property_query.iter(world) {
        if property.owner_id != Some(player_id) {
            continue;
        }
        owned_properties.push((*position, property.terrain));
        if registry.is_production_facility(property.terrain.as_str()) {
            production_positions.insert((position.x, position.y));
        }
    }
    owned_properties.sort_by_key(|(position, _)| (position.y, position.x));

    // 地形連結成分は盤面と移動種別だけで決まる。Reserve候補・敵・目標ごとに
    // flood fillを作り直さず、この再配置パス全体で移動種別別の索引を共有する。
    let mut connectivity = TerrainConnectivity::default();
    for (entity, position, stats) in units {
        let campaign_binding = campaign_by_entity.get(&entity).copied();

        // 戦闘艦など目標座標そのものへ入れないunitは、敵座標ではなく
        // 射程内の到達可能マスを予備配置地点にする。
        let mut engagement_target = None;
        let mut engagement_rank = None;
        if stats.max_cargo == 0 && stats.movement_type == crate::resources::MovementType::Ship {
            for (enemy_position, enemy_stats) in &enemies {
                let can_damage = damage_chart.as_ref().is_some_and(|chart| {
                    chart
                        .get_base_damage(stats.unit_type, enemy_stats.unit_type)
                        .unwrap_or(0)
                        .max(
                            chart
                                .get_base_damage_secondary(stats.unit_type, enemy_stats.unit_type)
                                .unwrap_or(0),
                        )
                        > 0
                });
                if !can_damage {
                    continue;
                }
                for y in 0..map.height {
                    for x in 0..map.width {
                        if map.distance(x, y, enemy_position.x, enemy_position.y)
                            > stats.max_range.max(1)
                            || map.get_terrain(x, y).is_none_or(|terrain| {
                                crate::systems::movement::get_valid_movement_cost(
                                    &registry,
                                    stats.movement_type,
                                    terrain,
                                )
                                .is_none()
                            })
                            || !connectivity.is_reachable(
                                &map,
                                &registry,
                                (position.x, position.y),
                                (x, y),
                                stats.movement_type,
                            )
                        {
                            continue;
                        }
                        let rank = (map.distance(position.x, position.y, x, y), y, x);
                        if engagement_rank.is_none_or(|current| rank < current) {
                            engagement_rank = Some(rank);
                            engagement_target = Some(GridPosition { x, y });
                        }
                    }
                }
            }
        }

        let campaign_target = campaign_binding.map(|(_, target)| target).filter(|target| {
            connectivity.is_reachable(
                &map,
                &registry,
                (position.x, position.y),
                (target.x, target.y),
                stats.movement_type,
            )
        });
        let active_target = campaign_assignments
            .iter()
            .filter(|assignment| {
                connectivity.is_reachable(
                    &map,
                    &registry,
                    (position.x, position.y),
                    (assignment.target_position.x, assignment.target_position.y),
                    stats.movement_type,
                )
            })
            .min_by_key(|assignment| {
                (
                    campaign_assignment_priority(assignment),
                    map.distance(
                        position.x,
                        position.y,
                        assignment.target_position.x,
                        assignment.target_position.y,
                    ),
                )
            })
            .map(|assignment| assignment.target_position);
        let recovery_target = owned_properties
            .iter()
            .filter(|(_, terrain)| registry.can_repair_on_terrain(stats.unit_type, *terrain))
            .filter(|(target, _)| {
                connectivity.is_reachable(
                    &map,
                    &registry,
                    (position.x, position.y),
                    (target.x, target.y),
                    stats.movement_type,
                )
            })
            .min_by_key(|(target, _)| {
                (
                    map.distance(position.x, position.y, target.x, target.y),
                    target.y,
                    target.x,
                )
            })
            .map(|(target, _)| *target);
        let mut target = engagement_target
            .or(campaign_target)
            .or(active_target)
            .or(recovery_target)
            .unwrap_or(position);

        // 生産施設そのものを予備位置にせず、移動可能なら隣接マスへ退避する。
        if production_positions.contains(&(target.x, target.y)) {
            if let Some((x, y)) = map
                .get_adjacent(target.x, target.y)
                .into_iter()
                .filter(|candidate| !production_positions.contains(candidate))
                .filter(|candidate| !occupied.contains(candidate))
                .filter(|candidate| {
                    map.get_terrain(candidate.0, candidate.1)
                        .is_some_and(|terrain| {
                            crate::systems::movement::get_valid_movement_cost(
                                &registry,
                                stats.movement_type,
                                terrain,
                            )
                            .is_some()
                        })
                })
                .min_by_key(|candidate| (candidate.1, candidate.0))
            {
                target = GridPosition { x, y };
            }
        }

        // V4では明示Squadを正本とし、旧SoloFallbackによる「任務あり」判定を残さない。
        manager.solo_fallbacks.remove(&entity);
        let reusable_id = manager
            .proposal_view()
            .squads
            .iter()
            .find(|s| {
                s.plan.owner_id == Some(player_id)
                    && s.plan.mission_type == MissionType::Reserve
                    && s.reserve_entity == Some(entity)
                    && s.members.is_empty()
                    && squad_is_mutable_by_player(world, s, player_id)
            })
            .map(|s| s.id);
        let mut proposals = OperationLedger::proposals(manager);
        let id = if let Some(id) = reusable_id {
            proposals.replace_members(id, BTreeSet::from([entity]));
            id
        } else {
            proposals
                .create_owned(
                    MissionType::Reserve,
                    player_id,
                    BTreeSet::from([entity]),
                    None,
                    Vec::new(),
                )
                .id
        };
        let mut squad = proposals.squad_mut(id).unwrap();
        squad.remember_reserve(Some(entity));
        squad.plan_mut().target = Some(target);
        squad.plan_mut().target_island = campaign_binding.map(|(island_id, _)| island_id);
        squad.plan_mut().phase = MissionPhase::MovingToTarget;
    }
    // 別作戦へ採用されたEntityの空Reserve slotは解放する。
    OperationLedger::squad_list(manager).retain(|squad| {
        squad.plan.owner_id != Some(player_id)
            || squad.plan.mission_type != MissionType::Reserve
            || !squad.members.is_empty()
            || !squad_is_mutable_by_player(world, squad, player_id)
    });
}

/// 正規化後に実Squadを失った占領役と輸送役を、Reserveへ落とす前に
/// 現在の島作戦へもう一度まとめる。
///
/// 初回のSquad構築では同じEntityを複数作戦が候補にできるため、排他正規化の敗者が
/// その後のfree pool処理へ戻れなかった。この固定点パスは未割当Entityだけを線形走査し、
/// 現在必要な占領深度までを実輸送波へ変換する。価格や固定台数は使わない。
fn reconnect_unassigned_transport_waves(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    campaign_assignments: &[crate::ai::island_campaign::IslandCampaignAssignment],
) {
    reconnection::reconnect(world, manager, player_id, campaign_assignments);
}

/// 同じEntityでも、所属欠落から上陸後の引渡し待ちへ進むことを固定点で区別する。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum V4ReconnectionReason {
    Unbound,
    LandedCargo,
}

/// 現在bindingを失ったEntityと、現地に到着した輸送cargoだけを修復対象にする。
/// 有効な局地任務を再生せず、搭載中や他島のcargoも引き剥がさない。
fn pending_v4_reconnections(
    world: &World,
    manager: &SquadManager,
    player: PlayerId,
    campaign_assignments: &[crate::ai::island_campaign::IslandCampaignAssignment],
) -> HashMap<Entity, V4ReconnectionReason> {
    let mut pending = unbound_entity_ids(world, manager, player)
        .into_iter()
        .map(|entity| (entity, V4ReconnectionReason::Unbound))
        .collect::<HashMap<_, _>>();
    let Some(islands) = world.get_resource::<crate::ai::islands::IslandMap>() else {
        return pending;
    };
    let Some(registry) = world.get_resource::<UnitOperationRegistry>() else {
        return pending;
    };
    let active_islands = campaign_assignments
        .iter()
        .map(|assignment| assignment.island_id)
        .collect::<HashSet<_>>();
    let transports = manager
        .proposal_view()
        .squads
        .iter()
        .filter(|squad| {
            squad.plan.mission_type == MissionType::Transport
                && squad_is_mutable_by_player(world, squad, player)
        })
        .filter_map(|squad| {
            let island = squad.plan.target_island?;
            active_islands
                .contains(&island)
                .then_some((squad.id, island))
        })
        .collect::<HashMap<_, _>>();
    for (entity, binding) in registry.player_assignments(player) {
        if !matches!(
            binding.role,
            OperationUnitRole::Cargo | OperationUnitRole::DeliveredCargo
        ) || world
            .get::<crate::components::Transporting>(entity)
            .is_some()
        {
            continue;
        }
        let Some(island) = binding.squad_id.and_then(|id| transports.get(&id)).copied() else {
            continue;
        };
        if binding.owner
            == (OperationOwner::Campaign {
                player_id: player,
                island_id: island,
            })
            && world
                .get::<GridPosition>(entity)
                .and_then(|position| islands.get_island_at(position))
                .is_some_and(|current| current.id == island)
        {
            pending.insert(entity, V4ReconnectionReason::LandedCargo);
        }
    }
    pending
}

/// 排他正規化で具体Squadだけを失ったEntityと上陸cargoを、現在の作戦意図へ戻す。
/// 到着ETAは候補除外に使わない。既存Entityは新規生産より遅い場合でも作戦へ寄与でき、
/// 計画遅延はrolling plan側の予実差として別に評価する。
fn reconnect_current_v4_operations(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    campaign_assignments: &[crate::ai::island_campaign::IslandCampaignAssignment],
) {
    let pending = pending_v4_reconnections(world, manager, player_id, campaign_assignments);
    if pending.is_empty() {
        return;
    }
    // portfolio生成後の排他正規化や手番途中の完了でSquadだけを失っても、Campaign ownerは
    // 正本台帳に残る。集計時点のVecだけを再生するとこのEntityが再びReserveへ落ちるため、
    // 現在のownerを実割当へ合流させてからSquadを復元する。
    let owner_assignments = world
        .get_resource::<UnitOperationRegistry>()
        .map(|registry| registry.player_assignments(player_id))
        .unwrap_or_default();
    let repair_entities = pending.keys().copied().collect::<HashSet<_>>();
    let mut effective_assignments = campaign_assignments.to_vec();
    for assignment in &mut effective_assignments {
        let owner = OperationOwner::Campaign {
            player_id,
            island_id: assignment.island_id,
        };
        // 敗れた別ownerの元候補を復活させず、現在のownerだけを文脈に残す。
        for entities in [
            &mut assignment.transport_entities,
            &mut assignment.capture_entities,
            &mut assignment.combat_entities,
        ] {
            entities.retain(|entity| {
                owner_assignments
                    .get(entity)
                    .is_some_and(|current| current.owner == owner)
            });
        }
        for (entity, current) in &owner_assignments {
            if current.owner != owner
                || !repair_entities.contains(entity)
                || assignment.transport_entities.contains(entity)
                || assignment.capture_entities.contains(entity)
                || assignment.combat_entities.contains(entity)
                || world.get_entity(*entity).is_err()
            {
                continue;
            }
            let Some(stats) = world.get::<UnitStats>(*entity) else {
                continue;
            };
            match current.role {
                OperationUnitRole::Transport => assignment.transport_entities.push(*entity),
                OperationUnitRole::Cargo | OperationUnitRole::DeliveredCargo
                    if stats.can_capture =>
                {
                    assignment.capture_entities.push(*entity);
                }
                _ if stats.can_capture => assignment.capture_entities.push(*entity),
                _ => assignment.combat_entities.push(*entity),
            }
        }
        assignment
            .transport_entities
            .sort_by_key(|entity| entity.to_bits());
        assignment.transport_entities.dedup();
        assignment
            .capture_entities
            .sort_by_key(|entity| entity.to_bits());
        assignment.capture_entities.dedup();
        assignment
            .combat_entities
            .sort_by_key(|entity| entity.to_bits());
        assignment.combat_entities.dedup();
    }
    // 現在bindingを失ったEntityを含む作戦だけを修復する。
    // 完了済みの別作戦や継続配備を毎周組み直さない。
    effective_assignments.retain(|assignment| {
        assignment
            .transport_entities
            .iter()
            .chain(&assignment.capture_entities)
            .chain(&assignment.combat_entities)
            .any(|entity| pending.contains_key(entity))
    });
    let mut connectivity = TerrainConnectivity::default();
    for assignment in &effective_assignments {
        // 上陸cargoの引渡しでは輸送編成を再実行しない。輸送が必要な欠落だけを処理する。
        let needs_transport = assignment
            .transport_entities
            .iter()
            .any(|e| repair_entities.contains(e))
            || assignment
                .capture_entities
                .iter()
                .chain(&assignment.combat_entities)
                .any(|e| {
                    repair_entities.contains(e)
                        && !entity_can_self_deploy_to_assignment(world, assignment, *e)
                });
        if needs_transport {
            prepare_campaign_transport_assignment_with_connectivity(
                world,
                manager,
                player_id,
                assignment,
                &mut connectivity,
            );
        }
        let mut local = assignment.clone();
        for entities in [
            &mut local.transport_entities,
            &mut local.capture_entities,
            &mut local.combat_entities,
        ] {
            entities.retain(|entity| repair_entities.contains(entity));
        }
        // 現在のCapture担当がいる施設へ修復要員を重ねず、未担当施設を優先する。
        // 既存の担当を動かさないため、元の全員partitionの再生では補完できない。
        let claimed = manager
            .proposal_view()
            .squads
            .iter()
            .filter(|squad| {
                squad.plan.mission_type == MissionType::Capture
                    && squad.plan.target_island == Some(assignment.island_id)
                    && squad_is_mutable_by_player(world, squad, player_id)
            })
            .filter(|squad| {
                squad.members.iter().any(|entity| {
                    !repair_entities.contains(entity)
                        && world
                            .get_resource::<UnitOperationRegistry>()
                            .and_then(|registry| registry.assignment(*entity))
                            .is_some_and(|binding| {
                                binding.squad_id == Some(squad.id)
                                    && binding.role == OperationUnitRole::Member
                            })
                })
            })
            .filter_map(|squad| squad.plan.target)
            .collect::<HashSet<_>>();
        local
            .capture_target_positions
            .retain(|target| !claimed.contains(target));
        if local.capture_target_positions.is_empty() {
            // 採用済み候補が全て担当済みでも、同島の別の未所有施設を探す。
            // 全施設が担当済みなら既存のnearest fallbackで援護を許す。
            local.capture_target_positions = world
                .iter_entities()
                .filter_map(|entity| {
                    let target = *entity.get::<GridPosition>()?;
                    let property = entity.get::<Property>()?;
                    (property.owner_id != Some(player_id)
                        && !claimed.contains(&target)
                        && world
                            .get_resource::<crate::ai::islands::IslandMap>()?
                            .get_island_at(&target)
                            .is_some_and(|island| island.id == assignment.island_id))
                    .then_some(target)
                })
                .collect();
            local
                .capture_target_positions
                .sort_by_key(|target| (target.y, target.x));
        }
        prepare_campaign_local_assignment_scoped(
            world,
            manager,
            player_id,
            &local,
            &mut connectivity,
            CampaignAssignmentScope::Reconnect(&repair_entities),
        );
    }
    let pending_deployment = world
        .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
        .is_some_and(|registry| {
            registry
                .active_entities(player_id)
                .iter()
                .any(|e| pending.contains_key(e))
        });
    if pending_deployment {
        crate::ai::v4::deployment::prepare_deployment_squads(
            world,
            manager,
            player_id,
            &HashSet::default(),
        );
    }
}

/// 正本の現在bindingを持つEntity。正規化の直後だけ、rosterの写像と一致する。
fn current_bound_entities(world: &World) -> HashSet<Entity> {
    world
        .get_resource::<UnitOperationRegistry>()
        .map(UnitOperationRegistry::bound_entities)
        .unwrap_or_default()
}

/// 正本の現在bindingに加え、変更できない提案viewを配分対象から除く。
/// mixed・他playerの参照を新しい自軍Squadへ重複加入させない、既存の保護規則である。
fn allocation_unavailable_entities(
    world: &World,
    manager: &SquadManager,
    player_id: PlayerId,
) -> HashSet<Entity> {
    let mut unavailable = current_bound_entities(world);
    for squad in manager
        .proposal_view()
        .squads
        .iter()
        .filter(|squad| !squad_is_mutable_by_player(world, squad, player_id))
    {
        unavailable.extend(squad.members.iter().copied());
        unavailable.extend(squad.cargo_entities.iter().copied());
        unavailable.extend(squad.delivered_cargo.iter().copied());
        unavailable.extend(*squad.transport_entity);
    }
    unavailable
}

/// 正規化直後の配分可能な未所属Entity。現在bindingと保護された参照を同時に見る。
fn unbound_entity_ids(
    world: &World,
    manager: &SquadManager,
    player_id: PlayerId,
) -> HashSet<Entity> {
    let unavailable = allocation_unavailable_entities(world, manager, player_id);
    unassigned_player_entities(world, player_id, &unavailable)
}

/// 手番途中の提案view（roster）で、どのSquadにも現れない自軍Entity。
///
/// 手番途中のDrop/Capture/撃破はrosterだけを変え、正本は次の正規化まで更新されない。
/// 手番末の再接続が必要かの判定は、この提案viewの変化を見る必要がある。
fn unassigned_in_roster_view(
    world: &World,
    manager: &SquadManager,
    player_id: PlayerId,
) -> HashSet<Entity> {
    let assigned = manager.mentioned_entities();
    unassigned_player_entities(world, player_id, &assigned)
}

fn unassigned_player_entities(
    world: &World,
    player_id: PlayerId,
    assigned: &HashSet<Entity>,
) -> HashSet<Entity> {
    world
        .iter_entities()
        .filter(|entity_ref| {
            entity_ref
                .get::<Faction>()
                .is_some_and(|faction| faction.0 == player_id)
                // 搭載中cargoは盤上で独立行動できず、輸送Squad以外へも再割当できない。
                // これを未割当と数えると、解消不能な同じcargoを理由に各AI stepで
                // campaign固定点を再実行し続ける。
                && entity_ref
                    .get::<crate::components::Transporting>()
                    .is_none()
                && !assigned.contains(&entity_ref.id())
        })
        .map(|entity_ref| entity_ref.id())
        .collect()
}

/// V4以外の従来のReserve解散。所属解放とID維持の方策を混ぜない。
fn remove_player_reserve_squads(manager: &mut SquadManager, player_id: PlayerId) {
    OperationLedger::squad_list(manager).retain(|squad| {
        squad.plan.owner_id != Some(player_id) || squad.plan.mission_type != MissionType::Reserve
    });
}

/// Reserveを配分可能へ戻すが、連続してReserveになるEntityの受入identityは保持する。
fn release_player_reserve_proposals(world: &World, manager: &mut SquadManager, player: PlayerId) {
    let slots = manager
        .proposal_view()
        .squads
        .iter()
        .filter(|s| {
            s.plan.owner_id == Some(player)
                && s.plan.mission_type == MissionType::Reserve
                && squad_is_mutable_by_player(world, s, player)
        })
        .map(|s| (s.id, s.members.iter().next().copied().or(s.reserve_entity)))
        .collect::<Vec<_>>();
    let mut proposals = OperationLedger::proposals(manager);
    for (id, entity) in slots {
        proposals.release_where(id, |_| true);
        let mut squad = proposals.squad_mut(id).unwrap();
        squad.remember_reserve(entity);
        squad.plan_mut().phase = MissionPhase::Forming;
    }
}

/// 戦略分析の前に再編入力を整える共通hook。Reserve解放→SoloFallback解除→所属正規化の
/// 順序を保つ。is_v4は呼出し側で確定した値を使い、ここでは実装経路を判定しない。
/// 手番末の再接続とは責務が異なるため、毎ターンのplan_squads入口でのみ使う。
fn prepare_squads_for_replanning(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    is_v4: bool,
) {
    // Reserveは永続所有権ではなく「次の再計画までの明示待機」である。
    // 毎手番いったんfree poolへ戻し、新しいcampaign・防衛・攻撃へ再徴用可能にする。
    if is_v4 {
        release_player_reserve_proposals(world, manager, player_id);
    } else {
        // 旧版は従来どおりReserve Squad自体を解散し、ID維持を波及させない。
        remove_player_reserve_squads(manager, player_id);
    }
    if is_v4 {
        // V4は手番末に全Entityへ明示Squadを保証する。旧V1/V3のSoloFallback印を
        // 次手番へ持ち越すと、free pool収集から歩兵・輸送役が除外され、島作戦へ
        // 再編入されないままReserveだけを毎手番作り直す循環になる。
        manager.solo_fallbacks.clear();
    }
    // 前手番の作戦所有権を使って古い重複Squadを一度だけ正規化してから分析する。
    reconcile_unique_operation_assignments(world, manager, player_id);
}

/// 所属の確定後は、再編せずに現在の護衛で出発許可だけを更新する。
/// pendingが空でも新しい護衛の配置を反映し、編成途中の待機判定を持ち越さない。
fn synchronize_current_v4_capture_departures(
    world: &World,
    manager: &mut SquadManager,
    player: PlayerId,
    assignments: &[crate::ai::island_campaign::IslandCampaignAssignment],
) {
    let pending = HashSet::default();
    let mut connectivity = TerrainConnectivity::default();
    for assignment in assignments {
        CampaignAssignmentScope::Reconnect(&pending).synchronize_departure(
            world,
            manager,
            player,
            assignment,
            &mut connectivity,
        );
    }
}

/// 所属欠落と上陸引渡しを作業集合として再接続する。
/// 古いcacheの全作戦再生と固定3周を廃止し、対象と修復理由の変化だけを処理する。
fn reconnect_v4_operation_fixed_point(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    campaign_assignments: &[crate::ai::island_campaign::IslandCampaignAssignment],
) {
    let mut seen = HashSet::default();
    loop {
        let before = pending_v4_reconnections(world, manager, player_id, campaign_assignments);
        if before.is_empty() {
            break;
        }
        let mut key = before
            .iter()
            .map(|(e, reason)| (e.to_bits(), *reason))
            .collect::<Vec<_>>();
        key.sort_unstable();
        // 同じ未所属集合への再訪は移管の循環であり、さらに再編しても解決しない。
        // 固定の周回数で打ち切らず、循環をcatch-allの明示Reserveへ渡す。
        if !seen.insert(key) {
            break;
        }
        reconnect_current_v4_operations(world, manager, player_id, campaign_assignments);
        reconcile_unique_operation_assignments(world, manager, player_id);
        reconnect_unassigned_transport_waves(world, manager, player_id, campaign_assignments);
        reconcile_unique_operation_assignments(world, manager, player_id);
        if pending_v4_reconnections(world, manager, player_id, campaign_assignments) == before {
            break;
        }
    }
    synchronize_current_v4_capture_departures(world, manager, player_id, campaign_assignments);
}

/// 手番途中のDrop/Capture/作戦完了でSquad参照を失ったV4 Entityだけを再接続する。
/// 戦略分析や生産計画は再実行せず、cache済みportfolioと現在の正規台帳を使う。
pub(crate) fn reconcile_v4_end_turn_reserves(world: &mut World, player_id: PlayerId) {
    if crate::ai::resolve_player_ai_version(world, player_id)
        != crate::ai::ai_version::AiVersion::V4
    {
        return;
    }
    let campaign_assignments = world
        .get_resource::<AdoptedTurnPortfolio>()
        .and_then(|cache| cache.campaign_portfolio(player_id))
        .map(|portfolio| {
            portfolio
                .defenses
                .iter()
                .chain(portfolio.active_offensives.iter())
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut manager = world.remove_resource::<SquadManager>().unwrap_or_default();
    // Reserveはこの手番の再割当をすでに試した明示待機である。Drop/Capture/撃破などで
    // 新たにSquad参照を失ったEntityがいない限り、同じportfolioへ全Reserveを何度も
    // 戻して再構築しても結果は変わらない。通常行動1件ごとの全campaign再走査を防ぐ。
    let landed_handoff_pending =
        pending_v4_reconnections(world, &manager, player_id, &campaign_assignments)
            .values()
            .any(|reason| *reason == V4ReconnectionReason::LandedCargo);
    if unassigned_in_roster_view(world, &manager, player_id).is_empty() && !landed_handoff_pending {
        synchronize_current_v4_capture_departures(
            world,
            &mut manager,
            player_id,
            &campaign_assignments,
        );
        world.insert_resource(manager);
        return;
    }
    // 当手番に既に再接続を試したReserveは保持し、新たに孤立したEntityだけを処理する。
    reconcile_unique_operation_assignments(world, &mut manager, player_id);
    reconnect_v4_operation_fixed_point(world, &mut manager, player_id, &campaign_assignments);
    // 現行作戦・生産目的・輸送波の全てへ再接続できなかったEntityだけを明示遊兵にする。
    assign_explicit_reserve_squads(world, &mut manager, player_id, &campaign_assignments);
    reconcile_unique_operation_assignments(world, &mut manager, player_id);
    let mut cache = world
        .remove_resource::<AdoptedTurnPortfolio>()
        .unwrap_or_default();
    cache.project_committed(world, player_id, &manager);
    world.insert_resource(cache);
    world.insert_resource(manager);
}

/// 通常の占領候補順位で、生産施設を距離何マス分だけ優先するか（従来値）。
const PRODUCTION_FACILITY_DIST_BONUS: usize = 6;

/// 汎用輸送もcache・campaign編成と同じ採用portfolioの地理的目標を使う。
/// 採用assignmentが見つからない場合は、島内の未所有物件へ戻す。
fn resolve_generic_transport_target(
    adopted_portfolio: &crate::ai::island_campaign::IslandCampaignPortfolio,
    island: crate::ai::islands::IslandId,
    player: PlayerId,
    island_properties: &HashMap<crate::ai::islands::IslandId, Vec<GridPosition>>,
    ownership: &HashMap<GridPosition, Option<PlayerId>>,
) -> Option<GridPosition> {
    adopted_portfolio
        .assignment_for(island)
        .map(|assignment| assignment.target_position)
        .or_else(|| {
            island_properties.get(&island).and_then(|positions| {
                let mut positions = positions.clone();
                positions.sort_by_key(|position| (position.y, position.x));
                positions
                    .into_iter()
                    .find(|position| ownership.get(position) != Some(&Some(player)))
            })
        })
}

/// 占領の並び順・歩兵選択に必要な読み取り専用入力。方策の設定値の束ではなく、
/// 共通再編本体が収集した事実を、通常／拡張のコードへ渡す。
struct CapturePlanningContext<'a> {
    map: &'a Map,
    registry: &'a MasterDataRegistry,
    island_map: &'a crate::ai::islands::IslandMap,
    infantry: &'a [(Entity, GridPosition, UnitStats)],
    capital: Option<GridPosition>,
    is_v3: bool,
}

/// ゲームの戦略状況に基づいて、自動的に部隊の構築と新規メンバーの割り当てを行います。
/// 再編の段は [`formation`] にある。
pub fn plan_squads(world: &mut World, perspective_player: PlayerId) {
    // 小規模マップのV4は extended 側の実装を使う。
    if crate::ai::code_path::is_extended() {
        return planning_policy::plan_squads(world, perspective_player);
    }
    formation::plan_squads_with_policy(
        world,
        perspective_player,
        formation::PlanningPolicy::Standard,
    );
}

/// 降車済みカーゴを通常の占領・攻撃部隊へ引き渡します。
fn handoff_delivered_cargo(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    cargo: Entity,
    target_island: Option<crate::ai::islands::IslandId>,
    preferred_target: Option<GridPosition>,
) -> bool {
    if world
        .get::<crate::components::Transporting>(cargo)
        .is_some()
        || manager
            .proposal_view()
            .squads
            .iter()
            .any(|squad| squad.members.contains(&cargo))
    {
        return false;
    }
    let (Some(position), Some(stats)) = (
        world.get::<GridPosition>(cargo).copied(),
        world.get::<UnitStats>(cargo).cloned(),
    ) else {
        return false;
    };
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned();
    let Some(map) = world.get_resource::<Map>().cloned() else {
        return false;
    };
    let Some(registry) = world.get_resource::<MasterDataRegistry>().cloned() else {
        return false;
    };
    let landing_island = island_map
        .as_ref()
        .and_then(|map| map.get_island_at(&position))
        .map(|island| island.id)
        .or(target_island);
    let Some(local_island) = landing_island else {
        return false;
    };
    let is_reachable_local_target = |target: GridPosition| {
        island_map
            .as_ref()
            .and_then(|map| map.get_island_at(&target))
            .is_some_and(|island| island.id == local_island)
            && is_terrain_reachable(
                &map,
                &registry,
                (position.x, position.y),
                (target.x, target.y),
                stats.movement_type,
            )
    };

    if stats.can_capture {
        // 同じ輸送目標から順に降りるcargoへ単一のpreferred_targetをそのまま渡すと、
        // 先に作ったCapture Squadと後続cargoが同じ施設へ集中する。既に同じ島で
        // 担当済みの施設を後順位へ回し、降車直後から島内の未所有施設を分担する。
        let claimed_targets: HashSet<_> = manager
            .proposal_view()
            .squads
            .iter()
            .filter(|squad| squad_is_mutable_by_player(world, squad, player_id))
            .filter(|squad| squad.plan.mission_type == MissionType::Capture)
            .filter(|squad| squad.plan.target_island == Some(local_island))
            .filter_map(|squad| squad.plan.target)
            .collect();
        let mut targets = Vec::new();
        let mut query = world.query::<(&GridPosition, &Property)>();
        for (target, property) in query.iter(world) {
            if property.owner_id == Some(player_id) {
                continue;
            }
            if !is_reachable_local_target(*target) {
                continue;
            }
            let preferred_rank = if Some(*target) == preferred_target {
                0u8
            } else {
                1u8
            };
            targets.push((
                u8::from(claimed_targets.contains(target)),
                preferred_rank,
                position.x.abs_diff(target.x) + position.y.abs_diff(target.y),
                target.y,
                target.x,
                *target,
            ));
        }
        targets.sort_by_key(|target| (target.0, target.1, target.2, target.3, target.4));
        if let Some((_, _, _, _, _, target)) = targets.first().copied() {
            let mut proposals = OperationLedger::proposals(manager);
            let mut squad = proposals.create_owned(
                MissionType::Capture,
                player_id,
                BTreeSet::from([cargo]),
                None,
                Vec::new(),
            );
            squad.plan_mut().target = Some(target);
            squad.plan_mut().target_island = Some(local_island);
            squad.plan_mut().phase = MissionPhase::MovingToTarget;
            return true;
        }
    }

    let mut enemy_targets = Vec::new();
    let mut query = world.query::<(&GridPosition, &Faction)>();
    for (target, faction) in query.iter(world) {
        if faction.0 == player_id {
            continue;
        }
        if !is_reachable_local_target(*target) {
            continue;
        }
        enemy_targets.push((
            position.x.abs_diff(target.x) + position.y.abs_diff(target.y),
            target.y,
            target.x,
            *target,
        ));
    }
    enemy_targets.sort_by_key(|target| (target.0, target.1, target.2));
    if let Some(target) = enemy_targets
        .first()
        .map(|target| target.3)
        .or_else(|| preferred_target.filter(|target| is_reachable_local_target(*target)))
    {
        let mut proposals = OperationLedger::proposals(manager);
        let mut squad = proposals.create_owned(
            MissionType::Attack,
            player_id,
            BTreeSet::from([cargo]),
            None,
            Vec::new(),
        );
        squad.plan_mut().target = Some(target);
        squad.plan_mut().target_island = Some(local_island);
        squad.plan_mut().phase = MissionPhase::MovingToTarget;
        true
    } else {
        false
    }
}

// ==========================================
// 輸送部隊用のフェーズ更新 & 実行ロジック
// ==========================================

fn get_target_position_for_island(
    map: &Map,
    registry: &MasterDataRegistry,
    island: &crate::ai::islands::Island,
    t_pos: GridPosition,
    movement_type: crate::resources::MovementType,
) -> Option<GridPosition> {
    if movement_type == crate::resources::MovementType::Ship {
        island
            .tiles
            .iter()
            .filter(|tile| {
                matches!(
                    map.get_terrain(tile.x, tile.y),
                    Some(Terrain::Port | Terrain::Shoal)
                ) && crate::systems::movement::get_valid_movement_cost(
                    registry,
                    movement_type,
                    map.get_terrain(tile.x, tile.y).unwrap(),
                )
                .is_some()
            })
            .min_by_key(|tile| {
                (
                    t_pos.x.abs_diff(tile.x) + t_pos.y.abs_diff(tile.y),
                    tile.y,
                    tile.x,
                )
            })
            .copied()
    } else {
        island
            .tiles
            .iter()
            .min_by_key(|p| {
                (
                    (p.x as i32 - t_pos.x as i32).abs() + (p.y as i32 - t_pos.y as i32).abs(),
                    p.x,
                    p.y,
                )
            })
            .cloned()
    }
}

/// 現在ターンに到達可能な輸送位置から、合法・到達可能・低脅威な降車位置を選びます。
fn select_landing_candidate(
    world: &mut World,
    transport_entity: Entity,
    cargo_entity: Entity,
    transport_position: GridPosition,
    reachable: &std::collections::BTreeSet<(usize, usize)>,
    target_island: Option<crate::ai::islands::IslandId>,
    target_position: Option<GridPosition>,
) -> Option<(GridPosition, GridPosition)> {
    let cargo_stats = world.get::<UnitStats>(cargo_entity).cloned()?;
    let cargo_health = world
        .get::<Health>(cargo_entity)
        .map(|health| health.current)
        .unwrap_or(100);
    let cargo_faction = world.get::<Faction>(cargo_entity)?.0;
    let map = world.resource::<Map>().clone();
    let registry = world.resource::<MasterDataRegistry>().clone();
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned();
    let damage_chart = world
        .get_resource::<crate::resources::DamageChart>()
        .cloned();

    let mut enemy_threats = Vec::new();
    let mut enemy_query = world.query::<(&GridPosition, &Faction, &UnitStats, Option<&Health>)>();
    for (position, faction, stats, health) in enemy_query.iter(world) {
        if faction.0 == cargo_faction {
            continue;
        }
        enemy_threats.push((
            *position,
            stats.unit_type,
            stats.cost,
            health.map(|value| value.current).unwrap_or(100),
            stats.min_range,
            stats.max_range,
            stats.max_movement,
        ));
    }

    let mut reachable_positions: Vec<_> = reachable.iter().copied().collect();
    reachable_positions.sort_by_key(|position| (position.1, position.0));
    let mut best = None;
    let empty_occupants = HashMap::default();
    let target_distances = target_position.map(|target| {
        calculate_all_turn_distances(
            &map,
            &registry,
            &empty_occupants,
            (target.x, target.y),
            cargo_stats.movement_type,
            cargo_stats.max_movement,
            0,
            cargo_faction,
        )
    });

    for (transport_x, transport_y) in reachable_positions {
        let candidate_transport = GridPosition {
            x: transport_x,
            y: transport_y,
        };
        let mut drop_tiles = crate::systems::transport::get_droppable_tiles_at(
            world,
            transport_entity,
            cargo_entity,
            candidate_transport,
        );
        drop_tiles.sort_by_key(|position| (position.1, position.0));

        for (drop_x, drop_y) in drop_tiles {
            let drop_position = GridPosition {
                x: drop_x,
                y: drop_y,
            };
            if target_island.is_some_and(|island_id| {
                island_map
                    .as_ref()
                    .and_then(|map| map.get_island_at(&drop_position))
                    .is_none_or(|island| island.id != island_id)
            }) {
                continue;
            }
            let turns = if let Some(distances) = &target_distances {
                let Some(distance) = distances.get(&drop_position) else {
                    continue;
                };
                distance.turns
            } else {
                0
            };

            let terrain_defense = map
                .get_terrain(drop_position.x, drop_position.y)
                .map(|terrain| registry.get_terrain_defense_bonus(terrain))
                .unwrap_or(0);
            let danger = damage_chart.as_ref().map_or(0, |chart| {
                crate::ai::threat::indirect_fire_expected_loss(
                    &map,
                    (drop_position.x, drop_position.y),
                    cargo_stats.unit_type,
                    cargo_stats.cost,
                    cargo_health,
                    terrain_defense,
                    &enemy_threats,
                    chart,
                )
            });
            let transport_distance = map.distance(
                transport_position.x,
                transport_position.y,
                transport_x,
                transport_y,
            ) as usize;
            let score = (
                danger,
                turns,
                transport_distance,
                drop_y,
                drop_x,
                transport_y,
                transport_x,
            );
            if best.is_none_or(|(_, _, best_score)| score < best_score) {
                best = Some((candidate_transport, drop_position, score));
            }
        }
    }

    best.map(|(transport, drop, _)| (transport, drop))
}

/// 中立島・兵站島の逐次便について、今手番に合流できない未搭載cargoを後続便へ戻す。
/// 1体でも搭載済みなら輸送を開始し、遠い2体目のためにヘリを数ターン待たせない。
fn detach_deferred_pickup_cargo(
    world: &mut World,
    squad: &mut SquadEdit<'_>,
) -> Option<Vec<Entity>> {
    if !squad.plan.allow_partial_departure
        || !squad.plan.departure_authorized
        || squad.plan.phase != MissionPhase::Transport(TransportPhase::Pickup)
    {
        return None;
    }
    let transport = (*squad.transport_entity)?;
    let loaded = world
        .get::<crate::components::CargoCapacity>(transport)?
        .loaded
        .clone();
    if loaded.is_empty() {
        return None;
    }
    let deferred: Vec<_> = squad
        .cargo_entities
        .iter()
        .copied()
        .filter(|cargo| !loaded.contains(cargo))
        .collect();
    if deferred.is_empty()
        || deferred
            .iter()
            .copied()
            .any(|cargo| cargo_can_board_transport_this_turn(world, cargo, transport))
    {
        return None;
    }

    OperationLedger::roster(squad).retain_cargo(|cargo| loaded.contains(cargo));
    squad.plan_mut().phase = MissionPhase::Transport(TransportPhase::Transit);
    Some(deferred)
}

/// 輸送部隊のフェーズ更新と完了判定
fn update_transport_squad_phase(world: &mut World, squad: &mut SquadEdit<'_>) -> bool {
    let Some(transport_entity) = *squad.transport_entity else {
        return true;
    };
    if world.get::<GridPosition>(transport_entity).is_none() {
        return true;
    }
    let phase = match squad.plan.phase {
        MissionPhase::Transport(phase) => phase,
        _ => return false,
    };

    // 実際の CargoCapacity を正とし、計画から漏れた搭載済みカーゴも追跡へ戻す。
    let mut loaded = world
        .get::<crate::components::CargoCapacity>(transport_entity)
        .map(|capacity| capacity.loaded.clone())
        .unwrap_or_default();
    loaded.sort_by_key(|entity| entity.to_bits());
    loaded.dedup();
    // 実搭載・降車の物理状態を積荷bindingへ反映する（handoff前の唯一の同期点）。
    let mut roster = OperationLedger::roster(squad);
    for cargo in &loaded {
        if !roster.has_cargo(*cargo) {
            roster.push_cargo(*cargo);
        }
    }

    let mut remaining = Vec::new();
    for cargo in roster.take_cargo() {
        let is_loaded = loaded.contains(&cargo)
            || world
                .get::<crate::components::Transporting>(cargo)
                .is_some_and(|transporting| transporting.0 == transport_entity);
        if is_loaded
            || phase == TransportPhase::Pickup && world.get::<GridPosition>(cargo).is_some()
        {
            remaining.push(cargo);
        } else if matches!(phase, TransportPhase::Transit | TransportPhase::Drop)
            && world.get::<GridPosition>(cargo).is_some()
        {
            roster.push_delivered(cargo);
        }
    }
    roster.replace_cargo(remaining);

    match phase {
        TransportPhase::Pickup => {
            if squad.cargo_entities.is_empty() {
                return true;
            }
            let all_loaded = squad.cargo_entities.iter().all(|cargo| {
                loaded.contains(cargo)
                    && world
                        .get::<crate::components::Transporting>(*cargo)
                        .is_some_and(|transporting| transporting.0 == transport_entity)
            });
            if all_loaded && squad.plan.departure_authorized {
                squad.plan_mut().phase = MissionPhase::Transport(TransportPhase::Transit);
            }
        }
        TransportPhase::Transit => {
            if loaded.is_empty() {
                squad.plan_mut().phase = MissionPhase::Transport(TransportPhase::Return);
            } else if let Some(cargo) = loaded.first().copied()
                && !crate::systems::transport::get_droppable_tiles(world, transport_entity, cargo)
                    .is_empty()
            {
                squad.plan_mut().phase = MissionPhase::Transport(TransportPhase::Drop);
            }
        }
        TransportPhase::Drop => {
            if loaded.is_empty() {
                squad.plan_mut().phase = MissionPhase::Transport(TransportPhase::Return);
            }
        }
        TransportPhase::Return => {
            if !loaded.is_empty() {
                squad.plan_mut().phase = MissionPhase::Transport(TransportPhase::Drop);
                return false;
            }
            if let Some(t_pos) = world.get::<GridPosition>(transport_entity).copied()
                && let Some(t_faction) = world
                    .get::<Faction>(transport_entity)
                    .map(|faction| faction.0)
            {
                let mut query = world.query::<(&GridPosition, &Property)>();
                let at_base = query.iter(world).any(|(position, property)| {
                    *position == t_pos && property.owner_id == Some(t_faction)
                });
                if at_base {
                    return true;
                }
            }
        }
    }

    false
}

/// 輸送部隊の実行ステップ意思決定
pub fn execute_transport_squad_step(
    world: &mut World,
    squad: &mut SquadEdit<'_>,
    skip_entities: &HashSet<Entity>,
) -> Option<(Entity, crate::ai::command::AiCommand)> {
    transport_step::execute(world, squad, skip_entities)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::strategy::analyze_strategy;
    use crate::components::*;
    use crate::resources::master_data::*;
    use crate::resources::*;

    pub(super) fn setup_test_world() -> World {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }

        let map = Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: GridTopology::Square,
        };

        // Setup a small map
        world.insert_resource(map.clone());
        world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
        world.insert_resource(SquadManager::new());
        world
    }

    /// 上陸済みcargoはbindingがあっても輸送待機へ残さず、現地の占領任務へ引き渡す。
    #[test]
    fn reconnection_hands_landed_bound_cargo_to_local_capture() {
        for path in [
            crate::ai::code_path::CodePath::MainCompatible,
            crate::ai::code_path::CodePath::Extended,
        ] {
            let _path = crate::ai::code_path::enter(path);
            // 編成末と手番途中の両方で、搭載中cargoのbindingを保持する。
            for (end_turn, loaded) in [(false, false), (true, false), (false, true), (true, true)] {
                use crate::ai::island_campaign::{
                    IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
                };
                let mut world = setup_test_world();
                let player = PlayerId(1);
                let target = GridPosition { x: 2, y: 0 };
                let mut map = Map::new(5, 1, Terrain::Plains, GridTopology::Square);
                map.set_terrain(target.x, target.y, Terrain::City).unwrap();
                let occupied_target = GridPosition { x: 1, y: 0 };
                map.set_terrain(occupied_target.x, occupied_target.y, Terrain::City)
                    .unwrap();
                let islands = crate::ai::islands::IslandMap::analyze(&map);
                let island = islands.get_island_at(&target).unwrap().id;
                world.insert_resource(map);
                world.insert_resource(islands);
                world.spawn((target, Property::new(Terrain::City, None, 200)));
                world.spawn((occupied_target, Property::new(Terrain::City, None, 200)));
                let current_capturer = world
                    .spawn((
                        Faction(player),
                        occupied_target,
                        Health {
                            current: 100,
                            max: 100,
                        },
                        UnitStats {
                            can_capture: true,
                            movement_type: MovementType::Infantry,
                            ..UnitStats::mock()
                        },
                    ))
                    .id();
                let cargo = world
                    .spawn((
                        Faction(player),
                        GridPosition { x: 0, y: 0 },
                        Health {
                            current: 100,
                            max: 100,
                        },
                        UnitStats {
                            can_capture: true,
                            movement_type: MovementType::Infantry,
                            ..UnitStats::mock()
                        },
                    ))
                    .id();
                // 修復不要な同作戦のAttack歩兵を、占領能力だけでCaptureへ転用しない。
                let attacker = world
                    .spawn((
                        Faction(player),
                        GridPosition { x: 3, y: 0 },
                        UnitStats {
                            can_capture: true,
                            ..UnitStats::mock()
                        },
                        Health {
                            current: 100,
                            max: 100,
                        },
                    ))
                    .id();
                world.spawn((
                    Faction(PlayerId(2)),
                    GridPosition { x: 4, y: 0 },
                    UnitStats::mock(),
                    Health {
                        current: 100,
                        max: 100,
                    },
                ));
                let mut settings = crate::ai::PlayerAiSettings::new();
                settings.set_version(player, crate::ai::AiVersion::V4);
                world.insert_resource(settings);
                let carrier = loaded.then(|| {
                    world
                        .spawn((
                            Faction(player),
                            GridPosition { x: 0, y: 0 },
                            UnitStats {
                                unit_type: UnitType::TransportHelicopter,
                                can_capture: false,
                                ..UnitStats::mock()
                            },
                            CargoCapacity {
                                max: 1,
                                loaded: vec![cargo],
                            },
                        ))
                        .id()
                });
                if let Some(carrier) = carrier {
                    world.entity_mut(cargo).insert(Transporting(carrier));
                }
                let mut manager = SquadManager::new();
                {
                    let mut proposals = OperationLedger::proposals(&mut manager);
                    let mut squad = proposals.create_owned(
                        MissionType::Transport,
                        player,
                        carrier.into_iter().collect(),
                        carrier,
                        vec![cargo],
                    );
                    squad.plan.target = Some(target);
                    squad.plan.target_island = Some(island);
                    let mut capture = proposals.create_owned(
                        MissionType::Capture,
                        player,
                        BTreeSet::from([current_capturer]),
                        None,
                        vec![],
                    );
                    capture.plan.target_island = Some(island);
                    capture.plan.target = Some(occupied_target);
                    let mut attack = proposals.create_owned(
                        MissionType::Attack,
                        player,
                        BTreeSet::from([attacker]),
                        None,
                        vec![],
                    );
                    attack.plan.target_island = Some(island);
                    attack.plan.target = Some(GridPosition { x: 4, y: 0 });
                }
                reconcile_unique_operation_assignments(&mut world, &mut manager, player);
                assert!(unbound_entity_ids(&world, &manager, player).is_empty());
                let original_attack = world
                    .resource::<UnitOperationRegistry>()
                    .assignment(attacker)
                    .unwrap();
                let requirement = IslandCampaignRequirement {
                    preferred_transport: None,
                    transport_slots: 0,
                    capture_units: 1,
                    ground_combat_units: 0,
                    combat_units: 0,
                    total_budget: 1000,
                };
                let assignment = IslandCampaignAssignment {
                    island_id: island,
                    decision: IslandCampaignDecision::Expand,
                    target_position: target,
                    capture_target_positions: vec![occupied_target, target],
                    priority_enemy_types: vec![],
                    requirement: requirement.clone(),
                    purchase_shortfall: requirement,
                    allocated_budget: 1000,
                    transport_entities: vec![],
                    capture_entities: vec![cargo],
                    combat_entities: vec![],
                    operation_ready: false,
                    continued_from_existing_squad: true,
                };
                let repeat_assignment = assignment.clone();
                if end_turn {
                    let mut cache = AdoptedTurnPortfolio::default();
                    cache.set_campaign_portfolio(
                        player,
                        crate::ai::island_campaign::IslandCampaignPortfolio {
                            active_offensives: vec![assignment],
                            ..Default::default()
                        },
                    );
                    world.insert_resource(cache);
                    world.insert_resource(manager);
                    reconcile_v4_end_turn_reserves(&mut world, player);
                    manager = world.remove_resource::<SquadManager>().unwrap();
                } else {
                    reconnect_v4_operation_fixed_point(
                        &mut world,
                        &mut manager,
                        player,
                        &[assignment],
                    );
                }
                assert_eq!(
                    world
                        .resource::<UnitOperationRegistry>()
                        .assignment(attacker),
                    Some(original_attack),
                    "上陸cargoだけを修復し、現在のAttack所属を維持する"
                );
                let attack = manager
                    .squads
                    .iter()
                    .find(|s| Some(s.id) == original_attack.squad_id)
                    .unwrap();
                assert_eq!(attack.plan.mission_type, MissionType::Attack);
                assert_eq!(attack.plan.target, Some(GridPosition { x: 4, y: 0 }));
                let binding = world
                    .resource::<UnitOperationRegistry>()
                    .assignment(cargo)
                    .unwrap();
                assert_eq!(
                    binding.role,
                    if loaded {
                        OperationUnitRole::Cargo
                    } else {
                        OperationUnitRole::Member
                    }
                );
                let squad = manager
                    .squads
                    .iter()
                    .find(|s| Some(s.id) == binding.squad_id)
                    .unwrap();
                assert_eq!(
                    squad.plan.mission_type,
                    if loaded {
                        MissionType::Transport
                    } else {
                        MissionType::Capture
                    }
                );
                assert_eq!(squad.plan.target, Some(target));
                if !loaded {
                    assert!(
                        squad.plan.departure_authorized,
                        "現在のAttack部隊を変更せず、護衛として出発判定へ計上する"
                    );
                }
                if loaded {
                    assert!(squad.cargo_entities.contains(&cargo));
                } else {
                    assert!(squad.members.contains(&cargo));
                }
                if !loaded {
                    // 所属修復が不要な次の入口でも、護衛の現在状態で出発許可を更新する。
                    OperationLedger::proposals(&mut manager).edit(binding.squad_id.unwrap(), |s| {
                        s.plan.departure_authorized = false;
                    });
                    assert!(
                        pending_v4_reconnections(
                            &world,
                            &manager,
                            player,
                            std::slice::from_ref(&repeat_assignment)
                        )
                        .is_empty()
                    );
                    if end_turn {
                        world.insert_resource(manager);
                        reconcile_v4_end_turn_reserves(&mut world, player);
                        manager = world.remove_resource::<SquadManager>().unwrap();
                    } else {
                        reconnect_v4_operation_fixed_point(
                            &mut world,
                            &mut manager,
                            player,
                            &[repeat_assignment],
                        );
                    }
                    assert_eq!(
                        world.resource::<UnitOperationRegistry>().assignment(cargo),
                        Some(binding)
                    );
                    assert!(
                        manager
                            .squads
                            .iter()
                            .find(|s| Some(s.id) == binding.squad_id)
                            .unwrap()
                            .plan
                            .departure_authorized,
                        "所属変更なしでも現在の護衛を反映し、古い出発待機を持ち越さない"
                    );
                }
            }
        }
    }

    /// 実所属が揃っている場合、古いportfolioを再生して別島の空placeholderを作らない。
    #[test]
    fn reconnection_does_not_replay_stale_portfolio_for_fully_bound_units() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };
        let mut world = setup_test_world();
        let player = PlayerId(1);
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::City).unwrap();
        map.set_terrain(2, 0, Terrain::City).unwrap();
        let islands = crate::ai::islands::IslandMap::analyze(&map);
        let stale_island = islands
            .get_island_at(&GridPosition { x: 0, y: 0 })
            .unwrap()
            .id;
        let current_island = islands
            .get_island_at(&GridPosition { x: 2, y: 0 })
            .unwrap()
            .id;
        world.insert_resource(map);
        world.insert_resource(islands);
        let entity = world
            .spawn((
                Faction(player),
                GridPosition { x: 2, y: 0 },
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats {
                    can_capture: true,
                    movement_type: MovementType::Infantry,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let mut manager = SquadManager::new();
        {
            let mut proposals = OperationLedger::proposals(&mut manager);
            let mut squad = proposals.create_owned(
                MissionType::Capture,
                player,
                BTreeSet::from([entity]),
                None,
                vec![],
            );
            squad.plan.target = Some(GridPosition { x: 2, y: 0 });
            squad.plan.target_island = Some(current_island);
            squad.plan.phase = MissionPhase::MovingToTarget;
        }
        reconcile_unique_operation_assignments(&mut world, &mut manager, player);
        let before = format!("{:?}", manager.squads);
        let binding = world.resource::<UnitOperationRegistry>().assignment(entity);
        let requirement = IslandCampaignRequirement {
            preferred_transport: Some(UnitType::TransportHelicopter),
            transport_slots: 1,
            capture_units: 1,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 5000,
        };
        let stale = IslandCampaignAssignment {
            island_id: stale_island,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 0, y: 0 },
            capture_target_positions: vec![GridPosition { x: 0, y: 0 }],
            priority_enemy_types: vec![],
            requirement: requirement.clone(),
            purchase_shortfall: requirement,
            allocated_budget: 5000,
            transport_entities: vec![],
            capture_entities: vec![entity],
            combat_entities: vec![],
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        reconnect_v4_operation_fixed_point(&mut world, &mut manager, player, &[stale]);
        assert_eq!(format!("{:?}", manager.squads), before);
        assert_eq!(
            world.resource::<UnitOperationRegistry>().assignment(entity),
            binding
        );
    }

    #[test]
    fn unique_operation_registry_keeps_one_campaign_owner_with_linear_reconcile() {
        let mut world = setup_test_world();
        let player = PlayerId(1);
        let entity = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let intended_island = crate::ai::islands::IslandId(7);
        let stale_island = crate::ai::islands::IslandId(5);
        let mut registry = UnitOperationRegistry::default();
        registry.assign(
            entity,
            UnitOperationAssignment {
                owner: OperationOwner::Campaign {
                    player_id: player,
                    island_id: intended_island,
                },
                squad_id: None,
                role: OperationUnitRole::Member,
                assigned_turn: 1,
            },
        );
        world.insert_resource(registry);
        let mut manager = SquadManager::new();
        let mut stale = manager.create_owned_squad(MissionType::Defense, player);
        let stale_id = stale.id;
        stale.plan.target_island = Some(stale_island);
        stale.members.insert(entity);
        let mut intended = manager.create_owned_squad(MissionType::Capture, player);
        let intended_id = intended.id;
        intended.plan.target_island = Some(intended_island);
        intended.members.insert(entity);

        reconcile_unique_operation_assignments(&mut world, &mut manager, player);

        assert!(
            !manager
                .squads
                .iter()
                .find(|squad| squad.id == stale_id)
                .unwrap()
                .members
                .contains(&entity)
        );
        assert!(
            manager
                .squads
                .iter()
                .find(|squad| squad.id == intended_id)
                .unwrap()
                .members
                .contains(&entity)
        );
        let registry = world.resource::<UnitOperationRegistry>();
        let assignment = registry.assignment(entity).expect("unique assignment");
        assert_eq!(
            assignment.owner,
            OperationOwner::Campaign {
                player_id: player,
                island_id: intended_island,
            }
        );
        assert_eq!(assignment.squad_id, Some(intended_id));
        assert_eq!(registry.last_reconcile_visits(), 2);
        assert_eq!(registry.rejected_conflicts(), 1);
    }

    /// F07の再編前処理は対象playerのReserveだけを解放し、旧campaign所有権に沿って
    /// 重複所属を正規化する。V4だけは従来仕様どおり他player分も含めSoloFallbackを全解除。
    #[test]
    fn replanning_preparation_releases_only_player_reserves_before_reconcile() {
        for path in [
            crate::ai::code_path::CodePath::MainCompatible,
            crate::ai::code_path::CodePath::Extended,
        ] {
            for is_v4 in [false, true] {
                let _guard = crate::ai::code_path::enter(path);
                let mut world = setup_test_world();
                let player = PlayerId(1);
                let other_player = PlayerId(2);
                let entity = world
                    .spawn((Faction(player), GridPosition { x: 0, y: 0 }))
                    .id();
                let other_entity = world
                    .spawn((Faction(other_player), GridPosition { x: 1, y: 0 }))
                    .id();
                let island = crate::ai::islands::IslandId(7);
                let mut registry = UnitOperationRegistry::default();
                registry.assign(
                    entity,
                    UnitOperationAssignment {
                        owner: OperationOwner::Campaign {
                            player_id: player,
                            island_id: island,
                        },
                        squad_id: None,
                        role: OperationUnitRole::Member,
                        assigned_turn: 1,
                    },
                );
                world.insert_resource(registry);
                let mut manager = SquadManager::new();
                let mut reserve = manager.create_owned_squad(MissionType::Reserve, player);
                let reserve_id = reserve.id;
                reserve.members.insert(entity);
                let mut other = manager.create_owned_squad(MissionType::Reserve, other_player);
                let other_id = other.id;
                other.members.insert(other_entity);
                other.plan.target = Some(GridPosition { x: 9, y: 0 });
                let other_before = format!("{other:?}");
                let mut ownerless = manager.create_squad(MissionType::Reserve);
                let ownerless_id = ownerless.id;
                ownerless.members.insert(other_entity);
                let ownerless_before = format!("{ownerless:?}");
                let mut stale = manager.create_owned_squad(MissionType::Defense, player);
                let stale_id = stale.id;
                stale.plan.target_island = Some(crate::ai::islands::IslandId(5));
                stale.members.insert(entity);
                let mut intended = manager.create_owned_squad(MissionType::Capture, player);
                let intended_id = intended.id;
                intended.plan.target_island = Some(island);
                intended.members.insert(entity);
                let fallback_marks = HashSet::from_iter([entity, other_entity]);
                manager.solo_fallbacks = fallback_marks.clone();

                prepare_squads_for_replanning(&mut world, &mut manager, player, is_v4);

                let expected_ids = if is_v4 {
                    vec![reserve_id, other_id, ownerless_id, stale_id, intended_id]
                } else {
                    vec![other_id, ownerless_id, stale_id, intended_id]
                };
                assert_eq!(
                    manager
                        .squads
                        .iter()
                        .map(|squad| squad.id)
                        .collect::<Vec<_>>(),
                    expected_ids,
                    "{path:?} is_v4={is_v4}"
                );
                let by_id = |id| manager.squads.iter().find(|squad| squad.id == id).unwrap();
                if is_v4 {
                    // identityを残してもmemberは配分前に解放し、実所属を保持させない。
                    let slot = by_id(reserve_id);
                    assert!(slot.members.is_empty());
                    assert_eq!(slot.reserve_entity, Some(entity));
                    assert_eq!(slot.plan.phase, MissionPhase::Forming);
                } else {
                    assert!(!manager.squads.iter().any(|squad| squad.id == reserve_id));
                }
                assert_eq!(format!("{:?}", by_id(other_id)), other_before);
                assert_eq!(format!("{:?}", by_id(ownerless_id)), ownerless_before);
                assert!(by_id(stale_id).members.is_empty());
                assert_eq!(by_id(intended_id).members, BTreeSet::from([entity]));
                assert_eq!(
                    manager.solo_fallbacks,
                    if is_v4 {
                        HashSet::default()
                    } else {
                        fallback_marks
                    }
                );
                let registry = world.resource::<UnitOperationRegistry>();
                let assignment = registry.assignment(entity).expect("具体Squadへ正規化");
                assert_eq!(assignment.squad_id, Some(intended_id));
                assert_eq!(
                    assignment.owner,
                    OperationOwner::Campaign {
                        player_id: player,
                        island_id: island,
                    }
                );
                // Reserveにいたentityは正規化の走査前に解放され、DefenseとCaptureだけを数える。
                assert_eq!(registry.last_reconcile_visits(), 2);
                assert_eq!(registry.rejected_conflicts(), 1);
            }
        }
    }

    #[test]
    fn campaign_claim_preempts_generic_squad_without_scanning_per_action() {
        let mut world = setup_test_world();
        let player = PlayerId(1);
        let entity = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let island = crate::ai::islands::IslandId(3);
        let mut manager = SquadManager::new();
        let mut generic = manager.create_owned_squad(MissionType::Defense, player);
        generic.members.insert(entity);
        let mut campaign = manager.create_owned_squad(MissionType::Capture, player);
        let campaign_id = campaign.id;
        campaign.plan.target_island = Some(island);
        campaign.members.insert(entity);

        reconcile_unique_operation_assignments(&mut world, &mut manager, player);

        assert_eq!(
            world.resource::<UnitOperationRegistry>().assignment(entity),
            Some(UnitOperationAssignment {
                owner: OperationOwner::Campaign {
                    player_id: player,
                    island_id: island,
                },
                squad_id: Some(campaign_id),
                role: OperationUnitRole::Member,
                assigned_turn: world.resource::<MatchState>().current_turn_number.0,
            })
        );
    }

    #[test]
    fn active_campaign_releases_entity_removed_from_current_portfolio() {
        let mut world = setup_test_world();
        let player = PlayerId(1);
        let stale = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let island = crate::ai::islands::IslandId(3);
        let mut registry = UnitOperationRegistry::default();
        registry.assign(
            stale,
            UnitOperationAssignment {
                owner: OperationOwner::Campaign {
                    player_id: player,
                    island_id: island,
                },
                squad_id: None,
                role: OperationUnitRole::Member,
                assigned_turn: 1,
            },
        );
        world.insert_resource(registry);
        let requirement = crate::ai::island_campaign::IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 0,
        };
        let portfolio = crate::ai::island_campaign::IslandCampaignPortfolio {
            active_offensives: vec![crate::ai::island_campaign::IslandCampaignAssignment {
                island_id: island,
                decision: crate::ai::island_campaign::IslandCampaignDecision::Contest,
                target_position: GridPosition { x: 3, y: 3 },
                capture_target_positions: Vec::new(),
                priority_enemy_types: Vec::new(),
                requirement: requirement.clone(),
                purchase_shortfall: requirement,
                allocated_budget: 0,
                transport_entities: Vec::new(),
                capture_entities: Vec::new(),
                combat_entities: Vec::new(),
                operation_ready: true,
                continued_from_existing_squad: true,
            }],
            ..Default::default()
        };

        claim_campaign_portfolio_assignments(&mut world, player, &portfolio);

        assert!(
            world
                .resource::<UnitOperationRegistry>()
                .assignment(stale)
                .is_none(),
            "同じ島の作戦継続だけを理由に旧Entityを作戦なしで固定しない"
        );
    }

    #[test]
    fn one_campaign_can_have_multiple_squads_but_entity_has_one_concrete_squad() {
        let mut world = setup_test_world();
        let player = PlayerId(1);
        let entity = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let island = crate::ai::islands::IslandId(3);
        let mut manager = SquadManager::new();
        let mut first = manager.create_owned_squad(MissionType::Transport, player);
        let first_id = first.id;
        first.plan.target_island = Some(island);
        first.members.insert(entity);
        let mut second = manager.create_owned_squad(MissionType::Defense, player);
        second.plan.target_island = Some(island);
        second.members.insert(entity);

        reconcile_unique_operation_assignments(&mut world, &mut manager, player);

        let containing_squads = manager
            .squads
            .iter()
            .filter(|squad| squad.members.contains(&entity))
            .map(|squad| squad.id)
            .collect::<Vec<_>>();
        assert_eq!(containing_squads, vec![first_id]);
        assert_eq!(
            world
                .resource::<UnitOperationRegistry>()
                .assignment(entity)
                .and_then(|assignment| assignment.squad_id),
            Some(first_id)
        );
    }

    /// Reserveは通常配分へ解放してから再採用し、連続する明示待機のidentityを保つ。
    #[test]
    fn reserve_replanning_reuses_identity_without_hiding_the_entity_from_allocation() {
        let mut world = setup_test_world();
        let player = PlayerId(1);
        let entity = world
            .spawn((
                Faction(player),
                GridPosition { x: 2, y: 2 },
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats::mock(),
            ))
            .id();
        let mut manager = SquadManager::new();
        let id = {
            let mut proposals = OperationLedger::proposals(&mut manager);
            let mut squad = proposals.create_owned(
                MissionType::Reserve,
                player,
                BTreeSet::from([entity]),
                None,
                vec![],
            );
            squad.plan.target = Some(GridPosition { x: 2, y: 2 });
            squad.id
        };
        reconcile_unique_operation_assignments(&mut world, &mut manager, player);
        release_player_reserve_proposals(&world, &mut manager, player);
        reconcile_unique_operation_assignments(&mut world, &mut manager, player);
        assert!(
            unbound_entity_ids(&world, &manager, player).contains(&entity),
            "空Reserveのidentityは所属として数えない"
        );
        assign_explicit_reserve_squads(&mut world, &mut manager, player, &[]);
        reconcile_unique_operation_assignments(&mut world, &mut manager, player);
        let reserve = manager
            .squads
            .iter()
            .find(|s| s.members.contains(&entity))
            .unwrap();
        assert_eq!(reserve.id, id);
        assert_eq!(reserve.plan.mission_type, MissionType::Reserve);
        assert_eq!(manager.squads.len(), 1);
    }

    /// Reserveの履歴IDは所属ではなく、foreign cargoを持つ保護対象の再利用を許可しない。
    #[test]
    fn reserve_identity_reuse_does_not_modify_foreign_cargo_holder() {
        let mut world = setup_test_world();
        let player = PlayerId(1);
        let entity = world
            .spawn((
                Faction(player),
                GridPosition { x: 2, y: 2 },
                Health {
                    current: 100,
                    max: 100,
                },
                UnitStats::mock(),
            ))
            .id();
        let foreign = world.spawn(Faction(PlayerId(2))).id();
        let mut manager = SquadManager::new();
        let id = {
            let mut proposals = OperationLedger::proposals(&mut manager);
            let mut holder = proposals.create_owned(
                MissionType::Reserve,
                player,
                BTreeSet::new(),
                None,
                vec![foreign],
            );
            holder.reserve_entity = Some(entity);
            holder.id
        };
        let before = format!("{:?}", manager.squads[0]);
        reconcile_unique_operation_assignments(&mut world, &mut manager, player);
        assign_explicit_reserve_squads(&mut world, &mut manager, player, &[]);
        let holder = manager.squads.iter().find(|s| s.id == id).unwrap();
        assert_eq!(format!("{holder:?}"), before);
        let actual = manager
            .squads
            .iter()
            .find(|s| s.members.contains(&entity))
            .unwrap();
        assert_ne!(actual.id, id);
        assert!(actual.cargo_entities.is_empty());
    }

    #[test]
    fn test_update_squads_solo_fallback() {
        let mut world = setup_test_world();
        let p1 = PlayerId(1);

        // Spawn a unit with low HP
        let unit1 = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 5, y: 5 },
                Health {
                    current: 50,
                    max: 100,
                },
            ))
            .id();

        // Spawn a healthy unit
        let unit2 = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 6, y: 5 },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        update_squads(&mut world, p1);

        let manager = world.get_resource::<SquadManager>().unwrap();
        assert!(
            manager.solo_fallbacks.contains(&unit1),
            "Unit with 50 HP should be in fallback"
        );
        assert!(
            !manager.solo_fallbacks.contains(&unit2),
            "Healthy unit should not be in fallback"
        );

        // Now heal the unit
        if let Some(mut h) = world.get_mut::<Health>(unit1) {
            h.current = 100;
        }

        update_squads(&mut world, p1);

        let manager = world.get_resource::<SquadManager>().unwrap();
        assert!(
            !manager.solo_fallbacks.contains(&unit1),
            "Unit should have recovered and left fallback"
        );
    }

    #[test]
    fn test_plan_squads_attack_creation() {
        let mut world = setup_test_world();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // Enemy cluster at (8,8)
        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 8, y: 8 },
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                max_movement: 6,
                cost: 7000,
                ..UnitStats::mock()
            },
        ));

        // Friendly units to form an attack squad
        let u1 = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 2, y: 2 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    cost: 7000,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        let u2 = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 2, y: 3 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    cost: 7000,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        plan_squads(&mut world, p1);

        let manager = world.get_resource::<SquadManager>().unwrap();

        // Verify Attack squad created
        let attack_squads: Vec<_> = manager
            .squads
            .iter()
            .filter(|s| s.plan.mission_type == MissionType::Attack)
            .collect();
        assert_eq!(
            attack_squads.len(),
            1,
            "Should create exactly 1 attack squad"
        );
        assert!(attack_squads[0].members.contains(&u1));
        assert!(attack_squads[0].members.contains(&u2));
        assert_eq!(
            attack_squads[0].plan.target,
            Some(GridPosition { x: 8, y: 8 })
        );
    }

    #[test]
    fn test_plan_squads_capture() {
        let mut world = setup_test_world();
        let p1 = PlayerId(1);

        // Friendly base so it doesn't think it's off-island
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::Factory, Some(p1), 100),
        ));

        // Unowned city to capture
        world.spawn((
            GridPosition { x: 5, y: 5 },
            Property::new(Terrain::City, None, 100),
        ));

        // Friendly infantry
        let inf = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 4, y: 4 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        plan_squads(&mut world, p1);

        let manager = world.get_resource::<SquadManager>().unwrap();

        // Verify Capture squad created
        let capture_squads: Vec<_> = manager
            .squads
            .iter()
            .filter(|s| s.plan.mission_type == MissionType::Capture)
            .collect();
        assert_eq!(capture_squads.len(), 1, "Should create 1 capture squad");
        assert!(capture_squads[0].members.contains(&inf));
        assert_eq!(
            capture_squads[0].plan.target,
            Some(GridPosition { x: 5, y: 5 })
        );
    }

    /// Issue #53: V3 では中立拠点が無くても敵所有拠点を目標とする占領部隊が
    /// 編成されること、V2 では従来通り編成されないことを検証する
    #[test]
    fn test_v3_capture_squad_targets_enemy_property() {
        let run = |version: crate::ai::ai_version::AiVersion| -> Option<GridPosition> {
            let mut world = setup_test_world();
            let p1 = PlayerId(1);
            let p2 = PlayerId(2);

            let mut settings = crate::ai::ai_version::PlayerAiSettings::new();
            settings.set_version(p1, version);
            settings.set_version(p2, version);
            world.insert_resource(settings);

            // 自軍の工場 (base island 判定用)
            world.spawn((
                GridPosition { x: 1, y: 1 },
                Property::new(Terrain::Factory, Some(p1), 100),
            ));

            // 敵所有の都市 (中立拠点は存在しない)
            world.spawn((
                GridPosition { x: 5, y: 5 },
                Property::new(Terrain::City, Some(p2), 100),
            ));

            // フリーの自軍歩兵
            world.spawn((
                p1,
                Faction(p1),
                GridPosition { x: 4, y: 4 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ));

            plan_squads(&mut world, p1);

            let manager = world.get_resource::<SquadManager>().unwrap();
            manager
                .squads
                .iter()
                .find(|s| s.plan.mission_type == MissionType::Capture)
                .and_then(|s| s.plan.target)
        };

        // V2: 敵拠点は占領部隊の対象にならない (従来挙動)
        assert_eq!(
            run(crate::ai::ai_version::AiVersion::V2),
            None,
            "V2 は敵拠点への占領部隊を編成しないはず"
        );

        // V3: 敵拠点を目標とする占領部隊が編成される
        assert_eq!(
            run(crate::ai::ai_version::AiVersion::V3),
            Some(GridPosition { x: 5, y: 5 }),
            "V3 は敵拠点 (5,5) への占領部隊を編成するはず"
        );
    }

    /// Issue #53 (Gemini #4): 敵生産施設への護衛の適格判定。
    /// 鈍足・損傷・弾切れのユニットは護衛から除外され、
    /// 機動力があり健全で弾薬のあるユニットのみが選ばれることを検証する。
    #[test]
    fn test_escort_eligibility_filter() {
        let infantry_movement = 3;

        // 機動力十分・健全・弾薬あり → 適格 (中戦車 移動6)
        let fast_tank = UnitStats {
            unit_type: UnitType::MdTank,
            max_movement: 6,
            max_ammo1: 6,
            max_ammo2: 9,
            ..UnitStats::mock()
        };
        assert!(
            escort_is_eligible(&fast_tank, infantry_movement, 100, 6, 9),
            "健全で機動力・弾薬のある戦車は護衛適格のはず"
        );

        // 鈍足 (移動1 < 歩兵3) → 不適格 (砲台のような据置き砲)
        let slow_gun = UnitStats {
            unit_type: UnitType::Artillery,
            max_movement: 1,
            max_ammo1: 5,
            ..UnitStats::mock()
        };
        assert!(
            !escort_is_eligible(&slow_gun, infantry_movement, 100, 5, 0),
            "歩兵より鈍足のユニットは護衛不適格のはず"
        );

        // 損傷 (HP < 70) → 不適格
        assert!(
            !escort_is_eligible(&fast_tank, infantry_movement, 50, 6, 9),
            "HP50 の損傷ユニットは護衛不適格のはず"
        );

        // 弾切れ (主武器0・副武器0) → 不適格
        assert!(
            !escort_is_eligible(&fast_tank, infantry_movement, 100, 0, 0),
            "主武器・副武器とも弾切れのユニットは護衛不適格のはず"
        );

        // 主武器弾切れでも副武器が残っていれば適格
        assert!(
            escort_is_eligible(&fast_tank, infantry_movement, 100, 0, 5),
            "副武器が残っていれば護衛適格のはず"
        );
    }

    /// Issue #53: 中立拠点と敵拠点が混在する場合、歩兵から近い目標が
    /// 優先的に割り当てられる (決定的な順序) ことを検証する
    #[test]
    fn test_v3_capture_targets_sorted_by_distance() {
        let mut world = setup_test_world();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        let mut settings = crate::ai::ai_version::PlayerAiSettings::new();
        settings.set_version(p1, crate::ai::ai_version::AiVersion::V3);
        world.insert_resource(settings);

        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::Factory, Some(p1), 100),
        ));
        // 近い敵拠点 (歩兵から距離2) と遠い中立拠点 (距離8)
        world.spawn((
            GridPosition { x: 5, y: 3 },
            Property::new(Terrain::City, Some(p2), 100),
        ));
        world.spawn((
            GridPosition { x: 9, y: 5 },
            Property::new(Terrain::City, None, 100),
        ));

        // フリー歩兵は1体のみ → 近い方 (敵拠点) に割り当てられるはず
        world.spawn((
            p1,
            Faction(p1),
            GridPosition { x: 3, y: 3 },
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                can_capture: true,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        plan_squads(&mut world, p1);

        let manager = world.get_resource::<SquadManager>().unwrap();
        let capture_targets: Vec<_> = manager
            .squads
            .iter()
            .filter(|s| s.plan.mission_type == MissionType::Capture && !s.members.is_empty())
            .filter_map(|s| s.plan.target)
            .collect();
        assert_eq!(
            capture_targets,
            vec![GridPosition { x: 5, y: 3 }],
            "1体しかいない歩兵は最も近い目標 (敵拠点) に割り当てられるはず"
        );
    }

    #[test]
    fn v3_primary_campaign_uses_only_its_reserved_entities() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let mut settings = crate::ai::ai_version::PlayerAiSettings::default();
        settings.set_version(player, crate::ai::ai_version::AiVersion::V3);
        world.insert_resource(settings);
        for entry in &mut world.resource_mut::<Players>().0 {
            entry.funds = 0;
        }

        let mut map = Map::new(7, 3, Terrain::Sea, GridTopology::Square);
        for x in 0..=2 {
            map.set_terrain(x, 1, Terrain::Plains).unwrap();
        }
        map.set_terrain(1, 1, Terrain::Airport).unwrap();
        map.set_terrain(4, 1, Terrain::City).unwrap();
        map.set_terrain(6, 1, Terrain::City).unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.insert_resource(SquadManager::new());
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::Airport, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 4, y: 1 },
            Property::new(Terrain::City, None, 100),
        ));
        world.spawn((
            GridPosition { x: 6, y: 1 },
            Property::new(Terrain::City, None, 100),
        ));

        let helicopter_stats = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let _primary_transport = world
            .spawn((
                player,
                Faction(player),
                GridPosition { x: 2, y: 1 },
                helicopter_stats.clone(),
                CargoCapacity {
                    max: helicopter_stats.max_cargo,
                    loaded: Vec::new(),
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let _secondary_transport = world
            .spawn((
                player,
                Faction(player),
                GridPosition { x: 0, y: 1 },
                helicopter_stats.clone(),
                CargoCapacity {
                    max: helicopter_stats.max_cargo,
                    loaded: Vec::new(),
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let infantry_stats = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let mut primary_capture = Vec::new();
        let mut secondary_capture = Vec::new();
        for _ in 0..2 {
            primary_capture.push(
                world
                    .spawn((
                        player,
                        Faction(player),
                        GridPosition { x: 2, y: 1 },
                        infantry_stats.clone(),
                        Health {
                            current: 100,
                            max: 100,
                        },
                    ))
                    .id(),
            );
        }
        for _ in 0..2 {
            secondary_capture.push(
                world
                    .spawn((
                        player,
                        Faction(player),
                        GridPosition { x: 0, y: 1 },
                        infantry_stats.clone(),
                        Health {
                            current: 100,
                            max: 100,
                        },
                    ))
                    .id(),
            );
        }

        let strategy = analyze_strategy(&mut world, player);
        assert_eq!(strategy.campaign_portfolio.active_offensives.len(), 2);
        let primary = &strategy.campaign_portfolio.active_offensives[0];
        let secondary = &strategy.campaign_portfolio.active_offensives[1];
        assert!(primary.operation_ready);
        assert!(secondary.operation_ready);
        let primary_reserved_transport = primary.transport_entities[0];
        let secondary_reserved_transport = secondary.transport_entities[0];
        assert_ne!(primary_reserved_transport, secondary_reserved_transport);
        *world
            .get_mut::<GridPosition>(primary_reserved_transport)
            .unwrap() = GridPosition { x: 2, y: 1 };
        *world
            .get_mut::<GridPosition>(secondary_reserved_transport)
            .unwrap() = GridPosition { x: 0, y: 1 };

        plan_squads(&mut world, player);

        let manager = world.resource::<SquadManager>();
        let squad = manager
            .squads
            .iter()
            .find(|squad| squad.plan.target_island == Some(primary.island_id))
            .unwrap();
        assert_eq!(squad.transport_entity, Some(primary_reserved_transport));
        assert!(
            squad
                .cargo_entities
                .iter()
                .all(|entity| primary.capture_entities.contains(entity))
        );
        assert!(
            !squad
                .cargo_entities
                .contains(&secondary.capture_entities[0])
        );
        assert!(manager.squads.iter().all(|squad| {
            squad.transport_entity != Some(secondary_reserved_transport)
                || squad.plan.target_island == Some(secondary.island_id)
        }));
    }

    #[test]
    fn v3_unreserved_loaded_transport_uses_safe_drop_without_primary_target() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let mut settings = crate::ai::ai_version::PlayerAiSettings::default();
        settings.set_version(player, crate::ai::ai_version::AiVersion::V3);
        world.insert_resource(settings);
        for entry in &mut world.resource_mut::<Players>().0 {
            entry.funds = 0;
        }

        let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
        for x in 0..=2 {
            map.set_terrain(x, 1, Terrain::Plains).unwrap();
        }
        map.set_terrain(1, 1, Terrain::Airport).unwrap();
        map.set_terrain(4, 1, Terrain::City).unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.insert_resource(SquadManager::new());
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::Airport, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 4, y: 1 },
            Property::new(Terrain::City, None, 100),
        ));

        let helicopter_stats = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let helicopter = world
            .spawn((
                player,
                Faction(player),
                GridPosition { x: 2, y: 1 },
                helicopter_stats.clone(),
                CargoCapacity {
                    max: helicopter_stats.max_cargo,
                    loaded: Vec::new(),
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let infantry_stats = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        for _ in 0..2 {
            world.spawn((
                player,
                Faction(player),
                GridPosition { x: 2, y: 1 },
                infantry_stats.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
            ));
        }

        let tank_stats = master_data
            .create_unit_stats(&UnitName(UnitType::Tank.as_str().to_owned()))
            .unwrap();
        let tank = world
            .spawn((
                player,
                Faction(player),
                GridPosition { x: 9_999, y: 9_999 },
                tank_stats,
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let lander_stats = master_data
            .create_unit_stats(&UnitName(UnitType::Lander.as_str().to_owned()))
            .unwrap();
        let lander = world
            .spawn((
                player,
                Faction(player),
                GridPosition { x: 0, y: 1 },
                lander_stats.clone(),
                CargoCapacity {
                    max: lander_stats.max_cargo,
                    loaded: vec![tank],
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        world.entity_mut(tank).insert(Transporting(lander));

        let strategy = analyze_strategy(&mut world, player);
        let primary = strategy
            .campaign_portfolio
            .active_offensives
            .first()
            .unwrap();
        assert!(primary.operation_ready);
        assert!(primary.transport_entities.contains(&helicopter));
        assert!(!primary.transport_entities.contains(&lander));

        plan_squads(&mut world, player);

        let manager = world.resource::<SquadManager>();
        let drop_squad = manager
            .squads
            .iter()
            .find(|squad| squad.transport_entity == Some(lander))
            .expect("unreserved loaded transport must receive a safe Drop squad");
        assert_eq!(
            drop_squad.plan.phase,
            MissionPhase::Transport(TransportPhase::Drop)
        );
        assert_eq!(drop_squad.plan.target_island, None);
        assert_eq!(drop_squad.plan.target, None);
        assert_eq!(drop_squad.cargo_entities, vec![tank]);
    }

    #[test]
    fn v3_non_ready_campaign_assignment_does_not_launch_transport() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let opponent = PlayerId(2);
        let mut settings = crate::ai::ai_version::PlayerAiSettings::default();
        settings.set_version(player, crate::ai::ai_version::AiVersion::V3);
        world.insert_resource(settings);
        for entry in &mut world.resource_mut::<Players>().0 {
            entry.funds = if entry.id == player { 32_700 } else { 0 };
        }

        let origin = GridPosition { x: 0, y: 1 };
        let target = GridPosition { x: 4, y: 1 };
        let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(origin.x, origin.y, Terrain::Port).unwrap();
        map.set_terrain(3, 1, Terrain::Shoal).unwrap();
        map.set_terrain(target.x, target.y, Terrain::Capital)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.insert_resource(SquadManager::new());
        world.spawn((origin, Property::new(Terrain::Capital, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::Capital, Some(opponent), 100)));

        let infantry_stats = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let infantry = world
            .spawn((
                player,
                Faction(player),
                origin,
                infantry_stats,
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let tank_stats = master_data
            .create_unit_stats(&UnitName(UnitType::Tank.as_str().to_owned()))
            .unwrap();
        let tank = world
            .spawn((
                player,
                Faction(player),
                origin,
                tank_stats,
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let lander_stats = master_data
            .create_unit_stats(&UnitName(UnitType::Lander.as_str().to_owned()))
            .unwrap();
        let lander = world
            .spawn((
                player,
                Faction(player),
                origin,
                lander_stats.clone(),
                CargoCapacity {
                    max: lander_stats.max_cargo,
                    loaded: Vec::new(),
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        let strategy = analyze_strategy(&mut world, player);
        let assignment = strategy
            .campaign_portfolio
            .active_offensives
            .first()
            .unwrap();
        assert!(!assignment.operation_ready);
        assert!(assignment.transport_entities.contains(&lander));

        plan_squads(&mut world, player);

        let (forming_id, target_island, forming_cargo) = {
            let manager = world.resource::<SquadManager>();
            let forming = manager
                .squads
                .iter()
                .find(|squad| squad.transport_entity == Some(lander))
                .unwrap();
            assert_eq!(forming.plan.phase, MissionPhase::Forming);
            (
                forming.id,
                forming.plan.target_island.unwrap(),
                forming.cargo_entities.clone(),
            )
        };

        let repeated = analyze_strategy(&mut world, player);
        let repeated_assignment = repeated
            .campaign_portfolio
            .assignment_for(target_island)
            .unwrap();
        assert!(!repeated_assignment.operation_ready);
        assert!(repeated_assignment.transport_entities.contains(&lander));

        plan_squads(&mut world, player);

        {
            let manager = world.resource::<SquadManager>();
            let forming: Vec<_> = manager
                .squads
                .iter()
                .filter(|squad| {
                    squad.plan.target_island == Some(target_island)
                        && squad.plan.phase == MissionPhase::Forming
                })
                .collect();
            assert_eq!(forming.len(), 1);
            assert_eq!(forming[0].transport_entity, Some(lander));
            assert!(
                forming_cargo
                    .iter()
                    .all(|entity| forming[0].cargo_entities.contains(entity)),
                "再分析で既存cargoを失わず、空き容量へ同じ作戦の要員を追加する"
            );
        }

        let second_infantry_stats = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let second_infantry = world
            .spawn((
                player,
                Faction(player),
                origin,
                second_infantry_stats,
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let artillery_stats = master_data
            .create_unit_stats(&UnitName(UnitType::Artillery.as_str().to_owned()))
            .unwrap();
        let artillery = world
            .spawn((
                player,
                Faction(player),
                origin,
                artillery_stats,
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let helicopter_stats = master_data
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let helicopter = world
            .spawn((
                player,
                Faction(player),
                origin,
                helicopter_stats.clone(),
                CargoCapacity {
                    max: helicopter_stats.max_cargo,
                    loaded: Vec::new(),
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        let ready = analyze_strategy(&mut world, player);
        let ready_assignment = ready
            .campaign_portfolio
            .assignment_for(target_island)
            .unwrap()
            .clone();
        assert!(ready_assignment.operation_ready);
        assert!(ready_assignment.transport_entities.contains(&lander));
        assert!(ready_assignment.transport_entities.contains(&helicopter));
        assert!(ready_assignment.capture_entities.contains(&infantry));
        assert!(ready_assignment.capture_entities.contains(&second_infantry));
        assert!(
            ready_assignment.combat_entities.contains(&tank)
                || ready_assignment.capture_entities.contains(&tank)
        );
        // 必要Entity数を満たした後は、高価だからという理由だけで追加cargoを要求しない。
        assert!(!ready_assignment.combat_entities.contains(&artillery));
        assert!(!ready_assignment.capture_entities.contains(&artillery));

        plan_squads(&mut world, player);

        let assert_ready_operation = |world: &World| {
            let manager = world.resource::<SquadManager>();
            let operations: Vec<_> = manager
                .squads
                .iter()
                .filter(|squad| squad.plan.target_island == Some(target_island))
                .collect();
            assert_eq!(operations.len(), ready_assignment.transport_entities.len());
            let original = operations
                .iter()
                .find(|squad| squad.id == forming_id)
                .unwrap();
            assert_eq!(original.transport_entity, Some(lander));

            let mut owned = Vec::new();
            for operation in &operations {
                let transport = operation.transport_entity.unwrap();
                assert_eq!(operation.members, BTreeSet::from([transport]));
                assert_eq!(
                    operation.plan.target,
                    Some(ready_assignment.target_position)
                );
                assert_eq!(
                    operation.plan.phase,
                    MissionPhase::Transport(TransportPhase::Pickup)
                );
                let capacity = world.get::<CargoCapacity>(transport).unwrap();
                let transport_stats = world.get::<UnitStats>(transport).unwrap();
                assert!(operation.cargo_entities.len() <= capacity.max as usize);
                assert!(operation.cargo_entities.iter().all(|cargo| {
                    world.get::<UnitStats>(*cargo).is_some_and(|cargo_stats| {
                        transport_stats
                            .loadable_unit_types
                            .contains(&cargo_stats.unit_type)
                    })
                }));
                owned.extend(operation.members.iter().copied());
                owned.extend(operation.cargo_entities.iter().copied());
            }
            let unique: HashSet<_> = owned.iter().copied().collect();
            assert_eq!(owned.len(), unique.len());
            for entity in ready_assignment
                .transport_entities
                .iter()
                .chain(ready_assignment.capture_entities.iter())
                .chain(ready_assignment.combat_entities.iter())
            {
                assert!(unique.contains(entity));
                let squad_owners = manager
                    .squads
                    .iter()
                    .filter(|squad| {
                        squad.members.contains(entity) || squad.cargo_entities.contains(entity)
                    })
                    .count();
                assert_eq!(squad_owners, 1);
            }
        };
        assert_ready_operation(&world);

        plan_squads(&mut world, player);
        assert_ready_operation(&world);
    }

    struct ReadyAssaultFixture {
        world: World,
        load_schedule: Schedule,
        target_island: crate::ai::islands::IslandId,
        lander: Entity,
        helicopter: Entity,
        cargo: [Entity; 4],
    }

    fn setup_ready_assault_reconciliation_world() -> ReadyAssaultFixture {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let opponent = PlayerId(2);
        let mut settings = crate::ai::ai_version::PlayerAiSettings::default();
        settings.set_version(player, crate::ai::ai_version::AiVersion::V3);
        world.insert_resource(settings);
        world.resource_mut::<MatchState>().current_phase = Phase::Main;
        for entry in &mut world.resource_mut::<Players>().0 {
            entry.funds = 0;
        }

        let port = GridPosition { x: 0, y: 1 };
        let base = GridPosition { x: 1, y: 1 };
        let target = GridPosition { x: 4, y: 1 };
        let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(port.x, port.y, Terrain::Port).unwrap();
        map.set_terrain(base.x, base.y, Terrain::Capital).unwrap();
        map.set_terrain(3, 1, Terrain::Shoal).unwrap();
        map.set_terrain(target.x, target.y, Terrain::Capital)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.spawn((base, Property::new(Terrain::Capital, Some(player), 100)));
        world.spawn((target, Property::new(Terrain::Capital, Some(opponent), 100)));

        let lander = world
            .spawn((
                player,
                Faction(player),
                port,
                UnitStats {
                    unit_type: UnitType::Lander,
                    movement_type: MovementType::Ship,
                    max_movement: 5,
                    max_cargo: 2,
                    loadable_unit_types: vec![
                        UnitType::Infantry,
                        UnitType::Tank,
                        UnitType::Artillery,
                    ],
                    cost: 16_500,
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
                HasMoved(false),
                ActionCompleted(false),
                Fuel {
                    current: 90,
                    max: 90,
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let helicopter = world
            .spawn((
                player,
                Faction(player),
                base,
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    movement_type: MovementType::Air,
                    max_movement: 7,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    cost: 4_000,
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
                HasMoved(false),
                ActionCompleted(false),
                Fuel {
                    current: 60,
                    max: 60,
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        let spawn_cargo = |world: &mut World,
                           position: GridPosition,
                           unit_type: UnitType,
                           movement_type: MovementType,
                           can_capture: bool,
                           cost: u32| {
            world
                .spawn((
                    player,
                    Faction(player),
                    position,
                    UnitStats {
                        unit_type,
                        movement_type,
                        max_movement: 5,
                        can_capture,
                        cost,
                        ..UnitStats::mock()
                    },
                    HasMoved(false),
                    ActionCompleted(false),
                    Fuel {
                        current: 70,
                        max: 70,
                    },
                    Health {
                        current: 100,
                        max: 100,
                    },
                ))
                .id()
        };
        let loaded_infantry = spawn_cargo(
            &mut world,
            GridPosition { x: 9_999, y: 9_999 },
            UnitType::Infantry,
            MovementType::Infantry,
            true,
            1_000,
        );
        let waiting_infantry = spawn_cargo(
            &mut world,
            base,
            UnitType::Infantry,
            MovementType::Infantry,
            true,
            1_000,
        );
        let tank = spawn_cargo(
            &mut world,
            port,
            UnitType::Tank,
            MovementType::Tank,
            false,
            7_000,
        );
        let artillery = spawn_cargo(
            &mut world,
            port,
            UnitType::Artillery,
            MovementType::Artillery,
            false,
            6_200,
        );
        world
            .get_mut::<CargoCapacity>(helicopter)
            .unwrap()
            .loaded
            .push(loaded_infantry);
        world
            .entity_mut(loaded_infantry)
            .insert(Transporting(helicopter));

        // 同一島の既存Formingを輸送役ごとに分け、さらに空の重複Squadも混在させる。
        let mut manager = SquadManager::new();
        let mut lander_squad = manager.create_owned_squad(MissionType::Transport, player);
        lander_squad.members.insert(lander);
        lander_squad.transport_entity = Some(lander).into();
        lander_squad.cargo_entities = vec![tank, artillery].into();
        lander_squad.plan.target_island = Some(target_island);
        lander_squad.plan.target = Some(target);
        let mut helicopter_squad = manager.create_owned_squad(MissionType::Transport, player);
        helicopter_squad.members.insert(helicopter);
        helicopter_squad.transport_entity = Some(helicopter).into();
        helicopter_squad.cargo_entities = vec![loaded_infantry, waiting_infantry].into();
        helicopter_squad.plan.target_island = Some(target_island);
        helicopter_squad.plan.target = Some(target);
        let mut duplicate = manager.create_owned_squad(MissionType::Transport, player);
        duplicate.plan.target_island = Some(target_island);
        duplicate.plan.target = Some(target);
        world.insert_resource(manager);

        let mut load_schedule = Schedule::default();
        crate::systems::add_main_game_systems(&mut load_schedule);
        ReadyAssaultFixture {
            world,
            load_schedule,
            target_island,
            lander,
            helicopter,
            cargo: [loaded_infantry, waiting_infantry, tank, artillery],
        }
    }

    fn reset_transport_actions(world: &mut World) {
        let mut query = world.query::<(&mut ActionCompleted, &mut HasMoved)>();
        for (mut action, mut moved) in query.iter_mut(world) {
            action.0 = false;
            moved.0 = false;
        }
    }

    #[test]
    fn ready_assault_partitions_all_cargo_across_compatible_transports_and_loads() {
        let mut fixture = setup_ready_assault_reconciliation_world();
        let player = PlayerId(1);
        let strategy = analyze_strategy(&mut fixture.world, player);
        let assignment = strategy
            .campaign_portfolio
            .assignment_for(fixture.target_island)
            .unwrap()
            .clone();
        assert!(assignment.operation_ready);
        assert_eq!(assignment.transport_entities.len(), 2);
        assert_eq!(assignment.capture_entities.len(), 2);
        assert_eq!(assignment.combat_entities.len(), 2);

        plan_squads(&mut fixture.world, player);

        {
            let manager = fixture.world.resource::<SquadManager>();
            let transport_squads: Vec<_> = manager
                .squads
                .iter()
                .filter(|squad| squad.plan.target_island == Some(fixture.target_island))
                .collect();
            assert_eq!(transport_squads.len(), 2);
            for squad in &transport_squads {
                let transport = squad.transport_entity.unwrap();
                let capacity = fixture.world.get::<CargoCapacity>(transport).unwrap();
                let stats = fixture.world.get::<UnitStats>(transport).unwrap();
                assert_eq!(squad.members, BTreeSet::from([transport]));
                assert!(squad.cargo_entities.len() <= capacity.max as usize);
                assert!(squad.cargo_entities.iter().all(|cargo| {
                    fixture
                        .world
                        .get::<UnitStats>(*cargo)
                        .is_some_and(|cargo_stats| {
                            stats.loadable_unit_types.contains(&cargo_stats.unit_type)
                        })
                }));
            }
            let helicopter_squad = transport_squads
                .iter()
                .find(|squad| squad.transport_entity == Some(fixture.helicopter))
                .unwrap();
            assert!(helicopter_squad.cargo_entities.contains(&fixture.cargo[0]));

            for entity in assignment
                .transport_entities
                .iter()
                .chain(assignment.capture_entities.iter())
                .chain(assignment.combat_entities.iter())
            {
                let owners = transport_squads
                    .iter()
                    .filter(|squad| {
                        squad.members.contains(entity) || squad.cargo_entities.contains(entity)
                    })
                    .count();
                assert_eq!(
                    owners, 1,
                    "reserved Entity must have exactly one Squad owner"
                );
            }
        }

        // 既存Pickup意思決定とLoad systemを実行し、両輸送役が実搭載完了後に進めることを確認する。
        let mut executed_steps = Vec::new();
        for _ in 0..8 {
            reset_transport_actions(&mut fixture.world);
            let pickup_ids: Vec<_> = fixture
                .world
                .resource::<SquadManager>()
                .squads
                .iter()
                .filter(|squad| {
                    squad.plan.target_island == Some(fixture.target_island)
                        && squad.plan.phase == MissionPhase::Transport(TransportPhase::Pickup)
                })
                .map(|squad| squad.id)
                .collect();
            for squad_id in pickup_ids {
                let mut manager = fixture.world.remove_resource::<SquadManager>().unwrap();
                let mut squad = manager
                    .edit_each()
                    .find(|squad| squad.id == squad_id)
                    .unwrap();
                let (entity, command) = execute_transport_squad_step(
                    &mut fixture.world,
                    &mut squad,
                    &HashSet::default(),
                )
                .expect("Pickup squad must advance toward loading");
                assert!(matches!(
                    command,
                    crate::ai::command::AiCommand::Load { .. }
                        | crate::ai::command::AiCommand::Wait { .. }
                ));
                executed_steps.push((entity, format!("{command:?}")));
                fixture.world.insert_resource(manager);
                crate::ai::runtime::issue::execute_ai_command(&mut fixture.world, entity, command);
                fixture.load_schedule.run(&mut fixture.world);
            }
            update_squads(&mut fixture.world, player);
            let all_ready = fixture
                .world
                .resource::<SquadManager>()
                .squads
                .iter()
                .filter(|squad| squad.plan.target_island == Some(fixture.target_island))
                .all(|squad| {
                    matches!(
                        squad.plan.phase,
                        MissionPhase::Transport(TransportPhase::Transit | TransportPhase::Drop)
                    )
                });
            if all_ready {
                break;
            }
        }

        let manager = fixture.world.resource::<SquadManager>();
        let transport_squads: Vec<_> = manager
            .squads
            .iter()
            .filter(|squad| squad.plan.target_island == Some(fixture.target_island))
            .collect();
        assert_eq!(transport_squads.len(), 2);
        let final_states: Vec<_> = transport_squads
            .iter()
            .map(|squad| {
                (
                    squad.transport_entity,
                    squad.plan.phase.clone(),
                    squad.cargo_entities.clone(),
                    squad.transport_entity.and_then(|transport| {
                        fixture
                            .world
                            .get::<CargoCapacity>(transport)
                            .map(|capacity| capacity.loaded.clone())
                    }),
                    squad.transport_entity.and_then(|transport| {
                        fixture
                            .world
                            .get::<UnitStats>(transport)
                            .map(|stats| stats.unit_type)
                    }),
                    squad.transport_entity.and_then(|transport| {
                        fixture.world.get::<GridPosition>(transport).copied()
                    }),
                    squad.plan.pickup_position,
                )
            })
            .collect();
        assert!(
            transport_squads.iter().all(|squad| matches!(
                squad.plan.phase,
                MissionPhase::Transport(TransportPhase::Transit | TransportPhase::Drop)
            )),
            "final transport states: {final_states:?}; steps: {executed_steps:?}"
        );
        for squad in transport_squads {
            let transport = squad.transport_entity.unwrap();
            let capacity = fixture.world.get::<CargoCapacity>(transport).unwrap();
            assert_eq!(capacity.loaded.len(), squad.cargo_entities.len());
            assert!(capacity.loaded.len() <= capacity.max as usize);
            assert!(capacity.loaded.iter().all(|cargo| {
                fixture
                    .world
                    .get::<Transporting>(*cargo)
                    .is_some_and(|transporting| transporting.0 == transport)
            }));
        }
    }

    #[test]
    fn ready_assault_preserves_loaded_drop_squad_while_reconciling_forming_transport() {
        let mut fixture = setup_ready_assault_reconciliation_world();
        let player = PlayerId(1);
        let drop_position = GridPosition { x: 4, y: 1 };

        // Lander側は既に上陸地点まで到達済み、Helicopter側だけがFormingの混在状態を作る。
        fixture
            .world
            .get_mut::<CargoCapacity>(fixture.lander)
            .unwrap()
            .loaded = vec![fixture.cargo[2], fixture.cargo[3]];
        for cargo in [fixture.cargo[2], fixture.cargo[3]] {
            fixture
                .world
                .entity_mut(cargo)
                .insert(Transporting(fixture.lander));
        }
        {
            let mut manager = fixture.world.remove_resource::<SquadManager>().unwrap();
            manager.squads.retain(|squad| squad.id != SquadId(2));
            let mut lander_squad = manager
                .edit_each()
                .find(|squad| squad.transport_entity == Some(fixture.lander))
                .unwrap();
            lander_squad.plan.phase = MissionPhase::Transport(TransportPhase::Drop);
            lander_squad.plan.pickup_position = Some(GridPosition { x: 0, y: 1 });
            lander_squad.plan.drop_position = Some(drop_position);
            fixture.world.insert_resource(manager);
        }

        let assignment = analyze_strategy(&mut fixture.world, player)
            .campaign_portfolio
            .assignment_for(fixture.target_island)
            .unwrap()
            .clone();
        assert!(assignment.operation_ready);
        assert!(assignment.transport_entities.contains(&fixture.lander));
        assert!(assignment.transport_entities.contains(&fixture.helicopter));

        plan_squads(&mut fixture.world, player);
        let snapshot = {
            let manager = fixture.world.resource::<SquadManager>();
            let mut squads: Vec<_> = manager
                .squads
                .iter()
                .filter(|squad| squad.plan.target_island == Some(fixture.target_island))
                .map(|squad| {
                    (
                        squad.id,
                        squad.transport_entity,
                        squad.members.clone(),
                        squad.cargo_entities.clone(),
                        squad.plan.phase.clone(),
                        squad.plan.pickup_position,
                        squad.plan.drop_position,
                    )
                })
                .collect();
            squads.sort_by_key(|squad| squad.0.0);
            assert_eq!(squads.len(), 2, "空のlive輸送Squadを残さない");

            let lander = squads
                .iter()
                .find(|squad| squad.1 == Some(fixture.lander))
                .expect("既存Lander Squadをtransport一致で再利用する");
            assert_eq!(lander.0, SquadId(0));
            assert_eq!(
                lander.4,
                MissionPhase::Transport(TransportPhase::Drop),
                "実搭載済みDropをTransitへ後退させない"
            );
            assert_eq!(lander.6, Some(drop_position));
            assert_eq!(lander.2, BTreeSet::from([fixture.lander]));
            assert_eq!(
                lander.3.iter().copied().collect::<HashSet<_>>(),
                HashSet::from_iter([fixture.cargo[2], fixture.cargo[3]])
            );

            let helicopter = squads
                .iter()
                .find(|squad| squad.1 == Some(fixture.helicopter))
                .expect("Forming Helicopterを別partition Squadとして再利用する");
            assert_eq!(helicopter.0, SquadId(1));
            assert_eq!(
                helicopter.4,
                MissionPhase::Transport(TransportPhase::Pickup)
            );
            squads
        };

        plan_squads(&mut fixture.world, player);
        let manager = fixture.world.resource::<SquadManager>();
        let mut repeated: Vec<_> = manager
            .squads
            .iter()
            .filter(|squad| squad.plan.target_island == Some(fixture.target_island))
            .map(|squad| {
                (
                    squad.id,
                    squad.transport_entity,
                    squad.members.clone(),
                    squad.cargo_entities.clone(),
                    squad.plan.phase.clone(),
                    squad.plan.pickup_position,
                    squad.plan.drop_position,
                )
            })
            .collect();
        repeated.sort_by_key(|squad| squad.0.0);
        assert_eq!(repeated, snapshot);
    }

    #[test]
    fn ready_assault_does_not_reverse_load_landed_handoff_during_drop_forming_replan() {
        let mut fixture = setup_ready_assault_reconciliation_world();
        let player = PlayerId(1);
        let target = GridPosition { x: 4, y: 1 };
        let loaded_cargo = fixture.cargo[3];
        let landed_cargo = fixture.cargo[1];
        let waiting_cargo = fixture.cargo[2];
        {
            let mut stats = fixture.world.get_mut::<UnitStats>(waiting_cargo).unwrap();
            stats.unit_type = UnitType::Infantry;
            stats.movement_type = MovementType::Infantry;
            stats.can_capture = true;
        }

        fixture
            .world
            .get_mut::<CargoCapacity>(fixture.lander)
            .unwrap()
            .loaded = vec![loaded_cargo];
        fixture
            .world
            .entity_mut(loaded_cargo)
            .insert(Transporting(fixture.lander));
        *fixture.world.get_mut::<GridPosition>(landed_cargo).unwrap() = target;
        fixture
            .world
            .entity_mut(landed_cargo)
            .remove::<Transporting>();
        fixture.world.spawn((
            PlayerId(2),
            Faction(PlayerId(2)),
            target,
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                can_capture: true,
                cost: 1_000,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let (drop_id, local_id) = {
            let mut manager = fixture.world.remove_resource::<SquadManager>().unwrap();
            manager.squads.retain(|squad| squad.id != SquadId(2));
            let mut lander_squad = manager
                .edit_each()
                .find(|squad| squad.transport_entity == Some(fixture.lander))
                .unwrap();
            lander_squad.cargo_entities = vec![loaded_cargo].into();
            lander_squad.plan.phase = MissionPhase::Transport(TransportPhase::Drop);
            lander_squad.plan.drop_position = Some(target);
            let drop_id = lander_squad.id;

            let mut local = manager.create_squad(MissionType::Capture);
            local.members.insert(landed_cargo);
            local.plan.target_island = Some(fixture.target_island);
            local.plan.target = Some(target);
            local.plan.phase = MissionPhase::MovingToTarget;
            let local_id = local.id;
            fixture.world.insert_resource(manager);
            (drop_id, local_id)
        };

        let assignment = analyze_strategy(&mut fixture.world, player)
            .campaign_portfolio
            .assignment_for(fixture.target_island)
            .unwrap()
            .clone();
        assert!(assignment.operation_ready);
        assert!(assignment.capture_entities.contains(&landed_cargo));

        plan_squads(&mut fixture.world, player);
        let snapshot = {
            let manager = fixture.world.resource::<SquadManager>();
            let drop = manager
                .squads
                .iter()
                .find(|squad| squad.id == drop_id)
                .expect("Drop Squadを維持する");
            assert_eq!(
                drop.plan.phase,
                MissionPhase::Transport(TransportPhase::Drop)
            );
            assert_eq!(drop.cargo_entities, vec![loaded_cargo]);
            assert_eq!(drop.plan.drop_position, Some(target));

            let local = manager
                .squads
                .iter()
                .find(|squad| squad.id == local_id)
                .expect("上陸済み戦闘Entityの通常Squadを維持する");
            assert_eq!(local.plan.mission_type, MissionType::Capture);
            assert!(local.members.contains(&landed_cargo));
            assert_eq!(local.plan.target_island, Some(fixture.target_island));

            let forming_transport = manager
                .squads
                .iter()
                .find(|squad| squad.transport_entity == Some(fixture.helicopter))
                .expect("残る洋上cargoだけを別輸送役へ割り当てる");
            assert!(!forming_transport.cargo_entities.contains(&landed_cargo));
            assert!(
                forming_transport.cargo_entities.iter().all(|cargo| [
                    fixture.cargo[0],
                    waiting_cargo
                ]
                .contains(cargo))
            );
            assert!(manager.squads.iter().all(|squad| {
                squad.plan.mission_type != MissionType::Transport
                    || !squad.cargo_entities.contains(&landed_cargo)
            }));

            manager
                .squads
                .iter()
                .map(|squad| {
                    (
                        squad.id,
                        squad.plan.mission_type.clone(),
                        squad.members.clone(),
                        squad.transport_entity,
                        squad.cargo_entities.clone(),
                        squad.plan.target_island,
                        squad.plan.target,
                        squad.plan.phase.clone(),
                        squad.plan.pickup_position,
                        squad.plan.drop_position,
                    )
                })
                .collect::<Vec<_>>()
        };

        plan_squads(&mut fixture.world, player);
        let manager = fixture.world.resource::<SquadManager>();
        let repeated = manager
            .squads
            .iter()
            .map(|squad| {
                (
                    squad.id,
                    squad.plan.mission_type.clone(),
                    squad.members.clone(),
                    squad.transport_entity,
                    squad.cargo_entities.clone(),
                    squad.plan.target_island,
                    squad.plan.target,
                    squad.plan.phase.clone(),
                    squad.plan.pickup_position,
                    squad.plan.drop_position,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(repeated, snapshot);
    }

    #[test]
    fn assault_uses_ready_package_without_waiting_for_unreachable_extra_combat() {
        let mut fixture = setup_ready_assault_reconciliation_world();
        let player = PlayerId(1);
        fixture
            .world
            .get_mut::<CargoCapacity>(fixture.helicopter)
            .unwrap()
            .loaded
            .clear();
        fixture
            .world
            .entity_mut(fixture.cargo[0])
            .remove::<Transporting>();
        *fixture
            .world
            .get_mut::<GridPosition>(fixture.cargo[0])
            .unwrap() = GridPosition { x: 1, y: 1 };
        // Artilleryを通行不能なSeaへ置き、Landerとの合流だけを不成立にする。
        *fixture
            .world
            .get_mut::<GridPosition>(fixture.cargo[3])
            .unwrap() = GridPosition { x: 2, y: 1 };

        // 既存FormingはLanderだけを保持し、新しいHelicopterはfree poolに残る状況を作る。
        let target = GridPosition { x: 4, y: 1 };
        let target_island = fixture.target_island;
        let mut manager = SquadManager::new();
        let mut forming = manager.create_squad(MissionType::Transport);
        forming.members.insert(fixture.lander);
        forming.transport_entity = Some(fixture.lander).into();
        forming.cargo_entities = vec![fixture.cargo[2]].into();
        forming.plan.target_island = Some(target_island);
        forming.plan.target = Some(target);
        fixture.world.insert_resource(manager);

        let strategy = analyze_strategy(&mut fixture.world, player);
        let assignment = strategy
            .campaign_portfolio
            .assignment_for(target_island)
            .unwrap();
        assert!(assignment.operation_ready);
        assert!(assignment.transport_entities.contains(&fixture.lander));
        assert!(assignment.transport_entities.contains(&fixture.helicopter));
        // 合流不能な追加戦闘cargoは予約せず、到達可能な完成packageだけで出航する。
        assert!(!assignment.combat_entities.contains(&fixture.cargo[3]));

        plan_squads(&mut fixture.world, player);

        let manager = fixture.world.resource::<SquadManager>();
        let operation_squads: Vec<_> = manager
            .squads
            .iter()
            .filter(|squad| squad.plan.target_island == Some(target_island))
            .collect();
        assert!(!operation_squads.is_empty());
        assert!(
            operation_squads
                .iter()
                .any(|squad| squad.plan.phase != MissionPhase::Forming)
        );
    }

    #[test]
    fn ready_assault_preserves_unincorporated_loaded_forming_squad() {
        let mut fixture = setup_ready_assault_reconciliation_world();
        let player = PlayerId(1);
        let assignment = analyze_strategy(&mut fixture.world, player)
            .campaign_portfolio
            .assignment_for(fixture.target_island)
            .unwrap()
            .clone();
        assert!(assignment.operation_ready);

        let extra_cargo = fixture
            .world
            .spawn((
                player,
                Faction(player),
                GridPosition { x: 9_999, y: 9_999 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let extra_transport = fixture
            .world
            .spawn((
                player,
                Faction(player),
                GridPosition { x: 0, y: 1 },
                UnitStats {
                    unit_type: UnitType::Lander,
                    movement_type: MovementType::Ship,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Tank],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: vec![extra_cargo],
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        fixture
            .world
            .entity_mut(extra_cargo)
            .insert(Transporting(extra_transport));
        {
            let mut manager = fixture.world.remove_resource::<SquadManager>().unwrap();
            let mut extra = manager.create_squad(MissionType::Transport);
            extra.members.insert(extra_transport);
            extra.transport_entity = Some(extra_transport).into();
            extra.cargo_entities = vec![extra_cargo].into();
            extra.plan.target_island = Some(fixture.target_island);
            extra.plan.target = Some(assignment.target_position);
            fixture.world.insert_resource(manager);
        }

        let mut manager = fixture.world.remove_resource::<SquadManager>().unwrap();
        assert!(!reconcile_ready_forming_campaign_squad(
            &fixture.world,
            &mut manager,
            player,
            &assignment,
        ));
        let preserved = manager
            .squads
            .iter()
            .find(|squad| squad.transport_entity == Some(extra_transport))
            .expect("assignment外transportの実搭載状態を持つForming Squadは削除しない");
        assert_eq!(preserved.plan.phase, MissionPhase::Forming);
        assert_eq!(preserved.cargo_entities, vec![extra_cargo]);
    }

    #[test]
    fn ready_assault_removes_duplicate_forming_squads_idempotently() {
        let mut fixture = setup_ready_assault_reconciliation_world();
        let player = PlayerId(1);
        let strategy = analyze_strategy(&mut fixture.world, player);
        assert!(
            strategy
                .campaign_portfolio
                .assignment_for(fixture.target_island)
                .is_some_and(|assignment| assignment.operation_ready)
        );

        plan_squads(&mut fixture.world, player);
        let snapshot = {
            let manager = fixture.world.resource::<SquadManager>();
            let mut squads: Vec<_> = manager
                .squads
                .iter()
                .filter(|squad| squad.plan.target_island == Some(fixture.target_island))
                .map(|squad| {
                    (
                        squad.id,
                        squad.transport_entity,
                        squad.members.clone(),
                        squad.cargo_entities.clone(),
                        squad.plan.phase.clone(),
                        squad.plan.pickup_position,
                    )
                })
                .collect();
            squads.sort_by_key(|squad| squad.0.0);
            assert_eq!(squads.len(), 2);
            assert_eq!(squads[0].0, SquadId(0));
            assert_eq!(squads[1].0, SquadId(1));
            assert!(squads.iter().all(|squad| !squad.2.is_empty()));
            squads
        };

        plan_squads(&mut fixture.world, player);
        let manager = fixture.world.resource::<SquadManager>();
        let mut repeated: Vec<_> = manager
            .squads
            .iter()
            .filter(|squad| squad.plan.target_island == Some(fixture.target_island))
            .map(|squad| {
                (
                    squad.id,
                    squad.transport_entity,
                    squad.members.clone(),
                    squad.cargo_entities.clone(),
                    squad.plan.phase.clone(),
                    squad.plan.pickup_position,
                )
            })
            .collect();
        repeated.sort_by_key(|squad| squad.0.0);
        assert_eq!(repeated, snapshot);
        assert!(manager.squads.iter().all(|squad| {
            squad.plan.target_island != Some(fixture.target_island)
                || !squad.members.is_empty()
                || squad.plan.mission_type != MissionType::Transport
        }));
        assert!(repeated.iter().any(|squad| squad.1 == Some(fixture.lander)));
        assert!(
            repeated
                .iter()
                .any(|squad| squad.1 == Some(fixture.helicopter))
        );
    }

    fn setup_transport_phase_world() -> (World, Entity, Entity, Entity, crate::ai::islands::IslandId)
    {
        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Shoal).unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let target_island = island_map
            .get_island_at(&GridPosition { x: 2, y: 0 })
            .unwrap()
            .id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);

        let player = PlayerId(1);
        let capture = world
            .spawn((
                Faction(player),
                GridPosition { x: 9999, y: 9999 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let combat = world
            .spawn((
                Faction(player),
                GridPosition { x: 9999, y: 9999 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    can_capture: false,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Lander,
                    movement_type: MovementType::Ship,
                    max_movement: 6,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry, UnitType::Tank],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: vec![capture, combat],
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        world.entity_mut(capture).insert(Transporting(transport));
        world.entity_mut(combat).insert(Transporting(transport));
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(Terrain::City, Some(PlayerId(2)), 100),
        ));
        (world, transport, capture, combat, target_island)
    }

    #[test]
    fn transport_pickup_waits_until_all_cargo_are_loaded() {
        let (mut world, transport, capture, combat, target_island) = setup_transport_phase_world();
        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .retain(|entity| *entity == capture);
        world.entity_mut(combat).remove::<Transporting>();
        *world.get_mut::<GridPosition>(combat).unwrap() = GridPosition { x: 0, y: 0 };

        let mut manager = SquadManager::new();
        let mut squad = manager.create_squad(MissionType::Transport).clone();
        squad.members.insert(transport);
        squad.transport_entity = Some(transport).into();
        squad.cargo_entities = vec![capture, combat].into();
        squad.plan.target_island = Some(target_island);
        squad.plan.phase = MissionPhase::Transport(TransportPhase::Pickup);

        assert!(!update_transport_squad_phase(&mut world, &mut squad.edit()));
        assert_eq!(
            squad.plan.phase,
            MissionPhase::Transport(TransportPhase::Pickup)
        );

        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .push(combat);
        world.entity_mut(combat).insert(Transporting(transport));
        *world.get_mut::<GridPosition>(combat).unwrap() = GridPosition { x: 9999, y: 9999 };
        assert!(!update_transport_squad_phase(&mut world, &mut squad.edit()));
        assert_eq!(
            squad.plan.phase,
            MissionPhase::Transport(TransportPhase::Transit)
        );
    }

    #[test]
    fn loaded_assault_wave_waits_for_departure_authorization() {
        let (mut world, transport, capture, combat, target_island) = setup_transport_phase_world();
        let mut squad = SquadManager::new()
            .create_squad(MissionType::Transport)
            .clone();
        squad.members.insert(transport);
        squad.transport_entity = Some(transport).into();
        squad.cargo_entities = vec![capture, combat].into();
        squad.plan.target_island = Some(target_island);
        squad.plan.phase = MissionPhase::Transport(TransportPhase::Pickup);
        squad.plan.departure_authorized = false;

        assert!(!update_transport_squad_phase(&mut world, &mut squad.edit()));
        assert_eq!(
            squad.plan.phase,
            MissionPhase::Transport(TransportPhase::Pickup),
            "搭載済みでも兵站gate前のAssaultを逐次発進させない"
        );

        squad.plan.departure_authorized = true;
        assert!(!update_transport_squad_phase(&mut world, &mut squad.edit()));
        assert_eq!(
            squad.plan.phase,
            MissionPhase::Transport(TransportPhase::Transit)
        );
    }

    /// campaign責務の移管はkeyed storeへ記録し、旧責務の索引を残さない。
    #[test]
    fn campaign_responsibilities_update_keyed_proposals_without_roster_reimport() {
        let player = PlayerId(1);
        let island = crate::ai::islands::IslandId(0);
        let mut world = World::new();
        let first = world.spawn(Faction(player)).id();
        let second = world.spawn(Faction(player)).id();
        let mut manager = SquadManager::default();
        let mut original = manager.create_owned_squad(MissionType::Attack, player);
        original.plan.target_island = Some(island);
        original.members.extend([first, second]);
        let original_id = original.id;
        assign_campaign_responsibilities(
            &world,
            &mut manager,
            player,
            island,
            vec![CampaignResponsibility {
                mission_type: MissionType::Attack,
                target: GridPosition { x: 2, y: 0 },
                members: vec![first, second],
            }],
            &[MissionType::Attack],
        );
        let proposed = manager.squads.iter().find(|s| s.id == original_id).unwrap();
        assert_eq!(proposed.members, BTreeSet::from([first, second]));
    }

    /// mixed・他playerの保護されたrosterから、自軍EntityをReserveへ横取りしない。
    #[test]
    fn explicit_reserve_keeps_protected_mixed_and_foreign_rosters_unavailable() {
        for owner in [None, Some(PlayerId(2))] {
            let mut world = World::new();
            world.insert_resource(Map::new(3, 1, Terrain::Plains, GridTopology::Square));
            world.insert_resource(MasterDataRegistry::load().unwrap());
            let player = PlayerId(1);
            let own = world
                .spawn((
                    Faction(player),
                    GridPosition { x: 0, y: 0 },
                    UnitStats::mock(),
                ))
                .id();
            let foreign = world
                .spawn((
                    Faction(PlayerId(2)),
                    GridPosition { x: 1, y: 0 },
                    UnitStats::mock(),
                ))
                .id();
            let mut manager = SquadManager::default();
            let protected = {
                let mut squad = manager.create_squad(MissionType::Attack);
                squad.plan.owner_id = owner;
                squad.members.extend([own, foreign]);
                squad.id
            };
            reconcile_unique_operation_assignments(&mut world, &mut manager, player);
            assert!(
                world
                    .resource::<UnitOperationRegistry>()
                    .assignment(own)
                    .is_none()
            );
            assign_explicit_reserve_squads(&mut world, &mut manager, player, &[]);
            assert!(
                manager
                    .squads
                    .iter()
                    .find(|squad| squad.id == protected)
                    .unwrap()
                    .members
                    .contains(&own)
            );
            assert!(
                manager
                    .squads
                    .iter()
                    .all(|squad| squad.plan.owner_id != Some(player)
                        || !squad.members.contains(&own)),
                "保護されたmixed/foreign参照を新しい自軍Squadへ重複加入させない: {owner:?}"
            );
        }
    }

    #[test]
    fn unassigned_empty_transport_receives_explicit_reserve_away_from_airport() {
        let mut world = World::new();
        let mut map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Airport).unwrap();
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let player = PlayerId(1);
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Airport, Some(player), 100),
        ));
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    movement_type: MovementType::Air,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
            ))
            .id();
        let mut manager = SquadManager::new();
        manager.solo_fallbacks.insert(transport);

        assign_explicit_reserve_squads(&mut world, &mut manager, player, &[]);

        let reserve = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&transport))
            .expect("未割当輸送機にも明示任務を与える");
        assert_eq!(reserve.plan.mission_type, MissionType::Reserve);
        assert_ne!(reserve.plan.target, Some(GridPosition { x: 0, y: 0 }));
        assert!(
            !manager.solo_fallbacks.contains(&transport),
            "旧SoloFallbackを残して実行任務ありと誤認しない"
        );
    }

    #[test]
    fn end_turn_reconcile_keeps_existing_reserve_without_new_unassigned_entity() {
        let mut world = World::new();
        let player = PlayerId(1);
        let mut ai_settings = crate::ai::PlayerAiSettings::new();
        ai_settings.set_version(player, crate::ai::AiVersion::V4);
        world.insert_resource(ai_settings);
        let unit = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats::mock(),
            ))
            .id();
        let mut manager = SquadManager::new();
        let reserve_id = {
            let mut reserve = manager.create_owned_squad(MissionType::Reserve, player);
            reserve.members.insert(unit);
            reserve.plan.target = Some(GridPosition { x: 1, y: 0 });
            reserve.id
        };
        world.insert_resource(manager);

        reconcile_v4_end_turn_reserves(&mut world, player);

        let manager = world.resource::<SquadManager>();
        let reserve = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&unit))
            .expect("新規未割当がなければ既存Reserveを同一手番中に解体しない");
        assert_eq!(reserve.id, reserve_id);
        assert_eq!(reserve.plan.mission_type, MissionType::Reserve);
    }

    #[test]
    fn transported_cargo_does_not_trigger_end_turn_reassignment() {
        let mut world = World::new();
        let player = PlayerId(1);
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats::mock(),
            ))
            .id();
        let cargo = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats::mock(),
                Transporting(transport),
            ))
            .id();
        let mut manager = SquadManager::new();
        OperationLedger::proposals(&mut manager).create_owned(
            MissionType::Reserve,
            player,
            BTreeSet::from([transport]),
            None,
            Vec::new(),
        );

        let unassigned = unassigned_in_roster_view(&world, &manager, player);

        assert!(!unassigned.contains(&cargo));
        assert!(unassigned.is_empty());
    }

    #[test]
    fn campaign_owner_without_squad_is_reconnected_to_live_campaign_before_reserve() {
        let mut world = World::new();
        let map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map
            .get_island_at(&GridPosition { x: 1, y: 0 })
            .unwrap()
            .id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let player = PlayerId(1);
        let unit = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    can_capture: true,
                    ..UnitStats::mock()
                },
            ))
            .id();
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::City, None, 100),
        ));
        let mut operation_registry = UnitOperationRegistry::default();
        operation_registry.assign(
            unit,
            UnitOperationAssignment {
                owner: OperationOwner::Campaign {
                    player_id: player,
                    island_id,
                },
                squad_id: None,
                role: OperationUnitRole::Cargo,
                assigned_turn: 1,
            },
        );
        world.insert_resource(operation_registry);
        let mut manager = SquadManager::new();
        let mut stale = manager.create_owned_squad(MissionType::Attack, player);
        stale.members.insert(unit);
        stale.plan.target = Some(GridPosition { x: 2, y: 0 });

        reconcile_unique_operation_assignments(&mut world, &mut manager, player);
        let assignment = crate::ai::island_campaign::IslandCampaignAssignment {
            island_id,
            decision: crate::ai::island_campaign::IslandCampaignDecision::Secure,
            target_position: GridPosition { x: 1, y: 0 },
            capture_target_positions: vec![GridPosition { x: 1, y: 0 }],
            priority_enemy_types: Vec::new(),
            requirement: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            purchase_shortfall: crate::ai::island_campaign::IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 0,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: true,
        };
        reconnect_current_v4_operations(&mut world, &mut manager, player, &[assignment]);
        reconcile_unique_operation_assignments(&mut world, &mut manager, player);

        let capture = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&unit))
            .expect("campaign ownerだけ残ったEntityを実Squadへ再接続する");
        assert_eq!(capture.plan.mission_type, MissionType::Capture);
        assert_eq!(capture.plan.target_island, Some(island_id));
        assert_eq!(
            world
                .resource::<UnitOperationRegistry>()
                .assignment(unit)
                .and_then(|assignment| assignment.squad_id),
            Some(capture.id)
        );
    }

    #[test]
    fn campaign_placeholder_absorbs_and_launches_later_partial_wave() {
        let mut world = World::new();
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Hex);
        map.set_terrain(0, 0, Terrain::Plains).unwrap();
        map.set_terrain(2, 0, Terrain::City).unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let target_island = island_map
            .get_island_at(&GridPosition { x: 2, y: 0 })
            .unwrap()
            .id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let player = PlayerId(2);
        let cargo = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    can_capture: true,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    movement_type: MovementType::Air,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
            ))
            .id();
        let mut manager = SquadManager::new();
        let placeholder_id = {
            let mut placeholder = manager.create_owned_squad(MissionType::Transport, player);
            placeholder.plan.target_island = Some(target_island);
            placeholder.plan.target = Some(GridPosition { x: 2, y: 0 });
            placeholder.plan.phase = MissionPhase::Forming;
            placeholder.id
        };
        let requirement = crate::ai::island_campaign::IslandCampaignRequirement {
            preferred_transport: Some(UnitType::TransportHelicopter),
            transport_slots: 2,
            capture_units: 1,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 5_000,
        };
        let assignment = crate::ai::island_campaign::IslandCampaignAssignment {
            island_id: target_island,
            decision: crate::ai::island_campaign::IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 2, y: 0 },
            capture_target_positions: vec![GridPosition { x: 2, y: 0 }],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: requirement,
            allocated_budget: 5_000,
            transport_entities: vec![transport],
            capture_entities: vec![cargo],
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: false,
        };

        assert!(!prepare_campaign_transport_assignment(
            &world,
            &mut manager,
            player,
            &assignment,
        ));

        let placeholder = manager
            .squads
            .iter()
            .find(|squad| squad.id == placeholder_id)
            .unwrap();
        assert_eq!(placeholder.transport_entity, Some(transport));
        assert!(placeholder.members.contains(&transport));
        assert_eq!(placeholder.cargo_entities, vec![cargo]);
        assert_eq!(
            placeholder.plan.phase,
            MissionPhase::Transport(TransportPhase::Pickup)
        );
    }

    /// 先頭波はFormingの複製から作るため、旧降車位置を持ち越さずNoneへ戻す（R回帰契約）。
    #[test]
    fn promoted_first_wave_does_not_inherit_forming_drop_position() {
        let mut world = World::new();
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Hex);
        map.set_terrain(0, 0, Terrain::Plains).unwrap();
        map.set_terrain(2, 0, Terrain::City).unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let target_island = island_map
            .get_island_at(&GridPosition { x: 2, y: 0 })
            .unwrap()
            .id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let player = PlayerId(2);
        let cargo = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    can_capture: true,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    movement_type: MovementType::Air,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
            ))
            .id();
        let mut manager = SquadManager::new();
        let placeholder_id = {
            let mut placeholder = manager.create_owned_squad(MissionType::Transport, player);
            placeholder.plan.target_island = Some(target_island);
            placeholder.plan.target = Some(GridPosition { x: 2, y: 0 });
            placeholder.plan.phase = MissionPhase::Forming;
            // 前の便で選んだ降車位置が残ったForming。
            placeholder.plan.drop_position = Some(GridPosition { x: 2, y: 0 });
            placeholder.id
        };
        let requirement = crate::ai::island_campaign::IslandCampaignRequirement {
            preferred_transport: Some(UnitType::TransportHelicopter),
            transport_slots: 2,
            capture_units: 1,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 5_000,
        };
        let assignment = crate::ai::island_campaign::IslandCampaignAssignment {
            island_id: target_island,
            decision: crate::ai::island_campaign::IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 2, y: 0 },
            capture_target_positions: vec![GridPosition { x: 2, y: 0 }],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: requirement,
            allocated_budget: 5_000,
            transport_entities: vec![transport],
            capture_entities: vec![cargo],
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: false,
        };

        assert!(!prepare_campaign_transport_assignment(
            &world,
            &mut manager,
            player,
            &assignment,
        ));

        let placeholder = manager
            .squads
            .iter()
            .find(|squad| squad.id == placeholder_id)
            .unwrap();
        assert_eq!(placeholder.transport_entity, Some(transport));
        assert!(placeholder.members.contains(&transport));
        assert_eq!(placeholder.cargo_entities, vec![cargo]);
        assert_eq!(
            placeholder.plan.phase,
            MissionPhase::Transport(TransportPhase::Pickup)
        );
        assert_eq!(placeholder.plan.drop_position, None);
    }

    #[test]
    fn pickup_loads_second_same_tile_cargo_after_transport_is_exhausted() {
        let mut world = World::new();
        let map = Map::new(2, 1, Terrain::Plains, GridTopology::Square);
        world.insert_resource(map.clone());
        world.insert_resource(MasterDataRegistry::load().unwrap());
        world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
        let player = PlayerId(1);
        let first = world
            .spawn((
                Faction(player),
                GridPosition { x: 9999, y: 9999 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    ..UnitStats::mock()
                },
                ActionCompleted(true),
            ))
            .id();
        let second = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    ..UnitStats::mock()
                },
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    movement_type: MovementType::Air,
                    max_movement: 7,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 60,
                    max: 60,
                },
                CargoCapacity {
                    max: 2,
                    loaded: vec![first],
                },
                HasMoved(true),
                ActionCompleted(true),
            ))
            .id();
        world.entity_mut(first).insert(Transporting(transport));

        let mut manager = SquadManager::new();
        let mut squad = manager
            .create_owned_squad(MissionType::Transport, player)
            .clone();
        squad.members.insert(transport);
        squad.transport_entity = Some(transport).into();
        squad.cargo_entities = vec![first, second].into();
        squad.plan.pickup_position = Some(GridPosition { x: 0, y: 0 });
        squad.plan.phase = MissionPhase::Transport(TransportPhase::Pickup);

        let (entity, command) =
            execute_transport_squad_step(&mut world, &mut squad.edit(), &HashSet::default())
                .expect("同じマスの2体目を同ターンに積載する");
        assert_eq!(entity, second);
        assert!(matches!(
            command,
            crate::ai::command::AiCommand::Load {
                transport_entity,
                ..
            } if transport_entity == transport
        ));
    }

    #[test]
    fn exhausted_return_transport_has_no_second_command() {
        let mut world = World::new();
        world.insert_resource(Map::new(2, 1, Terrain::Plains, GridTopology::Square));
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let player = PlayerId(1);
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    movement_type: MovementType::Air,
                    max_movement: 7,
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 60,
                    max: 60,
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
                HasMoved(true),
                ActionCompleted(true),
            ))
            .id();
        let mut manager = SquadManager::new();
        let mut squad = manager
            .create_owned_squad(MissionType::Transport, player)
            .clone();
        squad.members.insert(transport);
        squad.transport_entity = Some(transport).into();
        squad.plan.phase = MissionPhase::Transport(TransportPhase::Return);

        assert!(
            execute_transport_squad_step(&mut world, &mut squad.edit(), &HashSet::default())
                .is_none()
        );
    }

    #[test]
    fn pickup_loads_reachable_cargo_before_moving_the_transport() {
        let mut world = World::new();
        world.insert_resource(Map::new(4, 1, Terrain::Plains, GridTopology::Square));
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let player = PlayerId(1);
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    movement_type: MovementType::Air,
                    max_movement: 6,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 60,
                    max: 60,
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();
        let cargo = world
            .spawn((
                Faction(player),
                GridPosition { x: 3, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();
        let mut manager = SquadManager::new();
        let mut squad = manager
            .create_owned_squad(MissionType::Transport, player)
            .clone();
        squad.members.insert(transport);
        squad.transport_entity = Some(transport).into();
        squad.cargo_entities = vec![cargo].into();
        squad.plan.pickup_position = Some(GridPosition { x: 3, y: 0 });
        squad.plan.phase = MissionPhase::Transport(TransportPhase::Pickup);

        let (entity, command) =
            execute_transport_squad_step(&mut world, &mut squad.edit(), &HashSet::default())
                .expect("cargo should board the current helicopter before it moves");
        assert_eq!(entity, cargo);
        assert!(matches!(
            command,
            crate::ai::command::AiCommand::Load {
                transport_entity,
                target_pos: GridPosition { x: 0, y: 0 },
            } if transport_entity == transport
        ));
    }

    #[test]
    fn pickup_vacates_active_airport_then_loads_cargo_in_the_same_turn() {
        let mut world = World::new();
        let mut map = Map::new(3, 2, Terrain::Plains, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::Capital).unwrap();
        map.set_terrain(1, 0, Terrain::Airport).unwrap();
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let player = PlayerId(1);
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Airport, Some(player), 100),
        ));
        let cargo = world
            .spawn((
                Faction(player),
                GridPosition { x: 2, y: 1 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();
        let transport_stats = UnitStats {
            unit_type: UnitType::TransportHelicopter,
            movement_type: MovementType::Air,
            max_movement: 7,
            max_cargo: 2,
            loadable_unit_types: vec![UnitType::Infantry],
            ..UnitStats::mock()
        };
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 1, y: 0 },
                transport_stats.clone(),
                Fuel {
                    current: 60,
                    max: 60,
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();
        let pickup = select_pickup_position(
            &world,
            player,
            GridPosition { x: 1, y: 0 },
            &transport_stats,
            &[cargo],
            &mut TerrainConnectivity::default(),
        )
        .expect("airport以外に合法な合流点がある");
        assert_ne!(pickup, GridPosition { x: 1, y: 0 });

        let mut manager = SquadManager::new();
        let mut squad = manager
            .create_owned_squad(MissionType::Transport, player)
            .clone();
        squad.members.insert(transport);
        squad.transport_entity = Some(transport).into();
        squad.cargo_entities = vec![cargo].into();
        squad.plan.pickup_position = Some(pickup);
        squad.plan.phase = MissionPhase::Transport(TransportPhase::Pickup);

        let (first_entity, first_command) =
            execute_transport_squad_step(&mut world, &mut squad.edit(), &HashSet::default())
                .expect("transport must vacate the airport before loading");
        assert_eq!(first_entity, transport);
        let crate::ai::command::AiCommand::Wait { target_pos } = first_command else {
            panic!("airport relief must move the transport first");
        };
        assert_ne!(target_pos, GridPosition { x: 1, y: 0 });

        *world.get_mut::<GridPosition>(transport).unwrap() = target_pos;
        world.get_mut::<HasMoved>(transport).unwrap().0 = true;
        world.get_mut::<ActionCompleted>(transport).unwrap().0 = true;
        let (second_entity, second_command) =
            execute_transport_squad_step(&mut world, &mut squad.edit(), &HashSet::default())
                .expect("cargo must board the exhausted transport in the same turn");
        assert_eq!(second_entity, cargo);
        assert!(matches!(
            second_command,
            crate::ai::command::AiCommand::Load {
                transport_entity,
                target_pos: load_pos,
            } if transport_entity == transport && load_pos == target_pos
        ));
    }

    #[test]
    fn pickup_prefers_a_faster_rendezvous_over_the_transport_current_position() {
        let mut world = World::new();
        world.insert_resource(Map::new(8, 1, Terrain::Plains, GridTopology::Square));
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let player = PlayerId(1);
        let cargo = world
            .spawn((
                Faction(player),
                GridPosition { x: 7, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let transport_stats = UnitStats {
            unit_type: UnitType::TransportHelicopter,
            movement_type: MovementType::Air,
            max_movement: 3,
            loadable_unit_types: vec![UnitType::Infantry],
            ..UnitStats::mock()
        };

        let pickup = select_pickup_position(
            &world,
            player,
            GridPosition { x: 0, y: 0 },
            &transport_stats,
            &[cargo],
            &mut TerrainConnectivity::default(),
        )
        .unwrap();

        assert_ne!(pickup, GridPosition { x: 0, y: 0 });
        let map = world.resource::<Map>();
        let transport_turns = map.distance(0, 0, pickup.x, pickup.y).div_ceil(3);
        let cargo_turns = map.distance(7, 0, pickup.x, pickup.y).div_ceil(3);
        assert_eq!(transport_turns.max(cargo_turns), 2);
    }

    #[test]
    fn pickup_pairing_prefers_near_cargo_even_when_it_has_a_later_entity_id() {
        let mut world = World::new();
        world.insert_resource(Map::new(8, 1, Terrain::Plains, GridTopology::Square));
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let player = PlayerId(1);
        let spawn_cargo = |world: &mut World, x| {
            world
                .spawn((
                    Faction(player),
                    GridPosition { x, y: 0 },
                    UnitStats {
                        unit_type: UnitType::Infantry,
                        movement_type: MovementType::Infantry,
                        max_movement: 3,
                        ..UnitStats::mock()
                    },
                ))
                .id()
        };
        let far = spawn_cargo(&mut world, 7);
        let near = spawn_cargo(&mut world, 1);
        let transport_stats = UnitStats {
            unit_type: UnitType::TransportHelicopter,
            movement_type: MovementType::Air,
            max_movement: 7,
            loadable_unit_types: vec![UnitType::Infantry],
            ..UnitStats::mock()
        };
        let mut connectivity = TerrainConnectivity::default();

        let far_rank = cargo_pickup_rank(
            &world,
            player,
            GridPosition { x: 0, y: 0 },
            &transport_stats,
            far,
            &mut connectivity,
        )
        .unwrap();
        let near_rank = cargo_pickup_rank(
            &world,
            player,
            GridPosition { x: 0, y: 0 },
            &transport_stats,
            near,
            &mut connectivity,
        )
        .unwrap();

        assert!(near_rank < far_rank);
    }

    #[test]
    fn partial_logistics_flight_defers_distant_second_cargo() {
        let mut world = World::new();
        world.insert_resource(Map::new(6, 1, Terrain::Plains, GridTopology::Square));
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let player = PlayerId(1);
        let loaded = world
            .spawn((
                Faction(player),
                GridPosition { x: 9999, y: 9999 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    ..UnitStats::mock()
                },
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();
        let deferred = world
            .spawn((
                Faction(player),
                GridPosition { x: 5, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    movement_type: MovementType::Air,
                    max_movement: 6,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: vec![loaded],
                },
            ))
            .id();
        world.entity_mut(loaded).insert(Transporting(transport));

        let mut manager = SquadManager::new();
        let mut squad = manager
            .create_owned_squad(MissionType::Transport, player)
            .clone();
        squad.members.insert(transport);
        squad.transport_entity = Some(transport).into();
        squad.cargo_entities = vec![loaded, deferred].into();
        squad.plan.allow_partial_departure = true;
        squad.plan.phase = MissionPhase::Transport(TransportPhase::Pickup);

        let detached = detach_deferred_pickup_cargo(&mut world, &mut squad.edit())
            .expect("distant second cargo should become a follow-up flight");
        assert_eq!(detached, vec![deferred]);
        assert_eq!(squad.cargo_entities, vec![loaded]);
        assert_eq!(
            squad.plan.phase,
            MissionPhase::Transport(TransportPhase::Transit)
        );
    }

    /// 先頭cargoが行動済みでも、到達可能な後続cargoを同じ手番に積載する。
    #[test]
    fn transport_pickup_loads_reachable_next_cargo_in_same_turn() {
        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        world.insert_resource(Map::new(4, 1, Terrain::Plains, GridTopology::Square));
        world.insert_resource(registry);
        let player = PlayerId(1);
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    movement_type: MovementType::Air,
                    max_movement: 6,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                Fuel {
                    current: 60,
                    max: 60,
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
                HasMoved(true),
                ActionCompleted(true),
            ))
            .id();
        let completed = world
            .spawn((
                Faction(player),
                GridPosition { x: 1, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    ..UnitStats::mock()
                },
                HasMoved(true),
                ActionCompleted(true),
            ))
            .id();
        let actionable = world
            .spawn((
                Faction(player),
                GridPosition { x: 3, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    ..UnitStats::mock()
                },
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();
        let mut manager = SquadManager::new();
        let mut squad = manager.create_squad(MissionType::Transport).clone();
        squad.members.insert(transport);
        squad.transport_entity = Some(transport).into();
        squad.cargo_entities = vec![completed, actionable].into();
        squad.plan.pickup_position = Some(GridPosition { x: 0, y: 0 });
        squad.plan.phase = MissionPhase::Transport(TransportPhase::Pickup);

        let (entity, command) =
            execute_transport_squad_step(&mut world, &mut squad.edit(), &HashSet::default())
                .expect("second cargo should load without a staging wait");
        assert_eq!(entity, actionable);
        assert!(matches!(
            command,
            crate::ai::command::AiCommand::Load {
                transport_entity,
                target_pos: GridPosition { x: 0, y: 0 },
            } if transport_entity == transport
        ));
    }

    #[test]
    fn transport_drop_waits_for_all_cargo_and_hands_them_off() {
        let (mut world, transport, capture, combat, target_island) = setup_transport_phase_world();
        let mut manager = SquadManager::new();
        let mut squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport).into();
        squad.cargo_entities = vec![capture, combat].into();
        squad.plan.target_island = Some(target_island);
        squad.plan.target = Some(GridPosition { x: 2, y: 0 });
        squad.plan.phase = MissionPhase::Transport(TransportPhase::Drop);
        world.insert_resource(manager);

        // 1体目を降ろしても Drop を維持し、占領部隊へ引き渡す。
        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .retain(|entity| *entity == combat);
        world.entity_mut(capture).remove::<Transporting>();
        *world.get_mut::<GridPosition>(capture).unwrap() = GridPosition { x: 1, y: 0 };
        update_squads(&mut world, PlayerId(1));
        {
            let manager = world.resource::<SquadManager>();
            let transport_squad = manager
                .squads
                .iter()
                .find(|squad| squad.plan.mission_type == MissionType::Transport)
                .unwrap();
            assert_eq!(
                transport_squad.plan.phase,
                MissionPhase::Transport(TransportPhase::Drop)
            );
            assert_eq!(transport_squad.cargo_entities, vec![combat]);
            assert!(manager.squads.iter().any(|squad| {
                squad.plan.mission_type == MissionType::Capture && squad.members.contains(&capture)
            }));
        }

        // 最後のカーゴを降ろした後だけ Return へ進み、戦闘部隊へ引き渡す。
        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .clear();
        world.entity_mut(combat).remove::<Transporting>();
        *world.get_mut::<GridPosition>(combat).unwrap() = GridPosition { x: 2, y: 0 };
        update_squads(&mut world, PlayerId(1));
        let manager = world.resource::<SquadManager>();
        let transport_squad = manager
            .squads
            .iter()
            .find(|squad| squad.plan.mission_type == MissionType::Transport)
            .unwrap();
        assert_eq!(
            transport_squad.plan.phase,
            MissionPhase::Transport(TransportPhase::Return)
        );
        assert!(transport_squad.cargo_entities.is_empty());
        assert!(manager.squads.iter().any(|squad| {
            squad.plan.mission_type == MissionType::Attack && squad.members.contains(&combat)
        }));
    }

    #[test]
    fn empty_transport_helicopter_secures_landing_zone_against_infantry() {
        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let target_island = island_map
            .get_island_at(&GridPosition { x: 2, y: 0 })
            .unwrap()
            .id;
        world.insert_resource(map);
        world.insert_resource(registry.clone());
        world.insert_resource(island_map);
        let player = PlayerId(1);
        let helicopter_stats = registry
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let helicopter = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                helicopter_stats.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                Ammo {
                    ammo1: helicopter_stats.max_ammo1,
                    max_ammo1: helicopter_stats.max_ammo1,
                    ammo2: helicopter_stats.max_ammo2,
                    max_ammo2: helicopter_stats.max_ammo2,
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
            ))
            .id();
        let reusable_helicopter = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                helicopter_stats.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                Ammo {
                    ammo1: helicopter_stats.max_ammo1,
                    max_ammo1: helicopter_stats.max_ammo1,
                    ammo2: helicopter_stats.max_ammo2,
                    max_ammo2: helicopter_stats.max_ammo2,
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
            ))
            .id();
        let enemy_position = GridPosition { x: 2, y: 0 };
        let infantry_stats = registry
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let enemy = world
            .spawn((
                Faction(PlayerId(2)),
                enemy_position,
                infantry_stats,
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let mut manager = SquadManager::new();
        let mut squad = manager.create_owned_squad(MissionType::Transport, player);
        squad.members.insert(helicopter);
        squad.transport_entity = Some(helicopter).into();
        squad.plan.target_island = Some(target_island);
        squad.plan.target = Some(enemy_position);
        squad.plan.phase = MissionPhase::Transport(TransportPhase::Drop);
        let mut reusable = manager.create_owned_squad(MissionType::Transport, player);
        reusable.members.insert(reusable_helicopter);
        reusable.transport_entity = Some(reusable_helicopter).into();
        reusable.plan.target_island = Some(target_island);
        reusable.plan.target = Some(enemy_position);
        reusable.plan.phase = MissionPhase::Transport(TransportPhase::Drop);
        world.insert_resource(manager);

        update_squads(&mut world, player);

        {
            let manager = world.resource::<SquadManager>();
            let escort = manager
                .squads
                .iter()
                .find(|squad| squad.members.contains(&helicopter))
                .expect("輸送ヘリを局地護衛へ引き渡す");
            assert_eq!(escort.plan.mission_type, MissionType::Attack);
            assert_eq!(escort.plan.target, Some(enemy_position));
            assert_eq!(escort.plan.phase, MissionPhase::MovingToTarget);
            assert_eq!(escort.transport_entity, Some(helicopter));
            assert!(escort.plan.return_after_combat);
            let returning = manager
                .squads
                .iter()
                .find(|squad| squad.members.contains(&reusable_helicopter))
                .expect("余剰輸送ヘリも輸送任務に保持する");
            assert_eq!(returning.plan.mission_type, MissionType::Transport);
            assert_eq!(
                returning.plan.phase,
                MissionPhase::Transport(TransportPhase::Return)
            );
            assert!(!returning.plan.return_after_combat);
        }

        // 局地脅威が消えたらAttackのまま遊兵化せず、同じ機体を帰還輸送へ戻す。
        world.despawn(enemy);
        update_squads(&mut world, player);
        let manager = world.resource::<SquadManager>();
        let returning = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&helicopter))
            .expect("護衛完了後も輸送機として管理を継続する");
        assert_eq!(returning.plan.mission_type, MissionType::Transport);
        assert_eq!(
            returning.plan.phase,
            MissionPhase::Transport(TransportPhase::Return)
        );
        assert_eq!(returning.transport_entity, Some(helicopter));
        assert!(!returning.plan.return_after_combat);
    }

    #[test]
    fn targetless_safe_drop_handoff_does_not_create_cross_sea_duty() {
        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
        let landing = GridPosition { x: 0, y: 0 };
        let remote = GridPosition { x: 2, y: 0 };
        map.set_terrain(landing.x, landing.y, Terrain::City)
            .unwrap();
        map.set_terrain(remote.x, remote.y, Terrain::City).unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);

        let player = PlayerId(1);
        world.spawn((landing, Property::new(Terrain::City, Some(player), 100)));
        world.spawn((remote, Property::new(Terrain::City, None, 100)));
        let cargo = world
            .spawn((
                Faction(player),
                GridPosition { x: 9_999, y: 9_999 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let transport = world
            .spawn((
                Faction(player),
                landing,
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    movement_type: MovementType::Air,
                    max_movement: 7,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: vec![cargo],
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        world.entity_mut(cargo).insert(Transporting(transport));

        let mut manager = SquadManager::new();
        let mut safe_drop = manager.create_squad(MissionType::Transport);
        safe_drop.members.insert(transport);
        safe_drop.transport_entity = Some(transport).into();
        safe_drop.cargo_entities = vec![cargo].into();
        safe_drop.plan.target_island = None;
        safe_drop.plan.target = None;
        safe_drop.plan.phase = MissionPhase::Transport(TransportPhase::Drop);
        world.insert_resource(manager);

        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .clear();
        world.entity_mut(cargo).remove::<Transporting>();
        *world.get_mut::<GridPosition>(cargo).unwrap() = landing;
        update_squads(&mut world, player);

        let manager = world.resource::<SquadManager>();
        assert!(manager.squads.iter().all(|squad| {
            squad.plan.mission_type == MissionType::Transport || !squad.members.contains(&cargo)
        }));
    }

    #[test]
    fn targetless_safe_drop_handoff_uses_reachable_duty_on_actual_landing_island() {
        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let landing = GridPosition { x: 0, y: 0 };
        let local_target = GridPosition { x: 1, y: 0 };
        let mut map = Map::new(2, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(landing.x, landing.y, Terrain::City)
            .unwrap();
        map.set_terrain(local_target.x, local_target.y, Terrain::City)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let landing_island = island_map.get_island_at(&landing).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);
        let player = PlayerId(1);
        world.spawn((landing, Property::new(Terrain::City, Some(player), 100)));
        world.spawn((local_target, Property::new(Terrain::City, None, 100)));
        let cargo = world
            .spawn((
                Faction(player),
                landing,
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let mut manager = SquadManager::new();

        handoff_delivered_cargo(&mut world, &mut manager, player, cargo, None, None);

        let capture = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&cargo))
            .expect("landed cargo must receive a reachable local duty");
        assert_eq!(capture.plan.mission_type, MissionType::Capture);
        assert_eq!(capture.plan.target_island, Some(landing_island));
        assert_eq!(capture.plan.target, Some(local_target));
    }

    #[test]
    fn delivered_capture_cargo_split_across_unclaimed_local_properties() {
        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let landing = GridPosition { x: 0, y: 0 };
        let city = GridPosition { x: 1, y: 0 };
        let preferred_airport = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(city.x, city.y, Terrain::City).unwrap();
        map.set_terrain(preferred_airport.x, preferred_airport.y, Terrain::Airport)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map.get_island_at(&landing).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);
        world.spawn((city, Property::new(Terrain::City, None, 100)));
        world.spawn((
            preferred_airport,
            Property::new(Terrain::Airport, None, 100),
        ));
        let player = PlayerId(1);
        let cargo: Vec<_> = (0..2)
            .map(|_| {
                world
                    .spawn((
                        Faction(player),
                        landing,
                        UnitStats {
                            unit_type: UnitType::Infantry,
                            movement_type: MovementType::Infantry,
                            max_movement: 3,
                            can_capture: true,
                            ..UnitStats::mock()
                        },
                    ))
                    .id()
            })
            .collect();
        let mut manager = SquadManager::new();

        for entity in &cargo {
            assert!(handoff_delivered_cargo(
                &mut world,
                &mut manager,
                player,
                *entity,
                Some(island_id),
                Some(preferred_airport),
            ));
        }

        let mut targets: Vec<_> = manager
            .squads
            .iter()
            .filter(|squad| squad.plan.mission_type == MissionType::Capture)
            .filter_map(|squad| squad.plan.target)
            .collect();
        targets.sort_by_key(|target| (target.y, target.x));
        assert_eq!(targets, vec![city, preferred_airport]);
        assert_eq!(manager.squads.len(), cargo.len());
    }

    #[test]
    fn targetless_safe_drop_attack_handoff_records_actual_landing_island() {
        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let landing = GridPosition { x: 0, y: 0 };
        let enemy_position = GridPosition { x: 1, y: 0 };
        let mut map = Map::new(2, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(landing.x, landing.y, Terrain::City)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let landing_island = island_map.get_island_at(&landing).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);
        let player = PlayerId(1);
        let cargo = world
            .spawn((
                Faction(player),
                landing,
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    can_capture: false,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        world.spawn((
            Faction(PlayerId(2)),
            enemy_position,
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));
        let mut manager = SquadManager::new();

        handoff_delivered_cargo(&mut world, &mut manager, player, cargo, None, None);

        let attack = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&cargo))
            .expect("landed combat cargo must receive a reachable local attack duty");
        assert_eq!(attack.plan.mission_type, MissionType::Attack);
        assert_eq!(attack.plan.target, Some(enemy_position));
        assert_eq!(attack.plan.target_island, Some(landing_island));
        world.insert_resource(manager);
        update_squads(&mut world, player);
        let manager = world.resource::<SquadManager>();
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|squad| squad.members.contains(&cargo))
                .count(),
            1
        );
        assert_eq!(
            manager
                .squads
                .iter()
                .find(|squad| squad.members.contains(&cargo))
                .and_then(|squad| squad.plan.target_island),
            Some(landing_island)
        );
    }

    #[test]
    fn landing_prefers_tile_outside_indirect_fire_range() {
        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(5, 3, Terrain::Sea, GridTopology::Square);
        for x in 1..=4 {
            map.set_terrain(x, 0, Terrain::Plains).unwrap();
        }
        map.set_terrain(1, 1, Terrain::Shoal).unwrap();
        map.set_terrain(3, 1, Terrain::Shoal).unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let target_island = island_map
            .get_island_at(&GridPosition { x: 4, y: 0 })
            .unwrap()
            .id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);
        let mut damage_chart = crate::resources::DamageChart::new();
        damage_chart.insert_damage(UnitType::Artillery, UnitType::Infantry, 50);
        world.insert_resource(damage_chart);

        let player = PlayerId(1);
        let cargo = world
            .spawn((
                Faction(player),
                GridPosition { x: 9999, y: 9999 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    cost: 1000,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::Lander,
                    movement_type: MovementType::Ship,
                    max_movement: 6,
                    max_cargo: 1,
                    loadable_unit_types: vec![UnitType::Infantry],
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 1,
                    loaded: vec![cargo],
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        world.entity_mut(cargo).insert(Transporting(transport));
        world.spawn((
            Faction(PlayerId(2)),
            GridPosition { x: 1, y: 2 },
            UnitStats {
                unit_type: UnitType::Artillery,
                movement_type: MovementType::Artillery,
                min_range: 2,
                max_range: 3,
                max_movement: 5,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        // 到達可能タイルは決定性確保のため BTreeSet で扱う
        let reachable = std::collections::BTreeSet::from([(1, 1), (3, 1)]);
        let selected = select_landing_candidate(
            &mut world,
            transport,
            cargo,
            GridPosition { x: 1, y: 1 },
            &reachable,
            Some(target_island),
            Some(GridPosition { x: 4, y: 0 }),
        )
        .unwrap();
        assert_eq!(selected.0, GridPosition { x: 3, y: 1 });
        assert_eq!(selected.1, GridPosition { x: 3, y: 0 });
    }

    #[test]
    fn shoal_separated_unit_remains_transport_candidate() {
        let registry = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(1, 0, Terrain::Shoal).unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let target = GridPosition { x: 2, y: 0 };
        let target_island = island_map.get_island_at(&target).unwrap().id;
        let candidate_entity = Entity::from_raw(1);
        let candidates = vec![(
            candidate_entity,
            GridPosition { x: 0, y: 0 },
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                can_capture: true,
                ..UnitStats::mock()
            },
        )];
        let transport_stats = UnitStats {
            unit_type: UnitType::Lander,
            movement_type: MovementType::Ship,
            max_cargo: 2,
            loadable_unit_types: vec![UnitType::Infantry],
            ..UnitStats::mock()
        };
        let mut cache = TurnDistanceCache::default();
        let mut connectivity = TerrainConnectivity::default();

        assert_eq!(
            select_nearest_compatible_cargo(
                &candidates,
                GridPosition { x: 1, y: 0 },
                &transport_stats,
                target_island,
                Some(target),
                &island_map,
                &map,
                &registry,
                &HashMap::default(),
                PlayerId(1),
                &mut cache,
                &mut connectivity,
                &[],
                None,
                false,
            ),
            Some(0)
        );
    }

    #[test]
    fn secure_replaces_unreachable_stale_capture_with_allocator_selected_reachable_unit() {
        use crate::ai::island_campaign::{
            IslandCampaignAssessment, IslandCampaignDecision, IslandCampaignPortfolio,
            IslandCampaignState,
        };

        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let owned_port = GridPosition { x: 0, y: 0 };
        let bridge = GridPosition { x: 1, y: 0 };
        let neutral_city = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(owned_port.x, owned_port.y, Terrain::Port)
            .unwrap();
        map.set_terrain(bridge.x, bridge.y, Terrain::Plains)
            .unwrap();
        map.set_terrain(neutral_city.x, neutral_city.y, Terrain::City)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map.get_island_at(&owned_port).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);
        world.spawn((
            owned_port,
            Property::new(Terrain::Port, Some(PlayerId(1)), 100),
        ));
        world.spawn((neutral_city, Property::new(Terrain::City, None, 100)));

        let player = PlayerId(1);
        let disconnected = world
            .spawn((
                Faction(player),
                owned_port,
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Ship,
                    max_movement: 5,
                    can_capture: true,
                    cost: 500,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let reachable = world
            .spawn((
                Faction(player),
                bridge,
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    can_capture: true,
                    cost: 1_000,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let stale_duplicate = world
            .spawn((
                Faction(player),
                bridge,
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    can_capture: false,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let legacy_member = world
            .spawn((
                Faction(player),
                bridge,
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    can_capture: false,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let mut manager = SquadManager::new();
        let mut stale = manager.create_squad(MissionType::Capture);
        stale.members.insert(disconnected);
        stale.plan.target_island = Some(island_id);
        stale.plan.target = Some(neutral_city);
        stale.plan.phase = MissionPhase::MovingToTarget;
        let mut duplicate = manager.create_squad(MissionType::Capture);
        duplicate.members.insert(stale_duplicate);
        duplicate.plan.target_island = Some(island_id);
        duplicate.plan.target = Some(neutral_city);
        duplicate.plan.phase = MissionPhase::MovingToTarget;
        let mut legacy = manager.create_squad(MissionType::Capture);
        legacy.members.insert(legacy_member);
        legacy.plan.target_island = None;
        legacy.plan.target = Some(neutral_city);
        legacy.plan.phase = MissionPhase::MovingToTarget;
        let legacy_id = legacy.id;
        let portfolio = IslandCampaignPortfolio {
            islands: vec![IslandCampaignAssessment {
                island_id,
                state: IslandCampaignState::Secured,
                decision: IslandCampaignDecision::Secure,
                state_reason: "友軍が足場を確保している島である".to_owned(),
                decision_reason: "未所有の拠点を確保する".to_owned(),
                pause_cause: None,
                neutral_properties: 1,
                friendly_properties: 1,
                enemy_properties: 0,
                friendly_combat_units: 0,
                enemy_combat_units: 0,
                friendly_arrival_eta: Some(0),
                enemy_arrival_eta: None,
                friendly_capture_eta: Some(1),
                enemy_capture_eta: None,
                roi_production_sites: 0,
                transport_eta: None,
                expansion_payback_turns: None,
                required_budget: 0,
                allocated_budget: 0,
            }],
            active_offensives: Vec::new(),
            defenses: Vec::new(),
        };

        let protected = prepare_secure_local_captures(&world, &mut manager, player, &portfolio);

        assert_eq!(protected, HashSet::from_iter([reachable]));
        let capture = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&reachable))
            .expect("reachable Secure capture unit must own the local duty");
        assert_eq!(capture.plan.target_island, Some(island_id));
        assert_eq!(capture.plan.target, Some(neutral_city));
        assert_eq!(capture.members, BTreeSet::from([reachable]));
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|squad| {
                    squad.plan.mission_type == MissionType::Capture
                        && squad.plan.target_island == Some(island_id)
                })
                .count(),
            1
        );
        assert!(manager.squads.iter().all(|squad| {
            !squad.members.contains(&disconnected)
                && !squad.members.contains(&stale_duplicate)
                && !squad.cargo_entities.contains(&disconnected)
                && !squad.delivered_cargo.contains(&disconnected)
        }));
        let legacy = manager
            .squads
            .iter()
            .find(|squad| squad.id == legacy_id)
            .expect("legacy-local duty without campaign island must remain");
        assert_eq!(legacy.members, BTreeSet::from([legacy_member]));
        let snapshot: Vec<_> = manager
            .squads
            .iter()
            .map(|squad| {
                (
                    squad.id,
                    squad.plan.mission_type.clone(),
                    squad.members.clone(),
                    squad.plan.target_island,
                    squad.plan.target,
                )
            })
            .collect();
        prepare_secure_local_captures(&world, &mut manager, player, &portfolio);
        let repeated: Vec<_> = manager
            .squads
            .iter()
            .map(|squad| {
                (
                    squad.id,
                    squad.plan.mission_type.clone(),
                    squad.members.clone(),
                    squad.plan.target_island,
                    squad.plan.target,
                )
            })
            .collect();
        assert_eq!(repeated, snapshot);
    }

    #[test]
    fn update_squads_leaves_mixed_owner_squad_byte_for_byte_unchanged() {
        let mut world = setup_test_world();
        let player_a = PlayerId(1);
        let player_b = PlayerId(2);
        let member_a = world
            .spawn((
                Faction(player_a),
                GridPosition { x: 0, y: 0 },
                UnitStats::mock(),
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let member_b = world
            .spawn((
                Faction(player_b),
                GridPosition { x: 1, y: 0 },
                UnitStats::mock(),
                Health {
                    current: 50,
                    max: 100,
                },
            ))
            .id();
        let mut manager = SquadManager::new();
        let mut mixed = manager.create_squad(MissionType::Attack);
        mixed.members = BTreeSet::from([member_a, member_b]).into();
        mixed.plan.target = Some(GridPosition { x: 3, y: 3 });
        mixed.plan.target_island = Some(crate::ai::islands::IslandId(0));
        mixed.plan.phase = MissionPhase::MovingToTarget;
        let snapshot = mixed.clone();
        world.insert_resource(manager);

        update_squads(&mut world, player_b);

        let retained = &world.resource::<SquadManager>().squads[0];
        assert_eq!(retained.id, snapshot.id);
        assert_eq!(retained.plan.owner_id, snapshot.plan.owner_id);
        assert_eq!(retained.plan.mission_type, snapshot.plan.mission_type);
        assert_eq!(retained.members, snapshot.members);
        assert_eq!(retained.plan.target, snapshot.plan.target);
        assert_eq!(retained.plan.target_island, snapshot.plan.target_island);
        assert_eq!(retained.plan.phase, snapshot.plan.phase);
        assert_eq!(retained.transport_entity, snapshot.transport_entity);
        assert_eq!(retained.cargo_entities, snapshot.cargo_entities);
        assert_eq!(retained.delivered_cargo, snapshot.delivered_cargo);
        assert_eq!(retained.plan.pickup_position, snapshot.plan.pickup_position);
        assert_eq!(retained.plan.drop_position, snapshot.plan.drop_position);
    }

    #[test]
    fn defense_preempted_observe_island_releases_stale_forming_operation() {
        use crate::ai::island_campaign::{
            IslandCampaignAssessment, IslandCampaignDecision, IslandCampaignPauseCause,
            IslandCampaignPortfolio, IslandCampaignState,
        };

        let mut world = setup_test_world();
        let player = PlayerId(1);
        let transport = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    movement_type: MovementType::Air,
                    max_cargo: 2,
                    ..UnitStats::mock()
                },
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let island_id = crate::ai::islands::IslandId(0);
        let mut manager = SquadManager::new();
        let mut stale = manager.create_squad(MissionType::Transport);
        stale.members.insert(transport);
        stale.transport_entity = Some(transport).into();
        stale.plan.target_island = Some(island_id);
        stale.plan.target = Some(GridPosition { x: 3, y: 3 });
        stale.plan.phase = MissionPhase::Forming;
        let stale_id = stale.id;
        let mut placeholder = manager.create_owned_squad(MissionType::Transport, player);
        placeholder.plan.target_island = Some(island_id);
        placeholder.plan.target = Some(GridPosition { x: 3, y: 3 });
        placeholder.plan.phase = MissionPhase::Forming;
        let placeholder_id = placeholder.id;
        let portfolio = IslandCampaignPortfolio {
            islands: vec![IslandCampaignAssessment {
                island_id,
                state: IslandCampaignState::OpenNeutral,
                decision: IslandCampaignDecision::Observe,
                state_reason: "未占領の中立島である".to_owned(),
                decision_reason: "診断表示の文言は制御に使わない".to_owned(),
                pause_cause: Some(IslandCampaignPauseCause::DefensePreemption),
                neutral_properties: 1,
                friendly_properties: 0,
                enemy_properties: 0,
                friendly_combat_units: 0,
                enemy_combat_units: 0,
                friendly_arrival_eta: None,
                enemy_arrival_eta: None,
                friendly_capture_eta: None,
                enemy_capture_eta: None,
                roi_production_sites: 0,
                transport_eta: Some(1),
                expansion_payback_turns: Some(1),
                required_budget: 0,
                allocated_budget: 0,
            }],
            active_offensives: Vec::new(),
            defenses: Vec::new(),
        };

        let paused = campaign_paused_islands(&portfolio);
        apply_campaign_pauses(&world, &mut manager, player, &paused);
        assert!(manager.squads.iter().all(|squad| squad.id != stale_id));
        assert!(
            manager
                .squads
                .iter()
                .all(|squad| squad.id != placeholder_id)
        );
        assert!(paused.contains(&island_id));
    }

    #[test]
    fn defend_does_not_assign_transported_or_unreachable_reserved_entities() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let mut world = setup_test_world();
        let player = PlayerId(1);
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::City).unwrap();
        map.set_terrain(2, 0, Terrain::Plains).unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let defended_island = island_map
            .get_island_at(&GridPosition { x: 0, y: 0 })
            .unwrap()
            .id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        let unreachable = world
            .spawn((
                Faction(player),
                GridPosition { x: 2, y: 0 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let transported = world
            .spawn((
                Faction(player),
                GridPosition { x: 9_999, y: 9_999 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let carrier = world.spawn((Faction(player),)).id();
        world.entity_mut(transported).insert(Transporting(carrier));
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 2,
            total_budget: 2_000,
        };
        let assignment = IslandCampaignAssignment {
            island_id: defended_island,
            decision: IslandCampaignDecision::Defend,
            target_position: GridPosition { x: 0, y: 0 },
            capture_target_positions: vec![GridPosition { x: 0, y: 0 }],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 2_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: vec![unreachable, transported],
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let mut manager = SquadManager::new();

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);
        assert!(manager.squads.iter().all(|squad| {
            squad.plan.mission_type != MissionType::Defense
                || !squad.members.contains(&unreachable) && !squad.members.contains(&transported)
        }));
    }

    #[test]
    fn campaign_attack_uses_a_reachable_ranged_firing_position() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let firing_position = GridPosition { x: 0, y: 0 };
        let bridge = GridPosition { x: 1, y: 0 };
        let enemy_position = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(firing_position.x, firing_position.y, Terrain::Port)
            .unwrap();
        map.set_terrain(bridge.x, bridge.y, Terrain::Plains)
            .unwrap();
        map.set_terrain(enemy_position.x, enemy_position.y, Terrain::City)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map.get_island_at(&enemy_position).unwrap().id;
        let battleship_stats = registry
            .create_unit_stats(&UnitName(UnitType::Battleship.as_str().to_owned()))
            .unwrap();
        let enemy_stats = registry
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);
        let player = PlayerId(1);
        let combat = world
            .spawn((
                Faction(player),
                firing_position,
                battleship_stats.clone(),
                Ammo {
                    ammo1: battleship_stats.max_ammo1,
                    max_ammo1: battleship_stats.max_ammo1,
                    ammo2: battleship_stats.max_ammo2,
                    max_ammo2: battleship_stats.max_ammo2,
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        world.spawn((
            Faction(PlayerId(2)),
            enemy_position,
            enemy_stats,
            Health {
                current: 100,
                max: 100,
            },
        ));
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 1,
            total_budget: battleship_stats.cost,
        };
        let assignment = IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Contest,
            target_position: enemy_position,
            capture_target_positions: vec![enemy_position],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: battleship_stats.cost,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: vec![combat],
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let mut manager = SquadManager::new();

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);

        let attack = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&combat))
            .expect("armed ranged member must keep a local responsibility");
        assert_eq!(attack.plan.mission_type, MissionType::Attack);
        assert_eq!(attack.plan.target_island, Some(island_id));
        assert_eq!(attack.plan.target, Some(enemy_position));
    }

    #[test]
    fn campaign_attack_uses_local_hold_when_same_island_enemy_is_unreachable() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let hold_position = GridPosition { x: 0, y: 0 };
        let bridge = GridPosition { x: 1, y: 0 };
        let enemy_position = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(hold_position.x, hold_position.y, Terrain::Port)
            .unwrap();
        map.set_terrain(bridge.x, bridge.y, Terrain::Plains)
            .unwrap();
        map.set_terrain(enemy_position.x, enemy_position.y, Terrain::City)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map.get_island_at(&enemy_position).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);
        let player = PlayerId(1);
        let combat = world
            .spawn((
                Faction(player),
                hold_position,
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Ship,
                    cost: 7_000,
                    ..UnitStats::mock()
                },
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 9,
                    max_ammo2: 9,
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        world.spawn((
            Faction(PlayerId(2)),
            enemy_position,
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 1,
            total_budget: 7_000,
        };
        let assignment = IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Contest,
            target_position: enemy_position,
            capture_target_positions: vec![enemy_position],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 7_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: vec![combat],
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let mut manager = SquadManager::new();

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);

        assert!(manager.squads.iter().all(|squad| {
            squad.plan.mission_type != MissionType::Attack || !squad.members.contains(&combat)
        }));
        let local_hold = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&combat))
            .expect("unreachable local combat must remain protected by a local responsibility");
        assert_eq!(local_hold.plan.mission_type, MissionType::Defense);
        assert_eq!(local_hold.plan.target_island, Some(island_id));
        assert_eq!(local_hold.plan.target, Some(hold_position));
    }

    #[test]
    fn campaign_combat_partitions_unreachable_members_into_stable_local_duties() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let ship_hold = GridPosition { x: 0, y: 0 };
        let land_position = GridPosition { x: 1, y: 0 };
        let enemy_position = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(ship_hold.x, ship_hold.y, Terrain::Port)
            .unwrap();
        map.set_terrain(land_position.x, land_position.y, Terrain::Plains)
            .unwrap();
        map.set_terrain(enemy_position.x, enemy_position.y, Terrain::City)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map.get_island_at(&enemy_position).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);
        let player = PlayerId(1);
        let ship_member = world
            .spawn((
                Faction(player),
                ship_hold,
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Ship,
                    cost: 7_000,
                    ..UnitStats::mock()
                },
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 9,
                    max_ammo2: 9,
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let land_member = world
            .spawn((
                Faction(player),
                land_position,
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    cost: 7_000,
                    ..UnitStats::mock()
                },
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 9,
                    max_ammo2: 9,
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        world.spawn((
            Faction(PlayerId(2)),
            enemy_position,
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 2,
            total_budget: 14_000,
        };
        let assignment = IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Contest,
            target_position: enemy_position,
            capture_target_positions: vec![enemy_position],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 14_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: vec![ship_member, land_member],
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let mut manager = SquadManager::new();

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);
        let snapshot: Vec<_> = manager
            .squads
            .iter()
            .map(|squad| {
                (
                    squad.id,
                    squad.plan.mission_type.clone(),
                    squad.plan.target_island,
                    squad.plan.target,
                    squad.members.clone(),
                )
            })
            .collect();
        let attack = manager
            .squads
            .iter()
            .find(|squad| squad.plan.mission_type == MissionType::Attack)
            .expect("reachable land member must receive an Attack duty");
        assert_eq!(attack.plan.target, Some(enemy_position));
        assert_eq!(attack.members, BTreeSet::from([land_member]));
        let defense = manager
            .squads
            .iter()
            .find(|squad| squad.plan.mission_type == MissionType::Defense)
            .expect("unreachable ship member must receive a local Defense hold");
        assert_eq!(defense.plan.target, Some(ship_hold));
        assert_eq!(defense.members, BTreeSet::from([ship_member]));
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|squad| squad.members.contains(&ship_member))
                .count(),
            1
        );
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|squad| squad.members.contains(&land_member))
                .count(),
            1
        );

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);
        let repeated: Vec<_> = manager
            .squads
            .iter()
            .map(|squad| {
                (
                    squad.id,
                    squad.plan.mission_type.clone(),
                    squad.plan.target_island,
                    squad.plan.target,
                    squad.members.clone(),
                )
            })
            .collect();
        assert_eq!(repeated, snapshot);
    }

    #[test]
    fn campaign_capture_preserves_exact_assigned_facility_over_nearer_property() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let start = GridPosition { x: 0, y: 0 };
        let nearby_city = GridPosition { x: 1, y: 0 };
        let assigned_airport = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(nearby_city.x, nearby_city.y, Terrain::City)
            .unwrap();
        map.set_terrain(assigned_airport.x, assigned_airport.y, Terrain::Airport)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map.get_island_at(&assigned_airport).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);
        let player = PlayerId(1);
        let capture_units: Vec<_> = (0..2)
            .map(|_| {
                world
                    .spawn((
                        Faction(player),
                        start,
                        UnitStats {
                            unit_type: UnitType::Infantry,
                            movement_type: MovementType::Infantry,
                            can_capture: true,
                            cost: 1_000,
                            ..UnitStats::mock()
                        },
                    ))
                    .id()
            })
            .collect();
        world.spawn((nearby_city, Property::new(Terrain::City, None, 200)));
        world.spawn((assigned_airport, Property::new(Terrain::Airport, None, 200)));
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 2,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 2_000,
        };
        let assignment = IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Contest,
            target_position: assigned_airport,
            capture_target_positions: vec![assigned_airport, nearby_city],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 2_000,
            transport_entities: Vec::new(),
            capture_entities: capture_units.clone(),
            combat_entities: Vec::new(),
            operation_ready: true,
            continued_from_existing_squad: true,
        };
        let mut manager = SquadManager::new();

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);

        let capture_squads: Vec<_> = manager
            .squads
            .iter()
            .filter(|squad| squad.plan.mission_type == MissionType::Capture)
            .collect();
        assert_eq!(capture_squads.len(), 2);
        assert!(capture_squads.iter().all(|squad| squad.members.len() == 1));
        let mut targets: Vec<_> = capture_squads
            .iter()
            .filter_map(|squad| squad.plan.target)
            .collect();
        targets.sort_by_key(|position| (position.y, position.x));
        assert_eq!(targets, vec![nearby_city, assigned_airport]);
        let assigned_members: BTreeSet<_> = capture_squads
            .iter()
            .flat_map(|squad| squad.members.iter().copied())
            .collect();
        assert_eq!(assigned_members, capture_units.into_iter().collect());
    }

    #[test]
    fn campaign_combat_uses_one_attack_squad_for_a_common_reachable_enemy() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let positions = [GridPosition { x: 0, y: 0 }, GridPosition { x: 1, y: 0 }];
        let enemy_position = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(enemy_position.x, enemy_position.y, Terrain::City)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map.get_island_at(&enemy_position).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);
        let player = PlayerId(1);
        let members: Vec<_> = positions
            .iter()
            .map(|position| {
                world
                    .spawn((
                        Faction(player),
                        *position,
                        UnitStats {
                            unit_type: UnitType::Tank,
                            movement_type: MovementType::Tank,
                            cost: 7_000,
                            ..UnitStats::mock()
                        },
                        Ammo {
                            ammo1: 9,
                            max_ammo1: 9,
                            ammo2: 9,
                            max_ammo2: 9,
                        },
                        Health {
                            current: 100,
                            max: 100,
                        },
                    ))
                    .id()
            })
            .collect();
        world.spawn((
            Faction(PlayerId(2)),
            enemy_position,
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 2,
            total_budget: 14_000,
        };
        let assignment = IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Contest,
            target_position: enemy_position,
            capture_target_positions: vec![enemy_position],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 14_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: members.clone(),
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let mut manager = SquadManager::new();

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);

        let attack_squads: Vec<_> = manager
            .squads
            .iter()
            .filter(|squad| squad.plan.mission_type == MissionType::Attack)
            .collect();
        assert_eq!(attack_squads.len(), 1);
        assert_eq!(attack_squads[0].plan.target, Some(enemy_position));
        assert_eq!(
            attack_squads[0].members,
            members.iter().copied().collect::<BTreeSet<_>>()
        );
        assert!(
            manager
                .squads
                .iter()
                .all(|squad| squad.plan.mission_type != MissionType::Defense)
        );
    }

    #[test]
    fn campaign_attack_selects_reachable_same_island_enemy() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let combat_position = GridPosition { x: 0, y: 0 };
        let enemy_position = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(enemy_position.x, enemy_position.y, Terrain::City)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map.get_island_at(&enemy_position).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);
        let player = PlayerId(1);
        let combat = world
            .spawn((
                Faction(player),
                combat_position,
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    cost: 7_000,
                    ..UnitStats::mock()
                },
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 9,
                    max_ammo2: 9,
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        world.spawn((
            Faction(PlayerId(2)),
            enemy_position,
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 1,
            total_budget: 7_000,
        };
        let assignment = IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Contest,
            target_position: enemy_position,
            capture_target_positions: vec![enemy_position],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 7_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: vec![combat],
            operation_ready: true,
            continued_from_existing_squad: false,
        };
        let mut manager = SquadManager::new();

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);

        let attack = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&combat))
            .expect("reachable local combat must receive an Attack responsibility");
        assert_eq!(attack.plan.mission_type, MissionType::Attack);
        assert_eq!(attack.plan.target_island, Some(island_id));
        assert_eq!(attack.plan.target, Some(enemy_position));
    }

    #[test]
    fn purchase_only_assignment_keeps_forming_placeholder_across_replans() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let world = setup_test_world();
        let player = PlayerId(1);
        let island_id = world
            .resource::<crate::ai::islands::IslandMap>()
            .get_island_at(&GridPosition { x: 2, y: 2 })
            .unwrap()
            .id;
        let requirement = IslandCampaignRequirement {
            preferred_transport: Some(UnitType::TransportHelicopter),
            transport_slots: 2,
            capture_units: 2,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 4_000,
        };
        let assignment = IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 2, y: 2 },
            capture_target_positions: vec![GridPosition { x: 2, y: 2 }],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: requirement,
            allocated_budget: 4_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        let mut manager = SquadManager::new();

        prepare_campaign_transport_assignment(&world, &mut manager, player, &assignment);
        let placeholder = manager
            .squads
            .iter()
            .find(|squad| squad.plan.target_island == Some(island_id))
            .expect("funded purchase-only operation must retain a Forming identity");
        assert_eq!(placeholder.plan.phase, MissionPhase::Forming);
        assert_eq!(placeholder.plan.target, Some(assignment.target_position));
        assert!(placeholder.members.is_empty());
        assert!(placeholder.transport_entity.is_none());
        let placeholder_id = placeholder.id;

        prepare_campaign_transport_assignment(&world, &mut manager, player, &assignment);
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|squad| squad.plan.target_island == Some(island_id))
                .count(),
            1
        );
        assert_eq!(
            manager
                .squads
                .iter()
                .find(|squad| squad.plan.target_island == Some(island_id))
                .map(|squad| squad.id),
            Some(placeholder_id)
        );
    }

    /// 作戦島へ自力で飛べる航空戦力は搭載不能な輸送cargoにせず、直接Attackへ割り当てる。
    #[test]
    fn campaign_self_deploying_air_unit_does_not_stall_in_transport_forming() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(5, 1, Terrain::Sea, GridTopology::Square);
        let source = GridPosition { x: 0, y: 0 };
        let target = GridPosition { x: 4, y: 0 };
        map.set_terrain(source.x, source.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(target.x, target.y, Terrain::City).unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let target_island = island_map.get_island_at(&target).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.insert_resource(registry.clone());

        let player = PlayerId(1);
        let helicopter_stats = registry
            .create_unit_stats(&UnitName(UnitType::TransportHelicopter.as_str().to_owned()))
            .unwrap();
        let bomber_stats = registry
            .create_unit_stats(&UnitName(UnitType::Bomber.as_str().to_owned()))
            .unwrap();
        let enemy_stats = registry
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let transport = world
            .spawn((
                Faction(player),
                source,
                helicopter_stats.clone(),
                CargoCapacity {
                    max: helicopter_stats.max_cargo,
                    loaded: Vec::new(),
                },
            ))
            .id();
        let bomber = world
            .spawn((
                Faction(player),
                source,
                bomber_stats.clone(),
                Ammo {
                    ammo1: bomber_stats.max_ammo1,
                    max_ammo1: bomber_stats.max_ammo1,
                    ammo2: bomber_stats.max_ammo2,
                    max_ammo2: bomber_stats.max_ammo2,
                },
            ))
            .id();
        world.spawn((Faction(PlayerId(2)), target, enemy_stats));

        let requirement = IslandCampaignRequirement {
            preferred_transport: Some(UnitType::TransportHelicopter),
            transport_slots: 2,
            capture_units: 1,
            ground_combat_units: 0,
            combat_units: 1,
            total_budget: bomber_stats.cost.saturating_add(5_000),
        };
        let assignment = IslandCampaignAssignment {
            island_id: target_island,
            decision: IslandCampaignDecision::Assault,
            target_position: target,
            capture_target_positions: vec![target],
            priority_enemy_types: vec![UnitType::Infantry],
            requirement: requirement.clone(),
            purchase_shortfall: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 1,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 1_000,
            },
            allocated_budget: bomber_stats.cost.saturating_add(4_000),
            transport_entities: vec![transport],
            capture_entities: Vec::new(),
            combat_entities: vec![bomber],
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        let mut manager = SquadManager::new();

        prepare_campaign_transport_assignment(&world, &mut manager, player, &assignment);
        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);

        let transport_squad = manager
            .squads
            .iter()
            .find(|squad| squad.plan.mission_type == MissionType::Transport)
            .expect("incomplete transport package should keep its forming squad");
        assert!(!transport_squad.cargo_entities.contains(&bomber));
        let attack = manager
            .squads
            .iter()
            .find(|squad| squad.plan.mission_type == MissionType::Attack)
            .expect("self-deploying bomber should receive an attack mission");
        assert!(attack.members.contains(&bomber));
        assert_eq!(attack.plan.target_island, Some(target_island));
        assert_eq!(attack.plan.target, Some(target));
    }

    #[test]
    fn empty_forming_placeholders_are_isolated_by_explicit_player_owner() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let world = setup_test_world();
        let player_a = PlayerId(1);
        let player_b = PlayerId(2);
        let island_id = world
            .resource::<crate::ai::islands::IslandMap>()
            .get_island_at(&GridPosition { x: 3, y: 3 })
            .unwrap()
            .id;
        let requirement = IslandCampaignRequirement {
            preferred_transport: Some(UnitType::TransportHelicopter),
            transport_slots: 2,
            capture_units: 2,
            ground_combat_units: 0,
            combat_units: 0,
            total_budget: 4_000,
        };
        let assignment = IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 3, y: 3 },
            capture_target_positions: vec![GridPosition { x: 3, y: 3 }],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: requirement,
            allocated_budget: 4_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        let mut manager = SquadManager::new();

        prepare_campaign_transport_assignment(&world, &mut manager, player_a, &assignment);
        prepare_campaign_transport_assignment(&world, &mut manager, player_b, &assignment);
        prepare_campaign_transport_assignment(&world, &mut manager, player_a, &assignment);

        let player_a_placeholders: Vec<_> = manager
            .squads
            .iter()
            .filter(|squad| squad.plan.owner_id == Some(player_a))
            .collect();
        let player_b_placeholders: Vec<_> = manager
            .squads
            .iter()
            .filter(|squad| squad.plan.owner_id == Some(player_b))
            .collect();
        assert_eq!(player_a_placeholders.len(), 1);
        assert_eq!(player_b_placeholders.len(), 1);
        assert_ne!(player_a_placeholders[0].id, player_b_placeholders[0].id);
        assert_eq!(player_a_placeholders[0].plan.target_island, Some(island_id));
        assert_eq!(player_b_placeholders[0].plan.target_island, Some(island_id));
    }

    #[test]
    fn defend_replaces_same_island_unreachable_stale_member_with_exact_reachable_set() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let port = GridPosition { x: 0, y: 0 };
        let bridge = GridPosition { x: 1, y: 0 };
        let defended = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(port.x, port.y, Terrain::Port).unwrap();
        map.set_terrain(bridge.x, bridge.y, Terrain::Plains)
            .unwrap();
        map.set_terrain(defended.x, defended.y, Terrain::City)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map.get_island_at(&defended).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);

        let player = PlayerId(1);
        let stale = world
            .spawn((
                Faction(player),
                port,
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Ship,
                    cost: 7_000,
                    ..UnitStats::mock()
                },
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 9,
                    max_ammo2: 9,
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let stale_duplicate = world
            .spawn((
                Faction(player),
                port,
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Ship,
                    cost: 7_000,
                    ..UnitStats::mock()
                },
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 9,
                    max_ammo2: 9,
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let reachable = world
            .spawn((
                Faction(player),
                bridge,
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    cost: 7_000,
                    ..UnitStats::mock()
                },
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 9,
                    max_ammo2: 9,
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 1,
            total_budget: 7_000,
        };
        let assignment = IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Defend,
            target_position: defended,
            capture_target_positions: vec![defended],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: 7_000,
            transport_entities: Vec::new(),
            capture_entities: Vec::new(),
            combat_entities: vec![reachable],
            operation_ready: true,
            continued_from_existing_squad: true,
        };
        let mut manager = SquadManager::new();
        let mut defense = manager.create_squad(MissionType::Defense);
        defense.members.insert(stale);
        defense.plan.target_island = Some(island_id);
        defense.plan.target = Some(defended);
        defense.plan.phase = MissionPhase::MovingToTarget;
        let mut duplicate = manager.create_squad(MissionType::Defense);
        duplicate.members.insert(stale_duplicate);
        duplicate.plan.target_island = Some(island_id);
        duplicate.plan.target = Some(defended);
        duplicate.plan.phase = MissionPhase::MovingToTarget;

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);

        let defense = manager
            .squads
            .iter()
            .find(|squad| squad.plan.mission_type == MissionType::Defense)
            .unwrap();
        assert_eq!(defense.members, BTreeSet::from([reachable]));
        assert_eq!(defense.plan.target_island, Some(island_id));
        assert_eq!(defense.plan.target, Some(defended));
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|squad| {
                    squad.plan.mission_type == MissionType::Defense
                        && squad.plan.target_island == Some(island_id)
                })
                .count(),
            1
        );
        assert!(manager.squads.iter().all(|squad| {
            !squad.members.contains(&stale) && !squad.members.contains(&stale_duplicate)
        }));
        let snapshot = manager.squads.clone();
        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);
        assert_eq!(manager.squads.len(), snapshot.len());
        for (actual, expected) in manager.squads.iter().zip(snapshot.iter()) {
            assert_eq!(actual.id, expected.id);
            assert_eq!(actual.plan.mission_type, expected.plan.mission_type);
            assert_eq!(actual.members, expected.members);
            assert_eq!(actual.plan.target_island, expected.plan.target_island);
            assert_eq!(actual.plan.target, expected.plan.target);
            assert_eq!(actual.plan.phase, expected.plan.phase);
        }
    }

    #[test]
    fn threatened_island_keeps_unowned_property_capture_separate_from_defense() {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let defended = GridPosition { x: 0, y: 0 };
        let staging = GridPosition { x: 1, y: 0 };
        let neutral = GridPosition { x: 2, y: 0 };
        let mut map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(defended.x, defended.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(neutral.x, neutral.y, Terrain::City)
            .unwrap();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map.get_island_at(&neutral).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(registry);
        world.insert_resource(island_map);

        let player = PlayerId(1);
        world.spawn((defended, Property::new(Terrain::Airport, Some(player), 100)));
        world.spawn((neutral, Property::new(Terrain::City, None, 100)));
        let capturer = world
            .spawn((
                Faction(player),
                staging,
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let defender = world
            .spawn((
                Faction(player),
                defended,
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 1,
            ground_combat_units: 0,
            combat_units: 1,
            total_budget: 8_000,
        };
        let assignment = IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Defend,
            target_position: defended,
            capture_target_positions: vec![neutral],
            priority_enemy_types: Vec::new(),
            requirement: requirement.clone(),
            purchase_shortfall: IslandCampaignRequirement {
                preferred_transport: None,
                transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units: 0,
                total_budget: 0,
            },
            allocated_budget: requirement.total_budget,
            transport_entities: Vec::new(),
            capture_entities: vec![capturer],
            combat_entities: vec![defender],
            operation_ready: true,
            continued_from_existing_squad: true,
        };
        let mut manager = SquadManager::new();

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);

        let capture = manager
            .squads
            .iter()
            .find(|squad| squad.plan.mission_type == MissionType::Capture)
            .expect("脅威下でも未所有施設の占領任務を維持する");
        assert_eq!(capture.members, BTreeSet::from([capturer]));
        assert_eq!(capture.plan.target, Some(neutral));
        let defense = manager
            .squads
            .iter()
            .find(|squad| squad.plan.mission_type == MissionType::Defense)
            .expect("占領要員とは別に防衛戦力を配置する");
        assert_eq!(defense.members, BTreeSet::from([defender]));
        assert_eq!(defense.plan.target, Some(defended));
        assert!(manager.squads.iter().all(|squad| {
            !(squad.members.contains(&capturer) && squad.members.contains(&defender))
        }));
    }

    /// 中立都市(3,0)を狙う占領兵(0,0)と、その手前(2,0)の敵歩兵、未合流の護衛ヘリを置いた
    /// 局地Captureの盤面を作る。割当には占領兵だけを含め、護衛は呼出し側で加える。
    fn local_capture_race_world() -> (
        World,
        PlayerId,
        crate::ai::island_campaign::IslandCampaignAssignment,
        Entity,
    ) {
        use crate::ai::island_campaign::{
            IslandCampaignAssignment, IslandCampaignDecision, IslandCampaignRequirement,
        };

        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let map = Map::new(5, 1, Terrain::Plains, GridTopology::Square);
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let target = GridPosition { x: 3, y: 0 };
        let island_id = island_map.get_island_at(&target).unwrap().id;
        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let mut settings = crate::ai::ai_version::PlayerAiSettings::new();
        settings.set_version(player, crate::ai::ai_version::AiVersion::V4);
        let mut damage_chart = crate::resources::DamageChart::new();
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Infantry, 50);
        damage_chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 70);
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.insert_resource(registry);
        world.insert_resource(settings);
        world.insert_resource(damage_chart);
        world.spawn((target, Property::new(Terrain::City, None, 100)));
        let capturer = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        world.spawn((
            Faction(enemy),
            GridPosition { x: 2, y: 0 },
            UnitStats {
                unit_type: UnitType::Infantry,
                movement_type: MovementType::Infantry,
                max_movement: 3,
                max_range: 1,
                can_capture: true,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));
        let escort = world
            .spawn((
                Faction(player),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Bcopters,
                    movement_type: MovementType::Air,
                    max_movement: 8,
                    max_range: 1,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let requirement = IslandCampaignRequirement {
            preferred_transport: None,
            transport_slots: 0,
            capture_units: 1,
            ground_combat_units: 0,
            combat_units: 1,
            total_budget: 8_500,
        };
        let assignment = IslandCampaignAssignment {
            island_id,
            decision: IslandCampaignDecision::Contest,
            target_position: target,
            capture_target_positions: vec![target],
            priority_enemy_types: vec![UnitType::Infantry],
            requirement: requirement.clone(),
            purchase_shortfall: requirement,
            allocated_budget: 8_500,
            transport_entities: Vec::new(),
            capture_entities: vec![capturer],
            combat_entities: Vec::new(),
            operation_ready: false,
            continued_from_existing_squad: false,
        };
        (world, player, assignment, escort)
    }

    #[test]
    fn v4_local_capture_launches_only_when_intercepting_enemy_has_timely_escort() {
        let (world, player, mut assignment, escort) = local_capture_race_world();
        let mut manager = SquadManager::new();

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);
        let capture = manager
            .squads
            .iter()
            .find(|squad| squad.plan.mission_type == MissionType::Capture)
            .expect("局地Capture");
        assert!(!capture.plan.departure_authorized);

        assignment.combat_entities.push(escort);
        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);
        let capture = manager
            .squads
            .iter()
            .find(|squad| squad.plan.mission_type == MissionType::Capture)
            .expect("護衛合流後の局地Capture");
        assert!(capture.plan.departure_authorized);
    }

    /// V4が作戦用に生産した配備台帳の戦闘Entityも、局地Captureの護衛として数える。
    /// campaignの戦闘枠に入っていなくても、盤上の味方戦力を理由に占領兵を待たせない。
    #[test]
    fn v4_deployment_combat_unit_counts_as_local_capture_escort() {
        let (mut world, player, assignment, escort) = local_capture_race_world();
        let mut deployments = crate::ai::v4::deployment::V4DeploymentRegistry::default();
        deployments.assign_target_for_test(player, escort, escort);
        world.insert_resource(deployments);
        let mut manager = SquadManager::new();

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);

        let capture = manager
            .squads
            .iter()
            .find(|squad| squad.plan.mission_type == MissionType::Capture)
            .expect("局地Capture");
        assert!(capture.plan.departure_authorized);
    }

    /// 敵へ到達できない戦闘memberは、現在地で待たずに前進先の施設の防御へ向かう。
    /// 前進先が無いときだけ、従来どおり現在地で防御する。
    #[test]
    fn unreachable_combat_members_advance_to_the_next_capture_target() {
        let mut world = World::new();
        let map = Map::new(10, 1, Terrain::Plains, GridTopology::Square);
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map
            .get_island_at(&GridPosition { x: 0, y: 0 })
            .unwrap()
            .id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let source = GridPosition { x: 1, y: 0 };
        let tank = world
            .spawn((
                Faction(PlayerId(1)),
                source,
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let next_property = GridPosition { x: 8, y: 0 };
        let mut connectivity = TerrainConnectivity::default();

        let advancing = campaign_combat_responsibilities(
            &world,
            PlayerId(1),
            island_id,
            &[tank],
            &[next_property],
            &mut connectivity,
        );
        let holding = campaign_combat_responsibilities(
            &world,
            PlayerId(1),
            island_id,
            &[tank],
            &[],
            &mut connectivity,
        );

        assert_eq!(advancing.len(), 1);
        assert_eq!(advancing[0].mission_type, MissionType::Defense);
        assert_eq!(advancing[0].target, next_property);
        assert_eq!(advancing[0].members, vec![tank]);
        assert_eq!(holding.len(), 1);
        assert_eq!(holding[0].target, source);
    }

    /// 小規模マップ展開の局地Captureは、拡張経路だけ護衛を待たずに出撃する
    /// （F06・H01の経路差を固定する）。通常経路は迎撃される占領兵を待機させる。
    #[test]
    fn small_map_capture_departs_without_escort_only_on_extended_path() {
        for (path, expected) in [
            (crate::ai::code_path::CodePath::MainCompatible, false),
            (crate::ai::code_path::CodePath::Extended, true),
        ] {
            let _guard = crate::ai::code_path::enter(path);
            let (mut world, player, assignment, _escort) = local_capture_race_world();
            world.insert_resource(
                crate::ai::strategy_profile::V4StrategyProfileRegistry::with_profile(
                    player,
                    crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion,
                ),
            );
            let mut manager = SquadManager::new();

            prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);
            let capture = manager
                .squads
                .iter()
                .find(|squad| squad.plan.mission_type == MissionType::Capture)
                .expect("局地Capture");
            assert_eq!(capture.plan.departure_authorized, expected, "{path:?}");
        }
    }
    #[test]
    fn shoal_is_legal_pickup_for_adjacent_ground_cargo() {
        let mut world = World::new();
        let registry = MasterDataRegistry::load().unwrap();
        let mut map = Map::new(2, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(1, 0, Terrain::Shoal).unwrap();
        world.insert_resource(map);
        world.insert_resource(registry);
        let cargo = world
            .spawn((
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: MovementType::Infantry,
                    max_movement: 3,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let transport_stats = UnitStats {
            unit_type: UnitType::Lander,
            movement_type: MovementType::Ship,
            max_movement: 6,
            loadable_unit_types: vec![UnitType::Infantry],
            ..UnitStats::mock()
        };

        assert_eq!(
            select_pickup_position(
                &world,
                PlayerId(1),
                GridPosition { x: 1, y: 0 },
                &transport_stats,
                &[cargo],
                &mut TerrainConnectivity::default(),
            ),
            Some(GridPosition { x: 1, y: 0 })
        );
    }
}

// 手番ごとの部隊再編の段と、通常／小規模V4の方策。
mod formation;

// 汎用編成の提案（Squadの新設・追加）と、1つの更新口での確定。
mod assignment;

// 小規模マップ戦略など拡張経路の実装。main互換経路と本文が異なる関数と拡張専用の項目を置く。
mod capture_responsibilities;
mod combat_responsibilities;
mod local_assignment;
mod planning_policy;

// 再編本体と通常／拡張hookの差分契約。
// 未追跡のテスト資産のため、ファイルが存在する場合だけ build.rs がcfgを立てる。
// rustfmtはcfgを評価せずにファイルを探すため、マクロ経由で宣言して存在しない
// クローンでも `cargo fmt` が通るようにする。
macro_rules! optional_test_module {
    ($name:ident) => {
        #[cfg(all(test, openwars_planning_contract_tests))]
        mod $name;
    };
}
optional_test_module!(planning_contract_tests);

mod transport_step;

mod update;

mod reconnection;
