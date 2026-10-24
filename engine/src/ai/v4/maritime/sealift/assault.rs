//! 敵の地上部隊が守る陸塊への揚陸作戦（まとめて上陸する）。
//!
//! 輸送船を1隻ずつ揚陸地点へ送ると、降ろした2体は次の敵手番に各個撃破される。
//! 敵の地上部隊が居る陸塊へは、次の手順で一斉に上陸する。
//!
//! 1. 浜の選択: 陸塊へ降ろせる停泊セル（港・浅瀬）を近いものどうしで浜にまとめ、
//!    上陸できるか・反撃の危険・進軍路で得られるもので浜を選ぶ（[`super::landing_site`]）。
//!    上陸済みの浜（橋頭堡）は、塞がれて上陸できなくならない限り使い続ける。
//! 2. 集結: 積荷を載せた輸送船は、次の手番に浜へ届く海上のセル（集結海域）で待つ。
//! 3. 出撃: 今手番に浜へ届く輸送船（波）がそろい、上陸戦力が浜の守りを上回ったら、
//!    同じ手番に一斉に降ろす。戦艦は手番の初めに浜の守りを優先して撃つ。
//!
//! 出撃の条件は [`AssaultTuning`] にまとめる（評価で調整するため環境変数で上書きできる）。

use super::landing_site::{LandingSite, strength};
use super::{Cell, SealiftContext};
use crate::ai::context::board::BoardUnit;
use crate::ai::v4::maritime::theater::sea_distances;
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, PlayerId};
use crate::resources::MovementType;
use crate::systems::combat::get_expected_damage;
use bevy_ecs::prelude::*;
use std::rc::Rc;
use std::sync::OnceLock;

/// 主な浜と同時に降ろす浜の、評価の下限（主な浜の評価に対する%）。
const ALSO_LAND_PERCENT: i64 = 25;
/// 浜へ降ろした1体を、次の敵手番に同時に攻撃できる敵の数の見込み。
const ATTACKERS_PER_LANDED_UNIT: usize = 3;
/// 乗船地点の選択で航行手番数を見積もる、輸送船の移動力。
const LANDER_MOVEMENT: u32 = 5;

/// 揚陸作戦の調整値。
#[derive(Debug, Clone)]
pub(crate) struct AssaultTuning {
    /// 揚陸作戦を使うか。
    pub(crate) enabled: bool,
    /// 出撃に必要な、上陸戦力の浜の守りに対する比（%）。
    pub(crate) ratio_percent: i64,
    /// 浜に守りがあるときの、1回の出撃の最少輸送船数。
    pub(crate) min_wave: usize,
    /// 集結を始めてから、戦力比に届かなくても出撃する手番数。
    pub(crate) max_wait: u32,
    /// 浜の守り・上陸済みの味方を数える、停泊セルからの距離。
    pub(crate) defense_radius: u32,
    /// 選んだ浜を別の浜へ変える、評価の比（%）。
    pub(crate) switch_percent: i64,
    /// 戦艦が浜の守りを撃つときの、損害価値の上乗せ（%）。
    pub(crate) beach_fire_bonus_percent: u32,
    /// 味方が上陸済みの浜（橋頭堡）を、他の浜の評価に関わらず続けて使うか。
    pub(crate) keep_beachhead: bool,
    /// 撃てない戦艦を浜の支援位置へ集めるか。
    pub(crate) battleship_support: bool,
    /// 揚陸作戦の間、占領役より戦闘部隊（重戦闘部隊が最優先）を先に積むか。
    pub(crate) combat_first: bool,
    /// 揚陸作戦の間、運ぶ戦闘部隊の購入を許す積載枠の割合（%）。None なら常の基準。
    pub(crate) heavy_backlog_percent: Option<u32>,
}

impl AssaultTuning {
    fn from_env() -> Self {
        fn read<T: std::str::FromStr>(key: &str, default: T) -> T {
            std::env::var(key)
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(default)
        }
        Self {
            enabled: read("OPENWARS_ASSAULT_ENABLED", 1u32) != 0,
            ratio_percent: read("OPENWARS_ASSAULT_RATIO", 150),
            min_wave: read("OPENWARS_ASSAULT_MIN_WAVE", 2),
            max_wait: read("OPENWARS_ASSAULT_MAX_WAIT", 3),
            defense_radius: read("OPENWARS_ASSAULT_DEFENSE_RADIUS", 3),
            switch_percent: read("OPENWARS_ASSAULT_SWITCH", 150),
            beach_fire_bonus_percent: read("OPENWARS_ASSAULT_FIRE_BONUS", 100),
            keep_beachhead: read("OPENWARS_ASSAULT_KEEP_BEACHHEAD", 0u32) != 0,
            battleship_support: read("OPENWARS_ASSAULT_BS_SUPPORT", 0u32) != 0,
            combat_first: read("OPENWARS_ASSAULT_COMBAT_FIRST", 1u32) != 0,
            heavy_backlog_percent: std::env::var("OPENWARS_ASSAULT_HEAVY_PERCENT")
                .ok()
                .and_then(|value| value.parse().ok()),
        }
    }
}

/// 揚陸作戦の調整値（プロセスで1回だけ読む）。
pub(crate) fn tuning() -> &'static AssaultTuning {
    static TUNING: OnceLock<AssaultTuning> = OnceLock::new();
    TUNING.get_or_init(AssaultTuning::from_env)
}

/// プレイヤーごとの揚陸作戦。戦艦の射撃（手番の初め）が浜の守りを優先するために残す。
#[derive(Resource, Debug, Clone, Default)]
pub(crate) struct AssaultPlans(pub(crate) HashMap<PlayerId, AssaultPlan>);

/// 1つの揚陸作戦。
#[derive(Debug, Clone)]
pub(crate) struct AssaultPlan {
    /// 上陸先の陸塊。
    pub(crate) landmass: u32,
    /// 上陸に使う全ての浜の停泊セル（輸送船が止まって降ろすセル）。
    pub(crate) stops: Vec<Cell>,
    /// 主な浜（評価で選んだ浜）の停泊セル。次の手番の浜の選択はこの浜を引き継ぐ。
    primary: Vec<Cell>,
    /// 浜を起点にした島内の首都攻略DAGの起点（降車セルのうち敵首都へ最も近いセル）。
    pub(crate) origin: Option<GridPosition>,
    /// この判断をした手番。
    decided_turn: u32,
    /// この浜で集結を始めた手番（出撃したら次の集結の始まりに戻す）。
    since_turn: u32,
    /// この手番に一斉に降ろすか。
    pub(crate) launch: bool,
}

impl SealiftContext {
    /// 前の手番までの作戦を引き継ぎ、この手番の揚陸作戦を決める。
    pub(super) fn plan_assault(
        &self,
        previous: Option<&AssaultPlan>,
        turn: u32,
    ) -> Option<AssaultPlan> {
        let tuning = tuning();
        if !tuning.enabled {
            return None;
        }
        // 同じ手番の中では、最初の判断（輸送船が動く前の波の構成）を保つ。
        if let Some(previous) = previous
            && previous.decided_turn == turn
        {
            return Some(previous.clone());
        }
        let landmass = self.assault_landmass()?;
        let sites = self.landing_sites(landmass);
        if std::env::var("OPENWARS_ASSAULT_TRACE").is_ok() {
            for site in &sites {
                eprintln!(
                    "SITE p{} T{turn} first{:?} n{} free{}/{} drop{} route{:?}/{} counter{} reinf{} fr{} value{} score{}",
                    self.player_id.0,
                    site.stops.first(),
                    site.stops.len(),
                    site.free_stops,
                    site.free_drop_cells,
                    site.drop_cells.len(),
                    site.route_steps,
                    site.route_properties,
                    site.counterattack,
                    site.reinforcement,
                    site.friendly,
                    site.value(),
                    site.score()
                );
            }
        }
        let (primary, since_turn) = self.choose_landing_site(&sites, previous, turn)?;
        // 主な浜のほか、上陸できて評価が主な浜の ALSO_LAND_PERCENT 以上ある浜にも同時に降ろす。
        // 1つの浜の少ない停泊セルだけに降ろすと、輸送船がその沖に固まって順番を待ち、
        // 揚陸の量が停泊セルの数で頭打ちになる。
        let mut stops: Vec<Cell> = sites
            .iter()
            .filter(|other| {
                other.stops == primary.stops
                    || (other.feasible()
                        && other.score() * 100 >= primary.score() * ALSO_LAND_PERCENT)
            })
            .flat_map(|other| other.stops.iter().copied())
            .collect();
        stops.sort_unstable();
        stops.dedup();
        // 出撃の判断は主な浜の守りで行い、出撃した波は同時に降ろす全ての浜の停泊セルへ降ろす。
        let site = primary;
        let enemy_occupied: HashSet<Cell> = self
            .board
            .units
            .iter()
            .filter(|unit| unit.owner != self.player_id && unit.is_deployed())
            .map(|unit| (unit.position.x, unit.position.y))
            .collect();
        let free_stops = stops
            .iter()
            .filter(|stop| !enemy_occupied.contains(stop))
            .count();
        // 1手番に降ろせるのは停泊セルの数の輸送船まで。積荷の強い輸送船から数える。
        let mut wave: Vec<(i64, &BoardUnit)> = self
            .assault_wave(landmass, &stops)
            .into_iter()
            .map(|transport| (self.cargo_strength(transport), transport))
            .collect();
        wave.sort_unstable_by_key(|(value, _)| std::cmp::Reverse(*value));
        wave.truncate(free_stops);
        // 上陸戦力は、降ろす積荷・上陸済みの味方・浜を撃てる戦艦の削り量の和。
        let power: i64 =
            wave.iter().map(|(value, _)| value).sum::<i64>() + site.friendly + site.naval_support;
        let waited = turn.saturating_sub(since_turn);
        // 浜の守りは、降ろした波が次の敵手番に受ける損害の見込み（[`Self::landing_exposure`]）。
        let landed: Vec<&BoardUnit> = wave
            .iter()
            .flat_map(|(_, transport)| self.droppable_cargo(transport))
            .collect();
        // 浜の守り（次の敵手番に降車セルを攻撃できる敵の生産費×HP）と近くの敵生産拠点からの
        // 増援で判断する。
        let defense = site.threat();
        let defended = defense > site.friendly;
        // 停泊セルが少ない浜では、波の最少数を停泊セルの数で頭打ちにする。
        let min_wave = tuning.min_wave.min(site.free_stops);
        let by_strength = !defended
            || site.friendly * 100 >= tuning.ratio_percent * defense
            || (wave.len() >= min_wave
                && (power * 100 >= tuning.ratio_percent * defense
                    // 待ちが長い・浜が満杯でも、守りの半分に届かない上陸はしない。
                    || (power * 2 >= defense
                        && (waited >= tuning.max_wait || wave.len() >= site.free_stops))));
        // 待ちが長い浜は、降ろした波が受ける損害の見込みで判断し直す。安い歩兵の群れで海岸線を
        // 埋められると、生産費の守りは大きく見えるが、重戦車にはほとんど傷を付けられない。
        let by_exposure = waited >= tuning.max_wait
            && wave.len() >= min_wave
            && power * 100 >= tuning.ratio_percent * self.landing_exposure(site, &landed);
        let launch = !wave.is_empty() && site.feasible() && (by_strength || by_exposure);
        if std::env::var("OPENWARS_ASSAULT_TRACE").is_ok() {
            eprintln!(
                "ASSAULT p{} T{turn} land{landmass} stops{:?} also{} free{} route{:?}/{} counter{} reinf{} fr{} naval{} score{} wave{} power{} waited{} launch{launch}",
                self.player_id.0,
                site.stops,
                stops.len(),
                site.free_stops,
                site.route_steps,
                site.route_properties,
                site.counterattack,
                site.reinforcement,
                site.friendly,
                site.naval_support,
                site.score(),
                wave.len(),
                power,
                waited
            );
        }
        Some(AssaultPlan {
            landmass,
            stops,
            primary: primary.stops.clone(),
            origin: primary.origin,
            decided_turn: turn,
            // 出撃したら、次の波の集結はこの手番から数え直す。
            since_turn: if launch { turn } else { since_turn },
            launch,
        })
    }

    /// 輸送船が今手番に降ろせる積荷の部隊。
    fn droppable_cargo<'a>(&'a self, transport: &BoardUnit) -> Vec<&'a BoardUnit> {
        self.cargo
            .get(&transport.entity)
            .into_iter()
            .flatten()
            .filter(|item| item.can_drop)
            .filter_map(|item| {
                self.board
                    .units
                    .iter()
                    .find(|unit| unit.entity == item.entity)
            })
            .collect()
    }

    /// 浜へ降ろした部隊 `landed` が、次の敵手番に受ける損害の見込み（資金換算）。
    ///
    /// 出撃の判断は、この損害の見込みを上陸戦力と比べる。浜の守りの各部隊は、降ろした部隊のうち最も大きな損害（生産費×損害率）を
    /// 与えられる部隊を攻撃するとみなす。1体を同時に攻撃できるのは [`ATTACKERS_PER_LANDED_UNIT`] 体
    /// までとし、損害の大きい守りから数える。生産費で比べると、重戦車にほとんど傷を
    /// 付けられない歩兵の群れを強い守りと見なしてしまう。近くの敵生産拠点からの増援の
    /// 見込みを足す。まだ降ろす部隊が無ければ、守りの戦力（生産費×HP）で見積もる。
    fn landing_exposure(&self, site: &LandingSite, landed: &[&BoardUnit]) -> i64 {
        if landed.is_empty() {
            return site.threat();
        }
        // 降車セルの地形効果は、最も守りの薄いセルで数える（控えめな見積もり）。
        let terrain_defense = site
            .drop_cells
            .iter()
            .filter_map(|cell| self.board.map.get_terrain(cell.x, cell.y))
            .map(|terrain| self.board.registry.get_terrain_defense_bonus(terrain))
            .min()
            .unwrap_or(0);
        let mut blows: Vec<i64> = site
            .defenders
            .iter()
            .filter_map(|entity| self.board.units.iter().find(|unit| unit.entity == *entity))
            .map(|defender| {
                landed
                    .iter()
                    .map(|target| {
                        let damage = get_expected_damage(
                            &defender.stats,
                            defender.health.unwrap_or(100),
                            defender
                                .ammo
                                .unwrap_or((defender.stats.max_ammo1, defender.stats.max_ammo2)),
                            &target.stats,
                            terrain_defense,
                            defender.stats.min_range.max(1),
                            &self.board.registry,
                            &self.chart,
                            false,
                        )
                        .min(target.health.unwrap_or(100));
                        i64::from(target.stats.cost) * i64::from(damage) / 100
                    })
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        blows.sort_unstable_by(|a, b| b.cmp(a));
        blows.truncate(landed.len() * ATTACKERS_PER_LANDED_UNIT);
        // 波の戦力を超える損害は数えない（全滅以上の損害は無い）。
        let wave_value: i64 = landed.iter().map(|unit| strength(unit)).sum();
        blows.iter().sum::<i64>().min(wave_value) + site.reinforcement
    }

    /// 輸送船が今手番に降ろせる積荷の戦力。
    fn cargo_strength(&self, transport: &BoardUnit) -> i64 {
        self.cargo
            .get(&transport.entity)
            .into_iter()
            .flatten()
            .filter(|item| item.can_drop)
            .map(|item| {
                self.board
                    .units
                    .iter()
                    .find(|unit| unit.entity == item.entity)
                    .map_or(i64::from(item.stats.cost), strength)
            })
            .sum()
    }

    /// 揚陸作戦の上陸先。敵の地上部隊が居る陸塊のうち、積荷を載せた輸送船が最も多く
    /// 向かっている陸塊。
    fn assault_landmass(&self) -> Option<u32> {
        let mut counts: HashMap<u32, u32> = HashMap::default();
        for transport in self.transports() {
            if transport.stats.movement_type != MovementType::Ship {
                continue;
            }
            let Some((landmass, _)) = self.destination(transport) else {
                continue;
            };
            if self
                .landmasses
                .info
                .get(&landmass)
                .is_some_and(|info| info.enemy_ground > 0)
            {
                *counts.entry(landmass).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .max_by_key(|(landmass, count)| {
                let capital = self
                    .landmasses
                    .info
                    .get(landmass)
                    .is_some_and(|info| info.enemy_capital);
                (*count, capital, std::cmp::Reverse(*landmass))
            })
            .map(|(landmass, _)| landmass)
    }

    /// 揚陸先の浜を選ぶ。選んだ浜と、その浜で集結を始めた手番を返す。
    ///
    /// 敵に塞がれた浜も候補に残す。塞いでいる敵の戦力は反撃の見込みとして評価に入り、
    /// 戦艦が浜の守りとして優先して撃つ。降車セルが空くまで出撃しない（`plan_assault`）。
    ///
    /// 1. 上陸済みの浜（首都攻略DAGの起点に接する浜）を使い続ける。
    /// 2. まだ上陸していなければ、評価（得られるもの÷反撃の危険）が最も良い浜を選ぶ。
    ///    前の手番の浜は、明らかに良い浜が無い限り変えない。
    fn choose_landing_site<'a>(
        &self,
        sites: &'a [LandingSite],
        previous: Option<&AssaultPlan>,
        turn: u32,
    ) -> Option<(&'a LandingSite, u32)> {
        let tuning = tuning();
        let map = &self.board.map;
        let continuing = |site: &LandingSite| {
            previous
                .filter(|previous| {
                    site.stops
                        .iter()
                        .any(|stop| previous.primary.contains(stop))
                })
                .map_or(turn, |previous| previous.since_turn)
        };
        // 1. 橋頭堡（首都攻略DAGの起点）。
        if let Some(origin) = self.beachhead_origins.first().copied() {
            let site = sites.iter().min_by_key(|site| {
                let distance = site
                    .drop_cells
                    .iter()
                    .map(|cell| map.distance(cell.x, cell.y, origin.x, origin.y))
                    .min()
                    .unwrap_or(u32::MAX);
                (distance, site.stops.clone())
            })?;
            return Some((site, continuing(site)));
        }
        // 2. 前の手番の浜を続けるか、より良い浜へ移る。
        let best = sites
            .iter()
            .max_by_key(|site| (site.score(), std::cmp::Reverse(site.stops.clone())))?;
        // 長い海岸線を分けた浜は重なり合うので、前の手番と同じ停泊セルの浜を優先して引き継ぐ。
        let kept = previous.and_then(|previous| {
            sites
                .iter()
                .find(|site| site.stops == previous.primary)
                .or_else(|| {
                    sites.iter().find(|site| {
                        site.stops
                            .iter()
                            .any(|stop| previous.primary.contains(stop))
                    })
                })
        });
        let site = match kept {
            Some(kept)
                if (tuning.keep_beachhead && kept.friendly > 0)
                    || best.score() * 100 <= kept.score() * tuning.switch_percent =>
            {
                kept
            }
            _ => best,
        };
        Some((site, continuing(site)))
    }

    /// 今手番に浜 `stops` へ届いて降ろせる、陸塊 `landmass` 行きの輸送船（波）。
    fn assault_wave(&self, landmass: u32, stops: &[Cell]) -> Vec<&BoardUnit> {
        self.transports()
            .into_iter()
            .filter(|transport| {
                transport.stats.movement_type == MovementType::Ship
                    && self.fresh.contains(&transport.entity)
                    && self
                        .cargo
                        .get(&transport.entity)
                        .is_some_and(|cargo| cargo.iter().any(|item| item.can_drop))
                    && self
                        .destination(transport)
                        .is_some_and(|(destination, _)| destination == landmass)
                    && {
                        let reachable = self.reachable(transport);
                        stops.iter().any(|stop| {
                            reachable.contains_key(stop) && self.is_free_for(transport, *stop)
                        })
                    }
            })
            .collect()
    }

    /// 輸送船が停止セル `stop` から陸塊 `landmass` へ降ろすのを、揚陸作戦が止めるか。
    pub(super) fn assault_holds(&self, transport: &BoardUnit, stop: Cell, landmass: u32) -> bool {
        self.assault.as_ref().is_some_and(|plan| {
            plan.landmass == landmass
                && transport.stats.movement_type == MovementType::Ship
                && !(plan.launch && plan.stops.contains(&stop))
        })
    }

    /// 乗船地点 `cell` から揚陸作戦の浜までの、輸送船の航行手番数。作戦が無ければ0。
    ///
    /// 取り残された部隊の乗船地点は、陸路の手番数とこの航行手番数の和で選ぶ。浜へ海路で
    /// 近い乗船地点でも、部隊が歩いて何手番もかかるなら、近くの港・浅瀬から乗る方が早い。
    pub(super) fn sail_turns(&self, cell: Cell) -> u32 {
        let Some(plan) = self.assault.as_ref() else {
            return 0;
        };
        let field = self
            .sail_cache
            .borrow_mut()
            .get_or_insert_with(|| Rc::new(sea_distances(&self.board, plan.stops.iter().copied())))
            .clone();
        field
            .get(&cell)
            .map_or(0, |distance| distance.div_ceil(LANDER_MOVEMENT))
    }

    /// 揚陸作戦の陸塊へ向かう輸送船の目的地。出撃前は集結海域、出撃後は浜の停泊セル。
    /// 揚陸作戦の対象外なら None。
    pub(super) fn assault_destination(
        &self,
        transport: &BoardUnit,
        landmass: u32,
    ) -> Option<HashSet<Cell>> {
        let plan = self.assault.as_ref()?;
        if plan.landmass != landmass || transport.stats.movement_type != MovementType::Ship {
            return None;
        }
        let stops: HashSet<Cell> = plan.stops.iter().copied().collect();
        if plan.launch {
            return Some(stops);
        }
        // 次の手番に浜へ届く、浜そのものではない海上のセル。
        let movement = transport.stats.max_movement.max(1);
        let staging: HashSet<Cell> = sea_distances(&self.board, stops.iter().copied())
            .into_iter()
            .filter(|(_, distance)| (1..=movement).contains(distance))
            .map(|(cell, _)| cell)
            .collect();
        Some(if staging.is_empty() { stops } else { staging })
    }
}
