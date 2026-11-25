//! 地形で決まった線に対し、砲座の脅威を減らす配置だけを仮盤面で選ぶ。

use super::{
    ground_movement,
    terrain::{TerrainLine, select_terrain_line},
};
use crate::ai::tactics::{board::TacticalBoard, unit::UnitState};
use crate::collections::{HashMap, HashSet};
use crate::components::{Fuel, GridPosition, Health, PlayerId, Property, UnitStats};
use crate::resources::{
    Map, MovementType, Terrain, UnitRegistry, UnitType, master_data::MasterDataRegistry,
};
use crate::systems::movement::{OccupantInfo, calculate_reachable_tile_costs};
use crate::systems::production::is_within_production_range;
use bevy_ecs::prelude::*;
use std::{cmp::Reverse, collections::BTreeMap, sync::OnceLock};

#[cfg(test)]
mod tests;

type Occupancy = HashMap<(usize, usize), OccupantInfo>;
type Risk = [usize; 4];

fn xy(pos: GridPosition) -> (usize, usize) {
    (pos.x, pos.y)
}

/// 地形選定の端点を対局開始時に保存する。対局中の所有権の変化とは分ける。
#[derive(Resource)]
pub(crate) struct InitialHoldSites {
    sites: HashMap<PlayerId, Vec<GridPosition>>,
    lines: HashMap<PlayerId, OnceLock<Option<TerrainLine>>>,
}

impl InitialHoldSites {
    pub(crate) fn observe(world: &World) -> Self {
        let mut sites: HashMap<_, Vec<_>> = HashMap::default();
        for entity in world.iter_entities() {
            if let (Some(pos), Some(property)) =
                (entity.get::<GridPosition>(), entity.get::<Property>())
                && matches!(property.terrain, Terrain::Capital | Terrain::Factory)
                && let Some(owner) = property.owner_id
            {
                sites.entry(owner).or_default().push(*pos);
            }
        }
        let lines = sites
            .keys()
            .map(|&owner| (owner, OnceLock::new()))
            .collect();
        Self { sites, lines }
    }

    pub(crate) fn line(&self, board: &TacticalBoard) -> Option<&TerrainLine> {
        self.line_for(board.player_id, &board.map, &board.registry)
    }

    pub(crate) fn line_for(
        &self,
        player_id: PlayerId,
        map: &Map,
        registry: &MasterDataRegistry,
    ) -> Option<&TerrainLine> {
        self.lines
            .get(&player_id)?
            .get_or_init(|| {
                let ours = self.sites.get(&player_id)?;
                let theirs = self
                    .sites
                    .iter()
                    .filter(|(id, _)| **id != player_id)
                    .flat_map(|(_, positions)| positions.iter().copied())
                    .collect::<Vec<_>>();
                select_terrain_line(map, registry, ours, &theirs)
            })
            .as_ref()
    }
}

pub(crate) struct EligibleHolder<'a> {
    pub(crate) state: &'a UnitState,
    pub(crate) stops: &'a [(usize, usize)],
}

#[derive(Clone, Copy)]
struct Placement {
    unit: Entity,
    from: GridPosition,
    to: GridPosition,
    defense: u32,
    cost: u32,
}

#[derive(Clone, Copy)]
pub(crate) struct HoldDecision {
    placement: Placement,
    before: Risk,
    after: Risk,
}

impl HoldDecision {
    pub(crate) fn unit(self) -> Entity {
        self.placement.unit
    }
    pub(crate) fn target(self) -> GridPosition {
        self.placement.to
    }

    pub(crate) fn trace_execution(self, world: &World, player: PlayerId) {
        trace(world, player, || {
            serde_json::json!({
                "phase": "selected", "unit": self.unit().to_bits(), "from": xy(self.placement.from),
                "to": xy(self.target()), "before": self.before, "after": self.after,
            })
        });
    }
}

fn trace_enabled() -> bool {
    std::env::var("OPENWARS_TRACE_HOLD_LINE").as_deref() == Ok("1")
}

fn trace(world: &World, player: PlayerId, details: impl FnOnce() -> serde_json::Value) {
    if trace_enabled() {
        let turn = world
            .get_resource::<crate::resources::MatchState>()
            .map(|state| state.current_turn_number.0);
        eprintln!(
            "HOLD_LINE {}",
            serde_json::json!({"turn": turn, "player": player.0, "detail": details()})
        );
    }
}

fn apply(occ: &mut Occupancy, placement: Placement, source: &Occupancy) {
    // 現位置の守備が未確約の歩兵は、評価用の盤面から一時的に除いていることがある。
    occ.remove(&xy(placement.from));
    occ.insert(xy(placement.to), source[&xy(placement.from)].clone());
}

fn choose(
    actual: &Occupancy,
    projected: &Occupancy,
    candidates: impl IntoIterator<Item = Placement>,
    risk: impl Fn(&Occupancy) -> Risk,
) -> Option<HoldDecision> {
    let before = risk(projected);
    candidates
        .into_iter()
        .filter_map(|placement| {
            let mut next = projected.clone();
            apply(&mut next, placement, actual);
            let after = risk(&next);
            (after < before).then_some(HoldDecision {
                placement,
                before,
                after,
            })
        })
        .min_by_key(|decision| {
            let p = decision.placement;
            (
                decision.after,
                p.from != p.to,
                Reverse(p.defense),
                p.cost,
                p.unit.to_bits(),
                p.to.y,
                p.to.x,
            )
        })
}

#[derive(Clone, Copy)]
struct Mover {
    position: GridPosition,
    owner: PlayerId,
    kind: UnitType,
    movement: MovementType,
    mp: u32,
    fuel: u32,
}

impl Mover {
    fn new(position: GridPosition, owner: PlayerId, stats: &UnitStats, fuel: u32) -> Self {
        Self {
            position,
            owner,
            kind: stats.unit_type,
            movement: stats.movement_type,
            mp: stats.max_movement,
            fuel,
        }
    }

    fn stops(
        self,
        map: &Map,
        master: &MasterDataRegistry,
        occ: &Occupancy,
    ) -> BTreeMap<(usize, usize), u32> {
        // 到達集合には通過だけが可能な味方占有マスも入るため、合法な停止位置へ絞る。
        let from = xy(self.position);
        let mut stops = calculate_reachable_tile_costs(
            map,
            occ,
            from,
            self.movement,
            self.mp,
            self.fuel,
            self.owner,
            self.kind,
            master,
        );
        stops.retain(|pos, _| *pos == from || !occ.contains_key(pos));
        stops
    }
}

struct Battery {
    position: GridPosition,
    unit: Option<Entity>,
    kinds: Vec<UnitType>,
}

struct ThreatModel<'a> {
    board: &'a TacticalBoard,
    terrain: &'a TerrainLine,
    enemies: Vec<(Entity, Mover)>,
    batteries: Vec<Battery>,
}

#[derive(Default)]
struct Threats {
    actual: HashSet<(Entity, Entity)>,
    planned: HashSet<(Entity, GridPosition)>,
    advance: HashSet<(Entity, GridPosition)>,
}

impl Threats {
    fn risk(&self) -> Risk {
        let attackers: HashSet<_> = self.actual.iter().map(|(enemy, _)| *enemy).collect();
        [
            attackers.len(),
            self.actual.len(),
            self.planned.len(),
            self.advance.len(),
        ]
    }

    fn removed_from(&self, after: &Self) -> serde_json::Value {
        let mut actual: Vec<_> = self
            .actual
            .difference(&after.actual)
            .map(|(enemy, target)| (enemy.to_bits(), target.to_bits()))
            .collect();
        actual.sort_unstable();
        let sort_cells = |before: &HashSet<(Entity, GridPosition)>, later: &HashSet<_>| {
            let mut stops: Vec<_> = before
                .difference(later)
                .map(|(enemy, pos)| (enemy.to_bits(), xy(*pos)))
                .collect();
            stops.sort_unstable();
            stops
        };
        serde_json::json!({"actual": actual, "planned": sort_cells(&self.planned, &after.planned),
            "advance": sort_cells(&self.advance, &after.advance)})
    }
}

fn place_battery(
    occ: &Occupancy,
    position: GridPosition,
    owner: PlayerId,
    kind: UnitType,
) -> Option<Occupancy> {
    if occ
        .get(&xy(position))
        .is_some_and(|unit| unit.player_id != owner)
    {
        return None;
    }
    let mut projected = occ.clone();
    // 味方がいる場合も重ねずに置換し、予定砲座に砲が着いた盤面を作る。
    projected.insert(
        xy(position),
        OccupantInfo {
            player_id: owner,
            unit_type: kind,
            is_transport: false,
            loadable_types: Vec::new(),
            free_slots: 0,
        },
    );
    Some(projected)
}

impl ThreatModel<'_> {
    fn assess(&self, occ: &Occupancy) -> Threats {
        let mut result = Threats::default();
        let planned = self
            .batteries
            .iter()
            .filter(|b| b.unit.is_none())
            .filter_map(|b| {
                place_battery(occ, b.position, self.board.player_id, b.kinds[0]).map(|p| (b, p))
            })
            .collect::<Vec<_>>();
        let deployed = self.batteries.iter().any(|b| b.unit.is_some());
        let map = &self.board.map;
        let master = &self.board.registry;
        for &(enemy, mover) in &self.enemies {
            let stops = mover.stops(map, master, occ);
            if !deployed && !self.terrain.home.contains(&mover.position) {
                result.advance.extend(stops.keys().filter_map(|&(x, y)| {
                    let pos = GridPosition { x, y };
                    (pos != mover.position
                        && (self.terrain.home.contains(&pos) || self.terrain.cells.contains(&pos)))
                    .then_some((enemy, pos))
                }));
            }
            for battery in &self.batteries {
                if let Some(unit) = battery.unit
                    && self.can_hit(enemy, mover.position, battery, &stops)
                {
                    result.actual.insert((enemy, unit));
                }
            }
            for (battery, with_battery) in &planned {
                let stops = mover.stops(map, master, with_battery);
                if self.can_hit(enemy, mover.position, battery, &stops) {
                    result.planned.insert((enemy, battery.position));
                }
            }
        }
        result
    }

    fn can_hit(
        &self,
        enemy: Entity,
        origin: GridPosition,
        battery: &Battery,
        stops: &BTreeMap<(usize, usize), u32>,
    ) -> bool {
        stops.keys().any(|&(x, y)| {
            let d = self
                .board
                .map
                .distance(x, y, battery.position.x, battery.position.y);
            battery.kinds.iter().any(|&kind| {
                self.board
                    .action_targets
                    .can_hit_type(enemy, kind, d, (x, y) != xy(origin))
            })
        })
    }
}

fn batteries(
    world: &World,
    board: &TacticalBoard,
    terrain: &TerrainLine,
    capture_sites: &HashSet<GridPosition>,
) -> Vec<Battery> {
    let Some(unit_registry) = world.get_resource::<UnitRegistry>() else {
        return Vec::new();
    };
    let capitals = board.own_capitals();
    let production = |position: GridPosition, tile: Terrain| {
        board.registry.is_production_facility(tile.as_str())
            && is_within_production_range(&capitals, position.x, position.y, board.map.topology)
    };
    let facilities = board
        .properties
        .iter()
        .filter(|&&(pos, tile, owner)| owner == Some(board.player_id) && production(pos, tile))
        .map(|&(_, tile, _)| tile.as_str())
        .collect::<HashSet<_>>();
    let on_board = board
        .action_targets
        .units()
        .iter()
        .filter(|u| u.faction == board.player_id && !u.is_transporting)
        .filter_map(|u| world.get::<UnitStats>(u.entity).map(|s| s.unit_type))
        .collect::<HashSet<_>>();
    let mut indirect = unit_registry
        .0
        .values()
        .filter(|stats| {
            stats.min_range > 1
                && ground_movement(stats.movement_type)
                && (on_board.contains(&stats.unit_type)
                    || facilities
                        .iter()
                        .any(|tile| board.registry.can_produce_unit(tile, stats.unit_type)))
        })
        .collect::<Vec<_>>();
    indirect.sort_unstable_by_key(|stats| stats.unit_type.as_str());
    board
        .properties
        .iter()
        .filter_map(|&(position, tile, owner)| {
            if !terrain.home.contains(&position)
                || production(position, tile)
                || (owner != Some(board.player_id) && !capture_sites.contains(&position))
            {
                return None;
            }
            let kinds = indirect
                .iter()
                .filter(|stats| {
                    board.registry.can_repair_on_terrain(stats.unit_type, tile)
                        && terrain.cells.iter().any(|p| {
                            (stats.min_range..=stats.max_range)
                                .contains(&board.map.distance(position.x, position.y, p.x, p.y))
                        })
                })
                .map(|stats| stats.unit_type)
                .collect::<Vec<_>>();
            if kinds.is_empty() {
                return None;
            }
            let unit = board.action_targets.units().iter().find(|u| {
                owner == Some(board.player_id)
                    && u.position == position
                    && u.faction == board.player_id
                    && !u.is_transporting
                    && world
                        .get::<UnitStats>(u.entity)
                        .is_some_and(|stats| kinds.contains(&stats.unit_type))
            });
            Some(match unit {
                Some(u) => Battery {
                    position,
                    unit: Some(u.entity),
                    kinds: vec![world.get::<UnitStats>(u.entity)?.unit_type],
                },
                None => Battery {
                    position,
                    unit: None,
                    kinds,
                },
            })
        })
        .collect()
}

fn legal_placements(
    model: &ThreatModel,
    actual: &Occupancy,
    holder: &EligibleHolder,
) -> Vec<Placement> {
    let state = holder.state;
    let board = model.board;
    let mover = Mover::new(state.position, board.player_id, &state.stats, state.fuel);
    let stops = mover.stops(&board.map, &board.registry, actual);
    model
        .terrain
        .cells
        .iter()
        .filter_map(|&to| {
            if (model.terrain.cells.contains(&state.position) && to != state.position)
                || !holder.stops.contains(&xy(to))
                || crate::ai::tactics::scoring::blocks_own_production(board, to)
            {
                return None;
            }
            let cost = *stops.get(&xy(to))?;
            let defense = board
                .registry
                .get_terrain_defense_bonus(board.map.get_terrain(to.x, to.y)?);
            let damage = crate::ai::threat::expected_incoming_damage(
                &board.map,
                xy(to),
                state.stats.unit_type,
                defense,
                &board.enemies,
                &board.damage_chart,
                &board.registry,
            );
            (damage < state.hp).then_some(Placement {
                unit: state.entity,
                from: state.position,
                to,
                defense,
                cost,
            })
        })
        .collect()
}

/// 次の1件を含む保持割当を順に求める。発行される行動ごとに最新盤面から計算する。
pub(crate) fn plan(
    world: &World,
    board: &TacticalBoard,
    holders: &[EligibleHolder],
    capture_sites: &HashSet<GridPosition>,
) -> Vec<HoldDecision> {
    let Some(terrain) = world
        .get_resource::<InitialHoldSites>()
        .and_then(|initial| initial.line(board))
    else {
        trace(
            world,
            board.player_id,
            || serde_json::json!({"phase": "rejected", "reason": "no_terrain_line"}),
        );
        return Vec::new();
    };
    if holders.is_empty() {
        trace(
            world,
            board.player_id,
            || serde_json::json!({"phase": "rejected", "reason": "no_eligible_holder"}),
        );
        return Vec::new();
    }
    let batteries = batteries(world, board, terrain, capture_sites);
    if batteries.is_empty() {
        trace(
            world,
            board.player_id,
            || serde_json::json!({"phase": "rejected", "reason": "no_battery_site"}),
        );
        return Vec::new();
    }
    let enemies = board
        .action_targets
        .units()
        .iter()
        .filter_map(|u| {
            let stats = world.get::<UnitStats>(u.entity)?;
            let fuel = world.get::<Fuel>(u.entity)?.current;
            (u.faction != board.player_id
                && !u.is_transporting
                && ground_movement(stats.movement_type)
                && world
                    .get::<Health>(u.entity)
                    .is_some_and(|hp| hp.current > 0))
            .then_some((u.entity, Mover::new(u.position, u.faction, stats, fuel)))
        })
        .collect();
    let model = ThreatModel {
        board,
        terrain,
        enemies,
        batteries,
    };
    let mut actual = board.occupancy.clone();
    let mut projected = actual.clone();
    for holder in holders
        .iter()
        .filter(|holder| terrain.cells.contains(&holder.state.position))
    {
        projected.remove(&xy(holder.state.position));
    }
    let mut used = HashSet::default();
    let mut plan = Vec::new();
    let mut first_removed = None;
    let stopped = loop {
        let candidates = holders
            .iter()
            .filter(|holder| !used.contains(&holder.state.entity))
            .flat_map(|holder| legal_placements(&model, &actual, holder))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            break "no_legal_stop";
        }
        let Some(decision) = choose(&actual, &projected, candidates, |occ| {
            model.assess(occ).risk()
        }) else {
            break "no_marginal_risk_reduction";
        };
        let mut next = actual.clone();
        apply(&mut next, decision.placement, &actual);
        let before = (plan.is_empty() && trace_enabled()).then(|| model.assess(&projected));
        apply(&mut projected, decision.placement, &actual);
        if let Some(before) = before {
            first_removed = Some(before.removed_from(&model.assess(&projected)));
        }
        actual = next;
        used.insert(decision.unit());
        plan.push(decision);
    };
    trace(world, board.player_id, || {
        let invaders: Vec<_> = model
            .enemies
            .iter()
            .filter(|(_, mover)| terrain.home.contains(&mover.position))
            .map(|(entity, mover)| (entity.to_bits(), xy(mover.position)))
            .collect();
        let sites: Vec<_> = model
            .batteries
            .iter()
            .map(|b| (xy(b.position), b.unit.map(Entity::to_bits)))
            .collect();
        let assignments: Vec<_> = plan
            .iter()
            .map(|d| (d.unit().to_bits(), xy(d.target()), d.before, d.after))
            .collect();
        let unassigned_on_line: Vec<_> = holders
            .iter()
            .filter(|holder| {
                terrain.cells.contains(&holder.state.position)
                    && !used.contains(&holder.state.entity)
            })
            .map(|holder| (holder.state.entity.to_bits(), xy(holder.state.position)))
            .collect();
        serde_json::json!({"phase": "plan", "line": terrain.cells.iter().copied().map(xy).collect::<Vec<_>>(),
            "invaders": invaders, "batteries": sites, "eligible": holders.len(),
            "stopped": stopped, "assignments": assignments, "unassigned_on_line": unassigned_on_line,
            "threats_removed_by_first": first_removed})
    });
    plan
}
