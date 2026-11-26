//! ゼロベースの要所集中評価（`OPENWARS_V4_KEY_TERRAIN_PLAN=1`）。
//!
//! 既存の複雑な局地スコア合算や多数の優先度分岐に依存せず、
//! 1. マップから押さえるべき要所（前線防衛線・前線拠点・砲座）を特定し、
//! 2. マクロの要所確保・維持がミクロの局地戦や被弾回避に負けない明確な優先度階層で
//!
//! 各ユニットの行動（占領・砲撃・前線戦闘・前線確保・前進）を評価する。

use super::*;
use crate::ai::v4::hold_line::planner::InitialHoldSites;

/// 要所集中プランの有効化スイッチ（既定はオフ）。
pub(crate) fn enabled() -> bool {
    std::env::var("OPENWARS_V4_KEY_TERRAIN_PLAN").is_ok_and(|v| v == "1")
}

/// 砲座とみなす前線都市から前線ラインまでの最小・最大距離。
const BATTERY_MIN_RANGE: u32 = 2;
const BATTERY_MAX_RANGE: u32 = 4;
/// 砲座を維持する最低HP閾値（これ以下は修理・合流を優先）。
const BATTERY_MIN_HOLD_HP: u32 = 30;
/// 押し上げ判定で見る前線周辺の半径（敵の前線圧力・自軍前衛の厚み）。
const FRONT_PRESSURE_RADIUS: u32 = 2;
/// 押し上げを開始するために前線周辺に揃っているべき最低前衛数。
const MIN_PUSH_VANGUARD: usize = 4;
/// フォーカス撃破ターゲットへの加点。
const FOCUS_KILL_BONUS: i32 = 4_000;
/// 前線都市の占領実行への追加ボーナス。
const FRONT_CITY_CAPTURE_BONUS: i32 = 500;
/// 航空機の大幅赤字攻撃を抑止する余剰損失閾値。
const AIR_SURPLUS_LOSS_THRESHOLD: i32 = 4_000;
/// 目標への1ターン接近あたりのボーナス。
const TURN_APPROACH_WEIGHT: i32 = 2_000;
/// 目標への距離1マス接近あたりのボーナス。
const DISTANCE_APPROACH_WEIGHT: i32 = 100;
/// 地形防御1%あたりの基本点。
const DEFENSE_BONUS_WEIGHT: i32 = 15;
/// 生産枠不足時に工場上射撃へ課すコスト。
const PRODUCTION_SLOT_PENALTY: i32 = 1_000;

/// マップから導出した要所情報。
struct KeyTerrainContext {
    /// 前線防衛線（森などの有利地形ライン）。
    front_line: HashSet<GridPosition>,
    /// 自陣側最前線の都市群。
    front_cities: HashSet<GridPosition>,
    /// 非生産かつ前線防衛線を射程に収める砲座候補都市。
    battery_sites: HashSet<GridPosition>,
    /// 自陣側とみなす初期拠点・都市集合（前線より自陣側かどうかの判定に使用）。
    home_sites: HashSet<GridPosition>,
    /// 現在味方が占拠していない空きの前線防衛線セル。
    open_front_cells: Vec<GridPosition>,
    /// 現在味方間接ユニットが乗っていない空きの砲座候補都市。
    open_battery_sites: Vec<GridPosition>,
    /// 自軍が十分な戦力優位を築き、前線を押し上げる局面か。
    is_push_phase: bool,
}

impl KeyTerrainContext {
    /// 現在の盤面から要所情報を構築する。
    fn observe(world: &World, board: &SmallMapBoard) -> Self {
        let tactical = &board.tactical;
        let player_id = tactical.player_id;
        let front_line: HashSet<GridPosition> = resolve_hold_line(world, tactical)
            .map(|line| line.cells.into_iter().collect())
            .unwrap_or_default();
        let front_cities = crate::ai::squad::planning_policy::home_front_cities(
            world,
            player_id,
            &tactical.map,
            &tactical.registry,
        );
        let home_sites = world
            .get_resource::<InitialHoldSites>()
            .and_then(|sites| sites.line(tactical).map(|line| line.home.clone()))
            .unwrap_or_default();

        let battery_sites: HashSet<GridPosition> = front_cities
            .iter()
            .copied()
            .filter(|pos| {
                !board.own_production.contains((pos.x, pos.y))
                    && (front_line.is_empty()
                        || front_line.iter().any(|front| {
                            let dist = tactical.map.distance(pos.x, pos.y, front.x, front.y);
                            (BATTERY_MIN_RANGE..=BATTERY_MAX_RANGE).contains(&dist)
                        }))
            })
            .collect();

        let mut open_front_cells: Vec<GridPosition> = front_line
            .iter()
            .copied()
            .filter(|pos| {
                !tactical
                    .occupancy
                    .get(&(pos.x, pos.y))
                    .is_some_and(|occ| occ.player_id == player_id)
            })
            .collect();
        open_front_cells.sort_by_key(|p| (p.x, p.y));

        let mut open_battery_sites: Vec<GridPosition> = battery_sites
            .iter()
            .copied()
            .filter(|pos| {
                !tactical
                    .occupancy
                    .get(&(pos.x, pos.y))
                    .is_some_and(|occ| occ.player_id == player_id)
            })
            .collect();
        open_battery_sites.sort_by_key(|p| (p.x, p.y));

        let mut ctx = Self {
            front_line,
            front_cities,
            battery_sites,
            home_sites,
            open_front_cells,
            open_battery_sites,
            is_push_phase: false,
        };

        ctx.is_push_phase = ctx.detect_push_phase(
            &tactical.map,
            player_id,
            &tactical.occupancy,
            &tactical.enemies,
            &tactical.registry,
        );

        ctx
    }

    /// 前線周辺の敵の厚みと自軍前衛の厚みから、前線を押し上げる攻勢転移の局面か見極める。
    fn detect_push_phase(
        &self,
        map: &crate::resources::Map,
        player_id: PlayerId,
        occupancy: &HashMap<(usize, usize), OccupantInfo>,
        enemies: &[crate::ai::threat::EnemyThreatInfo],
        registry: &crate::resources::master_data::MasterDataRegistry,
    ) -> bool {
        if self.front_line.is_empty() {
            return false;
        }
        // 自陣侵入敵や前線上の敵がいる間は防衛・奪還を優先し、押し上げない。
        if enemies.iter().any(|enemy| {
            self.is_behind_front(map, enemy.position) || self.front_line.contains(&enemy.position)
        }) {
            return false;
        }

        let front_enemies = enemies
            .iter()
            .filter(|enemy| {
                self.front_line.iter().any(|front| {
                    map.distance(enemy.position.x, enemy.position.y, front.x, front.y)
                        <= FRONT_PRESSURE_RADIUS
                })
            })
            .count();

        let front_vanguard = occupancy
            .iter()
            .filter(|&(&(x, y), occ)| {
                occ.player_id == player_id
                    && !occ.is_transport
                    && registry.ai_category_for(occ.unit_type).is_some_and(|cat| {
                        use crate::resources::master_data::AiUnitCategory;
                        matches!(
                            cat,
                            AiUnitCategory::Infantry
                                | AiUnitCategory::Tank
                                | AiUnitCategory::AntiAirTank
                                | AiUnitCategory::Fighter
                                | AiUnitCategory::AirForce
                        )
                    })
                    && self
                        .front_line
                        .iter()
                        .any(|front| map.distance(x, y, front.x, front.y) <= FRONT_PRESSURE_RADIUS)
            })
            .count();

        front_vanguard >= MIN_PUSH_VANGUARD && front_vanguard >= front_enemies.saturating_mul(2)
    }

    /// 指定座標が前線防衛線より自陣側（後方または前線都市上）にあるか判定する。
    fn is_behind_front(&self, map: &crate::resources::Map, pos: GridPosition) -> bool {
        if self.front_cities.contains(&pos) || self.home_sites.contains(&pos) {
            return true;
        }
        if self.home_sites.is_empty() || self.front_line.is_empty() {
            return false;
        }
        let Some(nearest_front) = self
            .front_line
            .iter()
            .copied()
            .min_by_key(|front| map.distance(pos.x, pos.y, front.x, front.y))
        else {
            return false;
        };
        let dist_pos = self
            .home_sites
            .iter()
            .map(|h| map.distance(h.x, h.y, pos.x, pos.y))
            .min()
            .unwrap_or(u32::MAX);
        let dist_front = self
            .home_sites
            .iter()
            .map(|h| map.distance(h.x, h.y, nearest_front.x, nearest_front.y))
            .min()
            .unwrap_or(u32::MAX);
        dist_pos < dist_front
    }
}

/// ゼロベースの要所集中評価で全ユニットの行動候補を評価する。
pub(super) fn evaluate_actions(
    world: &World,
    board: &SmallMapBoard,
    movable_units: Vec<Entity>,
    distances: &mut ApproachDistances,
) -> Vec<EvaluatedAction> {
    let context = KeyTerrainContext::observe(world, board);
    let observed_units: Vec<SmallMapUnit> = movable_units
        .into_iter()
        .filter_map(|entity| SmallMapUnit::observe(world, board, entity, distances))
        .collect();

    let focus_targets = collect_focus_kill_targets(world, board, &observed_units);
    let relay_units = collect_relay_capture_units(board, &context, &observed_units, distances);

    observed_units
        .into_iter()
        .filter_map(|unit| {
            let entity = unit.state.entity;
            let origin = unit.state.position;
            let best = evaluate_unit(
                world,
                board,
                &context,
                &focus_targets,
                &relay_units,
                &unit,
                distances,
            );
            let (rank, command) = best.into_inner()?;
            Some(EvaluatedAction {
                entity,
                origin,
                command,
                rank,
                movement: None,
                hold: None,
                forced_target: None,
            })
        })
        .collect()
}

/// 占領中の歩兵が今ターン前線へ届き、かつ後続の歩兵が1ターンでその都市を引き継げる場合にリレー対象とする。
fn collect_relay_capture_units(
    board: &SmallMapBoard,
    context: &KeyTerrainContext,
    units: &[SmallMapUnit],
    distances: &mut ApproachDistances,
) -> HashSet<Entity> {
    let mut relays = HashSet::default();
    if context.is_push_phase || context.open_front_cells.is_empty() {
        return relays;
    }
    let reaches_open_front = |u: &SmallMapUnit| {
        u.candidate_tiles.iter().any(|&(cx, cy)| {
            context
                .open_front_cells
                .contains(&GridPosition { x: cx, y: cy })
        })
    };

    for unit in units {
        let state = &unit.state;
        if !state.stats.can_capture || state.combat_ineffective {
            continue;
        }
        let pos = state.position;
        let can_capture_here = board
            .tactical
            .action_targets
            .action_targets_at(state.entity, &state.stats, pos, false)
            .can_capture;
        if !can_capture_here {
            continue;
        }
        // 生産施設は1ターン遅らせないようリレー対象から外す。
        let is_prod = board
            .tactical
            .map
            .get_terrain(pos.x, pos.y)
            .is_some_and(|t| matches!(t, Terrain::Factory | Terrain::Airport | Terrain::Port));
        if is_prod || !reaches_open_front(unit) {
            continue;
        }
        let has_successor = units.iter().any(|other| {
            if other.state.entity == state.entity
                || !other.state.stats.can_capture
                || other.state.combat_ineffective
                || reaches_open_front(other)
            {
                return false;
            }
            let other_capturing_here = board
                .tactical
                .action_targets
                .action_targets_at(
                    other.state.entity,
                    &other.state.stats,
                    other.state.position,
                    false,
                )
                .can_capture;
            !other_capturing_here
                && distances
                    .turns(&other.state.stats, other.state.position, pos, 0)
                    .turns
                    <= 1
        });
        if has_successor {
            relays.insert(state.entity);
        }
    }
    relays
}

/// 今ターン中に複数ユニットの合算火力で撃破できる敵集合を求める。
fn collect_focus_kill_targets(
    world: &World,
    board: &SmallMapBoard,
    units: &[SmallMapUnit],
) -> HashSet<Entity> {
    let mut max_damage_by_enemy: HashMap<Entity, Vec<(Entity, u32)>> = HashMap::default();
    for unit in units {
        let mut best_for_unit: HashMap<Entity, u32> = HashMap::default();
        for &(x, y) in &unit.candidate_tiles {
            let pos = GridPosition { x, y };
            let is_moved = pos != unit.state.position;
            let actions = board.tactical.action_targets.action_targets_at(
                unit.state.entity,
                &unit.state.stats,
                pos,
                is_moved,
            );
            for target in actions.attackable_targets {
                let Some(exchange) = crate::ai::pruning::evaluate_attack_exchange(
                    world,
                    unit.state.entity,
                    target,
                    pos,
                    &board.tactical.damage_chart,
                ) else {
                    continue;
                };
                best_for_unit
                    .entry(target)
                    .and_modify(|dmg| *dmg = (*dmg).max(exchange.expected_damage))
                    .or_insert(exchange.expected_damage);
            }
        }
        for (target, dmg) in best_for_unit {
            if dmg > 0 {
                max_damage_by_enemy
                    .entry(target)
                    .or_default()
                    .push((unit.state.entity, dmg));
            }
        }
    }

    let mut focus = HashSet::default();
    for (target, attackers) in max_damage_by_enemy {
        if attackers.len() >= 2
            && let Some(health) = world.get::<Health>(target)
        {
            let total: u32 = attackers.iter().map(|(_, dmg)| *dmg).sum();
            if total >= health.current {
                focus.insert(target);
            }
        }
    }
    focus
}

/// 1ユニットの全ての合法移動先・行動を評価し、最良の1手を選ぶ。
fn evaluate_unit(
    world: &World,
    board: &SmallMapBoard,
    context: &KeyTerrainContext,
    focus_targets: &HashSet<Entity>,
    relay_units: &HashSet<Entity>,
    unit: &SmallMapUnit,
    distances: &mut ApproachDistances,
) -> BestAction {
    let mut best = BestAction::default();
    let tactical = &board.tactical;
    let state = &unit.state;

    // 各ユニットが目指す基本の接近目標を決定する。
    let approach_goal = resolve_approach_goal(board, context, unit, distances);

    for &(x, y) in &unit.candidate_tiles {
        let pos = GridPosition { x, y };
        let is_stationary = pos == state.position;
        let actions = tactical.action_targets.action_targets_at(
            state.entity,
            &state.stats,
            pos,
            !is_stationary,
        );
        let relieves_gridlock = board.own_production.gridlocked
            && unit.starts_on_own_production
            && !state.combat_ineffective
            && !board.own_production.contains((x, y));
        let defense_bonus = terrain_defense_bonus(tactical, pos);
        let on_front_line = context.front_line.contains(&pos);
        let on_battery = state.stats.min_range > 1
            && (context.battery_sites.contains(&pos)
                || is_active_supply_battery_at(tactical, state, pos));
        let on_own_production = board.own_production.contains((x, y));

        // 1. 占領実行（CaptureExecution）
        if actions.can_capture {
            let priority = if relieves_gridlock {
                ActionPriority::ProductionSiteRelief
            } else if is_stationary && relay_units.contains(&state.entity) {
                // 後続が引き継げるため、前線へ押し出して1ターン短縮する。
                ActionPriority::CaptureAdvance
            } else {
                ActionPriority::CaptureExecution
            };
            let front_bonus = if context.front_cities.contains(&pos) {
                FRONT_CITY_CAPTURE_BONUS
            } else {
                0
            };
            best.offer((priority, CAPTURE_SCORE + front_bonus), || {
                AiCommand::Capture { target_pos: pos }
            });
        }

        // 2. 攻撃評価
        for &target in &actions.attackable_targets {
            evaluate_attack_candidate(
                world,
                board,
                context,
                focus_targets,
                unit,
                pos,
                target,
                defense_bonus,
                on_front_line,
                on_battery,
                on_own_production,
                relieves_gridlock,
                &mut best,
            );
        }

        // 3. 合流評価（前線上での合流は前線保持と同格に扱う）
        for &target in &actions.mergeable_targets {
            let Some((merge_score, _)) = assess_merge(world, state, unit.campaign.as_ref(), target)
            else {
                continue;
            };
            let priority = if relieves_gridlock {
                ActionPriority::ProductionSiteRelief
            } else if on_front_line {
                ActionPriority::FavorableLocalTarget
            } else {
                ActionPriority::TacticalMerge
            };
            best.offer((priority, merge_score), || AiCommand::Merge {
                target_pos: pos,
                target_entity: target,
            });
        }

        // 4. 待機・前進・要所保持の評価
        evaluate_wait_candidate(
            board,
            context,
            unit,
            pos,
            is_stationary,
            actions.can_wait,
            defense_bonus,
            on_front_line,
            on_battery,
            on_own_production,
            relieves_gridlock,
            approach_goal,
            distances,
            &mut best,
        );
    }

    best
}

/// ユニットが今ターン非戦闘時に近づくべき目標座標を決定する。
fn resolve_approach_goal(
    board: &SmallMapBoard,
    context: &KeyTerrainContext,
    unit: &SmallMapUnit,
    distances: &mut ApproachDistances,
) -> Option<GridPosition> {
    let state = &unit.state;
    // 損耗ユニットは最寄り修理拠点へ向かう。
    if state.combat_ineffective && !unit.repair_sites.is_empty() {
        return unit
            .repair_sites
            .iter()
            .copied()
            .min_by_key(|&pos| distances.turns(&state.stats, state.position, pos, 0));
    }
    // 占領契約がある場合はその目標へ向かう。
    if let Some(contract) = unit.capture_contract {
        return Some(contract.target);
    }
    // 間接ユニットは空いている砲座都市、なければ前線ラインへ向かう。
    if state.stats.min_range > 1 {
        if !context.open_battery_sites.is_empty() {
            return context
                .open_battery_sites
                .iter()
                .copied()
                .min_by_key(|&pos| distances.turns(&state.stats, state.position, pos, 0));
        }
        return context
            .front_line
            .iter()
            .copied()
            .min_by_key(|&pos| distances.turns(&state.stats, state.position, pos, 0));
    }
    // 歩兵・直接ユニットは通常時は空いている前線ライン、押し上げ局面または埋まっていれば最寄り敵へ向かう。
    if !context.is_push_phase && state.stats.can_capture && !context.open_front_cells.is_empty() {
        return context
            .open_front_cells
            .iter()
            .copied()
            .min_by_key(|&pos| distances.turns(&state.stats, state.position, pos, 0));
    }
    board
        .tactical
        .enemies
        .iter()
        .map(|enemy| enemy.position)
        .min_by_key(|&pos| {
            board
                .tactical
                .map
                .distance(state.position.x, state.position.y, pos.x, pos.y)
        })
}

/// 攻撃候補の評価。マクロ（要所上・砲座・自陣防衛）をミクロの局地攻撃より上位に置く。
#[allow(clippy::too_many_arguments)]
fn evaluate_attack_candidate(
    world: &World,
    board: &SmallMapBoard,
    context: &KeyTerrainContext,
    focus_targets: &HashSet<Entity>,
    unit: &SmallMapUnit,
    pos: GridPosition,
    target: Entity,
    defense_bonus: u32,
    on_front_line: bool,
    on_battery: bool,
    on_own_production: bool,
    relieves_gridlock: bool,
    best: &mut BestAction,
) {
    let tactical = &board.tactical;
    let state = &unit.state;
    let Some(exchange) = crate::ai::pruning::evaluate_attack_exchange(
        world,
        state.entity,
        target,
        pos,
        &tactical.damage_chart,
    ) else {
        return;
    };
    let Some(attack) = assess_attack(world, tactical, state, target, &exchange) else {
        return;
    };

    let is_survivable_lethal =
        attack.expected_damage >= attack.target_hp && exchange.expected_counter_damage < state.hp;
    let post_counter_hp = state.hp.saturating_sub(exchange.expected_counter_damage);

    let post_attack_loss = if state.stats.movement_type == crate::resources::MovementType::Air {
        air_post_attack_expected_loss_value(
            tactical,
            state,
            pos,
            post_counter_hp,
            defense_bonus,
            attack.target_position,
            attack.expected_damage,
        )
    } else if state.stats.min_range <= 1 {
        crate::ai::threat::post_attack_expected_loss_value(
            &tactical.map,
            (pos.x, pos.y),
            state.stats.unit_type,
            state.stats.cost,
            post_counter_hp,
            defense_bonus,
            attack.target_position,
            attack.expected_damage,
            &tactical.enemies,
            &tactical.damage_chart,
            &tactical.registry,
        )
    } else {
        0
    };

    // 航空機の大幅赤字特攻は抑止する。
    if state.stats.movement_type == crate::resources::MovementType::Air
        && post_attack_loss > (exchange.damage_value_dealt as i32).saturating_mul(2)
        && post_attack_loss - (exchange.damage_value_dealt as i32) >= AIR_SURPLUS_LOSS_THRESHOLD
    {
        return;
    }

    let enemy_behind_front = context.is_behind_front(&tactical.map, attack.target_position);
    let enemy_on_front_line = context.front_line.contains(&attack.target_position);
    let favorable = exchange.is_favorable_matchup();

    // 歩兵が平地等で反撃死・大損するだけの非致死・不利交換は無謀なので除外する（自陣防衛は除く）。
    if state.stats.can_capture
        && !enemy_behind_front
        && !on_front_line
        && !favorable
        && !is_survivable_lethal
    {
        return;
    }

    let priority = if relieves_gridlock {
        ActionPriority::ProductionSiteRelief
    } else if state.stats.min_range > 1 {
        // 間接ユニット: 砲座・定位置射撃を最優先（被弾回避で逃げない）。
        if on_battery || pos == state.position {
            ActionPriority::AssaultSupportFire
        } else {
            ActionPriority::AssaultStrike
        }
    } else if enemy_behind_front
        && (favorable || is_survivable_lethal || attack.expected_damage >= 35)
    {
        // 前線より自陣側に侵入した敵は優先排除する。
        ActionPriority::AssaultSupportFire
    } else if on_front_line && (favorable || is_survivable_lethal) {
        // 前線防衛線（森など）上からの攻撃。
        ActionPriority::AssaultStrike
    } else if enemy_on_front_line && is_survivable_lethal {
        // 前線ライン上の敵を仕留め切るトドメ。
        ActionPriority::FavorableStrategicTarget
    } else if favorable || is_survivable_lethal {
        // 通常の有利・致死攻撃（後続の届かない歩兵や直接ユニット）。
        ActionPriority::StrategicTargetFallback
    } else if on_front_line && exchange.expected_counter_damage < state.hp {
        // 前線上での反撃死しない削り攻撃。
        ActionPriority::StrategicTargetFallback
    } else {
        return;
    };

    let focus_bonus = if focus_targets.contains(&target) {
        FOCUS_KILL_BONUS
    } else {
        0
    };
    let slot_penalty = if on_own_production
        && unit.can_end_turn_off_production
        && board
            .own_production
            .free_slots
            .saturating_sub(usize::from(!unit.starts_on_own_production))
            < board.own_production.demand_slots
    {
        PRODUCTION_SLOT_PENALTY
    } else {
        0
    };
    let loss_penalty = if on_battery { 0 } else { post_attack_loss / 2 };
    let score = (exchange.damage_value_dealt as i32)
        - (exchange.counter_value_received as i32)
        - loss_penalty
        + focus_bonus
        + (defense_bonus as i32) * DEFENSE_BONUS_WEIGHT
        - slot_penalty;

    best.offer((priority, score), || AiCommand::Attack {
        target_pos: pos,
        target_entity: target,
    });
}

/// 待機・前進・要所保持候補の評価。
#[allow(clippy::too_many_arguments)]
fn evaluate_wait_candidate(
    board: &SmallMapBoard,
    context: &KeyTerrainContext,
    unit: &SmallMapUnit,
    pos: GridPosition,
    is_stationary: bool,
    can_wait: bool,
    defense_bonus: u32,
    on_front_line: bool,
    on_battery: bool,
    on_own_production: bool,
    relieves_gridlock: bool,
    approach_goal: Option<GridPosition>,
    distances: &mut ApproachDistances,
    best: &mut BestAction,
) {
    if !can_wait {
        return;
    }
    let tactical = &board.tactical;
    let state = &unit.state;

    // 健全なユニットは生産口を塞がない。
    let healthy_blocks_production = !state.combat_ineffective
        && on_own_production
        && (unit.has_non_production_wait_destination || !unit.starts_on_own_production);
    if healthy_blocks_production {
        return;
    }

    let nearest_enemy_dist = tactical
        .enemy_positions()
        .map(|enemy| tactical.map.distance(enemy.x, enemy.y, pos.x, pos.y))
        .min()
        .unwrap_or(u32::MAX);

    // 砲座を維持すべき間接ユニットか（敵に最小射程内へ潜り込まれておらず、HPも十分）。
    let holds_battery = on_battery
        && !state.combat_ineffective
        && state.hp > BATTERY_MIN_HOLD_HP
        && nearest_enemy_dist >= state.stats.min_range;

    let advances_capture = unit.capture_contract.is_some_and(|contract| {
        pos == contract.target
            || distances.turns(&state.stats, pos, contract.target, 0) < contract.turns_before
    });
    let capture_is_front_city = unit
        .capture_contract
        .is_some_and(|contract| context.front_cities.contains(&contract.target));

    let priority = if relieves_gridlock {
        ActionPriority::ProductionSiteRelief
    } else if holds_battery {
        // 間接ユニットが前線拠点（砲座）を押さえて維持する。
        ActionPriority::FavorableStrategicTarget
    } else if advances_capture && capture_is_front_city {
        // 前線都市の占領前進は森の待機より優先する。
        ActionPriority::FavorableStrategicTarget
    } else if !context.is_push_phase
        && on_front_line
        && !state.combat_ineffective
        && state.stats.min_range <= 1
    {
        // 前線防衛線（森など）の空きマスを確保して維持する。
        ActionPriority::FavorableLocalTarget
    } else if advances_capture {
        ActionPriority::CaptureAdvance
    } else if !is_stationary && approach_goal.is_some() {
        ActionPriority::RouteAdvance
    } else {
        ActionPriority::Normal
    };

    let mut score = (defense_bonus as i32) * DEFENSE_BONUS_WEIGHT;
    if let Some(goal) = approach_goal {
        let turns = distances.turns(&state.stats, pos, goal, 0);
        let dist = tactical.map.distance(pos.x, pos.y, goal.x, goal.y);
        score -= (turns.turns as i32) * TURN_APPROACH_WEIGHT;
        score -= (dist as i32) * DISTANCE_APPROACH_WEIGHT;
    }

    // 砲座維持（holds_battery）以外の間接・航空ユニットは、通常の露出リスクを評価する。
    if !holds_battery
        && (state.stats.min_range > 1
            || state.stats.movement_type == crate::resources::MovementType::Air)
    {
        score -= crate::ai::threat::exposure_penalty(
            &tactical.map,
            (pos.x, pos.y),
            state.stats.unit_type,
            state.stats.cost,
            state.hp,
            state.stats.min_range,
            defense_bonus,
            &tactical.enemies,
            &tactical.damage_chart,
            &tactical.registry,
        );
    }

    best.offer((priority, score), || AiCommand::Wait { target_pos: pos });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{GridTopology, Map, Terrain};

    #[test]
    fn is_behind_front_identifies_home_side_and_front_cities_without_hardcoding() {
        let mut map = Map::new(11, 15, Terrain::Plains, GridTopology::Hex);
        for x in 1..=9 {
            map.set_terrain(x, 7, Terrain::Forest).unwrap();
        }
        let context = KeyTerrainContext {
            front_line: (1..=9).map(|x| GridPosition { x, y: 7 }).collect(),
            front_cities: [GridPosition { x: 5, y: 5 }].into_iter().collect(),
            battery_sites: HashSet::default(),
            home_sites: [GridPosition { x: 5, y: 3 }].into_iter().collect(),
            open_front_cells: Vec::new(),
            open_battery_sites: Vec::new(),
            is_push_phase: false,
        };
        assert!(context.is_behind_front(&map, GridPosition { x: 5, y: 5 }));
        assert!(context.is_behind_front(&map, GridPosition { x: 5, y: 6 }));
        assert!(!context.is_behind_front(&map, GridPosition { x: 5, y: 7 }));
        assert!(!context.is_behind_front(&map, GridPosition { x: 5, y: 8 }));
    }

    #[test]
    fn priority_hierarchy_places_key_terrain_above_local_micro_actions() {
        // 砲座射撃・自陣侵入敵排除 > 前線ライン上からの攻撃 > 砲座維持 > 前線ライン保持 > 局地攻撃
        assert!(ActionPriority::AssaultSupportFire > ActionPriority::AssaultStrike);
        assert!(ActionPriority::AssaultStrike > ActionPriority::FavorableStrategicTarget);
        assert!(ActionPriority::FavorableStrategicTarget > ActionPriority::FavorableLocalTarget);
        assert!(ActionPriority::FavorableLocalTarget > ActionPriority::StrategicTargetFallback);
        assert!(ActionPriority::StrategicTargetFallback > ActionPriority::RouteAdvance);
    }

    #[test]
    fn detect_push_phase_matches_human_board_intuition() {
        use crate::ai::threat::EnemyThreatInfo;
        use crate::resources::UnitType;
        use crate::resources::master_data::MasterDataRegistry;

        let mut map = Map::new(11, 15, Terrain::Plains, GridTopology::Hex);
        for x in 1..=9 {
            map.set_terrain(x, 7, Terrain::Forest).unwrap();
        }
        let context = KeyTerrainContext {
            front_line: (1..=9).map(|x| GridPosition { x, y: 7 }).collect(),
            front_cities: [GridPosition { x: 5, y: 5 }].into_iter().collect(),
            battery_sites: HashSet::default(),
            home_sites: [GridPosition { x: 5, y: 3 }].into_iter().collect(),
            open_front_cells: Vec::new(),
            open_battery_sites: Vec::new(),
            is_push_phase: false,
        };
        let registry = MasterDataRegistry::load().unwrap();
        let make_occ = |player_id: PlayerId, unit_type: UnitType| OccupantInfo {
            player_id,
            is_transport: false,
            unit_type,
            loadable_types: Vec::new(),
            free_slots: 0,
        };
        let make_enemy = |x: usize, y: usize| EnemyThreatInfo {
            position: GridPosition { x, y },
            unit_type: UnitType::Infantry,
            strategic_value: 1_000,
            hp: 100,
            min_range: 1,
            max_range: 1,
            max_movement: 3,
            ammo: (10, 10),
        };

        // 味方前衛がy=7に8体揃っている盤面
        let mut occupancy = HashMap::default();
        for x in 1..=8 {
            occupancy.insert((x, 7), make_occ(PlayerId(2), UnitType::Infantry));
        }

        // 1. 敵がy=8..9に6体残っている（2倍優位ではない）→ まだ押し上げない
        let dense_enemies: Vec<_> = (1..=6).map(|x| make_enemy(x, 8)).collect();
        assert!(!context.detect_push_phase(
            &map,
            PlayerId(2),
            &occupancy,
            &dense_enemies,
            &registry
        ));

        // 2. 敵がy<=6に1体でも侵入している → 押し上げない
        let behind_enemies = vec![make_enemy(5, 6), make_enemy(3, 9)];
        assert!(!context.detect_push_phase(
            &map,
            PlayerId(2),
            &occupancy,
            &behind_enemies,
            &registry
        ));

        // 3. 敵がy=7の森上に1体でも残っている → 押し上げない
        let on_front_enemies = vec![make_enemy(9, 7), make_enemy(3, 9)];
        assert!(!context.detect_push_phase(
            &map,
            PlayerId(2),
            &occupancy,
            &on_front_enemies,
            &registry
        ));

        // 4. 前線周辺の敵が3体まで減り、自軍前衛が2倍以上（8 >= 6）→ 押し上げ発動！
        let collapsed_enemies = vec![make_enemy(2, 8), make_enemy(5, 9), make_enemy(7, 9)];
        assert!(context.detect_push_phase(
            &map,
            PlayerId(2),
            &occupancy,
            &collapsed_enemies,
            &registry
        ));
    }
}
