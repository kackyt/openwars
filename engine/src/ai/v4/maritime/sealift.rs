//! 輸送船・輸送ヘリによる島嶼間の兵員輸送。
//!
//! 仕事の無い陸塊に取り残された地上部隊（[`Landmasses::is_stranded`]）を拾い、
//! 未所有の拠点や敵が残る陸塊へ降ろす。1回の呼び出しで1命令だけを返し、次の順で選ぶ。
//!
//! 1. 降車: 積荷を載せた輸送部隊が今手番に揚陸地点へ届くなら降ろす。
//! 2. 出発・航行: 積荷を載せた輸送部隊を目的の陸塊へ進める。今手番にまだ乗れる部隊が
//!    居る場合は、乗船を待ってから出る。
//! 3. 迎え: 空の輸送部隊を、取り残された部隊の近くの乗船地点へ向かわせる。
//! 4. 乗船: 行動を終えた輸送部隊へ、取り残された部隊を乗せる。
//! 5. 集結: まだ乗れない取り残された部隊を乗船地点の近くへ寄せる（生産施設は塞がない）。
//!
//! 輸送部隊は積載すると同じ手番に移動できず、積んだ部隊はその手番に降ろせない。
//! 輸送船は港か浅瀬からしか降ろせない。輸送ヘリは燃料が尽きると墜落するため、
//! 燃料が足りなくなる前に自軍の空港へ戻す。
//!
//! 輸送部隊の移動先は、資金換算の点数を足し引きせず、次の優先順で選ぶ。
//! 自軍の生産施設を塞がない → 次の敵手番に撃沈されない → 目的地へ着くまでの手番数が
//! 少ない → 被弾が少ない → 目的地に近い。被弾は同じ前進量の中での選び分けにだけ使い、
//! 撃沈されない限り揚陸地点への接近を止めない。

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
/// 敵首都のある陸塊へ占領役を届ける上乗せ。
const ENEMY_CAPITAL_BONUS: i64 = 2000;
/// 護衛の無い占領役を降ろすとき、敵の地上部隊を数える降車セルからの距離。
const DROP_DANGER_RADIUS: u32 = 2;
/// 揚陸地点を、積荷の目標からこの距離以内に絞る。
const LANDING_TARGET_RADIUS: u32 = 3;
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
        return None;
    }
    let context = SealiftContext::observe(world, player_id, skip);
    let decision = context.decide();
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
            });
        }
        for list in cargo.values_mut() {
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
            boarding_cache: RefCell::default(),
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

    /// 取り残された、輸送部隊に乗れる自軍の地上部隊。
    fn stranded(&self) -> Vec<&BoardUnit> {
        let mut units: Vec<_> = self
            .own_units()
            .filter(|unit| self.landmasses.is_stranded(unit))
            .collect();
        // 占領役を先に運ぶ（中立拠点の収入が最も早く戦局を変える）。
        units.sort_by_key(|unit| (!unit.stats.can_capture, unit.entity));
        units
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
            if has_room && boardable {
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
        // 4. 乗船。
        for unit in &stranded {
            if !self.fresh.contains(&unit.entity) {
                continue;
            }
            if let Some(command) = self.board_transport(unit, &transports) {
                return Some((unit.entity, command));
            }
        }
        // 5. 集結。生産施設を塞いでいる部隊から先に動かし、施設を空ける。
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
        let capturers = cargo.iter().filter(|cargo| cargo.stats.can_capture).count() as i64;
        let fighters = cargo.len() as i64 - capturers;
        let capture_need = i64::from(info.capture_need());
        let assigned_capturers = capturers.min(capture_need);
        let mut value = assigned_capturers * CAPTURE_SLOT_VALUE;
        if info.needs_combat() {
            // 占領枠を超えた歩兵も、敵の残る陸塊では戦力として働く。
            value += (fighters + capturers - assigned_capturers) * COMBAT_SLOT_VALUE;
        }
        if info.enemy_capital && capturers > 0 {
            value += ENEMY_CAPITAL_BONUS;
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
        if item.stats.can_capture && !info.unowned.is_empty() {
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
        let cargo = self.cargo.get(&transport.entity)?;
        let current = (transport.position.x, transport.position.y);
        let mut best: Option<(i64, HashSet<(usize, usize)>)> = None;
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
                .is_none_or(|(best_score, _)| score > *best_score)
            {
                best = Some((score, cells));
            }
        }
        let (_, cells) = best?;
        let field = self.distance_field(transport, &cells);
        self.step_toward(transport, &*field, false, threat)
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
                    // 港は陸塊の一部、浅瀬は陸塊に接していれば乗れる。
                    match terrain {
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
                let walk = map.distance(unit.position.x, unit.position.y, cell.0, cell.1);
                let turns = walk.div_ceil(unit.stats.max_movement.max(1));
                cells
                    .entry(cell)
                    .and_modify(|best| *best = (*best).min(turns))
                    .or_insert(turns);
            }
        }
        cells
    }

    /// 空の輸送部隊を、取り残された部隊を最も早く拾える乗船セルへ向かわせる。
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
            let turns = distance.div_ceil(movement).max(*walk_turns);
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
        let map = &self.board.map;
        let landmass = self.landmasses.land.at(unit.position)?;
        let boarding = self.boarding_cells(landmass);
        if boarding.is_empty() {
            return self.leave_production_site(unit);
        }
        let current = (unit.position.x, unit.position.y);
        // 乗船セルの隣で待つ。乗船セル（港）そのものや生産施設は塞がない。
        // 優先順: 生産施設を塞がない → 乗船セルに近い → 現在地に留まる。
        let cell = self
            .reachable(unit)
            .keys()
            .copied()
            .filter(|cell| self.is_free_for(unit, *cell))
            .min_by_key(|cell| {
                let distance = boarding
                    .iter()
                    .map(|target| map.distance(cell.0, cell.1, target.0, target.1))
                    .min()
                    .unwrap_or(0);
                (
                    self.production_sites.contains(cell),
                    distance.max(1),
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
        // 戦車は海へ入れないので、沖合で揚陸地点の1手前まで進む（留まらない）。
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
        assert_eq!(target_pos, GridPosition { x: 8, y: 0 });
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
