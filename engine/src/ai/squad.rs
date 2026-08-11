#![allow(clippy::collapsible_if)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::manual_while_let_some)]
#![allow(clippy::unnecessary_map_or)]

use crate::ai::cluster::detect_enemy_clusters;
use crate::ai::strategy::{analyze_strategy, analyze_strategy_with_reserved_entities};
use crate::ai::turn_distance::{
    TerrainConnectivity, TurnDistanceCache, calculate_all_turn_distances, calculate_turn_distance,
    is_terrain_reachable,
};
use crate::components::{Ammo, Faction, GridPosition, Health, PlayerId, Property, UnitStats};
use crate::resources::{Map, Terrain, UnitType, master_data::MasterDataRegistry};
use crate::systems::movement::calculate_reachable_tiles;
use bevy_ecs::prelude::*;
use std::collections::{BTreeSet, HashMap, HashSet};

/// ミッションの種別
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionType {
    Attack,
    Capture,
    Defense,
    Transport,
    Interception(crate::ai::emergency::EmergencyMissionId),
}

/// 輸送ミッションの各フェーズ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportPhase {
    Pickup,
    Transit,
    Drop,
    Return,
}

/// 部隊が実行中のミッションの状態
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionPhase {
    Forming,
    MovingToTarget,
    Executing,
    Completed,
    Transport(TransportPhase),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SquadId(pub u32);

/// 部隊（Squad）の定義
#[derive(Debug, Clone)]
pub struct Squad {
    pub id: SquadId,
    /// Entityがまだ無いFormingもplayer間で共有しない、Squad自身の明示所有者。
    pub owner_id: Option<PlayerId>,
    /// 部隊メンバー。HashSet はプロセス・スレッドごとに反復順が変わり、
    /// 「先頭メンバーの位置」を基準にする探索が同一seedでも再現しなくなるため、
    /// Entity 順で安定する BTreeSet を用いる。
    pub members: BTreeSet<Entity>,
    pub mission_type: MissionType,
    pub target: Option<GridPosition>, // 攻撃・防衛・占領の目標座標
    pub target_island: Option<crate::ai::islands::IslandId>, // 輸送ターゲットの島
    pub phase: MissionPhase,
    /// 輸送部隊の輸送役。HashSet の列挙順ではなく明示的に保持する。
    pub transport_entity: Option<Entity>,
    /// この輸送部隊へ割り当てたカーゴ。占領要員を先頭にした決定的な順序で保持する。
    pub cargo_entities: Vec<Entity>,
    /// Pickup 時の合流位置。
    pub pickup_position: Option<GridPosition>,
    /// Transit/Drop で最後に選択した降車位置。
    pub drop_position: Option<GridPosition>,
    /// 降車済みで通常部隊への引き継ぎを待つカーゴ。
    pub delivered_cargo: Vec<Entity>,
    /// 中立島・兵站島への便は、搭載済みcargoを遠い後続cargo待ちで止めず逐次発進できる。
    /// 敵領Assaultは侵攻波の分断を避けるためfalseを維持する。
    pub allow_partial_departure: bool,
    /// 着陸地点の軽歩兵だけを排除した後、輸送任務のReturnへ復帰する一時護衛状態。
    pub return_after_combat: bool,
}

/// 全ての部隊を管理するリソース
#[derive(Resource, Default, Debug, Clone)]
pub struct SquadManager {
    pub squads: Vec<Squad>,
    pub solo_fallbacks: HashSet<Entity>,
    next_id: u32,
}

impl SquadManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_squad(&mut self, mission_type: MissionType) -> &mut Squad {
        let squad = Squad {
            id: SquadId(self.next_id),
            owner_id: None,
            members: BTreeSet::new(),
            mission_type,
            target: None,
            target_island: None,
            phase: MissionPhase::Forming,
            transport_entity: None,
            cargo_entities: Vec::new(),
            pickup_position: None,
            drop_position: None,
            delivered_cargo: Vec::new(),
            allow_partial_departure: false,
            return_after_combat: false,
        };
        self.next_id += 1;
        self.squads.push(squad);
        self.squads.last_mut().unwrap()
    }

    pub fn create_owned_squad(
        &mut self,
        mission_type: MissionType,
        owner_id: PlayerId,
    ) -> &mut Squad {
        let squad = self.create_squad(mission_type);
        squad.owner_id = Some(owner_id);
        squad
    }

    pub fn remove_squad(&mut self, id: SquadId) {
        self.squads.retain(|s| s.id != id);
    }
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
        return HashSet::new();
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

    let mut unit_positions = HashMap::new();
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
    match squad.owner_id {
        Some(owner) => {
            owner == player_id && inferred_owner.is_none_or(|inferred| inferred == owner)
        }
        // ownerless legacy Squadは既知Factionが一意な場合だけ安全に扱い、空Squadは誰にも開放しない。
        None => inferred_owner == Some(player_id),
    }
}

fn adopt_legacy_squad_owners(world: &World, manager: &mut SquadManager) {
    let mut squads: Vec<_> = manager.squads.iter_mut().collect();
    squads.sort_by_key(|squad| squad.id.0);
    for squad in squads {
        if squad.owner_id.is_none() {
            if let Ok(Some(owner)) = inferred_squad_owner(world, squad) {
                // 一意なlive Factionを持つlegacy Squadだけを決定的に移行する。
                squad.owner_id = Some(owner);
            }
        }
    }
}

fn is_purchase_campaign_placeholder(squad: &Squad, player_id: PlayerId) -> bool {
    squad.owner_id == Some(player_id)
        && squad.mission_type == MissionType::Transport
        && squad.phase == MissionPhase::Forming
        && squad.transport_entity.is_none()
        && squad.members.is_empty()
        && squad.cargo_entities.is_empty()
        && squad.delivered_cargo.is_empty()
        && squad.target_island.is_some()
        && squad.target.is_some()
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
    let mut manager = world.remove_resource::<SquadManager>().unwrap_or_default();
    adopt_legacy_squad_owners(world, &mut manager);

    // 存在しなくなったエンティティの削除
    let mut existing_entities = HashSet::new();
    let mut units_needing_fallback = Vec::new();
    let mut units_recovered = Vec::new();

    let mut faction_query = world.query::<(Entity, &Faction)>();
    for (entity, _) in faction_query.iter(world) {
        existing_entities.insert(entity);
    }
    let mut status_query = world.query::<(Entity, &Faction, &Health, Option<&Ammo>)>();
    for (entity, faction, health, ammo_opt) in status_query.iter(world) {
        if faction.0 != perspective_player {
            continue;
        }
        // SoloFallback の判定 (HP < 60 または 弾薬切れ)
        let mut no_ammo = false;
        if let Some(ammo) = ammo_opt {
            no_ammo =
                (ammo.max_ammo1 > 0 && ammo.ammo1 == 0) && (ammo.max_ammo2 > 0 && ammo.ammo2 == 0);
        }

        if health.current < 60 || no_ammo {
            units_needing_fallback.push(entity);
        } else if health.current >= 70 && !no_ammo {
            // 回復条件を満たした
            units_recovered.push(entity);
        }
    }

    // perspective player所有または実体所有者のないSquadだけをcleanupする。
    for squad in &mut manager.squads {
        if !squad_is_mutable_by_player(world, squad, perspective_player) {
            continue;
        }
        squad.members.retain(|e| existing_entities.contains(e));
        squad
            .cargo_entities
            .retain(|cargo| existing_entities.contains(cargo));
        squad
            .delivered_cargo
            .retain(|cargo| existing_entities.contains(cargo));
        if squad
            .transport_entity
            .is_some_and(|transport| !existing_entities.contains(&transport))
        {
            squad.transport_entity = None;
        }
    }

    // SoloFallback の更新
    manager
        .solo_fallbacks
        .retain(|e| existing_entities.contains(e));

    for e in units_needing_fallback {
        manager.solo_fallbacks.insert(e);
        // 所有者が一致するSquadだけから外し、foreign/mixed Squadは変更しない。
        for squad in &mut manager.squads {
            if squad_is_mutable_by_player(world, squad, perspective_player) {
                squad.members.remove(&e);
            }
        }
    }

    for e in units_recovered {
        manager.solo_fallbacks.remove(&e);
    }

    // 輸送ヘリの一時護衛は、対象軽歩兵が消えた時点で輸送任務へ戻す。
    // Attack Squadを完了させて遊兵化せず、次の占領波へ再利用できる状態まで帰還させる。
    for squad in &mut manager.squads {
        if !squad.return_after_combat
            || !squad_is_mutable_by_player(world, squad, perspective_player)
        {
            continue;
        }
        let next_target =
            squad
                .transport_entity
                .zip(squad.target_island)
                .and_then(|(transport, island)| {
                    light_infantry_target_for_transport(
                        world,
                        transport,
                        perspective_player,
                        island,
                    )
                });
        if let Some(target) = next_target {
            squad.target = Some(target);
        } else {
            squad.mission_type = MissionType::Transport;
            squad.target = None;
            squad.phase = MissionPhase::Transport(TransportPhase::Return);
            squad.return_after_combat = false;
        }
    }

    // 所有者を推定できない空Formingは購入作戦のidentityとして扱わず、任意playerへ継承しない。
    manager.squads.retain(|squad| {
        squad.owner_id.is_some()
            || squad.mission_type != MissionType::Transport
            || squad.phase != MissionPhase::Forming
            || squad.transport_entity.is_some()
            || !squad.members.is_empty()
            || !squad.cargo_entities.is_empty()
            || !squad.delivered_cargo.is_empty()
            || squad.target_island.is_none()
            || squad.target.is_none()
    });

    // 輸送部隊のフェーズ更新と完了判定
    let mut delivered_units = Vec::new();
    let mut deferred_pickups = Vec::new();
    let mut i = 0;
    while i < manager.squads.len() {
        if manager.squads[i].mission_type == MissionType::Transport
            && squad_is_mutable_by_player(world, &manager.squads[i], perspective_player)
        {
            let mut squad = manager.squads[i].clone();
            let preserve_purchase_placeholder =
                is_purchase_campaign_placeholder(&squad, perspective_player);
            if let Some(deferred) = detach_deferred_pickup_cargo(world, &mut squad) {
                deferred_pickups.push((
                    squad.owner_id.unwrap_or(perspective_player),
                    deferred,
                    squad.target_island,
                    squad.target,
                    squad.allow_partial_departure,
                ));
            }
            let should_remove = update_transport_squad_phase(world, &mut squad);
            if matches!(squad.phase, MissionPhase::Transport(TransportPhase::Return))
                && let (Some(transport), Some(target_island)) =
                    (squad.transport_entity, squad.target_island)
                && let Some(enemy_target) = light_infantry_target_for_transport(
                    world,
                    transport,
                    perspective_player,
                    target_island,
                )
            {
                // 最後の降車後も歩兵脅威が残るなら、武装輸送ヘリをただちに局地護衛へ
                // 転用する。対空・重装甲相手にはこの転用をせず、通常どおり帰還させる。
                squad.mission_type = MissionType::Attack;
                squad.target = Some(enemy_target);
                squad.pickup_position = None;
                squad.drop_position = None;
                squad.phase = MissionPhase::MovingToTarget;
                squad.return_after_combat = true;
            }
            let preserve_targetless_delivery = squad.transport_entity.is_none()
                && squad.target_island.is_none()
                && squad.target.is_none()
                && !squad.delivered_cargo.is_empty();
            delivered_units.extend(
                squad
                    .delivered_cargo
                    .drain(..)
                    .map(|cargo| (squad.id, cargo, squad.target_island, squad.target)),
            );
            if should_remove && !preserve_targetless_delivery && !preserve_purchase_placeholder {
                manager.squads.remove(i);
            } else {
                manager.squads[i] = squad;
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    // 部分搭載便から切り離したcargoは遊兵化させず、同じ作戦の後続便として保持する。
    // 後段のcampaign reconcileが利用可能な別輸送役へ再partitionする。
    for (owner_id, cargo, target_island, target, allow_partial_departure) in deferred_pickups {
        let deferred = manager.create_owned_squad(MissionType::Transport, owner_id);
        deferred.cargo_entities = cargo;
        deferred.target_island = target_island;
        deferred.target = target;
        deferred.allow_partial_departure = allow_partial_departure;
        deferred.phase = MissionPhase::Forming;
    }

    // 降車したユニットを輸送部隊から解放し、通常の占領・攻撃部隊へ引き渡す。
    for (source_id, cargo, target_island, preferred_target) in delivered_units {
        let handed_off = handoff_delivered_cargo(
            world,
            &mut manager,
            perspective_player,
            cargo,
            target_island,
            preferred_target,
        );
        if !handed_off && target_island.is_none() {
            let source_index = manager
                .squads
                .iter()
                .position(|squad| squad.id == source_id);
            let hold_index = source_index.or_else(|| {
                manager.squads.iter().position(|squad| {
                    squad.mission_type == MissionType::Transport
                        && squad.transport_entity.is_none()
                        && squad.members.is_empty()
                        && squad.target_island.is_none()
                        && squad.target.is_none()
                        && squad.phase == MissionPhase::Forming
                        && squad_is_mutable_by_player(world, squad, perspective_player)
                })
            });
            let hold = if let Some(index) = hold_index {
                &mut manager.squads[index]
            } else {
                // 帰還済み輸送から切り離した、現地cargo専用の一時holdをSquad派生状態で表す。
                manager.create_owned_squad(MissionType::Transport, perspective_player)
            };
            if hold.transport_entity.is_none() {
                // source輸送が消失済みでもcleanupで落ちない、輸送非依存の安全な派生状態へ正規化する。
                hold.owner_id = Some(perspective_player);
                hold.mission_type = MissionType::Transport;
                hold.members.clear();
                hold.cargo_entities.clear();
                hold.target_island = None;
                hold.target = None;
                hold.pickup_position = None;
                hold.drop_position = None;
                hold.phase = MissionPhase::Forming;
            }
            if !hold.delivered_cargo.contains(&cargo) {
                hold.delivered_cargo.push(cargo);
                hold.delivered_cargo.sort_by_key(|entity| entity.to_bits());
            }
        }
    }

    // 占領完了した Capture 部隊の解散
    let mut properties_ownership = HashMap::new();
    {
        let mut q_props = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in q_props.iter(world) {
            properties_ownership.insert(*pos, prop.owner_id);
        }
    }

    manager.squads.retain(|squad| {
        if squad_is_mutable_by_player(world, squad, perspective_player)
            && squad.mission_type == MissionType::Capture
        {
            if let Some(target_pos) = squad.target {
                if properties_ownership.get(&target_pos) == Some(&Some(perspective_player)) {
                    // 自軍の所有になったので解散
                    return false;
                }
            }
        }
        true
    });

    // 侵攻島の敵・未占領拠点が無くなった通常部隊は島拘束を解除する。
    if let Some(island_map) = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
    {
        let mut active_islands = HashSet::new();
        for (position, owner) in &properties_ownership {
            if *owner != Some(perspective_player)
                && let Some(island) = island_map.get_island_at(position)
            {
                active_islands.insert(island.id);
            }
        }
        let mut enemy_query = world.query::<(&GridPosition, &Faction)>();
        for (position, faction) in enemy_query.iter(world) {
            if faction.0 != perspective_player
                && let Some(island) = island_map.get_island_at(position)
            {
                active_islands.insert(island.id);
            }
        }
        for squad in &mut manager.squads {
            if squad_is_mutable_by_player(world, squad, perspective_player)
                && matches!(
                    squad.mission_type,
                    MissionType::Attack | MissionType::Defense
                )
                && squad
                    .target_island
                    .is_some_and(|island| !active_islands.contains(&island))
            {
                squad.target_island = None;
            }
        }
    }

    // メンバーが0になった部隊の解散
    manager.squads.retain(|s| {
        !s.members.is_empty()
            || s.phase == MissionPhase::Completed
            || s.phase == MissionPhase::Forming
                && (s.transport_entity.is_some()
                    || !s.cargo_entities.is_empty()
                    || !s.delivered_cargo.is_empty()
                    || s.target_island.is_some()
                    || s.target.is_some())
    });

    world.insert_resource(manager);
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

    let mut loaded_owner = HashMap::new();
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
    let has_target_forming = manager.squads.iter().any(|squad| {
        squad_is_mutable_by_player(world, squad, player_id)
            && squad.mission_type == MissionType::Transport
            && squad.phase == MissionPhase::Forming
            && squad.target_island == Some(assignment.island_id)
    });
    if !has_target_forming {
        // 通常のlive作戦だけになった後は再partitionせず、実行フェーズの進行をそのまま維持する。
        return false;
    }

    let assignment_transports: HashSet<_> = assignment.transport_entities.iter().copied().collect();
    let mut candidate_squads = Vec::new();
    manager.squads.retain(|squad| {
        let is_target_forming = squad_is_mutable_by_player(world, squad, player_id)
            && squad.mission_type == MissionType::Transport
            && squad.phase == MissionPhase::Forming
            && squad.target_island == Some(assignment.island_id);
        let is_target_live = squad_is_mutable_by_player(world, squad, player_id)
            && squad.mission_type == MissionType::Transport
            && squad.target_island == Some(assignment.island_id)
            && squad
                .transport_entity
                .is_some_and(|transport| assignment_transports.contains(&transport))
            && matches!(
                squad.phase,
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
                squad.phase,
                MissionPhase::Transport(TransportPhase::Transit | TransportPhase::Drop)
            )
        })
        .filter_map(|squad| squad.transport_entity)
        .collect();
    let Some(mut partitions) = build_ready_campaign_transport_partitions(
        world,
        player_id,
        assignment,
        &advanced_live_transports,
    ) else {
        // 完全なcapacity/loadability/rendezvousが成立しない場合は元の作戦群を保つ。
        manager.squads.extend(candidate_squads);
        manager.squads.sort_by_key(|squad| squad.id.0);
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
    for squad in &mut manager.squads {
        if !squad_is_mutable_by_player(world, squad, player_id) {
            continue;
        }
        squad
            .members
            .retain(|entity| !owned_entities.contains(entity));
        squad
            .cargo_entities
            .retain(|entity| !owned_entities.contains(entity));
        squad
            .delivered_cargo
            .retain(|entity| !owned_entities.contains(entity));
        if squad
            .transport_entity
            .is_some_and(|transport| owned_entities.contains(&transport))
        {
            squad.transport_entity = None;
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
        match squad.phase {
            MissionPhase::Transport(TransportPhase::Drop | TransportPhase::Transit) => {
                partition.all_loaded
            }
            MissionPhase::Transport(TransportPhase::Pickup) => {
                !partition.all_loaded && squad.pickup_position == Some(partition.pickup_position)
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
                    match squad.phase {
                        MissionPhase::Transport(TransportPhase::Drop) => 0u8,
                        MissionPhase::Transport(TransportPhase::Transit) => 1u8,
                        MissionPhase::Transport(TransportPhase::Pickup) => 2u8,
                        _ => 5u8,
                    }
                } else if squad.phase == MissionPhase::Forming {
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
                squad.phase == MissionPhase::Forming && !has_unincorporated_loaded_state(squad)
            });
            *selected = if let Some(index) = fallback {
                Some((candidate_squads.remove(index), false))
            } else {
                manager.create_owned_squad(MissionType::Transport, player_id);
                Some((manager.squads.pop().unwrap(), false))
            };
        }
    }

    let mut reconciled = Vec::with_capacity(partitions.len());
    for (partition, selected) in partitions.into_iter().zip(selected_squads) {
        let (mut squad, preserve_live_state) = selected.unwrap();
        squad.members.clear();
        squad.members.insert(partition.transport);
        squad.transport_entity = Some(partition.transport);
        squad.cargo_entities = partition.cargo;
        squad.target_island = Some(assignment.island_id);
        squad.target = Some(assignment.target_position);
        squad.delivered_cargo.clear();
        if !preserve_live_state {
            squad.allow_partial_departure = matches!(
                assignment.decision,
                crate::ai::island_campaign::IslandCampaignDecision::Expand
                    | crate::ai::island_campaign::IslandCampaignDecision::Secure
                    | crate::ai::island_campaign::IslandCampaignDecision::Contest
                    | crate::ai::island_campaign::IslandCampaignDecision::Reinforce
            );
            squad.pickup_position = Some(partition.pickup_position);
            squad.drop_position = None;
            // 新規またはForming Squadは、自身の全cargoが実搭載済みの場合だけTransitを許可する。
            squad.phase = MissionPhase::Transport(if partition.all_loaded {
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
        .filter(|squad| squad.phase == MissionPhase::Forming)
        .filter(has_unincorporated_loaded_state);

    // 空の重複候補は破棄し、assignment輸送役ごとに1 Squadだけを残す。
    manager.squads.extend(reconciled);
    manager.squads.extend(preserved_loaded_squads);
    manager.squads.sort_by_key(|squad| squad.id.0);
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
    is_terrain_reachable(
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
    if !matches!(
        assignment.decision,
        crate::ai::island_campaign::IslandCampaignDecision::Expand
            | crate::ai::island_campaign::IslandCampaignDecision::Secure
            | crate::ai::island_campaign::IslandCampaignDecision::Contest
            | crate::ai::island_campaign::IslandCampaignDecision::Reinforce
    ) {
        return false;
    }
    let Some(index) = manager
        .squads
        .iter()
        .enumerate()
        .filter(|(_, squad)| {
            squad_is_mutable_by_player(world, squad, player_id)
                && squad.mission_type == MissionType::Transport
                && squad.target_island == Some(assignment.island_id)
                && squad.phase == MissionPhase::Forming
        })
        .min_by_key(|(_, squad)| squad.id.0)
        .map(|(index, _)| index)
    else {
        return false;
    };

    let forming = manager.squads.remove(index);
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
        manager.squads.insert(index, forming);
        return false;
    }

    let mut connectivity = TerrainConnectivity::default();
    let mut launched_transports = HashSet::new();
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
            manager.squads.pop().unwrap()
        };
        wave.members.clear();
        wave.members.insert(transport);
        wave.transport_entity = Some(transport);
        wave.cargo_entities = cargo;
        wave.pickup_position = Some(pickup_position);
        wave.drop_position = None;
        wave.delivered_cargo.clear();
        wave.phase = MissionPhase::Transport(if unloaded.is_empty() {
            TransportPhase::Transit
        } else {
            TransportPhase::Pickup
        });
        wave.allow_partial_departure = true;
        waves.push(wave);
    }
    if waves.is_empty() {
        manager.squads.insert(index, forming);
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
        let follow_up = manager.create_owned_squad(MissionType::Transport, player_id);
        follow_up.members.extend(remaining_transports);
        follow_up.transport_entity = follow_up
            .members
            .iter()
            .min_by_key(|entity| entity.to_bits())
            .copied();
        follow_up.cargo_entities = remaining_cargo;
        follow_up.target_island = forming.target_island;
        follow_up.target = forming.target;
        follow_up.allow_partial_departure = true;
        follow_up.phase = MissionPhase::Forming;
    }
    manager.squads.extend(waves);
    manager.squads.sort_by_key(|squad| squad.id.0);
    true
}

/// portfolio assignmentごとにFormingを復元し、readyなら既存の安全なpartitionへ接続する。
fn prepare_campaign_transport_assignment(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
) -> bool {
    adopt_legacy_squad_owners(world, manager);
    for squad in manager.squads.iter_mut().filter(|squad| {
        squad_is_mutable_by_player(world, squad, player_id)
            && squad.mission_type == MissionType::Transport
            && squad.target_island == Some(assignment.island_id)
            && matches!(
                squad.phase,
                MissionPhase::Forming
                    | MissionPhase::Transport(
                        TransportPhase::Pickup | TransportPhase::Transit | TransportPhase::Drop
                    )
            )
    }) {
        // 再分析で兵站施設が特定された場合、進行中便も島の代表座標ではなく
        // 空港・港・都市という具体的な作戦施設へ目標を同期する。
        squad.target = Some(assignment.target_position);
    }
    if assignment.transport_entities.is_empty() && assignment.operation_ready {
        return false;
    }

    let assignment_transports: HashSet<_> = assignment.transport_entities.iter().copied().collect();
    let covered_transports: HashSet<_> = manager
        .squads
        .iter()
        .filter(|squad| squad_is_mutable_by_player(world, squad, player_id))
        .filter(|squad| squad.mission_type == MissionType::Transport)
        .filter(|squad| squad.target_island == Some(assignment.island_id))
        .filter_map(|squad| squad.transport_entity)
        .filter(|transport| assignment_transports.contains(transport))
        .collect();
    let forming_index = manager
        .squads
        .iter()
        .enumerate()
        .filter(|(_, squad)| {
            squad_is_mutable_by_player(world, squad, player_id)
                && squad.mission_type == MissionType::Transport
                && squad.target_island == Some(assignment.island_id)
                && squad.phase == MissionPhase::Forming
        })
        .min_by_key(|(_, squad)| squad.id.0)
        .map(|(index, _)| index);
    let uncovered_transports: Vec<_> = assignment
        .transport_entities
        .iter()
        .filter(|transport| !covered_transports.contains(transport))
        .copied()
        .collect();
    let needs_forming =
        forming_index.is_some() || !assignment.operation_ready || !uncovered_transports.is_empty();

    if needs_forming {
        let already_owned: HashSet<_> = manager
            .squads
            .iter()
            .filter(|squad| squad_is_mutable_by_player(world, squad, player_id))
            .filter(|squad| !squad.return_after_combat)
            .filter(|squad| squad.target_island == Some(assignment.island_id))
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
            .filter(|cargo| !entity_can_self_deploy_to_assignment(world, assignment, *cargo))
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

        // 購入待ちで作った空placeholderへ、後のターンに完成した実Entityを必ず追加入隊させる。
        // placeholderが存在するだけで新規Entityを無所属にすると、汎用戦闘や迎撃へ流出する。
        let index = forming_index.unwrap_or_else(|| {
            manager.create_owned_squad(MissionType::Transport, player_id);
            manager.squads.len() - 1
        });
        let squad = &mut manager.squads[index];
        squad.members.extend(uncovered_transports);
        if squad.transport_entity.is_none() {
            squad.transport_entity = squad
                .members
                .iter()
                .filter(|entity| assignment_transports.contains(entity))
                .min_by_key(|entity| entity.to_bits())
                .copied();
        }
        squad.cargo_entities.extend(forming_cargo);
        squad.cargo_entities.sort_by_key(|entity| entity.to_bits());
        squad.cargo_entities.dedup();
        squad.target_island = Some(assignment.island_id);
        squad.target = Some(assignment.target_position);
        squad.allow_partial_departure = matches!(
            assignment.decision,
            crate::ai::island_campaign::IslandCampaignDecision::Expand
                | crate::ai::island_campaign::IslandCampaignDecision::Secure
                | crate::ai::island_campaign::IslandCampaignDecision::Contest
                | crate::ai::island_campaign::IslandCampaignDecision::Reinforce
        );
        squad.phase = MissionPhase::Forming;
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
                    is_terrain_reachable(
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
        && let Some(target) =
            nearest_campaign_property_target(world, player_id, assignment.island_id, members)
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
                campaign_member_distance_to_position(world, *member, target)
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
        if let Some(target) =
            nearest_campaign_property_target(world, player_id, assignment.island_id, &[member])
        {
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
) -> Option<usize> {
    let position = world.get::<GridPosition>(member)?;
    let stats = world.get::<UnitStats>(member)?;
    let map = world.get_resource::<Map>()?;
    let registry = world.get_resource::<MasterDataRegistry>()?;
    is_terrain_reachable(
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
                || !is_terrain_reachable(
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
struct CampaignResponsibility {
    mission_type: MissionType,
    target: GridPosition,
    members: Vec<Entity>,
}

fn campaign_combat_responsibilities(
    world: &World,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    members: &[Entity],
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
                        campaign_member_attack_distance(world, *member, *enemy, *target)
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

    // 敵へ到達できないmemberは、全員が到達できるmember位置ごとに決定的に現地防御へまとめる。
    while !remaining.is_empty() {
        let mut hold_candidates: Vec<_> = remaining
            .iter()
            .filter_map(|source| {
                let target = world.get::<GridPosition>(*source).copied()?;
                let reachable: Vec<_> = remaining
                    .iter()
                    .filter_map(|member| {
                        campaign_member_distance_to_position(world, *member, target)
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
            MissionType::Transport => 3,
            MissionType::Interception(_) => 4,
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

fn assign_campaign_responsibilities(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    island_id: crate::ai::islands::IslandId,
    mut responsibilities: Vec<CampaignResponsibility>,
    managed_missions: &[MissionType],
) {
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
    let mut used_ids = HashSet::new();
    for responsibility in &responsibilities {
        let member_set: HashSet<_> = responsibility.members.iter().copied().collect();
        let existing_id = manager
            .squads
            .iter()
            .filter(|squad| !used_ids.contains(&squad.id))
            .filter(|squad| squad_is_mutable_by_player(world, squad, player_id))
            .filter(|squad| squad.mission_type == responsibility.mission_type)
            .filter(|squad| squad.target_island == Some(island_id))
            .min_by_key(|squad| {
                let compatibility = if squad.target == Some(responsibility.target) {
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

    // assignment memberを他責務・輸送cargoから先に外し、各partitionへ一度だけ所属させる。
    for squad in &mut manager.squads {
        if !squad_is_mutable_by_player(world, squad, player_id) {
            continue;
        }
        squad.members.retain(|entity| !all_members.contains(entity));
        squad
            .cargo_entities
            .retain(|entity| !all_members.contains(entity));
        squad
            .delivered_cargo
            .retain(|entity| !all_members.contains(entity));
    }
    for (responsibility, selected_id) in responsibilities.iter().zip(&selected_ids) {
        let squad = manager
            .squads
            .iter_mut()
            .find(|squad| squad.id == *selected_id)
            .unwrap();
        squad.members = responsibility.members.iter().copied().collect();
        squad.mission_type = responsibility.mission_type.clone();
        squad.target_island = Some(island_id);
        squad.target = Some(responsibility.target);
        if squad.phase == MissionPhase::Completed {
            squad.phase = MissionPhase::Forming;
        }
    }
    let selected_set: HashSet<_> = selected_ids.into_iter().collect();
    manager.squads.retain(|squad| {
        selected_set.contains(&squad.id)
            || !squad_is_mutable_by_player(world, squad, player_id)
            || squad.return_after_combat
            || squad.target_island != Some(island_id)
            || !managed_missions.contains(&squad.mission_type)
    });
    manager.squads.sort_by_key(|squad| squad.id.0);
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

fn prepare_campaign_local_assignment(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    assignment: &crate::ai::island_campaign::IslandCampaignAssignment,
) {
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
            let members: Vec<_> = assignment
                .capture_entities
                .iter()
                .chain(assignment.combat_entities.iter())
                .copied()
                .filter(|entity| {
                    if world
                        .get::<crate::components::Transporting>(*entity)
                        .is_some()
                    {
                        return false;
                    }
                    let (Some(position), Some(stats), Some(map), Some(registry)) = (
                        world.get::<GridPosition>(*entity),
                        world.get::<UnitStats>(*entity),
                        world.get_resource::<Map>(),
                        world.get_resource::<MasterDataRegistry>(),
                    ) else {
                        return false;
                    };
                    is_terrain_reachable(
                        map,
                        registry,
                        (position.x, position.y),
                        (assignment.target_position.x, assignment.target_position.y),
                        stats.movement_type,
                    )
                })
                .collect();
            assign_campaign_members(
                world,
                manager,
                player_id,
                MissionType::Defense,
                assignment.island_id,
                assignment.target_position,
                members,
            );
        }
        IslandCampaignDecision::Secure => {
            let capture = local_entities(&assignment.capture_entities);
            let responsibilities = campaign_assignment_capture_responsibilities(
                world, player_id, assignment, &capture,
            );
            assign_campaign_responsibilities(
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
                world, player_id, assignment, &capture,
            );
            assign_campaign_responsibilities(
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
                        || entity_can_self_deploy_to_assignment(world, assignment, *entity)
                })
                .collect::<Vec<_>>();
            let responsibilities =
                campaign_combat_responsibilities(world, player_id, assignment.island_id, &combat);
            assign_campaign_responsibilities(
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
}

fn prepare_secure_local_captures(
    world: &World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    portfolio: &crate::ai::island_campaign::IslandCampaignPortfolio,
) -> HashSet<Entity> {
    use crate::ai::island_campaign::{IslandCampaignDecision, campaign_unit_type_rank};

    let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() else {
        return HashSet::new();
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
    let mut protected = HashSet::new();
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
                    || manager.squads.iter().any(|squad| {
                        let references_entity = squad.members.contains(&entity)
                            || squad.cargo_entities.contains(&entity)
                            || squad.delivered_cargo.contains(&entity);
                        references_entity
                            && (matches!(squad.mission_type, MissionType::Interception(_))
                                || !squad_is_mutable_by_player(world, squad, player_id))
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
                for squad in &mut manager.squads {
                    if !squad_is_mutable_by_player(world, squad, player_id) {
                        continue;
                    }
                    squad.members.remove(&entity);
                    squad.cargo_entities.retain(|cargo| *cargo != entity);
                    squad.delivered_cargo.retain(|cargo| *cargo != entity);
                    if squad.transport_entity == Some(entity) {
                        squad.transport_entity = None;
                    }
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
    manager.squads.retain(|squad| {
        let is_empty_placeholder = squad.owner_id == Some(player_id)
            && squad.mission_type == MissionType::Transport
            && squad.phase == MissionPhase::Forming
            && squad.transport_entity.is_none()
            && squad.members.is_empty()
            && squad.cargo_entities.is_empty()
            && squad.delivered_cargo.is_empty()
            && squad.target_island.is_some();
        !is_empty_placeholder
            || !squad_is_mutable_by_player(world, squad, player_id)
            || squad
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

    manager.squads.retain_mut(|squad| {
        if !squad_is_mutable_by_player(world, squad, player_id)
            || squad.mission_type != MissionType::Transport
            || squad
                .target_island
                .is_none_or(|island| !paused_islands.contains(&island))
        {
            return true;
        }
        let Some(transport) = squad.transport_entity else {
            // 輸送役を特定できないidle Formingは解放し、現地通常部隊には触れない。
            return squad.phase != MissionPhase::Forming;
        };
        if world
            .get::<Faction>(transport)
            .is_none_or(|faction| faction.0 != player_id)
        {
            return true;
        }

        match squad.phase {
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
                squad.members.clear();
                squad.members.insert(transport);
                squad.cargo_entities = loaded;
                squad.target_island = None;
                squad.target = None;
                squad.pickup_position = None;
                squad.drop_position = None;
                squad.phase = MissionPhase::Transport(TransportPhase::Drop);
                true
            }
            MissionPhase::Transport(TransportPhase::Return) | MissionPhase::Completed => false,
            MissionPhase::Transport(TransportPhase::Transit | TransportPhase::Drop) => true,
            MissionPhase::MovingToTarget | MissionPhase::Executing => true,
        }
    });
}

fn interception_unavailable_entities(
    manager: &SquadManager,
    perspective_player: PlayerId,
) -> HashSet<Entity> {
    let mut unavailable = manager.solo_fallbacks.clone();
    for squad in &manager.squads {
        if squad.owner_id != Some(perspective_player)
            || squad.mission_type == MissionType::Transport
        {
            unavailable.extend(squad.members.iter().copied());
            unavailable.extend(squad.cargo_entities.iter().copied());
            unavailable.extend(squad.delivered_cargo.iter().copied());
            unavailable.extend(squad.transport_entity);
        }
    }
    unavailable
}

fn detach_interception_members(
    manager: &mut SquadManager,
    perspective_player: PlayerId,
    reserved_entities: &HashSet<Entity>,
) {
    for squad in &mut manager.squads {
        if squad.owner_id == Some(perspective_player)
            && squad.mission_type != MissionType::Transport
        {
            squad
                .members
                .retain(|entity| !reserved_entities.contains(entity));
        }
    }
    manager.squads.retain(|squad| {
        !squad.members.is_empty()
            || squad.mission_type == MissionType::Transport
            || squad.owner_id != Some(perspective_player)
    });
}

fn apply_interception_squads(
    manager: &mut SquadManager,
    perspective_player: PlayerId,
    plan: &crate::ai::emergency::EmergencyMissionPlan,
) {
    for mission in &plan.missions {
        let squad =
            manager.create_owned_squad(MissionType::Interception(mission.id), perspective_player);
        squad.members.insert(mission.assigned_entity);
        squad.target = Some(mission.target_position);
        squad.phase = MissionPhase::MovingToTarget;
    }
}

/// ゲームの戦略状況に基づいて、自動的に部隊の構築と新規メンバーの割り当てを行います。
pub fn plan_squads(world: &mut World, perspective_player: PlayerId) {
    // 1. まず既存部隊のクリーンアップと SoloFallback 判定を実行
    update_squads(world, perspective_player);

    // V3 の戦略拡張 (#53: 敵拠点の奪取目標化) を有効にするかどうか
    let is_v3 = crate::ai::resolve_player_ai_version(world, perspective_player).uses_v3_tactics();
    let is_v4 = crate::ai::resolve_player_ai_version(world, perspective_player)
        .uses_operation_driven_production();
    let mut manager = world.remove_resource::<SquadManager>().unwrap_or_default();
    let mut tactical_reserved_entities = HashSet::new();
    let strategy = if is_v3 {
        // 緊急ミッションは盤面から毎ターン再構築し、通常部隊より先に担当Entityを予約する。
        manager.squads.retain(|squad| {
            squad.owner_id != Some(perspective_player)
                || !matches!(squad.mission_type, MissionType::Interception(_))
        });
        let unavailable = interception_unavailable_entities(&manager, perspective_player);
        let deployment_entities = if is_v4 {
            world
                .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
                .map(|deployments| deployments.active_entities(perspective_player))
                .unwrap_or_default()
        } else {
            HashSet::new()
        };
        let emergency_plan = crate::ai::emergency::analyze_interceptions_with_protected(
            world,
            perspective_player,
            &unavailable,
            &deployment_entities,
        );
        let reserved_entities = emergency_plan.reserved_entities();
        tactical_reserved_entities.extend(reserved_entities.iter().copied());
        detach_interception_members(&mut manager, perspective_player, &reserved_entities);

        // 島嶼キャンペーン分析が緊急担当EntityとV4局地任務Entityを再予約しないようにする。
        // deploymentは生産目的が解消されるまで、汎用free poolより先に確保する。
        let mut strategy_reserved_entities = deployment_entities;
        strategy_reserved_entities.extend(reserved_entities.iter().copied());

        // 更新済みManagerを一時的に戻して、予約済みEntityを除外した戦略分析を行う。
        world.insert_resource(manager);
        let strategy = analyze_strategy_with_reserved_entities(
            world,
            perspective_player,
            &strategy_reserved_entities,
        );
        manager = world.remove_resource::<SquadManager>().unwrap_or_default();
        apply_interception_squads(&mut manager, perspective_player, &emergency_plan);
        world.insert_resource(emergency_plan);
        strategy
    } else {
        world.remove_resource::<crate::ai::emergency::EmergencyMissionPlan>();
        world.insert_resource(manager);
        let strategy = analyze_strategy(world, perspective_player);
        manager = world.remove_resource::<SquadManager>().unwrap_or_default();
        strategy
    };
    let enemy_clusters = detect_enemy_clusters(world, perspective_player);
    if is_v3 {
        let mut cache = world
            .remove_resource::<crate::ai::engine::AiTurnStrategyCache>()
            .unwrap_or_default();
        cache.set_campaign_portfolio(perspective_player, strategy.campaign_portfolio.clone());
        cache.mark_squads_planned(perspective_player);
        world.insert_resource(cache);
    }
    let paused_campaign_islands = if is_v3 {
        campaign_paused_islands(&strategy.campaign_portfolio)
    } else {
        HashSet::new()
    };
    let secure_reserved_entities = if is_v3 {
        apply_campaign_pauses(
            world,
            &mut manager,
            perspective_player,
            &paused_campaign_islands,
        );
        prepare_secure_local_captures(
            world,
            &mut manager,
            perspective_player,
            &strategy.campaign_portfolio,
        )
    } else {
        HashSet::new()
    };
    let mut campaign_assignments: Vec<_> = if is_v3 {
        strategy
            .campaign_portfolio
            .defenses
            .iter()
            .chain(strategy.campaign_portfolio.active_offensives.iter())
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    campaign_assignments.sort_by_key(campaign_assignment_priority);
    let active_campaign_islands: HashSet<_> = campaign_assignments
        .iter()
        .map(|assignment| assignment.island_id)
        .collect();
    remove_abandoned_campaign_placeholders(
        world,
        &mut manager,
        perspective_player,
        &active_campaign_islands,
    );
    let mut all_campaign_reserved_entities: HashSet<_> = campaign_assignments
        .iter()
        .flat_map(|assignment| {
            assignment
                .transport_entities
                .iter()
                .chain(assignment.capture_entities.iter())
                .chain(assignment.combat_entities.iter())
        })
        .copied()
        .collect();
    all_campaign_reserved_entities.extend(secure_reserved_entities);
    let mut blocked_campaign_islands = HashSet::new();
    for assignment in &campaign_assignments {
        if prepare_campaign_transport_assignment(
            world,
            &mut manager,
            perspective_player,
            assignment,
        ) {
            blocked_campaign_islands.insert(assignment.island_id);
        }
        prepare_campaign_local_assignment(world, &mut manager, perspective_player, assignment);
    }

    // V4のCombat/Intercept枠で生産した実Entityは、緊急迎撃の次、
    // campaign再配分とgeneric free poolより前に要求元の局地Attack任務へ予約する。
    // 既存campaign assignmentへ残っていても、明示的な生産目的を優先して切り離す。
    if is_v4 {
        let deployment_reserved = crate::ai::v4::deployment::prepare_deployment_squads(
            world,
            &mut manager,
            perspective_player,
            &tactical_reserved_entities,
        );
        all_campaign_reserved_entities.extend(deployment_reserved);
    }

    // #53 (V3): メンバーが全滅した占領部隊を解散する。
    // 残しておくと「そのターゲットには部隊が存在する」と誤判定され続け
    // (dedupe に引っかかる)、失敗した奪取目標へ二度と部隊が送られなくなる
    if is_v3 {
        manager
            .squads
            .retain(|s| s.mission_type != MissionType::Capture || !s.members.is_empty());
    }

    let map = world.resource::<Map>().clone();
    let registry = world
        .get_resource::<MasterDataRegistry>()
        .cloned()
        .unwrap_or_default();
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
        .unwrap_or_else(|| crate::ai::islands::IslandMap::analyze(&map));

    // 他の部隊にすでに割り当て済みの全メンバーを特定
    let mut busy_entities = HashSet::new();
    for squad in &manager.squads {
        for &member in &squad.members {
            busy_entities.insert(member);
        }
        busy_entities.extend(squad.cargo_entities.iter().copied());
        busy_entities.extend(squad.delivered_cargo.iter().copied());
    }

    // 占有情報（経路探索用）
    let mut unit_positions = HashMap::new();
    let mut q_all_units = world.query::<(
        &Faction,
        &GridPosition,
        &UnitStats,
        Option<&crate::components::Transporting>,
        Option<&crate::components::CargoCapacity>,
    )>();
    for (faction, pos, stats, transporting, cargo) in q_all_units.iter(world) {
        if transporting.is_some() {
            continue;
        }
        unit_positions.insert(
            (pos.x, pos.y),
            crate::systems::movement::OccupantInfo {
                player_id: faction.0,
                is_transport: stats.max_cargo > 0,
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots: cargo
                    .map(|capacity| capacity.max.saturating_sub(capacity.loaded.len() as u32))
                    .unwrap_or(0),
            },
        );
    }

    let mut turn_cache = TurnDistanceCache::default();
    // マップ全体のフラッドフィルを毎回やり直さないよう、地形連結判定を使い回す。
    let mut connectivity = TerrainConnectivity::default();

    // フリーの自軍ユニットを収集
    let mut free_combat_units = Vec::new();
    let mut free_infantry = Vec::new();
    let mut free_transports = Vec::new();

    let mut q_my_units = world.query::<(
        Entity,
        &Faction,
        &GridPosition,
        &UnitStats,
        Option<&crate::components::Transporting>,
        Option<&crate::components::Fuel>,
        Option<&crate::components::CargoCapacity>,
    )>();
    for (entity, faction, pos, stats, transporting, fuel, cargo) in q_my_units.iter(world) {
        if faction.0 == perspective_player
            && !busy_entities.contains(&entity)
            && !all_campaign_reserved_entities.contains(&entity)
            && !manager.solo_fallbacks.contains(&entity)
            && transporting.is_none()
            && fuel.is_none_or(|fuel| fuel.current > 0)
        {
            let is_transport = stats.unit_type == UnitType::TransportHelicopter
                || stats.unit_type == UnitType::Lander;
            let is_infantry =
                stats.unit_type == UnitType::Infantry || stats.unit_type == UnitType::Mech;

            if is_transport {
                if cargo.is_some_and(|capacity| capacity.max > 0) {
                    free_transports.push((entity, *pos, stats.clone()));
                }
            } else if is_infantry {
                free_infantry.push((entity, *pos, stats.clone()));
            } else {
                free_combat_units.push((entity, *pos, stats.clone()));
            }
        }
    }

    // A. 輸送部隊の割り当て（planner.rs からの統合）
    let mut base_islands = HashSet::new();
    let mut target_islands = Vec::new();
    let mut properties_ownership = HashMap::new();
    {
        let mut q_props = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in q_props.iter(world) {
            properties_ownership.insert(*pos, prop.owner_id);
        }
    }
    let (b_islands, t_islands) =
        island_map.classify_islands(perspective_player, &properties_ownership);
    base_islands.extend(b_islands.iter().copied());
    target_islands.extend(t_islands.iter().copied());

    // ---------------------------------------------------------
    // V1からの統合: 島ごとの期待値（IslandScore）の算出
    // ---------------------------------------------------------
    let mut own_production_bases = Vec::new();
    let mut enemy_production_count_map = std::collections::HashMap::new();
    let mut enemy_owned_islands = HashSet::new();
    let mut island_props_map = std::collections::HashMap::new();

    for (pos, owner) in &properties_ownership {
        if let Some(island) = island_map.get_island_at(pos) {
            island_props_map
                .entry(island.id)
                .or_insert_with(Vec::new)
                .push(*pos);

            let terrain = map
                .get_terrain(pos.x, pos.y)
                .unwrap_or(crate::resources::Terrain::City);
            if *owner == Some(perspective_player) {
                if terrain == crate::resources::Terrain::Factory
                    || terrain == crate::resources::Terrain::Capital
                {
                    own_production_bases.push(*pos);
                }
            } else if owner.is_some() {
                enemy_owned_islands.insert(island.id);
                if terrain == crate::resources::Terrain::Factory
                    || terrain == crate::resources::Terrain::Capital
                {
                    *enemy_production_count_map.entry(island.id).or_insert(0) += 1;
                }
            }
        }
    }

    let mut objectives = Vec::new();
    for &target_id in &target_islands {
        let mut min_distance = i32::MAX;
        let mut props_with_terrain = Vec::new();

        if let Some(positions) = island_props_map.get(&target_id) {
            for tile in positions {
                if properties_ownership.get(tile) == Some(&Some(perspective_player)) {
                    continue; // 自分の拠点はスキップ
                }
                props_with_terrain.push((
                    *tile,
                    map.get_terrain(tile.x, tile.y)
                        .unwrap_or(crate::resources::Terrain::City),
                ));

                let mut local_min_dist = i32::MAX;
                let mut nearest_base_pos = None;
                for base_pos in &own_production_bases {
                    let dist = (tile.x as i32 - base_pos.x as i32).abs()
                        + (tile.y as i32 - base_pos.y as i32).abs();
                    if dist < local_min_dist {
                        local_min_dist = dist;
                        nearest_base_pos = Some(*base_pos);
                    }
                }

                // 徒歩圏内（6マス以下かつ同じ島）は除外
                if nearest_base_pos
                    .and_then(|p| island_map.get_island_at(&p))
                    .filter(|base_island| base_island.id == target_id && local_min_dist <= 6)
                    .is_some()
                {
                    continue;
                }

                if local_min_dist < min_distance {
                    min_distance = local_min_dist;
                }
            }
        }

        let distance_to_nearest_base = if min_distance == i32::MAX {
            10
        } else {
            min_distance
        };
        let enemy_prod = *enemy_production_count_map.get(&target_id).unwrap_or(&0);

        let objective = crate::ai::objectives::Objective::evaluate(
            target_id,
            &props_with_terrain,
            distance_to_nearest_base,
            enemy_prod,
            &registry,
        );
        objectives.push(objective);
    }

    if is_v3 {
        let assigned_islands: HashSet<_> = campaign_assignments
            .iter()
            .map(|assignment| assignment.island_id)
            .collect();
        // portfolio assignmentは上の島別bridgeだけが所有し、旧経路は未予約の島内目標に限定する。
        objectives.retain(|objective| {
            !assigned_islands.contains(&objective.target_island)
                && !blocked_campaign_islands.contains(&objective.target_island)
                && !paused_campaign_islands.contains(&objective.target_island)
                && base_islands.contains(&objective.target_island)
                && !enemy_owned_islands.contains(&objective.target_island)
        });
    }

    // スコア降順ソート（海を渡る必要がある別島を最優先）。
    // 同点時は島IDで決定し、HashSet や ECS の列挙順へ依存しない。
    objectives.sort_by_key(|objective| {
        let is_same_island = base_islands.contains(&objective.target_island);
        let group = if is_same_island { 1u8 } else { 0u8 }; // 別島=0(高優先), 同島=1(低優先)
        (
            group,
            std::cmp::Reverse(objective.priority_score),
            objective.target_island.0,
        )
    });

    // #54 (V3): 自軍の歩兵 (占領要員) が存在する島の集合。
    // 輸送カーゴの選定で「占領要員が未着の島へはまず歩兵を送る」判定に使う
    let mut my_infantry_islands = HashSet::new();
    if is_v3 {
        for ((x, y), occ) in &unit_positions {
            if occ.player_id == perspective_player
                && matches!(occ.unit_type, UnitType::Infantry | UnitType::Mech)
            {
                let pos = GridPosition { x: *x, y: *y };
                if let Some(island) = island_map.get_island_at(&pos) {
                    my_infantry_islands.insert(island.id);
                }
            }
        }
    }

    // 輸送役の選択順を座標と Entity ID で固定する。
    // 逆順にソートし、後続のループで pop() (O(1)) から取り出せるようにする。
    free_transports
        .sort_by_key(|(entity, pos, _)| std::cmp::Reverse((pos.y, pos.x, entity.to_bits())));

    // 優先順位の高い島から輸送機を割り当てる
    for objective in objectives.iter() {
        if free_transports.is_empty() {
            break;
        }

        let mut to_assign = objective.needed_infantry.0;
        let target_position = strategy
            .campaign_portfolio
            .assignment_for(objective.target_island)
            .map(|assignment| assignment.target_position)
            .or_else(|| {
                island_props_map
                    .get(&objective.target_island)
                    .and_then(|positions| {
                        let mut positions = positions.clone();
                        positions.sort_by_key(|position| (position.y, position.x));
                        positions.into_iter().find(|position| {
                            properties_ownership.get(position) != Some(&Some(perspective_player))
                        })
                    })
            });
        let mut enemy_types = Vec::new();
        let mut enemy_query = world.query::<(&GridPosition, &Faction, &UnitStats)>();
        for (position, faction, stats) in enemy_query.iter(world) {
            if faction.0 != perspective_player
                && island_map
                    .get_island_at(position)
                    .is_some_and(|island| island.id == objective.target_island)
            {
                enemy_types.push(stats.unit_type);
            }
        }
        enemy_types.sort_by_key(|unit_type| *unit_type as u8);
        enemy_types.dedup();
        let damage_chart = world
            .get_resource::<crate::resources::DamageChart>()
            .cloned();

        while to_assign > 0 && !free_transports.is_empty() {
            let (transport_entity, transport_position, transport_stats) =
                free_transports.pop().unwrap();
            let capacity = world
                .get::<crate::components::CargoCapacity>(transport_entity)
                .map(|cargo| cargo.max as usize)
                .unwrap_or(0);
            if capacity == 0 {
                continue;
            }

            let mut cargo_entities = world
                .get::<crate::components::CargoCapacity>(transport_entity)
                .map(|cargo| cargo.loaded.clone())
                .unwrap_or_default();
            cargo_entities.sort_by_key(|entity| entity.to_bits());
            cargo_entities.dedup();
            cargo_entities.truncate(capacity);
            let initially_loaded_count = cargo_entities.len();
            let mut assigned_capture_count = cargo_entities
                .iter()
                .filter(|entity| {
                    world
                        .get::<UnitStats>(**entity)
                        .is_some_and(|stats| stats.can_capture)
                })
                .count();
            let mut selected_entries = Vec::new();

            if cargo_entities.len() < capacity {
                // V3 は不足している占領要員を先に補い、残り容量へ有効な戦闘要員を積む。
                // V2 は従来どおり戦闘要員を先に検討する。
                let search_order = if is_v3 && assigned_capture_count == 0 {
                    [false, true]
                } else {
                    [true, false]
                };

                for search_combat in search_order {
                    if cargo_entities.len() >= capacity {
                        break;
                    }
                    let candidates = if search_combat {
                        &free_combat_units
                    } else {
                        &free_infantry
                    };
                    let Some(index) = select_nearest_compatible_cargo(
                        candidates,
                        transport_position,
                        &transport_stats,
                        objective.target_island,
                        target_position,
                        &island_map,
                        &map,
                        &registry,
                        &unit_positions,
                        perspective_player,
                        &mut turn_cache,
                        &mut connectivity,
                        &enemy_types,
                        damage_chart.as_ref(),
                        search_combat,
                    ) else {
                        continue;
                    };
                    let entry = if search_combat {
                        free_combat_units.swap_remove(index)
                    } else {
                        free_infantry.swap_remove(index)
                    };
                    if entry.2.can_capture {
                        assigned_capture_count += 1;
                    }
                    cargo_entities.push(entry.0);
                    selected_entries.push((search_combat, entry));

                    // V2 は単一カーゴ運用を維持し、V3 のみ残り容量へ護衛を追加する。
                    if !is_v3 {
                        break;
                    }
                }

                while is_v3 && cargo_entities.len() < capacity {
                    let Some(index) = select_nearest_compatible_cargo(
                        &free_infantry,
                        transport_position,
                        &transport_stats,
                        objective.target_island,
                        target_position,
                        &island_map,
                        &map,
                        &registry,
                        &unit_positions,
                        perspective_player,
                        &mut turn_cache,
                        &mut connectivity,
                        &enemy_types,
                        damage_chart.as_ref(),
                        false,
                    ) else {
                        break;
                    };
                    let entry = free_infantry.swap_remove(index);
                    if entry.2.can_capture {
                        assigned_capture_count += 1;
                    }
                    cargo_entities.push(entry.0);
                    selected_entries.push((false, entry));
                }
            }

            if cargo_entities.is_empty() {
                continue;
            }
            let requires_pickup = cargo_entities.len() > initially_loaded_count;
            let mut pickup_position = if requires_pickup {
                select_pickup_position(
                    world,
                    perspective_player,
                    transport_position,
                    &transport_stats,
                    &cargo_entities[initially_loaded_count..],
                    &mut connectivity,
                )
            } else {
                Some(transport_position)
            };
            if pickup_position.is_none() {
                // 不成立の組み合わせは free pool へ戻し、搭載済みカーゴだけで継続する。
                for (is_combat, entry) in selected_entries.drain(..) {
                    if entry.2.can_capture {
                        assigned_capture_count = assigned_capture_count.saturating_sub(1);
                    }
                    if is_combat {
                        free_combat_units.push(entry);
                    } else {
                        free_infantry.push(entry);
                    }
                }
                cargo_entities.truncate(initially_loaded_count);
                if cargo_entities.is_empty() {
                    continue;
                }
                pickup_position = Some(transport_position);
            }

            let squad = manager.create_owned_squad(MissionType::Transport, perspective_player);
            squad.members.insert(transport_entity);
            squad.transport_entity = Some(transport_entity);
            squad.cargo_entities = cargo_entities;
            squad.target_island = Some(objective.target_island);
            squad.target = target_position;
            squad.pickup_position = pickup_position;
            squad.allow_partial_departure = true;
            squad.phase =
                MissionPhase::Transport(if requires_pickup && !selected_entries.is_empty() {
                    TransportPhase::Pickup
                } else {
                    TransportPhase::Transit
                });

            to_assign = to_assign.saturating_sub(assigned_capture_count);
        }
    }

    free_transports.sort_by_key(|(entity, position, _)| (position.y, position.x, entity.to_bits()));

    // campaignへ予約されていない搭載済み輸送機は、別assignmentへ転用せず安全に降ろす。
    let mut i = 0;
    while i < free_transports.len() {
        let (transport_entity, transport_position, _) = free_transports[i].clone();
        let mut cargo_entities = world
            .get::<crate::components::CargoCapacity>(transport_entity)
            .map(|cargo| cargo.loaded.clone())
            .unwrap_or_default();
        cargo_entities.sort_by_key(|entity| entity.to_bits());
        cargo_entities.dedup();

        if !cargo_entities.is_empty() {
            let squad = manager.create_owned_squad(MissionType::Transport, perspective_player);
            squad.members.insert(transport_entity);
            squad.transport_entity = Some(transport_entity);
            squad.cargo_entities = cargo_entities;
            squad.pickup_position = Some(transport_position);
            squad.target_island = None;
            squad.target = None;
            squad.phase = MissionPhase::Transport(TransportPhase::Drop);
            free_transports.remove(i);
        } else {
            i += 1;
        }
    }

    // B. 防衛部隊の立ち上げ（Defenseフェーズ）
    let mut my_capital_pos = None;
    let mut q_props = world.query::<(&GridPosition, &Property)>();
    for (pos, prop) in q_props.iter(world) {
        if prop.terrain == Terrain::Capital && prop.owner_id == Some(perspective_player) {
            my_capital_pos = Some(*pos);
            break;
        }
    }

    if let Some(capital) = my_capital_pos {
        for cluster in &enemy_clusters {
            let turn_dist = calculate_turn_distance(
                &map,
                &registry,
                &unit_positions,
                (capital.x, capital.y),
                (cluster.center.x, cluster.center.y),
                crate::resources::MovementType::Infantry,
                3,
                1,
                perspective_player,
                &mut turn_cache,
            );

            if turn_dist.turns <= 5 {
                // すでにこのクラスターを防衛目標としている部隊があるか
                let exists = manager.squads.iter().any(|s| {
                    squad_is_mutable_by_player(world, s, perspective_player)
                        && s.mission_type == MissionType::Defense
                        && s.target == Some(cluster.center)
                });

                if !exists {
                    let squad =
                        manager.create_owned_squad(MissionType::Defense, perspective_player);
                    squad.target = Some(cluster.center);
                    squad.phase = MissionPhase::Forming;

                    // 最寄りの戦闘ユニットを最大2基割り当てる
                    free_combat_units.sort_by_key(|(_, pos, stats)| {
                        calculate_turn_distance(
                            &map,
                            &registry,
                            &unit_positions,
                            (pos.x, pos.y),
                            (cluster.center.x, cluster.center.y),
                            stats.movement_type,
                            stats.max_movement,
                            1,
                            perspective_player,
                            &mut turn_cache,
                        )
                    });

                    let assign_count = free_combat_units.len().min(3);
                    for _ in 0..assign_count {
                        let (ent, _, _) = free_combat_units.remove(0);
                        squad.members.insert(ent);
                    }
                }
            }
        }
    }

    // C. 占領部隊の立ち上げ（Expansionフェーズ）
    // #53 (V3): 中立拠点に加えて敵所有拠点も奪取目標に含める。
    // 従来は中立拠点のみが対象だったため、中立を取り切ると占領部隊が
    // 編成されなくなり、敵領土の奪取が発生しなかった。
    let mut capture_targets: Vec<GridPosition> =
        strategy.unowned_properties.iter().cloned().collect();
    if is_v3 {
        capture_targets.extend(strategy.enemy_properties.iter().cloned());
        capture_targets.retain(|target| {
            island_map
                .get_island_at(target)
                .is_none_or(|island| !paused_campaign_islands.contains(&island.id))
        });
    }
    // 最寄りのフリー歩兵から近い順に割り当てる (HashSet 順の非決定性も排除し、
    // 限られた歩兵を近い目標へ優先的に振り分ける)。
    // #53 (V3): 工場・首都などの生産施設は敵の増援源であり、奪取すれば
    // 前線の消耗戦を根本から崩せるため、多少遠くても優先する
    // (前線都市の取り返し合いに全歩兵が吸われる膠着の解消)
    const PRODUCTION_FACILITY_DIST_BONUS: usize = 6;
    // #53 (V3): 敵生産施設への奪取部隊は同時に1個まで (集中スピアヘッド)。
    // 複数同時に編成すると前線から兵力が抜かれすぎて防衛線が崩壊し、
    // 逐次投入の各個撃破で全部隊が溶けるため
    const MAX_CONCURRENT_FACILITY_CAPTURES: usize = 1;
    let is_enemy_facility = |t: &GridPosition| -> bool {
        properties_ownership
            .get(t)
            .is_some_and(|o| o.is_some() && *o != Some(perspective_player))
            && map
                .get_terrain(t.x, t.y)
                .is_some_and(|tr| registry.is_production_facility(tr.as_str()))
    };
    let mut active_facility_captures = manager
        .squads
        .iter()
        .filter(|s| {
            squad_is_mutable_by_player(world, s, perspective_player)
                && s.mission_type == MissionType::Capture
                && !s.members.is_empty()
                && s.target.as_ref().is_some_and(&is_enemy_facility)
        })
        .count();
    capture_targets.sort_by_key(|t| {
        let d = free_infantry
            .iter()
            .map(|(_, pos, _)| pos.x.abs_diff(t.x) + pos.y.abs_diff(t.y))
            .min()
            .unwrap_or(usize::MAX);
        let facility_bonus = if is_v3
            && map
                .get_terrain(t.x, t.y)
                .is_some_and(|tr| registry.is_production_facility(tr.as_str()))
        {
            PRODUCTION_FACILITY_DIST_BONUS
        } else {
            0
        };
        (d.saturating_sub(facility_bonus), t.x, t.y)
    });

    // #53 (V3): 敵生産施設への突入 (スピアヘッド) は戦力優勢 (Assault フェーズ)
    // のときのみ許可する。拮抗・劣勢時に前線から兵力を抜くと防衛線が崩壊する
    let allow_facility_capture = strategy.phase == crate::ai::strategy::GamePhase::Assault;

    for unowned_pos in &capture_targets {
        // #53 (V3): 敵生産施設への奪取部隊は同時 MAX_CONCURRENT_FACILITY_CAPTURES 個まで
        let target_is_enemy_facility = is_enemy_facility(unowned_pos);
        if target_is_enemy_facility
            && (!allow_facility_capture
                || active_facility_captures >= MAX_CONCURRENT_FACILITY_CAPTURES)
        {
            continue;
        }

        let target_island_opt = island_map.get_island_at(unowned_pos);
        if target_island_opt.is_none() {
            continue;
        }
        let target_island = target_island_opt.unwrap();
        let is_on_base_island = base_islands.contains(&target_island.id);

        // この未占領拠点と同じ島にいるフリーの歩兵を探す
        let inf_on_same_island_idx = free_infantry.iter().position(|(_, pos, stats)| {
            island_map
                .get_island_at(pos)
                .is_some_and(|island| island.id == target_island.id)
                && is_terrain_reachable(
                    &map,
                    &registry,
                    (pos.x, pos.y),
                    (unowned_pos.x, unowned_pos.y),
                    stats.movement_type,
                )
        });

        // V3の洋上移動はportfolio transportだけが担当し、generic Captureは到達可能な現地要員に限定する。
        let can_capture = inf_on_same_island_idx.is_some()
            || !is_v3 && is_on_base_island && !free_infantry.is_empty();

        if can_capture {
            let exists = manager.squads.iter().any(|s| {
                squad_is_mutable_by_player(world, s, perspective_player)
                    && s.mission_type == MissionType::Capture
                    && s.target == Some(*unowned_pos)
            });

            if !exists {
                let squad = manager.create_owned_squad(MissionType::Capture, perspective_player);
                squad.target = Some(*unowned_pos);
                if is_v3 {
                    squad.target_island = Some(target_island.id);
                }
                squad.phase = MissionPhase::Forming;

                // 割り当てる歩兵の選択：
                // 同じ島にいる歩兵がいればそれを優先、いなければ free_infantry の最後の歩兵を割り当てる
                let assigned_inf_idx = if let Some(idx) = inf_on_same_island_idx {
                    idx
                } else {
                    free_infantry.len() - 1
                };

                let (inf_ent, _, inf_stats) = free_infantry.remove(assigned_inf_idx);
                let inf_movement = inf_stats.max_movement;
                squad.members.insert(inf_ent);

                // #53 (V3): 敵生産施設 (工場・首都等) への奪取部隊には護衛の
                // 戦闘ユニットを最大2両随伴させる。敵の生産圏は防御が厚く、
                // 単独の歩兵では到達前に撃破されて奪取が成立しないため。
                // 護衛は「歩兵に随伴できる機動力があり戦闘可能」なユニットに限定する
                // (鈍足の砲台や弾切れ・損傷ユニットを組み込むと部隊が機能しないため)
                if is_v3 && target_is_enemy_facility {
                    // 対象拠点から「遠い順」に並べ替え、末尾 (最も近いユニット) から pop() で
                    // 取り出すことで Vec 先頭削除による O(N) シフトを避ける (#57 レビュー対応)。
                    free_combat_units.sort_by_key(|(_, pos, _)| {
                        std::cmp::Reverse(
                            pos.x.abs_diff(unowned_pos.x) + pos.y.abs_diff(unowned_pos.y),
                        )
                    });
                    let mut assigned = 0;
                    // 護衛条件を満たさず不採用としたユニットは後で free pool に戻す
                    let mut rejected = Vec::new();
                    while assigned < 2 {
                        let Some((ent, pos, stats)) = free_combat_units.pop() else {
                            break;
                        };
                        let hp = world
                            .get::<crate::components::Health>(ent)
                            .map(|h| h.current)
                            .unwrap_or(100);
                        let (ammo1, ammo2) = world
                            .get::<crate::components::Ammo>(ent)
                            .map(|a| (a.ammo1, a.ammo2))
                            .unwrap_or((u32::MAX, u32::MAX));
                        if escort_is_eligible(&stats, inf_movement, hp, ammo1, ammo2) {
                            squad.members.insert(ent);
                            assigned += 1;
                        } else {
                            rejected.push((ent, pos, stats));
                        }
                    }
                    // 護衛に採用しなかった候補は他部隊が利用できるよう free pool へ戻す
                    free_combat_units.append(&mut rejected);
                    active_facility_captures += 1;
                }
            }
        }
    }

    // D. 攻撃部隊の立ち上げ
    for cluster in &enemy_clusters {
        if free_combat_units.is_empty() {
            break;
        }

        let exists = manager.squads.iter().any(|s| {
            squad_is_mutable_by_player(world, s, perspective_player)
                && s.mission_type == MissionType::Attack
                && s.target == Some(cluster.center)
        });

        if !exists {
            let squad = manager.create_owned_squad(MissionType::Attack, perspective_player);
            squad.target = Some(cluster.center);
            squad.phase = MissionPhase::Forming;

            // 最寄りの戦闘ユニットを最大2基割り当てる
            free_combat_units.sort_by_key(|(_, pos, stats)| {
                calculate_turn_distance(
                    &map,
                    &registry,
                    &unit_positions,
                    (pos.x, pos.y),
                    (cluster.center.x, cluster.center.y),
                    stats.movement_type,
                    stats.max_movement,
                    1,
                    perspective_player,
                    &mut turn_cache,
                )
            });

            let assign_count = free_combat_units.len().min(3);
            for _ in 0..assign_count {
                let (ent, _, _) = free_combat_units.remove(0);
                squad.members.insert(ent);
            }
        }
    }

    // ---------------------------------------------------------------
    // 定員管理付きの余剰ユニット割り当てロジック（交通渋滞解消）
    // 1部隊の最大人数。これを超えると後続ユニットが前線で渋滞を起こす
    const MAX_SQUAD_SIZE: usize = 3;

    while !free_combat_units.is_empty() {
        let (ent, pos, stats) = free_combat_units.pop().unwrap();

        // ステップ1: 定員未満の既存 Attack/Defense 部隊のうち、最も近いものを探す
        let mut best_squad_idx = None;
        let mut min_dist = crate::ai::turn_distance::TurnDistance {
            turns: u32::MAX,
            used_mp: u32::MAX,
        };

        for (idx, squad) in manager.squads.iter().enumerate() {
            if squad_is_mutable_by_player(world, squad, perspective_player)
                && (squad.mission_type == MissionType::Attack
                    || squad.mission_type == MissionType::Defense)
                && squad.members.len() < MAX_SQUAD_SIZE
            {
                if let Some(target) = squad.target {
                    let dist = calculate_turn_distance(
                        &map,
                        &registry,
                        &unit_positions,
                        (pos.x, pos.y),
                        (target.x, target.y),
                        stats.movement_type,
                        stats.max_movement,
                        1,
                        perspective_player,
                        &mut turn_cache,
                    );
                    if dist < min_dist {
                        min_dist = dist;
                        best_squad_idx = Some(idx);
                    }
                }
            }
        }

        if let Some(idx) = best_squad_idx {
            // 定員未満の部隊に吸収
            manager.squads[idx].members.insert(ent);
        } else {
            // ステップ2: 既存部隊がすべて定員に達している場合、
            // 最も近い敵クラスターを目標とする新規 Attack 部隊（第2波）を新設する
            let mut nearest_cluster_dist = crate::ai::turn_distance::TurnDistance {
                turns: u32::MAX,
                used_mp: u32::MAX,
            };
            let mut nearest_cluster_center = None;

            for cluster in &enemy_clusters {
                let dist = calculate_turn_distance(
                    &map,
                    &registry,
                    &unit_positions,
                    (pos.x, pos.y),
                    (cluster.center.x, cluster.center.y),
                    stats.movement_type,
                    stats.max_movement,
                    1,
                    perspective_player,
                    &mut turn_cache,
                );
                if dist < nearest_cluster_dist {
                    nearest_cluster_dist = dist;
                    nearest_cluster_center = Some(cluster.center);
                }
            }

            let final_target;
            // 敵が15ターン以上遠い場合や存在しない場合は、最寄りの拠点（未占領または敵所有）を目標にする
            if nearest_cluster_dist.turns <= 15 {
                final_target = nearest_cluster_center;
            } else {
                let mut nearest_prop_dist = u32::MAX;
                let mut nearest_prop_pos = None;
                for (p_pos, p_owner) in &properties_ownership {
                    if *p_owner != Some(perspective_player) {
                        // 処理時間を削減するためマンハッタン距離による概算を使用
                        let dist = (pos.x as i32 - p_pos.x as i32).unsigned_abs()
                            + (pos.y as i32 - p_pos.y as i32).unsigned_abs();
                        if dist < nearest_prop_dist {
                            nearest_prop_dist = dist;
                            nearest_prop_pos = Some(*p_pos);
                        }
                    }
                }
                final_target = nearest_prop_pos.or(nearest_cluster_center);
            }

            if let Some(target) = final_target {
                // 新規部隊を立ち上げて1体目のメンバーとして割り当てる
                let new_squad = manager.create_owned_squad(MissionType::Attack, perspective_player);
                new_squad.target = Some(target);
                new_squad.phase = MissionPhase::Forming;
                new_squad.members.insert(ent);
            }
            // 目標がまったく存在しない場合は放置（SoloFallback として機能）
        }
    }

    if is_v4 {
        crate::ai::v4::victory_roadmap::reconcile_campaign_roadmap(
            world,
            perspective_player,
            &strategy.campaign_portfolio,
            &manager,
        );
    }
    world.insert_resource(manager);
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
            .squads
            .iter()
            .filter(|squad| squad_is_mutable_by_player(world, squad, player_id))
            .filter(|squad| squad.mission_type == MissionType::Capture)
            .filter(|squad| squad.target_island == Some(local_island))
            .filter_map(|squad| squad.target)
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
            let squad = manager.create_owned_squad(MissionType::Capture, player_id);
            squad.members.insert(cargo);
            squad.target = Some(target);
            squad.target_island = Some(local_island);
            squad.phase = MissionPhase::MovingToTarget;
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
        let squad = manager.create_owned_squad(MissionType::Attack, player_id);
        squad.members.insert(cargo);
        squad.target = Some(target);
        squad.target_island = Some(local_island);
        squad.phase = MissionPhase::MovingToTarget;
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
    let empty_occupants = HashMap::new();
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
fn detach_deferred_pickup_cargo(world: &mut World, squad: &mut Squad) -> Option<Vec<Entity>> {
    if !squad.allow_partial_departure
        || squad.phase != MissionPhase::Transport(TransportPhase::Pickup)
    {
        return None;
    }
    let transport = squad.transport_entity?;
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

    squad.cargo_entities.retain(|cargo| loaded.contains(cargo));
    squad.phase = MissionPhase::Transport(TransportPhase::Transit);
    Some(deferred)
}

/// 輸送部隊のフェーズ更新と完了判定
pub fn update_transport_squad_phase(world: &mut World, squad: &mut Squad) -> bool {
    let Some(transport_entity) = squad.transport_entity else {
        return true;
    };
    if world.get::<GridPosition>(transport_entity).is_none() {
        return true;
    }
    let phase = match squad.phase {
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
    for cargo in &loaded {
        if !squad.cargo_entities.contains(cargo) {
            squad.cargo_entities.push(*cargo);
        }
    }

    let mut remaining = Vec::new();
    for cargo in squad.cargo_entities.drain(..) {
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
            squad.delivered_cargo.push(cargo);
        }
    }
    squad.cargo_entities = remaining;

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
            if all_loaded {
                squad.phase = MissionPhase::Transport(TransportPhase::Transit);
            }
        }
        TransportPhase::Transit => {
            if loaded.is_empty() {
                squad.phase = MissionPhase::Transport(TransportPhase::Return);
            } else if let Some(cargo) = loaded.first().copied()
                && !crate::systems::transport::get_droppable_tiles(world, transport_entity, cargo)
                    .is_empty()
            {
                squad.phase = MissionPhase::Transport(TransportPhase::Drop);
            }
        }
        TransportPhase::Drop => {
            if loaded.is_empty() {
                squad.phase = MissionPhase::Transport(TransportPhase::Return);
            }
        }
        TransportPhase::Return => {
            if !loaded.is_empty() {
                squad.phase = MissionPhase::Transport(TransportPhase::Drop);
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
    squad: &mut Squad,
    skip_entities: &std::collections::HashSet<Entity>,
) -> Option<(Entity, crate::ai::engine::AiCommand)> {
    let transport_entity = squad.transport_entity?;

    let (t_pos, t_stats, t_fuel, t_faction) = {
        let t_pos = world.get::<GridPosition>(transport_entity).cloned()?;
        let t_stats = world.get::<UnitStats>(transport_entity).cloned()?;
        let t_fuel = world
            .get::<crate::components::Fuel>(transport_entity)
            .map(|f| f.current)?;
        let t_faction = world.get::<Faction>(transport_entity).cloned()?;
        (t_pos, t_stats, t_fuel, t_faction.0)
    };

    let phase = match squad.phase {
        MissionPhase::Transport(p) => p,
        _ => return None,
    };
    let loaded_cargo = world
        .get::<crate::components::CargoCapacity>(transport_entity)
        .map(|capacity| capacity.loaded.clone())
        .unwrap_or_default();
    let must_vacate_production_site = phase == TransportPhase::Pickup
        && squad.pickup_position.is_some_and(|pickup| pickup != t_pos)
        && active_production_positions(world, t_faction).contains(&(t_pos.x, t_pos.y))
        && world
            .get::<crate::components::HasMoved>(transport_entity)
            .is_some_and(|moved| !moved.0)
        && world
            .get::<crate::components::ActionCompleted>(transport_entity)
            .is_some_and(|action| !action.0)
        && !skip_entities.contains(&transport_entity);

    // Pickupではヘリを先に動かすと、今いるヘリへ到達可能なcargoとの合流を
    // 自分から崩してしまうため即時Loadを優先する。ただし生産施設上だけは、先に
    // 指定済みの非生産Pickupへ退避する。同じ手番の後続呼び出しでcargo側が移動Load
    // できるため、搭載ターンを遅らせずに次ターンの生産枠を空けられる。
    if phase == TransportPhase::Pickup
        && !must_vacate_production_site
        && let Some(cargo) = squad.cargo_entities.iter().copied().find(|cargo| {
            !loaded_cargo.contains(cargo)
                && !skip_entities.contains(cargo)
                && cargo_can_board_transport_this_turn(world, *cargo, transport_entity)
        })
    {
        return Some((
            cargo,
            crate::ai::engine::AiCommand::Load {
                transport_entity,
                target_pos: t_pos,
            },
        ));
    }
    let cargo_entity = match phase {
        TransportPhase::Pickup => {
            let unloaded: Vec<_> = squad
                .cargo_entities
                .iter()
                .copied()
                .filter(|cargo| {
                    !loaded_cargo.contains(cargo)
                        && world
                            .get::<crate::components::Transporting>(*cargo)
                            .is_none()
                })
                .collect();
            // 先頭cargoが行動済みでも、後続cargoは同じ手番に合流点へ前進できる。
            // 全員行動済みの場合だけ先頭をfallbackにし、輸送役側の移動判定を継続する。
            unloaded
                .iter()
                .copied()
                .find(|cargo| {
                    !skip_entities.contains(cargo)
                        && world
                            .get::<crate::components::HasMoved>(*cargo)
                            .is_some_and(|moved| !moved.0)
                        && world
                            .get::<crate::components::ActionCompleted>(*cargo)
                            .is_some_and(|action| !action.0)
                })
                .or_else(|| unloaded.first().copied())
        }
        TransportPhase::Transit | TransportPhase::Drop => squad
            .cargo_entities
            .iter()
            .copied()
            .find(|cargo| loaded_cargo.contains(cargo)),
        TransportPhase::Return => None,
    };

    let mut unit_positions = HashMap::new();
    let mut query = world.query::<(
        Entity,
        &GridPosition,
        &Faction,
        &UnitStats,
        Option<&crate::components::CargoCapacity>,
        Option<&crate::components::Transporting>,
    )>();
    for (_e, pos, faction, stats, cargo_opt, transporting_opt) in query.iter(world) {
        if transporting_opt.is_some() {
            continue;
        }
        let free_slots = cargo_opt
            .map(|c| c.max.saturating_sub(c.loaded.len() as u32))
            .unwrap_or(0);
        unit_positions.insert(
            (pos.x, pos.y),
            crate::systems::movement::OccupantInfo {
                player_id: faction.0,
                is_transport: stats.max_cargo > 0,
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots,
            },
        );
    }

    let reachable = {
        let map = world.resource::<Map>();
        let registry = world.resource::<MasterDataRegistry>();
        calculate_reachable_tiles(
            map,
            &unit_positions,
            (t_pos.x, t_pos.y),
            t_stats.movement_type,
            t_stats.max_movement,
            t_fuel,
            t_faction,
            t_stats.unit_type,
            registry,
        )
    };

    match phase {
        TransportPhase::Pickup => {
            let cargo_entity = cargo_entity?;
            let cargo_pos = world.get::<GridPosition>(cargo_entity).cloned()?;
            let pickup_position = squad.pickup_position.unwrap_or(t_pos);
            let dist = (t_pos.x as i32 - cargo_pos.x as i32).abs()
                + (t_pos.y as i32 - cargo_pos.y as i32).abs();

            let transport_moved = world
                .get::<crate::components::HasMoved>(transport_entity)
                .map_or(true, |moved| moved.0);
            let transport_action_completed = world
                .get::<crate::components::ActionCompleted>(transport_entity)
                .map_or(true, |action| action.0);
            let cargo_action_completed = world
                .get::<crate::components::ActionCompleted>(cargo_entity)
                .map_or(true, |action| action.0);
            // 積載システムは同じ輸送役への複数cargo積載を許可している。輸送役は
            // 1体目の積載で行動済みになるが、同じマスの未行動cargoまで翌ターンへ
            // 送る必要はないため、積載可否はcargo自身の行動状態だけで判定する。
            if dist == 0 && !cargo_action_completed && !must_vacate_production_site {
                return Some((
                    cargo_entity,
                    crate::ai::engine::AiCommand::Load {
                        transport_entity,
                        target_pos: t_pos,
                    },
                ));
            }

            // 1. 輸送機がまだ行動していないなら、輸送機を歩兵へ近づける

            if !transport_moved
                && !transport_action_completed
                && !skip_entities.contains(&transport_entity)
            {
                let mut best_tile = t_pos;
                let mut min_score = None;

                let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
                let map = world.resource::<Map>();
                let registry = world.resource::<MasterDataRegistry>();

                for target_tile in &reachable {
                    let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                        map,
                        registry,
                        &unit_positions,
                        (target_tile.0, target_tile.1),
                        (pickup_position.x, pickup_position.y),
                        t_stats.movement_type,
                        t_stats.max_movement,
                        0, // 指定した合流地点を目指す
                        t_faction,
                        &mut cache,
                    );

                    let dx = target_tile.0 as i32 - pickup_position.x as i32;
                    let dy = target_tile.1 as i32 - pickup_position.y as i32;
                    let m_dist = dx.abs() + dy.abs();
                    let score = (t_dist, m_dist, target_tile.0, target_tile.1);

                    if min_score.map_or(true, |m| score < m) {
                        min_score = Some(score);
                        best_tile = GridPosition {
                            x: target_tile.0,
                            y: target_tile.1,
                        };
                    }
                }
                return Some((
                    transport_entity,
                    crate::ai::engine::AiCommand::Wait {
                        target_pos: best_tile,
                    },
                ));
            }

            // 2. 輸送機がすでに行動済みなら、歩兵を輸送機へ近づける
            let cargo_moved = world
                .get::<crate::components::HasMoved>(cargo_entity)
                .map_or(true, |moved| moved.0);

            if !cargo_moved && !cargo_action_completed && !skip_entities.contains(&cargo_entity) {
                let c_stats = world.get::<UnitStats>(cargo_entity).cloned()?;
                let c_fuel = world
                    .get::<crate::components::Fuel>(cargo_entity)
                    .map(|f| f.current)
                    .unwrap_or(99);

                let cargo_reachable = {
                    let map = world.resource::<Map>();
                    let registry = world.resource::<MasterDataRegistry>();
                    crate::systems::movement::calculate_reachable_tiles(
                        map,
                        &unit_positions,
                        (cargo_pos.x, cargo_pos.y),
                        c_stats.movement_type,
                        c_stats.max_movement,
                        c_fuel,
                        t_faction,
                        c_stats.unit_type,
                        registry,
                    )
                };

                // 合流地点までこの手番に到達できるなら、同じ座標でWaitして翌手番に
                // Loadし直さず、移動と積載を1つのコマンドとして確定する。
                if cargo_reachable.contains(&(t_pos.x, t_pos.y)) {
                    return Some((
                        cargo_entity,
                        crate::ai::engine::AiCommand::Load {
                            transport_entity,
                            target_pos: t_pos,
                        },
                    ));
                }

                let mut best_tile = cargo_pos;
                let mut min_score = None;

                let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
                let map = world.resource::<Map>();
                let registry = world.resource::<MasterDataRegistry>();
                let cargo_goal = if t_pos == pickup_position {
                    pickup_position
                } else {
                    map.get_adjacent(pickup_position.x, pickup_position.y)
                        .into_iter()
                        .filter(|position| {
                            map.get_terrain(position.0, position.1)
                                .and_then(|terrain| {
                                    crate::systems::movement::get_valid_movement_cost(
                                        registry,
                                        c_stats.movement_type,
                                        terrain,
                                    )
                                })
                                .is_some()
                                && is_terrain_reachable(
                                    map,
                                    registry,
                                    (cargo_pos.x, cargo_pos.y),
                                    *position,
                                    c_stats.movement_type,
                                )
                        })
                        .min_by_key(|position| {
                            (
                                map.distance(cargo_pos.x, cargo_pos.y, position.0, position.1),
                                position.1,
                                position.0,
                            )
                        })
                        .map(|position| GridPosition {
                            x: position.0,
                            y: position.1,
                        })
                        .unwrap_or(cargo_pos)
                };

                for target_tile in &cargo_reachable {
                    let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                        map,
                        registry,
                        &unit_positions,
                        (target_tile.0, target_tile.1),
                        (cargo_goal.x, cargo_goal.y),
                        c_stats.movement_type,
                        c_stats.max_movement,
                        0, // 輸送役到着前は合流点の隣接マスで待機する
                        t_faction,
                        &mut cache,
                    );

                    let dx = target_tile.0 as i32 - cargo_goal.x as i32;
                    let dy = target_tile.1 as i32 - cargo_goal.y as i32;
                    let m_dist = dx.abs() + dy.abs();
                    let score = (t_dist, m_dist, target_tile.0, target_tile.1);

                    if min_score.map_or(true, |m| score < m) {
                        min_score = Some(score);
                        best_tile = GridPosition {
                            x: target_tile.0,
                            y: target_tile.1,
                        };
                    }
                }

                return Some((
                    cargo_entity,
                    crate::ai::engine::AiCommand::Wait {
                        target_pos: best_tile,
                    },
                ));
            }
        }
        TransportPhase::Transit => {
            let cargo_entity = cargo_entity?;
            if skip_entities.contains(&transport_entity)
                || world
                    .get::<crate::components::ActionCompleted>(transport_entity)
                    .is_some_and(|action| action.0)
            {
                return None;
            }
            if let Some(target_island_id) = squad.target_island {
                let (island_tiles, target_pos) = {
                    if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>()
                    {
                        if let Some(island) =
                            island_map.islands.iter().find(|i| i.id == target_island_id)
                        {
                            let map = world.resource::<Map>();
                            let registry = world.resource::<MasterDataRegistry>();
                            let target_pos = get_target_position_for_island(
                                map,
                                registry,
                                island,
                                t_pos,
                                t_stats.movement_type,
                            );
                            (Some(island.tiles.clone()), target_pos)
                        } else {
                            (None, None)
                        }
                    } else {
                        (None, None)
                    }
                };

                if let (Some(_), Some(target_pos)) = (island_tiles, target_pos) {
                    if let Some((transport_target, drop_target)) = select_landing_candidate(
                        world,
                        transport_entity,
                        cargo_entity,
                        t_pos,
                        &reachable,
                        squad.target_island,
                        squad.target,
                    ) {
                        squad.drop_position = Some(drop_target);
                        squad.phase = MissionPhase::Transport(TransportPhase::Drop);
                        return Some((
                            transport_entity,
                            crate::ai::engine::AiCommand::Drop {
                                transport_target_pos: transport_target,
                                cargo_drop_pos: drop_target,
                                cargo_entity,
                            },
                        ));
                    }

                    let mut best_tile = t_pos;
                    let mut min_score = None;

                    let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
                    let map = world.resource::<Map>();
                    let registry = world.resource::<MasterDataRegistry>();

                    for target_tile in &reachable {
                        let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                            map,
                            registry,
                            &unit_positions,
                            (target_tile.0, target_tile.1),
                            (target_pos.x, target_pos.y),
                            t_stats.movement_type,
                            t_stats.max_movement,
                            1, // 目標に隣接するマスを目指す
                            t_faction,
                            &mut cache,
                        );

                        let dx = target_tile.0 as i32 - target_pos.x as i32;
                        let dy = target_tile.1 as i32 - target_pos.y as i32;
                        let m_dist = dx.abs() + dy.abs();
                        let score = (t_dist, m_dist, target_tile.0, target_tile.1);

                        if min_score.map_or(true, |m| score < m) {
                            min_score = Some(score);
                            best_tile = GridPosition {
                                x: target_tile.0,
                                y: target_tile.1,
                            };
                        }
                    }

                    return Some((
                        transport_entity,
                        crate::ai::engine::AiCommand::Wait {
                            target_pos: best_tile,
                        },
                    ));
                }
            }
        }
        TransportPhase::Drop => {
            let cargo_entity = cargo_entity?;
            if skip_entities.contains(&transport_entity)
                || world
                    .get::<crate::components::ActionCompleted>(transport_entity)
                    .is_some_and(|action| action.0)
            {
                return None;
            }
            if let Some((transport_target, drop_target)) = select_landing_candidate(
                world,
                transport_entity,
                cargo_entity,
                t_pos,
                &reachable,
                squad.target_island,
                squad.target,
            ) {
                squad.drop_position = Some(drop_target);
                return Some((
                    transport_entity,
                    crate::ai::engine::AiCommand::Drop {
                        transport_target_pos: transport_target,
                        cargo_drop_pos: drop_target,
                        cargo_entity,
                    },
                ));
            }

            if let Some(target_island_id) = squad.target_island {
                if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() {
                    if let Some(island) =
                        island_map.islands.iter().find(|i| i.id == target_island_id)
                    {
                        let map = world.resource::<Map>();
                        let registry = world.resource::<MasterDataRegistry>();
                        if let Some(target_pos) = get_target_position_for_island(
                            map,
                            registry,
                            island,
                            t_pos,
                            t_stats.movement_type,
                        ) {
                            let mut best_tile = t_pos;
                            let mut min_score = None;
                            let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
                            let map = world.resource::<Map>();
                            let registry = world.resource::<MasterDataRegistry>();

                            for target_tile in &reachable {
                                let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                                    map,
                                    registry,
                                    &unit_positions,
                                    (target_tile.0, target_tile.1),
                                    (target_pos.x, target_pos.y),
                                    t_stats.movement_type,
                                    t_stats.max_movement,
                                    1, // 目標に隣接するマスを目指す
                                    t_faction,
                                    &mut cache,
                                );
                                let m_dist = (target_tile.0 as i32 - target_pos.x as i32).abs()
                                    + (target_tile.1 as i32 - target_pos.y as i32).abs();
                                let score = (t_dist, m_dist, target_tile.0, target_tile.1);

                                if min_score.map_or(true, |m| score < m) {
                                    min_score = Some(score);
                                    best_tile = GridPosition {
                                        x: target_tile.0,
                                        y: target_tile.1,
                                    };
                                }
                            }
                            return Some((
                                transport_entity,
                                crate::ai::engine::AiCommand::Wait {
                                    target_pos: best_tile,
                                },
                            ));
                        }
                    }
                }
            }
            return Some((
                transport_entity,
                crate::ai::engine::AiCommand::Wait { target_pos: t_pos },
            ));
        }
        TransportPhase::Return => {
            let map = world.resource::<Map>().clone();
            let registry = world.resource::<MasterDataRegistry>().clone();
            let mut return_targets = Vec::new();
            let mut query = world.query::<(&GridPosition, &Property)>();
            for (position, property) in query.iter(world) {
                if property.owner_id != Some(t_faction)
                    || crate::systems::movement::get_valid_movement_cost(
                        &registry,
                        t_stats.movement_type,
                        property.terrain,
                    )
                    .is_none()
                    || t_stats.movement_type == crate::resources::MovementType::Ship
                        && property.terrain != Terrain::Port
                    || !is_terrain_reachable(
                        &map,
                        &registry,
                        (t_pos.x, t_pos.y),
                        (position.x, position.y),
                        t_stats.movement_type,
                    )
                {
                    continue;
                }
                return_targets.push((
                    t_pos.x.abs_diff(position.x) + t_pos.y.abs_diff(position.y),
                    position.y,
                    position.x,
                    *position,
                ));
            }
            return_targets.sort_by_key(|target| (target.0, target.1, target.2));
            let Some(nearest_prop_pos) = return_targets.first().map(|target| target.3) else {
                return Some((
                    transport_entity,
                    crate::ai::engine::AiCommand::Wait { target_pos: t_pos },
                ));
            };

            let mut best_tile = t_pos;
            let mut min_turn_dist = 999.0;
            let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
            let map = world.resource::<Map>();
            let registry = world.resource::<MasterDataRegistry>();

            for target_tile in &reachable {
                let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                    map,
                    registry,
                    &unit_positions,
                    (target_tile.0, target_tile.1),
                    (nearest_prop_pos.x, nearest_prop_pos.y),
                    t_stats.movement_type,
                    t_stats.max_movement,
                    1,
                    t_faction,
                    &mut cache,
                );
                let dx = target_tile.0 as i32 - nearest_prop_pos.x as i32;
                let dy = target_tile.1 as i32 - nearest_prop_pos.y as i32;
                let m_dist = dx.abs() + dy.abs();
                let e_dist_sq = dx * dx + dy * dy;
                let score = t_dist.turns as f32
                    + (m_dist as f32 / 1_000.0)
                    + (e_dist_sq as f32 / 10_000_000.0)
                    + (target_tile.0 as f32 / 100_000_000.0)
                    + (target_tile.1 as f32 / 1_000_000_000.0);

                if score < min_turn_dist {
                    min_turn_dist = score;
                    best_tile = GridPosition {
                        x: target_tile.0,
                        y: target_tile.1,
                    };
                }
            }
            return Some((
                transport_entity,
                crate::ai::engine::AiCommand::Wait {
                    target_pos: best_tile,
                },
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::*;
    use crate::resources::master_data::*;
    use crate::resources::*;

    fn setup_test_world() -> World {
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
            .filter(|s| s.mission_type == MissionType::Attack)
            .collect();
        assert_eq!(
            attack_squads.len(),
            1,
            "Should create exactly 1 attack squad"
        );
        assert!(attack_squads[0].members.contains(&u1));
        assert!(attack_squads[0].members.contains(&u2));
        assert_eq!(attack_squads[0].target, Some(GridPosition { x: 8, y: 8 }));
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
            .filter(|s| s.mission_type == MissionType::Capture)
            .collect();
        assert_eq!(capture_squads.len(), 1, "Should create 1 capture squad");
        assert!(capture_squads[0].members.contains(&inf));
        assert_eq!(capture_squads[0].target, Some(GridPosition { x: 5, y: 5 }));
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
                .find(|s| s.mission_type == MissionType::Capture)
                .and_then(|s| s.target)
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
            .filter(|s| s.mission_type == MissionType::Capture && !s.members.is_empty())
            .filter_map(|s| s.target)
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
            .find(|squad| squad.target_island == Some(primary.island_id))
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
                || squad.target_island == Some(secondary.island_id)
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
            drop_squad.phase,
            MissionPhase::Transport(TransportPhase::Drop)
        );
        assert_eq!(drop_squad.target_island, None);
        assert_eq!(drop_squad.target, None);
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
            assert_eq!(forming.phase, MissionPhase::Forming);
            (
                forming.id,
                forming.target_island.unwrap(),
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
                    squad.target_island == Some(target_island)
                        && squad.phase == MissionPhase::Forming
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
                .filter(|squad| squad.target_island == Some(target_island))
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
                assert_eq!(operation.target, Some(ready_assignment.target_position));
                assert_eq!(
                    operation.phase,
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
        let lander_squad = manager.create_owned_squad(MissionType::Transport, player);
        lander_squad.members.insert(lander);
        lander_squad.transport_entity = Some(lander);
        lander_squad.cargo_entities = vec![tank, artillery];
        lander_squad.target_island = Some(target_island);
        lander_squad.target = Some(target);
        let helicopter_squad = manager.create_owned_squad(MissionType::Transport, player);
        helicopter_squad.members.insert(helicopter);
        helicopter_squad.transport_entity = Some(helicopter);
        helicopter_squad.cargo_entities = vec![loaded_infantry, waiting_infantry];
        helicopter_squad.target_island = Some(target_island);
        helicopter_squad.target = Some(target);
        let duplicate = manager.create_owned_squad(MissionType::Transport, player);
        duplicate.target_island = Some(target_island);
        duplicate.target = Some(target);
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
                .filter(|squad| squad.target_island == Some(fixture.target_island))
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
                    squad.target_island == Some(fixture.target_island)
                        && squad.phase == MissionPhase::Transport(TransportPhase::Pickup)
                })
                .map(|squad| squad.id)
                .collect();
            for squad_id in pickup_ids {
                let mut manager = fixture.world.remove_resource::<SquadManager>().unwrap();
                let squad = manager
                    .squads
                    .iter_mut()
                    .find(|squad| squad.id == squad_id)
                    .unwrap();
                let (entity, command) =
                    execute_transport_squad_step(&mut fixture.world, squad, &HashSet::new())
                        .expect("Pickup squad must advance toward loading");
                assert!(matches!(
                    command,
                    crate::ai::engine::AiCommand::Load { .. }
                        | crate::ai::engine::AiCommand::Wait { .. }
                ));
                executed_steps.push((entity, format!("{command:?}")));
                fixture.world.insert_resource(manager);
                crate::ai::engine::execute_ai_command(&mut fixture.world, entity, command);
                fixture.load_schedule.run(&mut fixture.world);
            }
            update_squads(&mut fixture.world, player);
            let all_ready = fixture
                .world
                .resource::<SquadManager>()
                .squads
                .iter()
                .filter(|squad| squad.target_island == Some(fixture.target_island))
                .all(|squad| {
                    matches!(
                        squad.phase,
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
            .filter(|squad| squad.target_island == Some(fixture.target_island))
            .collect();
        assert_eq!(transport_squads.len(), 2);
        let final_states: Vec<_> = transport_squads
            .iter()
            .map(|squad| {
                (
                    squad.transport_entity,
                    squad.phase.clone(),
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
                    squad.pickup_position,
                )
            })
            .collect();
        assert!(
            transport_squads.iter().all(|squad| matches!(
                squad.phase,
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
            let lander_squad = manager
                .squads
                .iter_mut()
                .find(|squad| squad.transport_entity == Some(fixture.lander))
                .unwrap();
            lander_squad.phase = MissionPhase::Transport(TransportPhase::Drop);
            lander_squad.pickup_position = Some(GridPosition { x: 0, y: 1 });
            lander_squad.drop_position = Some(drop_position);
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
                .filter(|squad| squad.target_island == Some(fixture.target_island))
                .map(|squad| {
                    (
                        squad.id,
                        squad.transport_entity,
                        squad.members.clone(),
                        squad.cargo_entities.clone(),
                        squad.phase.clone(),
                        squad.pickup_position,
                        squad.drop_position,
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
                HashSet::from([fixture.cargo[2], fixture.cargo[3]])
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
            .filter(|squad| squad.target_island == Some(fixture.target_island))
            .map(|squad| {
                (
                    squad.id,
                    squad.transport_entity,
                    squad.members.clone(),
                    squad.cargo_entities.clone(),
                    squad.phase.clone(),
                    squad.pickup_position,
                    squad.drop_position,
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
            let lander_squad = manager
                .squads
                .iter_mut()
                .find(|squad| squad.transport_entity == Some(fixture.lander))
                .unwrap();
            lander_squad.cargo_entities = vec![loaded_cargo];
            lander_squad.phase = MissionPhase::Transport(TransportPhase::Drop);
            lander_squad.drop_position = Some(target);
            let drop_id = lander_squad.id;

            let local = manager.create_squad(MissionType::Capture);
            local.members.insert(landed_cargo);
            local.target_island = Some(fixture.target_island);
            local.target = Some(target);
            local.phase = MissionPhase::MovingToTarget;
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
            assert_eq!(drop.phase, MissionPhase::Transport(TransportPhase::Drop));
            assert_eq!(drop.cargo_entities, vec![loaded_cargo]);
            assert_eq!(drop.drop_position, Some(target));

            let local = manager
                .squads
                .iter()
                .find(|squad| squad.id == local_id)
                .expect("上陸済み戦闘Entityの通常Squadを維持する");
            assert_eq!(local.mission_type, MissionType::Capture);
            assert!(local.members.contains(&landed_cargo));
            assert_eq!(local.target_island, Some(fixture.target_island));

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
                squad.mission_type != MissionType::Transport
                    || !squad.cargo_entities.contains(&landed_cargo)
            }));

            manager
                .squads
                .iter()
                .map(|squad| {
                    (
                        squad.id,
                        squad.mission_type.clone(),
                        squad.members.clone(),
                        squad.transport_entity,
                        squad.cargo_entities.clone(),
                        squad.target_island,
                        squad.target,
                        squad.phase.clone(),
                        squad.pickup_position,
                        squad.drop_position,
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
                    squad.mission_type.clone(),
                    squad.members.clone(),
                    squad.transport_entity,
                    squad.cargo_entities.clone(),
                    squad.target_island,
                    squad.target,
                    squad.phase.clone(),
                    squad.pickup_position,
                    squad.drop_position,
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
        let forming = manager.create_squad(MissionType::Transport);
        forming.members.insert(fixture.lander);
        forming.transport_entity = Some(fixture.lander);
        forming.cargo_entities = vec![fixture.cargo[2]];
        forming.target_island = Some(target_island);
        forming.target = Some(target);
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
            .filter(|squad| squad.target_island == Some(target_island))
            .collect();
        assert!(!operation_squads.is_empty());
        assert!(
            operation_squads
                .iter()
                .any(|squad| squad.phase != MissionPhase::Forming)
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
            let extra = manager.create_squad(MissionType::Transport);
            extra.members.insert(extra_transport);
            extra.transport_entity = Some(extra_transport);
            extra.cargo_entities = vec![extra_cargo];
            extra.target_island = Some(fixture.target_island);
            extra.target = Some(assignment.target_position);
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
        assert_eq!(preserved.phase, MissionPhase::Forming);
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
                .filter(|squad| squad.target_island == Some(fixture.target_island))
                .map(|squad| {
                    (
                        squad.id,
                        squad.transport_entity,
                        squad.members.clone(),
                        squad.cargo_entities.clone(),
                        squad.phase.clone(),
                        squad.pickup_position,
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
            .filter(|squad| squad.target_island == Some(fixture.target_island))
            .map(|squad| {
                (
                    squad.id,
                    squad.transport_entity,
                    squad.members.clone(),
                    squad.cargo_entities.clone(),
                    squad.phase.clone(),
                    squad.pickup_position,
                )
            })
            .collect();
        repeated.sort_by_key(|squad| squad.0.0);
        assert_eq!(repeated, snapshot);
        assert!(manager.squads.iter().all(|squad| {
            squad.target_island != Some(fixture.target_island)
                || !squad.members.is_empty()
                || squad.mission_type != MissionType::Transport
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
        squad.transport_entity = Some(transport);
        squad.cargo_entities = vec![capture, combat];
        squad.target_island = Some(target_island);
        squad.phase = MissionPhase::Transport(TransportPhase::Pickup);

        assert!(!update_transport_squad_phase(&mut world, &mut squad));
        assert_eq!(squad.phase, MissionPhase::Transport(TransportPhase::Pickup));

        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .push(combat);
        world.entity_mut(combat).insert(Transporting(transport));
        *world.get_mut::<GridPosition>(combat).unwrap() = GridPosition { x: 9999, y: 9999 };
        assert!(!update_transport_squad_phase(&mut world, &mut squad));
        assert_eq!(
            squad.phase,
            MissionPhase::Transport(TransportPhase::Transit)
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
            let placeholder = manager.create_owned_squad(MissionType::Transport, player);
            placeholder.target_island = Some(target_island);
            placeholder.target = Some(GridPosition { x: 2, y: 0 });
            placeholder.phase = MissionPhase::Forming;
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
            placeholder.phase,
            MissionPhase::Transport(TransportPhase::Pickup)
        );
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
        squad.transport_entity = Some(transport);
        squad.cargo_entities = vec![first, second];
        squad.pickup_position = Some(GridPosition { x: 0, y: 0 });
        squad.phase = MissionPhase::Transport(TransportPhase::Pickup);

        let (entity, command) =
            execute_transport_squad_step(&mut world, &mut squad, &HashSet::new())
                .expect("同じマスの2体目を同ターンに積載する");
        assert_eq!(entity, second);
        assert!(matches!(
            command,
            crate::ai::engine::AiCommand::Load {
                transport_entity,
                ..
            } if transport_entity == transport
        ));
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
        squad.transport_entity = Some(transport);
        squad.cargo_entities = vec![cargo];
        squad.pickup_position = Some(GridPosition { x: 3, y: 0 });
        squad.phase = MissionPhase::Transport(TransportPhase::Pickup);

        let (entity, command) =
            execute_transport_squad_step(&mut world, &mut squad, &HashSet::new())
                .expect("cargo should board the current helicopter before it moves");
        assert_eq!(entity, cargo);
        assert!(matches!(
            command,
            crate::ai::engine::AiCommand::Load {
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
        squad.transport_entity = Some(transport);
        squad.cargo_entities = vec![cargo];
        squad.pickup_position = Some(pickup);
        squad.phase = MissionPhase::Transport(TransportPhase::Pickup);

        let (first_entity, first_command) =
            execute_transport_squad_step(&mut world, &mut squad, &HashSet::new())
                .expect("transport must vacate the airport before loading");
        assert_eq!(first_entity, transport);
        let crate::ai::engine::AiCommand::Wait { target_pos } = first_command else {
            panic!("airport relief must move the transport first");
        };
        assert_ne!(target_pos, GridPosition { x: 1, y: 0 });

        *world.get_mut::<GridPosition>(transport).unwrap() = target_pos;
        world.get_mut::<HasMoved>(transport).unwrap().0 = true;
        world.get_mut::<ActionCompleted>(transport).unwrap().0 = true;
        let (second_entity, second_command) =
            execute_transport_squad_step(&mut world, &mut squad, &HashSet::new())
                .expect("cargo must board the exhausted transport in the same turn");
        assert_eq!(second_entity, cargo);
        assert!(matches!(
            second_command,
            crate::ai::engine::AiCommand::Load {
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
        squad.transport_entity = Some(transport);
        squad.cargo_entities = vec![loaded, deferred];
        squad.allow_partial_departure = true;
        squad.phase = MissionPhase::Transport(TransportPhase::Pickup);

        let detached = detach_deferred_pickup_cargo(&mut world, &mut squad)
            .expect("distant second cargo should become a follow-up flight");
        assert_eq!(detached, vec![deferred]);
        assert_eq!(squad.cargo_entities, vec![loaded]);
        assert_eq!(
            squad.phase,
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
        squad.transport_entity = Some(transport);
        squad.cargo_entities = vec![completed, actionable];
        squad.pickup_position = Some(GridPosition { x: 0, y: 0 });
        squad.phase = MissionPhase::Transport(TransportPhase::Pickup);

        let (entity, command) =
            execute_transport_squad_step(&mut world, &mut squad, &HashSet::new())
                .expect("second cargo should load without a staging wait");
        assert_eq!(entity, actionable);
        assert!(matches!(
            command,
            crate::ai::engine::AiCommand::Load {
                transport_entity,
                target_pos: GridPosition { x: 0, y: 0 },
            } if transport_entity == transport
        ));
    }

    #[test]
    fn transport_drop_waits_for_all_cargo_and_hands_them_off() {
        let (mut world, transport, capture, combat, target_island) = setup_transport_phase_world();
        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.cargo_entities = vec![capture, combat];
        squad.target_island = Some(target_island);
        squad.target = Some(GridPosition { x: 2, y: 0 });
        squad.phase = MissionPhase::Transport(TransportPhase::Drop);
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
                .find(|squad| squad.mission_type == MissionType::Transport)
                .unwrap();
            assert_eq!(
                transport_squad.phase,
                MissionPhase::Transport(TransportPhase::Drop)
            );
            assert_eq!(transport_squad.cargo_entities, vec![combat]);
            assert!(manager.squads.iter().any(|squad| {
                squad.mission_type == MissionType::Capture && squad.members.contains(&capture)
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
            .find(|squad| squad.mission_type == MissionType::Transport)
            .unwrap();
        assert_eq!(
            transport_squad.phase,
            MissionPhase::Transport(TransportPhase::Return)
        );
        assert!(transport_squad.cargo_entities.is_empty());
        assert!(manager.squads.iter().any(|squad| {
            squad.mission_type == MissionType::Attack && squad.members.contains(&combat)
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
        let squad = manager.create_owned_squad(MissionType::Transport, player);
        squad.members.insert(helicopter);
        squad.transport_entity = Some(helicopter);
        squad.target_island = Some(target_island);
        squad.target = Some(enemy_position);
        squad.phase = MissionPhase::Transport(TransportPhase::Drop);
        world.insert_resource(manager);

        update_squads(&mut world, player);

        {
            let manager = world.resource::<SquadManager>();
            let escort = manager
                .squads
                .iter()
                .find(|squad| squad.members.contains(&helicopter))
                .expect("輸送ヘリを局地護衛へ引き渡す");
            assert_eq!(escort.mission_type, MissionType::Attack);
            assert_eq!(escort.target, Some(enemy_position));
            assert_eq!(escort.phase, MissionPhase::MovingToTarget);
            assert_eq!(escort.transport_entity, Some(helicopter));
            assert!(escort.return_after_combat);
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
        assert_eq!(returning.mission_type, MissionType::Transport);
        assert_eq!(
            returning.phase,
            MissionPhase::Transport(TransportPhase::Return)
        );
        assert_eq!(returning.transport_entity, Some(helicopter));
        assert!(!returning.return_after_combat);
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
        let safe_drop = manager.create_squad(MissionType::Transport);
        safe_drop.members.insert(transport);
        safe_drop.transport_entity = Some(transport);
        safe_drop.cargo_entities = vec![cargo];
        safe_drop.target_island = None;
        safe_drop.target = None;
        safe_drop.phase = MissionPhase::Transport(TransportPhase::Drop);
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
            squad.mission_type == MissionType::Transport || !squad.members.contains(&cargo)
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
        assert_eq!(capture.mission_type, MissionType::Capture);
        assert_eq!(capture.target_island, Some(landing_island));
        assert_eq!(capture.target, Some(local_target));
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
            .filter(|squad| squad.mission_type == MissionType::Capture)
            .filter_map(|squad| squad.target)
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
        assert_eq!(attack.mission_type, MissionType::Attack);
        assert_eq!(attack.target, Some(enemy_position));
        assert_eq!(attack.target_island, Some(landing_island));
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
                .and_then(|squad| squad.target_island),
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
                &HashMap::new(),
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
        let stale = manager.create_squad(MissionType::Capture);
        stale.members.insert(disconnected);
        stale.target_island = Some(island_id);
        stale.target = Some(neutral_city);
        stale.phase = MissionPhase::MovingToTarget;
        let duplicate = manager.create_squad(MissionType::Capture);
        duplicate.members.insert(stale_duplicate);
        duplicate.target_island = Some(island_id);
        duplicate.target = Some(neutral_city);
        duplicate.phase = MissionPhase::MovingToTarget;
        let legacy = manager.create_squad(MissionType::Capture);
        legacy.members.insert(legacy_member);
        legacy.target_island = None;
        legacy.target = Some(neutral_city);
        legacy.phase = MissionPhase::MovingToTarget;
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

        assert_eq!(protected, HashSet::from([reachable]));
        let capture = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&reachable))
            .expect("reachable Secure capture unit must own the local duty");
        assert_eq!(capture.target_island, Some(island_id));
        assert_eq!(capture.target, Some(neutral_city));
        assert_eq!(capture.members, BTreeSet::from([reachable]));
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|squad| {
                    squad.mission_type == MissionType::Capture
                        && squad.target_island == Some(island_id)
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
                    squad.mission_type.clone(),
                    squad.members.clone(),
                    squad.target_island,
                    squad.target,
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
                    squad.mission_type.clone(),
                    squad.members.clone(),
                    squad.target_island,
                    squad.target,
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
        let mixed = manager.create_squad(MissionType::Attack);
        mixed.members = BTreeSet::from([member_a, member_b]);
        mixed.target = Some(GridPosition { x: 3, y: 3 });
        mixed.target_island = Some(crate::ai::islands::IslandId(0));
        mixed.phase = MissionPhase::MovingToTarget;
        let snapshot = mixed.clone();
        world.insert_resource(manager);

        update_squads(&mut world, player_b);

        let retained = &world.resource::<SquadManager>().squads[0];
        assert_eq!(retained.id, snapshot.id);
        assert_eq!(retained.owner_id, snapshot.owner_id);
        assert_eq!(retained.mission_type, snapshot.mission_type);
        assert_eq!(retained.members, snapshot.members);
        assert_eq!(retained.target, snapshot.target);
        assert_eq!(retained.target_island, snapshot.target_island);
        assert_eq!(retained.phase, snapshot.phase);
        assert_eq!(retained.transport_entity, snapshot.transport_entity);
        assert_eq!(retained.cargo_entities, snapshot.cargo_entities);
        assert_eq!(retained.delivered_cargo, snapshot.delivered_cargo);
        assert_eq!(retained.pickup_position, snapshot.pickup_position);
        assert_eq!(retained.drop_position, snapshot.drop_position);
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
        let stale = manager.create_squad(MissionType::Transport);
        stale.members.insert(transport);
        stale.transport_entity = Some(transport);
        stale.target_island = Some(island_id);
        stale.target = Some(GridPosition { x: 3, y: 3 });
        stale.phase = MissionPhase::Forming;
        let stale_id = stale.id;
        let placeholder = manager.create_owned_squad(MissionType::Transport, player);
        placeholder.target_island = Some(island_id);
        placeholder.target = Some(GridPosition { x: 3, y: 3 });
        placeholder.phase = MissionPhase::Forming;
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
            squad.mission_type != MissionType::Defense
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
        assert_eq!(attack.mission_type, MissionType::Attack);
        assert_eq!(attack.target_island, Some(island_id));
        assert_eq!(attack.target, Some(enemy_position));
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
            squad.mission_type != MissionType::Attack || !squad.members.contains(&combat)
        }));
        let local_hold = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&combat))
            .expect("unreachable local combat must remain protected by a local responsibility");
        assert_eq!(local_hold.mission_type, MissionType::Defense);
        assert_eq!(local_hold.target_island, Some(island_id));
        assert_eq!(local_hold.target, Some(hold_position));
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
                    squad.mission_type.clone(),
                    squad.target_island,
                    squad.target,
                    squad.members.clone(),
                )
            })
            .collect();
        let attack = manager
            .squads
            .iter()
            .find(|squad| squad.mission_type == MissionType::Attack)
            .expect("reachable land member must receive an Attack duty");
        assert_eq!(attack.target, Some(enemy_position));
        assert_eq!(attack.members, BTreeSet::from([land_member]));
        let defense = manager
            .squads
            .iter()
            .find(|squad| squad.mission_type == MissionType::Defense)
            .expect("unreachable ship member must receive a local Defense hold");
        assert_eq!(defense.target, Some(ship_hold));
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
                    squad.mission_type.clone(),
                    squad.target_island,
                    squad.target,
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
            .filter(|squad| squad.mission_type == MissionType::Capture)
            .collect();
        assert_eq!(capture_squads.len(), 2);
        assert!(capture_squads.iter().all(|squad| squad.members.len() == 1));
        let mut targets: Vec<_> = capture_squads
            .iter()
            .filter_map(|squad| squad.target)
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
            .filter(|squad| squad.mission_type == MissionType::Attack)
            .collect();
        assert_eq!(attack_squads.len(), 1);
        assert_eq!(attack_squads[0].target, Some(enemy_position));
        assert_eq!(
            attack_squads[0].members,
            members.iter().copied().collect::<BTreeSet<_>>()
        );
        assert!(
            manager
                .squads
                .iter()
                .all(|squad| squad.mission_type != MissionType::Defense)
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
        assert_eq!(attack.mission_type, MissionType::Attack);
        assert_eq!(attack.target_island, Some(island_id));
        assert_eq!(attack.target, Some(enemy_position));
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
            .find(|squad| squad.target_island == Some(island_id))
            .expect("funded purchase-only operation must retain a Forming identity");
        assert_eq!(placeholder.phase, MissionPhase::Forming);
        assert_eq!(placeholder.target, Some(assignment.target_position));
        assert!(placeholder.members.is_empty());
        assert!(placeholder.transport_entity.is_none());
        let placeholder_id = placeholder.id;

        prepare_campaign_transport_assignment(&world, &mut manager, player, &assignment);
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|squad| squad.target_island == Some(island_id))
                .count(),
            1
        );
        assert_eq!(
            manager
                .squads
                .iter()
                .find(|squad| squad.target_island == Some(island_id))
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
            .find(|squad| squad.mission_type == MissionType::Transport)
            .expect("incomplete transport package should keep its forming squad");
        assert!(!transport_squad.cargo_entities.contains(&bomber));
        let attack = manager
            .squads
            .iter()
            .find(|squad| squad.mission_type == MissionType::Attack)
            .expect("self-deploying bomber should receive an attack mission");
        assert!(attack.members.contains(&bomber));
        assert_eq!(attack.target_island, Some(target_island));
        assert_eq!(attack.target, Some(target));
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
            .filter(|squad| squad.owner_id == Some(player_a))
            .collect();
        let player_b_placeholders: Vec<_> = manager
            .squads
            .iter()
            .filter(|squad| squad.owner_id == Some(player_b))
            .collect();
        assert_eq!(player_a_placeholders.len(), 1);
        assert_eq!(player_b_placeholders.len(), 1);
        assert_ne!(player_a_placeholders[0].id, player_b_placeholders[0].id);
        assert_eq!(player_a_placeholders[0].target_island, Some(island_id));
        assert_eq!(player_b_placeholders[0].target_island, Some(island_id));
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
        let defense = manager.create_squad(MissionType::Defense);
        defense.members.insert(stale);
        defense.target_island = Some(island_id);
        defense.target = Some(defended);
        defense.phase = MissionPhase::MovingToTarget;
        let duplicate = manager.create_squad(MissionType::Defense);
        duplicate.members.insert(stale_duplicate);
        duplicate.target_island = Some(island_id);
        duplicate.target = Some(defended);
        duplicate.phase = MissionPhase::MovingToTarget;

        prepare_campaign_local_assignment(&world, &mut manager, player, &assignment);

        let defense = manager
            .squads
            .iter()
            .find(|squad| squad.mission_type == MissionType::Defense)
            .unwrap();
        assert_eq!(defense.members, BTreeSet::from([reachable]));
        assert_eq!(defense.target_island, Some(island_id));
        assert_eq!(defense.target, Some(defended));
        assert_eq!(
            manager
                .squads
                .iter()
                .filter(|squad| {
                    squad.mission_type == MissionType::Defense
                        && squad.target_island == Some(island_id)
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
            assert_eq!(actual.mission_type, expected.mission_type);
            assert_eq!(actual.members, expected.members);
            assert_eq!(actual.target_island, expected.target_island);
            assert_eq!(actual.target, expected.target);
            assert_eq!(actual.phase, expected.phase);
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
