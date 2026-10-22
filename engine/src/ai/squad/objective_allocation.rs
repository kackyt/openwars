//! 施設を目的とする攻勢の戦闘割当（標準V4）。
//!
//! 任務の目的は占領・確保する施設であり、敵ユニットはその達成を妨げる場合だけ
//! 排除対象にする。各施設の占領完了までに妨害できる敵へ、「間に合う・有効打を持つ・
//! 反撃損失が少ない」部隊を、敵HPを削り切る分だけ1体ずつ排他的に割り当てる。
//! 交戦の見積りは自拠点防衛と共通の [`crate::ai::engagement`] を使う。
//! 余った戦力は、島内に残る未確保施設を守る敵の排除へ回し、それでも余った部隊を
//! 施設の前衛・護衛として前進させる。

use super::{
    CampaignResponsibility, MissionType, campaign_advance_responsibilities,
    campaign_combat_responsibilities,
};
use crate::ai::engagement::{Combatant, expected_exchange};
use crate::ai::islands::{IslandId, IslandMap};
use crate::ai::turn_distance::{
    ActionTurnDistanceCache, TerrainConnectivity, calculate_action_distance_to_range,
};
use crate::ai::v4::property_control::{capture_actions_needed, capture_power_from_hp};
use crate::collections::{HashMap, HashSet};
use crate::components::{
    Ammo, Faction, Fuel, GridPosition, Health, PlayerId, Property, Transporting, UnitStats,
};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Map, UnitType};
use crate::systems::movement::OccupantInfo;
use bevy_ecs::prelude::*;
use std::cmp::Reverse;

/// 一度に守る施設の数。前線から遠い施設の敵まで先回りして追わない。
const MAX_OBJECTIVES: usize = 4;

/// この手番数以内に施設を攻撃・占領できる敵を、その施設を守る敵とみなす。
const GUARD_TURNS: u32 = 2;

struct UnitView<'a> {
    entity: Entity,
    position: GridPosition,
    owner: PlayerId,
    stats: &'a UnitStats,
    hp: u32,
    fuel: u32,
    ammo: (u32, u32),
}

impl UnitView<'_> {
    fn combatant(&self) -> Combatant<'_> {
        Combatant {
            stats: self.stats,
            hp: self.hp,
            ammo: self.ammo,
        }
    }
}

fn unit_view(world: &World, entity: Entity) -> Option<UnitView<'_>> {
    if world.get::<Transporting>(entity).is_some() {
        return None;
    }
    let stats = world.get::<UnitStats>(entity)?;
    let hp = world.get::<Health>(entity)?.current;
    let position = *world.get::<GridPosition>(entity)?;
    let owner = world.get::<Faction>(entity)?.0;
    (hp > 0).then(|| UnitView {
        entity,
        position,
        owner,
        stats,
        hp,
        fuel: world
            .get::<Fuel>(entity)
            .map_or(stats.max_fuel, |fuel| fuel.current),
        ammo: world
            .get::<Ammo>(entity)
            .map_or((0, 0), |ammo| (ammo.ammo1, ammo.ammo2)),
    })
}

/// 占領を妨げる敵1体。`deadline` は自軍の占領完了予定手番（これより遅い排除は間に合わない）。
#[derive(Clone, Copy)]
struct Threat {
    enemy: Entity,
    eta: u32,
    deadline: u32,
}

/// 盤面の固定資料。
struct Board<'w> {
    map: &'w Map,
    data: &'w MasterDataRegistry,
    chart: &'w DamageChart,
    islands: &'w IslandMap,
}

#[allow(clippy::too_many_arguments)]
fn turns_to_range(
    board: &Board<'_>,
    occupied: &HashMap<(usize, usize), OccupantInfo>,
    unit: &UnitView<'_>,
    target: GridPosition,
    range: (u32, u32),
    cache: &mut ActionTurnDistanceCache,
) -> Option<crate::ai::turn_distance::ActionTurnDistance> {
    calculate_action_distance_to_range(
        board.map,
        board.data,
        occupied,
        (unit.position.x, unit.position.y),
        (target.x, target.y),
        unit.stats.movement_type,
        unit.stats.max_movement,
        unit.fuel,
        range.0,
        range.1,
        unit.owner,
        cache,
    )
}

fn base_damage(chart: &DamageChart, attacker: UnitType, defender: UnitType) -> u32 {
    chart.get_base_damage(attacker, defender).unwrap_or(0).max(
        chart
            .get_base_damage_secondary(attacker, defender)
            .unwrap_or(0),
    )
}

/// 施設ごとの自軍占領完了予定手番。占領兵がいなければ戦闘員の到着から見積もる。
fn capture_deadlines(
    world: &World,
    board: &Board<'_>,
    occupied: &HashMap<(usize, usize), OccupantInfo>,
    capturers: &[UnitView<'_>],
    fighters: &[UnitView<'_>],
    objectives: &[GridPosition],
    cache: &mut ActionTurnDistanceCache,
) -> Vec<u32> {
    objectives
        .iter()
        .map(|objective| {
            let capture_points = world
                .iter_entities()
                .find(|entity| entity.get::<GridPosition>() == Some(objective))
                .and_then(|entity| entity.get::<Property>().map(|p| p.capture_points))
                .unwrap_or(200);
            let by_capturer = capturers
                .iter()
                .filter_map(|unit| {
                    let arrival =
                        turns_to_range(board, occupied, unit, *objective, (0, 0), cache)?.turns;
                    Some(arrival.saturating_add(capture_actions_needed(
                        capture_points,
                        capture_power_from_hp(unit.hp),
                    )))
                })
                .min();
            by_capturer
                .or_else(|| {
                    fighters
                        .iter()
                        .filter_map(|unit| {
                            turns_to_range(board, occupied, unit, *objective, (0, 0), cache)
                        })
                        .map(|route| route.turns.saturating_add(2))
                        .min()
                })
                .unwrap_or(u32::MAX)
        })
        .collect()
}

/// 施設の占領を妨げる敵。施設上に居座る敵、占領完了までに占領兵を攻撃できる敵、
/// 施設を先に占領し得る敵だけを選び、遠くの敵は追わない。
#[allow(clippy::too_many_arguments)]
fn objective_threats(
    board: &Board<'_>,
    player: PlayerId,
    island: IslandId,
    enemies: &[&UnitView<'_>],
    capturer_type: UnitType,
    objectives: &[GridPosition],
    deadlines: &[u32],
    cache: &mut ActionTurnDistanceCache,
) -> Vec<Threat> {
    let empty = HashMap::default();
    let mut threats = enemies
        .iter()
        .filter(|enemy| {
            enemy.owner != player
                && board
                    .islands
                    .get_island_at(&enemy.position)
                    .is_some_and(|found| found.id == island)
        })
        .filter_map(|enemy| {
            let harms_capturer = base_damage(board.chart, enemy.stats.unit_type, capturer_type) > 0;
            objectives
                .iter()
                .zip(deadlines)
                .filter_map(|(objective, deadline)| {
                    if enemy.position == *objective {
                        // 居座る敵を排除しない限り占領を始められない。
                        return Some((0, u32::MAX));
                    }
                    let range = if harms_capturer {
                        (enemy.stats.min_range, enemy.stats.max_range)
                    } else if enemy.stats.can_capture {
                        (0, 0)
                    } else {
                        return None;
                    };
                    let eta = turns_to_range(board, &empty, enemy, *objective, range, cache)?.turns;
                    (eta <= *deadline).then_some((eta, *deadline))
                })
                .min()
                .map(|(eta, deadline)| Threat {
                    enemy: enemy.entity,
                    eta,
                    deadline,
                })
        })
        .collect::<Vec<_>>();
    threats.sort_unstable_by_key(|threat| (threat.eta, threat.enemy.to_bits()));
    threats
}

/// 1体の部隊が敵集団へ対処する見積り。集団内で最も早く有効打を出せる敵を基準にする。
struct Candidate {
    member: usize,
    ready: u32,
    damage: u32,
    loss: u32,
}

/// 割当の途中経過。部隊は1つの集団だけを受け持ち、集団に入った敵は再び数えない。
struct Allocation {
    assigned: Vec<bool>,
    handled: HashSet<Entity>,
    responsibilities: Vec<CampaignResponsibility>,
}

impl Allocation {
    /// `unit` が `group` のいずれかへ有効打を出す最良の見積り。
    #[allow(clippy::too_many_arguments)]
    fn candidate(
        board: &Board<'_>,
        occupied: &HashMap<(usize, usize), OccupantInfo>,
        member: usize,
        unit: &UnitView<'_>,
        group: &[&UnitView<'_>],
        deadline: u32,
        cache: &mut ActionTurnDistanceCache,
    ) -> Option<Candidate> {
        group
            .iter()
            .filter_map(|enemy| {
                let route = turns_to_range(
                    board,
                    occupied,
                    unit,
                    enemy.position,
                    (unit.stats.min_range, unit.stats.max_range),
                    cache,
                )?;
                // 間接攻撃は移動した手番に撃てないため、構え直しの1手番を加える。
                let setup = u32::from(unit.stats.min_range > 1 && route.requires_movement);
                let ready = route.turns.saturating_add(setup);
                if ready > deadline {
                    return None;
                }
                let exchange = expected_exchange(
                    board.map,
                    board.data,
                    board.chart,
                    unit.combatant(),
                    enemy.combatant(),
                    route.firing_position,
                    enemy.position,
                )?;
                Some(Candidate {
                    member,
                    ready,
                    damage: exchange.damage,
                    loss: exchange.loss,
                })
            })
            .min_by_key(|candidate| (candidate.ready, Reverse(candidate.damage), candidate.loss))
    }

    /// 目標の敵と、その周囲で1手番以内に反撃へ加われる敵をまとめた集団。
    /// 1体ずつの交換だけで必要数を決めると、周囲の敵からの反撃で小部隊を失う。
    fn support_group<'a, 'w>(
        &self,
        board: &Board<'_>,
        enemies: &'a [&'a UnitView<'w>],
        target: &'a UnitView<'w>,
        cache: &mut ActionTurnDistanceCache,
    ) -> Vec<&'a UnitView<'w>> {
        let empty = HashMap::default();
        let mut group = vec![target];
        group.extend(enemies.iter().copied().filter(|enemy| {
            enemy.entity != target.entity
                && !self.handled.contains(&enemy.entity)
                && turns_to_range(
                    board,
                    &empty,
                    enemy,
                    target.position,
                    // 目標に隣接する自軍を撃てる範囲。
                    (0, enemy.stats.max_range.saturating_add(1)),
                    cache,
                )
                .is_some_and(|route| route.turns <= 1)
        }));
        group
    }

    /// 対処できる部隊が少ない敵から順に、集団の合計HPを削り切るまで、到着の早い順・
    /// 有効打の大きい順・損失の少ない順に部隊を割り当てる。
    fn assign(
        &mut self,
        board: &Board<'_>,
        occupied: &HashMap<(usize, usize), OccupantInfo>,
        fighters: &[UnitView<'_>],
        enemies: &[&UnitView<'_>],
        threats: &[Threat],
        cache: &mut ActionTurnDistanceCache,
    ) {
        // 有効打を持つ部隊が少ない敵（対空しか当たらない航空機など）を先に処理し、
        // 汎用部隊が希少な部隊を先取りしないようにする。
        let mut ordered = threats
            .iter()
            .filter_map(|threat| {
                let enemy = enemies.iter().find(|unit| unit.entity == threat.enemy)?;
                let capable = fighters
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| !self.assigned[*index])
                    .filter(|(index, unit)| {
                        Self::candidate(
                            board,
                            occupied,
                            *index,
                            unit,
                            &[*enemy],
                            threat.deadline,
                            cache,
                        )
                        .is_some()
                    })
                    .count();
                Some((capable, *threat))
            })
            .collect::<Vec<_>>();
        ordered.sort_by_key(|(capable, threat)| (*capable, threat.eta, threat.enemy.to_bits()));

        for (_, threat) in ordered {
            if self.handled.contains(&threat.enemy) {
                continue;
            }
            let Some(target) = enemies.iter().find(|unit| unit.entity == threat.enemy) else {
                continue;
            };
            let group = self.support_group(board, enemies, target, cache);
            let mut candidates = fighters
                .iter()
                .enumerate()
                .filter(|(index, _)| !self.assigned[*index])
                .filter_map(|(index, unit)| {
                    Self::candidate(board, occupied, index, unit, &group, threat.deadline, cache)
                })
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                continue;
            }
            for enemy in &group {
                self.handled.insert(enemy.entity);
            }
            let mut remaining = group
                .iter()
                .map(|enemy| enemy.hp)
                .fold(0_u32, u32::saturating_add);
            let mut squad = Vec::new();
            while remaining > 0 {
                let Some(position) = candidates
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, candidate)| {
                        (
                            candidate.ready,
                            Reverse(candidate.damage.min(remaining)),
                            candidate.loss,
                            fighters[candidate.member].entity.to_bits(),
                        )
                    })
                    .map(|(position, _)| position)
                else {
                    break;
                };
                let chosen = candidates.swap_remove(position);
                self.assigned[chosen.member] = true;
                remaining = remaining.saturating_sub(chosen.damage);
                squad.push(fighters[chosen.member].entity);
            }
            self.responsibilities.push(CampaignResponsibility {
                mission_type: MissionType::Attack,
                target: target.position,
                members: squad,
            });
        }
    }
}

/// 余った戦闘役を首都ルートの前線へ配る。配った戦闘役は `remaining` から外す。
fn route_front_responsibilities(
    world: &World,
    map: &Map,
    player: PlayerId,
    island: IslandId,
    remaining: &mut Vec<Entity>,
) -> Vec<CampaignResponsibility> {
    let fronts =
        crate::ai::v4::capital_route::topology::same_land_route_fronts(world, player, island);
    // 進軍路が1本なら配分を選ぶ余地がない。前線一点へ集めると、拠点列を進む敵から
    // 占領役を守る戦力まで引き抜くため、従来どおり占領目標への前進に任せる
    // （拡張経路の戦闘配分も進軍路が2本以上のときだけ働く）。
    if fronts.len() < 2 || remaining.is_empty() {
        return Vec::new();
    }
    // 全進軍路の前線が敵首都の地域に入った終盤は、前線拠点の防衛ではなく首都への攻撃にする。
    if let Some(geometry) = crate::ai::v4::capital_route::node_operation::capital_assault_geometry(
        world, player, island,
    ) {
        return capital_assault_responsibilities(world, map, player, geometry, remaining);
    }
    let demands = crate::ai::v4::capital_route::force_demand::same_land_route_force_demands(
        world, player, island,
    );
    let slots = crate::ai::v4::capital_route::force_demand::apportion_route_force_slots(
        remaining.len(),
        &demands,
    );
    // 配る先は進軍路の固定識別点ではなく、DAGで求めた各進軍路の現在の前線にする。
    // DAGが無い（初期化前の）島だけ識別点を使う。
    let fronts = crate::ai::v4::capital_route::node_operation::capital_route_current_fronts(
        world, player, island,
    )
    .filter(|current| current.len() == fronts.len())
    .unwrap_or(fronts);
    let groups = super::combat_responsibilities::distribute_members_across_route_fronts(
        world, map, remaining, &fronts, &slots,
    );
    remaining.clear();
    fronts
        .into_iter()
        .zip(groups)
        .filter(|(_, members)| !members.is_empty())
        .map(|(front, members)| CampaignResponsibility {
            mission_type: MissionType::Defense,
            target: front,
            members,
        })
        .collect()
}

/// 敵首都の攻略で、余った戦闘役を首都への攻撃（Attack）へ回す。首都の地域にいる敵に
/// 有効打を持たない部隊（対空専用など）は、入口の射撃位置を塞がないよう手前で待たせる。
///
/// 防衛（Defense）のままでは有利な交換しか攻撃せず、敵首都の袋地で生産され続ける敵を
/// 削り切れない。Attackにすると、作戦上必要な攻撃として不利な交換も受け入れる。
fn capital_assault_responsibilities(
    world: &World,
    map: &Map,
    player: PlayerId,
    geometry: crate::ai::v4::capital_route::node_operation::CapitalAssaultGeometry,
    remaining: &mut Vec<Entity>,
) -> Vec<CampaignResponsibility> {
    let capital = geometry.capital;
    let radius = crate::ai::v4::capital_route::node_operation::capital_assault_region_radius();
    let enemy_types = world
        .iter_entities()
        .filter_map(|entity| unit_view(world, entity.id()))
        .filter(|unit| {
            unit.owner != player
                && map.distance(unit.position.x, unit.position.y, capital.x, capital.y) <= radius
        })
        .map(|unit| unit.stats.unit_type)
        .collect::<Vec<_>>();
    let chart = world.get_resource::<DamageChart>();
    // 首都の地域に敵がいなければ、全員で首都へ向かう（占領役の前進を援護する）。
    let can_hit_region = |entity: Entity| {
        enemy_types.is_empty()
            || world.get::<UnitStats>(entity).is_some_and(|stats| {
                chart.is_some_and(|chart| {
                    enemy_types
                        .iter()
                        .any(|enemy| base_damage(chart, stats.unit_type, *enemy) > 0)
                })
            })
    };
    let (assault, staged): (Vec<_>, Vec<_>) = remaining
        .drain(..)
        .partition(|entity| can_hit_region(*entity));
    [
        (MissionType::Attack, capital, assault),
        (MissionType::Defense, geometry.staging, staged),
    ]
    .into_iter()
    .filter(|(_, _, members)| !members.is_empty())
    .map(|(mission_type, target, members)| CampaignResponsibility {
        mission_type,
        target,
        members,
    })
    .collect()
}

/// 施設を目的とする攻勢の戦闘責務。`objectives` は優先順の未所有施設。
#[allow(clippy::too_many_arguments)]
pub(super) fn objective_combat_responsibilities(
    world: &World,
    player: PlayerId,
    island: IslandId,
    members: &[Entity],
    capture_members: &[Entity],
    objectives: &[GridPosition],
    connectivity: &mut TerrainConnectivity,
) -> Vec<CampaignResponsibility> {
    let objectives = &objectives[..objectives.len().min(MAX_OBJECTIVES)];
    let (Some(map), Some(data), Some(chart), Some(islands)) = (
        world.get_resource::<Map>(),
        world.get_resource::<MasterDataRegistry>(),
        world.get_resource::<DamageChart>(),
        world.get_resource::<IslandMap>(),
    ) else {
        return Vec::new();
    };
    if objectives.is_empty() {
        // 確保すべき施設が残っていない島だけ、島内の敵の掃討を任務にする。
        return campaign_combat_responsibilities(world, player, island, members, &[], connectivity);
    }
    let board = Board {
        map,
        data,
        chart,
        islands,
    };
    let mut ordered = members.to_vec();
    ordered.sort_unstable_by_key(|entity| entity.to_bits());
    ordered.dedup();
    let fighters = ordered
        .iter()
        .filter_map(|entity| unit_view(world, *entity))
        .collect::<Vec<_>>();
    let capturers = capture_members
        .iter()
        .filter_map(|entity| unit_view(world, *entity))
        .collect::<Vec<_>>();
    let all_units = world
        .iter_entities()
        .filter_map(|entity| entity.get::<Faction>().map(|_| entity.id()))
        .filter_map(|entity| unit_view(world, entity))
        .collect::<Vec<_>>();
    let occupied = all_units
        .iter()
        .map(|unit| {
            (
                (unit.position.x, unit.position.y),
                OccupantInfo {
                    player_id: unit.owner,
                    is_transport: false,
                    unit_type: unit.stats.unit_type,
                    loadable_types: Vec::new(),
                    free_slots: 0,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let mut cache = ActionTurnDistanceCache::default();
    let deadlines = capture_deadlines(
        world, &board, &occupied, &capturers, &fighters, objectives, &mut cache,
    );
    let capturer_type = capturers
        .first()
        .map_or(UnitType::Infantry, |unit| unit.stats.unit_type);
    let enemies = all_units
        .iter()
        .filter(|unit| unit.owner != player)
        .collect::<Vec<_>>();
    let threats = objective_threats(
        &board,
        player,
        island,
        &enemies,
        capturer_type,
        objectives,
        &deadlines,
        &mut cache,
    );

    let mut allocation = Allocation {
        assigned: vec![false; fighters.len()],
        handled: HashSet::default(),
        responsibilities: Vec::new(),
    };
    // 1. 目的施設の占領を今すぐ妨げる敵。
    allocation.assign(&board, &occupied, &fighters, &enemies, &threats, &mut cache);

    // 2. 余った戦力は、島内に残る未確保施設（敵の工場・首都を含む）を守る敵へ回す。
    //    施設から離れて動く敵は対象外にし、島中の敵を追い回さない。
    let remaining_properties = world
        .iter_entities()
        .filter_map(|entity| {
            let position = *entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.owner_id != Some(player)
                && board
                    .islands
                    .get_island_at(&position)
                    .is_some_and(|found| found.id == island))
            .then_some(position)
        })
        .collect::<Vec<_>>();
    let guard_deadlines = vec![GUARD_TURNS; remaining_properties.len()];
    let mut guards = objective_threats(
        &board,
        player,
        island,
        &enemies,
        capturer_type,
        &remaining_properties,
        &guard_deadlines,
        &mut cache,
    )
    .into_iter()
    .filter(|threat| !allocation.handled.contains(&threat.enemy))
    .map(|threat| Threat {
        // 施設を確保するまでに排除すればよく、到着期限は課さない。
        deadline: u32::MAX,
        ..threat
    })
    .collect::<Vec<_>>();
    // 前線に近い敵から順に手当てする。
    guards.sort_by_key(|threat| {
        let position = enemies
            .iter()
            .find(|unit| unit.entity == threat.enemy)
            .map_or(GridPosition { x: 0, y: 0 }, |unit| unit.position);
        let front = fighters
            .iter()
            .map(|unit| {
                board
                    .map
                    .distance(unit.position.x, unit.position.y, position.x, position.y)
            })
            .min()
            .unwrap_or(u32::MAX);
        (front, threat.enemy.to_bits())
    });
    allocation.assign(&board, &occupied, &fighters, &enemies, &guards, &mut cache);

    let Allocation {
        assigned,
        mut responsibilities,
        ..
    } = allocation;
    // 3. 排除に要らない部隊は施設の前衛・護衛として前進させる。施設へも届かない部隊だけ
    // 従来どおり届く敵の排除または現地防御に回す。
    let mut remaining = fighters
        .iter()
        .zip(&assigned)
        .filter(|(_, assigned)| !**assigned)
        .map(|(unit, _)| unit.entity)
        .collect::<Vec<_>>();
    // 進軍路が2本以上の島では、余った戦闘役を占領目標そのものへ集めず、首都ルートの
    // 各進軍路の現在の前線へ、ルートごとの必要戦力に応じて配る。進軍路が1本以下の島は従来どおり
    // 占領目標へ前進させる。
    responsibilities.extend(route_front_responsibilities(
        world,
        board.map,
        player,
        island,
        &mut remaining,
    ));
    responsibilities.extend(campaign_advance_responsibilities(
        world,
        &mut remaining,
        objectives,
        connectivity,
    ));
    responsibilities.extend(campaign_combat_responsibilities(
        world,
        player,
        island,
        &remaining,
        &[],
        connectivity,
    ));
    responsibilities
}

#[cfg(test)]
mod tests;
