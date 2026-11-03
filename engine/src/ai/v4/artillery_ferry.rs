//! 砲台の運搬: 補給輸送車で砲台を撃てる位置まで運び、撃つ敵が居なくなったら積み直して前へ運ぶ
//! （V4・標準経路）。
//!
//! 砲台は射程3〜6で中戦車・重自走砲にも大きな損害を与えるが、移動力1・燃料1で自力では
//! ほとんど動けない。補給輸送車（移動力5、砲台1基を積める）で運べば、山や川を挟んだ
//! 経路の向こうの敵も撃てる位置へ据えられる。
//!
//! 降ろす位置は、射程に収める敵（砲台が損害を与えられるもの）の数が多いマスを選ぶ。同じ数なら
//! 自軍の都市（修理・補給・地形防御が効く）、次に与えられる損害の価値が大きいマスを選ぶ。
//! 降ろしてよいマスの条件は、ゲームのルールと明示された要求だけにする（砲台が入れる空きマス、
//! 自軍の生産施設でない、橋と橋の手前を塞がない）。盾の後ろにしか置かない、といった条件は
//! 付けない。通路を塞ぐ直接部隊の配置は進軍路作戦（`squad/route_operations`）の責務で、
//! 現状は前面の1マスへ向かわせるだけで、通路全体を塞ぐ割当はまだ無い。
//!
//! 積み込むと輸送部隊はその手番に動けず、載せた手番には降ろせない。そのため次の順に判断する。
//!
//! 0. 補給輸送車は、隣の弾が減った砲台へ補給する（[`ArtilleryFerry::supply`]）。
//! 1. 射程に敵が居る砲台は動かずに撃つ（[`ArtilleryFerry::fire`]）。
//! 2. 砲台を載せた補給輸送車は、撃てる位置の隣まで運んで降ろす。撃てる位置が無ければ
//!    前線へ近づく（[`ArtilleryFerry::deliver`]）。
//! 3. 今の位置から撃てる敵が居ない砲台は、隣の空の補給輸送車へ乗る（[`ArtilleryFerry::board`]）。
//! 4. 空の補給輸送車は、乗る砲台の隣へ迎えに行く（[`ArtilleryFerry::meet`]）。
//! 5. 交戦中の砲台の補給に残る補給輸送車が、砲台の乗り込めない森・山の上に居れば、
//!    砲台の隣の乗り込めるマスへ移る（[`ArtilleryFerry::tend`]）。撃つ敵が居なくなったら
//!    すぐ積み直せるようにするため。
//! 6. 砲台と、交戦中の砲台の補給に残る補給輸送車は、その場で待つ。迎えを待つ砲台も
//!    自分では動かさない（燃料1の砲台は1歩で燃料が尽き、輸送車へ乗れなくなる）
//!    （[`ArtilleryFerry::hold`]）。
//!
//! 攻める進軍路には、新しく作った砲台を割り当てる（[`update_gun_routes`]）。隘路の多い盤面では、
//! 射程の長い砲台が盾の後ろから回廊の敵を一方的に撃てるので、攻めの進軍路の戦力の中心に置く。
//! 既に居る砲台（守りの進軍路の砲台など）は割り当てを変えない。何基作るかは生産計画が決める。
//! 割り当てた砲台は、陣地が無いときその進軍路の要衝へ向かう。攻めの要衝は占領が進むと
//! 先へ移るので、砲台は前線について前進する。
//!
//! 撃つ敵が居なくなった（その戦線の敵を掃討し終えた）砲台は、輸送車で前へ運ぶ。行き先は
//! 「陣地」（敵を射程に収めて降ろせるマス）とし、[`FAR_SITE_SEARCH_TURNS`] 手番以内で最も
//! 多くの敵を射程に収める陣地へ向かう。今降ろせる位置よりその陣地の方が多くの敵を収めるなら、
//! 手前で降ろさずに進む。敵に最も近い自軍の都市（盤面全体で一つ）へ向かうと、掃討した戦線
//! から別の戦線へ運ばれてしまうので、今居る戦線の陣地を先に探す。撃てる敵が居る砲台は動かさない。陣地の評価は今の
//! 敵の位置で決まるので、撃てる砲台を「より良い陣地」へ移すと、着く頃には敵が動いていて
//! また移すことになり、撃てない手番ばかり増える（分岐シミュレーションで射撃が半減した）。

use crate::ai::command::AiCommand;
use crate::ai::context::board::{BoardUnit, PlayerBoard};
use crate::ai::v4::maritime::threat::ThreatMap;
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, HasMoved, PlayerId, Transporting, UnitStats};
use crate::resources::{DamageChart, MovementType, Terrain, UnitType};
use crate::systems::movement::{
    OccupantInfo, calculate_reachable_tile_costs, get_valid_movement_cost,
};
use crate::systems::turn_management::PROPERTY_REPAIR_HP;
use bevy_ecs::prelude::*;
use std::cmp::Reverse;

/// 盤面のセル座標。
type Cell = (usize, usize);

/// 射程の外でも、この距離までに敵が居れば動かずに待つ（敵が寄ってくれば撃てる）。
pub(crate) const HOLD_MARGIN: u32 = 2;

/// 陣地を探す範囲（輸送車の移動手番）。
const FAR_SITE_SEARCH_TURNS: u32 = 4;

/// 砲台の運搬を使うか。Cargo feature または環境変数で切り替える。
pub(crate) fn artillery_ferry_enabled() -> bool {
    crate::ai::features::artillery_ferry_enabled()
}

/// 自力でほとんど動けない間接砲（砲台）か。
pub(crate) fn is_ferried_gun(stats: &UnitStats) -> bool {
    stats.min_range > 1 && stats.max_movement <= 1
}

/// 砲台を積んで陸上を走る輸送部隊（補給輸送車）の能力か。輸送船・輸送ヘリは含めない。
pub(crate) fn is_artillery_carrier(stats: &UnitStats) -> bool {
    stats.max_cargo > 0
        && !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
        && stats.loadable_unit_types.contains(&UnitType::Artillery)
}

/// 自軍の、補給輸送車で運ぶ砲台と、それを運ぶ補給輸送車。生産施設から退避させない。
/// 砲台は工場の上から隣の輸送車へ直接乗るので、どちらを動かしても乗り込みが遅れる。
pub(crate) fn own_ferry_units(world: &mut World, player_id: PlayerId) -> HashSet<Entity> {
    let mut query = world.query::<(Entity, &crate::components::Faction, &UnitStats)>();
    query
        .iter(world)
        .filter(|(_, faction, stats)| {
            faction.0 == player_id && (is_ferried_gun(stats) || is_artillery_carrier(stats))
        })
        .map(|(entity, ..)| entity)
        .collect()
}

/// 自軍の砲台・補給輸送車の、運搬としての次の1手。担当外ならNone。
#[cfg(test)]
fn decide_artillery_ferry(
    world: &mut World,
    player_id: PlayerId,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    decide_artillery_ferry_phase(world, player_id, skip, false)
        .or_else(|| decide_artillery_ferry_phase(world, player_id, skip, true))
}

pub(crate) fn decide_artillery_ferry_phase(
    world: &mut World,
    player_id: PlayerId,
    skip: &HashSet<Entity>,
    post: bool,
) -> Option<(Entity, AiCommand)> {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    // 通常部隊の行動で盤面が変わるため、フェーズ間の不成立をキャッシュしない。
    let routes = update_gun_routes(world, player_id, turn);
    let guards = guard_posts(world, player_id, &routes);
    if !has_idle_gun_or_carrier(world, player_id, skip) {
        return None;
    }
    let board = PlayerBoard::observe(world, player_id);
    let chart = world
        .get_resource::<DamageChart>()
        .cloned()
        .unwrap_or_default();
    let moved = board
        .units
        .iter()
        .filter(|unit| {
            world
                .get::<HasMoved>(unit.entity)
                .is_some_and(|moved| moved.0)
        })
        .map(|unit| unit.entity)
        .collect::<HashSet<_>>();
    let cargo_of = board
        .units
        .iter()
        .filter_map(|unit| {
            world
                .get::<Transporting>(unit.entity)
                .map(|transporting| (transporting.0, unit.entity))
        })
        .collect::<HashMap<_, _>>();
    let occupancy = board.occupancy();
    let threat = ThreatMap::new(&board, &chart, &occupancy, player_id);
    let regional_targets = world
        .get_resource::<GunRouteAssignment>()
        .and_then(|memory| memory.by_player.get(&player_id))
        .map(|memory| {
            memory
                .regions
                .iter()
                .filter_map(|(entity, area)| {
                    let gun = board.units.iter().find(|unit| unit.entity == *entity)?;
                    let targets = board
                        .units
                        .iter()
                        .filter(|enemy| {
                            enemy.owner != player_id
                                && enemy.is_deployed()
                                && area.contains(&enemy.position)
                                && enemy.health.is_none_or(|hp| hp > 0)
                                && best_damage(&chart, gun.stats.unit_type, enemy.stats.unit_type)
                                    > 0
                        })
                        .map(|enemy| enemy.entity)
                        .collect::<HashSet<_>>();
                    (!targets.is_empty()).then_some((*entity, targets))
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let bridge_approach = crate::ai::tactics::chokepoint::bridge_approach_cells(&board.map);
    let repair_cities = repair_cities(&board);
    let ferry = ArtilleryFerry {
        bridge_approach: &bridge_approach,
        repair_cities: &repair_cities,
        board: &board,
        chart: &chart,
        occupancy: &occupancy,
        threat: &threat,
        moved: &moved,
        cargo_of: &cargo_of,
        skip,
        routes: &routes,
        guards: &guards,
        regional_targets: &regional_targets,
    };
    let decision = if post {
        ferry
            .deliver()
            .or_else(|| ferry.board())
            .or_else(|| ferry.meet())
            .or_else(|| ferry.tend())
            .or_else(|| ferry.hold())
    } else {
        ferry
            .supply()
            .or_else(|| ferry.fire())
            .or_else(|| ferry.vacate_facility())
    };
    if std::env::var("OPENWARS_ARTILLERY_FERRY_DEBUG").is_ok() {
        let guns = board
            .units
            .iter()
            .filter(|unit| unit.owner == player_id && is_ferried_gun(&unit.stats))
            .map(|unit| (unit.position, unit.action_completed, unit.fuel))
            .collect::<Vec<_>>();
        let carriers = board
            .units
            .iter()
            .filter(|unit| unit.owner == player_id && is_artillery_carrier(&unit.stats))
            .map(|unit| (unit.position, unit.action_completed, unit.cargo))
            .collect::<Vec<_>>();
        eprintln!("FERRY T{turn} guns={guns:?} carriers={carriers:?} -> {decision:?}");
    }
    decision
}

/// 砲台の進軍路への割り当て（砲台 → 進軍路の要衝）と、これまでに見た砲台。
#[derive(Resource, Debug, Clone, Default)]
pub(crate) struct GunRouteAssignment {
    by_player: HashMap<PlayerId, GunRoutes>,
}

#[derive(Debug, Clone, Default)]
struct GunRoutes {
    /// 互換の要衝割当を決めた手番。確定DAG指示の更新は同手番でも投影する。
    turn: u32,
    routes: HashMap<Entity, GridPosition>,
    /// 搭載で地上DAGから外れても、確定済みの制圧地域を配送完了まで保持する。
    regions: HashMap<Entity, Vec<GridPosition>>,
    /// 前の手番までに居た砲台。ここに無い砲台は新しく作った砲台。
    seen: HashSet<Entity>,
}

/// 自軍の砲台と、その居場所（積まれていれば輸送車の位置）・据わっているか。
fn own_guns(world: &mut World, player: PlayerId) -> Vec<(Entity, UnitStats, GridPosition, bool)> {
    let mut query = world.query::<(
        Entity,
        &crate::components::Faction,
        &UnitStats,
        &GridPosition,
        Option<&Transporting>,
    )>();
    let units = query
        .iter(world)
        .filter(|(_, faction, stats, ..)| faction.0 == player && is_ferried_gun(stats))
        .map(|(entity, _, stats, position, carrier)| {
            (
                entity,
                stats.clone(),
                *position,
                carrier.map(|carrier| carrier.0),
            )
        })
        .collect::<Vec<_>>();
    let mut guns = units
        .into_iter()
        .filter_map(|(entity, stats, position, carrier)| match carrier {
            None => Some((entity, stats, position, true)),
            Some(carrier) => world
                .get::<GridPosition>(carrier)
                .map(|at| (entity, stats, *at, false)),
        })
        .collect::<Vec<_>>();
    guns.sort_by_key(|(entity, ..)| *entity);
    guns
}

/// 攻める進軍路（配分案の守りでない作戦）へ、新しく作った砲台を割り当てる。手番の最初に
/// 一度だけ決める。砲台を何基買うかは生産計画（RollingPlan）が決め、ここでは数を固定しない。
///
/// - 一度割り当てた砲台は外さない。前の要衝に最も近い要衝を持つ作戦が攻めの作戦なら、その
///   要衝へ付け替え（要衝が前へ移ると受け持ちも前へ動く）、守りの作戦の方が近ければ（配分案が
///   攻守を入れ替えた）前の要衝のまま、その場の受け持ちを続ける。配分案は手番ごとに揺れるので、
///   それに合わせて砲台を戦線の間で運び直したりしない。
/// - 新しく作った砲台（前の手番までに居なかった砲台）は、割り当ての少ない攻めの要衝から順に、
///   要衝に近いものを割り当てる。既に居る砲台（守りの砲台など）は引き抜かない。
fn update_gun_routes(
    world: &mut World,
    player: PlayerId,
    turn: u32,
) -> HashMap<Entity, GridPosition> {
    let previous = world
        .get_resource::<GunRouteAssignment>()
        .and_then(|memory| memory.by_player.get(&player))
        .cloned();
    if let Some(routes) = previous.as_ref().filter(|routes| routes.turn == turn) {
        let mut regions = routes.regions.clone();
        let mut routes = routes.routes.clone();
        apply_committed_gun_routes(world, player, &mut routes, &mut regions);
        if let Some(mut memory) = world.get_resource_mut::<GunRouteAssignment>()
            && let Some(cached) = memory.by_player.get_mut(&player)
        {
            cached.routes = routes.clone();
            cached.regions = regions;
        }
        return routes;
    }
    let orders = world
        .get_resource::<crate::ai::v4::capital_route::force_demand::RoutePostureMemory>()
        .and_then(|memory| memory.by_player.get(&player))
        .map(|decision| {
            decision
                .orders
                .iter()
                .map(|order| (order.key_point, order.defend))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let attack_points = orders
        .iter()
        .filter(|(_, defend)| !defend)
        .map(|(point, _)| *point)
        .collect::<Vec<_>>();
    let guns = own_guns(world, player);
    let Some(map) = world.get_resource::<crate::resources::Map>() else {
        return HashMap::default();
    };
    let distance = |a: GridPosition, b: GridPosition| map.distance(a.x, a.y, b.x, b.y);
    let mut routes = HashMap::<Entity, GridPosition>::default();
    // 前の手番の割り当てを引き継ぐ。前の要衝に最も近い作戦が攻めならその要衝へ付け替える。
    for (entity, ..) in &guns {
        let Some(point) = previous
            .as_ref()
            .and_then(|routes| routes.routes.get(entity))
        else {
            continue;
        };
        let nearest = orders
            .iter()
            .min_by_key(|(candidate, _)| (distance(*candidate, *point), candidate.y, candidate.x));
        let kept = match nearest {
            // 搭載で地上DAGの観測から外れた間も、積み込む直前の確定配送先を維持する。
            _ if world.get::<Transporting>(*entity).is_some() => *point,
            Some((key_point, false)) => *key_point,
            _ => *point,
        };
        routes.insert(*entity, kept);
    }
    // 新しく作った砲台（前の手番までに居なかった砲台）を、進軍路作戦（SquadManager）の
    // 所属に従って攻め・守りに振り分ける。守りの進軍路へ配属された砲台は攻めへ引き抜かない。
    let is_new = |entity: &Entity| {
        previous
            .as_ref()
            .is_some_and(|previous| !previous.seen.contains(entity))
    };
    let squad_routes = world
        .get_resource::<crate::ai::squad::SquadManager>()
        .map(|manager| manager.route_operation_members.clone());
    let mut defending_new = HashSet::<Entity>::default();
    if let Some(squad_routes) = squad_routes.as_ref() {
        let unassigned_new_guns: Vec<Entity> = guns
            .iter()
            .map(|(entity, ..)| *entity)
            .filter(|entity| !routes.contains_key(entity) && is_new(entity))
            .collect();
        for entity in unassigned_new_guns {
            let Some(assigned) = squad_routes.get(&entity).copied() else {
                continue;
            };
            match orders.iter().min_by_key(|(candidate, _)| {
                (distance(*candidate, assigned), candidate.y, candidate.x)
            }) {
                Some((key_point, false)) => {
                    routes.insert(entity, *key_point);
                }
                Some((_, true)) => {
                    defending_new.insert(entity);
                }
                None => {}
            }
        }
    }
    // SquadManagerに所属が無い新しい砲台（単体テスト等）は、割り当ての少ない攻めの要衝から順に埋める。
    while let Some(point) = attack_points.iter().min_by_key(|point| {
        (
            routes.values().filter(|assigned| assigned == point).count(),
            point.y,
            point.x,
        )
    }) {
        let Some((entity, ..)) = guns
            .iter()
            .filter(|(entity, ..)| {
                !routes.contains_key(entity) && !defending_new.contains(entity) && is_new(entity)
            })
            .min_by_key(|(entity, _, at, _)| (distance(*at, *point), *entity))
        else {
            break;
        };
        routes.insert(*entity, *point);
    }
    let mut regions = previous
        .as_ref()
        .map(|memory| memory.regions.clone())
        .unwrap_or_default();
    regions.retain(|entity, _| guns.iter().any(|(gun, ..)| gun == entity));
    apply_committed_gun_routes(world, player, &mut routes, &mut regions);
    let seen = guns.iter().map(|(entity, ..)| *entity).collect();
    let mut memory = world
        .remove_resource::<GunRouteAssignment>()
        .unwrap_or_default();
    memory.by_player.insert(
        player,
        GunRoutes {
            turn,
            routes: routes.clone(),
            regions,
            seen,
        },
    );
    world.insert_resource(memory);
    routes
}

/// 前進する砲台は、別の要衝割当でなくDAGの現在地域を配送先の正本にする。
/// 保持役・DAG外の砲台は、既存の防衛・輸送割当を維持する。
fn committed_gun_route(world: &World, player: PlayerId, gun: Entity) -> Option<GridPosition> {
    let order = crate::ai::v4::capital_route::commitment::capital_route_commitment_for_entity(
        world, player, gun,
    )?;
    (order.phase == crate::ai::v4::capital_route::model::CapitalRouteExecutionPhase::Advance)
        .then_some(order.execution_target)
}

fn apply_committed_gun_routes(
    world: &mut World,
    player: PlayerId,
    routes: &mut HashMap<Entity, GridPosition>,
    regions: &mut HashMap<Entity, Vec<GridPosition>>,
) {
    for (gun, ..) in own_guns(world, player) {
        if let Some(order) =
            crate::ai::v4::capital_route::commitment::capital_route_commitment_for_entity(
                world, player, gun,
            )
        {
            if order.phase
                == crate::ai::v4::capital_route::model::CapitalRouteExecutionPhase::Advance
            {
                routes.insert(gun, order.execution_target);
                regions.insert(gun, order.control_area.clone());
            } else {
                regions.remove(&gun);
            }
        } else if world
            .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
            .and_then(|registry| registry.attack_target(gun))
            .is_some()
        {
            // 新しい明示配備は、古い地域の記憶より優先する。
            regions.remove(&gun);
        }
    }
}

/// 運搬中の砲台について、記憶している攻めの進軍路の要衝を返す（生産の既存戦力照合用）。
pub(in crate::ai::v4) fn assigned_attack_route(
    world: &World,
    player: PlayerId,
    gun: Entity,
) -> Option<GridPosition> {
    committed_gun_route(world, player, gun).or_else(|| {
        world
            .get_resource::<GunRouteAssignment>()
            .and_then(|memory| memory.by_player.get(&player))
            .and_then(|routes| routes.routes.get(&gun))
            .copied()
    })
}

/// 攻めに割り当てていない砲台の守り場所: 最も近い守りの進軍路の要衝（攻めの砲台の居ない
/// 守りは工場付近）。砲台はそこから射程以内に留まり、敵を追って前へ出ない。
fn guard_posts(
    world: &mut World,
    player: PlayerId,
    routes: &HashMap<Entity, GridPosition>,
) -> HashMap<Entity, GridPosition> {
    let defend_points = world
        .get_resource::<crate::ai::v4::capital_route::force_demand::RoutePostureMemory>()
        .and_then(|memory| memory.by_player.get(&player))
        .map(|decision| {
            decision
                .orders
                .iter()
                .filter(|order| order.defend)
                .map(|order| order.key_point)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if defend_points.is_empty() {
        return HashMap::default();
    }
    let guns = own_guns(world, player);
    let Some(map) = world.get_resource::<crate::resources::Map>() else {
        return HashMap::default();
    };
    guns.into_iter()
        .filter(|(entity, ..)| !routes.contains_key(entity))
        .filter_map(|(entity, _, at, _)| {
            defend_points
                .iter()
                .min_by_key(|point| (map.distance(at.x, at.y, point.x, point.y), point.y, point.x))
                .map(|point| (entity, *point))
        })
        .collect()
}

/// 盤面の観測前に、行動可能な砲台または輸送車がいるか調べる。
fn has_idle_gun_or_carrier(world: &mut World, player_id: PlayerId, skip: &HashSet<Entity>) -> bool {
    let mut query = world.query::<(
        Entity,
        &crate::components::Faction,
        &UnitStats,
        &crate::components::ActionCompleted,
    )>();
    let mut has_idle = false;
    for (entity, faction, stats, done) in query.iter(world) {
        if faction.0 != player_id {
            continue;
        }
        let gun = is_ferried_gun(stats);
        has_idle |= !done.0 && !skip.contains(&entity) && (gun || is_artillery_carrier(stats));
    }
    has_idle
}

/// 損傷した直接部隊の回復場所として、砲台を置かずに空けておく自軍の都市。
///
/// 都市防衛の基本は、直接部隊が通路を塞ぎ、間接部隊を都市に置くこと。ただし都市が複数
/// あるときは、前線で傷んだ直接部隊が下がって回復する都市も要る。物件の上では1手番に
/// [`PROPERTY_REPAIR_HP`] まで回復するので、それ以上損傷した直接部隊を「回復が要る」とする。
/// 回復が要る部隊の数だけ、その部隊に近い都市から順に空ける。ただし最低1つの都市は砲台に
/// 残す（都市が1つなら空けない）。
fn repair_cities(board: &PlayerBoard) -> HashSet<Cell> {
    let cities = board
        .properties
        .iter()
        .filter(|property| {
            property.owner == Some(board.player) && property.terrain == Terrain::City
        })
        .map(|property| (property.position.x, property.position.y))
        .collect::<Vec<_>>();
    if cities.len() < 2 {
        return HashSet::default();
    }
    let mut injured = board
        .units
        .iter()
        .filter(|unit| {
            unit.owner == board.player
                && unit.is_deployed()
                && unit.stats.min_range <= 1
                && unit.stats.max_range > 0
                && !is_artillery_carrier(&unit.stats)
                && unit
                    .health
                    .is_some_and(|hp| 100u32.saturating_sub(hp) >= PROPERTY_REPAIR_HP)
        })
        .collect::<Vec<_>>();
    // 損傷の大きい部隊から順に、近い都市を割り当てる（同じなら座標順で決定的にする）。
    injured.sort_by_key(|unit| {
        (
            unit.health.unwrap_or(100),
            unit.position.y,
            unit.position.x,
            unit.entity.to_bits(),
        )
    });
    let mut reserved = HashSet::default();
    for unit in injured.into_iter().take(cities.len() - 1) {
        let nearest = cities
            .iter()
            .filter(|city| !reserved.contains(*city))
            .min_by_key(|city| {
                (
                    board
                        .map
                        .distance(unit.position.x, unit.position.y, city.0, city.1),
                    city.1,
                    city.0,
                )
            });
        if let Some(city) = nearest {
            reserved.insert(*city);
        }
    }
    reserved
}

/// 運搬の判断で共有する盤面。
struct ArtilleryFerry<'a> {
    board: &'a PlayerBoard,
    chart: &'a DamageChart,
    occupancy: &'a HashMap<Cell, OccupantInfo>,
    threat: &'a ThreatMap<'a>,
    /// この手番に移動済みの部隊。
    moved: &'a HashSet<Entity>,
    /// 輸送部隊から、載せている部隊へ。
    cargo_of: &'a HashMap<Entity, Entity>,
    skip: &'a HashSet<Entity>,
    /// 砲台から、割り当てた攻めの進軍路の要衝へ（[`update_gun_routes`]）。
    routes: &'a HashMap<Entity, GridPosition>,
    /// 攻めに割り当てていない砲台から、守り場所へ（[`guard_posts`]）。
    guards: &'a HashMap<Entity, GridPosition>,
    /// 確定済み地域に有効な敵が残る砲台だけの標的。射撃・輸送・据え付けで共用する。
    regional_targets: &'a HashMap<Entity, HashSet<Entity>>,
    /// 橋とその近辺。砲台を据えると後続の直接部隊の通路を塞ぐ。
    bridge_approach: &'a HashSet<GridPosition>,
    /// 損傷した直接部隊の回復場所として空けておく自軍都市（[`repair_cities`]）。
    repair_cities: &'a HashSet<Cell>,
}

/// 据えた砲台が射程に収める敵。比較は (敵の数, 自軍の都市か, 損害の価値) の順。
/// 多くの防衛ユニットを射程に収める位置を最優先し、同じ数なら修理・補給の効く都市を選ぶ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
struct Coverage {
    covered: u32,
    own_city: bool,
    value: i64,
}

impl ArtilleryFerry<'_> {
    fn own(&self) -> impl Iterator<Item = &BoardUnit> {
        self.board
            .units
            .iter()
            .filter(|unit| unit.owner == self.board.player && unit.is_deployed())
    }

    fn enemies(&self) -> impl Iterator<Item = &BoardUnit> {
        self.board
            .units
            .iter()
            .filter(|unit| unit.owner != self.board.player && unit.is_deployed())
    }

    fn unit(&self, entity: Entity) -> Option<&BoardUnit> {
        self.board.units.iter().find(|unit| unit.entity == entity)
    }

    fn can_act(&self, unit: &BoardUnit) -> bool {
        unit.action_completed == Some(false) && !self.skip.contains(&unit.entity)
    }

    fn distance(&self, from: Cell, to: GridPosition) -> u32 {
        self.board.map.distance(from.0, from.1, to.x, to.y)
    }

    fn cell(position: GridPosition) -> Cell {
        (position.x, position.y)
    }

    /// 別地域の敵を撃って行動済みになると搭載もできないため、制圧中の地域を優先する。
    /// 地域内の有効な敵を掃討した後・DAG外・保持役では従来どおり周囲も撃てる。
    fn serves_mission(&self, gun: &BoardUnit, enemy: &BoardUnit) -> bool {
        self.regional_targets
            .get(&gun.entity)
            .is_none_or(|targets| targets.contains(&enemy.entity))
    }

    /// 砲台が `cell` から射程に捉える敵のうち、与える損害の価値が最大のものとその価値。
    fn best_shot(&self, gun: &BoardUnit, cell: Cell) -> Option<(&BoardUnit, i64)> {
        self.enemies()
            .filter(|enemy| self.serves_mission(gun, enemy))
            .filter(|enemy| {
                (gun.stats.min_range..=gun.stats.max_range)
                    .contains(&self.distance(cell, enemy.position))
            })
            .map(|enemy| (enemy, self.shot_value(gun, enemy)))
            .filter(|(_, value)| *value > 0)
            .max_by_key(|(enemy, value)| (*value, Reverse((enemy.position.y, enemy.position.x))))
    }

    /// 砲台が敵1体へ与える損害の価値（基本ダメージ×敵の価格×敵の残りHP）。
    fn shot_value(&self, gun: &BoardUnit, enemy: &BoardUnit) -> i64 {
        i64::from(best_damage(
            self.chart,
            gun.stats.unit_type,
            enemy.stats.unit_type,
        )) * i64::from(enemy.stats.cost)
            * i64::from(enemy.health.unwrap_or(100))
            / 10_000
    }

    /// 敵が射程のすぐ外に居て、動かずに待てば次の手番に撃てそうか。
    /// 敵の移動力いっぱいまで数えると、工場の上の砲台まで待ち続けて運べなくなる。
    fn enemy_near(&self, gun: &BoardUnit) -> bool {
        self.enemies().any(|enemy| {
            self.serves_mission(gun, enemy)
                && self.distance(Self::cell(gun.position), enemy.position)
                    <= gun.stats.max_range.saturating_add(HOLD_MARGIN)
        })
    }

    /// 砲台をそのマスへ降ろしてよいか。ゲームのルールと明示された要求だけで決める。
    /// - 砲台が入れる空きマス（現在地は除く）
    /// - 自軍の生産施設でない（据わると生産できなくなる）
    /// - 橋と橋の手前でない（後続の直接部隊の通り道を塞がない）
    /// - 損傷した直接部隊の回復場所として空ける都市でない（[`repair_cities`]）
    ///
    /// 盾の有無は条件にしない。通路を塞ぐ配置は進軍路作戦の責務（通路全体を塞ぐ割当は未実装）。
    fn drop_allowed(&self, gun: &BoardUnit, cell: Cell, current: Cell) -> bool {
        (cell == current || !self.occupancy.contains_key(&cell))
            && self.passable(&gun.stats, cell)
            && !self.on_own_facility(cell)
            && !self.repair_cities.contains(&cell)
            && !self.bridge_approach.contains(&GridPosition {
                x: cell.0,
                y: cell.1,
            })
    }

    /// 守りの砲台の持ち場から外れていないか。守りの砲台は持ち場（守りの要衝）を射程に収める
    /// 範囲で寄ってくる敵を待ち、敵を追って前へ出ない。攻め・未割当の砲台には制限が無い。
    fn serves_post(&self, gun: &BoardUnit, cell: Cell) -> bool {
        if self.routes.contains_key(&gun.entity) {
            return true;
        }
        self.guards
            .get(&gun.entity)
            .is_none_or(|post| self.distance(cell, *post) <= gun.stats.max_range)
    }

    /// `cell` に据えた砲台が射程に収める敵（損害を与えられるもの）の数と価値。
    /// 多くの防衛ユニットを同時に撃てる位置ほど、補充を上回る速さで前線を削れる。
    fn coverage(&self, gun: &BoardUnit, cell: Cell) -> Coverage {
        let mut coverage = Coverage {
            own_city: self.on_own_city(cell),
            ..Coverage::default()
        };
        for enemy in self
            .enemies()
            .filter(|enemy| self.serves_mission(gun, enemy))
            .filter(|enemy| {
                (gun.stats.min_range..=gun.stats.max_range)
                    .contains(&self.distance(cell, enemy.position))
            })
        {
            let value = self.shot_value(gun, enemy);
            if value > 0 {
                coverage.covered = coverage.covered.saturating_add(1);
                coverage.value = coverage.value.saturating_add(value);
            }
        }
        coverage
    }

    /// 自軍の都市の上か。都市は手番開始時に修理・補給が効き、地形防御もある。
    fn on_own_city(&self, cell: Cell) -> bool {
        self.board.properties.iter().any(|property| {
            property.owner == Some(self.board.player)
                && property.terrain == Terrain::City
                && Self::cell(property.position) == cell
        })
    }

    /// `origin` から輸送車で `turns` 手番以内に運べる陣地と、そこで射程に収める敵・輸送車の
    /// 移動コスト。陣地は、降ろしてよいマス（[`Self::drop_allowed`]）のうち敵を1体以上射程に
    /// 収めるマス。輸送車の運べる範囲は、自軍の補給輸送車の足（移動種別・移動力）で測る。
    fn sites(&self, gun: &BoardUnit, origin: Cell, turns: u32) -> Vec<(Cell, Coverage, u32)> {
        let Some(carrier) = self.own().find(|unit| is_artillery_carrier(&unit.stats)) else {
            return Vec::new();
        };
        let reach = calculate_reachable_tile_costs(
            &self.board.map,
            self.occupancy,
            origin,
            carrier.stats.movement_type,
            carrier.stats.max_movement.saturating_mul(turns),
            carrier.stats.max_fuel,
            self.board.player,
            carrier.stats.unit_type,
            &self.board.registry,
        );
        let current = Self::cell(gun.position);
        // 降ろすマスごとに、隣の止まれるマスのうち最も安く着くコスト。
        let mut drop_cost = HashMap::<Cell, u32>::default();
        for (stop, cost) in &reach {
            for cell in self.board.map.get_adjacent(stop.0, stop.1) {
                let entry = drop_cost.entry(cell).or_insert(u32::MAX);
                *entry = (*entry).min(*cost);
            }
        }
        let mut sites = drop_cost
            .into_iter()
            .filter(|(cell, _)| {
                self.drop_allowed(gun, *cell, current) && self.serves_post(gun, *cell)
            })
            .filter_map(|(cell, cost)| {
                let coverage = self.coverage(gun, cell);
                (coverage.covered > 0).then_some((cell, coverage, cost))
            })
            .collect::<Vec<_>>();
        sites.sort_by_key(|(cell, ..)| *cell);
        sites
    }

    /// `turns` 手番以内に運べる陣地のうち、最も多くの敵を射程に収めるマス。同じなら
    /// 味方都市・損害の価値が大きい方、次に砲台の被弾が少ない方、それも同じなら早く着く方。
    fn best_site(&self, gun: &BoardUnit, origin: Cell, turns: u32) -> Option<(Cell, Coverage)> {
        self.sites(gun, origin, turns)
            .into_iter()
            .max_by_key(|(cell, coverage, cost)| {
                (
                    *coverage,
                    Reverse(self.threat.damage_hp(gun, *cell)),
                    Reverse(*cost),
                    Reverse(*cell),
                )
            })
            .map(|(cell, coverage, _)| (cell, coverage))
    }

    /// `goal` から各マスへの、輸送車の移動コスト（向かう先までの近さを道のりで測る）。
    fn travel_costs(&self, carrier: &BoardUnit, goal: Cell) -> HashMap<Cell, u32> {
        calculate_reachable_tile_costs(
            &self.board.map,
            self.occupancy,
            goal,
            carrier.stats.movement_type,
            carrier
                .stats
                .max_movement
                .saturating_mul(FAR_SITE_SEARCH_TURNS.saturating_mul(4)),
            u32::MAX,
            self.board.player,
            carrier.stats.unit_type,
            &self.board.registry,
        )
        .into_iter()
        .collect()
    }

    /// 前線の都市: 敵に最も近い自軍の都市（降ろした後に乗り直せる都市に限る）。撃てる敵が
    /// まだ射程に来ていなくても、砲台をここへ据えておけば、敵が寄ってきたときに補給を
    /// 受けながら撃ち続けられる。
    fn frontline_city(&self, gun: &BoardUnit) -> Option<GridPosition> {
        self.board
            .properties
            .iter()
            .filter(|property| {
                property.owner == Some(self.board.player)
                    && property.terrain == Terrain::City
                    && self.passable(&gun.stats, Self::cell(property.position))
            })
            .filter_map(|property| {
                let nearest = self
                    .enemies()
                    .map(|enemy| self.distance(Self::cell(property.position), enemy.position))
                    .min()?;
                Some((nearest, property.position))
            })
            .min_by_key(|(nearest, position)| (*nearest, position.y, position.x))
            .map(|(_, position)| position)
    }

    /// 自軍の拠点（補給と修理が効く）の上か。
    fn on_own_property(&self, cell: Cell) -> bool {
        self.board.properties.iter().any(|property| {
            property.owner == Some(self.board.player) && Self::cell(property.position) == cell
        })
    }

    fn reachable(&self, unit: &BoardUnit) -> Vec<Cell> {
        calculate_reachable_tile_costs(
            &self.board.map,
            self.occupancy,
            Self::cell(unit.position),
            unit.stats.movement_type,
            unit.stats.max_movement,
            unit.fuel.unwrap_or(unit.stats.max_fuel),
            unit.owner,
            unit.stats.unit_type,
            &self.board.registry,
        )
        .into_keys()
        .collect()
    }

    /// 部隊が止まれるセル（現在地か空きセル）。
    fn stops(&self, unit: &BoardUnit) -> Vec<Cell> {
        let current = Self::cell(unit.position);
        self.reachable(unit)
            .into_iter()
            .filter(|cell| *cell == current || !self.occupancy.contains_key(cell))
            .collect()
    }

    fn passable(&self, stats: &UnitStats, cell: Cell) -> bool {
        self.board
            .map
            .get_terrain(cell.0, cell.1)
            .is_some_and(|terrain| {
                terrain != Terrain::Sea
                    && get_valid_movement_cost(&self.board.registry, stats.movement_type, terrain)
                        .is_some()
            })
    }

    /// 自軍の生産施設の上か（砲台が据わると、その施設で生産できなくなる）。
    fn on_own_facility(&self, cell: Cell) -> bool {
        self.board.properties.iter().any(|property| {
            property.owner == Some(self.board.player)
                && Self::cell(property.position) == cell
                && self
                    .board
                    .registry
                    .is_production_facility(property.terrain.as_str())
        })
    }

    fn has_room(&self, carrier: &BoardUnit) -> bool {
        carrier
            .cargo
            .is_some_and(|cargo| cargo.max > 0 && cargo.loaded < cargo.max)
            && !self.cargo_of.contains_key(&carrier.entity)
    }

    /// 交戦中の砲台（射程のすぐ外までに敵が居る）の付き添い（補給役）の補給輸送車。砲台の隣に
    /// 居る空の補給輸送車のうち、砲台が乗り込めるマスに居るもの（無ければEntityの小さいもの）を
    /// 1両だけ選ぶ。補給は1両で足り、2両目は他の砲台を迎えに行ける。
    fn tender_of(&self, gun: &BoardUnit) -> Option<Entity> {
        if !is_ferried_gun(&gun.stats) || !gun.is_deployed() || !self.enemy_near(gun) {
            return None;
        }
        self.own()
            .filter(|carrier| {
                is_artillery_carrier(&carrier.stats)
                    && carrier.stats.can_supply
                    && self.has_room(carrier)
                    && self.distance(Self::cell(carrier.position), gun.position) == 1
            })
            .min_by_key(|carrier| {
                (
                    !self.boardable_from(gun, Self::cell(carrier.position)),
                    carrier.entity,
                )
            })
            .map(|carrier| carrier.entity)
    }

    /// 交戦中の砲台の付き添いに選ばれた補給輸送車か（[`Self::tender_of`]）。
    /// 補給のために砲台のそばへ残し、後方の砲台を迎えに行かせない。
    fn tends_engaged_gun(&self, carrier: &BoardUnit) -> bool {
        is_artillery_carrier(&carrier.stats)
            && self
                .own()
                .any(|gun| self.tender_of(gun) == Some(carrier.entity))
    }

    /// 0. 補給輸送車は、隣の弾か燃料が減った砲台へ、砲台が撃つ・乗る前に補給する。補給は
    ///    対象が未行動のときだけ効き、補給された砲台はその手番にまだ撃てる・乗れる。
    ///    燃料1の砲台は1歩動くと燃料が尽き、輸送車へ乗り込む1歩も踏めなくなるので、
    ///    燃料を戻してから乗せる。
    fn supply(&self) -> Option<(Entity, AiCommand)> {
        self.own()
            .filter(|carrier| {
                is_artillery_carrier(&carrier.stats)
                    && carrier.stats.can_supply
                    && self.has_room(carrier)
                    && self.can_act(carrier)
            })
            .find_map(|carrier| {
                let gun = self
                    .own()
                    .filter(|gun| {
                        is_ferried_gun(&gun.stats)
                            && self.can_act(gun)
                            && self.distance(Self::cell(carrier.position), gun.position) == 1
                            && (gun
                                .ammo
                                .is_some_and(|(primary, _)| primary < gun.stats.max_ammo1)
                                || gun.fuel.is_some_and(|fuel| fuel < gun.stats.max_fuel))
                    })
                    .min_by_key(|gun| {
                        (
                            gun.fuel.unwrap_or(gun.stats.max_fuel),
                            gun.ammo.map_or(0, |(primary, _)| primary),
                            gun.entity,
                        )
                    })?;
                Some((
                    carrier.entity,
                    AiCommand::Supply {
                        target_pos: carrier.position,
                        target_entity: gun.entity,
                    },
                ))
            })
    }

    /// 1. 射程に敵が居る砲台は、動かずに最も価値の高い敵を撃つ。
    fn fire(&self) -> Option<(Entity, AiCommand)> {
        self.own()
            .filter(|gun| {
                is_ferried_gun(&gun.stats)
                    && self.can_act(gun)
                    && !self.moved.contains(&gun.entity)
                    && gun
                        .ammo
                        .is_none_or(|(primary, _)| primary > 0 || gun.stats.max_ammo1 == 0)
            })
            .find_map(|gun| {
                let (enemy, _) = self.best_shot(gun, Self::cell(gun.position)).or_else(|| {
                    // 地域の敵が遠く、今は搭載もできない砲台まで射撃禁止にしない。
                    // 迎車は後処理で進め、隣接して搭載できる手番だけ移動用の行動を残す。
                    let can_board = self.own().any(|carrier| {
                        is_artillery_carrier(&carrier.stats)
                            && self.has_room(carrier)
                            && self.boardable_from(gun, Self::cell(carrier.position))
                    }) && self.has_better_place(gun);
                    if can_board {
                        return None;
                    }
                    self.enemies()
                        .filter(|enemy| {
                            (gun.stats.min_range..=gun.stats.max_range)
                                .contains(&self.distance(Self::cell(gun.position), enemy.position))
                        })
                        .map(|enemy| (enemy, self.shot_value(gun, enemy)))
                        .filter(|(_, value)| *value > 0)
                        .max_by_key(|(enemy, value)| {
                            (*value, Reverse((enemy.position.y, enemy.position.x)))
                        })
                })?;
                Some((
                    gun.entity,
                    AiCommand::Attack {
                        target_pos: gun.position,
                        target_entity: enemy.entity,
                    },
                ))
            })
    }

    /// 2. 砲台を載せた補給輸送車を、撃てる位置の隣まで運んで降ろす。
    fn deliver(&self) -> Option<(Entity, AiCommand)> {
        for carrier in self
            .own()
            .filter(|unit| is_artillery_carrier(&unit.stats) && self.can_act(unit))
        {
            let Some(gun) = self
                .cargo_of
                .get(&carrier.entity)
                .and_then(|entity| self.unit(*entity))
                .filter(|gun| is_ferried_gun(&gun.stats))
            else {
                continue;
            };
            // 今手番に載せた砲台は降ろせない。
            if gun.action_completed != Some(false) {
                continue;
            }
            if let Some(command) = self.deliver_one(carrier, gun) {
                return Some((carrier.entity, command));
            }
        }
        None
    }

    fn deliver_one(&self, carrier: &BoardUnit, gun: &BoardUnit) -> Option<AiCommand> {
        let current = Self::cell(carrier.position);
        let fresh = !self.moved.contains(&carrier.entity);
        let stops = if fresh {
            self.stops(carrier)
        } else {
            vec![current]
        };
        let hp = carrier.health.unwrap_or(100).max(1);
        // (輸送車が撃破されない, 射程に収める敵, 砲台の被弾が少ない, 輸送車の被弾が少ない, 座標)。
        // 被弾は降ろす条件ではなく、同じ数の敵を収める位置の間の比較にだけ使う。
        type DropKey = (
            bool,
            Coverage,
            Reverse<u32>,
            Reverse<u32>,
            Reverse<(Cell, Cell)>,
        );
        let mut best_drop: Option<DropKey> = None;
        // (撃破されない, 向かう先に近い, 被弾が少ない, 座標) が最大の移動先。
        // 輸送車ごと撃破されると砲台も失うので、撃破される停止位置だけは避ける。
        type MoveKey = (bool, Reverse<u32>, Reverse<u32>, Reverse<Cell>);
        let mut best_move: Option<MoveKey> = None;
        // 向かう先: 運べる範囲で最も多くの敵を射程に収める陣地。無ければ進軍路の要衝・
        // 守りの地点、それも無ければ前線の都市、都市も無ければ敵へ近づく。
        let far_site = self.best_site(gun, current, FAR_SITE_SEARCH_TURNS);
        let destination = far_site.map(|(cell, _)| cell).or_else(|| {
            self.routes
                .get(&gun.entity)
                .or_else(|| self.guards.get(&gun.entity))
                .map(|point| Self::cell(*point))
        });
        let frontline = self.frontline_city(gun);
        let travel = destination
            .or(frontline.map(Self::cell))
            .map(|goal| self.travel_costs(carrier, goal));
        let front = |cell: Cell| match (&travel, destination.or(frontline.map(Self::cell))) {
            (Some(costs), Some(goal)) => costs.get(&cell).copied().unwrap_or_else(|| {
                self.distance(
                    cell,
                    GridPosition {
                        x: goal.0,
                        y: goal.1,
                    },
                ) * 10
            }),
            _ => self
                .enemies()
                .map(|enemy| self.distance(cell, enemy.position))
                .min()
                .unwrap_or(u32::MAX),
        };
        for stop in stops {
            let threat_hp = self.threat.damage_hp(carrier, stop);
            let survives = threat_hp < hp;
            let move_key = (
                survives,
                Reverse(front(stop)),
                Reverse(threat_hp),
                Reverse(stop),
            );
            if best_move.is_none_or(|best| move_key > best) {
                best_move = Some(move_key);
            }
            for cell in self.board.map.get_adjacent(stop.0, stop.1) {
                if !self.drop_allowed(gun, cell, current) || !self.serves_post(gun, cell) {
                    continue;
                }
                let coverage = self.coverage(gun, cell);
                if coverage.covered == 0 {
                    continue;
                }
                let key = (
                    survives,
                    coverage,
                    Reverse(self.threat.damage_hp(gun, cell)),
                    Reverse(threat_hp),
                    Reverse((stop, cell)),
                );
                if best_drop.is_none_or(|best| key > best) {
                    best_drop = Some(key);
                }
            }
        }
        // 今降ろせる位置より多くの敵を射程に収める陣地へ運べるなら、手前で降ろさず進む。
        if let Some((true, coverage, _, _, Reverse((stop, cell)))) = best_drop
            && far_site.is_none_or(|(_, far)| far.covered <= coverage.covered)
        {
            return Some(AiCommand::Drop {
                transport_target_pos: GridPosition {
                    x: stop.0,
                    y: stop.1,
                },
                cargo_drop_pos: GridPosition {
                    x: cell.0,
                    y: cell.1,
                },
                cargo_entity: gun.entity,
            });
        }
        // 運べる範囲に敵を射程に収める陣地が無いときだけ、受け持ちの地点の近くへ据えて
        // 敵の接近を待つ。攻めの砲台は要衝から距離3以内まで前進してから据える（自軍拠点の
        // 上なら修理・補給を受けながら待てるので早めに据える）。陣地へ運べるなら待たずに進む。
        if far_site.is_none()
            && let Some(home) = self
                .routes
                .get(&gun.entity)
                .or_else(|| self.guards.get(&gun.entity))
                .copied()
        {
            let attacking = self.routes.contains_key(&gun.entity);
            let stops = if fresh {
                self.stops(carrier)
            } else {
                vec![current]
            };
            let wait = stops
                .into_iter()
                .filter(|stop| self.threat.damage_hp(carrier, *stop) < hp)
                .flat_map(|stop| {
                    self.board
                        .map
                        .get_adjacent(stop.0, stop.1)
                        .into_iter()
                        .map(move |cell| (stop, cell))
                })
                .filter(|(_, cell)| {
                    self.drop_allowed(gun, *cell, current)
                        && (!attacking
                            || self.distance(*cell, home) <= 3
                            || self.on_own_property(*cell)
                            || !fresh)
                })
                .min_by_key(|(stop, cell)| {
                    let toward = if attacking {
                        self.distance(*cell, home)
                    } else {
                        self.enemies()
                            .map(|enemy| self.distance(*cell, enemy.position))
                            .min()
                            .unwrap_or_else(|| self.distance(*cell, home))
                    };
                    (toward, self.threat.damage_hp(carrier, *stop), *stop, *cell)
                });
            if let Some((stop, cell)) = wait {
                return Some(AiCommand::Drop {
                    transport_target_pos: GridPosition {
                        x: stop.0,
                        y: stop.1,
                    },
                    cargo_drop_pos: GridPosition {
                        x: cell.0,
                        y: cell.1,
                    },
                    cargo_entity: gun.entity,
                });
            }
        }
        // 向かう陣地も受け持ちの地点も無ければ、前線の都市（敵に最も近い自軍都市）の上へ据える。
        if far_site.is_none()
            && let Some(city) = frontline
        {
            let city_cell = Self::cell(city);
            let stop = if fresh {
                self.stops(carrier)
            } else {
                vec![current]
            }
            .into_iter()
            .filter(|stop| {
                *stop != city_cell
                    && self.distance(*stop, city) == 1
                    && self.drop_allowed(gun, city_cell, current)
            })
            .max_by_key(|stop| {
                let threat_hp = self.threat.damage_hp(carrier, *stop);
                (threat_hp < hp, Reverse(threat_hp), Reverse(*stop))
            });
            if let Some(stop) = stop
                && self.threat.damage_hp(carrier, stop) < hp
            {
                return Some(AiCommand::Drop {
                    transport_target_pos: GridPosition {
                        x: stop.0,
                        y: stop.1,
                    },
                    cargo_drop_pos: city,
                    cargo_entity: gun.entity,
                });
            }
        }
        let (.., Reverse(stop)) = best_move?;
        (fresh && stop != current).then_some(AiCommand::Wait {
            target_pos: GridPosition {
                x: stop.0,
                y: stop.1,
            },
        })
    }

    /// 運ぶべき砲台（未行動・未移動・燃料あり）: 今の位置から撃てる敵が居らず、運べば
    /// 敵を射程に収められる場所がある砲台。射程のすぐ外に敵が居ても待たせない。
    /// 待っても敵が射程へ来るとは限らず、運べば次の手番から撃てる位置へ据えられる。
    fn idle_guns(&self) -> Vec<&BoardUnit> {
        self.own()
            .filter(|gun| {
                is_ferried_gun(&gun.stats)
                    && self.can_act(gun)
                    && !self.moved.contains(&gun.entity)
                    && gun.fuel.is_none_or(|fuel| fuel > 0)
                    && self.best_shot(gun, Self::cell(gun.position)).is_none()
                    && self.has_better_place(gun)
            })
            .collect()
    }

    /// 撃てる敵の居ない砲台に、運んで撃てる場所、または前進すべき攻めの作戦目標があるか。
    fn has_better_place(&self, gun: &BoardUnit) -> bool {
        let current = Self::cell(gun.position);
        // 自軍の生産施設（工場・首都など）の上にいる砲台は、施設を空けて後続の生産を
        // 可能にするために運ぶ。
        if self.on_own_facility(current) {
            return true;
        }
        // 運べる範囲に敵を射程に収める陣地があれば運ぶ。撃てる状態の砲台はidle_gunsへ
        // 入らないので、射撃中の積み直しは増やさない。
        if self
            .sites(gun, current, FAR_SITE_SEARCH_TURNS)
            .iter()
            .any(|(cell, ..)| *cell != current)
        {
            return true;
        }
        // 配分案がない場合でも、交戦できる敵が全くいなければ無目的に積み直さない。
        if !self.routes.contains_key(&gun.entity) && !self.guards.contains_key(&gun.entity) {
            return self
                .enemies()
                .any(|enemy| self.serves_mission(gun, enemy) && self.shot_value(gun, enemy) > 0);
        }
        // 攻めの砲台は、撃てる敵が居なくなっても要衝（進軍目標）が遠いなら前進のために運ぶ。
        // （守りの砲台は陣地で寄ってくる敵を待つため、前進運搬はしない）
        self.routes
            .get(&gun.entity)
            .is_some_and(|home| self.distance(current, *home) > 2)
    }

    /// 砲台がそのセルへ1歩で乗り込めるか。
    fn boardable_from(&self, gun: &BoardUnit, cell: Cell) -> bool {
        self.distance(cell, gun.position) == 1 && self.passable(&gun.stats, cell)
    }

    /// 3. 運ぶべき砲台が、隣の空の補給輸送車へ乗る。
    fn board(&self) -> Option<(Entity, AiCommand)> {
        for gun in self.idle_guns() {
            // まだ動ける輸送車へ乗るとその手番には運べないが、既に隣に居るなら次の手番に
            // 運べる。迎えに来て行動を終えた輸送車を優先する。
            let carrier = self
                .own()
                .filter(|carrier| {
                    is_artillery_carrier(&carrier.stats)
                        && self.has_room(carrier)
                        && self.boardable_from(gun, Self::cell(carrier.position))
                })
                .min_by_key(|carrier| {
                    (
                        self.can_act(carrier) && !self.moved.contains(&carrier.entity),
                        carrier.position.y,
                        carrier.position.x,
                    )
                });
            if let Some(carrier) = carrier {
                return Some((
                    gun.entity,
                    AiCommand::Load {
                        target_pos: carrier.position,
                        transport_entity: carrier.entity,
                    },
                ));
            }
        }
        None
    }

    /// 4. 空の補給輸送車が、運ぶべき砲台の隣へ迎えに行く。既に隣に迎えが居る砲台は除く。
    fn meet(&self) -> Option<(Entity, AiCommand)> {
        let pending = self
            .idle_guns()
            .into_iter()
            .filter(|gun| {
                !self.own().any(|carrier| {
                    is_artillery_carrier(&carrier.stats)
                        && self.has_room(carrier)
                        && !self.can_act(carrier)
                        && self.boardable_from(gun, Self::cell(carrier.position))
                })
            })
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return None;
        }
        for carrier in self.own().filter(|unit| {
            is_artillery_carrier(&unit.stats)
                && self.can_act(unit)
                && self.has_room(unit)
                && !self.moved.contains(&unit.entity)
                && !self.tends_engaged_gun(unit)
        }) {
            let hp = carrier.health.unwrap_or(100).max(1);
            // (撃破されない, 砲台が乗れる, 砲台に近い, 被弾が少ない, 座標) が最大の待ち合わせ先。
            let best = self
                .stops(carrier)
                .into_iter()
                .filter_map(|stop| {
                    let nearest = pending
                        .iter()
                        .map(|gun| self.distance(stop, gun.position))
                        .min()?;
                    let boardable = pending.iter().any(|gun| self.boardable_from(gun, stop));
                    let threat_hp = self.threat.damage_hp(carrier, stop);
                    Some((
                        threat_hp < hp,
                        boardable,
                        Reverse(nearest),
                        Reverse(threat_hp),
                        Reverse(stop),
                    ))
                })
                .max();
            let Some((.., Reverse(stop))) = best else {
                continue;
            };
            if stop == Self::cell(carrier.position) {
                continue;
            }
            return Some((
                carrier.entity,
                AiCommand::Wait {
                    target_pos: GridPosition {
                        x: stop.0,
                        y: stop.1,
                    },
                },
            ));
        }
        None
    }

    /// 5. 交戦中の砲台に付き添う補給輸送車が、砲台の乗り込めないマス（森・山）に居れば、
    ///    砲台の隣の乗り込めるマスへ移る。砲台は撃つ敵が居なくなった手番にすぐ乗れる。
    fn tend(&self) -> Option<(Entity, AiCommand)> {
        self.own()
            .filter(|carrier| {
                self.can_act(carrier)
                    && !self.moved.contains(&carrier.entity)
                    && self.tends_engaged_gun(carrier)
            })
            .find_map(|carrier| {
                let current = Self::cell(carrier.position);
                let guns = self
                    .own()
                    .filter(|gun| self.tender_of(gun) == Some(carrier.entity))
                    .collect::<Vec<_>>();
                if guns.iter().any(|gun| self.boardable_from(gun, current)) {
                    return None;
                }
                let hp = carrier.health.unwrap_or(100).max(1);
                // (付き添う砲台が乗れる, 被弾が少ない, 座標) が最大の、撃破されない移動先。
                let (_, stop) = self
                    .stops(carrier)
                    .into_iter()
                    .filter(|stop| guns.iter().any(|gun| self.boardable_from(gun, *stop)))
                    .filter_map(|stop| {
                        let threat_hp = self.threat.damage_hp(carrier, stop);
                        (threat_hp < hp).then_some((Reverse(threat_hp), Reverse(stop)))
                    })
                    .max()
                    .map(|(threat, Reverse(stop))| (threat, stop))?;
                Some((
                    carrier.entity,
                    AiCommand::Wait {
                        target_pos: GridPosition {
                            x: stop.0,
                            y: stop.1,
                        },
                    },
                ))
            })
    }

    /// 5.5 自軍生産施設（工場・首都など）の上にいる補給輸送車や砲台は、
    ///     施設を塞いで後続の生産を止めることを防ぐため、施設の外へ退く（vacate）。
    fn vacate_facility(&self) -> Option<(Entity, AiCommand)> {
        // まず補給輸送車: 移動力があるので前線方向の空きマスへ進む
        for carrier in self.own().filter(|unit| {
            is_artillery_carrier(&unit.stats)
                && self.can_act(unit)
                && !self.moved.contains(&unit.entity)
                && self.on_own_facility(Self::cell(unit.position))
        }) {
            let current = Self::cell(carrier.position);
            let hp = carrier.health.unwrap_or(100).max(1);
            let best_stop = self
                .stops(carrier)
                .into_iter()
                .filter(|stop| *stop != current && !self.on_own_facility(*stop))
                .min_by_key(|stop| {
                    let nearest_enemy = self
                        .enemies()
                        .map(|e| self.distance(*stop, e.position))
                        .min()
                        .unwrap_or(u32::MAX);
                    let threat_hp = self.threat.damage_hp(carrier, *stop);
                    (threat_hp >= hp, nearest_enemy, threat_hp, *stop)
                });
            if let Some(stop) = best_stop {
                return Some((
                    carrier.entity,
                    AiCommand::Wait {
                        target_pos: GridPosition {
                            x: stop.0,
                            y: stop.1,
                        },
                    },
                ));
            }
        }
        // 砲台: 工場の上にいる砲台で、隣に空いている平地等があれば1歩出て工場を空ける
        for gun in self.own().filter(|unit| {
            is_ferried_gun(&unit.stats)
                && self.can_act(unit)
                && !self.moved.contains(&unit.entity)
                && unit.fuel.is_none_or(|f| f > 0)
                && self.on_own_facility(Self::cell(unit.position))
        }) {
            let current = Self::cell(gun.position);
            let empty_adjacent = self
                .board
                .map
                .get_adjacent(current.0, current.1)
                .into_iter()
                .filter(|cell| {
                    !self.occupancy.contains_key(cell)
                        && self.passable(&gun.stats, *cell)
                        && !self.on_own_facility(*cell)
                })
                .min_by_key(|cell| {
                    let threat = self.threat.damage_hp(gun, *cell);
                    (threat, *cell)
                });
            if let Some(target) = empty_adjacent {
                return Some((
                    gun.entity,
                    AiCommand::Wait {
                        target_pos: GridPosition {
                            x: target.0,
                            y: target.1,
                        },
                    },
                ));
            }
        }
        None
    }

    /// 6. 砲台と、交戦中の砲台の補給に残る補給輸送車は、動かずにその場で待つ。砲台は燃料1で、
    ///    自分で1歩動くと輸送車へ乗れなくなるので、迎えを待つ砲台も通常の戦術で動かさない。
    ///    ただし、自軍の生産施設の上にいる場合は後続の生産を塞ぐため待機させない。
    fn hold(&self) -> Option<(Entity, AiCommand)> {
        self.own()
            .find(|unit| {
                self.can_act(unit)
                    && !self.moved.contains(&unit.entity)
                    && (is_ferried_gun(&unit.stats)
                        || self.tends_engaged_gun(unit)
                        || (is_artillery_carrier(&unit.stats)
                            && unit.cargo.is_some_and(|cargo| cargo.loaded == 0)))
                    && !self.on_own_facility(Self::cell(unit.position))
            })
            .map(|unit| {
                (
                    unit.entity,
                    AiCommand::Wait {
                        target_pos: unit.position,
                    },
                )
            })
    }
}

/// 主武器・副武器のうち有効な方の基本ダメージ。
fn best_damage(chart: &DamageChart, attacker: UnitType, defender: UnitType) -> u32 {
    chart.get_base_damage(attacker, defender).unwrap_or(0).max(
        chart
            .get_base_damage_secondary(attacker, defender)
            .unwrap_or(0),
    )
}

#[cfg(test)]
mod tests;
