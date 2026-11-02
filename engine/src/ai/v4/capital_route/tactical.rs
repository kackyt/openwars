//! 確定済みDAG指令を戦術位置へ投影する。

use crate::ai::v4::capital_route::commitment::capital_route_commitment_for_entity;
use crate::ai::v4::capital_route::model::{
    CapitalRouteCommitment, CapitalRouteExecutionPhase, CapitalRouteNodeObjective,
    CapitalRouteNodeOperationRegistry, CapitalRouteNodeScope,
};
use crate::ai::v4::capital_route::topology::capital_route_topology_for;
use crate::ai::v4::{
    BTreeSet, Entity, Faction, GridPosition, HashMap, Health, Map, MasterDataRegistry, PlayerId,
    Property, Terrain, UnitStats, World, get_valid_movement_cost, operation,
};

/// 占有を含まない地形距離だけを手番内で共有する。進入口の占有は毎回取り直す。
#[derive(bevy_ecs::prelude::Resource, Default)]
struct RegionTerrainDistances(
    std::sync::Mutex<(
        Option<u64>,
        crate::ai::turn_distance::SourceTurnDistanceCache,
    )>,
);

pub(crate) fn prepare_region_tactical_cache(world: &mut World) {
    use std::hash::{Hash, Hasher};
    let Some(map) = world.get_resource::<Map>() else {
        return;
    };
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    map.width.hash(&mut hash);
    map.height.hash(&mut hash);
    std::mem::discriminant(&map.topology).hash(&mut hash);
    for terrain in &map.tiles {
        std::mem::discriminant(terrain).hash(&mut hash);
    }
    world
        .get_resource::<crate::resources::MatchState>()
        .map(|state| state.current_turn_number.0)
        .hash(&mut hash);
    let generation = hash.finish();
    let cache = world.get_resource_or_insert_with(RegionTerrainDistances::default);
    let mut cache = cache.0.lock().unwrap_or_else(|poison| poison.into_inner());
    if cache.0 != Some(generation) {
        *cache = (Some(generation), Default::default());
    }
}

/// 未完了Capture Nodeの施設セルを、占領可能unitのために予約する。
///
/// Areaのanchorと実際の施設が離れている場合、Combat unitも同じ局地戦領域を自由に
/// 動くため、施設上で待機して歩兵を永久に遮断し得る。Node単位の予約にすることで、
/// 別枝から同じAreaへ入ったSquadにも共通の占領セル境界を適用する。
pub(in crate::ai::v4) fn capital_route_capture_target_is_reserved(
    operations: &CapitalRouteNodeOperationRegistry,
    player_id: PlayerId,
    position: GridPosition,
) -> bool {
    operations.operations.values().any(|operation| {
        operation.id.topology.player_id == player_id
            && operation.objective == CapitalRouteNodeObjective::Capture
            && operation.state != crate::ai::v4::victory_roadmap::RoadmapNodeState::Secured
            && operation.capture_targets.contains(&position)
    })
}

/// Entityが現在の地域Nodeへ入ったかを返す。
///
/// 橋Nodeは出口を越えるまで地域到達扱いにしない。橋上または手前にいるだけで
/// 戦術器の自由行動へ切り替えると、唯一の通路を越えずに作戦が止まるためである。
pub(in crate::ai::v4) fn capital_route_region_reached(
    commitment: &CapitalRouteCommitment,
    position: GridPosition,
) -> bool {
    let reached_scope = match commitment.scope {
        // Point Nodeはanchor、橋だけは向こう岸の出口そのものへ到達して初めて到達扱いにする。
        CapitalRouteNodeScope::Point => position == commitment.execution_target,
        CapitalRouteNodeScope::Area => commitment.control_area.contains(&position),
    };
    reached_scope && (commitment.crossing_exit.is_none() || commitment.crossed)
}

/// DAGがDefenseを前進Controlへ再指令した場合、戦術にも同じ攻勢役割を渡す。
/// 保持・待機中の防衛役を、作戦上必要な不利交換へ巻き込まない。
pub(crate) fn capital_route_tactical_mission(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    mission: &crate::ai::squad::MissionType,
) -> crate::ai::squad::MissionType {
    if *mission == crate::ai::squad::MissionType::Defense
        && capital_route_commitment_for_entity(world, player_id, entity)
            .is_some_and(|order| order.phase == CapitalRouteExecutionPhase::Advance)
    {
        crate::ai::squad::MissionType::Attack
    } else {
        mission.clone()
    }
}

/// Areaへ到達した後の移動候補を、Roadmapが指示した局地戦領域へ限定する。
///
/// `None` はDAG外、到達前、保持・補給、回復中のいずれかであり、呼び出し側の通常の
/// 候補集合を使う。`Some` の場合だけ、Control/Captureの戦術器が同じNode内で位置を
/// 選ぶ。これによりanchorへの一点直行を止めても、古いSquad目標へ逸脱しない。
pub(crate) fn capital_route_allows_tactical_position(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    position: GridPosition,
) -> Option<bool> {
    if capital_route_is_recovering(world, player_id, entity) {
        return None;
    }
    let commitment = capital_route_commitment_for_entity(world, player_id, entity)?;
    if commitment.phase != CapitalRouteExecutionPhase::Advance {
        return None;
    }
    let current = world.get::<GridPosition>(entity).copied()?;
    if !capital_route_region_reached(commitment, current) {
        return None;
    }
    // ここは地域境界だけを判定する。占領予定地の退避・敵生産口の占位は
    // 各戦術器の既存ルールで判定し、同じ施設を二重の制約で候補から落とさない。
    Some(commitment.control_area.contains(&position))
}

/// 橋へ進入する直接戦闘・占領役がいるなら、間接火力は通過枠の外から支える。
/// 間接火力しか残っていない場合は、自ら出口へ進めるよう物理占位の任務を保つ。
fn point_has_assault_unit(
    world: &World,
    player_id: PlayerId,
    order: &CapitalRouteCommitment,
) -> bool {
    world.iter_entities().any(|unit| {
        unit.get::<Faction>()
            .is_some_and(|owner| owner.0 == player_id)
            && unit.get::<crate::components::Transporting>().is_none()
            && unit.get::<UnitStats>().is_some_and(|stats| {
                stats.min_range <= 1 && (stats.max_range > 0 || stats.can_capture)
            })
            && !capital_route_is_recovering(world, player_id, unit.id())
            && capital_route_commitment_for_entity(world, player_id, unit.id()).is_some_and(
                |other| {
                    other.island_id == order.island_id
                        && other.target_anchor == order.target_anchor
                        && other.phase == CapitalRouteExecutionPhase::Advance
                },
            )
    })
}

/// 地域への進入先・占領先は実際に踏む位置であり、射程内から支える参照点ではない。
/// 敵が占有している場合は先に排除するため、通常の攻撃射程を用いる。
pub(crate) fn capital_route_target_requires_occupancy(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    target: GridPosition,
) -> bool {
    capital_route_commitment_for_entity(world, player_id, entity).is_some_and(|order| {
        order.phase == CapitalRouteExecutionPhase::Advance
            && !capital_route_is_recovering(world, player_id, entity)
            && !(order.scope == CapitalRouteNodeScope::Point
                && world
                    .get::<UnitStats>(entity)
                    .is_some_and(|stats| stats.min_range > 1)
                && point_has_assault_unit(world, player_id, order))
            && (order.scope == CapitalRouteNodeScope::Point
                || world
                    .get::<GridPosition>(entity)
                    .is_some_and(|at| !capital_route_region_reached(order, *at))
                || world
                    .get::<UnitStats>(entity)
                    .is_some_and(|stats| stats.can_capture))
    }) && !world.iter_entities().any(|unit| {
        unit.get::<GridPosition>() == Some(&target)
            && unit
                .get::<Faction>()
                .is_some_and(|owner| owner.0 != player_id)
            && unit.get::<UnitStats>().is_some()
            && unit.get::<crate::components::Transporting>().is_none()
    })
}

/// 地域の外側にいる火力も、入口ではなく排除すべき地域内の敵へ射程を合わせる。
fn regional_enemy_target(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    order: &CapitalRouteCommitment,
) -> Option<GridPosition> {
    let stats = world.get::<UnitStats>(entity)?;
    let position = world.get::<GridPosition>(entity)?;
    let map = world.get_resource::<Map>()?;
    let chart = world.get_resource::<crate::resources::DamageChart>()?;
    let ammo = world.get::<crate::components::Ammo>(entity);
    world
        .iter_entities()
        .filter_map(|enemy| {
            let at = *enemy.get::<GridPosition>()?;
            let enemy_stats = enemy.get::<UnitStats>()?;
            if enemy.get::<Faction>()?.0 == player_id
                || enemy.get::<crate::components::Transporting>().is_some()
                || !order.control_area.contains(&at)
            {
                return None;
            }
            let effective = (ammo.is_none_or(|ammo| stats.max_ammo1 == 0 || ammo.ammo1 > 0)
                && chart
                    .get_base_damage(stats.unit_type, enemy_stats.unit_type)
                    .unwrap_or(0)
                    > 0)
                || (ammo.is_none_or(|ammo| stats.max_ammo2 == 0 || ammo.ammo2 > 0)
                    && chart
                        .get_base_damage_secondary(stats.unit_type, enemy_stats.unit_type)
                        .unwrap_or(0)
                        > 0);
            effective.then_some(at)
        })
        .min_by_key(|at| {
            (
                map.distance(position.x, position.y, at.x, at.y)
                    .saturating_sub(stats.max_range),
                at.y,
                at.x,
            )
        })
}

/// 非占領ユニットが自軍の占領予定物件を踏んで占領を塞ぐのを防ぐため、
/// 当該位置が自軍の未完了Capture目標として予約されているかを判定する。
pub(crate) fn is_capture_target_reserved(
    world: &World,
    player_id: PlayerId,
    position: GridPosition,
) -> bool {
    // 既に自軍が所有している物件は占領完了済みなので予約対象外
    let is_unowned = world.iter_entities().any(|e| {
        e.get::<GridPosition>() == Some(&position)
            && e.get::<Property>()
                .is_some_and(|p| p.owner_id != Some(player_id))
    });
    if !is_unowned {
        return false;
    }
    let reserved_in_registry = world
        .get_resource::<CapitalRouteNodeOperationRegistry>()
        .is_some_and(|operations| {
            capital_route_capture_target_is_reserved(operations, player_id, position)
        });
    if reserved_in_registry {
        return true;
    }
    world
        .get_resource::<crate::ai::squad::SquadManager>()
        .is_some_and(|manager| {
            manager.squads.iter().any(|squad| {
                squad.plan.owner_id == Some(player_id)
                    && squad.plan.mission_type == crate::ai::squad::MissionType::Capture
                    && squad.plan.target == Some(position)
            })
        })
}

/// 地域への進入先は代表点に固定しない。地形経路で近い空きセルを選び、
/// 先に進入した味方のマスと占領予約を後続の目的地から外す。
fn region_entry_target(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    commitment: &CapitalRouteCommitment,
) -> Option<GridPosition> {
    let position = *world.get::<GridPosition>(entity)?;
    let stats = world.get::<UnitStats>(entity)?;
    let map = world.get_resource::<Map>()?;
    let master = world.get_resource::<MasterDataRegistry>()?;
    let occupancy = world
        .iter_entities()
        .filter_map(|unit| {
            if unit.get::<crate::components::Transporting>().is_some() {
                return None;
            }
            let at = unit.get::<GridPosition>()?;
            let faction = unit.get::<Faction>()?;
            let stats = unit.get::<UnitStats>()?;
            Some((
                (at.x, at.y),
                crate::systems::movement::OccupantInfo {
                    player_id: faction.0,
                    is_transport: false,
                    unit_type: stats.unit_type,
                    loadable_types: Vec::new(),
                    free_slots: 0,
                },
            ))
        })
        .collect::<HashMap<_, _>>();
    // 敵の占有は攻撃で解消する前提の目標選択。合法な実移動は行動候補側で判定する。
    let distances = if let Some(cache) = world.get_resource::<RegionTerrainDistances>() {
        let mut cache = cache.0.lock().unwrap_or_else(|poison| poison.into_inner());
        crate::ai::turn_distance::calculate_source_turn_distances_cached(
            map,
            master,
            &HashMap::default(),
            (position.x, position.y),
            stats.movement_type,
            stats.max_movement,
            player_id,
            &mut cache.1,
        )
    } else {
        crate::ai::turn_distance::calculate_source_turn_distances_cached(
            map,
            master,
            &HashMap::default(),
            (position.x, position.y),
            stats.movement_type,
            stats.max_movement,
            player_id,
            &mut Default::default(),
        )
    };
    commitment
        .control_area
        .iter()
        .copied()
        .filter(|cell| !occupancy.contains_key(&(cell.x, cell.y)))
        .filter_map(|cell| distances.get(&cell).map(|distance| (cell, *distance)))
        .min_by_key(|(cell, distance)| (*distance, cell.y, cell.x))
        .map(|(cell, _)| cell)
        .or_else(|| {
            // 全進入口を敵が占めていても、排除する敵位置を目標として保つ。
            // 味方で埋まった地域は待機し、実際の占領予約・占位は戦術側が判断する。
            commitment
                .control_area
                .iter()
                .copied()
                .filter(|cell| {
                    occupancy
                        .get(&(cell.x, cell.y))
                        .is_some_and(|unit| unit.player_id != player_id)
                })
                .filter_map(|cell| distances.get(&cell).map(|distance| (cell, *distance)))
                .min_by_key(|(cell, distance)| (*distance, cell.y, cell.x))
                .map(|(cell, _)| cell)
        })
}

/// 地域内の占領担当を未所有施設へ一対一で割り当てる。
/// 既に対象上にいる担当を距離0で優先し、余剰部隊を同じ施設へ重ねない。
fn regional_capture_target(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    commitment: &CapitalRouteCommitment,
    targets: &[GridPosition],
) -> Option<GridPosition> {
    let map = world.get_resource::<Map>()?;
    let mut pairs = world
        .iter_entities()
        .filter_map(|unit| {
            let at = *unit.get::<GridPosition>()?;
            let stats = unit.get::<UnitStats>()?;
            let faction = unit.get::<Faction>()?;
            let order = capital_route_commitment_for_entity(world, player_id, unit.id())?;
            let assignment = world
                .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()?
                .assignment(unit.id())?;
            let squad = world
                .get_resource::<crate::ai::squad::SquadManager>()?
                .squads
                .iter()
                .find(|squad| Some(squad.id) == assignment.squad_id)?;
            if !crate::ai::tactics::campaign::campaign_mission_allows_capture(
                &capital_route_tactical_mission(
                    world,
                    player_id,
                    unit.id(),
                    &squad.plan.mission_type,
                ),
            ) {
                return None;
            }
            (faction.0 == player_id
                && stats.can_capture
                && unit.get::<crate::components::Transporting>().is_none()
                && !capital_route_is_recovering(world, player_id, unit.id())
                && order.island_id == commitment.island_id
                && order.target_anchor == commitment.target_anchor
                && order.phase == CapitalRouteExecutionPhase::Advance
                && commitment.control_area.contains(&at))
            .then_some((unit.id(), at))
        })
        .flat_map(|(unit, at)| {
            targets.iter().map(move |target| {
                (
                    map.distance(at.x, at.y, target.x, target.y),
                    unit.to_bits(),
                    target.y,
                    target.x,
                    unit,
                    *target,
                )
            })
        })
        .collect::<Vec<_>>();
    pairs.sort_unstable_by_key(|pair| (pair.0, pair.1, pair.2, pair.3));
    let mut assigned_units = crate::collections::HashSet::default();
    let mut assigned_targets = crate::collections::HashSet::default();
    for (_, _, _, _, unit, target) in pairs {
        if assigned_units.contains(&unit) || assigned_targets.contains(&target) {
            continue;
        }
        assigned_units.insert(unit);
        assigned_targets.insert(target);
        if unit == entity {
            return Some(target);
        }
    }
    None
}

/// 行動決定側へ、現在の地域作戦の戦術的な目標を返す。
///
/// 外側では回廊の入口／橋向こうの出口へ進める。地域内のControl Nodeはanchorへの
/// 引力を外して局地戦術へ委ね、Capture Nodeだけが未確保の周辺拠点を選ぶ。
/// `Some(None)` は「DAG所属だが一点目標を持たない地域Control中」を意味する。
pub(crate) fn capital_route_tactical_target(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
) -> Option<Option<GridPosition>> {
    if capital_route_is_recovering(world, player_id, entity) {
        return Some(None);
    }
    let commitment = capital_route_commitment_for_entity(world, player_id, entity)?;
    // 保持・補給は局地戦の自由移動へ切り替えず、それぞれの防衛地点・配送地点を
    // 維持する。Point/Areaの分類は前進Squadの到達後の戦術だけに適用する。
    if commitment.phase != CapitalRouteExecutionPhase::Advance {
        return Some(Some(commitment.execution_target));
    }
    let position = world.get::<GridPosition>(entity).copied()?;
    let can_capture = world
        .get::<UnitStats>(entity)
        .is_some_and(|stats| stats.can_capture);
    // 小規模mapの序盤は、Capture Nodeの集結点へ着いてから物件を選ぶと、実際には
    // 到達済みの複数レーンを一手ずつ失う。
    // 占領役は未所有物件群との1対1排他マッチングを行い、各歩兵を別々の物件へ分散進軍させる。
    // 戦闘役（Recon等）は自陣物件群が未確保の間、敵首都に最も近い最前線物件へ先行展開する。
    if crate::ai::strategy_profile::current_profile(world, player_id)
        == crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion
        && commitment.objective == CapitalRouteNodeObjective::Capture
    {
        let map = world.get_resource::<Map>()?;
        let properties = world
            .iter_entities()
            .filter_map(|p| {
                Some((
                    *p.get::<GridPosition>()?,
                    p.get::<Property>()?.owner_id,
                    p.get::<Property>()?.terrain,
                ))
            })
            .collect::<Vec<_>>();
        let owners = properties
            .iter()
            .map(|(pos, owner, _)| (*pos, *owner))
            .collect::<HashMap<_, _>>();
        let unowned_targets = commitment
            .capture_targets
            .iter()
            .filter(|target| owners.get(target) != Some(&Some(player_id)))
            .copied()
            .collect::<Vec<_>>();

        if !unowned_targets.is_empty() {
            let my_capital = properties
                .iter()
                .find_map(|(pos, owner, terrain)| {
                    (*owner == Some(player_id) && *terrain == Terrain::Capital).then_some(*pos)
                })
                .unwrap_or(commitment.execution_target);
            let enemy_capital = properties.iter().find_map(|(pos, owner, terrain)| {
                (*owner != Some(player_id) && owner.is_some() && *terrain == Terrain::Capital)
                    .then_some(*pos)
            });

            if can_capture {
                // 自軍の占領歩兵を全員収集
                let mut capturers = world
                    .iter_entities()
                    .filter_map(|e| {
                        let f = e.get::<Faction>()?.0;
                        let pos = *e.get::<GridPosition>()?;
                        let stats = e.get::<UnitStats>()?;
                        let hp = e.get::<Health>()?;
                        (f == player_id && stats.can_capture && hp.current > 0)
                            .then_some((e.id(), pos))
                    })
                    .collect::<Vec<_>>();
                capturers.sort_unstable_by_key(|(e, pos)| (pos.x, pos.y, e.to_bits()));

                // 奥（自軍首都から遠く、敵首都に近い順）からソート
                let mut sorted_targets = unowned_targets.clone();
                sorted_targets.sort_unstable_by_key(|t| {
                    let d_my = map.distance(my_capital.x, my_capital.y, t.x, t.y);
                    let d_enemy = enemy_capital
                        .map(|ec| map.distance(ec.x, ec.y, t.x, t.y))
                        .unwrap_or(0);
                    (std::cmp::Reverse(d_my), d_enemy, t.y, t.x)
                });

                // 貪欲マッチング: 奥のtargetから順に、最も近い未割当の歩兵を割り当てる
                let mut target_to_unit: HashMap<GridPosition, Entity> = HashMap::default();
                let mut unit_to_target: HashMap<Entity, GridPosition> = HashMap::default();
                let mut available_capturers = capturers.clone();

                for t in &sorted_targets {
                    if available_capturers.is_empty() {
                        break;
                    }
                    if let Some(best_idx) = available_capturers
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, (e, pos))| {
                            (map.distance(pos.x, pos.y, t.x, t.y), e.to_bits())
                        })
                        .map(|(idx, _)| idx)
                    {
                        let (best_e, _best_pos) = available_capturers.remove(best_idx);
                        target_to_unit.insert(*t, best_e);
                        unit_to_target.insert(best_e, *t);
                    }
                }

                if let Some(&assigned_target) = unit_to_target.get(&entity) {
                    return Some(Some(assigned_target));
                }

                // 割り当てから漏れた歩兵は、自分から最も近い未所有物件をフォールバックとして目指す
                if let Some(fallback_target) = unowned_targets
                    .iter()
                    .min_by_key(|t| (map.distance(position.x, position.y, t.x, t.y), t.y, t.x))
                    .copied()
                {
                    return Some(Some(fallback_target));
                }
            } else {
                // 戦闘ユニット（Recon等）:
                // 小規模マップでは1ターンで移動＋直接攻撃が成立するため、最寄りの敵を
                // 直接目標にできる。敵が射程圏内にいる間は敵を優先して前進し、
                // 敵がいなければ最前線物件（Frontier）へスクリーニング展開する。
                // 撤退する敵を延々追いかけて拠点を失うのを防ぐため、射程圏外の敵は
                // 追わず、従来どおり物件の確保を優先する。
                let is_direct_fire = world
                    .get::<UnitStats>(entity)
                    .is_some_and(|s| s.min_range <= 1);
                if is_direct_fire {
                    let reach = world
                        .get::<UnitStats>(entity)
                        .map(|s| s.max_movement + s.max_range)
                        .unwrap_or(0);
                    let nearest_enemy = world
                        .iter_entities()
                        .filter_map(|e| {
                            let faction = e.get::<Faction>()?.0;
                            let epos = *e.get::<GridPosition>()?;
                            let hp = e.get::<Health>()?.current;
                            (faction != player_id
                                && hp > 0
                                && map.distance(position.x, position.y, epos.x, epos.y) <= reach)
                                .then_some(epos)
                        })
                        .min_by_key(|epos| map.distance(position.x, position.y, epos.x, epos.y));
                    if let Some(enemy) = nearest_enemy {
                        return Some(Some(enemy));
                    }
                }
                // 自陣物件が未占領の間、最も敵陣（敵首都）に近い最前線物件（Frontier）へ先行してスクリーニングする
                let frontier_target = unowned_targets
                    .iter()
                    .min_by_key(|t| {
                        let d_enemy = enemy_capital
                            .map(|ec| map.distance(ec.x, ec.y, t.x, t.y))
                            .unwrap_or(0);
                        let d_my = map.distance(my_capital.x, my_capital.y, t.x, t.y);
                        (d_enemy, std::cmp::Reverse(d_my), t.y, t.x)
                    })
                    .copied();

                if let Some(target) = frontier_target {
                    return Some(Some(target));
                }
            }
        }
    }
    // 地域外の直接戦闘役は、敵が残っていても空いた入口へ進む。
    // 敵座標を移動目標にすると武器射程で接近完了となり、制圧のための実進入が消える。
    // 射撃対象の優先順位は別窓口で渡し、間接火力だけは地域外から射程を合わせる。
    let reached = capital_route_region_reached(commitment, position);
    let indirect = world
        .get::<UnitStats>(entity)
        .is_some_and(|stats| stats.min_range > 1);
    if !can_capture
        && (reached || indirect)
        && let Some(enemy) = regional_enemy_target(world, player_id, entity, commitment)
    {
        return Some(Some(enemy));
    }
    if !capital_route_region_reached(commitment, position) {
        return Some(if commitment.scope == CapitalRouteNodeScope::Area {
            region_entry_target(world, player_id, entity, commitment)
        } else {
            Some(commitment.execution_target)
        });
    }
    if commitment.objective != CapitalRouteNodeObjective::Capture {
        return Some(None);
    }
    let owners = world
        .iter_entities()
        .filter_map(|property| {
            Some((
                *property.get::<GridPosition>()?,
                property.get::<Property>()?.owner_id,
            ))
        })
        .collect::<HashMap<_, _>>();
    if can_capture {
        let targets = commitment
            .capture_targets
            .iter()
            .copied()
            .filter(|target| owners.get(target) != Some(&Some(player_id)))
            .collect::<Vec<_>>();
        return Some(
            regional_capture_target(world, player_id, entity, commitment, &targets)
                .or_else(|| regional_enemy_target(world, player_id, entity, commitment)),
        );
    }
    Some(None)
}

/// 既存の戦術器向けに、地点を持つ場合だけを返す互換窓口。
#[cfg(test)]
pub(crate) fn capital_route_waypoint(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
) -> Option<GridPosition> {
    capital_route_tactical_target(world, player_id, entity).flatten()
}

/// DAG所属を保ったまま、回復だけは一時的に通常の修理経路へ委ねるかを返す。
///
/// Recover Entity に古いSquad目標を与えると、修理拠点ではなく横の占領目標へ戻って
/// しまう。経路そのものはRegistryに残し、行動選択だけを回復優先へ切り替える。
pub(crate) fn capital_route_is_recovering(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
) -> bool {
    // HP低下はmember個別の戦術例外であり、SquadのRoadmap orderは更新しない。
    // 回復が終われば同じSquad orderへ自然に復帰する。
    capital_route_commitment_for_entity(world, player_id, entity).is_some()
        && world
            .get::<Health>(entity)
            .is_some_and(|health| health.current < 70)
}

/// DAG所属Entityが指定地点を占領してよいかを返す。
///
/// Capture Nodeでは周辺拠点群だけを許可し、Control Nodeでは地域優勢を作る前に
/// 横の拠点へ目的をすり替えない。DAG外のEntityには従来どおり制約を掛けない。
pub(crate) fn capital_route_allows_capture_at(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    position: GridPosition,
) -> bool {
    let Some(commitment) = capital_route_commitment_for_entity(world, player_id, entity) else {
        return true;
    };
    world
        .get::<UnitStats>(entity)
        .is_some_and(|stats| stats.can_capture)
        // 待機線の歩兵が後方の施設を勝手に占領して、前衛Nodeの完了条件を崩さない。
        && commitment.phase != CapitalRouteExecutionPhase::Stage
        && commitment.objective == CapitalRouteNodeObjective::Capture
        && commitment.capture_targets.contains(&position)
}

/// 実行用のセル列から、この手番に到達できる最も前進した位置を返す。
///
/// 座標の直線距離ではなく、DAGセル列に沿った区間目標までの残り移動コストで比較する。
/// これにより山を挟むmap_26でも、橋へ近いが山で隔てられたセルではなく、迂回路上の
/// 次の到達セルを選ぶ。
pub(crate) fn capital_route_advance_destination(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    reachable: &BTreeSet<(usize, usize)>,
) -> Option<GridPosition> {
    if capital_route_is_recovering(world, player_id, entity) {
        return None;
    }
    let commitment = capital_route_commitment_for_entity(world, player_id, entity)?;
    let position = world.get::<GridPosition>(entity).copied()?;
    let stats = world.get::<UnitStats>(entity)?;
    let map = world.get_resource::<Map>()?;
    let master_data = world.get_resource::<MasterDataRegistry>()?;
    let topology = capital_route_topology_for(world, player_id, commitment.island_id)?;
    // 待機線は前衛の局地戦へ渡さず、指定された後方セルで行動終了する。
    // `Wait` はMoveUnitCommandを伴うため、工場上の新造unitもこのセルまで退避できる。
    if commitment.phase == CapitalRouteExecutionPhase::Stage {
        if position == commitment.execution_target {
            return reachable
                .contains(&(position.x, position.y))
                .then_some(position);
        }
        let current_progress = topology.from_home.get(&position).copied().unwrap_or(0);
        let stage_index = commitment
            .path
            .iter()
            .rposition(|cell| *cell == commitment.execution_target)?;
        return commitment
            .path
            .iter()
            .enumerate()
            .filter(|(index, _)| *index <= stage_index)
            .filter(|(_, cell)| reachable.contains(&(cell.x, cell.y)))
            .filter(|(_, cell)| {
                topology
                    .from_home
                    .get(cell)
                    .is_some_and(|progress| *progress >= current_progress)
            })
            .max_by_key(|(index, cell)| (*index, cell.y, cell.x))
            .map(|(_, cell)| *cell)
            .or_else(|| {
                reachable
                    .contains(&(position.x, position.y))
                    .then_some(position)
            });
    }
    if capital_route_region_reached(commitment, position) {
        // 地域内ではanchorへ直行させず、Controlなら局地戦術、Captureなら周辺拠点への
        // 接近スコアへ制御を渡す。
        return None;
    }
    let current_progress = topology.from_home.get(&position).copied().unwrap_or(0);
    let end_index = commitment
        .path
        .iter()
        .rposition(|cell| *cell == commitment.execution_target)?;
    // 戦闘・支援unitは「実際に占領すべき施設」だけを占領役へ空ける。Control Pointの
    // anchorや橋出口は通過・占有して初めて優勢を観測できるため、一歩手前へ止めない。
    // 既にCapture対象を占有している場合だけは、後方の到達可能セルへ戻して退避する。
    let requires_crossing = commitment.crossing_exit.is_some() && !commitment.crossed;
    let reserves_capture_target = commitment.objective == CapitalRouteNodeObjective::Capture
        && commitment
            .capture_targets
            .contains(&commitment.execution_target);
    let advance_end_index = if commitment.phase == CapitalRouteExecutionPhase::Advance
        && !stats.can_capture
        && reserves_capture_target
        && !requires_crossing
    {
        end_index.saturating_sub(1)
    } else {
        end_index
    };
    if commitment.phase == CapitalRouteExecutionPhase::Advance
        && !stats.can_capture
        && reserves_capture_target
        && !requires_crossing
        && position == commitment.execution_target
    {
        return commitment
            .path
            .iter()
            .take(end_index)
            .rev()
            .find(|cell| reachable.contains(&(cell.x, cell.y)))
            .copied();
    }
    let current_index = commitment
        .path
        .iter()
        .enumerate()
        .filter(|(_, cell)| {
            topology
                .from_home
                .get(cell)
                .is_some_and(|progress| *progress <= current_progress)
        })
        .map(|(index, _)| index)
        .max()
        .unwrap_or(0)
        .min(advance_end_index);

    commitment
        .path
        .iter()
        .enumerate()
        .filter(|(index, _)| *index > current_index)
        .filter(|(index, _)| *index <= advance_end_index)
        .filter(|(_, cell)| reachable.contains(&(cell.x, cell.y)))
        .filter_map(|(index, cell)| {
            let remaining_cost = commitment
                .path
                .iter()
                .skip(index.saturating_add(1))
                .take(advance_end_index.saturating_sub(index))
                .try_fold(0_u32, |total, next| {
                    let terrain = map.get_terrain(next.x, next.y)?;
                    let step_cost =
                        get_valid_movement_cost(master_data, stats.movement_type, terrain)?;
                    Some(total.saturating_add(step_cost))
                })?;
            Some((*cell, remaining_cost, index))
        })
        .min_by_key(|(cell, remaining_cost, index)| {
            (*remaining_cost, std::cmp::Reverse(*index), cell.y, cell.x)
        })
        .map(|(cell, _, _)| cell)
}
