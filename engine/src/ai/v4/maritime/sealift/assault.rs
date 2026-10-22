//! 敵の地上部隊が守る陸塊への揚陸作戦（まとめて上陸する）。
//!
//! 輸送船を1隻ずつ揚陸地点へ送ると、降ろした2体は次の敵手番に各個撃破される。
//! 敵の地上部隊が居る陸塊へは、次の手順で一斉に上陸する。
//!
//! 1. 浜の選択: 陸塊へ降ろせる停泊セル（港・浅瀬）を近いものどうしで浜にまとめ、
//!    近くの未所有拠点（目標）の多さと、近くの敵地上戦力の少なさで浜を選ぶ。
//! 2. 集結: 積荷を載せた輸送船は、次の手番に浜へ届く海上のセル（集結海域）で待つ。
//! 3. 出撃: 今手番に浜へ届く輸送船（波）がそろい、上陸戦力が浜の守りを上回ったら、
//!    同じ手番に一斉に降ろす。戦艦は手番の初めに浜の守りを優先して撃つ。
//!
//! 出撃の条件は [`AssaultTuning`] にまとめる（評価で調整するため環境変数で上書きできる）。

use super::{Cell, SealiftContext};
use crate::ai::context::board::BoardUnit;
use crate::ai::v4::maritime::landmass::is_ground;
use crate::ai::v4::maritime::theater::sea_distances;
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, PlayerId};
use crate::resources::{MovementType, Terrain};
use crate::systems::movement::get_valid_movement_cost;
use bevy_ecs::prelude::*;
use std::sync::OnceLock;

/// 停泊セルを同じ浜にまとめる距離。
const BEACH_LINK_DISTANCE: u32 = 3;

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
    /// 浜の目標（未所有拠点）を数える、停泊セルからの距離。
    pub(crate) objective_radius: u32,
    /// 選んだ浜を別の浜へ変える、評価の比（%）。
    pub(crate) switch_percent: i64,
    /// 戦艦が浜の守りを撃つときの、損害価値の上乗せ（%）。
    pub(crate) beach_fire_bonus_percent: u32,
    /// 乗船地点を浜に近い港・浅瀬へ絞るか。
    pub(crate) embark: bool,
    /// 乗船地点の、浜への海路距離の最小からの許容幅。
    pub(crate) embark_slack: u32,
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
            objective_radius: read("OPENWARS_ASSAULT_OBJECTIVE_RADIUS", 6),
            switch_percent: read("OPENWARS_ASSAULT_SWITCH", 150),
            beach_fire_bonus_percent: read("OPENWARS_ASSAULT_FIRE_BONUS", 100),
            embark: read("OPENWARS_ASSAULT_EMBARK", 0u32) != 0,
            embark_slack: read("OPENWARS_ASSAULT_EMBARK_SLACK", 2),
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
    /// 浜の停泊セル（輸送船が止まって降ろすセル）。
    pub(crate) stops: Vec<Cell>,
    /// この判断をした手番。
    decided_turn: u32,
    /// この浜で集結を始めた手番（出撃したら次の集結の始まりに戻す）。
    since_turn: u32,
    /// この手番に一斉に降ろすか。
    pub(crate) launch: bool,
}

/// 浜の評価。
struct Beach {
    stops: Vec<Cell>,
    /// 停泊セルの近くの未所有拠点の重み（首都は3、他は1）。
    objective: i64,
    /// 停泊セルの近くの敵地上戦力（生産費×HP）。
    defense: i64,
    /// 停泊セルの近くに上陸済みの味方地上戦力。
    friendly: i64,
}

impl Beach {
    fn score(&self) -> i64 {
        let net = (self.defense - self.friendly).max(0);
        self.objective * 100_000 / (net + 5_000)
    }
}

/// 部隊の戦力（生産費×HP）。
fn strength(unit: &BoardUnit) -> i64 {
    i64::from(unit.stats.cost) * i64::from(unit.health.unwrap_or(100)) / 100
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
        let beaches = self.beaches(landmass);
        let best = beaches.iter().max_by_key(|beach| beach.score())?;
        // 前の浜が残っていれば、明らかに良い浜が無い限り変えない（集結をやり直さない）。
        let kept = previous.and_then(|previous| {
            beaches
                .iter()
                .find(|beach| beach.stops.iter().any(|stop| previous.stops.contains(stop)))
        });
        let (beach, since_turn) = match (kept, previous) {
            (Some(kept), Some(previous))
                if best.score() * 100 <= kept.score() * tuning.switch_percent =>
            {
                (kept, previous.since_turn)
            }
            _ => (best, turn),
        };
        // 1手番に降ろせるのは停泊セルの数の輸送船まで。積荷の強い輸送船から数える。
        let mut wave: Vec<i64> = self
            .assault_wave(landmass, &beach.stops)
            .into_iter()
            .map(|transport| self.cargo_strength(transport))
            .collect();
        wave.sort_unstable_by(|a, b| b.cmp(a));
        wave.truncate(beach.stops.len());
        let power: i64 = wave.iter().sum::<i64>() + beach.friendly;
        let waited = turn.saturating_sub(since_turn);
        let defended = beach.defense > beach.friendly;
        let launch = !wave.is_empty()
            && (!defended
                || beach.friendly * 100 >= tuning.ratio_percent * beach.defense
                || (wave.len() >= tuning.min_wave
                    && (power * 100 >= tuning.ratio_percent * beach.defense
                        || waited >= tuning.max_wait
                        || wave.len() >= beach.stops.len())));
        if std::env::var("OPENWARS_ASSAULT_TRACE").is_ok() {
            eprintln!(
                "ASSAULT p{} T{turn} land{landmass} stops{:?} obj{} def{} fr{} wave{} power{} waited{} launch{launch}",
                self.player_id.0,
                beach.stops,
                beach.objective,
                beach.defense,
                beach.friendly,
                wave.len(),
                power,
                waited
            );
        }
        Some(AssaultPlan {
            landmass,
            stops: beach.stops.clone(),
            decided_turn: turn,
            // 出撃したら、次の波の集結はこの手番から数え直す。
            since_turn: if launch { turn } else { since_turn },
            launch,
        })
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

    /// 陸塊 `landmass` へ輸送船が降ろせる停泊セルを、近いものどうし（3マス以内で連鎖）で
    /// まとめた浜ごとの評価。目標の無い浜は除く。
    fn beaches(&self, landmass: u32) -> Vec<Beach> {
        let map = &self.board.map;
        let registry = &self.board.registry;
        let mut stops: Vec<Cell> = Vec::new();
        for y in 0..map.height {
            for x in 0..map.width {
                let Some(terrain) = map.get_terrain(x, y) else {
                    continue;
                };
                if !matches!(terrain, Terrain::Shoal | Terrain::Port)
                    || get_valid_movement_cost(registry, MovementType::Ship, terrain).is_none()
                    || !map.get_adjacent(x, y).into_iter().any(|(nx, ny)| {
                        self.landmasses.land.at(GridPosition { x: nx, y: ny }) == Some(landmass)
                    })
                {
                    continue;
                }
                stops.push((x, y));
            }
        }
        // 近く（BEACH_LINK_DISTANCE以内）で連鎖する停泊セルを1つの浜にまとめる。
        let mut groups: Vec<Vec<Cell>> = Vec::new();
        for stop in stops {
            let linked: Vec<usize> = groups
                .iter()
                .enumerate()
                .filter(|(_, group)| {
                    group.iter().any(|other| {
                        map.distance(stop.0, stop.1, other.0, other.1) <= BEACH_LINK_DISTANCE
                    })
                })
                .map(|(index, _)| index)
                .collect();
            let mut merged = vec![stop];
            for index in linked.into_iter().rev() {
                merged.extend(groups.swap_remove(index));
            }
            merged.sort_unstable();
            groups.push(merged);
        }
        let tuning = tuning();
        let near = |stops: &[Cell], position: GridPosition, radius: u32| {
            stops
                .iter()
                .any(|stop| map.distance(stop.0, stop.1, position.x, position.y) <= radius)
        };
        let mut beaches: Vec<Beach> = groups
            .into_iter()
            .map(|stops| {
                let objective = self
                    .board
                    .properties
                    .iter()
                    .filter(|property| {
                        property.owner != Some(self.player_id)
                            && self.landmasses.land.at(property.position) == Some(landmass)
                            && near(&stops, property.position, tuning.objective_radius)
                    })
                    .map(|property| {
                        if property.terrain == Terrain::Capital {
                            3
                        } else {
                            1
                        }
                    })
                    .sum();
                let mut defense = 0;
                let mut friendly = 0;
                for unit in self.board.units.iter().filter(|unit| {
                    unit.is_deployed()
                        && is_ground(unit)
                        && self.landmasses.land.at(unit.position) == Some(landmass)
                        && near(&stops, unit.position, tuning.defense_radius)
                }) {
                    if unit.owner == self.player_id {
                        friendly += strength(unit);
                    } else {
                        defense += strength(unit);
                    }
                }
                Beach {
                    stops,
                    objective,
                    defense,
                    friendly,
                }
            })
            .filter(|beach| beach.objective > 0)
            .collect();
        beaches.sort_by_key(|beach| beach.stops.clone());
        beaches
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

    /// 乗船地点（港・浅瀬）を、揚陸作戦の浜へ海路で近いものに絞る。
    ///
    /// 取り残された部隊が島の端々の乗船地点へ散ると、輸送船は近くの数体だけを拾って
    /// 往復し、遠くの部隊（戦車・自走砲など）は運ばれない。浜へ近い乗船地点に集める。
    pub(super) fn embark_cells(&self, cells: Vec<Cell>) -> Vec<Cell> {
        let tuning = tuning();
        let Some(plan) = self.assault.as_ref().filter(|_| tuning.embark) else {
            return cells;
        };
        let field = sea_distances(&self.board, plan.stops.iter().copied());
        let Some(nearest) = cells
            .iter()
            .filter_map(|cell| field.get(cell))
            .min()
            .copied()
        else {
            return cells;
        };
        cells
            .into_iter()
            .filter(|cell| {
                field
                    .get(cell)
                    .is_some_and(|distance| *distance <= nearest + tuning.embark_slack)
            })
            .collect()
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
