//! 版によらない採点項目と、候補の比較。
//!
//! scoreは同じ移動先の行動どうしを比べる相対値で、1ターン分の接近を1000点、
//! 占領を10000点、撃破を5000点などの目安で加減する。行動の優先度（[`ActionPriority`]）は
//! scoreより先に比べる別軸である。

use super::approach::{
    ApproachDistances, approach_bonus, manhattan, nearest_approach_bonus, path_approach_bonus,
};
use super::board::TacticalBoard;
use super::unit::UnitState;
use crate::ai::command::AiCommand;
use crate::ai::context::tactical::is_unit_stranded;
use crate::ai::context::tactical::strategic_target_value;
use crate::ai::tactics::campaign::CampaignActionContext;

use crate::ai::pruning::AttackExchange;
use crate::components::{Faction, GridPosition, Health, UnitStats};
use crate::resources::{MovementType, Terrain};
use bevy_ecs::prelude::*;

/// 占領行動の固定加点。
pub(crate) const CAPTURE_SCORE: i32 = 10_000;

/// (優先度, score)。辞書式に比べる。
pub(crate) type Rank = (ActionPriority, i32);

/// 評価済み候補の最良。同点は先に評価した候補を残す。
#[derive(Default)]
pub(crate) struct BestAction(Option<(Rank, AiCommand)>);

impl BestAction {
    pub(crate) fn offer(&mut self, rank: Rank, command: impl FnOnce() -> AiCommand) {
        if self.0.as_ref().is_none_or(|(best, _)| rank > *best) {
            self.0 = Some((rank, command()));
        }
    }

    /// 優先度・scoreを比べずに置き換える（後段の規則で選択を上書きする場合）。
    pub(crate) fn replace(&mut self, rank: Rank, command: AiCommand) {
        self.0 = Some((rank, command));
    }

    pub(crate) fn command(&self) -> Option<&AiCommand> {
        self.0.as_ref().map(|(_, command)| command)
    }

    pub(crate) fn into_inner(self) -> Option<(Rank, AiCommand)> {
        self.0
    }
}

/// 全unitを通じた最良の1手。
#[derive(Default)]
pub(crate) struct BestUnitAction(Option<(Rank, Entity, AiCommand)>);

impl BestUnitAction {
    pub(crate) fn offer(&mut self, entity: Entity, unit_best: BestAction) {
        if let Some((rank, command)) = unit_best.into_inner()
            && self
                .0
                .as_ref()
                .is_none_or(|(best_rank, ..)| rank > *best_rank)
        {
            self.0 = Some((rank, entity, command));
        }
    }

    /// 観測用の読取口。候補の再評価やcommand生成は行わない。
    pub(crate) fn evaluated_choice(&self) -> Option<(Rank, Entity, &AiCommand)> {
        self.0
            .as_ref()
            .map(|(rank, entity, command)| (*rank, *entity, command))
    }

    /// 最良の1手を確定し、その評価をEventで通知する。
    pub(crate) fn choose(
        self,
        world: &mut World,
        board: &TacticalBoard,
    ) -> Option<(Entity, AiCommand)> {
        let (rank, entity, command) = self.0?;
        report_evaluated_action(world, board, entity, &command, rank.1);
        Some((entity, command))
    }
}

pub(crate) fn terrain_defense_bonus(board: &TacticalBoard, position: GridPosition) -> u32 {
    board
        .map
        .get_terrain(position.x, position.y)
        .map(|terrain| board.registry.get_terrain_defense_bonus(terrain))
        .unwrap_or(0)
}

/// V3以降の位置取り（呼出元がV3以降に限って加える）。
///
/// - #44: HPが低下しているほど防御地形を高く評価する。ただし敵の脅威が近い場合に限る
///   （安全な後方で損傷unitが森へ引きこもり、前線合流が遅れるのを防ぐ）。
/// - #50: 敵攻撃unitの脅威圏に入るマスは、期待被弾価値に応じて減点する。撃破・占領など
///   リターンの大きい行動は行動側の加点で相殺される。
/// - #45: 間接攻撃unitは、射程内や敵の接近を待ち受けられる位置で待機し、
///   最小射程の内側へは前進しない。
pub(crate) fn v3_positioning_score(
    board: &TacticalBoard,
    unit: &UnitState,
    position: GridPosition,
    defense_bonus: u32,
) -> i32 {
    let stats = &unit.stats;
    let mut score = 0;
    if unit.hp < 70 && unit.threat_nearby {
        score += defense_bonus as i32 * (100 - unit.hp as i32) * 2;
    }
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

/// 地形防御の加点の上限（約半移動分の接近点）。高価なunitほど避けられる損失が大きく、
/// 上限が無いと防御地形に留まる価値が前進を上回り、進軍が止まる。
pub(crate) const DIRECT_THREAT_TERRAIN_CAP: i32 = 1000;

/// V4の通常経路で、直接攻撃unitが敵の直接攻撃圏で防御地形に止まる価値。
///
/// 直接攻撃unitは敵の直接攻撃圏への踏み込みを減点しない（[`v3_positioning_score`]）。
/// そのため地形防御の効果が `地形効果×10` の同点比較にしか残らず、前線で道路に止まる。
/// 圏内に限り、地形で避けられる期待損失を上限つきで加点し、同程度の前進なら防御地形で
/// 止まるようにする。小規模マップの経路では前進が遅れたため使わない。
pub(crate) fn direct_threat_terrain_score(
    board: &TacticalBoard,
    unit: &UnitState,
    position: GridPosition,
    defense_bonus: u32,
) -> i32 {
    let stats = &unit.stats;
    if !board.weighs_direct_threat_terrain || stats.min_range > 1 {
        return 0;
    }
    crate::ai::threat::direct_reach_terrain_relief(
        &board.map,
        (position.x, position.y),
        stats.unit_type,
        stats.cost,
        unit.hp,
        defense_bonus,
        &board.enemies,
        &board.damage_chart,
    )
    .min(DIRECT_THREAT_TERRAIN_CAP)
}

/// Squad目標への接近（到達可否によらず加点する）。
pub(crate) fn squad_target_approach_score(
    unit: &UnitState,
    position: GridPosition,
    target: GridPosition,
    distances: &mut ApproachDistances,
) -> i32 {
    let stats = &unit.stats;
    let turns = distances.turns(stats, position, target, stats.max_range);
    approach_bonus(turns.turns, manhattan(position, target), stats.max_movement)
}

/// Squad目標への実経路での接近（V4）。射程に捉えるまでの経路の残り移動コストで比べる。
pub(crate) fn squad_target_path_approach_score(
    unit: &UnitState,
    position: GridPosition,
    target: GridPosition,
    distances: &mut ApproachDistances,
) -> i32 {
    let stats = &unit.stats;
    let turns = distances.turns(stats, position, target, stats.max_range);
    path_approach_bonus(turns, manhattan(position, target), stats.max_movement)
}

/// campaignの非占領unitにとってのSquad目標は、全員が重なる駐留マスではなく、
/// 部隊が集合して戦う参照点である。自分の射程で参照点に働ける距離までは同点とし、
/// 中心1マスだけへ吸着させない。そこから先の位置取りは地形・脅威・攻撃の評価に委ねる。
pub(crate) fn reference_point_approach_score(
    unit: &UnitState,
    position: GridPosition,
    target: GridPosition,
    distances: &mut ApproachDistances,
) -> i32 {
    let stats = &unit.stats;
    let turns = distances.turns(stats, position, target, stats.max_range);
    let outside_reach = (manhattan(position, target) - reference_reach(stats) as i32).max(0);
    approach_bonus(turns.turns, outside_reach, stats.max_movement)
}

/// 参照点に対して働ける距離。直接攻撃unitも隣接マスから参照点を支えられる。
pub(crate) fn reference_reach(stats: &UnitStats) -> u32 {
    stats.max_range.max(1)
}

/// 単独行動の健全な非占領unitが敵へ近づく価値。
pub(crate) fn solo_enemy_approach_score(
    board: &TacticalBoard,
    unit: &UnitState,
    position: GridPosition,
    distances: &mut ApproachDistances,
) -> i32 {
    let stats = &unit.stats;
    let nearest = distances.nearest(stats, position, board.enemy_positions(), stats.max_range);
    nearest_approach_bonus(nearest, stats.max_movement)
}

/// 空の輸送役が自軍の生産施設へ戻る価値。
pub(crate) fn empty_transport_return_score(
    world: &World,
    board: &TacticalBoard,
    unit: &UnitState,
    position: GridPosition,
    distances: &mut ApproachDistances,
) -> i32 {
    let stats = &unit.stats;
    let is_empty_transport = stats.max_cargo > 0
        && world
            .get::<crate::components::CargoCapacity>(unit.entity)
            .is_some_and(|cargo| cargo.loaded.is_empty());
    if !is_empty_transport {
        return 0;
    }
    let nearest = distances.nearest(stats, position, board.own_production_sites(), 0);
    nearest_approach_bonus(nearest, stats.max_movement)
}

/// 取り残された健全な歩兵が、輸送を待てる海岸へ寄る価値。
///
/// 座礁した戦闘車両に広げると、全unitが海岸に密集して海峡越しの交戦誤判定を招き、
/// 拡張が停止する退行が観測されたため、歩兵に限る。
pub(crate) fn stranded_infantry_score(
    world: &World,
    board: &TacticalBoard,
    unit: &UnitState,
    position: GridPosition,
    distances: &mut ApproachDistances,
) -> i32 {
    if !unit.is_infantry()
        || unit.combat_ineffective
        || !is_unit_stranded(
            world,
            &unit.position,
            board.player_id,
            &board.properties,
            &board.enemies,
        )
    {
        return 0;
    }
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

/// 占領能力（搭載中の占領兵を含む）があれば未所有物件へ、単独の戦闘unitは敵へ近づく価値。
///
/// #53: Squad所属の占領unitは部隊目標への接近だけに従う。最寄り物件への引力を併用すると
/// 前線都市へ引き戻され、後方の敵生産施設を狙う部隊が機能しない。
pub(crate) fn capture_or_hunt_score(
    world: &World,
    board: &TacticalBoard,
    unit: &UnitState,
    is_solo: bool,
    position: GridPosition,
    distances: &mut ApproachDistances,
) -> i32 {
    let stats = &unit.stats;
    if can_capture_or_carries_capturer(world, unit) && (!board.uses_v3_tactics || is_solo) {
        let nearest =
            distances.nearest(stats, position, board.unowned_properties(), stats.max_range);
        nearest_approach_bonus(nearest, stats.max_movement)
    } else if is_solo {
        solo_hunt_score(board, unit, position, distances)
    } else {
        0
    }
}

fn can_capture_or_carries_capturer(world: &World, unit: &UnitState) -> bool {
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
    unit: &UnitState,
    position: GridPosition,
    distances: &mut ApproachDistances,
) -> i32 {
    let stats = &unit.stats;
    let mut score = 0;
    let mut best_target_turns: i32 = 999;
    let mut best_target_position = None;
    let mut max_potential = -1.0;
    for (enemy_position, enemy_type, enemy_value, enemy_hp, ..) in &board.enemies {
        let turns = distances
            .turns(stats, position, *enemy_position, stats.max_range)
            .turns
            + ship_blocked_penalty(board, unit, *enemy_position);
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
fn ship_blocked_penalty(board: &TacticalBoard, unit: &UnitState, target: GridPosition) -> u32 {
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

/// 攻撃標的の事実と、攻撃による価値（優先度は版ごとに決める）。
pub(crate) struct AttackAssessment {
    pub(crate) target_hp: u32,
    pub(crate) target_position: GridPosition,
    pub(crate) expected_damage: u32,
    /// 撃破時の加点などを含む攻撃のscore（移動先の価値は含まない）。
    pub(crate) score: i32,
}

/// 交換予測から攻撃のscoreを求める。期待damageが0、または標的の状態が欠ける場合はNone。
pub(crate) fn assess_attack(
    world: &World,
    board: &TacticalBoard,
    unit: &UnitState,
    target: Entity,
    exchange: &AttackExchange,
) -> Option<AttackAssessment> {
    let target_stats = world.get::<UnitStats>(target)?;
    let target_health = world.get::<Health>(target)?;
    let target_position = *world.get::<GridPosition>(target)?;
    let target_faction = world.get::<Faction>(target)?;
    // 撃破判定・damage期待値は、移動後位置を渡した交換予測を正本とする。
    let expected_damage = exchange.expected_damage;
    if expected_damage == 0 {
        return None;
    }
    let target_value = strategic_target_value(
        target_stats,
        target_position,
        target_faction.0,
        world.get::<crate::components::CargoCapacity>(target),
        &board.unit_costs,
        &board.properties,
        &board.registry,
    );
    let mut score = 2000 + (expected_damage.saturating_mul(target_value) / 100) as i32;
    // 金額ROIを禁止条件にはしない。同じ作戦島で複数の敵を撃てる場合だけ、
    // 双方のHP損耗率差を使って相性のよい対象へ戦術的に切り替える。
    score += exchange.matchup_margin() / 2;
    if unit.combat_ineffective && expected_damage < target_health.current {
        score -= 3000;
    }
    if expected_damage >= target_health.current {
        score += 5000;
    }
    Some(AttackAssessment {
        target_hp: target_health.current,
        target_position,
        expected_damage,
        score,
    })
}

/// campaign所属unitは、輸送任務中は攻撃せず、配備目標か作戦島の上の敵だけを攻撃する。
pub(crate) fn attack_serves_campaign(
    world: &World,
    campaign: Option<&CampaignActionContext>,
    deployment_target: Option<Entity>,
    target: Entity,
) -> bool {
    !campaign.is_some_and(|context| {
        let target_position = world.get::<GridPosition>(target).copied();
        context.mission_type == crate::ai::squad::MissionType::Transport
            || (deployment_target != Some(target)
                && !target_position.is_some_and(|position| {
                    crate::ai::tactics::campaign::campaign_position_is_on_target_island(
                        world, context, position,
                    )
                }))
    })
}

/// 移動先にある自軍の回復拠点（この兵種が回復できる物件）と、それが生産施設か。
pub(crate) fn repair_site_at(
    board: &TacticalBoard,
    unit: &UnitState,
    position: GridPosition,
) -> Option<bool> {
    board
        .properties
        .iter()
        .find(|(site, terrain, owner)| {
            *site == position
                && *owner == Some(board.player_id)
                && board
                    .registry
                    .can_repair_on_terrain(unit.stats.unit_type, *terrain)
        })
        // 実際に生産できる首都圏の施設だけを、生産枠の封鎖として評価する。
        .map(|(_, terrain, _)| {
            board.registry.is_production_facility(terrain.as_str())
                && crate::systems::production::is_within_production_range(
                    &board.own_capitals(),
                    position.x,
                    position.y,
                    board.map.topology,
                )
        })
}

/// 回復できない自軍生産施設に居座ることの減点（生産施設の封鎖と同じ重さ）。
pub(crate) const FOREIGN_PRODUCTION_BLOCK_PENALTY: i32 = -20_000;

/// 位置が、自軍首都の生産圏内にある自軍の生産施設か（生産を塞ぐ位置か）。
pub(crate) fn blocks_own_production(board: &TacticalBoard, position: GridPosition) -> bool {
    board.own_production_sites().any(|site| site == position)
        && crate::systems::production::is_within_production_range(
            &board.own_capitals(),
            position.x,
            position.y,
            board.map.topology,
        )
}

/// 待機の回復・占有に関する加減点。回復の必要があれば回復拠点で待ち、
/// 不要なら生産施設を塞がない。`repair_site` は [`repair_site_at`] の結果。
pub(crate) fn wait_recovery_score(unit: &UnitState, repair_site: Option<bool>) -> i32 {
    match repair_site {
        Some(_) if unit.combat_ineffective => 8000,
        Some(_) if unit.hp < 100 || unit.ammo.0 < unit.stats.max_ammo1 => 1000,
        // 回復も攻撃も不要なのに空港・工場上で待機すると、次の生産を物理的に封鎖する。
        Some(true) => -20_000,
        // 生産施設ではない回復拠点も、用がなければ占有し続けない。
        Some(false) => -2000,
        None if unit.combat_ineffective => -5000,
        None => 0,
    }
}

/// 合流の可否と基礎score。他の作戦の味方、HPが溢れて返金のない合流はNone。
/// 返り値は (基礎score, 合流相手の状態)。
pub(crate) fn assess_merge(
    world: &World,
    unit: &UnitState,
    campaign: Option<&CampaignActionContext>,
    target: Entity,
) -> Option<(i32, Health)> {
    let other_operation = campaign.is_some_and(|context| {
        world
            .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
            .and_then(|assignments| assignments.assignment(target))
            .is_none_or(|assignment| assignment.owner != context.owner)
    });
    if other_operation {
        return None;
    }
    let target_health = *world.get::<Health>(target)?;
    world.get::<UnitStats>(target)?;
    if crate::ai::pruning::is_overflow_merge_without_refund(unit.hp, target_health) {
        return None;
    }
    let mut score = 3000;
    if unit.combat_ineffective || target_health.current < 40 {
        score += 4000;
    }
    if unit.hp + target_health.current <= target_health.max {
        score += 1000;
    }
    Some((score, target_health))
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

/// #45 (V3): 待ち伏せポジションのスコア。射程内で待機して先制攻撃を狙う位置
pub(crate) const AMBUSH_IN_RANGE_BONUS: i32 = 4000;
/// #45 (V3): 敵の進行を1〜2ターン待ち受けられる位置のスコア
pub(crate) const AMBUSH_NEAR_RANGE_BONUS: i32 = 2000;
/// #45 (V3): 最小射程より内側 (攻撃不能な近距離) へ前進するペナルティ
pub(crate) const AMBUSH_TOO_CLOSE_PENALTY: i32 = 3000;
/// #45 (V3): 待ち受けゾーンとみなす最大射程からのマージン (敵の接近を想定)
pub(crate) const AMBUSH_APPROACH_MARGIN: u32 = 2;

/// 通常スコアとは別軸で、作戦上必要な不利交換と戦術的に有利な攻撃を順序付ける。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ActionPriority {
    Normal,
    /// 同じ作戦の前衛が開いた後方射点への、間接部隊の追随。
    SupportAdvance,
    /// DAG区間のセル列に沿う前進。通常の局地位置取りより優先するが、
    /// その場で成立する有利な戦闘・占領は妨げない。
    RouteAdvance,
    /// 専任占領役が、実経路上で割当物件へ近づく移動。
    CaptureAdvance,
    /// 作戦パッケージが実行段階にあり、他に有利な局地標的がない場合の必要攻撃。
    StrategicTargetFallback,
    /// 損耗部隊同士が前線で即時合流し、戦力・ZoCを再建する戦術行動。
    TacticalMerge,
    /// 同じ作戦圏内で見つけた、現在兵種と相性のよい敵への攻撃。
    FavorableLocalTarget,
    /// 作戦対象そのものとの相性もよい攻撃。
    FavorableStrategicTarget,
    /// 敵の有効な生産可能マスを占位し、敵の戦力補充を物理的に遮断する行動。
    /// 自軍の生産口を開けるProductionSiteReliefの攻勢的鏡像であり、
    /// 敵の補充速度を物理的に削ぐことで全滅勝利を確定させる。
    EnemyProductionBlockade,
    /// 専任占領役が目標物件上で占領を実行する行動。
    /// 前面戦闘は同行するCombat役へ分担し、占領完了を最優先する。
    CaptureExecution,
    /// 全生産施設が自軍で埋まったとき、少なくとも一枠を開ける移動・移動攻撃。
    /// 後続を途切れさせない盤面契約であり、通常の戦術scoreとは分離して扱う。
    ProductionSiteRelief,
}
