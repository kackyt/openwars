//! 輸送船・輸送ヘリによる島嶼間の兵員輸送。
//!
//! 仕事の無い陸塊に取り残された地上部隊（[`Landmasses::is_stranded`]）を拾い、
//! 未所有の拠点や敵が残る陸塊へ降ろす。1回の呼び出しで1命令だけを返し、次の順で選ぶ。
//!
//! 1. 降車: 積荷を載せた輸送部隊が今手番に揚陸地点へ届くなら降ろす。
//! 2. 出発・航行: 積荷を載せた輸送部隊を目的の陸塊へ進める。今手番にまだ乗れる部隊が
//!    居る場合は、乗船を待ってから出る。
//! 3. 迎え: 空の輸送部隊を、取り残された部隊の近くの乗船地点へ向かわせる。
//! 4. 相乗り: 取り残された歩兵を装甲車へ載せる（輸送船の1枠で2体を運ぶ）。
//! 5. 乗船: 行動を終えた輸送部隊へ、取り残された部隊を乗せる。揚陸作戦の間は
//!    中戦車・重戦車・重自走砲など1枠あたりの戦力が大きい部隊から積む。
//! 6. 集結: まだ乗れない取り残された部隊を乗船地点の近くへ寄せる（生産施設は塞がない）。
//!
//! 輸送部隊は積載すると同じ手番に移動できず、積んだ部隊はその手番に降ろせない。
//! 輸送船は港か浅瀬からしか降ろせない。輸送ヘリは燃料が尽きると墜落するため、
//! 燃料が足りなくなる前に自軍の空港へ戻す。
//!
//! 輸送部隊の移動先は、資金換算の点数を足し引きせず、次の優先順で選ぶ。
//! 自軍の生産施設を塞がない → 次の敵手番に撃沈されない → 目的地へ着くまでの手番数が
//! 少ない → 被弾が少ない → 目的地に近い。被弾は同じ前進量の中での選び分けにだけ使い、
//! 撃沈されない限り揚陸地点への接近を止めない。

mod assault;
mod landing_site;

pub(crate) use assault::{AssaultPlans, tuning as assault_tuning};
pub(crate) use landing_site::strength as landing_site_strength;

use super::landmass::{Landmasses, is_ground};
use super::theater::sea_distances;
use super::threat::ThreatMap;
use crate::ai::command::AiCommand;
use crate::ai::context::board::{BoardUnit, PlayerBoard};
use crate::collections::{HashMap, HashSet};
use crate::components::{
    ActionCompleted, Faction, GridPosition, HasMoved, PlayerId, Transporting, UnitStats,
};
use crate::resources::{DamageChart, MovementType, Terrain, UnitType};
use crate::systems::movement::{
    OccupantInfo, calculate_reachable_tile_costs, get_valid_movement_cost,
};
use bevy_ecs::prelude::*;
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::rc::Rc;

/// 揚陸先の評価で、占領役1体が拠点を獲れる価値（資金換算）。
const CAPTURE_SLOT_VALUE: i64 = 3000;
/// 揚陸先の評価で、戦闘役1体が敵の残る陸塊へ加わる価値。
const COMBAT_SLOT_VALUE: i64 = 2500;
/// 重戦闘部隊を敵首都の陸塊へ運ぶ価値の、他の陸塊への倍率。
const CAPITAL_HEAVY_WEIGHT: i64 = 3;
/// 敵首都のある陸塊へ占領役を届ける上乗せ。
const ENEMY_CAPITAL_BONUS: i64 = 2000;
/// 護衛の無い占領役を降ろすとき、敵の地上部隊を数える降車セルからの距離。
const DROP_DANGER_RADIUS: u32 = 2;
/// 揚陸地点を、積荷の目標からこの距離以内に絞る。
const LANDING_TARGET_RADIUS: u32 = 3;
/// 輸送船が乗船セルへ来るまでの手番数を見積もる、輸送船の移動力。
const LANDER_PICKUP_MOVEMENT: u32 = 5;
/// 燃料の残りがこの手番数分の消費を下回る前に補給へ戻る。
const FUEL_RESERVE_TURNS: u32 = 2;

/// 自軍の輸送部隊・取り残された部隊の次の1手を返す。担当外なら None。
pub(crate) fn decide_sealift_action(
    world: &mut World,
    player_id: PlayerId,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    // 一度「やることが無い」と判断した手番では再計算しない（盤面の観測が重い）。
    if world
        .get_resource::<SealiftIdle>()
        .is_some_and(|idle| idle.player_id == player_id && idle.turn == turn)
    {
        return None;
    }
    if !has_active_transport(world, player_id, skip) {
        if std::env::var("OPENWARS_SEALIFT_DEBUG").is_ok() {
            eprintln!("SEALIFT-SKIP T{turn} no active transport");
        }
        return None;
    }
    let mut context = SealiftContext::observe(world, player_id, skip);
    context.beachhead_origins =
        crate::ai::v4::capital_route::beachhead_route_origins(world, player_id);
    // 敵の守る陸塊への揚陸は、輸送船をそろえてから一斉に行う（手番ごとに1回判断する）。
    let previous = world
        .get_resource::<AssaultPlans>()
        .and_then(|plans| plans.0.get(&player_id).cloned());
    context.assault = context.plan_assault(previous.as_ref(), turn);
    let mut plans = world.get_resource_or_insert_with(AssaultPlans::default);
    match &context.assault {
        Some(plan) => {
            plans.0.insert(player_id, plan.clone());
        }
        None => {
            plans.0.remove(&player_id);
        }
    }
    let decision = context.decide();
    if std::env::var("OPENWARS_SEALIFT_DEBUG").is_ok() && decision.is_none() {
        eprintln!("SEALIFT-IDLE T{turn}");
    }
    if decision.is_none() {
        world.insert_resource(SealiftIdle { player_id, turn });
    }
    decision
}

/// 自軍の港・空港（首都の生産圏内）に居る未行動の地上部隊を、隣の空きセルへ退かせる。
///
/// 地上部隊は港・空港で回復も生産もできず、居座ると艦船・航空機の生産を物理的に塞ぐ。
/// 輸送船へ乗るときは輸送船のセルへ進入するので、港の上で待つ必要は無い。
pub(crate) fn decide_port_relief(
    world: &mut World,
    player_id: PlayerId,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let mut sites = Vec::new();
    let mut capitals = Vec::new();
    let mut properties = world.query::<(&GridPosition, &crate::components::Property)>();
    for (position, property) in properties.iter(world) {
        if property.owner_id != Some(player_id) {
            continue;
        }
        match property.terrain {
            Terrain::Port | Terrain::Airport => sites.push(*position),
            Terrain::Capital => capitals.push(*position),
            _ => {}
        }
    }
    let topology = world.resource::<crate::resources::Map>().topology;
    sites.retain(|site| {
        crate::systems::production::is_within_production_range(&capitals, site.x, site.y, topology)
    });
    if sites.is_empty() {
        return None;
    }
    let mut units = world.query::<(
        Entity,
        &Faction,
        &GridPosition,
        &UnitStats,
        &HasMoved,
        &ActionCompleted,
    )>();
    let blocker = units
        .iter(world)
        .filter(|(entity, faction, position, stats, moved, done)| {
            faction.0 == player_id
                && !moved.0
                && !done.0
                && !skip.contains(entity)
                && !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
                && sites.contains(position)
        })
        .map(|(entity, ..)| entity)
        .min()?;
    let context = SealiftContext::observe(world, player_id, skip);
    // 港・空港の生産が戦局に効く盤面（海で隔てられた首都、または敵が艦船を出している）に
    // 限る。陸続きの盤面では、手番の初めに退かせると攻撃の機会を奪うだけになる。
    let theater = super::theater::MaritimeTheater::observe(&context.board, player_id);
    if !theater.capitals_separated_by_sea && theater.enemy_ships == 0 {
        return None;
    }
    let unit = context
        .board
        .units
        .iter()
        .find(|unit| unit.entity == blocker)?;
    context
        .leave_production_site(unit)
        .map(|command| (blocker, command))
}

/// その手番に輸送の判断が尽きたことの記録。
#[derive(Resource, Debug, Clone, Copy)]
struct SealiftIdle {
    player_id: PlayerId,
    turn: u32,
}

/// 島嶼間輸送を担う部隊種別か（輸送船・輸送ヘリ）。
pub(crate) fn is_sealift_transport(stats: &UnitStats) -> bool {
    matches!(
        stats.unit_type,
        UnitType::Lander | UnitType::TransportHelicopter
    ) && stats.max_cargo > 0
}

/// 少数の敵が上陸した本拠に守りとして残す戦力の、敵の上陸部隊の戦力に対する比（%）。
const HOME_GARRISON_PERCENT: i64 = 200;
/// 1枠あたりの戦力が大きく、揚陸作戦で先に運ぶ重戦闘部隊の価格の下限
/// （中戦車・重戦車・重自走砲。軽戦車・ロケット砲などは含まない）。
const HEAVY_COMBAT_COST: u32 = 15000;

/// 歩兵を載せて陸上を運ぶ部隊か（装甲車）。
pub(crate) fn is_ground_carrier(unit: &BoardUnit) -> bool {
    unit.stats.max_cargo > 0 && !is_sealift_transport(&unit.stats) && is_ground(unit)
}

/// 取り残された部隊を輸送部隊へ積む順（小さいほど先）。
fn loading_rank(unit: &BoardUnit, combat_first: bool) -> u8 {
    let carrier = is_ground_carrier(unit) && unit.cargo.is_some_and(|cargo| cargo.loaded > 0);
    let heavy = !unit.stats.can_capture && !carrier && unit.stats.cost >= HEAVY_COMBAT_COST;
    match (combat_first, heavy, carrier, unit.stats.can_capture) {
        (true, true, ..) => 0,
        (true, _, true, _) => 1,
        (true, _, _, false) => 2,
        (true, ..) => 3,
        (false, _, true, _) => 0,
        (false, _, _, true) => 1,
        (false, true, ..) => 2,
        (false, ..) => 3,
    }
}

/// 輸送の判断が必要な状態か。行動を終えていない輸送部隊があるか、行動を終えた
/// 輸送部隊に空きがあり、まだ乗り込める未行動の地上部隊が居る場合に限る。
fn has_active_transport(world: &mut World, player_id: PlayerId, skip: &HashSet<Entity>) -> bool {
    let mut query = world.query::<(
        Entity,
        &Faction,
        &UnitStats,
        &ActionCompleted,
        Option<&crate::components::CargoCapacity>,
    )>();
    let mut has_room = false;
    let mut fresh_ground = false;
    for (entity, faction, stats, done, capacity) in query.iter(world) {
        if faction.0 != player_id {
            continue;
        }
        if is_sealift_transport(stats) {
            if !done.0 && !skip.contains(&entity) {
                return true;
            }
            has_room |=
                capacity.is_some_and(|capacity| capacity.loaded.len() < capacity.max as usize);
        } else if !done.0
            && !skip.contains(&entity)
            && !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
        {
            fresh_ground = true;
        }
    }
    has_room && fresh_ground
}

/// 盤面のセル座標。
type Cell = (usize, usize);
/// unitの到達可能セルと最小移動コスト。
type ReachableCells = Rc<BTreeMap<Cell, u32>>;
/// 陸路の移動コストの場（セル → コスト）。
type LandCosts = Rc<HashMap<Cell, u32>>;
/// 揚陸地点の優先順のキー（大きいほど良い）: 輸送部隊が撃沈されない、揚陸の価値、
/// 降車セルの近くの敵の少なさ、積荷の目標への近さ、輸送部隊の被弾の少なさ、座標。
type DropKey = (
    bool,
    i64,
    Reverse<u32>,
    Reverse<i64>,
    Reverse<u32>,
    Reverse<(Cell, Cell)>,
);

/// 輸送中の部隊。
#[derive(Debug, Clone)]
struct Cargo {
    entity: Entity,
    stats: UnitStats,
    /// この手番に降ろせるか（今手番に乗った部隊は降ろせない）。
    can_drop: bool,
    /// この積荷（装甲車）に同乗している占領役の数。
    passengers: u32,
}

impl Cargo {
    /// 揚陸先で拠点を占領する部隊を含むか（占領役自身か、占領役を載せた装甲車）。
    fn carries_capturer(&self) -> bool {
        self.stats.can_capture || self.passengers > 0
    }
}

/// 1回の判断で共有する輸送の盤面。
struct SealiftContext {
    board: PlayerBoard,
    landmasses: Landmasses,
    chart: DamageChart,
    player_id: PlayerId,
    occupancy: HashMap<(usize, usize), OccupantInfo>,
    /// 移動も行動もしていない自軍unit。
    fresh: HashSet<Entity>,
    /// 行動を終えていない自軍unit（移動済みで降車を続ける輸送部隊を含む）。
    active: HashSet<Entity>,
    cargo: HashMap<Entity, Vec<Cargo>>,
    production_sites: HashSet<(usize, usize)>,
    own_airports: Vec<(usize, usize)>,
    own_ports: Vec<(usize, usize)>,
    reachable_cache: RefCell<HashMap<Entity, ReachableCells>>,
    boarding_cache: RefCell<HashMap<u32, Rc<Vec<Cell>>>>,
    /// 揚陸作戦の浜からの海路距離（[`assault`] の航行手番数の見積もり）。
    sail_cache: RefCell<Option<Rc<HashMap<Cell, u32>>>>,
    /// unitの現在地から陸路で各セルへ移動するコスト（占有は無視する）。
    walk_cache: RefCell<HashMap<Entity, LandCosts>>,
    /// 陸塊・移動タイプごとの、各セルから乗船して浜へ着くまでのコスト（陸路の移動コスト
    /// と、航行手番数を移動力で換算したコストの和）。
    gather_cache: RefCell<HashMap<(u32, MovementType, u32), LandCosts>>,
    /// 自軍の輸送船から各セルまでの海路距離。
    lander_cache: RefCell<Option<LandCosts>>,
    /// この手番の揚陸作戦（敵の守る陸塊への一斉上陸）。
    assault: Option<assault::AssaultPlan>,
    /// 橋頭堡を起点にした首都攻略DAGの起点。揚陸部隊はこの浜へ届け続ける。
    beachhead_origins: Vec<GridPosition>,
}

impl SealiftContext {
    fn observe(world: &mut World, player_id: PlayerId, skip: &HashSet<Entity>) -> Self {
        let board = PlayerBoard::observe(world, player_id);
        let landmasses = Landmasses::observe(&board, player_id);
        let chart = world
            .get_resource::<DamageChart>()
            .cloned()
            .unwrap_or_default();
        let mut fresh = HashSet::default();
        let mut active = HashSet::default();
        let mut status = world.query::<(Entity, &Faction, &HasMoved, &ActionCompleted)>();
        for (entity, faction, moved, done) in status.iter(world) {
            if faction.0 != player_id || skip.contains(&entity) || done.0 {
                continue;
            }
            active.insert(entity);
            if !moved.0 {
                fresh.insert(entity);
            }
        }
        let mut cargo: HashMap<Entity, Vec<Cargo>> = HashMap::default();
        let mut loaded = world.query::<(Entity, &Transporting, &UnitStats, &ActionCompleted)>();
        for (entity, transporting, stats, done) in loaded.iter(world) {
            cargo.entry(transporting.0).or_default().push(Cargo {
                entity,
                stats: stats.clone(),
                can_drop: !done.0,
                passengers: 0,
            });
        }
        // 装甲車に載った占領役を、装甲車を積んだ輸送部隊の側から数えられるようにする。
        let passengers: HashMap<Entity, u32> = cargo
            .iter()
            .map(|(carrier, list)| {
                let count = list.iter().filter(|item| item.stats.can_capture).count();
                (*carrier, count as u32)
            })
            .collect();
        for list in cargo.values_mut() {
            for item in list.iter_mut() {
                item.passengers = passengers.get(&item.entity).copied().unwrap_or(0);
            }
            list.sort_by_key(|cargo| cargo.entity);
        }
        let owned = |terrains: &[Terrain]| -> Vec<(usize, usize)> {
            board
                .properties
                .iter()
                .filter(|property| {
                    property.owner == Some(player_id) && terrains.contains(&property.terrain)
                })
                .map(|property| (property.position.x, property.position.y))
                .collect()
        };
        let production_sites = owned(&[
            Terrain::Capital,
            Terrain::Factory,
            Terrain::Airport,
            Terrain::Port,
        ])
        .into_iter()
        .collect();
        let own_airports = owned(&[Terrain::Airport]);
        let own_ports = owned(&[Terrain::Port]);
        let occupancy = board.occupancy();
        Self {
            board,
            landmasses,
            chart,
            player_id,
            occupancy,
            fresh,
            active,
            cargo,
            production_sites,
            own_airports,
            own_ports,
            reachable_cache: RefCell::default(),
            assault: None,
            beachhead_origins: Vec::new(),
            boarding_cache: RefCell::default(),
            sail_cache: RefCell::default(),
            walk_cache: RefCell::default(),
            gather_cache: RefCell::default(),
            lander_cache: RefCell::default(),
        }
    }

    fn own_units(&self) -> impl Iterator<Item = &BoardUnit> {
        self.board
            .units
            .iter()
            .filter(move |unit| unit.owner == self.player_id && unit.is_deployed())
    }

    fn transports(&self) -> Vec<&BoardUnit> {
        let mut transports: Vec<_> = self
            .own_units()
            .filter(|unit| is_sealift_transport(&unit.stats))
            .collect();
        transports.sort_by_key(|unit| unit.entity);
        transports
    }

    /// 取り残された、輸送部隊に乗れる自軍の地上部隊（装甲車へ相乗りする組を除く）。
    ///
    /// 積む順は、揚陸作戦の間は重戦闘部隊（中戦車・重戦車・重自走砲）→ 占領役を載せた
    /// 装甲車 → その他の戦闘部隊 → 占領役。輸送船の積載枠は2つしかないため、1枠あたりの
    /// 戦力が大きい部隊から運ぶ。作戦の無い間は、中立拠点の収入が最も早く戦局を変えるので
    /// 占領役（占領役を載せた装甲車を含む）を先に運ぶ。
    fn stranded(&self) -> Vec<&BoardUnit> {
        let paired: HashSet<Entity> = self
            .carrier_pairs()
            .iter()
            .flat_map(|(carrier, rider)| [carrier.entity, rider.entity])
            .collect();
        let mut units: Vec<_> = self
            .own_units()
            .filter(|unit| self.landmasses.is_stranded(unit) && !paired.contains(&unit.entity))
            .chain(self.home_surplus())
            .collect();
        let combat_first = self.assault.is_some() && assault_tuning().combat_first;
        units.sort_by_key(|unit| (loading_rank(unit, combat_first), unit.entity));
        units
    }

    /// 少数の敵が上陸した本拠（自軍の首都の陸塊）で、守りに要らない戦闘役。
    ///
    /// 敵の上陸部隊が居る本拠の戦闘役は取り残されていないが、全員を本拠に留めると輸送する
    /// 部隊が尽きる。敵の上陸部隊の戦力の [`HOME_GARRISON_PERCENT`] を守りとして残し
    /// （敵に近い部隊から）、残りを輸送を待つ部隊として扱う。
    fn home_surplus(&self) -> Vec<&BoardUnit> {
        let Some((home, info)) = self
            .landmasses
            .info
            .iter()
            .find(|(_, info)| info.own_capital && info.enemy_ground > 0)
            .map(|(home, info)| (*home, info))
        else {
            return Vec::new();
        };
        let map = &self.board.map;
        let on_home = |unit: &BoardUnit| self.landmasses.land.at(unit.position) == Some(home);
        let enemies: Vec<GridPosition> = self
            .board
            .units
            .iter()
            .filter(|unit| unit.owner != self.player_id && unit.is_deployed() && on_home(unit))
            .map(|unit| unit.position)
            .collect();
        let distance_to_enemy = |unit: &BoardUnit| {
            enemies
                .iter()
                .map(|enemy| map.distance(enemy.x, enemy.y, unit.position.x, unit.position.y))
                .min()
                .unwrap_or(u32::MAX)
        };
        let mut combat: Vec<&BoardUnit> = self
            .own_units()
            .filter(|unit| {
                is_ground(unit)
                    && !unit.stats.can_capture
                    && !is_ground_carrier(unit)
                    && on_home(unit)
            })
            .collect();
        combat.sort_by_key(|unit| (distance_to_enemy(unit), unit.entity));
        let mut garrison = info.enemy_strength * HOME_GARRISON_PERCENT / 100;
        combat
            .into_iter()
            .skip_while(|unit| {
                let keep = garrison > 0;
                garrison -= landing_site_strength(unit);
                keep
            })
            .collect()
    }

    /// 取り残された陸塊で、空の装甲車と、それに相乗りする占領役の組。
    ///
    /// 装甲車に歩兵を載せてから輸送船へ積むと、輸送船の1枠で装甲車と歩兵の2体を運べる。
    /// 装甲車ごとに、同じ陸塊で最も近い占領役を1体ずつ割り当てる。
    fn carrier_pairs(&self) -> Vec<(&BoardUnit, &BoardUnit)> {
        let stranded: Vec<&BoardUnit> = self
            .own_units()
            .filter(|unit| self.landmasses.is_stranded(unit))
            .collect();
        let mut carriers: Vec<&BoardUnit> = stranded
            .iter()
            .copied()
            .filter(|unit| is_ground_carrier(unit) && unit.cargo.is_some_and(|c| c.loaded == 0))
            .collect();
        carriers.sort_by_key(|unit| unit.entity);
        let map = &self.board.map;
        let mut taken = HashSet::default();
        let mut pairs = Vec::new();
        for carrier in carriers {
            let landmass = self.landmasses.land.at(carrier.position);
            let rider = stranded
                .iter()
                .copied()
                .filter(|unit| {
                    unit.stats.can_capture
                        && !taken.contains(&unit.entity)
                        && carrier
                            .stats
                            .loadable_unit_types
                            .contains(&unit.stats.unit_type)
                        && self.landmasses.land.at(unit.position) == landmass
                })
                .min_by_key(|unit| {
                    (
                        map.distance(
                            unit.position.x,
                            unit.position.y,
                            carrier.position.x,
                            carrier.position.y,
                        ),
                        unit.entity,
                    )
                });
            if let Some(rider) = rider {
                taken.insert(rider.entity);
                pairs.push((carrier, rider));
            }
        }
        pairs
    }

    /// 装甲車と占領役の相乗り。装甲車を先に乗船セルの近くへ寄せ（歩兵を載せた手番は
    /// 動けない）、届く歩兵を載せ、届かない歩兵は装甲車へ寄せる。
    fn pair_carriers(&self, transports: &[&BoardUnit]) -> Option<(Entity, AiCommand)> {
        let pairs = self.carrier_pairs();
        if std::env::var("OPENWARS_SEALIFT_DEBUG").is_ok() {
            let list: Vec<String> = pairs
                .iter()
                .map(|(c, r)| {
                    format!(
                        "R({},{}){}<-I({},{}){}",
                        c.position.x,
                        c.position.y,
                        if self.fresh.contains(&c.entity) {
                            ""
                        } else {
                            "*"
                        },
                        r.position.x,
                        r.position.y,
                        if self.fresh.contains(&r.entity) {
                            ""
                        } else {
                            "*"
                        }
                    )
                })
                .collect();
            eprintln!("PAIRS {}", list.join(" "));
        }
        for (carrier, _) in &pairs {
            if self.fresh.contains(&carrier.entity)
                && let Some(command) = self.gather(carrier, transports)
            {
                return Some((carrier.entity, command));
            }
        }
        let map = &self.board.map;
        for (carrier, rider) in &pairs {
            let target = (carrier.position.x, carrier.position.y);
            if self.fresh.contains(&rider.entity)
                && map.distance(rider.position.x, rider.position.y, target.0, target.1)
                    <= rider.stats.max_movement
                && self.reachable(rider).contains_key(&target)
            {
                return Some((
                    rider.entity,
                    AiCommand::Load {
                        target_pos: carrier.position,
                        transport_entity: carrier.entity,
                    },
                ));
            }
        }
        for (carrier, rider) in &pairs {
            if !self.fresh.contains(&rider.entity) {
                continue;
            }
            let current = (rider.position.x, rider.position.y);
            // 装甲車から陸路で数えたコスト（海を挟んだ装甲車を近いと見なさない）。
            let carrier_costs = self.walk_costs(carrier);
            // 生産施設を塞がない → 装甲車に近い → 現在地に留まる。
            let Some(cell) = self
                .reachable(rider)
                .keys()
                .copied()
                .filter(|cell| self.is_free_for(rider, *cell))
                .min_by_key(|cell| {
                    (
                        self.production_sites.contains(cell),
                        carrier_costs.get(cell).copied().unwrap_or(u32::MAX),
                        *cell != current,
                        *cell,
                    )
                })
            else {
                continue;
            };
            if cell != current {
                return Some((
                    rider.entity,
                    AiCommand::Wait {
                        target_pos: GridPosition {
                            x: cell.0,
                            y: cell.1,
                        },
                    },
                ));
            }
        }
        None
    }

    fn decide(&self) -> Option<(Entity, AiCommand)> {
        let threat = ThreatMap::new(&self.board, &self.chart, &self.occupancy, self.player_id);
        let transports = self.transports();
        let overseas_demand = self.has_overseas_demand();
        // 1. 降車（移動済みで降車を続ける輸送部隊を含む）。
        for transport in &transports {
            if let Some(command) = self.drop_now(transport, &threat) {
                return Some((transport.entity, command));
            }
        }
        let stranded = self.stranded();
        if std::env::var("OPENWARS_SEALIFT_DEBUG").is_ok() {
            let list: Vec<String> = stranded
                .iter()
                .map(|unit| {
                    format!(
                        "{:?}@({},{}){}",
                        unit.stats.unit_type,
                        unit.position.x,
                        unit.position.y,
                        if self.fresh.contains(&unit.entity) {
                            ""
                        } else {
                            "*"
                        }
                    )
                })
                .collect();
            eprintln!(
                "SEALIFT demand={overseas_demand} stranded={}",
                list.join(" ")
            );
        }
        for transport in &transports {
            if !self.fresh.contains(&transport.entity) {
                continue;
            }
            // 燃料・弾薬の補給が先（輸送ヘリは燃料切れで墜落する）。
            if let Some(command) = self.resupply(transport, &threat) {
                return Some((transport.entity, command));
            }
            let loaded = self.cargo.get(&transport.entity).map_or(0, Vec::len);
            let boardable = self.boardable_here(transport, &stranded);
            let has_room = (loaded as u32) < transport.stats.max_cargo;
            // 次の敵手番に沈められる場所では乗船を待たない（敵の戦艦が港の沖に居座ると、
            // 乗船を待つ輸送船と積荷がまとめて沈む）。
            let safe_here = threat
                .damage_hp(transport, (transport.position.x, transport.position.y))
                < transport.health.unwrap_or(100).max(1);
            if has_room && boardable && safe_here {
                // 2/3. 今手番に乗れる部隊が居るので、その場で乗船を待つ。
                return Some((
                    transport.entity,
                    AiCommand::Wait {
                        target_pos: transport.position,
                    },
                ));
            }
            if loaded > 0 {
                if let Some(command) = self.sail_to_destination(transport, &threat) {
                    return Some((transport.entity, command));
                }
            } else if overseas_demand
                && let Some(command) = self.go_to_pickup(transport, &stranded, &threat)
            {
                return Some((transport.entity, command));
            }
        }
        if !overseas_demand {
            return None;
        }
        // 4. 装甲車への相乗り（輸送船の1枠で装甲車と歩兵を運ぶ）。
        if let Some(decision) = self.pair_carriers(&transports) {
            return Some(decision);
        }
        // 5. 乗船。
        for unit in &stranded {
            if !self.fresh.contains(&unit.entity) {
                continue;
            }
            if let Some(command) = self.board_transport(unit, &transports) {
                return Some((unit.entity, command));
            }
        }
        // 6. 集結。生産施設を塞いでいる部隊から先に動かし、施設を空ける。
        let mut gathering = stranded.clone();
        gathering.sort_by_key(|unit| {
            !self
                .production_sites
                .contains(&(unit.position.x, unit.position.y))
        });
        for unit in &gathering {
            if !self.fresh.contains(&unit.entity) {
                continue;
            }
            if let Some(command) = self.gather(unit, &transports) {
                return Some((unit.entity, command));
            }
        }
        None
    }

    /// 自軍の部隊を送る理由のある陸塊が、取り残された部隊の陸塊以外にあるか。
    fn has_overseas_demand(&self) -> bool {
        self.landmasses
            .info
            .values()
            .any(|info| info.capture_need() > 0 || info.needs_combat())
    }

    /// unitの到達可能セルと最小移動コスト。同じ判断の中では1unitにつき1回だけ求める。
    fn reachable(&self, unit: &BoardUnit) -> ReachableCells {
        if let Some(cached) = self.reachable_cache.borrow().get(&unit.entity) {
            return Rc::clone(cached);
        }
        let reachable = Rc::new(calculate_reachable_tile_costs(
            &self.board.map,
            &self.occupancy,
            (unit.position.x, unit.position.y),
            unit.stats.movement_type,
            unit.stats.max_movement,
            unit.fuel.unwrap_or(unit.stats.max_fuel),
            self.player_id,
            unit.stats.unit_type,
            &self.board.registry,
        ));
        self.reachable_cache
            .borrow_mut()
            .insert(unit.entity, Rc::clone(&reachable));
        reachable
    }

    /// 自分自身のセル以外で、他unitが居ないセルか。
    fn is_free_for(&self, unit: &BoardUnit, cell: (usize, usize)) -> bool {
        cell == (unit.position.x, unit.position.y) || !self.occupancy.contains_key(&cell)
    }

    /// 輸送部隊が今のセルから積荷を降ろせるか（輸送船は港・浅瀬だけ）。
    fn can_unload_at(&self, transport: &BoardUnit, cell: (usize, usize)) -> bool {
        crate::systems::transport::can_unload_from_terrain(
            Some(transport.stats.movement_type),
            self.board.map.get_terrain(cell.0, cell.1),
        )
    }

    /// 陸塊 `landmass` へ積荷を降ろせる、輸送部隊の停止セル。
    ///
    /// 広い陸塊では、積荷の目標（未所有拠点・敵）の近くへ降ろせるセルに絞る。
    fn landing_cells(
        &self,
        transport: &BoardUnit,
        landmass: u32,
        cargo: &[Cargo],
    ) -> HashSet<(usize, usize)> {
        let map = &self.board.map;
        let targets: Vec<GridPosition> = cargo
            .iter()
            .flat_map(|item| self.cargo_targets(item, landmass))
            .collect();
        let mut cells = HashSet::default();
        let mut near_targets = HashSet::default();
        for y in 0..map.height {
            for x in 0..map.width {
                if !self.can_unload_at(transport, (x, y)) {
                    continue;
                }
                let stoppable = map.get_terrain(x, y).is_some_and(|terrain| {
                    get_valid_movement_cost(
                        &self.board.registry,
                        transport.stats.movement_type,
                        terrain,
                    )
                    .is_some()
                });
                if !stoppable
                    || !map.get_adjacent(x, y).into_iter().any(|(nx, ny)| {
                        self.landmasses.land.at(GridPosition { x: nx, y: ny }) == Some(landmass)
                    })
                {
                    continue;
                }
                cells.insert((x, y));
                if targets
                    .iter()
                    .any(|target| map.distance(x, y, target.x, target.y) <= LANDING_TARGET_RADIUS)
                {
                    near_targets.insert((x, y));
                }
            }
        }
        if near_targets.is_empty() {
            cells
        } else {
            near_targets
        }
    }

    /// 起点集合から輸送部隊の移動で数えた距離。輸送船は海路、輸送ヘリは直線距離。
    fn distance_field(
        &self,
        transport: &BoardUnit,
        targets: &HashSet<(usize, usize)>,
    ) -> Box<dyn Fn((usize, usize)) -> Option<u32> + '_> {
        if transport.stats.movement_type == MovementType::Ship {
            let field = sea_distances(&self.board, targets.iter().copied());
            Box::new(move |cell| field.get(&cell).copied())
        } else {
            let targets: Vec<_> = targets.iter().copied().collect();
            let map = &self.board.map;
            Box::new(move |cell| {
                targets
                    .iter()
                    .map(|target| map.distance(cell.0, cell.1, target.0, target.1))
                    .min()
            })
        }
    }

    /// 積荷の構成に対する、陸塊 `landmass` へ揚陸する価値。送る理由が無ければNone。
    fn landing_value(&self, cargo: &[Cargo], landmass: u32) -> Option<i64> {
        let info = self.landmasses.info.get(&landmass)?;
        // 占領役を載せた装甲車は、占領役と戦闘役の両方として数える。
        let capturers = cargo
            .iter()
            .map(|cargo| i64::from(cargo.stats.can_capture) + i64::from(cargo.passengers))
            .sum::<i64>();
        let fighters = cargo
            .iter()
            .filter(|cargo| !cargo.stats.can_capture)
            .count() as i64;
        let capture_need = i64::from(info.capture_need());
        let assigned_capturers = capturers.min(capture_need);
        let mut value = assigned_capturers * CAPTURE_SLOT_VALUE;
        if info.wants_combat() {
            // 占領枠を超えた歩兵も、敵の残る陸塊では戦力として働く。
            value += (fighters + capturers - assigned_capturers) * COMBAT_SLOT_VALUE;
        }
        if info.enemy_capital && capturers > 0 {
            value += ENEMY_CAPITAL_BONUS;
        }
        // 重戦闘部隊は敵首都の陸塊（決戦の場）へ優先して運ぶ。周辺の島の争奪には歩兵・
        // 装甲車を送り続け、重戦闘部隊を揚陸作戦の波にまとめる。
        if info.enemy_capital {
            let heavy = cargo
                .iter()
                .filter(|item| !item.carries_capturer() && item.stats.cost >= HEAVY_COMBAT_COST)
                .count() as i64;
            value += heavy * COMBAT_SLOT_VALUE * (CAPITAL_HEAVY_WEIGHT - 1);
        }
        (value > 0).then_some(value)
    }

    /// 積荷を降ろせるなら降車命令を返す。
    ///
    /// 移動前なら今手番に届く揚陸地点のうち最良の地点へ、移動済み（降車を続ける）なら
    /// 現在地から降ろす。揚陸地点は次の優先順で選ぶ: 輸送部隊が次の敵手番に撃沈されない
    /// → 揚陸の価値が高い → 護衛の無い占領役の降車セルの近くに敵が少ない → 積荷の目標に
    /// 近い → 輸送部隊の被弾が少ない。
    fn drop_now(&self, transport: &BoardUnit, threat: &ThreatMap) -> Option<AiCommand> {
        if !self.active.contains(&transport.entity) {
            return None;
        }
        let cargo: Vec<_> = self
            .cargo
            .get(&transport.entity)?
            .iter()
            .filter(|cargo| cargo.can_drop)
            .cloned()
            .collect();
        if cargo.is_empty() {
            return None;
        }
        let current = (transport.position.x, transport.position.y);
        let stops: Vec<(usize, usize)> = if self.fresh.contains(&transport.entity) {
            self.reachable(transport)
                .keys()
                .copied()
                .filter(|cell| self.is_free_for(transport, *cell))
                .collect()
        } else {
            vec![current]
        };
        let hp = transport.health.unwrap_or(100).max(1);
        // (優先順のキー, 降ろす積荷)
        let mut best: Option<(DropKey, Entity)> = None;
        for stop in stops {
            if !self.can_unload_at(transport, stop) {
                continue;
            }
            let threat_hp = threat.damage_hp(transport, stop);
            for item in &cargo {
                for (cell, landmass) in self.drop_cells(transport, item, stop) {
                    // 揚陸作戦の陸塊へは、出撃の手番に作戦の浜からだけ降ろす。
                    if self.assault_holds(transport, stop, landmass) {
                        continue;
                    }
                    // 揚陸先の陸塊に仕事が無ければ降ろさない（出発地へ戻すだけになる）。
                    let Some(value) = self.landing_value(std::slice::from_ref(item), landmass)
                    else {
                        continue;
                    };
                    let approach = self.drop_cell_approach(item, landmass, cell);
                    // 護衛の無い占領役は、降車セルの近くの敵に先に倒される。
                    let danger = if item.stats.can_capture {
                        self.enemies_near(cell, DROP_DANGER_RADIUS)
                    } else {
                        0
                    };
                    let key = (
                        threat_hp < hp,
                        value,
                        Reverse(danger),
                        Reverse(approach),
                        Reverse(threat_hp),
                        Reverse((stop, cell)),
                    );
                    if best.as_ref().is_none_or(|(best_key, _)| key > *best_key) {
                        best = Some((key, item.entity));
                    }
                }
            }
        }
        let ((.., Reverse((stop, cell))), cargo_entity) = best?;
        Some(AiCommand::Drop {
            transport_target_pos: GridPosition {
                x: stop.0,
                y: stop.1,
            },
            cargo_drop_pos: GridPosition {
                x: cell.0,
                y: cell.1,
            },
            cargo_entity,
        })
    }

    /// 停止セル `stop` から積荷 `item` を降ろせる隣接セルと、その陸塊。
    fn drop_cells(
        &self,
        transport: &BoardUnit,
        item: &Cargo,
        stop: (usize, usize),
    ) -> Vec<((usize, usize), u32)> {
        let current = (transport.position.x, transport.position.y);
        self.board
            .map
            .get_adjacent(stop.0, stop.1)
            .into_iter()
            .filter(|cell| {
                // 輸送部隊自身が動いて空くセルには降ろせる。
                (!self.occupancy.contains_key(cell) || *cell == current)
                    && *cell != stop
                    && self
                        .board
                        .map
                        .get_terrain(cell.0, cell.1)
                        .is_some_and(|terrain| {
                            get_valid_movement_cost(
                                &self.board.registry,
                                item.stats.movement_type,
                                terrain,
                            )
                            .is_some()
                        })
            })
            .filter_map(|cell| {
                let landmass = self.landmasses.land.at(GridPosition {
                    x: cell.0,
                    y: cell.1,
                })?;
                Some((cell, landmass))
            })
            .collect()
    }

    /// 積荷が陸塊 `landmass` で向かう目標（占領役は未所有拠点、戦闘役は敵と敵拠点）。
    fn cargo_targets(&self, item: &Cargo, landmass: u32) -> Vec<GridPosition> {
        let Some(info) = self.landmasses.info.get(&landmass) else {
            return Vec::new();
        };
        if item.carries_capturer() && !info.unowned.is_empty() {
            return info.unowned.clone();
        }
        let enemy_property = self.board.properties.iter().filter(|property| {
            property.owner.is_some_and(|owner| owner != self.player_id)
                && self.landmasses.land.at(property.position) == Some(landmass)
        });
        self.board
            .units
            .iter()
            .filter(|unit| {
                unit.owner != self.player_id
                    && unit.is_deployed()
                    && self.landmasses.land.at(unit.position) == Some(landmass)
            })
            .map(|unit| unit.position)
            .chain(enemy_property.map(|property| property.position))
            .collect()
    }

    /// 降車セルから、積荷が向かう目標までの距離。
    fn drop_cell_approach(&self, item: &Cargo, landmass: u32, cell: (usize, usize)) -> i64 {
        let map = &self.board.map;
        self.cargo_targets(item, landmass)
            .iter()
            .map(|target| i64::from(map.distance(cell.0, cell.1, target.x, target.y)))
            .min()
            .unwrap_or(0)
    }

    /// セル `cell` から `radius` 以内に居る、敵の地上部隊の数。
    fn enemies_near(&self, cell: (usize, usize), radius: u32) -> u32 {
        self.board
            .units
            .iter()
            .filter(|unit| {
                unit.owner != self.player_id
                    && unit.is_deployed()
                    && is_ground(unit)
                    && self
                        .board
                        .map
                        .distance(cell.0, cell.1, unit.position.x, unit.position.y)
                        <= radius
            })
            .count() as u32
    }

    /// 燃料が足りなくなった輸送部隊を補給施設へ戻す。
    fn resupply(&self, transport: &BoardUnit, threat: &ThreatMap) -> Option<AiCommand> {
        let fuel = transport.fuel?;
        let sites: HashSet<(usize, usize)> = match transport.stats.movement_type {
            MovementType::Air => self.own_airports.iter().copied().collect(),
            MovementType::Ship => self.own_ports.iter().copied().collect(),
            _ => return None,
        };
        let sites: HashSet<_> = sites
            .into_iter()
            .filter(|site| self.is_free_for(transport, *site))
            .collect();
        if sites.is_empty() {
            return None;
        }
        let field = self.distance_field(transport, &sites);
        let current = (transport.position.x, transport.position.y);
        let distance = field(current)?;
        let reserve = transport
            .stats
            .daily_fuel_consumption
            .saturating_mul(FUEL_RESERVE_TURNS)
            .saturating_add(2);
        if fuel > distance.saturating_add(reserve) {
            return None;
        }
        if distance == 0 {
            // 補給施設上で次の手番の補給を待つ。
            return Some(AiCommand::Wait {
                target_pos: transport.position,
            });
        }
        // 補給施設そのもの（空港・港）が目的地なので、生産施設への到着を許す。
        self.step_toward(transport, &field, true, threat)
    }

    /// 到達可能セルのうち、距離場の目的地へ向けて最良のセルへ動く。
    ///
    /// 次の優先順で選ぶ: 自軍の生産施設を塞がない（`site_is_goal` なら到着は許す）→
    /// 次の敵手番に撃沈されない → 目的地へ着くまでの手番数が少ない → 被弾が少ない →
    /// 目的地に近い → 現在地に留まる。同じ手番数で着けるセルの中では安全なセルで待ち、
    /// 次の手番に移動と降車を一度に行う。
    fn step_toward(
        &self,
        transport: &BoardUnit,
        field: &dyn Fn((usize, usize)) -> Option<u32>,
        site_is_goal: bool,
        threat: &ThreatMap,
    ) -> Option<AiCommand> {
        let current = (transport.position.x, transport.position.y);
        let hp = transport.health.unwrap_or(100).max(1);
        let movement = transport.stats.max_movement.max(1);
        let cell = self
            .reachable(transport)
            .keys()
            .copied()
            .filter(|cell| self.is_free_for(transport, *cell))
            .filter_map(|cell| {
                let distance = field(cell)?;
                let blocks_production =
                    self.production_sites.contains(&cell) && !(site_is_goal && distance == 0);
                let threat_hp = threat.damage_hp(transport, cell);
                Some((
                    (
                        blocks_production,
                        threat_hp >= hp,
                        distance.div_ceil(movement),
                        threat_hp,
                        distance,
                        cell != current,
                        cell,
                    ),
                    cell,
                ))
            })
            .min_by_key(|(key, _)| *key)
            .map(|(_, cell)| cell)?;
        Some(AiCommand::Wait {
            target_pos: GridPosition {
                x: cell.0,
                y: cell.1,
            },
        })
    }

    /// 積荷を載せた輸送部隊を、最も価値の高い陸塊の揚陸地点へ進める。
    fn sail_to_destination(&self, transport: &BoardUnit, threat: &ThreatMap) -> Option<AiCommand> {
        let (landmass, mut cells) = self.destination(transport)?;
        if std::env::var("OPENWARS_SEALIFT_DEBUG").is_ok() {
            let mut sample: Vec<_> = cells.iter().copied().collect();
            sample.sort_unstable();
            sample.truncate(4);
            eprintln!(
                "DEST lander({},{}) landmass{landmass} cargo{:?} cells{:?} assault{:?}",
                transport.position.x,
                transport.position.y,
                self.cargo
                    .get(&transport.entity)
                    .map(|c| c.iter().map(|i| i.stats.unit_type).collect::<Vec<_>>()),
                sample,
                self.assault
                    .as_ref()
                    .map(|plan| (plan.landmass, plan.launch))
            );
        }
        // 揚陸作戦の陸塊へは、出撃までは集結海域で待ち、出撃したら作戦の浜へ向かう。
        if let Some(assault_cells) = self.assault_destination(transport, landmass) {
            cells = assault_cells;
        }
        let field = self.distance_field(transport, &cells);
        self.step_toward(transport, &*field, false, threat)
    }

    /// 積荷を載せた輸送部隊が向かう陸塊と、その揚陸地点。
    fn destination(&self, transport: &BoardUnit) -> Option<(u32, HashSet<Cell>)> {
        let cargo = self.cargo.get(&transport.entity)?;
        let current = (transport.position.x, transport.position.y);
        let mut best: Option<(i64, u32, HashSet<Cell>)> = None;
        let mut landmasses: Vec<_> = self.landmasses.info.keys().copied().collect();
        landmasses.sort_unstable();
        for landmass in landmasses {
            let Some(value) = self.landing_value(cargo, landmass) else {
                continue;
            };
            let cells = self.landing_cells(transport, landmass, cargo);
            if cells.is_empty() {
                continue;
            }
            let field = self.distance_field(transport, &cells);
            let Some(distance) = field(current) else {
                continue;
            };
            let turns = i64::from(distance.div_ceil(transport.stats.max_movement.max(1)));
            let score = value * 10 / (turns + 1);
            if best
                .as_ref()
                .is_none_or(|(best_score, ..)| score > *best_score)
            {
                best = Some((score, landmass, cells));
            }
        }
        best.map(|(_, landmass, cells)| (landmass, cells))
    }

    /// このセルで、取り残された部隊が今手番にこの輸送部隊へ乗れるか。
    fn boardable_here(&self, transport: &BoardUnit, stranded: &[&BoardUnit]) -> bool {
        let map = &self.board.map;
        stranded.iter().any(|unit| {
            self.fresh.contains(&unit.entity)
                && transport
                    .stats
                    .loadable_unit_types
                    .contains(&unit.stats.unit_type)
                // 移動力の届かない部隊は到達判定を省く。
                && map.distance(
                    unit.position.x,
                    unit.position.y,
                    transport.position.x,
                    transport.position.y,
                ) <= unit.stats.max_movement
                && self
                    .reachable(unit)
                    .contains_key(&(transport.position.x, transport.position.y))
        })
    }

    /// 取り残された部隊のうち、この輸送部隊に乗れる部隊が乗船できるセル。
    ///
    /// 地上部隊は輸送船の居る港・浅瀬へ、輸送ヘリの居る陸上セルへ進入して乗る。
    fn pickup_cells(
        &self,
        transport: &BoardUnit,
        stranded: &[&BoardUnit],
    ) -> HashMap<(usize, usize), u32> {
        let map = &self.board.map;
        let mut cells: HashMap<(usize, usize), u32> = HashMap::default();
        for unit in stranded.iter().filter(|unit| {
            transport
                .stats
                .loadable_unit_types
                .contains(&unit.stats.unit_type)
        }) {
            let Some(landmass) = self.landmasses.land.at(unit.position) else {
                continue;
            };
            let radius = unit.stats.max_movement.max(1) * 2;
            for cell in super::theater::cells_within_ring(map, unit.position, 0, radius) {
                if !self.is_free_for(transport, cell) {
                    continue;
                }
                let Some(terrain) = map.get_terrain(cell.0, cell.1) else {
                    continue;
                };
                let boardable = if transport.stats.movement_type == MovementType::Ship {
                    // 港は陸塊の一部、浅瀬は陸塊に接していれば乗れる。乗船地点が浜に近い
                    // 港・浅瀬へ絞られていれば、そこだけで拾う。
                    self.boarding_cells(landmass).contains(&cell)
                        && match terrain {
                            Terrain::Port => {
                                self.landmasses.land.at(GridPosition {
                                    x: cell.0,
                                    y: cell.1,
                                }) == Some(landmass)
                            }
                            Terrain::Shoal => {
                                map.get_adjacent(cell.0, cell.1)
                                    .into_iter()
                                    .any(|(nx, ny)| {
                                        self.landmasses.land.at(GridPosition { x: nx, y: ny })
                                            == Some(landmass)
                                    })
                            }
                            _ => false,
                        }
                } else {
                    self.landmasses.land.at(GridPosition {
                        x: cell.0,
                        y: cell.1,
                    }) == Some(landmass)
                        && get_valid_movement_cost(
                            &self.board.registry,
                            unit.stats.movement_type,
                            terrain,
                        )
                        .is_some()
                };
                if !boardable {
                    continue;
                }
                // 陸路の移動コストで数える（海を挟んだ乗船セルを近いと見なさない）。
                let Some(walk) = self.walk_to(unit, cell) else {
                    continue;
                };
                let turns = walk.div_ceil(unit.stats.max_movement.max(1));
                cells
                    .entry(cell)
                    .and_modify(|best| *best = (*best).min(turns))
                    .or_insert(turns);
            }
        }
        cells
    }

    /// 空の輸送部隊を、取り残された部隊を拾って浜へ最も早く届けられる乗船セルへ向かわせる。
    fn go_to_pickup(
        &self,
        transport: &BoardUnit,
        stranded: &[&BoardUnit],
        threat: &ThreatMap,
    ) -> Option<AiCommand> {
        let cells = self.pickup_cells(transport, stranded);
        if cells.is_empty() {
            return None;
        }
        let current = (transport.position.x, transport.position.y);
        let own_field = self.distance_field(transport, &HashSet::from_iter([current]));
        // 輸送部隊の到着手番と、乗る部隊の到着手番の遅い方が最小のセルを選ぶ。
        let movement = transport.stats.max_movement.max(1);
        let mut best: Option<(u32, bool, (usize, usize))> = None;
        for (cell, walk_turns) in &cells {
            let Some(distance) = own_field(*cell) else {
                continue;
            };
            // 乗せた後に浜まで航行する手番数も足す（浜から遠い乗船セルで待たない）。
            let turns = distance.div_ceil(movement).max(*walk_turns) + self.sail_turns(*cell);
            let blocks_production = self.production_sites.contains(cell);
            let key = (turns, blocks_production, *cell);
            if best.is_none_or(|best| key < best) {
                best = Some(key);
            }
        }
        let (_, _, target) = best?;
        let field = self.distance_field(transport, &HashSet::from_iter([target]));
        self.step_toward(transport, &*field, true, threat)
    }

    /// 行動を終えた輸送部隊へ、今手番に届く取り残された部隊を乗せる。
    fn board_transport(&self, unit: &BoardUnit, transports: &[&BoardUnit]) -> Option<AiCommand> {
        let map = &self.board.map;
        let near: Vec<&&BoardUnit> = transports
            .iter()
            .filter(|transport| {
                map.distance(
                    unit.position.x,
                    unit.position.y,
                    transport.position.x,
                    transport.position.y,
                ) <= unit.stats.max_movement
            })
            .collect();
        if near.is_empty() {
            return None;
        }
        let reachable = self.reachable(unit);
        near.into_iter()
            .filter(|transport| {
                // 移動前の輸送部隊へ乗ると、その輸送部隊は今手番に動けなくなる。
                // 乗船を待つと決めた（行動を終えた）輸送部隊だけへ乗る。
                !self.fresh.contains(&transport.entity)
                    && transport
                        .stats
                        .loadable_unit_types
                        .contains(&unit.stats.unit_type)
                    && self.cargo.get(&transport.entity).map_or(0, Vec::len)
                        < transport.stats.max_cargo as usize
                    && reachable.contains_key(&(transport.position.x, transport.position.y))
            })
            .min_by_key(|transport| transport.entity)
            .map(|transport| AiCommand::Load {
                target_pos: transport.position,
                transport_entity: transport.entity,
            })
    }

    /// まだ乗れない取り残された部隊を、輸送部隊の乗船セルの近くへ寄せる。
    ///
    /// 乗船セルは、陸路で歩く手番数と、そこから浜まで輸送船で航行する手番数の和が最も
    /// 小さいものを選ぶ（[`Self::gather_costs`]）。
    fn gather(&self, unit: &BoardUnit, transports: &[&BoardUnit]) -> Option<AiCommand> {
        let carriers: Vec<&BoardUnit> = transports
            .iter()
            .copied()
            .filter(|transport| {
                transport
                    .stats
                    .loadable_unit_types
                    .contains(&unit.stats.unit_type)
            })
            .collect();
        if carriers.is_empty() {
            return self.leave_production_site(unit);
        }
        // 輸送船が拾える港・浅瀬が近くにあればそこへ寄せる。輸送ヘリは部隊の居場所まで
        // 迎えに来られるので、生産施設を塞がない限り動かない。
        if !carriers
            .iter()
            .any(|carrier| carrier.stats.movement_type == MovementType::Ship)
        {
            return self.leave_production_site(unit);
        }
        let landmass = self.landmasses.land.at(unit.position)?;
        let boarding = self.boarding_cells(landmass);
        if boarding.is_empty() {
            return self.leave_production_site(unit);
        }
        let costs = self.gather_costs(unit, landmass);
        let current = (unit.position.x, unit.position.y);
        // 乗船セルの隣で待つ。乗船セル（港・浅瀬）そのものや生産施設は塞がない。
        // 優先順: 生産施設を塞がない → 陸路で乗船セルへ歩き浜へ着くまでが早い → 現在地に
        // 留まる。直線距離で寄せると、海を挟んだ乗船セルへ向かって海岸で止まってしまう。
        let cell = self
            .reachable(unit)
            .keys()
            .copied()
            .filter(|cell| {
                self.is_free_for(unit, *cell) && (*cell == current || !boarding.contains(cell))
            })
            .min_by_key(|cell| {
                (
                    self.production_sites.contains(cell),
                    costs.get(cell).copied().unwrap_or(u32::MAX),
                    *cell != current,
                    *cell,
                )
            })?;
        (cell != current || self.production_sites.contains(&current)).then_some(AiCommand::Wait {
            target_pos: GridPosition {
                x: cell.0,
                y: cell.1,
            },
        })
    }

    /// 起点（セルと初期コスト）から、移動タイプ `movement_type` の陸路の移動コストで数えた
    /// 各セルへの最小コスト。占有は無視する（味方は通り抜けられ、敵は動く）。
    fn land_costs(
        &self,
        seeds: impl IntoIterator<Item = (Cell, u32)>,
        movement_type: MovementType,
    ) -> HashMap<Cell, u32> {
        let map = &self.board.map;
        let mut costs: HashMap<Cell, u32> = HashMap::default();
        let mut heap = std::collections::BinaryHeap::new();
        for (cell, cost) in seeds {
            if costs.get(&cell).is_none_or(|known| cost < *known) {
                costs.insert(cell, cost);
                heap.push(Reverse((cost, cell)));
            }
        }
        while let Some(Reverse((cost, cell))) = heap.pop() {
            if costs.get(&cell).is_some_and(|known| *known < cost) {
                continue;
            }
            for next in map.get_adjacent(cell.0, cell.1) {
                let Some(step) = map.get_terrain(next.0, next.1).and_then(|terrain| {
                    get_valid_movement_cost(&self.board.registry, movement_type, terrain)
                }) else {
                    continue;
                };
                let total = cost + step;
                if costs.get(&next).is_none_or(|known| total < *known) {
                    costs.insert(next, total);
                    heap.push(Reverse((total, next)));
                }
            }
        }
        costs
    }

    /// unitの現在地から陸路で各セルへ移動するコスト。同じ判断の中では1unitにつき1回だけ求める。
    fn walk_costs(&self, unit: &BoardUnit) -> LandCosts {
        if let Some(cached) = self.walk_cache.borrow().get(&unit.entity) {
            return Rc::clone(cached);
        }
        let costs = Rc::new(self.land_costs(
            [((unit.position.x, unit.position.y), 0)],
            unit.stats.movement_type,
        ));
        self.walk_cache
            .borrow_mut()
            .insert(unit.entity, Rc::clone(&costs));
        costs
    }

    /// unitが陸路で乗船セル `cell` へ入るコスト。浅瀬は地上部隊が進めないので、隣の陸の
    /// セルまでのコストに、輸送船へ乗り込む1歩を足す。
    fn walk_to(&self, unit: &BoardUnit, cell: Cell) -> Option<u32> {
        let costs = self.walk_costs(unit);
        costs.get(&cell).copied().or_else(|| {
            self.board
                .map
                .get_adjacent(cell.0, cell.1)
                .into_iter()
                .filter_map(|next| costs.get(&next))
                .min()
                .map(|cost| cost + 1)
        })
    }

    /// 自軍の輸送船から各セルまでの海路距離（最も近い輸送船から）。同じ判断の中で1回だけ求める。
    fn lander_arrival(&self) -> LandCosts {
        if let Some(cached) = self.lander_cache.borrow().as_ref() {
            return Rc::clone(cached);
        }
        let landers: Vec<Cell> = self
            .own_units()
            .filter(|unit| {
                is_sealift_transport(&unit.stats) && unit.stats.movement_type == MovementType::Ship
            })
            .map(|unit| (unit.position.x, unit.position.y))
            .collect();
        let field = Rc::new(sea_distances(&self.board, landers));
        *self.lander_cache.borrow_mut() = Some(Rc::clone(&field));
        field
    }

    /// 陸塊 `landmass` の各セルから、陸路で乗船セルへ歩き、輸送船で揚陸作戦の浜へ着くまでの
    /// コスト。輸送船が乗船セルへ来るまでの手番数と、浜までの航行手番数は、unitの移動力で
    /// 陸路のコストへ換算する（1手番の航行 = 1手番の行軍）。
    fn gather_costs(&self, unit: &BoardUnit, landmass: u32) -> LandCosts {
        let movement = unit.stats.max_movement.max(1);
        let key = (landmass, unit.stats.movement_type, movement);
        if let Some(cached) = self.gather_cache.borrow().get(&key) {
            return Rc::clone(cached);
        }
        // 輸送船が乗船セルへ来るまでの手番数も足す。輸送船の来ない端の乗船セルで待つと、
        // 近くに乗船セルがあっても運ばれない。
        let pickup = self.lander_arrival();
        let seeds: Vec<(Cell, u32)> = self
            .boarding_cells(landmass)
            .iter()
            .map(|cell| {
                let arrival = pickup
                    .get(cell)
                    .map_or(0, |distance| distance.div_ceil(LANDER_PICKUP_MOVEMENT));
                (*cell, (self.sail_turns(*cell) + arrival) * movement)
            })
            .collect();
        let costs = Rc::new(self.land_costs(seeds, unit.stats.movement_type));
        self.gather_cache
            .borrow_mut()
            .insert(key, Rc::clone(&costs));
        costs
    }

    /// 陸塊 `landmass` の部隊が輸送船へ乗り込める港・浅瀬。同じ判断の中で陸塊ごとに1回だけ求める。
    fn boarding_cells(&self, landmass: u32) -> Rc<Vec<(usize, usize)>> {
        if let Some(cached) = self.boarding_cache.borrow().get(&landmass) {
            return Rc::clone(cached);
        }
        let map = &self.board.map;
        let mut cells = Vec::new();
        for y in 0..map.height {
            for x in 0..map.width {
                let on_shore = match map.get_terrain(x, y) {
                    Some(Terrain::Port) => {
                        self.landmasses.land.at(GridPosition { x, y }) == Some(landmass)
                    }
                    Some(Terrain::Shoal) => map.get_adjacent(x, y).into_iter().any(|(nx, ny)| {
                        self.landmasses.land.at(GridPosition { x: nx, y: ny }) == Some(landmass)
                    }),
                    _ => false,
                };
                if on_shore {
                    cells.push((x, y));
                }
            }
        }
        let cells = Rc::new(cells);
        self.boarding_cache
            .borrow_mut()
            .insert(landmass, Rc::clone(&cells));
        cells
    }

    /// 生産施設の上に居る場合だけ、隣の空きセルへ退く。
    fn leave_production_site(&self, unit: &BoardUnit) -> Option<AiCommand> {
        let current = (unit.position.x, unit.position.y);
        if !self.production_sites.contains(&current) || !is_ground(unit) {
            return None;
        }
        self.reachable(unit)
            .iter()
            .map(|(cell, cost)| (*cell, *cost))
            .filter(|(cell, _)| {
                self.is_free_for(unit, *cell) && !self.production_sites.contains(cell)
            })
            .min_by_key(|(cell, cost)| (*cost, *cell))
            .map(|(cell, _)| AiCommand::Wait {
                target_pos: GridPosition {
                    x: cell.0,
                    y: cell.1,
                },
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::v4::maritime::test_support::{load, naval_world, property, spawn};

    fn decide(world: &mut World) -> Option<(Entity, AiCommand)> {
        decide_sealift_action(world, PlayerId(1), &HashSet::default())
    }

    #[test]
    fn loaded_lander_drops_capturer_next_to_neutral_city() {
        // 左: 自軍の島。中央: 海。右: 中立都市の島（浅瀬から揚陸できる）。
        let mut world = naval_world(&["H.~~~,C."]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 0, 6, 0);
        let lander = spawn(&mut world, 1, UnitType::Lander, 3, 0);
        let infantry = load(&mut world, lander, UnitType::Infantry, 1);

        let (entity, command) = decide(&mut world).unwrap();
        assert_eq!(entity, lander);
        match command {
            AiCommand::Drop {
                transport_target_pos,
                cargo_drop_pos,
                cargo_entity,
            } => {
                assert_eq!(transport_target_pos, GridPosition { x: 5, y: 0 });
                assert_eq!(cargo_drop_pos, GridPosition { x: 6, y: 0 });
                assert_eq!(cargo_entity, infantry);
            }
            other => panic!("揚陸するはず: {other:?}"),
        }
    }

    #[test]
    fn loaded_lander_closes_in_on_guarded_landing_site() {
        // 右の島に中立都市と敵の戦車。揚陸地点（浅瀬x=12）は今手番に届かない。
        // 戦車は海へ入れないので、沖合の集結海域（次の手番に浜へ届くセル）まで進む。
        let mut world = naval_world(&["H.~~~~~~~~~~,C.."]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 0, 13, 0);
        let lander = spawn(&mut world, 1, UnitType::Lander, 3, 0);
        load(&mut world, lander, UnitType::Infantry, 1);
        spawn(&mut world, 2, UnitType::Tank, 15, 0);

        let (entity, command) = decide(&mut world).unwrap();
        assert_eq!(entity, lander);
        let AiCommand::Wait { target_pos } = command else {
            panic!("揚陸地点へ進むはず: {command:?}");
        };
        assert!(
            (7..=11).contains(&target_pos.x),
            "次の手番に浜へ届く集結海域へ進む: {target_pos:?}"
        );
    }

    #[test]
    fn single_lander_waits_offshore_until_the_wave_can_beat_the_beach_guard() {
        // 浜（浅瀬x=6, y=0..1の2か所）の近くに敵の戦車。歩兵2体の輸送船1隻は、守りに届く
        // 波（2隻目）がそろうまで降ろさない。
        let mut world = naval_world(&["H.~~~~,C..", "..~~~~,..."]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 0, 7, 0);
        let lander = spawn(&mut world, 1, UnitType::Lander, 3, 0);
        load(&mut world, lander, UnitType::Infantry, 1);
        load(&mut world, lander, UnitType::Infantry, 1);
        spawn(&mut world, 2, UnitType::Tank, 8, 0);
        spawn(&mut world, 2, UnitType::Tank, 8, 1);

        let (entity, command) = decide(&mut world).unwrap();
        assert_eq!(entity, lander);
        assert!(
            matches!(command, AiCommand::Wait { target_pos } if target_pos.x < 6),
            "浜へ降ろさず沖で待つ: {command:?}"
        );
        let plans = world.resource::<AssaultPlans>();
        assert!(!plans.0[&PlayerId(1)].launch);
    }

    #[test]
    fn single_berth_beach_launches_with_one_lander() {
        // 浜は浅瀬(x=6)の1か所だけ。輸送船1隻で波がそろうので、軽い守りなら降ろす。
        let mut world = naval_world(&["H.~~~~,C.."]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 0, 7, 0);
        let lander = spawn(&mut world, 1, UnitType::Lander, 3, 0);
        load(&mut world, lander, UnitType::TankZ, 1);
        spawn(&mut world, 2, UnitType::Infantry, 9, 0);

        let (_, command) = decide(&mut world).unwrap();
        assert!(
            matches!(command, AiCommand::Drop { transport_target_pos, .. } if transport_target_pos.x == 6),
            "浜へ降ろす: {command:?}"
        );
        assert!(world.resource::<AssaultPlans>().0[&PlayerId(1)].launch);
    }

    #[test]
    fn heavy_tank_heads_for_the_enemy_capital_island() {
        // 近くの島(x=5..6)には敵の歩兵1体、遠くの島(x=13..15)に敵首都(14,0)。重戦車は
        // 敵首都の島へ向かう。
        let mut world = naval_world(&["H.~~,..~~~~~,.H.", "..~~~..~~~~~~...", "~~~~~~~~~~~~~~~~"]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 2, 14, 0);
        spawn(&mut world, 2, UnitType::Infantry, 6, 0);
        spawn(&mut world, 2, UnitType::Infantry, 14, 1);
        let lander = spawn(&mut world, 1, UnitType::Lander, 2, 1);
        load(&mut world, lander, UnitType::TankZ, 1);

        let board = PlayerBoard::observe(&mut world, PlayerId(1));
        let context = SealiftContext::observe(&mut world, PlayerId(1), &HashSet::default());
        let transport = board
            .units
            .iter()
            .find(|unit| unit.entity == lander)
            .unwrap();
        let (landmass, _) = context.destination(transport).unwrap();
        let capital_island = context
            .landmasses
            .land
            .at(GridPosition { x: 14, y: 0 })
            .unwrap();
        assert_eq!(landmass, capital_island);
    }

    #[test]
    fn assault_prefers_the_beach_where_more_landers_can_unload() {
        // 右の島(x=5..9)の中央に敵首都(7,1)。西は浅瀬1か所(4,1)、東は浅瀬3か所(x=10)。
        // 首都までの距離と守りは同じなので、1手番に多く降ろせる東の浜を主な浜に選ぶ。
        let mut world = naval_world(&[
            "H.~~~.....,~~",
            "..~~,..H..,~~",
            "~~~~~.....,~~",
            "~~~~~~~~~~~~~",
        ]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 2, 7, 1);
        for y in 0..3 {
            let lander = spawn(&mut world, 1, UnitType::Lander, 2, 2 + y / 2);
            load(&mut world, lander, UnitType::TankZ, 1);
        }
        spawn(&mut world, 2, UnitType::Infantry, 7, 1);

        decide(&mut world);
        let plan = &world.resource::<AssaultPlans>().0[&PlayerId(1)];
        assert!(
            plan.primary.contains(&(10, 1)),
            "主な浜は東: {:?}",
            plan.primary
        );
        assert!(
            !plan.primary.contains(&(4, 1)),
            "西の1か所の浜を主な浜にしない: {:?}",
            plan.primary
        );
    }

    #[test]
    fn assault_lands_on_every_comparable_beach_of_the_island() {
        // 右の島(x=5..9)の中央に敵首都(7,0)。西の浅瀬(x=4)と東の浅瀬(x=10)は首都から同じ
        // 距離で評価が並ぶので、作戦の停泊セルに両方の浜を含めて同時に降ろす。
        let mut world = naval_world(&["H.~~,..H..,~~", "~~~~~~~~~~~~~"]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 2, 7, 0);
        let lander = spawn(&mut world, 1, UnitType::Lander, 2, 1);
        load(&mut world, lander, UnitType::TankZ, 1);
        spawn(&mut world, 2, UnitType::Infantry, 7, 0);

        decide(&mut world);
        let plan = &world.resource::<AssaultPlans>().0[&PlayerId(1)];
        assert!(plan.stops.contains(&(4, 0)), "西の浜: {:?}", plan.stops);
        assert!(plan.stops.contains(&(10, 0)), "東の浜: {:?}", plan.stops);
    }

    #[test]
    fn two_landers_launch_together_against_a_light_guard() {
        // 浜（浅瀬x=6, y=0..1）の近くに敵の歩兵1体。輸送船2隻（歩兵4体）で一斉に降ろす。
        let mut world = naval_world(&["H.~~~~,C..", "..~~~~,..."]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 0, 7, 0);
        for y in 0..2 {
            let lander = spawn(&mut world, 1, UnitType::Lander, 3, y);
            load(&mut world, lander, UnitType::Infantry, 1);
            load(&mut world, lander, UnitType::Infantry, 1);
        }
        spawn(&mut world, 2, UnitType::Infantry, 9, 1);

        let (_, command) = decide(&mut world).unwrap();
        assert!(
            matches!(command, AiCommand::Drop { transport_target_pos, .. } if transport_target_pos.x == 6),
            "浜へ降ろす: {command:?}"
        );
        assert!(world.resource::<AssaultPlans>().0[&PlayerId(1)].launch);
    }

    #[test]
    fn empty_lander_waits_at_shore_for_stranded_infantry() {
        // 自軍の島は全て確保済み。歩兵は取り残されている。輸送船は浅瀬で待つ。
        let mut world = naval_world(&["H..,~~~~,C."]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 0, 9, 0);
        let infantry = spawn(&mut world, 1, UnitType::Infantry, 1, 0);
        let lander = spawn(&mut world, 1, UnitType::Lander, 5, 0);

        let (entity, command) = decide(&mut world).unwrap();
        assert_eq!(entity, lander);
        let AiCommand::Wait { target_pos } = command else {
            panic!("乗船地点へ向かうはず: {command:?}");
        };
        assert_eq!(target_pos, GridPosition { x: 3, y: 0 });

        // 輸送船が行動を終えたら、歩兵が乗り込む。
        world.entity_mut(lander).insert(GridPosition { x: 3, y: 0 });
        world.entity_mut(lander).insert(ActionCompleted(true));
        world.entity_mut(lander).insert(HasMoved(true));
        let (entity, command) = decide(&mut world).unwrap();
        assert_eq!(entity, infantry);
        assert!(matches!(
            command,
            AiCommand::Load { transport_entity, .. } if transport_entity == lander
        ));
    }

    #[test]
    fn stranded_infantry_rides_in_a_recon_instead_of_taking_a_lander_slot() {
        // 輸送船は浅瀬(x=3)で乗船を待つ。歩兵は輸送船の枠を使わず、装甲車へ乗る。
        let mut world = naval_world(&["H..,~~~~,C."]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 0, 9, 0);
        let lander = spawn(&mut world, 1, UnitType::Lander, 3, 0);
        world.entity_mut(lander).insert(ActionCompleted(true));
        world.entity_mut(lander).insert(HasMoved(true));
        let infantry = spawn(&mut world, 1, UnitType::Infantry, 1, 0);
        let recon = spawn(&mut world, 1, UnitType::Recon, 2, 0);

        let (entity, command) = decide(&mut world).unwrap();
        assert_eq!(entity, infantry);
        assert!(
            matches!(command, AiCommand::Load { transport_entity, .. } if transport_entity == recon),
            "歩兵は装甲車へ乗る: {command:?}"
        );

        // 歩兵を載せた装甲車は、次の手番に輸送船へ乗る。
        world.entity_mut(infantry).insert((
            Transporting(recon),
            GridPosition { x: 9999, y: 9999 },
            ActionCompleted(true),
        ));
        world
            .get_mut::<crate::components::CargoCapacity>(recon)
            .unwrap()
            .loaded
            .push(infantry);
        let (entity, command) = decide(&mut world).unwrap();
        assert_eq!(entity, recon);
        assert!(matches!(
            command,
            AiCommand::Load { transport_entity, .. } if transport_entity == lander
        ));
    }

    #[test]
    fn heavy_tank_boards_before_light_tank() {
        // 輸送船の枠は1枠あたりの戦力が大きい部隊から使う。
        let mut world = naval_world(&["H..,~~~~,C."]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 2, 9, 0);
        let lander = spawn(&mut world, 1, UnitType::Lander, 3, 0);
        world.entity_mut(lander).insert(ActionCompleted(true));
        world.entity_mut(lander).insert(HasMoved(true));
        spawn(&mut world, 1, UnitType::Tank, 2, 0);
        let medium = spawn(&mut world, 1, UnitType::MdTank, 1, 0);

        let (entity, command) = decide(&mut world).unwrap();
        assert_eq!(entity, medium, "中戦車が先に乗る: {command:?}");
        assert!(matches!(
            command,
            AiCommand::Load { transport_entity, .. } if transport_entity == lander
        ));
    }

    #[test]
    fn lander_lands_a_recon_carrying_infantry_next_to_a_neutral_city() {
        // 歩兵を載せた装甲車は占領役として、中立都市の島へ降ろす。
        let mut world = naval_world(&["H.~~~,C."]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 0, 6, 0);
        let lander = spawn(&mut world, 1, UnitType::Lander, 3, 0);
        let recon = load(&mut world, lander, UnitType::Recon, 1);
        load(&mut world, recon, UnitType::Infantry, 1);

        let (entity, command) = decide(&mut world).unwrap();
        assert_eq!(entity, lander);
        assert!(
            matches!(command, AiCommand::Drop { cargo_entity, .. } if cargo_entity == recon),
            "装甲車を降ろす: {command:?}"
        );
    }

    #[test]
    fn stranded_unit_walks_around_an_inlet_to_the_boarding_shoal() {
        // 乗船できる浅瀬(3,0)は入り江の向こう側。直線距離では(0,0)が最も近いが、
        // 陸路で回り込んで浅瀬へ近づく。
        let mut world = naval_world(&[".~~,~~~C", ".~~...~~", "......~~"]);
        property(&mut world, 0, 7, 0);
        let infantry = spawn(&mut world, 1, UnitType::Infantry, 0, 0);
        let lander = spawn(&mut world, 1, UnitType::Lander, 4, 0);
        world.entity_mut(lander).insert(ActionCompleted(true));
        world.entity_mut(lander).insert(HasMoved(true));

        let (entity, command) = decide(&mut world).unwrap();
        assert_eq!(entity, infantry);
        assert!(
            matches!(command, AiCommand::Wait { target_pos } if target_pos == GridPosition { x: 1, y: 2 }),
            "入り江を回り込む: {command:?}"
        );
    }

    #[test]
    fn no_transport_activity_without_overseas_demand() {
        // 全拠点が自軍。運ぶ理由が無い。
        let mut world = naval_world(&["H..,~~~~,C."]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 1, 9, 0);
        spawn(&mut world, 1, UnitType::Infantry, 1, 0);
        spawn(&mut world, 1, UnitType::Lander, 5, 0);
        assert!(decide(&mut world).is_none());
    }

    #[test]
    fn helicopter_returns_to_airport_before_fuel_runs_out() {
        let mut world = naval_world(&["A.......C"]);
        property(&mut world, 1, 0, 0);
        property(&mut world, 0, 8, 0);
        let heli = spawn(&mut world, 1, UnitType::TransportHelicopter, 6, 0);
        world
            .get_mut::<crate::components::Fuel>(heli)
            .unwrap()
            .current = 8;

        let (entity, command) = decide(&mut world).unwrap();
        assert_eq!(entity, heli);
        let AiCommand::Wait { target_pos } = command else {
            panic!("空港へ戻るはず: {command:?}");
        };
        assert_eq!(target_pos, GridPosition { x: 0, y: 0 });
    }
}
