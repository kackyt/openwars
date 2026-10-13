//! V2/V3と通常経路V4の行動選択。
//!
//! 行動可能な全unitの全移動先について、移動先そのものの価値（位置取り・目標への接近）を
//! 求め、その上で占領・攻撃・待機・合流の候補を評価して最良の1手を選ぶ。
//! 評価は (優先度, score) の辞書式比較で、同点は先に評価した候補を残す。

use super::approach::{ApproachDistances, nearest_approach_bonus, with_turn_distances};
use super::board::{SquadRoles, TacticalBoard, movable_units_and_occupancy};
use super::scoring::{
    BestAction, BestUnitAction, CAPTURE_SCORE, assess_attack, assess_merge, attack_serves_campaign,
    capture_or_hunt_score, empty_transport_return_score, repair_site_at, solo_enemy_approach_score,
    squad_target_approach_score, stranded_infantry_score, terrain_defense_bonus,
    v3_positioning_score, wait_recovery_score,
};
use super::unit::UnitState;
use crate::ai::engine::{
    ActionPriority, AiCommand, CampaignActionContext, air_move_preserves_return_fuel,
    campaign_action_context, campaign_mission_allows_capture,
    campaign_position_is_on_target_island,
};
use crate::ai::squad::{MissionType, SquadManager};
use crate::collections::HashSet;
use crate::components::{GridPosition, PlayerId};
use crate::systems::action::TacticalActionTargets;
use crate::systems::movement::calculate_reachable_tiles;
use bevy_ecs::prelude::*;

/// 行動可能な全unitから最良の1手を選び、評価内訳をEventで通知する。
pub(crate) fn decide_action(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let (movable_units, occupancy) =
        movable_units_and_occupancy(world, player_id, skip_entities, None);
    if movable_units.is_empty() {
        return None;
    }
    let squads = SquadRoles::from_manager(
        world
            .get_resource::<SquadManager>()
            .cloned()
            .unwrap_or_default(),
    );
    let board = TacticalBoard::observe(world, player_id, occupancy, squads);
    let best = with_turn_distances(world, &board, |world, distances| {
        let mut best = BestUnitAction::default();
        for entity in movable_units {
            let Some(unit) = UnitTactics::observe(world, &board, entity) else {
                continue;
            };
            best.offer(
                entity,
                best_action_for_unit(world, &board, &unit, distances),
            );
        }
        best
    });
    best.choose(world, &board)
}

/// 行動選択で使う1体のunitの事実。
struct UnitTactics {
    state: UnitState,
    /// V4の配備で指定された攻撃目標。
    deployment_target: Option<Entity>,
    /// V4のcampaign所属（作戦島・任務・目標）。
    campaign: Option<CampaignActionContext>,
    squad_target: Option<GridPosition>,
    has_offensive_mission: bool,
    /// Squad目標を持たない単独行動。
    is_solo: bool,
}

impl UnitTactics {
    /// 必要な状態が欠けているunit、またはcampaign所属に具体Squadが無いunit
    /// （作戦再構築へ戻す）は、この手番の候補にしない。
    fn observe(world: &World, board: &TacticalBoard, entity: Entity) -> Option<Self> {
        let state = UnitState::observe(world, board, entity)?;
        let deployment_target = world
            .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
            .and_then(|registry| registry.attack_target(entity));
        let campaign =
            campaign_action_context(world, board.player_id, entity, &board.squads.manager).ok()?;
        let squad_target = board.squads.target(entity);
        let has_offensive_mission = board.squads.mission(entity).is_some_and(|mission| {
            matches!(
                mission,
                MissionType::Attack | MissionType::Capture | MissionType::Transport
            )
        });
        let is_solo = board.squads.is_solo_fallback(entity) || squad_target.is_none();
        Some(Self {
            state,
            deployment_target,
            campaign,
            squad_target,
            has_offensive_mission,
            is_solo,
        })
    }

    /// 移動先候補。護衛待ちの占領役は、現在位置での攻撃・占領・待機だけを残す。
    fn candidate_tiles(&self, board: &TacticalBoard) -> std::collections::BTreeSet<(usize, usize)> {
        let unit = &self.state;
        let waits_for_escort = self.campaign.as_ref().is_some_and(|context| {
            context.mission_type == MissionType::Capture && !context.departure_authorized
        });
        if waits_for_escort {
            return std::iter::once((unit.position.x, unit.position.y)).collect();
        }
        calculate_reachable_tiles(
            &board.map,
            &board.occupancy,
            (unit.position.x, unit.position.y),
            unit.stats.movement_type,
            unit.stats.max_movement,
            unit.fuel,
            board.player_id,
            unit.stats.unit_type,
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
) -> BestAction {
    let state = &unit.state;
    let mut best = BestAction::default();
    for (x, y) in unit.candidate_tiles(board) {
        let position = GridPosition { x, y };
        if !air_move_preserves_return_fuel(
            &board.map,
            &board.properties,
            board.player_id,
            &state.stats,
            state.position,
            position,
            state.fuel,
        ) {
            continue;
        }
        let is_stationary = position == state.position;
        let actions = board.action_targets.action_targets_at(
            state.entity,
            &state.stats,
            position,
            !is_stationary,
        );
        let tile = TileCandidate {
            position,
            is_stationary,
            actions,
            base_score: tile_score(world, board, unit, position, distances),
        };
        consider_capture(world, unit, &tile, &mut best);
        consider_attacks(world, board, unit, &tile, &mut best);
        consider_wait(board, unit, &tile, &mut best);
        consider_merges(world, unit, &tile, &mut best);
    }
    best
}

/// 移動先そのものの価値: 地形・脅威に基づく位置取りと、任務・状態に応じた目標への接近。
fn tile_score(
    world: &World,
    board: &TacticalBoard,
    unit: &UnitTactics,
    position: GridPosition,
    distances: &mut ApproachDistances,
) -> i32 {
    let state = &unit.state;
    let defense_bonus = terrain_defense_bonus(board, position);
    let mut score = defense_bonus as i32 * 10;
    if board.uses_v3_tactics {
        score += v3_positioning_score(board, state, position, defense_bonus);
    }
    if !unit.is_solo
        && let Some(target) = unit.squad_target
    {
        score += squad_target_approach_score(state, position, target, distances);
    }
    // 単独行動: 戦闘不能なら回復拠点へ、健全な非占領unitなら敵へ近づく。
    if unit.is_solo {
        if state.combat_ineffective {
            let nearest = distances.nearest(
                &state.stats,
                position,
                board.own_repair_sites(&state.stats),
                0,
            );
            score += nearest_approach_bonus(nearest, state.stats.max_movement);
        } else if !state.stats.can_capture {
            score += solo_enemy_approach_score(board, state, position, distances);
        }
    }
    score += empty_transport_return_score(world, board, state, position, distances);
    score += stranded_infantry_score(world, board, state, position, distances);
    score += capture_or_hunt_score(world, board, state, unit.is_solo, position, distances);
    score
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
    for target in tile.actions.attackable_targets.iter().copied() {
        if !attack_serves_campaign(
            world,
            unit.campaign.as_ref(),
            unit.deployment_target,
            target,
        ) {
            continue;
        }
        let Some(exchange) = crate::ai::pruning::evaluate_attack_exchange(
            world,
            unit.state.entity,
            target,
            tile.position,
            &board.damage_chart,
        ) else {
            continue;
        };
        let Some(attack) = assess_attack(world, board, &unit.state, target, &exchange) else {
            continue;
        };
        let Some(priority) = attack_priority(unit, target, exchange.is_favorable_matchup()) else {
            continue;
        };
        best.offer((priority, tile.base_score + attack.score), || {
            AiCommand::Attack {
                target_pos: tile.position,
                target_entity: target,
            }
        });
    }
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

/// 待機。campaign所属unitは、作戦目標から遠ざかる移動で待機しない。
fn consider_wait(
    board: &TacticalBoard,
    unit: &UnitTactics,
    tile: &TileCandidate,
    best: &mut BestAction,
) {
    if !tile.actions.can_wait {
        return;
    }
    let state = &unit.state;
    let repair_site = repair_site_at(board, state, tile.position);
    let score = tile.base_score + wait_recovery_score(state, repair_site);
    let moves_away_from_campaign = unit.campaign.as_ref().is_some_and(|context| {
        if tile.is_stationary || repair_site.is_some() && state.combat_ineffective {
            return false;
        }
        context.target.is_none_or(|target| {
            board
                .map
                .distance(tile.position.x, tile.position.y, target.x, target.y)
                > board
                    .map
                    .distance(state.position.x, state.position.y, target.x, target.y)
        })
    });
    if moves_away_from_campaign {
        return;
    }
    best.offer((ActionPriority::Normal, score), || AiCommand::Wait {
        target_pos: tile.position,
    });
}

/// 合流。campaign所属unitは同じ作戦の味方とだけ合流する。
fn consider_merges(world: &World, unit: &UnitTactics, tile: &TileCandidate, best: &mut BestAction) {
    for target in tile.actions.mergeable_targets.iter().copied() {
        let Some((merge_score, _)) =
            assess_merge(world, &unit.state, unit.campaign.as_ref(), target)
        else {
            continue;
        };
        best.offer(
            (ActionPriority::Normal, tile.base_score + merge_score),
            || AiCommand::Merge {
                target_pos: tile.position,
                target_entity: target,
            },
        );
    }
}
