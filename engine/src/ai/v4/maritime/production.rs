//! 島で隔てられた盤面での生産の補正。
//!
//! 作戦生産は陸上の作戦単位で枠を逆算するため、島で隔てられた盤面では
//! 本拠島に地上部隊が溜まり、港・空港が空いたまま資金が積み上がる。ここでは
//! 次の2つを補う。
//!
//! - [`rejects_ground_purchase`] — 仕事の無い陸塊で、輸送能力を超えて地上部隊を
//!   買い足す命令を見送る（運べない部隊は戦局に寄与しない）。
//! - [`decide_supplementary_production`] — 作戦生産が使い切らなかった資金で、
//!   取り残された部隊を運ぶ輸送部隊と、海上戦域に寄与する戦艦を買う。

use super::landmass::Landmasses;
use super::sealift::{is_ground_carrier, is_sealift_transport};
use super::theater::MaritimeTheater;
use crate::ai::context::board::PlayerBoard;
use crate::collections::HashSet;
use crate::components::{GridPosition, PlayerId};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{MatchState, MovementType, Players, Terrain, UnitRegistry, UnitType};
use crate::systems::production::can_produce_at;
use bevy_ecs::prelude::*;

/// 取り残された部隊を、輸送部隊の全積載枠の何倍まで本拠島に溜めてよいか。
/// 地上部隊は毎手番すぐに買えるので、次の便で運べる分だけ待たせれば足りる。
const STRANDED_PER_TRANSPORT_SLOT: u32 = 1;
/// 輸送部隊がまだ揃っていなくても、本拠島に溜めてよい取り残された部隊の数。
/// 輸送部隊が届いたときに積荷が無い状態を避ける。
const MIN_STRANDED_BACKLOG: u32 = 2;
/// 戦艦の上限を1隻引き上げる余剰資金（戦艦の価格の何隻分か）。
const SURPLUS_PER_EXTRA_BATTLESHIP: u32 = 3;
/// 余剰資金で揃える航空戦闘部隊の上限（敵航空機の数を上乗せする）。
const MAX_AIR_COMBAT_UNITS: u32 = 6;
/// 作戦生産に輸送船の買い足しを許す上限。
const MAX_LANDERS: u32 = 6;
/// 余剰資金（輸送船3隻分ごとに1隻）で引き上げる輸送部隊の上限の最大幅。
const MAX_EXTRA_SEALIFT_TRANSPORTS: u32 = 4;
/// 輸送船の積載枠のうち、歩兵を載せた装甲車に回す割合（%）。
const CARRIER_SLOT_PERCENT: u32 = 50;
/// 同時に保有する輸送部隊（輸送船・輸送ヘリ）の上限。
const MAX_SEALIFT_TRANSPORTS: u32 = 6;

/// 調整中の上限（TODO: 調整後に定数へ戻す）。
fn tuned(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// 島嶼間輸送の需給。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SealiftStatus {
    /// 取り残された占領役（歩兵・重歩兵）。
    pub(crate) stranded_capturers: u32,
    /// 取り残されたその他の地上部隊。
    pub(crate) stranded_others: u32,
    /// 輸送船の数。
    pub(crate) landers: u32,
    /// 輸送ヘリの数。
    pub(crate) helicopters: u32,
    /// 占領役を載せられる全積載枠。
    pub(crate) capturer_slots: u32,
    /// 占領役以外も載せられる全積載枠（輸送船）。
    pub(crate) heavy_slots: u32,
    /// 他の陸塊に、運ぶ理由（未所有拠点・敵）があるか。
    pub(crate) overseas_demand: bool,
    /// 取り残された占領役のうち、装甲車に載っていない（地図上に居る）数。
    pub(crate) loose_capturers: u32,
    /// 取り残された、空の装甲車の数。
    pub(crate) empty_carriers: u32,
    /// 取り残された、占領役を載せた装甲車の数。
    pub(crate) loaded_carriers: u32,
}

impl SealiftStatus {
    pub(crate) fn observe(
        board: &PlayerBoard,
        landmasses: &Landmasses,
        player_id: PlayerId,
    ) -> Self {
        let mut status = Self {
            overseas_demand: landmasses
                .info
                .values()
                .any(|info| info.capture_need() > 0 || info.needs_combat()),
            ..Self::default()
        };
        for unit in board
            .units
            .iter()
            .filter(|unit| unit.owner == player_id && unit.is_deployed())
        {
            if is_sealift_transport(&unit.stats) {
                status.capturer_slots += unit.stats.max_cargo;
                match unit.stats.unit_type {
                    UnitType::Lander => {
                        status.landers += 1;
                        status.heavy_slots += unit.stats.max_cargo;
                    }
                    _ => status.helicopters += 1,
                }
            } else if landmasses.is_stranded(unit) {
                if is_ground_carrier(unit) {
                    // 歩兵を載せた装甲車は輸送船の1枠に占領役を運ぶ。空の装甲車は歩兵を
                    // 載せて1枠に収まるので、積載枠の需給に数えない。
                    match unit.cargo {
                        Some(cargo) if cargo.loaded > 0 => {
                            status.stranded_capturers += 1;
                            status.loaded_carriers += 1;
                        }
                        _ => status.empty_carriers += 1,
                    }
                } else if unit.stats.can_capture {
                    status.stranded_capturers += 1;
                    status.loose_capturers += 1;
                } else {
                    status.stranded_others += 1;
                }
            }
        }
        status
    }

    fn transports(&self) -> u32 {
        self.landers + self.helicopters
    }
}

/// 作戦生産の購入が、戦局に寄与しない部隊を溜めるだけになるか。
///
/// - 地上部隊: 生産施設の陸塊に仕事（未所有拠点・敵）が無く、既に取り残された部隊が
///   輸送能力を超えている場合に見送る。占領役以外は輸送船（港）が無い限り陸塊を出られない。
/// - 輸送船: 既に十分な数を持っていれば見送り、港を戦艦の生産へ回す。
/// - 戦艦: 揚陸作戦の火力が足りている、または港で回せる補給を超えるなら見送る。
pub(crate) fn rejects_ground_purchase(
    world: &mut World,
    player_id: PlayerId,
    command: &ProduceUnitCommand,
) -> bool {
    let Some(stats) = world
        .get_resource::<UnitRegistry>()
        .and_then(|registry| registry.get_stats(command.unit_type))
        .cloned()
    else {
        return false;
    };
    let lander = stats.unit_type == UnitType::Lander;
    let battleship = stats.unit_type == UnitType::Battleship;
    // 地図が無ければ陸塊を判定できないので見送らない（盤面の無い生産計画の検証など）。
    if (!lander
        && !battleship
        && matches!(stats.movement_type, MovementType::Air | MovementType::Ship))
        || !world.contains_resource::<crate::resources::Map>()
    {
        return false;
    }
    let board = PlayerBoard::observe(world, player_id);
    let landmasses = Landmasses::observe(&board, player_id);
    if battleship {
        // 戦艦は港でしか補給できない。揚陸作戦の火力が足りている、または港の数で回せる
        // 補給を超える戦艦は撃てないので見送る（[`super::force_balance`]）。
        let theater = MaritimeTheater::observe(&board, player_id);
        let supply_cap = theater.own_ports.len() * super::force_balance::BATTLESHIPS_PER_PORT;
        let not_needed = super::force_balance::LandingForceBalance::observe(
            world,
            &board,
            &landmasses,
            player_id,
        )
        .is_some_and(|balance| !balance.battleship_needed());
        return (not_needed && !theater.sea_contested())
            || theater.own_battleships as usize >= supply_cap;
    }
    if lander {
        // 港は戦艦と輸送船で取り合う。輸送船が揃った後も買い足すと戦艦を出せない。
        // 制海権を失っている間は、港の沖の敵の戦艦に沈められるので買わない。
        return MaritimeTheater::observe(&board, player_id).sea_contested()
            || SealiftStatus::observe(&board, &landmasses, player_id).landers
                >= tuned("OPENWARS_MAX_LANDERS", MAX_LANDERS);
    }
    let site = GridPosition {
        x: command.target_x,
        y: command.target_y,
    };
    let Some((_, info)) = landmasses.at(site) else {
        return false;
    };
    let site_has_work = if stats.can_capture {
        !info.unowned.is_empty() || info.enemy_ground > 0
    } else {
        info.needs_combat()
    };
    if site_has_work {
        return false;
    }
    let status = SealiftStatus::observe(&board, &landmasses, player_id);
    let theater = MaritimeTheater::observe(&board, player_id);
    // 制海権を失っている間は、運べない陸塊の地上部隊を買わない。輸送船は港の沖の敵の戦艦に
    // 沈められ、溜めた部隊は運べない。資金を戦艦へ回して制海権を取り戻す。
    if theater.sea_contested() {
        return true;
    }
    // 揚陸作戦の間は、浜を確保する戦闘部隊を運ぶ分だけ買う。
    let assault_active = world
        .get_resource::<super::sealift::AssaultPlans>()
        .is_some_and(|plans| plans.0.contains_key(&player_id));
    let heavy_percent = match super::sealift::assault_tuning().heavy_backlog_percent {
        Some(percent) if assault_active => percent,
        _ => tuned("OPENWARS_HEAVY_BACKLOG_PERCENT", 100),
    };
    let stranded = status.stranded_capturers + status.stranded_others;
    let total_cap = (STRANDED_PER_TRANSPORT_SLOT * status.capturer_slots).max(MIN_STRANDED_BACKLOG);
    if tuned("OPENWARS_ASSAULT_SPLIT", 0) != 0 {
        // 待機させる地上部隊は合計で輸送部隊の積載枠まで。揚陸作戦の間は、積載枠を
        // 戦闘部隊と占領役へ決まった割合で分ける（歩兵だけで本拠島を埋めない）。
        let heavy_cap = (status.heavy_slots * heavy_percent / 100).max(MIN_STRANDED_BACKLOG / 2);
        if stranded >= total_cap {
            return true;
        }
        return if stats.can_capture {
            assault_active
                && heavy_percent > 0
                && !theater.own_ports.is_empty()
                && status.stranded_capturers >= total_cap.saturating_sub(heavy_cap)
        } else {
            theater.own_ports.is_empty() || status.stranded_others >= heavy_cap
        };
    }
    if stats.can_capture {
        // 揚陸作戦で戦闘役（戦車）が要る間は、積載枠の半分を戦闘役のために空けておく。
        let combat_needed = super::force_balance::LandingForceBalance::observe(
            world,
            &board,
            &landmasses,
            player_id,
        )
        .is_some_and(|balance| balance.ground_combat_needed());
        let capturer_cap = if combat_needed {
            (total_cap / 2).max(MIN_STRANDED_BACKLOG)
        } else {
            total_cap
        };
        stranded >= capturer_cap
    } else if theater.own_ports.is_empty() {
        // 輸送船を持てない盤面では、占領役以外の地上部隊は本拠島から出られない。
        true
    } else {
        status.stranded_others
            >= (status.heavy_slots * heavy_percent / 100).max(MIN_STRANDED_BACKLOG / 2)
    }
}

/// 次に補う1件の生産命令。補う必要が無ければNone。
///
/// `produced_sites` はこの手番に生産済み・生産に失敗した施設で、候補から除く。
pub(crate) fn decide_supplementary_production(
    world: &mut World,
    player_id: PlayerId,
    produced_sites: &HashSet<(usize, usize)>,
) -> Option<ProduceUnitCommand> {
    let funds = world
        .get_resource::<Players>()?
        .0
        .iter()
        .find(|player| player.id == player_id)?
        .funds;
    let registry = world.get_resource::<UnitRegistry>()?.clone();
    let cost_of = |unit_type: UnitType| {
        registry
            .get_stats(unit_type)
            .map_or(u32::MAX, |stats| stats.cost)
    };
    let cheapest = cost_of(UnitType::TransportHelicopter)
        .min(cost_of(UnitType::Lander))
        .min(cost_of(UnitType::Battleship));
    if funds < cheapest {
        return None;
    }
    let turn = world
        .get_resource::<MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let board = PlayerBoard::observe(world, player_id);
    // 作戦生産は未発行の将来購入（最長で十数手番先）の費用を全額取り置く。手番を追うごとに
    // 収入が入るため、取り置きの全額を守ると資金が積み上がるだけになる。ここでは直近の購入に
    // 備えて、取り置きのうち1手番の収入までだけを残す。
    let income = income_per_turn(&board, player_id);
    let reserved = world
        .get_resource::<crate::ai::v4::trace::ProductionTraceDiagnostics>()
        .and_then(|diagnostics| diagnostics.reserved_funds(player_id, turn))
        .unwrap_or(0)
        .min(income);
    let spendable = funds.saturating_sub(reserved);
    let landmasses = Landmasses::observe(&board, player_id);
    let status = SealiftStatus::observe(&board, &landmasses, player_id);
    let theater = MaritimeTheater::observe(&board, player_id);

    let mut wishes = Vec::new();
    // 戦艦が役立つ盤面で資金が大きく余っている場合は、余剰に応じて上限を引き上げる。
    // 使い道の無い資金は戦局に寄与しないため、射程へ捉えられる敵がいる限り艦隊へ回す。
    let surplus_per_battleship = tuned("OPENWARS_BS_SURPLUS", SURPLUS_PER_EXTRA_BATTLESHIP)
        .saturating_mul(cost_of(UnitType::Battleship))
        .max(1);
    // 戦艦は港でしか補給できない。港の数で回せる補給の量を超えて持っても撃てない。
    let supply_cap = (theater.own_ports.len() * super::force_balance::BATTLESHIPS_PER_PORT) as u32;
    let battleship_cap = match theater.battleship_cap() {
        0 => 0,
        cap => cap
            .saturating_add(spendable / surplus_per_battleship)
            .min(supply_cap),
    };
    // 輸送船の積載枠のうち、歩兵を載せた装甲車に回す分。残りは戦車などの重戦闘部隊に使う。
    let carrier_cap = (status.heavy_slots * CARRIER_SLOT_PERCENT / 100).max(1);
    // 揚陸作戦があれば、戦艦と戦車の要否を作戦の火力・地上戦力の需給から決める
    // （[`super::force_balance`]）。無ければ従来どおり盤面の艦船数と余剰資金で決める。
    let balance =
        super::force_balance::LandingForceBalance::observe(world, &board, &landmasses, player_id);
    // 制海権を失っている間は、揚陸作戦の需給に関わらず敵の戦艦に並ぶまで補う。
    let battleship_wanted = theater.battleship_cap() > 0
        && ((theater.sea_contested() && theater.own_battleships < supply_cap)
            || match &balance {
                Some(balance) => balance.battleship_needed(),
                None => theater.own_battleships < battleship_cap,
            });
    if battleship_wanted {
        wishes.push(UnitType::Battleship);
    }
    // 輸送部隊の上限は、戦艦が要らない（補充を終えた）間だけ余剰資金で引き上げる（港の
    // 生産枠を戦艦と取り合わない）。余った資金を、運べずに溜まった部隊の輸送力へ回す。
    let surplus_transports = if battleship_wanted {
        0
    } else {
        (spendable / cost_of(UnitType::Lander).saturating_mul(3).max(1))
            .min(MAX_EXTRA_SEALIFT_TRANSPORTS)
    };
    // 制海権を失っている間は、輸送船は港の沖で沈められるので買い足さない。
    if !theater.sea_contested()
        && let Some(transport) = transport_wish(&status, &theater, surplus_transports)
    {
        wishes.insert(0, transport);
    }
    if let Some(aircraft) = air_wish(&board, &theater, player_id, spendable, &cost_of) {
        wishes.push(aircraft);
    }
    // 戦艦で削り切れない作戦地域の敵を受け止める戦車。運べる分（輸送船の積載枠の空き）だけ、
    // 戦艦が要る間は戦艦1隻分の資金を残した余りで買う（港が埋まっている手番でも次の
    // 手番の戦艦を買えるようにする）。
    if let Some(balance) = &balance {
        let stranded = status.stranded_capturers + status.stranded_others;
        let reserve = if battleship_wanted {
            cost_of(UnitType::Battleship)
        } else {
            0
        };
        let chart = world
            .get_resource::<crate::resources::DamageChart>()
            .cloned()
            .unwrap_or_default();
        // 作戦地域の敵を受け止める必要があるか、戦艦2隻分を超えて資金が余っている（他に
        // 使い道が無い）なら、輸送船の積載枠のうち装甲車に回さない分まで重戦闘部隊を待たせる。
        let budget = spendable.saturating_sub(reserve);
        let surplus = budget >= cost_of(UnitType::Battleship).saturating_mul(2);
        let heavy_room = status.heavy_slots.saturating_sub(carrier_cap).max(1);
        let tank = ((balance.ground_combat_needed() || surplus)
            && status.stranded_others < heavy_room)
            .then(|| {
                balance.best_landing_combat_unit(
                    &board,
                    &chart,
                    &|unit_type| registry.get_stats(unit_type).cloned(),
                    budget,
                )
            })
            .flatten();
        if std::env::var("OPENWARS_ASSAULT_TRACE").is_ok() {
            eprintln!(
                "FORCE p{} T{turn} zone{} flow{} naval{} ashore{} bs{}/{} need_bs{} need_ground{} slots{}/{} tank{:?} funds{}",
                player_id.0,
                balance.zone_enemy,
                balance.enemy_flow,
                balance.naval_capacity,
                balance.ashore,
                balance.battleships,
                balance.battleship_limit,
                battleship_wanted,
                balance.ground_combat_needed(),
                stranded,
                status.heavy_slots,
                tank,
                spendable
            );
        }
        wishes.extend(tank);
    }
    // 輸送船で運ぶ歩兵を装甲車へ載せると、輸送船の1枠で2体を運べる。装甲車に載って
    // いない取り残された歩兵の数まで、装甲車を買い足す（歩兵1体につき装甲車1台）。
    // 本拠島で待つ装甲車は輸送船の積載枠の半分まで（残りの枠は戦車などの重戦闘部隊に使う）。
    // 輸送船の積載枠が足りている（待機が枠に収まる）間は、装甲車の分だけ歩兵の到着が遅れるので
    // 買わない。
    let slots_short =
        status.stranded_capturers + status.stranded_others >= status.heavy_slots.max(1);
    if status.overseas_demand
        && status.landers > 0
        && slots_short
        && status.loose_capturers > status.empty_carriers
        && status.empty_carriers + status.loaded_carriers < carrier_cap
    {
        wishes.push(UnitType::Recon);
    }
    let factories: Vec<GridPosition> = board
        .properties
        .iter()
        .filter(|property| {
            property.owner == Some(player_id)
                && matches!(property.terrain, Terrain::Factory | Terrain::Capital)
        })
        .map(|property| property.position)
        .collect();
    let master = world.get_resource::<MasterDataRegistry>()?.clone();
    for unit_type in wishes {
        if cost_of(unit_type) > spendable {
            continue;
        }
        let sites = match registry
            .get_stats(unit_type)
            .map(|stats| stats.movement_type)
        {
            Some(MovementType::Air) => &theater.own_airports,
            Some(MovementType::Ship) => &theater.own_ports,
            _ => &factories,
        };
        let site = sites.iter().copied().find(|site| {
            !produced_sites.contains(&(site.x, site.y))
                && can_produce_at(world, player_id, site.x, site.y, unit_type, &master).is_ok()
        });
        if let Some(site) = site {
            return Some(ProduceUnitCommand {
                player_id,
                target_x: site.x,
                target_y: site.y,
                unit_type,
            });
        }
    }
    None
}

/// 取り残された部隊を運ぶために買い足す輸送部隊。足りていればNone。
///
/// 占領役だけなら速く安い輸送ヘリ（空港）を、戦車など占領役以外が溜まっていれば
/// 輸送船（港）を選ぶ。
fn transport_wish(
    status: &SealiftStatus,
    theater: &MaritimeTheater,
    surplus_transports: u32,
) -> Option<UnitType> {
    if !status.overseas_demand
        || status.transports()
            >= tuned("OPENWARS_MAX_SEALIFT", MAX_SEALIFT_TRANSPORTS) + surplus_transports
    {
        return None;
    }
    let stranded = status.stranded_capturers + status.stranded_others;
    // 取り残された部隊の買い足しは積載枠で頭打ちにする（[`rejects_ground_purchase`]）。
    // 待機が積載枠に達したら輸送部隊を足さないと、輸送部隊を失った後に輸送力が戻らない。
    if stranded == 0 || stranded < status.capturer_slots {
        return None;
    }
    let heavy_backlog = status.stranded_others > status.heavy_slots;
    if heavy_backlog && !theater.own_ports.is_empty() {
        return Some(UnitType::Lander);
    }
    if status.stranded_capturers > status.capturer_slots.saturating_sub(status.heavy_slots)
        && !theater.own_airports.is_empty()
    {
        return Some(UnitType::TransportHelicopter);
    }
    (!theater.own_ports.is_empty()).then_some(UnitType::Lander)
}

/// 余剰資金で補う航空戦力。島で隔てられた盤面で、自軍の空港がある場合だけ買う。
///
/// 敵の航空戦力が地上・海上戦力に比べて大きければ戦闘機で制空し、そうでなければ
/// 陸塊を越えて地上部隊・艦船を叩ける爆撃機（資金が足りなければ戦闘ヘリ）を選ぶ。
fn air_wish(
    board: &PlayerBoard,
    theater: &MaritimeTheater,
    player_id: PlayerId,
    spendable: u32,
    cost_of: &dyn Fn(UnitType) -> u32,
) -> Option<UnitType> {
    if theater.own_airports.is_empty() || !theater.capitals_separated_by_sea {
        return None;
    }
    let mut enemy_air_value = 0_u32;
    let mut enemy_bomber_value = 0_u32;
    let mut enemy_surface_value = 0_u32;
    let mut own_air_combat = 0_u32;
    let mut enemy_air_units = 0_u32;
    for unit in board.units.iter().filter(|unit| unit.is_deployed()) {
        let is_air = unit.stats.movement_type == MovementType::Air;
        if unit.owner == player_id {
            if is_air && !is_sealift_transport(&unit.stats) {
                own_air_combat += 1;
            }
            continue;
        }
        if is_air {
            enemy_air_units += 1;
            enemy_air_value = enemy_air_value.saturating_add(unit.stats.cost);
            if matches!(
                unit.stats.unit_type,
                UnitType::Bomber | UnitType::HeavyFighter | UnitType::Fighter
            ) {
                enemy_bomber_value = enemy_bomber_value.saturating_add(unit.stats.cost);
            }
        } else {
            enemy_surface_value = enemy_surface_value.saturating_add(unit.stats.cost);
        }
    }
    if own_air_combat >= MAX_AIR_COMBAT_UNITS + enemy_air_units {
        return None;
    }
    let candidate =
        if enemy_air_value > 0 && enemy_air_value.saturating_mul(2) >= enemy_surface_value {
            // 爆撃機・戦闘機が主体なら重戦闘機、戦闘ヘリ・輸送ヘリが主体なら戦闘機で足りる。
            if enemy_bomber_value.saturating_mul(2) >= enemy_air_value {
                UnitType::HeavyFighter
            } else {
                UnitType::Fighter
            }
        } else if enemy_surface_value > 0 {
            UnitType::Bomber
        } else {
            return None;
        };
    if cost_of(candidate) <= spendable {
        Some(candidate)
    } else if candidate == UnitType::Bomber && cost_of(UnitType::Bcopters) <= spendable {
        Some(UnitType::Bcopters)
    } else {
        None
    }
}

/// 自軍所有の拠点から得る1手番あたりの収入。
fn income_per_turn(board: &PlayerBoard, player_id: PlayerId) -> u32 {
    board
        .properties
        .iter()
        .filter(|property| property.owner == Some(player_id))
        .map(|property| board.registry.landscape_income(property.terrain.as_str()))
        .fold(0_u32, u32::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theater(ports: bool, airports: bool) -> MaritimeTheater {
        let at = GridPosition { x: 0, y: 0 };
        MaritimeTheater {
            own_ports: if ports { vec![at] } else { Vec::new() },
            own_airports: if airports { vec![at] } else { Vec::new() },
            ..MaritimeTheater::default()
        }
    }

    #[test]
    fn stranded_capturers_are_lifted_by_helicopters_when_airport_exists() {
        let status = SealiftStatus {
            stranded_capturers: 4,
            overseas_demand: true,
            ..SealiftStatus::default()
        };
        assert_eq!(
            transport_wish(&status, &theater(true, true), 0),
            Some(UnitType::TransportHelicopter)
        );
        assert_eq!(
            transport_wish(&status, &theater(true, false), 0),
            Some(UnitType::Lander)
        );
    }

    #[test]
    fn stranded_tanks_need_landers() {
        let status = SealiftStatus {
            stranded_others: 3,
            helicopters: 1,
            capturer_slots: 2,
            overseas_demand: true,
            ..SealiftStatus::default()
        };
        assert_eq!(
            transport_wish(&status, &theater(true, true), 0),
            Some(UnitType::Lander)
        );
    }

    #[test]
    fn no_transport_without_demand_or_backlog() {
        let mut status = SealiftStatus {
            stranded_capturers: 1,
            capturer_slots: 2,
            overseas_demand: true,
            ..SealiftStatus::default()
        };
        assert_eq!(transport_wish(&status, &theater(true, true), 0), None);
        // 待機が積載枠に達したら（地上部隊の買い足しが止まる点）輸送部隊を足す。
        status.stranded_capturers = 2;
        assert!(transport_wish(&status, &theater(true, true), 0).is_some());
        status.stranded_capturers = 5;
        status.overseas_demand = false;
        assert_eq!(transport_wish(&status, &theater(true, true), 0), None);
    }
}
