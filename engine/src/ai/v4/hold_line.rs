//! 地上部隊の到着見積もりから、物件を守る切断面を探す。CP A では診断にのみ使用する。

use crate::ai::code_path::would_use_extended;
use crate::ai::turn_distance::{SourceTurnDistanceCache, calculate_source_turn_distances_cached};
use crate::collections::{HashMap, HashSet};
use crate::components::{Faction, GridPosition, PlayerId, Property, Transporting, UnitStats};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Map, MovementType, Players, Terrain, UnitRegistry};
use crate::systems::movement::{OccupantInfo, get_valid_movement_cost};
use bevy_ecs::prelude::World;
use serde_json::{Value, json};

pub(crate) mod fire_support;
pub(crate) mod forced;
pub(crate) mod planner;
pub(crate) mod terrain;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy)]
struct HoldLineTile {
    pos: GridPosition,
    ours: Option<u32>,
    theirs: Option<u32>,
    defense: u32,
}

impl HoldLineTile {
    fn arrives_first(self, moves_first: bool) -> bool {
        match (self.ours, self.theirs) {
            (Some(ours), Some(theirs)) => ours < theirs || (ours == theirs && moves_first),
            (Some(_), None) => true,
            (None, _) => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Rejection {
    NoGroundRoute,
    NoCandidate,
    EnemyCanBypass,
    EnemyOccupied,
    UnreachableByUs,
}

#[derive(Debug)]
struct Layer {
    cells: Vec<GridPosition>,
    defense_sum: u64,
    rejection: Option<Rejection>,
}

impl Layer {
    fn new(
        tiles: &[HoldLineTile],
        cells: &HashSet<GridPosition>,
        reachable_before_cut: bool,
        reachable_after_cut: bool,
    ) -> Self {
        let mut positions: Vec<_> = cells.iter().copied().collect();
        positions.sort_unstable_by_key(|pos| (pos.y, pos.x));
        let layer_tiles = tiles.iter().filter(|tile| cells.contains(&tile.pos));
        let rejection = if !reachable_before_cut {
            Some(Rejection::NoGroundRoute)
        } else if cells.is_empty() {
            Some(Rejection::NoCandidate)
        } else if reachable_after_cut {
            Some(Rejection::EnemyCanBypass)
        } else if layer_tiles.clone().any(|tile| tile.theirs == Some(0)) {
            // 敵の現在地を切断面から取り除くだけでは、先に排除する必要のある敵を無視してしまう。
            Some(Rejection::EnemyOccupied)
        } else if layer_tiles.clone().any(|tile| tile.ours.is_none()) {
            Some(Rejection::UnreachableByUs)
        } else {
            None
        };
        let defense_sum = layer_tiles.map(|tile| u64::from(tile.defense)).sum();
        Self {
            cells: positions,
            defense_sum,
            rejection,
        }
    }

    fn defense_at_least(&self, other: &Self) -> bool {
        // 平均を比較する。大きい層が単にマス数で勝ったり、整数除算で同点になったりしない。
        u128::from(self.defense_sum) * other.cells.len() as u128
            >= u128::from(other.defense_sum) * self.cells.len() as u128
    }
}

#[derive(Debug)]
struct HoldLineSelection {
    boundary: Layer,
    enemy_side: Layer,
}

impl HoldLineSelection {
    fn strict_line(&self) -> Option<&[GridPosition]> {
        self.boundary
            .rejection
            .is_none()
            .then_some(&self.boundary.cells)
    }

    fn extended_line(&self) -> Option<&[GridPosition]> {
        if self.enemy_side.rejection.is_none()
            && (self.boundary.rejection.is_some()
                || self.enemy_side.defense_at_least(&self.boundary))
        {
            Some(&self.enemy_side.cells)
        } else {
            self.strict_line()
        }
    }
}

fn walk(
    sources: &[GridPosition],
    adjacent: &impl Fn(GridPosition) -> Vec<GridPosition>,
    can_enter: impl Fn(GridPosition) -> bool,
) -> HashSet<GridPosition> {
    let mut reached = HashSet::default();
    let mut pending = Vec::new();
    for &source in sources {
        if can_enter(source) && reached.insert(source) {
            pending.push(source);
        }
    }
    while let Some(pos) = pending.pop() {
        for neighbor in adjacent(pos) {
            if can_enter(neighbor) && reached.insert(neighbor) {
                pending.push(neighbor);
            }
        }
    }
    reached
}

fn select_hold_line(
    tiles: &[HoldLineTile],
    adjacent: impl Fn(GridPosition) -> Vec<GridPosition>,
    protected: &[GridPosition],
    enemy_sources: &[GridPosition],
    moves_first: bool,
) -> HoldLineSelection {
    let ground: HashSet<_> = tiles.iter().map(|tile| tile.pos).collect();
    let from_ours = walk(protected, &adjacent, |pos| ground.contains(&pos));
    let from_theirs = walk(enemy_sources, &adjacent, |pos| ground.contains(&pos));
    let connected = |pos| from_ours.contains(&pos) && from_theirs.contains(&pos);
    let reachable_before_cut = protected.iter().any(|pos| from_theirs.contains(pos));
    // 両軍の地上経路がつながる陸塊だけを対象とし、先着できる側とその外側を分ける。
    let front: HashSet<_> = tiles
        .iter()
        .filter(|tile| connected(tile.pos) && tile.arrives_first(moves_first))
        .map(|tile| tile.pos)
        .collect();
    let mut boundary = HashSet::default();
    let mut enemy_side = HashSet::default();
    for &pos in &front {
        for neighbor in adjacent(pos) {
            if connected(neighbor) && !front.contains(&neighbor) {
                boundary.insert(pos);
                enemy_side.insert(neighbor);
            }
        }
    }
    let make_layer = |cells: &HashSet<GridPosition>| {
        // 到着差の境界でも、自軍物件までの迂回路を遮れなければ陣地線ではない。
        let after_cut = walk(enemy_sources, &adjacent, |pos| {
            ground.contains(&pos) && !cells.contains(&pos)
        });
        Layer::new(
            tiles,
            cells,
            reachable_before_cut,
            protected.iter().any(|pos| after_cut.contains(pos)),
        )
    };
    HoldLineSelection {
        boundary: make_layer(&boundary),
        enemy_side: make_layer(&enemy_side),
    }
}

fn ground_movement(movement: MovementType) -> bool {
    !matches!(movement, MovementType::Air | MovementType::Ship)
}

#[derive(Clone, Copy)]
struct ArrivalOrigin {
    position: GridPosition,
    owner: PlayerId,
    movement: MovementType,
    max_mp: u32,
    production_delay: u32,
}

impl ArrivalOrigin {
    fn new(
        position: GridPosition,
        owner: PlayerId,
        stats: &UnitStats,
        production_delay: u32,
    ) -> Self {
        Self {
            position,
            owner,
            movement: stats.movement_type,
            max_mp: stats.max_movement,
            production_delay,
        }
    }
}

#[derive(Default)]
struct GroundBoard {
    protected: Vec<GridPosition>,
    enemy_sources: Vec<GridPosition>,
    origins: Vec<ArrivalOrigin>,
    occupied: HashMap<(usize, usize), OccupantInfo>,
}

impl GroundBoard {
    fn collect(world: &World, player: PlayerId, master: &MasterDataRegistry) -> Option<Self> {
        let registry = world.get_resource::<UnitRegistry>()?;
        // 進軍路の要衝と同じく、首都からの増援は最も安い占領兵を1手番後に送り出す。
        let recruit = registry
            .0
            .iter()
            .filter(|(kind, stats)| {
                stats.can_capture
                    && stats.cost > 0
                    && ground_movement(stats.movement_type)
                    && master.can_produce_unit(Terrain::Capital.as_str(), **kind)
            })
            .min_by_key(|(kind, stats)| (stats.cost, kind.as_str()))
            .map(|(_, stats)| stats);
        let mut board = Self::default();
        for entity in world.iter_entities() {
            let Some(&position) = entity.get::<GridPosition>() else {
                continue;
            };
            if let Some(property) = entity.get::<Property>()
                && let Some(owner) = property.owner_id
            {
                if owner == player {
                    board.protected.push(position);
                }
                if property.terrain == Terrain::Capital {
                    if let Some(stats) = recruit {
                        board
                            .origins
                            .push(ArrivalOrigin::new(position, owner, stats, 1));
                    }
                    if owner != player {
                        board.enemy_sources.push(position);
                    }
                }
            }
            if let (Some(faction), Some(stats), None) = (
                entity.get::<Faction>(),
                entity.get::<UnitStats>(),
                entity.get::<Transporting>(),
            ) {
                board.occupied.insert(
                    (position.x, position.y),
                    OccupantInfo {
                        player_id: faction.0,
                        is_transport: false,
                        unit_type: stats.unit_type,
                        loadable_types: Vec::new(),
                        free_slots: 0,
                    },
                );
                if ground_movement(stats.movement_type) {
                    board
                        .origins
                        .push(ArrivalOrigin::new(position, faction.0, stats, 0));
                    if faction.0 != player {
                        board.enemy_sources.push(position);
                    }
                }
            }
        }
        Some(board)
    }

    fn arrivals(
        &self,
        map: &Map,
        master: &MasterDataRegistry,
        player: PlayerId,
    ) -> [HashMap<GridPosition, u32>; 2] {
        let mut cache = SourceTurnDistanceCache::default();
        let mut arrivals: [HashMap<GridPosition, u32>; 2] = Default::default();
        for origin in &self.origins {
            let side = usize::from(origin.owner != player);
            if origin.production_delay == 0 {
                // 移動力がない実部隊も、現在地には既に到着している。
                arrivals[side].insert(origin.position, 0);
            }
            // CP A では総移動コスト÷移動力の近似値を観測する。手番内の余剰移動力は考慮しない。
            let field = calculate_source_turn_distances_cached(
                map,
                master,
                &self.occupied,
                (origin.position.x, origin.position.y),
                origin.movement,
                origin.max_mp,
                origin.owner,
                &mut cache,
            );
            for (&pos, distance) in field.iter() {
                let eta = distance.turns.saturating_add(origin.production_delay);
                arrivals[side]
                    .entry(pos)
                    .and_modify(|old| *old = (*old).min(eta))
                    .or_insert(eta);
            }
        }
        arrivals
    }
}

fn positions_json(positions: &[GridPosition]) -> Vec<Value> {
    positions
        .iter()
        .map(|pos| json!({"x": pos.x, "y": pos.y}))
        .collect()
}

/// 小規模V4の手番開始盤面から、陣地線と外側候補を診断する。任務・戦術への書き戻しはしない。
pub fn hold_line_diagnostics_for_player(world: &World, player_id: PlayerId) -> Option<Value> {
    if !would_use_extended(world, player_id) {
        return None;
    }
    let map = world.get_resource::<Map>()?;
    let master = world.get_resource::<MasterDataRegistry>()?;
    let players = world.get_resource::<Players>()?;
    let moves_first = players.0.first()?.id == player_id;
    let board = GroundBoard::collect(world, player_id, master)?;
    let [ours, theirs] = board.arrivals(map, master, player_id);
    let ground_types = [
        MovementType::Infantry,
        MovementType::Tank,
        MovementType::Artillery,
        MovementType::ArmoredCar,
    ];
    let mut tiles = Vec::new();
    for y in 0..map.height {
        for x in 0..map.width {
            let Some(terrain) = map.get_terrain(x, y) else {
                continue;
            };
            // 切断の検査は地形だけで行う。現時点の占有で通れなくても、進路自体は残っている。
            if ground_types
                .iter()
                .any(|&movement| get_valid_movement_cost(master, movement, terrain).is_some())
            {
                let pos = GridPosition { x, y };
                tiles.push(HoldLineTile {
                    pos,
                    ours: ours.get(&pos).copied(),
                    theirs: theirs.get(&pos).copied(),
                    defense: master.get_terrain_defense_bonus(terrain),
                });
            }
        }
    }
    let selection = select_hold_line(
        &tiles,
        |pos| {
            map.get_adjacent(pos.x, pos.y)
                .into_iter()
                .map(|(x, y)| GridPosition { x, y })
                .collect()
        },
        &board.protected,
        &board.enemy_sources,
        moves_first,
    );
    let layer_json = |layer: &Layer| {
        json!({
            "cells": positions_json(&layer.cells),
            "defense_sum": layer.defense_sum,
            "size": layer.cells.len(),
            "rejection": layer.rejection,
        })
    };
    let tiles: Vec<_> = tiles
        .iter()
        .map(|tile| {
            json!({
                "x": tile.pos.x,
                "y": tile.pos.y,
                "ours": tile.ours,
                "theirs": tile.theirs,
                "defense": tile.defense,
            })
        })
        .collect();
    Some(json!({
        "moves_first": moves_first,
        "strict_line": selection.strict_line().map(positions_json),
        "extended_line": selection.extended_line().map(positions_json),
        "boundary": layer_json(&selection.boundary),
        "enemy_side": layer_json(&selection.enemy_side),
        "tiles": tiles,
    }))
}

/// 敵の地上部隊（首都からの増援を含む）が各マスへ着くまでの手番数の最小値。
///
/// 陣地線の診断と同じ見積もり（総移動コスト÷移動力の近似）を、需要の有効期間の始端に使う。
pub(crate) fn enemy_ground_arrivals(
    world: &World,
    player_id: PlayerId,
) -> Option<HashMap<GridPosition, u32>> {
    let map = world.get_resource::<Map>()?;
    let master = world.get_resource::<MasterDataRegistry>()?;
    let board = GroundBoard::collect(world, player_id, master)?;
    let [_, theirs] = board.arrivals(map, master, player_id);
    Some(theirs)
}
