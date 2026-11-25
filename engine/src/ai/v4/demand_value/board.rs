//! 盤面（`World`）から需要・購入候補・既存部隊の候補を組み立てる（計画書 Step 2、F_TRACE 用）。
//!
//! 需要は3種類に絞る（計画書 3.2 節の最小構成）。どれも価値はルール（ダメコン・地形防御・収入）から導く。
//! - 陣地線のマス保持: 「保持したときの1ラウンドの損害差 − 敵に取られたときの損害差」。
//! - 砲座からの射撃: 射程内のマスにいる想定敵へ1発撃つ期待ダメージ（反撃なし）。
//! - 拠点の占領: 1手番あたりの収入（敵拠点なら奪う分も含めて2倍）。
//!
//! 既存部隊は「費用 1・自分の位置を施設とみなす」候補として入れる。費用 1 なので比率が極端に高く、
//! 貪欲法では必ず新規購入より先に配置される。購入の価値は「既存部隊だけの合計」との差で測る。

use super::{Candidate, Demand, DemandId, FundsValue, ServiceTiming, TurnWindow};
use crate::ai::turn_distance::{SourceTurnDistanceCache, calculate_source_turn_distances_cached};
use crate::ai::v4::hold_line::enemy_ground_arrivals;
use crate::ai::v4::hold_line::fire_support::{FireRange, FireSupportInput, battery_sites};
use crate::ai::v4::hold_line::planner::InitialHoldSites;
use crate::collections::{HashMap, HashSet};
use crate::components::{
    Faction, GridPosition, Health, PlayerId, Property, Transporting, UnitStats,
};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{
    DamageChart, Map, MatchState, MovementType, Players, UnitRegistry, UnitType,
};
use crate::systems::combat::calculate_damage_formula;
use crate::systems::movement::OccupantInfo;
use crate::systems::production::is_within_production_range;
use bevy_ecs::prelude::World;

/// 評価の終端（対局の最終手番）。エンジンは対局の手番上限を持たないため、診断の入力として受け取る。
const LAST_TURN_ENV: &str = "OPENWARS_F_TRACE_LAST_TURN";

/// 需要の種類（トレース表示用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DemandKind {
    Hold(GridPosition),
    Battery(GridPosition),
    Capture(GridPosition),
}

/// 購入候補1件と、その兵種・需要の種類。
#[derive(Debug, Clone, Copy)]
pub(crate) struct PurchaseRow {
    pub(crate) candidate: Candidate,
    pub(crate) unit_type: UnitType,
    pub(crate) kind: DemandKind,
}

/// 1手番分の評価盤面。
pub(crate) struct DemandBoard {
    pub(crate) turn: u32,
    pub(crate) funds: u32,
    pub(crate) demands: Vec<Demand>,
    /// 既存部隊の候補（費用 1、施設 = 部隊の現在地）。
    pub(crate) existing: Vec<Candidate>,
    /// 既存部隊の数（既存部隊の候補に割り当てる予算）。
    pub(crate) existing_units: u32,
    pub(crate) purchases: Vec<PurchaseRow>,
    pub(crate) block: HashMap<GridPosition, FundsValue>,
    pub(crate) kinds: HashMap<DemandId, DemandKind>,
}

/// 盤面上の部隊1体。
struct UnitRow {
    pos: GridPosition,
    owner: PlayerId,
    stats: UnitStats,
    hp: u32,
}

/// ダメージ計算に使う最小限の情報。
struct Combat<'a> {
    chart: &'a DamageChart,
    registry: &'a UnitRegistry,
}

impl Combat<'_> {
    /// `attacker`（HP `attacker_hp`）が防御 `defense` の `defender` に与えるダメージ（HP 割合、上限 100）。
    fn damage(
        &self,
        attacker: UnitType,
        attacker_hp: u32,
        defender: UnitType,
        defense: u32,
        is_counter: bool,
    ) -> u32 {
        if attacker_hp == 0 {
            return 0;
        }
        let base = self
            .chart
            .get_base_damage(attacker, defender)
            .or_else(|| self.chart.get_base_damage_secondary(attacker, defender))
            .unwrap_or(0);
        if base == 0 {
            return 0;
        }
        calculate_damage_formula(base, attacker_hp, defense, is_counter).min(100)
    }

    /// 隣接して撃ち合える（どちらも最小射程 1）か。
    fn is_direct(&self, unit: UnitType) -> bool {
        self.registry
            .get_stats(unit)
            .is_some_and(|stats| stats.min_range <= 1 && stats.max_range >= 1)
    }

    /// 1ラウンド（相手の攻撃と自分の攻撃がそれぞれ1回）で、自分 `ours` と相手 `theirs` が失う資金換算の価値。
    ///
    /// `our_defense` は自分の立つマス、`their_defense` は相手の立つマスの地形防御。
    /// `theirs_first` なら相手が先に撃つ手番から数える（反撃は撃たれた直後の HP で行う）。
    fn round_exchange(
        &self,
        ours: (UnitType, u32),
        theirs: (UnitType, u32),
        our_defense: u32,
        their_defense: u32,
    ) -> i64 {
        let (our_type, our_cost) = ours;
        let (their_type, their_cost) = theirs;
        let counters = self.is_direct(our_type) && self.is_direct(their_type);
        // 相手の攻撃と自分の反撃。
        let hit_us = self.damage(their_type, 100, our_type, our_defense, false);
        let counter_them = if counters {
            self.damage(our_type, 100 - hit_us, their_type, their_defense, true)
        } else {
            0
        };
        // 自分の攻撃と相手の反撃。
        let hit_them = self.damage(our_type, 100, their_type, their_defense, false);
        let counter_us = if counters {
            self.damage(their_type, 100 - hit_them, our_type, our_defense, true)
        } else {
            0
        };
        let their_loss = i64::from(counter_them + hit_them) * i64::from(their_cost);
        let our_loss = i64::from(hit_us + counter_us) * i64::from(our_cost);
        (their_loss - our_loss) / 100
    }
}

/// 評価の終端の手番。
fn last_turn() -> u32 {
    std::env::var(LAST_TURN_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(30)
}

fn units(world: &World) -> Vec<UnitRow> {
    world
        .iter_entities()
        .filter(|entity| entity.get::<Transporting>().is_none())
        .filter_map(|entity| {
            Some(UnitRow {
                pos: *entity.get::<GridPosition>()?,
                owner: entity.get::<Faction>()?.0,
                stats: entity.get::<UnitStats>()?.clone(),
                hp: entity.get::<Health>().map_or(100, |health| health.current),
            })
        })
        .collect()
}

fn ground(movement: MovementType) -> bool {
    !matches!(movement, MovementType::Air | MovementType::Ship)
}

fn is_indirect(stats: &UnitStats) -> bool {
    stats.min_range >= 2
}

/// 想定敵: 盤面の敵地上直接部隊のうち、価値（費用 × HP）の合計が最大の兵種。
/// 敵地上部隊がいなければ、最も安い占領可能な地上兵種（序盤の歩兵）を想定する。
fn representative_enemy(
    units: &[UnitRow],
    player: PlayerId,
    registry: &UnitRegistry,
) -> Option<(UnitType, u32)> {
    let mut totals: HashMap<UnitType, (u64, u32)> = HashMap::default();
    for unit in units
        .iter()
        .filter(|unit| unit.owner != player && ground(unit.stats.movement_type))
        .filter(|unit| !is_indirect(&unit.stats) && unit.stats.max_range >= 1)
    {
        let entry = totals
            .entry(unit.stats.unit_type)
            .or_insert((0, unit.stats.cost));
        entry.0 += u64::from(unit.stats.cost) * u64::from(unit.hp);
    }
    totals
        .into_iter()
        .max_by_key(|(kind, (total, _))| (*total, std::cmp::Reverse(kind.as_str())))
        .map(|(kind, (_, cost))| (kind, cost))
        .or_else(|| {
            registry
                .0
                .iter()
                .filter(|(_, stats)| {
                    stats.can_capture && stats.cost > 0 && ground(stats.movement_type)
                })
                .min_by_key(|(kind, stats)| (stats.cost, kind.as_str()))
                .map(|(kind, stats)| (*kind, stats.cost))
        })
}

/// 需要を1つずつ登録する。同じ（需要の種類, 兵種）には同じ ID を返す。
#[derive(Default)]
struct DemandTable {
    demands: Vec<Demand>,
    ids: HashMap<(DemandKind, Option<UnitType>), DemandId>,
    kinds: HashMap<DemandId, DemandKind>,
}

impl DemandTable {
    fn id(
        &mut self,
        kind: DemandKind,
        unit: Option<UnitType>,
        window: TurnWindow,
        per_turn: i64,
    ) -> Option<DemandId> {
        if per_turn <= 0 || window.len() == 0 {
            return None;
        }
        if let Some(id) = self.ids.get(&(kind, unit)) {
            return Some(*id);
        }
        let id = DemandId(self.demands.len() as u32);
        self.demands.push(Demand {
            id,
            window,
            per_turn: FundsValue(per_turn),
        });
        self.ids.insert((kind, unit), id);
        self.kinds.insert(id, kind);
        Some(id)
    }
}

/// 盤面から1手番分の評価盤面を組み立てる。陣地線が無い盤面では保持と砲座の需要を作らない。
pub(crate) fn build(world: &World, player: PlayerId) -> Option<DemandBoard> {
    let map = world.get_resource::<Map>()?;
    let master = world.get_resource::<MasterDataRegistry>()?;
    let registry = world.get_resource::<UnitRegistry>()?;
    let chart = world.get_resource::<DamageChart>()?;
    let turn = world.get_resource::<MatchState>()?.current_turn_number.0;
    let funds = world
        .get_resource::<Players>()?
        .0
        .iter()
        .find(|p| p.id == player)?
        .funds;
    let last = last_turn();
    let combat = Combat { chart, registry };
    let all_units = units(world);
    let enemy = representative_enemy(&all_units, player, registry)?;
    let enemy_eta = enemy_ground_arrivals(world, player).unwrap_or_default();
    // 敵が着く手番（絶対手番）。見積もりが無いマスには敵が来ないものとして扱う。
    let enemy_turn = |pos: &GridPosition| enemy_eta.get(pos).map(|eta| turn + eta);

    let occupied: HashMap<(usize, usize), OccupantInfo> = all_units
        .iter()
        .map(|unit| {
            (
                (unit.pos.x, unit.pos.y),
                OccupantInfo {
                    player_id: unit.owner,
                    is_transport: false,
                    unit_type: unit.stats.unit_type,
                    loadable_types: Vec::new(),
                    free_slots: 0,
                },
            )
        })
        .collect();
    let mut cache = SourceTurnDistanceCache::default();
    // `from` から `to` までの移動手番数（同じマスなら 0）。届かなければ `None`。
    let mut travel = |from: GridPosition, stats: &UnitStats, to: GridPosition| -> Option<u32> {
        if from == to {
            return Some(0);
        }
        let field = calculate_source_turn_distances_cached(
            map,
            master,
            &occupied,
            (from.x, from.y),
            stats.movement_type,
            stats.max_movement,
            player,
            &mut cache,
        );
        field.get(&to).map(|distance| distance.turns)
    };
    let defense = |pos: GridPosition| {
        map.get_terrain(pos.x, pos.y)
            .map_or(0, |terrain| master.get_terrain_defense_bonus(terrain))
    };

    let mut table = DemandTable::default();

    // ---- 生産施設 ----
    let properties: Vec<(GridPosition, Property)> = world
        .iter_entities()
        .filter_map(|entity| Some((*entity.get::<GridPosition>()?, *entity.get::<Property>()?)))
        .collect();
    let capitals: Vec<GridPosition> = properties
        .iter()
        .filter(|(_, property)| {
            property.owner_id == Some(player)
                && property.terrain == crate::resources::Terrain::Capital
        })
        .map(|(pos, _)| *pos)
        .collect();
    let facilities: Vec<(GridPosition, crate::resources::Terrain)> = properties
        .iter()
        .filter(|(pos, property)| {
            property.owner_id == Some(player)
                && master.is_production_facility(property.terrain.as_str())
                && is_within_production_range(&capitals, pos.x, pos.y, map.topology)
        })
        .map(|(pos, property)| (*pos, property.terrain))
        .collect();
    let free_facilities: Vec<_> = facilities
        .iter()
        .copied()
        .filter(|(pos, _)| !occupied.contains_key(&(pos.x, pos.y)))
        .collect();
    let mut buyable: Vec<(GridPosition, &UnitStats)> = Vec::new();
    for &(pos, terrain) in &free_facilities {
        let mut types: Vec<_> = registry
            .0
            .values()
            .filter(|stats| {
                stats.cost > 0
                    && stats.cost <= funds
                    && master.can_produce_unit(terrain.as_str(), stats.unit_type)
            })
            .collect();
        types.sort_unstable_by_key(|stats| stats.unit_type.as_str());
        buyable.extend(types.into_iter().map(|stats| (pos, stats)));
    }
    let my_units: Vec<&UnitRow> = all_units.iter().filter(|u| u.owner == player).collect();

    let mut rows = Rows::default();

    // 新規購入は次の手番から動く。既存部隊はこの手番に動ける（購入手番を1つ前とみなす）。
    let mut origins: Vec<(Origin, &UnitStats, u32)> = buyable
        .iter()
        .map(|(pos, stats)| {
            (
                Origin {
                    position: *pos,
                    buy_turn: turn,
                    existing: false,
                },
                *stats,
                100,
            )
        })
        .collect();
    origins.extend(my_units.iter().map(|unit| {
        (
            Origin {
                position: unit.pos,
                buy_turn: turn.saturating_sub(1),
                existing: true,
            },
            &unit.stats,
            unit.hp,
        )
    }));
    // 需要の価値は満タンの部隊で測る（同じ需要を兵種ごとに1つにまとめるため、個体の HP では変えない）。

    // ---- 陣地線の保持・砲座 ----
    let line = world
        .get_resource::<InitialHoldSites>()
        .and_then(|sites| sites.line_for(player, map, master));
    if let Some(line) = line {
        let line_set: HashSet<GridPosition> = line.cells.iter().copied().collect();
        let neighbors = |pos: GridPosition| -> Vec<GridPosition> {
            map.get_adjacent(pos.x, pos.y)
                .into_iter()
                .map(|(x, y)| GridPosition { x, y })
                .collect()
        };
        let mut enemy_side: Vec<GridPosition> = line
            .cells
            .iter()
            .flat_map(|cell| neighbors(*cell))
            .filter(|pos| !line_set.contains(pos) && !line.home.contains(pos))
            .collect();
        enemy_side.sort_unstable_by_key(|pos| (pos.y, pos.x));
        enemy_side.dedup();
        let average = |cells: &[GridPosition]| -> Option<u32> {
            (!cells.is_empty())
                .then(|| cells.iter().map(|pos| defense(*pos)).sum::<u32>() / cells.len() as u32)
        };

        for &cell in &line.cells {
            let around = neighbors(cell);
            let outside: Vec<_> = around
                .iter()
                .copied()
                .filter(|p| enemy_side.contains(p))
                .collect();
            let inside: Vec<_> = around
                .iter()
                .copied()
                .filter(|p| line.home.contains(p) && !line_set.contains(p))
                .collect();
            let (Some(their_defense), Some(our_defense)) = (average(&outside), average(&inside))
            else {
                continue;
            };
            let Some(start) = enemy_turn(&cell) else {
                continue;
            };
            let window = TurnWindow { start, end: last };
            for &(origin, stats, _) in &origins {
                if !ground(stats.movement_type) || is_indirect(stats) || stats.max_range == 0 {
                    continue;
                }
                let ours = (stats.unit_type, stats.cost);
                // 保持: 自分は線のマス、敵は外側。取られた: 敵が線のマス、自分は内側から攻める。
                let held = combat.round_exchange(ours, enemy, defense(cell), their_defense);
                let lost = combat.round_exchange(ours, enemy, our_defense, defense(cell));
                let per_turn = held - lost;
                let Some(id) = table.id(
                    DemandKind::Hold(cell),
                    Some(stats.unit_type),
                    window,
                    per_turn,
                ) else {
                    continue;
                };
                let Some(turns) = travel(origin.position, stats, cell) else {
                    continue;
                };
                rows.add(
                    origin,
                    stats,
                    Placement {
                        demand: id,
                        kind: DemandKind::Hold(cell),
                        occupy: cell,
                        timing: ServiceTiming::occupy(origin.buy_turn, turns),
                        release_after: None,
                    },
                );
            }
        }

        // 砲座: 射程ごとに候補を求め、射程内のマスにいる想定敵へ1発撃つ価値を1手番の価値とする。
        let mut sites_by_range: HashMap<(u32, u32), Vec<_>> = HashMap::default();
        for &(origin, stats, _) in &origins {
            if !ground(stats.movement_type) || !is_indirect(stats) {
                continue;
            }
            let range = (stats.min_range, stats.max_range);
            let sites = sites_by_range.entry(range).or_insert_with(|| {
                battery_sites(
                    &FireSupportInput {
                        line: &line.cells,
                        enemy_side: &enemy_side,
                        home: &line.home,
                        distance: |a: GridPosition, b: GridPosition| {
                            map.distance(a.x, a.y, b.x, b.y)
                        },
                    },
                    FireRange {
                        min: range.0,
                        max: range.1,
                    },
                )
            });
            for site in sites.iter() {
                let shot = site
                    .covered
                    .iter()
                    .map(|target| {
                        combat.damage(stats.unit_type, 100, enemy.0, defense(*target), false)
                    })
                    .max()
                    .unwrap_or(0);
                let Some(start) = site.covered.iter().filter_map(&enemy_turn).min() else {
                    continue;
                };
                let per_turn = i64::from(shot) * i64::from(enemy.1) / 100;
                let window = TurnWindow { start, end: last };
                let kind = DemandKind::Battery(site.position);
                let Some(id) = table.id(kind, Some(stats.unit_type), window, per_turn) else {
                    continue;
                };
                let Some(turns) = travel(origin.position, stats, site.position) else {
                    continue;
                };
                rows.add(
                    origin,
                    stats,
                    Placement {
                        demand: id,
                        kind,
                        occupy: site.position,
                        timing: ServiceTiming::indirect_fire(origin.buy_turn, turns),
                        release_after: None,
                    },
                );
            }
        }
    }

    // ---- 占領 ----
    for (pos, property) in &properties {
        if property.owner_id == Some(player) || property.max_capture_points == 0 {
            continue;
        }
        let income = i64::from(master.landscape_income(property.terrain.as_str()));
        // 敵の拠点は奪えば敵の収入も減る。
        let per_turn = if property.owner_id.is_some() {
            2 * income
        } else {
            income
        };
        let window = TurnWindow {
            start: turn,
            end: last,
        };
        let kind = DemandKind::Capture(*pos);
        let Some(id) = table.id(kind, None, window, per_turn) else {
            continue;
        };
        for &(origin, stats, hp) in &origins {
            if !stats.can_capture {
                continue;
            }
            let Some(turns) = travel(origin.position, stats, *pos) else {
                continue;
            };
            let arrival = origin.buy_turn + turns;
            // 敵が先に着く拠点は取れないものとして扱う。
            if enemy_turn(pos).is_some_and(|enemy_at| enemy_at < arrival) {
                continue;
            }
            // 1手番に表示HP × 10 だけ耐久を削る。既存部隊が占領中ならその残りから数える。
            let power = (hp.saturating_add(9) / 10 * 10).max(1);
            let remaining = if origin.existing && origin.position == *pos {
                property.capture_points
            } else {
                property.max_capture_points
            };
            let capture_turns = remaining.div_ceil(power).max(1);
            rows.add(
                origin,
                stats,
                Placement {
                    demand: id,
                    kind,
                    occupy: *pos,
                    timing: ServiceTiming {
                        occupy_from: arrival,
                        serve_from: arrival + capture_turns,
                    },
                    release_after: Some(arrival + capture_turns - 1),
                },
            );
        }
    }

    // ---- 生産施設を塞ぐ損: そこで最も安い占領兵を作って線を保持させた場合の1手番の価値 ----
    let cheapest_holder = registry
        .0
        .values()
        .filter(|stats| stats.can_capture && stats.cost > 0 && ground(stats.movement_type))
        .min_by_key(|stats| (stats.cost, stats.unit_type.as_str()))
        .map(|stats| stats.unit_type);
    let block_value = table
        .ids
        .iter()
        .filter(|((kind, unit), _)| matches!(kind, DemandKind::Hold(_)) && *unit == cheapest_holder)
        .map(|(_, id)| table.demands[id.0 as usize].per_turn)
        .max()
        .unwrap_or_default();
    let block = facilities
        .iter()
        .map(|(pos, _)| (*pos, block_value))
        .collect();

    Some(DemandBoard {
        turn,
        funds,
        existing_units: my_units.len() as u32,
        demands: table.demands,
        existing: rows.existing,
        purchases: rows.purchases,
        block,
        kinds: table.kinds,
    })
}

/// 候補の出どころ（購入する施設、または既存部隊の現在地）。
#[derive(Debug, Clone, Copy)]
struct Origin {
    position: GridPosition,
    buy_turn: u32,
    existing: bool,
}

/// 候補1件の置き方（満たす需要・とどまるマス・手番）。
struct Placement {
    demand: DemandId,
    kind: DemandKind,
    occupy: GridPosition,
    timing: ServiceTiming,
    release_after: Option<u32>,
}

/// 組み立て中の候補。
#[derive(Default)]
struct Rows {
    purchases: Vec<PurchaseRow>,
    existing: Vec<Candidate>,
}

impl Rows {
    /// 候補を1件足す。既存部隊は費用 1・現在地を施設とみなす。
    fn add(&mut self, origin: Origin, stats: &UnitStats, placement: Placement) {
        let candidate = Candidate {
            demand: placement.demand,
            facility: origin.position,
            cost: if origin.existing { 1 } else { stats.cost },
            buy_turn: origin.buy_turn,
            occupy: placement.occupy,
            timing: placement.timing,
            transit_loss: FundsValue(0),
            release_after: placement.release_after,
        };
        if origin.existing {
            self.existing.push(candidate);
        } else {
            self.purchases.push(PurchaseRow {
                candidate,
                unit_type: stats.unit_type,
                kind: placement.kind,
            });
        }
    }
}
