//! 砲台の運搬: 補給輸送車で砲台を撃てる位置まで運び、撃つ敵が居なくなったら積み直して前へ運ぶ
//! （V4・標準経路）。
//!
//! 砲台は射程3〜6で中戦車・重自走砲にも大きな損害を与えるが、移動力1・燃料1で自力では
//! ほとんど動けない。補給輸送車（移動力5、砲台1基を積める）で運べば、山や川を挟んだ
//! 経路の向こうの敵も撃てる位置へ据えられる。
//!
//! 降ろす位置は決め打ちせず、候補ごとに「次の手番に射程へ入る敵へ与えられる損害の価値」から
//! 「その位置で次の敵手番に受ける損害の価値」を引いて比べる。橋や山の隘路の手前は、
//! 敵がそこへ集まるので自然に高く評価される。自軍の拠点の上は補給と修理が効くので、
//! 同じ評価なら優先する。
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
//! 砲台は、隣に砲台の通れるマスが一つも無いマス（森・山に囲まれた都市など）へは降ろさない。
//! そこからは二度と輸送車へ乗れず、撃つ敵が居なくなっても前へ運べなくなる。
//!
//! 攻める進軍路には、新しく作った砲台を割り当てる（[`update_gun_routes`]）。隘路の多い盤面では、
//! 射程の長い砲台が盾の後ろから回廊の敵を一方的に撃てるので、攻めの進軍路の戦力の中心に置く。
//! 既に居る砲台（守りの進軍路の砲台など）は割り当てを変えない。何基作るかは生産計画が決める。
//! 割り当てた砲台は、その進軍路の要衝の周り
//! （射程＋[`ROUTE_ZONE_MARGIN`]マス）を受け持ち、外に居れば撃てる敵が居ても積んで運ぶ。
//! 攻めの要衝は占領が進むと先へ移るので、受け持ちの範囲も前へ動き、砲台は前線について前進する。
//!
//! 撃つ敵が居なくなった（その戦線の敵を掃討し終えた）砲台は、輸送車で前へ運ぶ。行き先は
//! 「陣地」（自軍の直接攻撃部隊の盾の後ろで、次の手番に撃てる敵の価値が受ける損害を上回り、
//! 乗り直せるマス）とし、[`SITE_SEARCH_TURNS`] 手番以内で最も評価の高い陣地、無ければ
//! [`FAR_SITE_SEARCH_TURNS`] 手番以内で最も早く着ける陣地へ向かう。敵に最も近い自軍の都市
//! （盤面全体で一つ）へ向かうと、掃討した戦線から別の戦線へ運ばれてしまうので、今居る戦線を
//! 前へ押し上げる向きの陣地を先に探す。撃てる敵が居る砲台は動かさない。陣地の評価は今の
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
use bevy_ecs::prelude::*;
use std::cmp::Reverse;

/// 盤面のセル座標。
type Cell = (usize, usize);

/// 射程の外でも、この距離までに敵が居れば動かずに待つ（敵が寄ってくれば撃てる）。
pub(crate) const HOLD_MARGIN: u32 = 2;

/// 進軍路に割り当てた砲台が受け持つ範囲（要衝から、射程にこのマス数を足した距離まで）。
const ROUTE_ZONE_MARGIN: u32 = 4;

/// 進軍路の受け持ちの範囲の半径（要衝からのマス数）: 射程＋[`ROUTE_ZONE_MARGIN`]。
pub(crate) fn route_zone_radius(gun: &UnitStats) -> u32 {
    gun.max_range.saturating_add(ROUTE_ZONE_MARGIN)
}

/// 陣地を探す範囲（輸送車の移動手番）。
const SITE_SEARCH_TURNS: u32 = 2;

/// 近くに陣地が無いときに、向かう陣地を探す範囲（輸送車の移動手番）。
const FAR_SITE_SEARCH_TURNS: u32 = 4;

/// 陣地が無いとき、受け持ちの範囲に入る降車マスを探す範囲（輸送車の移動手番）。
const ZONE_ENTRY_TURNS: u32 = 8;

/// 降車できなかった手番の、候補マスが落ちた理由の診断を標準エラーへ出すか。
fn drop_diagnostics_enabled() -> bool {
    std::env::var_os("OPENWARS_FERRY_DROP_DIAG").is_some()
}

/// 降車候補マスが落ちた最初の理由。`deliver_one` の判定順と同じ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DropRejection {
    Occupied,
    Impassable,
    NotReboardable,
    OutOfZone,
    OnFacility,
    OnUnownedProperty,
    ReservedForRepair,
    /// 敵の脅威圏内で、前に味方の盾が無い。
    NoScreen,
    /// 据えても次の手番に撃てる敵が無い、またはリスクが上回る。
    NoValue,
    Accepted,
}

/// 診断の対象（どの輸送車・砲台・範囲か）。
#[derive(Clone, Copy)]
struct DropDiagSubject {
    player: PlayerId,
    carrier_id: u64,
    carrier: GridPosition,
    gun: GridPosition,
    zone: Option<GridPosition>,
}

/// 降車できなかった手番の、理由ごとの候補マス数。
#[derive(Debug, Default)]
struct DropRejections {
    counts: [u32; 10],
}

impl DropRejections {
    fn tally(&mut self, reason: DropRejection) {
        self.counts[reason as usize] += 1;
    }

    fn report(&self, subject: &DropDiagSubject, kind: &str, context: &str) {
        let DropDiagSubject {
            player,
            carrier_id,
            carrier,
            gun,
            zone,
        } = *subject;
        let zone = zone.map_or("none".to_owned(), |point| {
            format!("({},{})", point.x, point.y)
        });
        eprintln!(
            "FERRY-DROP-DIAG P{player:?} id={carrier_id} kind={kind} zone={zone} carrier=({},{}) gun=({},{}) occupied={} impassable={} \
             not_reboardable={} out_of_zone={} facility={} unowned={} repair={} no_screen={} \
             no_value={} accepted={} {context}",
            carrier.x,
            carrier.y,
            gun.x,
            gun.y,
            self.counts[DropRejection::Occupied as usize],
            self.counts[DropRejection::Impassable as usize],
            self.counts[DropRejection::NotReboardable as usize],
            self.counts[DropRejection::OutOfZone as usize],
            self.counts[DropRejection::OnFacility as usize],
            self.counts[DropRejection::OnUnownedProperty as usize],
            self.counts[DropRejection::ReservedForRepair as usize],
            self.counts[DropRejection::NoScreen as usize],
            self.counts[DropRejection::NoValue as usize],
            self.counts[DropRejection::Accepted as usize],
        );
    }
}

/// 砲台の運搬を使うか。Cargo feature または環境変数で切り替える。
pub(crate) fn artillery_ferry_enabled() -> bool {
    crate::ai::features::artillery_ferry_enabled()
}

/// 自力でほとんど動けない間接砲（砲台）か。
pub(crate) fn is_ferried_gun(stats: &UnitStats) -> bool {
    stats.min_range > 1 && stats.max_movement <= 1
}

/// 現在燃料が最大値に満たない場合だけ補給が必要。
fn needs_refueling(fuel: Option<u32>, max_fuel: u32) -> bool {
    fuel.is_some_and(|current| current < max_fuel)
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
pub(crate) fn decide_artillery_ferry(
    world: &mut World,
    player_id: PlayerId,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let cached_idle = world
        .get_resource::<ArtilleryFerryIdle>()
        .is_some_and(|idle| idle.player_id == player_id && idle.turn == turn);
    if drop_diagnostics_enabled() {
        eprintln!("FERRY-TURN T{turn} P{player_id:?}");
    }
    // 砲台が1基も無い手番から記録し、後で作った砲台を新しい砲台として見分ける。
    let routes = update_gun_routes(world, player_id, turn);
    let guards = guard_posts(world, player_id, &routes);
    if cached_idle || !has_idle_gun_or_carrier(world, player_id, skip) {
        if std::env::var("OPENWARS_ARTILLERY_FERRY_DEBUG").is_ok() {
            eprintln!("FERRY-SKIP T{turn} P{player_id:?} cached={cached_idle}");
        }
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
    let ferry = ArtilleryFerry {
        board: &board,
        chart: &chart,
        occupancy: &occupancy,
        threat: &threat,
        moved: &moved,
        cargo_of: &cargo_of,
        skip,
        routes: &routes,
        guards: &guards,
    };
    let decision = ferry
        .supply()
        .or_else(|| ferry.fire())
        .or_else(|| ferry.deliver())
        .or_else(|| ferry.board())
        .or_else(|| ferry.meet())
        .or_else(|| ferry.tend())
        .or_else(|| ferry.vacate_facility())
        .or_else(|| ferry.hold());
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
    if decision.is_none() {
        world.insert_resource(ArtilleryFerryIdle { player_id, turn });
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
    /// 割り当てを決めた手番。手番の途中では決め直さない。
    turn: u32,
    routes: HashMap<Entity, GridPosition>,
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
        return routes.routes.clone();
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
    let seen = guns.iter().map(|(entity, ..)| *entity).collect();
    let mut memory = world
        .remove_resource::<GunRouteAssignment>()
        .unwrap_or_default();
    memory.by_player.insert(
        player,
        GunRoutes {
            turn,
            routes: routes.clone(),
            seen,
        },
    );
    world.insert_resource(memory);
    routes
}

/// 運搬中の砲台について、記憶している攻めの進軍路の要衝を返す（生産の既存戦力照合用）。
pub(in crate::ai::v4) fn assigned_attack_route(
    world: &World,
    player: PlayerId,
    gun: Entity,
) -> Option<GridPosition> {
    world
        .get_resource::<GunRouteAssignment>()
        .and_then(|memory| memory.by_player.get(&player))
        .and_then(|routes| routes.routes.get(&gun))
        .copied()
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

/// その手番に運搬の判断が尽きたことの記録。
#[derive(Resource, Debug, Clone, Copy)]
struct ArtilleryFerryIdle {
    player_id: PlayerId,
    turn: u32,
}

/// 判断の対象が居るか。盤面の観測は重いので先に調べる。自軍の砲台が居て、砲台か
/// 補給輸送車のどちらかがまだ行動できる場合に限る。
fn has_idle_gun_or_carrier(world: &mut World, player_id: PlayerId, skip: &HashSet<Entity>) -> bool {
    let mut query = world.query::<(
        Entity,
        &crate::components::Faction,
        &UnitStats,
        &crate::components::ActionCompleted,
    )>();
    let mut has_gun = false;
    let mut has_idle = false;
    for (entity, faction, stats, done) in query.iter(world) {
        if faction.0 != player_id {
            continue;
        }
        let gun = is_ferried_gun(stats);
        has_gun |= gun;
        has_idle |= !done.0 && !skip.contains(&entity) && (gun || is_artillery_carrier(stats));
    }
    has_gun && has_idle
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

    /// 砲台が `cell` から射程に捉える敵のうち、与える損害の価値が最大のものとその価値。
    fn best_shot(&self, gun: &BoardUnit, cell: Cell) -> Option<(&BoardUnit, i64)> {
        self.enemies()
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
            self.distance(Self::cell(gun.position), enemy.position)
                <= gun.stats.max_range.saturating_add(HOLD_MARGIN)
        })
    }

    /// 降ろす位置の評価: 次の手番に撃てる敵の価値から、その位置で受ける損害の価値を引く。
    /// 自軍都市の上ならターン開始時に修復（最大20HP）が効くため、受ける損害の実効リスクが相殺される。
    fn emplacement_value(&self, gun: &BoardUnit, cell: Cell) -> i64 {
        let Some((_, shot)) = self.best_shot(gun, cell) else {
            return 0;
        };
        let damage_hp = self.threat.damage_hp(gun, cell);
        let net_damage_hp = if self.on_own_property(cell) {
            damage_hp.saturating_sub(20)
        } else {
            damage_hp
        };
        let risk = i64::from(net_damage_hp) * i64::from(gun.stats.cost) / 100;
        shot - risk
    }

    /// 自軍の盾の後ろか。最も近い敵との間に、その敵へより近い自軍の直接攻撃部隊がいる。
    fn screened(&self, cell: Cell) -> bool {
        let Some(enemy) = self.enemies().min_by_key(|enemy| {
            (
                self.distance(cell, enemy.position),
                enemy.position.y,
                enemy.position.x,
            )
        }) else {
            return false;
        };
        let reach = self.distance(cell, enemy.position);
        self.own().any(|unit| {
            unit.stats.min_range <= 1
                && unit.stats.max_cargo == 0
                && self.distance(cell, unit.position) < reach
                && self.board.map.distance(
                    unit.position.x,
                    unit.position.y,
                    enemy.position.x,
                    enemy.position.y,
                ) < reach
        })
    }

    /// 砲台を守る味方の直接部隊が存在するか、または敵の脅威が届かない安全な位置か。
    /// 敵の移動・攻撃が届かない完全安全マス（threat == 0）であれば即座に布陣可能。
    /// 敵の脅威圏内（threat > 0）にあるマスでは、前方に味方直接部隊の盾（screened）が
    /// 存在することを完全必須化し、無防備な最前線への据え付けを禁止する。
    fn can_safely_emplace(&self, gun: &BoardUnit, cell: Cell) -> bool {
        if self.enemies().next().is_none() {
            return true;
        }
        // 敵の被弾脅威がゼロの安全な場所は安全に布陣可能
        if self.threat.damage_hp(gun, cell) == 0 {
            return true;
        }
        // 敵の脅威圏内では、前方に味方直接部隊の盾（スクリーン）が存在することが必須
        self.screened(cell)
    }

    /// `origin` から輸送車で `turns` 手番以内に運べる陣地と、その評価（[`Self::emplacement_value`]）・
    /// 輸送車の移動コスト。陣地は、盾の後ろにあり、評価が正で、降ろした後に乗り直せる空きマス。
    /// 輸送車の運べる範囲は、自軍の補給輸送車の足（移動種別・移動力）で測る。
    fn sites(&self, gun: &BoardUnit, origin: Cell, turns: u32) -> Vec<(Cell, i64, u32)> {
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
                self.droppable_cell(gun, *cell, current) && self.can_safely_emplace(gun, *cell)
            })
            .filter_map(|(cell, cost)| {
                let value = self.emplacement_value(gun, cell);
                (value > 0).then_some((cell, value, cost))
            })
            .collect::<Vec<_>>();
        sites.sort_by_key(|(cell, ..)| *cell);
        sites
    }

    /// 砲台を降ろせる地形のマスか。空きマスで、砲台が通れ、降ろした後に乗り直せ、受け持ちの
    /// 範囲に入り、自軍の生産施設・未所有の拠点・修理用に空ける都市の上ではない。
    /// 敵の脅威や射撃価値は見ない（それは据え付ける時点の判断）。
    fn droppable_cell(&self, gun: &BoardUnit, cell: Cell, current: Cell) -> bool {
        (cell == current || !self.occupancy.contains_key(&cell))
            && self.passable(&gun.stats, cell)
            && self.reboardable(gun, cell)
            && self.in_zone(gun, cell)
            && !self.on_own_facility(cell)
            && !self.on_unowned_property(cell)
            && !self.is_city_reserved_for_repair(cell, gun)
    }

    /// 陣地が無いときの行き先: 輸送車が `turns` 手番以内に着ける、受け持ちの範囲の降車マスのうち
    /// 最も早く着けるマス。要衝のマスそのものへ向かうと、要衝の周りの敵の影響で道が切れ、
    /// 直線距離で代用して行き止まりへ引き込まれる。輸送車の現在地から広げた経路なら、
    /// 輸送車自身の移動規則（敵のZOC・占有）がそのまま働く。
    fn zone_entry_site(
        &self,
        carrier: &BoardUnit,
        gun: &BoardUnit,
        origin: Cell,
        turns: u32,
    ) -> Option<Cell> {
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
        let mut drop_cost = HashMap::<Cell, u32>::default();
        for (stop, cost) in &reach {
            for cell in self.board.map.get_adjacent(stop.0, stop.1) {
                let entry = drop_cost.entry(cell).or_insert(u32::MAX);
                *entry = (*entry).min(*cost);
            }
        }
        drop_cost
            .into_iter()
            .filter(|(cell, _)| self.droppable_cell(gun, *cell, current))
            .min_by_key(|(cell, cost)| (*cost, *cell))
            .map(|(cell, _)| cell)
    }

    /// `turns` 手番以内に運べる陣地のうち、評価が最も大きいマスとその評価。
    fn best_site(&self, gun: &BoardUnit, origin: Cell, turns: u32) -> Option<(Cell, i64)> {
        self.sites(gun, origin, turns)
            .into_iter()
            .max_by_key(|(cell, value, _)| (*value, Reverse(*cell)))
            .map(|(cell, value, _)| (cell, value))
    }

    /// `turns` 手番以内に運べる陣地のうち、最も早く着けるマス（同じなら評価の大きい方）。
    /// 近くに陣地が無いとき、今の戦線を前へ押し上げる向きの行き先にする。
    fn nearest_site(&self, gun: &BoardUnit, origin: Cell, turns: u32) -> Option<Cell> {
        self.sites(gun, origin, turns)
            .into_iter()
            .min_by_key(|(cell, value, cost)| (*cost, Reverse(*value), *cell))
            .map(|(cell, ..)| cell)
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
                    && self.reboardable(gun, Self::cell(property.position))
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

    /// そのマスが、砲台の受け持ちの範囲か。攻めの進軍路に割り当てた砲台は要衝から
    /// 射程＋[`ROUTE_ZONE_MARGIN`]マスまで、守りの砲台は守り場所（工場付近）を射程に収める
    /// マスまで（敵を追って前へ出ない）。配分案が無ければどこでもよい。
    fn in_zone(&self, gun: &BoardUnit, cell: Cell) -> bool {
        if let Some(point) = self.routes.get(&gun.entity) {
            return self.distance(cell, *point) <= route_zone_radius(&gun.stats);
        }
        self.guards
            .get(&gun.entity)
            .is_none_or(|point| self.distance(cell, *point) <= gun.stats.max_range)
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

    /// 未所有の物件（敵または中立の都市・拠点など）の上か。
    /// 砲台は占領能力を持たないため、未所有物件へ降ろすと歩兵の占領を妨害してしまう。
    fn on_unowned_property(&self, cell: Cell) -> bool {
        self.board.properties.iter().any(|property| {
            property.owner != Some(self.board.player) && Self::cell(property.position) == cell
        })
    }

    /// 進軍路に割り当てた砲台が、受け持ちの範囲の外に据わっているか（撃てる敵が居ても運ぶ）。
    fn out_of_zone(&self, gun: &BoardUnit) -> bool {
        gun.is_deployed() && !self.in_zone(gun, Self::cell(gun.position))
    }

    /// 砲台をそのマスへ降ろしても、後で隣の輸送車へ乗り直せるか（隣に砲台の通れるマスがある）。
    fn reboardable(&self, gun: &BoardUnit, cell: Cell) -> bool {
        self.board
            .map
            .get_adjacent(cell.0, cell.1)
            .into_iter()
            .any(|neighbor| self.passable(&gun.stats, neighbor))
    }

    /// そのセルが自軍都市であり、かつ周囲の味方直接戦闘部隊の回復のために空けておくべきか。
    /// 都市の周囲（距離 2 以内）に消耗（HP <= 75）して回復を必要としている味方直接部隊が居る場合、
    /// その部隊の回復を阻害しないよう都市を空けておく。
    fn is_city_reserved_for_repair(&self, cell: Cell, gun: &BoardUnit) -> bool {
        if !self.on_own_property(cell) {
            return false;
        }
        let gun_needs_service = gun.health.is_some_and(|h| h < 100)
            || gun.ammo.is_some_and(|(curr, _)| curr <= 3)
            || needs_refueling(gun.fuel, gun.stats.max_fuel);
        if gun_needs_service {
            return false;
        }
        self.own().any(|unit| {
            unit.entity != gun.entity
                && !is_ferried_gun(&unit.stats)
                && !is_artillery_carrier(&unit.stats)
                && unit.health.is_some_and(|h| h <= 75)
                && self.distance(cell, unit.position) <= 2
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
        if !is_ferried_gun(&gun.stats)
            || !gun.is_deployed()
            || !self.enemy_near(gun)
            || self.out_of_zone(gun)
        {
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
                    && !self.out_of_zone(gun)
            })
            .find_map(|gun| {
                let (enemy, _) = self.best_shot(gun, Self::cell(gun.position))?;
                Some((
                    gun.entity,
                    AiCommand::Attack {
                        target_pos: gun.position,
                        target_entity: enemy.entity,
                    },
                ))
            })
    }

    /// 範囲の中心から、最寄りの敵と最寄りの自軍直接部隊までの距離（診断用）。
    fn zone_context(&self, zone: Option<GridPosition>) -> String {
        let Some(point) = zone else {
            return "zone_enemy=- zone_front=-".to_owned();
        };
        let center = Self::cell(point);
        let enemy = self
            .enemies()
            .map(|enemy| self.distance(center, enemy.position))
            .min();
        let front = self
            .own()
            .filter(|unit| unit.stats.min_range <= 1 && unit.stats.max_cargo == 0)
            .map(|unit| self.distance(center, unit.position))
            .min();
        let show = |value: Option<u32>| value.map_or("-".to_owned(), |v| v.to_string());
        format!("zone_enemy={} zone_front={}", show(enemy), show(front))
    }

    /// 降車候補マスが落ちる最初の理由（診断用）。判定順は `deliver_one` と同じ。
    fn drop_rejection(&self, gun: &BoardUnit, cell: Cell, current: Cell) -> DropRejection {
        if cell != current && self.occupancy.contains_key(&cell) {
            DropRejection::Occupied
        } else if !self.passable(&gun.stats, cell) {
            DropRejection::Impassable
        } else if !self.reboardable(gun, cell) {
            DropRejection::NotReboardable
        } else if !self.in_zone(gun, cell) {
            DropRejection::OutOfZone
        } else if self.on_own_facility(cell) {
            DropRejection::OnFacility
        } else if self.on_unowned_property(cell) {
            DropRejection::OnUnownedProperty
        } else if self.is_city_reserved_for_repair(cell, gun) {
            DropRejection::ReservedForRepair
        } else if !self.can_safely_emplace(gun, cell) {
            DropRejection::NoScreen
        } else if self.emplacement_value(gun, cell) <= 0 {
            DropRejection::NoValue
        } else {
            DropRejection::Accepted
        }
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
        // (輸送車が撃破されない, 降ろす位置の評価, 輸送車の被弾が少ない, 座標)。
        type DropKey = (bool, i64, Reverse<u32>, Reverse<(Cell, Cell)>);
        let mut best_drop: Option<DropKey> = None;
        // 降ろせない手番で、候補マスが落ちた理由の内訳を記録する診断（既定では無効）。
        let drop_diag = drop_diagnostics_enabled();
        let mut diag = DropRejections::default();
        // (撃破されない, 被弾ゼロの安全地帯, 前線に近い, 被弾が少ない, 座標) が最大の移動先。
        // 降車できない補給車が敵の射程内（threat_hp > 0）へ無防備に突っ込んで被弾するのを防ぐ。
        type MoveKey = (bool, bool, Reverse<u32>, Reverse<u32>, Reverse<Cell>);
        let mut best_move: Option<MoveKey> = None;
        // 今手番に撃てる位置へ降ろせないときの向かう先: 近くの陣地、無ければ少し遠くまでで
        // 最も早く着ける陣地（今の戦線を押し上げる）。進軍路に割り当てた砲台は、陣地が
        // 無ければその要衝へ向かう。どれも無ければ前線の都市、都市も無ければ敵へ近づく。
        let destination = self
            .best_site(gun, current, SITE_SEARCH_TURNS)
            .map(|(cell, _)| cell)
            .or_else(|| self.nearest_site(gun, current, FAR_SITE_SEARCH_TURNS))
            .or_else(|| {
                crate::ai::features::ferry_zone_entry_enabled()
                    .then(|| self.zone_entry_site(carrier, gun, current, ZONE_ENTRY_TURNS))
                    .flatten()
            })
            .or_else(|| {
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
        for &stop in &stops {
            let threat_hp = self.threat.damage_hp(carrier, stop);
            let survives = threat_hp < hp;
            let move_key = (
                survives,
                threat_hp == 0,
                Reverse(front(stop)),
                Reverse(threat_hp),
                Reverse(stop),
            );
            if best_move.is_none_or(|best| move_key > best) {
                best_move = Some(move_key);
            }
            for cell in self.board.map.get_adjacent(stop.0, stop.1) {
                if drop_diag {
                    diag.tally(self.drop_rejection(gun, cell, current));
                }
                if (cell != current && self.occupancy.contains_key(&cell))
                    || !self.passable(&gun.stats, cell)
                    || !self.reboardable(gun, cell)
                    || !self.in_zone(gun, cell)
                    || self.on_own_facility(cell)
                    || self.on_unowned_property(cell)
                    || self.is_city_reserved_for_repair(cell, gun)
                    || !self.can_safely_emplace(gun, cell)
                {
                    continue;
                }
                let value = self.emplacement_value(gun, cell);
                if value <= 0 {
                    continue;
                }
                let key = (survives, value, Reverse(threat_hp), Reverse((stop, cell)));
                if best_drop.is_none_or(|best| key > best) {
                    best_drop = Some(key);
                }
            }
        }
        if drop_diag && !matches!(best_drop, Some((true, ..))) {
            let (kind, zone) = match self.routes.get(&gun.entity) {
                Some(point) => ("route", Some(*point)),
                None => ("guard", self.guards.get(&gun.entity).copied()),
            };
            let subject = DropDiagSubject {
                player: self.board.player,
                carrier_id: carrier.entity.to_bits(),
                carrier: carrier.position,
                gun: gun.position,
                zone,
            };
            diag.report(&subject, kind, &self.zone_context(zone));
        }
        if let Some((true, _, _, Reverse((stop, cell)))) = best_drop {
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
        // 受け持ちの範囲まで来たのに撃てる位置が無ければ、範囲の中の盾の後ろ（味方より前に
        // 出ない）に据えて待つ。攻めの砲台は要衝から射程以内（距離<=3）まで前進してから据える。
        // ただし、味方スクリーンに守られた自軍都市（修復・自動補給拠点）に到達した場合は
        // 敵の接近を安全に待ち受けるため早期据え付けを許可する。
        if let Some(home) = self
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
                    (*cell == current || !self.occupancy.contains_key(cell))
                        && self.passable(&gun.stats, *cell)
                        && self.reboardable(gun, *cell)
                        && self.in_zone(gun, *cell)
                        && !self.on_own_facility(*cell)
                        && !self.on_unowned_property(*cell)
                        && !self.is_city_reserved_for_repair(*cell, gun)
                        && self.can_safely_emplace(gun, *cell)
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
        // 前線の都市の隣まで来られるなら、味方の盾の後ろであることを厳格に確認して都市の上へ据える。
        if let Some(city) = frontline {
            let city_cell = Self::cell(city);
            let city_free = city_cell == current || !self.occupancy.contains_key(&city_cell);
            let stop = if fresh {
                self.stops(carrier)
            } else {
                vec![current]
            }
            .into_iter()
            .filter(|stop| {
                city_free
                    && *stop != city_cell
                    && self.distance(*stop, city) == 1
                    && self.passable(&gun.stats, city_cell)
                    && !self.is_city_reserved_for_repair(city_cell, gun)
                    && self.can_safely_emplace(gun, city_cell)
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
        if drop_diag && fresh {
            // 目標へ最も近い停止先（安全を無視）と、実際に選んだ停止先の脅威・近さを比べる。
            let closest = stops.iter().copied().min_by_key(|cell| front(*cell));
            let threat_at = |cell: Cell| self.threat.damage_hp(carrier, cell);
            eprintln!(
                "FERRY-MOVE-DIAG id={} hp={} dest={} dest_src={} dest_cell={:?} at={:?} to={:?} stops={} cur(threat={},front={}) chosen(threat={},front={}) closest(threat={},front={})",
                carrier.entity.to_bits(),
                hp,
                destination.is_some(),
                if self.best_site(gun, current, SITE_SEARCH_TURNS).is_some() {
                    "best_site"
                } else if self
                    .nearest_site(gun, current, FAR_SITE_SEARCH_TURNS)
                    .is_some()
                {
                    "nearest_site"
                } else {
                    "route_or_guard"
                },
                destination,
                current,
                stop,
                stops.len(),
                threat_at(current),
                front(current),
                threat_at(stop),
                front(stop),
                closest.map_or(0, threat_at),
                closest.map_or(0, front),
            );
        }
        (fresh && stop != current).then_some(AiCommand::Wait {
            target_pos: GridPosition {
                x: stop.0,
                y: stop.1,
            },
        })
    }

    /// 運ぶべき砲台（未行動・未移動・燃料あり）: 受け持ちの範囲の外に居る砲台と、今の位置から
    /// 撃てる敵が居らず、他に撃てる場所がある砲台。射程のすぐ外に敵が居ても待たせない。
    /// 待っても敵が射程へ来るとは限らず、運べば次の手番から撃てる位置へ据えられる。
    fn idle_guns(&self) -> Vec<&BoardUnit> {
        self.own()
            .filter(|gun| {
                is_ferried_gun(&gun.stats)
                    && self.can_act(gun)
                    && !self.moved.contains(&gun.entity)
                    && gun.fuel.is_none_or(|fuel| fuel > 0)
                    && (self.out_of_zone(gun)
                        || (self.best_shot(gun, Self::cell(gun.position)).is_none()
                            && self.has_better_place(gun)))
            })
            .collect()
    }

    /// 撃てる敵の居ない砲台に、運んで撃てる場所、または前進すべき攻めの作戦目標があるか。
    fn has_better_place(&self, gun: &BoardUnit) -> bool {
        let current = Self::cell(gun.position);
        // 自軍の生産施設（工場・首都など）の上にいる砲台は、施設を空けて後続の生産を
        // 可能にするために絶対に運ぶ。
        if self.on_own_facility(current) {
            return true;
        }
        // 配分案の無い砲台は常に運ぶ。
        if !self.routes.contains_key(&gun.entity) && !self.guards.contains_key(&gun.entity) {
            return true;
        }
        // 4手番以内に今すぐ敵を撃てる陣地があるなら運ぶ。
        if self
            .sites(gun, current, FAR_SITE_SEARCH_TURNS)
            .iter()
            .any(|(cell, ..)| *cell != current)
        {
            return true;
        }
        // 攻めの砲台は、撃てる敵が居なくなっても要衝（進軍目標）が遠いなら前進のために運ぶ。
        // （守りの砲台は陣地で寄ってくる敵を待つため、前進運搬はしない）
        if let Some(home) = self.routes.get(&gun.entity)
            && self.distance(current, *home) > 2
        {
            return true;
        }
        false
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
                    && (is_ferried_gun(&unit.stats) || self.tends_engaged_gun(unit))
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
