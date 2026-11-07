//! 診断専用: 同一局面から攻略候補を実エンジンで分岐比較する。
//!
//! 通常AIからは呼ばない。セーブに残らない作戦記憶は全分岐で同じく初期化する。
//! 結果は短期・候補限定の比較であり、最適手や勝利を保証しない。
use crate::ai::command::AiCommand;
use crate::ai::turn_distance::{ActionTurnDistanceCache, calculate_action_distance_to_range};
use crate::collections::HashMap;
use crate::components::*;
use crate::events::*;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::*;
use crate::systems::action::{can_unit_move, is_unit_selectable};
use crate::systems::combat::get_attackable_targets_at;
use crate::systems::movement::{OccupantInfo, calculate_reachable_tiles};
use anyhow::{Result, ensure};
use bevy_ecs::event::EventCursor;
use bevy_ecs::prelude::*;
use serde::Serialize;
use std::cmp::Reverse;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProbePlan {
    Baseline,
    CounterBattery,
    Advance,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandRecord {
    pub unit: String,
    pub unit_type: String,
    pub origin: (usize, usize),
    pub destination: (usize, usize),
    pub target: Option<String>,
    pub action: String,
    pub accepted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttackRecord {
    pub turn: u32,
    pub attacker: String,
    pub player: u32,
    pub target: String,
    pub damage: u32,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct PositionMetrics {
    pub own_value: u64,
    pub enemy_value: u64,
    pub enemy_indirect_value: u64,
    pub blocked_production_sites: usize,
    pub capital_capture_remaining: u32,
    pub own_units_in_production_range: usize,
    pub front_distance: Option<u32>,
    pub winner: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BattleTotals {
    pub own_destroyed_value: u64,
    pub enemy_destroyed_value: u64,
    pub enemy_indirect_destroyed_value: u64,
    pub own_produced_value: u64,
    pub enemy_produced_value: u64,
    pub own_damage_value_dealt: u64,
    pub enemy_damage_value_dealt: u64,
    /// 実際に与えた損害価値−受けた損害価値。恣意的な重みは付けない。
    pub net_exchange_value: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanResult {
    pub plan: ProbePlan,
    pub initial: PositionMetrics,
    pub final_position: PositionMetrics,
    pub battle: BattleTotals,
    pub commands: Vec<CommandRecord>,
    pub attacks: Vec<AttackRecord>,
    pub comparison: String,
    pub completed_own_turns: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssaultReport {
    pub player: u32,
    pub objective: (usize, usize),
    pub own_turns: u32,
    pub results: Vec<PlanResult>,
    pub conclusion: String,
    pub limitations: Vec<String>,
}

#[derive(Clone)]
struct Mark {
    id: String,
    player: PlayerId,
    stats: UnitStats,
}

#[derive(Default)]
struct Audit {
    known: HashMap<Entity, Mark>,
    attacked: EventCursor<UnitAttackedEvent>,
    destroyed: EventCursor<UnitDestroyedEvent>,
    produced: EventCursor<UnitProducedEvent>,
    totals: BattleTotals,
    attacks: Vec<AttackRecord>,
}

impl Audit {
    fn remember(&mut self, world: &World) {
        for entity in world.iter_entities() {
            if let (Some(id), Some(faction), Some(stats)) = (
                entity.get::<UnitId>(),
                entity.get::<Faction>(),
                entity.get::<UnitStats>(),
            ) {
                self.known.insert(
                    entity.id(),
                    Mark {
                        id: id.0.to_string(),
                        player: faction.0,
                        stats: stats.clone(),
                    },
                );
            }
        }
    }

    fn apply(&mut self, world: &mut World, schedule: &mut Schedule, player: PlayerId) {
        self.remember(world);
        let turn = world.resource::<MatchState>().current_turn_number.0;
        schedule.run(world);
        self.remember(world);
        for event in self
            .attacked
            .read(world.resource::<Events<UnitAttackedEvent>>())
        {
            let (Some(attacker), Some(defender)) = (
                self.known.get(&event.attacker),
                self.known.get(&event.defender),
            ) else {
                continue;
            };
            let damage = u64::from(event.damage_dealt.min(event.defender_hp_before))
                * u64::from(defender.stats.cost)
                / 100;
            let counter = u64::from(
                event
                    .counter_damage_dealt
                    .unwrap_or(0)
                    .min(event.attacker_hp_before),
            ) * u64::from(attacker.stats.cost)
                / 100;
            if attacker.player == player {
                self.totals.own_damage_value_dealt += damage;
                self.totals.enemy_damage_value_dealt += counter;
            } else {
                self.totals.enemy_damage_value_dealt += damage;
                self.totals.own_damage_value_dealt += counter;
            }
            self.totals.net_exchange_value = self.totals.own_damage_value_dealt as i64
                - self.totals.enemy_damage_value_dealt as i64;
            self.attacks.push(AttackRecord {
                turn,
                attacker: attacker.id.clone(),
                player: attacker.player.0,
                target: defender.id.clone(),
                damage: event.damage_dealt,
            });
        }
        for event in self
            .destroyed
            .read(world.resource::<Events<UnitDestroyedEvent>>())
        {
            if let Some(unit) = self.known.get(&event.entity) {
                if unit.player == player {
                    self.totals.own_destroyed_value += u64::from(unit.stats.cost);
                } else {
                    self.totals.enemy_destroyed_value += u64::from(unit.stats.cost);
                    if unit.stats.min_range > 1 {
                        self.totals.enemy_indirect_destroyed_value += u64::from(unit.stats.cost);
                    }
                }
            }
        }
        for event in self
            .produced
            .read(world.resource::<Events<UnitProducedEvent>>())
        {
            if let Some(unit) = self.known.get(&event.entity) {
                if event.player_id == player {
                    self.totals.own_produced_value += u64::from(unit.stats.cost);
                } else {
                    self.totals.enemy_produced_value += u64::from(unit.stats.cost);
                }
            }
        }
    }
}

fn occupancy(world: &World) -> HashMap<(usize, usize), OccupantInfo> {
    world
        .iter_entities()
        .filter_map(|entity| {
            if entity.get::<Transporting>().is_some() {
                return None;
            }
            let position = entity.get::<GridPosition>()?;
            let faction = entity.get::<Faction>()?;
            let stats = entity.get::<UnitStats>()?;
            let capacity = entity.get::<CargoCapacity>();
            Some((
                (position.x, position.y),
                OccupantInfo {
                    player_id: faction.0,
                    is_transport: stats.max_cargo > 0,
                    unit_type: stats.unit_type,
                    loadable_types: stats.loadable_unit_types.clone(),
                    free_slots: capacity.map_or(stats.max_cargo, |cargo| {
                        cargo.max.saturating_sub(cargo.loaded.len() as u32)
                    }),
                },
            ))
        })
        .collect()
}

fn ground(stats: &UnitStats) -> bool {
    !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
}

fn available_units(world: &World, player: PlayerId, plan: ProbePlan) -> Vec<Entity> {
    let mut units = world
        .iter_entities()
        .filter(|entity| {
            let Some(stats) = entity.get::<UnitStats>() else {
                return false;
            };
            entity.get::<Faction>().is_some_and(|f| f.0 == player)
                && is_unit_selectable(world, entity.id())
                && ground(stats)
                && stats.max_range > 0
                && (plan == ProbePlan::CounterBattery || stats.min_range <= 1)
        })
        .map(|entity| entity.id())
        .collect::<Vec<_>>();
    // 間接火力を先に適用し、同じ敵へ重ね撃ちしないよう毎回残HPを読み直す。
    units.sort_by_key(|&entity| {
        let stats = world.get::<UnitStats>(entity).unwrap();
        (
            Reverse(stats.min_range > 1),
            world.get::<UnitId>(entity).unwrap().0,
        )
    });
    units
}

fn enemy_indirect(world: &World, player: PlayerId, objective: GridPosition) -> Vec<Entity> {
    world
        .iter_entities()
        .filter(|entity| {
            entity.get::<Faction>().is_some_and(|f| f.0 != player)
                && entity.get::<Transporting>().is_none()
                && entity.get::<Health>().is_some_and(|h| h.current > 0)
                && entity
                    .get::<UnitStats>()
                    .is_some_and(|s| ground(s) && s.min_range > 1)
                && entity
                    .get::<GridPosition>()
                    .zip(entity.get::<UnitStats>())
                    .is_some_and(|(p, s)| {
                        // 対象の生産圏へ射程が及び得る砲だけ。無関係な別戦線へ全軍を向けない。
                        world
                            .resource::<Map>()
                            .distance(p.x, p.y, objective.x, objective.y)
                            <= s.max_range + crate::systems::production::PRODUCTION_RANGE as u32
                    })
        })
        .map(|entity| entity.id())
        .collect()
}

fn reachable(world: &World, actor: Entity) -> Vec<GridPosition> {
    let stats = world.get::<UnitStats>(actor).unwrap();
    let position = *world.get::<GridPosition>(actor).unwrap();
    if !can_unit_move(world, actor) {
        return vec![position];
    }
    let occupancy = occupancy(world);
    let fuel = world.get::<Fuel>(actor).map_or(0, |fuel| fuel.current);
    calculate_reachable_tiles(
        world.resource::<Map>(),
        &occupancy,
        (position.x, position.y),
        stats.movement_type,
        stats.max_movement,
        fuel,
        world.get::<Faction>(actor).unwrap().0,
        stats.unit_type,
        world.resource::<MasterDataRegistry>(),
    )
    .into_iter()
    .filter(|cell| *cell == (position.x, position.y) || !occupancy.contains_key(cell))
    .map(|(x, y)| GridPosition { x, y })
    .collect()
}

/// 占有も含む実地形の接近距離。推定にすぎず、最終的な命令受理はエンジンで確認する。
fn distance_key(
    world: &World,
    actor: Entity,
    from: GridPosition,
    target: GridPosition,
    range: (u32, u32),
) -> Option<(u32, u32)> {
    let stats = world.get::<UnitStats>(actor)?;
    let fuel = world.get::<Fuel>(actor)?.current;
    let (min_range, max_range) = range;
    if fuel == 0 {
        let distance = world
            .resource::<Map>()
            .distance(from.x, from.y, target.x, target.y);
        return (world.get::<GridPosition>(actor) == Some(&from)
            && (min_range..=max_range).contains(&distance))
        .then_some((0, 0));
    }
    let occupancy = occupancy(world);
    let result = calculate_action_distance_to_range(
        world.resource::<Map>(),
        world.resource::<MasterDataRegistry>(),
        &occupancy,
        (from.x, from.y),
        (target.x, target.y),
        stats.movement_type,
        stats.max_movement,
        fuel,
        min_range,
        max_range,
        world.get::<Faction>(actor)?.0,
        &mut ActionTurnDistanceCache::default(),
    )?;
    Some((result.turns, result.used_mp))
}

/// 案ごとの接近先の範囲。対砲兵は自分の射程内、全体前進は敵首都の生産圏に入ること。
fn approach_range(plan: ProbePlan, stats: &UnitStats) -> (u32, u32) {
    match plan {
        ProbePlan::Advance => (0, crate::systems::production::PRODUCTION_RANGE as u32),
        _ => (stats.min_range, stats.max_range),
    }
}

fn attack_choice(
    world: &mut World,
    actor: Entity,
    cells: &[GridPosition],
    _player: PlayerId,
    plan: ProbePlan,
    objective: GridPosition,
) -> Option<(GridPosition, Entity)> {
    let current = *world.get::<GridPosition>(actor)?;
    let stats = world.get::<UnitStats>(actor)?;
    let approach = approach_range(plan, stats);
    let before = distance_key(world, actor, current, objective, approach);
    let mut best = None;
    for &position in cells {
        if plan == ProbePlan::Advance
            && (position == current
                || !distance_key(world, actor, position, objective, approach)
                    .zip(before)
                    .is_some_and(|(after, old)| after < old))
        {
            continue;
        }
        let allow_indirect =
            position == current && !world.get::<HasMoved>(actor).is_some_and(|moved| moved.0);
        for enemy in get_attackable_targets_at(world, actor, position, allow_indirect) {
            let stats = world.get::<UnitStats>(enemy)?;
            if plan == ProbePlan::CounterBattery
                && (stats.min_range <= 1
                    || !enemy_indirect(world, _player, objective).contains(&enemy))
            {
                continue;
            }
            let exchange = crate::ai::pruning::evaluate_attack_exchange(
                world,
                actor,
                enemy,
                position,
                world.resource::<DamageChart>(),
            )?;
            let hp = world.get::<Health>(enemy)?.current;
            let key = (
                exchange.expected_damage >= hp,
                exchange.damage_value_dealt,
                Reverse(exchange.counter_value_received),
                Reverse(world.resource::<Map>().distance(
                    position.x,
                    position.y,
                    objective.x,
                    objective.y,
                )),
                Reverse((position.x, position.y)),
                Reverse(world.get::<UnitId>(enemy)?.0),
            );
            if best.as_ref().is_none_or(|(old, _, _)| key > *old) {
                best = Some((key, position, enemy));
            }
        }
    }
    best.map(|(_, position, enemy)| (position, enemy))
}

fn move_choice(
    world: &World,
    actor: Entity,
    cells: &[GridPosition],
    player: PlayerId,
    plan: ProbePlan,
    objective: GridPosition,
    horizon: u32,
) -> Option<GridPosition> {
    let current = *world.get::<GridPosition>(actor)?;
    let stats = world.get::<UnitStats>(actor)?;
    let targets = if plan == ProbePlan::CounterBattery {
        enemy_indirect(world, player, objective)
            .into_iter()
            .filter_map(|enemy| world.get::<GridPosition>(enemy).copied())
            .collect::<Vec<_>>()
    } else {
        vec![objective]
    };
    let range = approach_range(plan, stats);
    let mut best = None;
    for target in targets {
        let Some(before) = distance_key(world, actor, current, target, range) else {
            continue;
        };
        // 対砲兵の接近だけは、2手番以内に着ける部隊に限る。全体前進は距離で除外しない。
        if plan == ProbePlan::CounterBattery && before.0 > horizon {
            continue;
        }
        for &cell in cells {
            if cell == current {
                continue;
            }
            let Some(after) = distance_key(world, actor, cell, target, range) else {
                continue;
            };
            if after >= before {
                continue;
            }
            let key = (after, cell.x, cell.y);
            if best.as_ref().is_none_or(|(old, _)| key < *old) {
                best = Some((key, cell));
            }
        }
    }
    best.map(|(_, cell)| cell)
}

fn issue_override(
    world: &mut World,
    schedule: &mut Schedule,
    audit: &mut Audit,
    player: PlayerId,
    actor: Entity,
    position: GridPosition,
    target: Option<Entity>,
) -> Result<CommandRecord> {
    ensure!(
        is_unit_selectable(world, actor),
        "行動済みの部隊を再選択した"
    );
    let origin = *world.get::<GridPosition>(actor).unwrap();
    let mark = Mark {
        id: world.get::<UnitId>(actor).unwrap().0.to_string(),
        player,
        stats: world.get::<UnitStats>(actor).unwrap().clone(),
    };
    let target_id =
        target.and_then(|entity| world.get::<UnitId>(entity).map(|id| id.0.to_string()));
    let start_attacks = audit.attacks.len();
    let capture = target.is_none()
        && mark.stats.can_capture
        && world.iter_entities().any(|entity| {
            entity.get::<GridPosition>() == Some(&position)
                && entity
                    .get::<Property>()
                    .is_some_and(|property| property.owner_id != Some(player))
        });
    let command = if let Some(enemy) = target {
        AiCommand::Attack {
            target_pos: position,
            target_entity: enemy,
        }
    } else if capture {
        AiCommand::Capture {
            target_pos: position,
        }
    } else {
        AiCommand::Wait {
            target_pos: position,
        }
    };
    crate::ai::execute_ai_command(world, actor, command);
    audit.apply(world, schedule, player);
    let moved = world
        .get::<GridPosition>(actor)
        .is_some_and(|p| *p == position);
    let accepted = if target.is_some() {
        audit.attacks[start_attacks..]
            .iter()
            .any(|attack| attack.attacker == mark.id)
            && (moved || world.get::<Health>(actor).is_none())
    } else {
        moved
            && world
                .get::<ActionCompleted>(actor)
                .is_some_and(|done| done.0)
    };
    ensure!(
        accepted,
        "診断命令が拒否された: {} {:?} -> {:?}",
        mark.id,
        origin,
        position
    );
    world.init_resource::<crate::ai::AiActionCooldown>();
    world
        .resource_mut::<crate::ai::AiActionCooldown>()
        .0
        .insert(actor);
    Ok(CommandRecord {
        unit: mark.id,
        unit_type: mark.stats.unit_type.as_str().to_owned(),
        origin: (origin.x, origin.y),
        destination: (position.x, position.y),
        target: target_id,
        action: if target.is_some() {
            "attack"
        } else if capture {
            "capture"
        } else {
            "move"
        }
        .to_owned(),
        accepted,
    })
}

fn apply_plan(
    world: &mut World,
    schedule: &mut Schedule,
    audit: &mut Audit,
    player: PlayerId,
    objective: GridPosition,
    plan: ProbePlan,
    horizon: u32,
) -> Result<Vec<CommandRecord>> {
    let mut records = Vec::new();
    if plan == ProbePlan::Baseline {
        return Ok(records);
    }
    for actor in available_units(world, player, plan) {
        if !is_unit_selectable(world, actor) || world.resource::<MatchState>().game_over.is_some() {
            continue;
        }
        let cells = reachable(world, actor);
        if std::env::var_os("ASSAULT_DEBUG").is_some() {
            let stats = world.get::<UnitStats>(actor).unwrap();
            let here = *world.get::<GridPosition>(actor).unwrap();
            let before = distance_key(world, actor, here, objective, approach_range(plan, stats));
            let better = cells
                .iter()
                .filter(|&&cell| {
                    distance_key(world, actor, cell, objective, approach_range(plan, stats))
                        .zip(before)
                        .is_some_and(|(after, old)| after < old)
                })
                .count();
            let terrain_ok = crate::ai::turn_distance::is_terrain_reachable(
                world.resource::<Map>(),
                world.resource::<MasterDataRegistry>(),
                (here.x, here.y),
                (objective.x, objective.y),
                stats.movement_type,
            );
            let free = calculate_action_distance_to_range(
                world.resource::<Map>(),
                world.resource::<MasterDataRegistry>(),
                &HashMap::default(),
                (here.x, here.y),
                (objective.x, objective.y),
                stats.movement_type,
                stats.max_movement,
                world.get::<Fuel>(actor).map_or(0, |f| f.current),
                0,
                crate::systems::production::PRODUCTION_RANGE as u32,
                world.get::<Faction>(actor).map_or(PlayerId(0), |f| f.0),
                &mut ActionTurnDistanceCache::default(),
            )
            .map(|r| r.turns);
            eprintln!("ASSAULT_DEBUG free_occ={:?}", free);
            eprintln!(
                "ASSAULT_DEBUG {:?} {:?} at=({},{}) obj=({},{}) terrain_ok={} before={:?} reachable={} better={} fuel={:?}",
                plan,
                stats.unit_type,
                here.x,
                here.y,
                objective.x,
                objective.y,
                terrain_ok,
                before,
                cells.len(),
                better,
                world.get::<Fuel>(actor).map(|f| f.current)
            );
        }
        if let Some((position, enemy)) =
            attack_choice(world, actor, &cells, player, plan, objective)
        {
            records.push(issue_override(
                world,
                schedule,
                audit,
                player,
                actor,
                position,
                Some(enemy),
            )?);
        } else if let Some(position) =
            move_choice(world, actor, &cells, player, plan, objective, horizon)
        {
            records.push(issue_override(
                world, schedule, audit, player, actor, position, None,
            )?);
        }
    }
    Ok(records)
}

fn active(world: &World) -> PlayerId {
    world.resource::<Players>().0[world.resource::<MatchState>().active_player_index.0].id
}

fn finish_turn(
    world: &mut World,
    schedule: &mut Schedule,
    audit: &mut Audit,
    player: PlayerId,
) -> Result<()> {
    let side = active(world);
    // 計算量の上限であり戦略の閾値ではない。超過時は成功扱いにしない。
    for _ in 0..2000 {
        if world.resource::<MatchState>().game_over.is_some() || active(world) != side {
            return Ok(());
        }
        crate::ai::execute_ai_turn(world, side);
        audit.apply(world, schedule, player);
    }
    anyhow::bail!("診断分岐の1手番が2000命令で終了しない")
}

fn metrics(world: &World, player: PlayerId, objective: GridPosition) -> PositionMetrics {
    let map = world.resource::<Map>();
    let master = world.resource::<MasterDataRegistry>();
    let mut result = PositionMetrics::default();
    for entity in world.iter_entities() {
        if let (Some(faction), Some(stats), Some(hp)) = (
            entity.get::<Faction>(),
            entity.get::<UnitStats>(),
            entity.get::<Health>(),
        ) {
            let value = u64::from(stats.cost) * u64::from(hp.current) / 100;
            if faction.0 == player {
                result.own_value += value;
            } else {
                result.enemy_value += value;
                if stats.min_range > 1 {
                    result.enemy_indirect_value += value;
                }
            }
            if faction.0 == player
                && entity.get::<Transporting>().is_none()
                && ground(stats)
                && stats.max_range > 0
                && let Some(p) = entity.get::<GridPosition>()
            {
                let distance = map.distance(p.x, p.y, objective.x, objective.y);
                if distance <= crate::systems::production::PRODUCTION_RANGE as u32 {
                    result.own_units_in_production_range += 1;
                }
                if stats.min_range <= 1 {
                    result.front_distance = Some(
                        result
                            .front_distance
                            .map_or(distance, |old| old.min(distance)),
                    );
                }
            }
        }
        if let (Some(position), Some(property)) =
            (entity.get::<GridPosition>(), entity.get::<Property>())
        {
            if *position == objective {
                result.capital_capture_remaining = property.capture_points;
            }
            let in_range = crate::systems::production::is_within_production_range(
                &[objective],
                position.x,
                position.y,
                map.topology,
            );
            if in_range && master.is_production_facility(property.terrain.as_str()) {
                let owned = property.owner_id == Some(player);
                let occupied = world.iter_entities().any(|unit| {
                    unit.get::<GridPosition>() == Some(position)
                        && unit.get::<Transporting>().is_none()
                        && unit.get::<Faction>().is_some_and(|f| f.0 == player)
                });
                if owned || occupied {
                    result.blocked_production_sites += 1;
                }
            }
        }
    }
    result.winner = match &world.resource::<MatchState>().game_over {
        Some(GameOverCondition::Winner(id)) => Some(id.0),
        _ => None,
    };
    result
}

/// 同じ入力に対する成果と実損耗を別軸で比較し、重み付き点数を捏造しない。
fn compare_outcomes(candidate: &PlanResult, baseline: &PlanResult, player: PlayerId) -> String {
    let a = &candidate.final_position;
    let b = &baseline.final_position;
    if a.winner.is_some_and(|winner| winner != player.0) && b.winner != a.winner {
        return "loss_in_horizon".into();
    }
    if b.winner.is_some_and(|winner| winner != player.0) && a.winner.is_none() {
        return "avoids_baseline_loss_in_horizon".into();
    }
    if a.winner == Some(player.0) && b.winner != Some(player.0) {
        return "win_in_horizon".into();
    }
    if b.winner == Some(player.0) && a.winner != Some(player.0) {
        return "worse_than_baseline_win".into();
    }
    let gain = [
        a.enemy_value <= b.enemy_value,
        a.enemy_indirect_value <= b.enemy_indirect_value,
        a.blocked_production_sites >= b.blocked_production_sites,
        a.capital_capture_remaining <= b.capital_capture_remaining,
        candidate.battle.net_exchange_value >= baseline.battle.net_exchange_value,
    ];
    let strict = [
        a.enemy_value < b.enemy_value,
        a.enemy_indirect_value < b.enemy_indirect_value,
        a.blocked_production_sites > b.blocked_production_sites,
        a.capital_capture_remaining < b.capital_capture_remaining,
        candidate.battle.net_exchange_value > baseline.battle.net_exchange_value,
    ];
    if gain.iter().all(|&v| v) {
        if strict.iter().any(|&v| v) {
            "dominates_baseline"
        } else {
            "equivalent_on_decision_axes"
        }
        .into()
    } else if !strict.iter().any(|&v| v) {
        "dominated_by_baseline".into()
    } else {
        "tradeoff_inspect_metrics".into()
    }
}

/// 各案へ同一セーブを復元し、自軍手番と敵の応答を実行して結果を返す。
pub fn compare_assaults(
    world: &mut World,
    master: &MasterDataRegistry,
    map_name: &str,
    player: PlayerId,
    objective: GridPosition,
    own_turns: u32,
) -> Result<AssaultReport> {
    compare_plans(
        world,
        master,
        map_name,
        player,
        objective,
        own_turns,
        &[
            ProbePlan::Baseline,
            ProbePlan::CounterBattery,
            ProbePlan::Advance,
        ],
    )
}

/// 指定した案だけを比較する。先頭は必ず基準（待つ）案。
pub fn compare_plans(
    world: &mut World,
    master: &MasterDataRegistry,
    map_name: &str,
    player: PlayerId,
    objective: GridPosition,
    own_turns: u32,
    plans: &[ProbePlan],
) -> Result<AssaultReport> {
    ensure!(
        plans.first() == Some(&ProbePlan::Baseline),
        "先頭は基準案にすること"
    );
    ensure!((1..=3).contains(&own_turns), "診断の自軍手番数は1〜3");
    ensure!(
        world.resource::<MatchState>().current_phase == Phase::Main && active(world) == player,
        "対象のMain手番から開始すること"
    );
    let saved = crate::serialize::export_save_data(world, map_name)?;
    let mut results = Vec::new();
    for &plan in plans {
        let (mut branch, mut schedule) = crate::serialize::import_save_data(&saved, master)?;
        let initial = metrics(&branch, player, objective);
        let mut audit = Audit::default();
        audit.remember(&branch);
        let commands = apply_plan(
            &mut branch,
            &mut schedule,
            &mut audit,
            player,
            objective,
            plan,
            own_turns,
        )?;
        let mut completed = 0;
        for _ in 0..own_turns {
            if branch.resource::<MatchState>().game_over.is_some() {
                break;
            }
            ensure!(active(&branch) == player, "分岐の手番境界が不整合");
            finish_turn(&mut branch, &mut schedule, &mut audit, player)?;
            completed += 1;
            if branch.resource::<MatchState>().game_over.is_none() {
                finish_turn(&mut branch, &mut schedule, &mut audit, player)?;
            }
        }
        results.push(PlanResult {
            plan,
            initial,
            final_position: metrics(&branch, player, objective),
            battle: audit.totals,
            commands,
            attacks: audit.attacks,
            comparison: String::new(),
            completed_own_turns: completed,
        });
    }
    let baseline = results[0].clone();
    for result in &mut results[1..] {
        result.comparison = compare_outcomes(result, &baseline, player);
    }
    results[0].comparison = "baseline".into();
    let supported = results[1..]
        .iter()
        .filter(|r| {
            matches!(
                r.comparison.as_str(),
                "dominates_baseline" | "win_in_horizon"
            )
        })
        .map(|r| format!("{:?}", r.plan))
        .collect::<Vec<_>>();
    Ok(AssaultReport {
        player: player.0,
        objective: (objective.x, objective.y),
        own_turns,
        results,
        conclusion: if supported.is_empty() {
            "この候補・期間では無条件に優越する攻略案なし".into()
        } else {
            format!("短期比較の攻略候補: {}", supported.join(", "))
        },
        limitations: vec![
            "全案同一セーブ・同一乱数状態だが作戦記憶は全案初期化".into(),
            "最初の自軍手番だけ介入し、以後は両軍の通常AI".into(),
            "対砲兵候補は現在の射撃と自力の接近のみ。砲台の新規輸送・降車計画は未探索".into(),
            "敵の応答は当該AIの実行であり、最悪応答の保証ではない".into(),
            "前進案の部隊順は固定。同時移動の最適な組合せ探索ではない".into(),
        ],
    })
}

#[cfg(test)]
mod tests;
