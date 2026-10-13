//! V2/V3と通常経路V4の行動選択。
//!
//! 行動可能な全unitの全移動先について、移動先そのものの価値（位置取り・目標への接近）を
//! 求め、その上で占領・攻撃・待機・合流の候補を評価して最良の1手を選ぶ。
//! 評価は (優先度, score) の辞書式比較で、同点は先に評価した候補を残す。

use super::approach::{ApproachDistances, approach_bonus, manhattan, nearest_approach_bonus};
use super::board::{TacticalBoard, movable_units_and_occupancy};
use crate::ai::engine::{
    AMBUSH_APPROACH_MARGIN, AMBUSH_IN_RANGE_BONUS, AMBUSH_NEAR_RANGE_BONUS,
    AMBUSH_TOO_CLOSE_PENALTY, ActionPriority, AiCommand, AiTurnStrategyCache,
    CampaignActionContext, air_move_preserves_return_fuel, campaign_action_context,
    campaign_mission_allows_capture, campaign_position_is_on_target_island, is_unit_stranded,
    strategic_target_value,
};
use crate::ai::squad::MissionType;
use crate::collections::HashSet;
use crate::components::{Faction, GridPosition, Health, PlayerId, UnitStats};
use crate::resources::{MovementType, Terrain, UnitType};
use crate::systems::action::TacticalActionTargets;
use crate::systems::movement::calculate_reachable_tiles;
use bevy_ecs::prelude::*;

/// 敵の脅威を「近い」とみなす格子距離（敵の移動+射程に余裕を持たせた半径）。
const THREAT_PROXIMITY_RADIUS: u32 = 8;
/// 占領行動の固定加点。
const CAPTURE_SCORE: i32 = 10_000;

type Rank = (ActionPriority, i32);

/// 行動可能な全unitから最良の1手を選び、評価内訳をEventで通知する。
pub(crate) fn decide_action(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let (movable_units, occupancy) = movable_units_and_occupancy(world, player_id, skip_entities);
    if movable_units.is_empty() {
        return None;
    }
    let board = TacticalBoard::observe(world, player_id, occupancy);
    // 自軍の移動は距離探索上の通行可否を変えない。敵が撃破されて占有マスが
    // 変わるまで、同一手番の全ユニットでDijkstra結果を共有する。
    let mut strategy_cache = world
        .remove_resource::<AiTurnStrategyCache>()
        .unwrap_or_default();
    let mut distance_cache =
        strategy_cache.take_action_distance_cache(player_id, &board.enemy_blocker_positions());
    let mut distances = ApproachDistances::new(&board, &mut distance_cache);

    let mut best: Option<(Rank, Entity, AiCommand)> = None;
    for entity in movable_units {
        let Some(unit) = UnitTactics::observe(world, &board, entity) else {
            continue;
        };
        if let Some((rank, command)) = best_action_for_unit(world, &board, &unit, &mut distances)
            && best
                .as_ref()
                .is_none_or(|(best_rank, ..)| rank > *best_rank)
        {
            best = Some((rank, entity, command));
        }
    }

    if let Some((rank, entity, command)) = &best {
        report_evaluated_action(world, &board, *entity, command, rank.1);
    }
    strategy_cache.put_action_distance_cache(distance_cache);
    world.insert_resource(strategy_cache);
    best.map(|(_, entity, command)| (entity, command))
}

/// 行動選択で使う1体のunitの事実。
struct UnitTactics {
    entity: Entity,
    stats: UnitStats,
    position: GridPosition,
    fuel: u32,
    hp: u32,
    ammo: (u32, u32),
    /// HP70未満、または主兵装の弾切れ。
    combat_ineffective: bool,
    /// V4の配備で指定された攻撃目標。
    deployment_target: Option<Entity>,
    /// V4のcampaign所属（作戦島・任務・目標）。
    campaign: Option<CampaignActionContext>,
    threat_nearby: bool,
    squad_target: Option<GridPosition>,
    has_offensive_mission: bool,
    /// Squad目標を持たない単独行動。
    is_solo: bool,
}

impl UnitTactics {
    /// 行動選択に必要な事実を集める。必要な状態が欠けているunit、またはcampaign所属に
    /// 具体Squadが無いunit（作戦再構築へ戻す）は、この手番の候補にしない。
    fn observe(world: &World, board: &TacticalBoard, entity: Entity) -> Option<Self> {
        let stats = world.get::<UnitStats>(entity)?.clone();
        let position = *world.get::<GridPosition>(entity)?;
        let fuel = world.get::<crate::components::Fuel>(entity)?.current;
        let hp = world.get::<Health>(entity)?.current;
        let ammo = world
            .get::<crate::components::Ammo>(entity)
            .map(|ammo| (ammo.ammo1, ammo.ammo2))
            .unwrap_or((99, 99));
        let combat_ineffective = hp < 70 || (stats.max_ammo1 > 0 && ammo.0 == 0);
        let deployment_target = world
            .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
            .and_then(|registry| registry.attack_target(entity));
        let campaign =
            campaign_action_context(world, board.player_id, entity, &board.squads.manager).ok()?;
        let threat_nearby = board.enemy_positions().any(|enemy| {
            (enemy.x.abs_diff(position.x) + enemy.y.abs_diff(position.y)) as u32
                <= THREAT_PROXIMITY_RADIUS
        });
        let squad_target = board.squads.target(entity);
        let has_offensive_mission = board.squads.mission(entity).is_some_and(|mission| {
            matches!(
                mission,
                MissionType::Attack | MissionType::Capture | MissionType::Transport
            )
        });
        let is_solo = board.squads.is_solo_fallback(entity) || squad_target.is_none();
        Some(Self {
            entity,
            stats,
            position,
            fuel,
            hp,
            ammo,
            combat_ineffective,
            deployment_target,
            campaign,
            threat_nearby,
            squad_target,
            has_offensive_mission,
            is_solo,
        })
    }

    /// 移動先候補。護衛待ちの占領役は、現在位置での攻撃・占領・待機だけを残す。
    fn candidate_tiles(&self, board: &TacticalBoard) -> std::collections::BTreeSet<(usize, usize)> {
        let waits_for_escort = self.campaign.as_ref().is_some_and(|context| {
            context.mission_type == MissionType::Capture && !context.departure_authorized
        });
        if waits_for_escort {
            return std::iter::once((self.position.x, self.position.y)).collect();
        }
        calculate_reachable_tiles(
            &board.map,
            &board.occupancy,
            (self.position.x, self.position.y),
            self.stats.movement_type,
            self.stats.max_movement,
            self.fuel,
            board.player_id,
            self.stats.unit_type,
            &board.registry,
        )
    }
}

/// 1つの移動先の事実。
struct TileCandidate {
    position: GridPosition,
    is_stationary: bool,
    actions: TacticalActionTargets,
    /// 移動先そのものの価値（位置取り・目標への接近）。各行動のscoreの土台になる。
    base_score: i32,
}

/// 1体のunitの全移動先・全行動のうち最良の (優先度, score) と命令。
fn best_action_for_unit(
    world: &World,
    board: &TacticalBoard,
    unit: &UnitTactics,
    distances: &mut ApproachDistances,
) -> Option<(Rank, AiCommand)> {
    let mut best = BestAction::default();
    for (x, y) in unit.candidate_tiles(board) {
        let position = GridPosition { x, y };
        if !air_move_preserves_return_fuel(
            &board.map,
            &board.properties,
            board.player_id,
            &unit.stats,
            unit.position,
            position,
            unit.fuel,
        ) {
            continue;
        }
        let is_stationary = position == unit.position;
        let actions = board.action_targets.action_targets_at(
            unit.entity,
            &unit.stats,
            position,
            !is_stationary,
        );
        let base_score = positional_score(board, unit, position)
            + objective_approach_score(world, board, unit, position, distances);
        let tile = TileCandidate {
            position,
            is_stationary,
            actions,
            base_score,
        };
        consider_capture(world, unit, &tile, &mut best);
        consider_attacks(world, board, unit, &tile, &mut best);
        consider_wait(board, unit, &tile, &mut best);
        consider_merges(world, unit, &tile, &mut best);
    }
    best.0
}

/// 評価済み候補の最良。同点は先に評価した候補を残す。
#[derive(Default)]
struct BestAction(Option<(Rank, AiCommand)>);

impl BestAction {
    fn offer(&mut self, rank: Rank, command: impl FnOnce() -> AiCommand) {
        if self.0.as_ref().is_none_or(|(best, _)| rank > *best) {
            self.0 = Some((rank, command()));
        }
    }
}

/// 移動先の地形・脅威に基づく位置取りの価値。
fn positional_score(board: &TacticalBoard, unit: &UnitTactics, position: GridPosition) -> i32 {
    let stats = &unit.stats;
    let defense_bonus = board
        .map
        .get_terrain(position.x, position.y)
        .map(|terrain| board.registry.get_terrain_defense_bonus(terrain))
        .unwrap_or(0);
    let mut score = defense_bonus as i32 * 10;
    if !board.uses_v3_tactics {
        return score;
    }
    // #44: HPが低下しているほど防御地形を高く評価する。ただし敵の脅威が近い場合に限る
    // （安全な後方で損傷unitが森へ引きこもり、前線合流が遅れるのを防ぐ）。
    if unit.hp < 70 && unit.threat_nearby {
        score += defense_bonus as i32 * (100 - unit.hp as i32) * 2;
    }
    // #50: 敵攻撃unitの脅威圏に入るマスは、期待被弾価値に応じて減点する。撃破・占領など
    // リターンの大きい行動は行動側の加点で相殺される。
    score -= crate::ai::threat::exposure_penalty(
        &board.map,
        (position.x, position.y),
        stats.unit_type,
        stats.cost,
        unit.hp,
        stats.min_range,
        defense_bonus,
        &board.enemies,
        &board.damage_chart,
    );
    // #45: 間接攻撃unitは、射程内や敵の接近を待ち受けられる位置で待機し、
    // 最小射程の内側へは前進しない。
    if stats.min_range > 1 && !unit.combat_ineffective && !board.enemies.is_empty() {
        let nearest_enemy = board
            .enemy_positions()
            .map(|enemy| board.map.distance(enemy.x, enemy.y, position.x, position.y))
            .min()
            .unwrap_or(u32::MAX);
        if nearest_enemy < stats.min_range {
            score -= AMBUSH_TOO_CLOSE_PENALTY;
        } else if nearest_enemy <= stats.max_range {
            score += AMBUSH_IN_RANGE_BONUS;
        } else if nearest_enemy <= stats.max_range + AMBUSH_APPROACH_MARGIN {
            score += AMBUSH_NEAR_RANGE_BONUS;
        }
    }
    score
}

/// 移動先から、任務・状態に応じた目標への接近の価値。
fn objective_approach_score(
    world: &World,
    board: &TacticalBoard,
    unit: &UnitTactics,
    position: GridPosition,
    distances: &mut ApproachDistances,
) -> i32 {
    let stats = &unit.stats;
    let mut score = 0;
    // 1. Squad目標への接近。
    if !unit.is_solo
        && let Some(target) = unit.squad_target
    {
        let turns = distances.turns(stats, position, target, stats.max_range);
        score += approach_bonus(turns.turns, manhattan(position, target), stats.max_movement);
    }
    // 2. 単独行動: 戦闘不能なら回復拠点へ、健全な非占領unitなら敵へ近づく。
    if unit.is_solo {
        if unit.combat_ineffective {
            let nearest = distances.nearest(stats, position, board.own_repair_sites(stats), 0);
            score += nearest_approach_bonus(nearest, stats.max_movement);
        } else if !stats.can_capture {
            let nearest =
                distances.nearest(stats, position, board.enemy_positions(), stats.max_range);
            score += nearest_approach_bonus(nearest, stats.max_movement);
        }
    }
    // 3. 空の輸送役は自軍の生産施設へ戻る。
    let is_empty_transport = stats.max_cargo > 0
        && world
            .get::<crate::components::CargoCapacity>(unit.entity)
            .is_some_and(|cargo| cargo.loaded.is_empty());
    if is_empty_transport {
        let nearest = distances.nearest(stats, position, board.own_production_sites(), 0);
        score += nearest_approach_bonus(nearest, stats.max_movement);
    }
    // 4. 取り残された歩兵は輸送を待つ海岸へ寄る。座礁した戦闘車両に広げると、
    //    全unitが海岸に密集して海峡越しの交戦誤判定を招いたため、歩兵に限る。
    let is_infantry = matches!(stats.unit_type, UnitType::Infantry | UnitType::Mech);
    if is_infantry
        && !unit.combat_ineffective
        && is_unit_stranded(
            world,
            &unit.position,
            board.player_id,
            &board.properties,
            &board.enemies,
        )
    {
        score += coast_approach_score(board, unit, position, distances);
    }
    // 5. 占領能力（搭載中の占領兵を含む）があれば未所有物件へ、単独の戦闘unitは敵へ。
    //    #53: Squad所属の占領unitは部隊目標への接近だけに従う。最寄り物件への引力を
    //    併用すると前線都市へ引き戻され、後方の敵生産施設を狙う部隊が機能しない。
    if can_capture_or_carries_capturer(world, unit) && (!board.uses_v3_tactics || unit.is_solo) {
        let nearest =
            distances.nearest(stats, position, board.unowned_properties(), stats.max_range);
        score += nearest_approach_bonus(nearest, stats.max_movement);
    } else if unit.is_solo {
        score += solo_hunt_score(board, unit, position, distances);
    }
    score
}

/// 輸送を待てる海岸（10マス以内の海に隣接する地点）へのターン距離による加点。
fn coast_approach_score(
    board: &TacticalBoard,
    unit: &UnitTactics,
    position: GridPosition,
    distances: &mut ApproachDistances,
) -> i32 {
    const COAST_SEARCH_RADIUS: usize = 10;
    let map = &board.map;
    let mut nearest_coast = u32::MAX;
    let min_x = position.x.saturating_sub(COAST_SEARCH_RADIUS);
    let max_x = (position.x + COAST_SEARCH_RADIUS).min(map.width - 1);
    let min_y = position.y.saturating_sub(COAST_SEARCH_RADIUS);
    let max_y = (position.y + COAST_SEARCH_RADIUS).min(map.height - 1);
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if map.get_terrain(x, y) == Some(Terrain::Sea) {
                let turns = distances.turns(&unit.stats, position, GridPosition { x, y }, 0);
                nearest_coast = nearest_coast.min(turns.turns);
            }
        }
    }
    if nearest_coast < 99 && nearest_coast > 0 {
        (100 - nearest_coast as i32).max(0) * 100
    } else {
        0
    }
}

fn can_capture_or_carries_capturer(world: &World, unit: &UnitTactics) -> bool {
    unit.stats.can_capture
        || world
            .get::<crate::components::CargoCapacity>(unit.entity)
            .is_some_and(|cargo| {
                cargo.loaded.iter().any(|loaded| {
                    world
                        .get::<UnitStats>(*loaded)
                        .is_some_and(|stats| stats.can_capture)
                })
            })
}

/// 単独行動の戦闘unitが、攻撃効果の最も大きい敵へ近づく価値。
///
/// 攻撃効果は 基礎damage × 敵価値 × 敵の損傷度 で比べる（0.1未満の差は同等とみなし、
/// 近い方を選ぶ）。攻撃できる敵がいなければ最寄りの敵、敵へ届かなければ未所有物件へ
/// 近づく。間接攻撃unitは標的を射程に収める位置を最も高く評価する。
fn solo_hunt_score(
    board: &TacticalBoard,
    unit: &UnitTactics,
    position: GridPosition,
    distances: &mut ApproachDistances,
) -> i32 {
    let stats = &unit.stats;
    let mut score = 0;
    let mut best_target_turns: i32 = 999;
    let mut best_target_position = None;
    let mut max_potential = -1.0;
    for (enemy_position, enemy_type, enemy_value, enemy_hp, ..) in &board.enemies {
        let turns = ship_adjusted_turns(board, unit, position, *enemy_position, distances);
        let base_damage = board
            .damage_chart
            .get_base_damage(stats.unit_type, *enemy_type)
            .or_else(|| {
                board
                    .damage_chart
                    .get_base_damage_secondary(stats.unit_type, *enemy_type)
            })
            .unwrap_or(0);
        let potential =
            base_damage as f32 * (*enemy_value as f32 / 100.0) * (2.0 - *enemy_hp as f32 / 100.0);
        if potential > max_potential {
            max_potential = potential;
            best_target_turns = turns as i32;
            best_target_position = Some(*enemy_position);
        } else if (potential - max_potential).abs() < 0.1 && (turns as i32) < best_target_turns {
            best_target_turns = turns as i32;
            best_target_position = Some(*enemy_position);
        }
    }

    if max_potential <= 0.0 {
        // 攻撃効果のある敵がいなければ最寄りの敵、届かなければ未所有物件へ近づく。
        let mut nearest: Option<(crate::ai::turn_distance::TurnDistance, i32)> = None;
        for (enemy_position, ..) in &board.enemies {
            let mut turns = distances.turns(stats, position, *enemy_position, stats.max_range);
            turns.turns += ship_blocked_penalty(board, unit, *enemy_position);
            let candidate = (turns, manhattan(position, *enemy_position));
            if nearest.is_none_or(|current| candidate < current) {
                nearest = Some(candidate);
                best_target_position = Some(*enemy_position);
            }
        }
        if nearest.is_none_or(|(turns, _)| turns.turns >= 99) {
            for property in board.unowned_properties() {
                let candidate = (
                    distances.turns(stats, position, property, 0),
                    manhattan(position, property),
                );
                if nearest.is_none_or(|current| candidate < current) {
                    nearest = Some(candidate);
                    best_target_position = Some(property);
                }
            }
        }
        if let Some((turns, grid_distance)) = nearest {
            best_target_turns = turns.turns as i32;
            if turns.turns < 99 {
                score += approach_bonus(turns.turns, grid_distance, stats.max_movement);
            }
        }
    }

    let approach = (100 - best_target_turns).max(0) * 100;
    if stats.min_range > 1
        && let Some(target) = best_target_position
    {
        let grid_distance = manhattan(position, target);
        if grid_distance >= stats.min_range as i32 && grid_distance <= stats.max_range as i32 {
            // 標的を射程に収める射撃位置。
            score += 10_000;
        } else if grid_distance < stats.min_range as i32 {
            // 最小射程の内側では撃てない。
            score -= 2000;
        } else {
            score += approach;
        }
    } else {
        score += approach;
    }
    score
}

/// 艦船の直接攻撃unitは陸上の敵へ届かない。ターン距離に20ターンを足して後回しにする。
fn ship_blocked_penalty(board: &TacticalBoard, unit: &UnitTactics, target: GridPosition) -> u32 {
    if unit.stats.movement_type != MovementType::Ship || unit.stats.max_range > 1 {
        return 0;
    }
    let Some(terrain) = board.map.get_terrain(target.x, target.y) else {
        return 0;
    };
    let move_cost = board
        .registry
        .get_movement_cost(MovementType::Ship, terrain.as_str())
        .unwrap_or(99);
    if move_cost >= 99 { 20 } else { 0 }
}

fn ship_adjusted_turns(
    board: &TacticalBoard,
    unit: &UnitTactics,
    from: GridPosition,
    target: GridPosition,
    distances: &mut ApproachDistances,
) -> u32 {
    distances
        .turns(&unit.stats, from, target, unit.stats.max_range)
        .turns
        + ship_blocked_penalty(board, unit, target)
}

/// 占領。campaign所属unitは、占領を許す任務で作戦島の上にいる場合だけ占領する。
fn consider_capture(
    world: &World,
    unit: &UnitTactics,
    tile: &TileCandidate,
    best: &mut BestAction,
) {
    if !tile.actions.can_capture {
        return;
    }
    let outside_campaign = unit.campaign.as_ref().is_some_and(|context| {
        !campaign_mission_allows_capture(&context.mission_type)
            || !campaign_position_is_on_target_island(world, context, tile.position)
    });
    if outside_campaign {
        return;
    }
    best.offer(
        (ActionPriority::Normal, tile.base_score + CAPTURE_SCORE),
        || AiCommand::Capture {
            target_pos: tile.position,
        },
    );
}

/// 攻撃。交換予測で期待damageが0の標的、作戦上の必要がない不利交換は候補にしない。
fn consider_attacks(
    world: &World,
    board: &TacticalBoard,
    unit: &UnitTactics,
    tile: &TileCandidate,
    best: &mut BestAction,
) {
    for target_entity in tile.actions.attackable_targets.iter().copied() {
        if !attack_serves_campaign(world, unit, target_entity) {
            continue;
        }
        let Some(exchange) = crate::ai::pruning::evaluate_attack_exchange(
            world,
            unit.entity,
            target_entity,
            tile.position,
            &board.damage_chart,
        ) else {
            continue;
        };
        let (Some(target_stats), Some(target_health), Some(target_position), Some(target_faction)) = (
            world.get::<UnitStats>(target_entity),
            world.get::<Health>(target_entity),
            world.get::<GridPosition>(target_entity),
            world.get::<Faction>(target_entity),
        ) else {
            continue;
        };
        // 撃破判定・damage期待値は、移動後位置を渡した交換予測を正本とする。
        let expected_damage = exchange.expected_damage;
        if expected_damage == 0 {
            continue;
        }
        let target_value = strategic_target_value(
            target_stats,
            *target_position,
            target_faction.0,
            world.get::<crate::components::CargoCapacity>(target_entity),
            &board.unit_costs,
            &board.properties,
            &board.registry,
        );
        let mut attack_score = 2000 + (expected_damage.saturating_mul(target_value) / 100) as i32;
        // 金額ROIを禁止条件にはしない。同じ作戦島で複数の敵を撃てる場合だけ、
        // 双方のHP損耗率差を使って相性のよい対象へ戦術的に切り替える。
        attack_score += exchange.matchup_margin() / 2;
        if unit.combat_ineffective && expected_damage < target_health.current {
            attack_score -= 3000;
        }
        if expected_damage >= target_health.current {
            attack_score += 5000;
        }
        let Some(priority) = attack_priority(unit, target_entity, exchange.is_favorable_matchup())
        else {
            continue;
        };
        best.offer((priority, tile.base_score + attack_score), || {
            AiCommand::Attack {
                target_pos: tile.position,
                target_entity,
            }
        });
    }
}

/// campaign所属unitは、輸送任務中は攻撃せず、配備目標か作戦島の上の敵だけを攻撃する。
fn attack_serves_campaign(world: &World, unit: &UnitTactics, target: Entity) -> bool {
    !unit.campaign.as_ref().is_some_and(|context| {
        let target_position = world.get::<GridPosition>(target).copied();
        context.mission_type == MissionType::Transport
            || (unit.deployment_target != Some(target)
                && !target_position.is_some_and(|position| {
                    campaign_position_is_on_target_island(world, context, position)
                }))
    })
}

/// 攻撃の優先度。V4はEntity単位の配備目標、V2/V3は攻勢Squad自体を戦略上の必要性とする。
/// 上陸cargoはSquad再編の境界でCapture・Transport・Attackのいずれにもなり得るため、
/// その差で必要攻撃を非決定的に枝刈りしてはならない。作戦上の必要性がない不利交換はNone。
fn attack_priority(
    unit: &UnitTactics,
    target: Entity,
    favorable_matchup: bool,
) -> Option<ActionPriority> {
    let is_strategic_target = unit
        .deployment_target
        .map_or(unit.has_offensive_mission, |deployment| {
            deployment == target
        });
    let has_strategic_mission = unit.deployment_target.is_some() || unit.has_offensive_mission;
    match (is_strategic_target, favorable_matchup) {
        (true, true) => Some(ActionPriority::FavorableStrategicTarget),
        (false, true) if has_strategic_mission => Some(ActionPriority::FavorableLocalTarget),
        // 生産・集結を終えて実行段階の作戦Entityは、他に有利な敵がいない場合だけ
        // 不利交換も受容する。ROIだけで戦略を停止しない。
        (true, false) => Some(ActionPriority::StrategicTargetFallback),
        (false, false) => None,
        (false, true) => Some(ActionPriority::Normal),
    }
}

/// 待機。回復の必要があれば回復拠点で待ち、不要なら生産施設を塞がない。
/// campaign所属unitは、作戦目標から遠ざかる移動で待機しない。
fn consider_wait(
    board: &TacticalBoard,
    unit: &UnitTactics,
    tile: &TileCandidate,
    best: &mut BestAction,
) {
    if !tile.actions.can_wait {
        return;
    }
    let repair_site = board.properties.iter().find(|(position, terrain, owner)| {
        *position == tile.position
            && *owner == Some(board.player_id)
            && board
                .registry
                .can_repair_on_terrain(unit.stats.unit_type, *terrain)
    });
    let is_on_recovery_property = repair_site.is_some();
    let is_on_production_site = repair_site
        .is_some_and(|(_, terrain, _)| board.registry.is_production_facility(terrain.as_str()));
    let mut score = tile.base_score;
    if is_on_recovery_property {
        if unit.combat_ineffective {
            score += 8000;
        } else if unit.hp < 100 || unit.ammo.0 < unit.stats.max_ammo1 {
            score += 1000;
        } else if is_on_production_site {
            // 回復も攻撃も不要なのに空港・工場上で待機すると、次の生産を物理的に封鎖する。
            score -= 20_000;
        } else {
            // 生産施設ではない回復拠点も、用がなければ占有し続けない。
            score -= 2000;
        }
    } else if unit.combat_ineffective {
        score -= 5000;
    }

    let moves_away_from_campaign = unit.campaign.as_ref().is_some_and(|context| {
        if tile.is_stationary || is_on_recovery_property && unit.combat_ineffective {
            return false;
        }
        context.target.is_none_or(|target| {
            board
                .map
                .distance(tile.position.x, tile.position.y, target.x, target.y)
                > board
                    .map
                    .distance(unit.position.x, unit.position.y, target.x, target.y)
        })
    });
    if moves_away_from_campaign {
        return;
    }
    best.offer((ActionPriority::Normal, score), || AiCommand::Wait {
        target_pos: tile.position,
    });
}

/// 合流。損耗した側を立て直す合流を優先し、HPが溢れて返金のない合流はしない。
/// campaign所属unitは同じ作戦の味方とだけ合流する。
fn consider_merges(world: &World, unit: &UnitTactics, tile: &TileCandidate, best: &mut BestAction) {
    for target_entity in tile.actions.mergeable_targets.iter().copied() {
        let other_operation = unit.campaign.as_ref().is_some_and(|context| {
            world
                .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
                .and_then(|assignments| assignments.assignment(target_entity))
                .is_none_or(|assignment| assignment.owner != context.owner)
        });
        if other_operation {
            continue;
        }
        let (Some(target_health), Some(_)) = (
            world.get::<Health>(target_entity),
            world.get::<UnitStats>(target_entity),
        ) else {
            continue;
        };
        if crate::ai::pruning::is_overflow_merge_without_refund(unit.hp, *target_health) {
            continue;
        }
        let mut merge_score = 3000;
        if unit.combat_ineffective || target_health.current < 40 {
            merge_score += 4000;
        }
        if unit.hp + target_health.current <= target_health.max {
            merge_score += 1000;
        }
        best.offer(
            (ActionPriority::Normal, tile.base_score + merge_score),
            || AiCommand::Merge {
                target_pos: tile.position,
                target_entity,
            },
        );
    }
}

/// AIが決定した最善の行動の評価をEventとして送出する。
fn report_evaluated_action(
    world: &mut World,
    board: &TacticalBoard,
    entity: Entity,
    command: &AiCommand,
    score: i32,
) {
    let mission_type = board.squads.mission_label(entity);
    if let Some(mut events) =
        world.get_resource_mut::<Events<crate::events::AiActionEvaluatedEvent>>()
    {
        events.send(crate::events::AiActionEvaluatedEvent {
            entity,
            mission_type,
            action_type: format!("{:?}", command),
            score,
        });
    }
}
